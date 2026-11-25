//! `agnoperiph-cli` — Headless HID enumeration, read-only probing, and safe-by-default CLI.

#![forbid(unsafe_code)]

use std::sync::Arc;

use agnoperiph_core::{
    BatteryStatus, DebounceMs, DeviceInfo, DeviceRegistry, DpiStage, DpiValue, HallEffectSnapshot,
    KeyIndex, LiftOffDistance, Peripheral, PointerSnapshot, PollingRateHz, ProfileSlot,
    ProfileSnapshot, RapidTriggerConfig, RgbSnapshot, TravelHundredthsMm,
};
use agnoperiph_driver_atk::AtkRs7Device;
use agnoperiph_driver_lamzu::LamzuAtlantisDevice;
use agnoperiph_driver_logitech::LogitechHidppDevice;
use agnoperiph_transport::{
    HidDeviceDescriptor, HidEnumerator, HidTransport, HidapiEnumerator, NoOpPacketSink, PacketSink,
    TracingPacketSink,
};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "agnoperiph-cli",
    version,
    about = "Headless USB HID inspector and peripheral CLI for AgnoPeriph"
)]
struct Cli {
    /// Suppress outgoing HID write commands while still validating and logging packets.
    #[arg(long, global = true, default_value_t = false)]
    dry_run: bool,

    /// Emit hexadecimal wire packet traces to stderr.
    #[arg(long, global = true, default_value_t = false)]
    trace_hex: bool,

    /// Emit machine-readable JSON output.
    #[arg(long, global = true, default_value_t = false)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Enumerate connected Windows HID interfaces (VID, PID, Usage Page, Usage ID, Interface #).
    List {
        /// Filter by USB Vendor ID (hex with optional 0x prefix, e.g. 0x046D or 046d).
        #[arg(long, value_parser = parse_u16_hex)]
        vid: Option<u16>,

        /// Filter by USB Product ID (hex with optional 0x prefix).
        #[arg(long, value_parser = parse_u16_hex)]
        pid: Option<u16>,

        /// Show only vendor-defined HID Usage Pages (>= 0xFF00), which typically host control protocols.
        #[arg(long, default_value_t = false)]
        vendor_pages_only: bool,

        /// Include full Windows HID device paths in table output.
        #[arg(long, default_value_t = false)]
        show_path: bool,
    },

    /// Perform read-only driver matching and status probing against connected peripherals.
    Probe {
        /// Optional USB Vendor ID filter (hex).
        #[arg(long, value_parser = parse_u16_hex)]
        vid: Option<u16>,

        /// Optional USB Product ID filter (hex).
        #[arg(long, value_parser = parse_u16_hex)]
        pid: Option<u16>,
    },

    /// Apply a validated runtime configuration write to a matched peripheral (use --dry-run first!).
    Set {
        /// Target USB Vendor ID (hex, e.g. 0x046D or 0x3554).
        #[arg(long, value_parser = parse_u16_hex)]
        vid: u16,

        /// Target USB Product ID (hex).
        #[arg(long, value_parser = parse_u16_hex)]
        pid: u16,

        /// Set active DPI value (validated against device bounds & step).
        #[arg(long)]
        dpi: Option<u32>,

        /// Set polling rate in Hz (125, 250, 500, 1000, 2000, 4000, 8000).
        #[arg(long)]
        polling_rate: Option<u16>,

        /// Set lift-off distance in tenths of mm (e.g., 10 = 1.0 mm, 20 = 2.0 mm).
        #[arg(long)]
        lod_tenths_mm: Option<u8>,

        /// Set switch debounce time in milliseconds.
        #[arg(long)]
        debounce_ms: Option<u8>,

        /// Enable or disable sensor Motion Sync.
        #[arg(long)]
        motion_sync: Option<bool>,

        /// Select onboard profile slot (zero-based index).
        #[arg(long)]
        profile_slot: Option<u8>,

        /// Set global Hall Effect actuation point in hundredths of mm (e.g., 150 = 1.50 mm).
        #[arg(long)]
        actuation_hundredths_mm: Option<u16>,

        /// Optional single key index (row * 16 + col) for per-key actuation or Rapid Trigger.
        #[arg(long)]
        key_index: Option<u16>,

        /// Enable/disable Rapid Trigger on `--key-index` (requires `--rt-press` and `--rt-release`).
        #[arg(long)]
        rapid_trigger: Option<bool>,

        /// Rapid Trigger press sensitivity in hundredths of mm (e.g., 20 = 0.20 mm).
        #[arg(long)]
        rt_press_hundredths_mm: Option<u16>,

        /// Rapid Trigger release sensitivity in hundredths of mm (e.g., 20 = 0.20 mm).
        #[arg(long)]
        rt_release_hundredths_mm: Option<u16>,
    },
}

#[derive(Debug, Serialize)]
struct ProbedDeviceReport {
    driver_id: String,
    info: DeviceInfo,
    battery: Option<BatteryStatus>,
    pointer: Option<PointerSnapshot>,
    hall_effect: Option<HallEffectSnapshot>,
    rgb: Option<RgbSnapshot>,
    profiles: Option<ProfileSnapshot>,
    warnings: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.trace_hex);

    let sink: Arc<dyn PacketSink> = if cli.trace_hex {
        Arc::new(TracingPacketSink)
    } else {
        Arc::new(NoOpPacketSink)
    };

    match cli.command {
        Command::List {
            vid,
            pid,
            vendor_pages_only,
            show_path,
        } => run_list(vid, pid, vendor_pages_only, show_path, cli.json),
        Command::Probe { vid, pid } => run_probe(vid, pid, cli.dry_run, cli.json, &sink),
        Command::Set {
            vid,
            pid,
            dpi,
            polling_rate,
            lod_tenths_mm,
            debounce_ms,
            motion_sync,
            profile_slot,
            actuation_hundredths_mm,
            key_index,
            rapid_trigger,
            rt_press_hundredths_mm,
            rt_release_hundredths_mm,
        } => run_set(
            vid,
            pid,
            SetParams {
                dpi,
                polling_rate,
                lod_tenths_mm,
                debounce_ms,
                motion_sync,
                profile_slot,
                actuation_hundredths_mm,
                key_index,
                rapid_trigger,
                rt_press_hundredths_mm,
                rt_release_hundredths_mm,
            },
            cli.dry_run,
            &sink,
        ),
    }
}

#[derive(Clone, Copy)]
struct SetParams {
    dpi: Option<u32>,
    polling_rate: Option<u16>,
    lod_tenths_mm: Option<u8>,
    debounce_ms: Option<u8>,
    motion_sync: Option<bool>,
    profile_slot: Option<u8>,
    actuation_hundredths_mm: Option<u16>,
    key_index: Option<u16>,
    rapid_trigger: Option<bool>,
    rt_press_hundredths_mm: Option<u16>,
    rt_release_hundredths_mm: Option<u16>,
}

fn build_registry() -> DeviceRegistry {
    let mut registry = DeviceRegistry::new();
    registry.register_driver(agnoperiph_driver_logitech::driver_descriptor());
    registry.register_driver(agnoperiph_driver_lamzu::driver_descriptor());
    registry.register_driver(agnoperiph_driver_atk::driver_descriptor());
    registry
}

fn init_tracing(trace_hex: bool) {
    let default_level = if trace_hex { "info" } else { "warn" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

fn run_list(
    vid_filter: Option<u16>,
    pid_filter: Option<u16>,
    vendor_pages_only: bool,
    show_path: bool,
    as_json: bool,
) -> Result<()> {
    let enumerator = HidapiEnumerator::new();
    let mut devices = enumerator
        .enumerate()
        .context("failed to enumerate Windows HID interfaces")?;

    devices.retain(|dev| {
        vid_filter.is_none_or(|v| dev.vid == v)
            && pid_filter.is_none_or(|p| dev.pid == p)
            && (!vendor_pages_only || dev.is_vendor_usage_page())
    });

    if as_json {
        println!("{}", serde_json::to_string_pretty(&devices)?);
        return Ok(());
    }

    if devices.is_empty() {
        println!("No matching HID interfaces found.");
        return Ok(());
    }

    println!(
        "{:<6} {:<6} {:<8} {:<8} {:<4} {:<9} {:<20} {:<30} {:<16}",
        "VID", "PID", "USAGE_PG", "USAGE_ID", "IF#", "BUS", "MANUFACTURER", "PRODUCT", "SERIAL"
    );
    println!("{}", "-".repeat(115));

    for dev in &devices {
        let vendor_marker = if dev.is_vendor_usage_page() { "*" } else { " " };
        let mfr = truncate(dev.manufacturer.as_deref().unwrap_or("-"), 20);
        let prod = truncate(dev.product.as_deref().unwrap_or("-"), 30);
        let serial = truncate(dev.serial_number.as_deref().unwrap_or("-"), 16);

        println!(
            "0x{:04X} 0x{:04X} 0x{:04X}{vendor_marker} 0x{:04X}   {:<4} {:<9} {:<20} {:<30} {:<16}",
            dev.vid,
            dev.pid,
            dev.usage_page,
            dev.usage_id,
            dev.interface_number,
            dev.bus_type,
            mfr,
            prod,
            serial,
        );
        if show_path {
            println!("       path: {}", dev.path);
        }
    }

    let vendor_count = devices.iter().filter(|d| d.is_vendor_usage_page()).count();
    println!(
        "\nTotal: {} HID interface(s) ({} with vendor-defined Usage Page >= 0xFF00 marked with '*').",
        devices.len(),
        vendor_count
    );

    Ok(())
}

/// Thin wrapper allowing `Box<dyn HidTransport>` to satisfy `T: HidTransport`.
struct BoxedTransport(Box<dyn HidTransport>);

impl HidTransport for BoxedTransport {
    fn descriptor(&self) -> &HidDeviceDescriptor {
        self.0.descriptor()
    }

    fn send_output_report(
        &mut self,
        report: &[u8],
    ) -> agnoperiph_transport::TransportResult<usize> {
        self.0.send_output_report(report)
    }

    fn read_input_report(
        &mut self,
        buf: &mut [u8],
        timeout: std::time::Duration,
    ) -> agnoperiph_transport::TransportResult<usize> {
        self.0.read_input_report(buf, timeout)
    }

    fn send_feature_report(&mut self, report: &[u8]) -> agnoperiph_transport::TransportResult<()> {
        self.0.send_feature_report(report)
    }

    fn get_feature_report(
        &mut self,
        buf: &mut [u8],
    ) -> agnoperiph_transport::TransportResult<usize> {
        self.0.get_feature_report(buf)
    }
}

fn open_matched_driver(
    driver_id: &str,
    transport: Box<dyn HidTransport>,
    dry_run: bool,
    sink: Arc<dyn PacketSink>,
) -> Result<Box<dyn Peripheral>> {
    let wrapped = BoxedTransport(transport);
    match driver_id {
        "logitech-hidpp" => {
            let dev = LogitechHidppDevice::open(wrapped, None, dry_run, Some(sink))
                .context("failed to probe Logitech HID++ device")?;
            Ok(Box::new(dev))
        }
        "lamzu-atlantis" => {
            let dev = LamzuAtlantisDevice::open(wrapped, dry_run, Some(sink))
                .context("failed to probe Lamzu Atlantis device")?;
            Ok(Box::new(dev))
        }
        "atk-rs7" => {
            let dev = AtkRs7Device::open(wrapped, dry_run, Some(sink))
                .context("failed to probe ATK RS7 Hall Effect keyboard")?;
            Ok(Box::new(dev))
        }
        other => bail!("unsupported driver id '{other}'"),
    }
}

fn run_probe(
    vid_filter: Option<u16>,
    pid_filter: Option<u16>,
    dry_run: bool,
    as_json: bool,
    sink: &Arc<dyn PacketSink>,
) -> Result<()> {
    if !as_json {
        eprintln!(
            "Reminder: Fully exit official vendor apps (G Hub, ATK HUB, Lamzu software, including tray icons) before live HID probing."
        );
    }

    let enumerator = HidapiEnumerator::new();
    let mut devices = enumerator
        .enumerate()
        .context("failed to enumerate Windows HID interfaces")?;
    devices.retain(|dev| {
        vid_filter.is_none_or(|v| dev.vid == v) && pid_filter.is_none_or(|p| dev.pid == p)
    });

    let registry = build_registry();
    let mut reports = Vec::new();
    let mut skipped_errors = Vec::new();

    for desc in &devices {
        let Some(matched) = registry.match_interface(&desc.to_match_descriptor()) else {
            continue;
        };
        let transport = match enumerator.open_path(desc) {
            Ok(t) => t,
            Err(err) => {
                skipped_errors.push(format!(
                    "{} ({}): {err}",
                    desc.short_id(),
                    matched.driver_id
                ));
                continue;
            }
        };

        let mut peripheral =
            match open_matched_driver(&matched.driver_id, transport, dry_run, sink.clone()) {
                Ok(p) => p,
                Err(err) => {
                    skipped_errors.push(format!(
                        "{} ({}): {err}",
                        desc.short_id(),
                        matched.driver_id
                    ));
                    continue;
                }
            };

        reports.push(collect_probe_report(matched.driver_id, peripheral.as_mut()));
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }

    print_probe_reports(&reports, &skipped_errors, devices.len(), dry_run);
    Ok(())
}

fn collect_probe_report(driver_id: String, peripheral: &mut dyn Peripheral) -> ProbedDeviceReport {
    let mut warnings = Vec::new();
    let battery = peripheral
        .as_battery_mut()
        .and_then(|b| match b.read_battery() {
            Ok(status) => Some(status),
            Err(err) => {
                warnings.push(format!("battery: {err}"));
                None
            }
        });
    let pointer = peripheral
        .as_pointer_mut()
        .and_then(|p| match p.read_pointer_state() {
            Ok(snap) => Some(snap),
            Err(err) => {
                warnings.push(format!("pointer: {err}"));
                None
            }
        });
    let hall_effect =
        peripheral
            .as_hall_effect_mut()
            .and_then(|he| match he.read_hall_effect_state() {
                Ok(snap) => Some(snap),
                Err(err) => {
                    warnings.push(format!("hall_effect: {err}"));
                    None
                }
            });
    let rgb = peripheral
        .as_rgb_mut()
        .and_then(|r| match r.read_rgb_state() {
            Ok(snap) => Some(snap),
            Err(err) => {
                warnings.push(format!("rgb: {err}"));
                None
            }
        });
    let profiles = peripheral
        .as_profiles_mut()
        .and_then(|pr| match pr.read_profile_state() {
            Ok(snap) => Some(snap),
            Err(err) => {
                warnings.push(format!("profiles: {err}"));
                None
            }
        });

    ProbedDeviceReport {
        driver_id,
        info: peripheral.info().clone(),
        battery,
        pointer,
        hall_effect,
        rgb,
        profiles,
        warnings,
    }
}

fn print_probe_reports(
    reports: &[ProbedDeviceReport],
    skipped_errors: &[String],
    total_interfaces: usize,
    dry_run: bool,
) {
    println!(
        "Probed {} matched device(s) across {total_interfaces} enumerated HID interface(s) (dry_run={dry_run}).",
        reports.len()
    );

    for report in reports {
        print_single_probe_report(report);
    }

    if !skipped_errors.is_empty() {
        println!(
            "\nUnanswered / non-matching collections ({}):",
            skipped_errors.len()
        );
        for err in skipped_errors {
            println!("  - {err}");
        }
    }
}

fn print_single_probe_report(report: &ProbedDeviceReport) {
    println!(
        "\n[{}] {} ({:04X}:{:04X}, {})",
        report.driver_id,
        report.info.model_name,
        report.info.vid,
        report.info.pid,
        report.info.connection
    );
    if let Some(fw) = &report.info.firmware_version {
        println!("  Firmware : {fw}");
    }
    if let Some(batt) = &report.battery {
        println!(
            "  Battery  : {}% ({}, voltage={:?} mV)",
            batt.percentage
                .map_or_else(|| "?".to_owned(), |p| p.to_string()),
            batt.state,
            batt.voltage_mv
        );
    }
    if let Some(ptr) = &report.pointer {
        let dpis: Vec<String> = ptr.dpi_stages.iter().map(|s| s.x.to_string()).collect();
        println!(
            "  Pointer  : polling={}, active_stage={}, stages=[{}]",
            ptr.polling_rate,
            ptr.active_dpi_stage,
            dpis.join(", ")
        );
        if ptr.lod.is_some() || ptr.debounce_ms.is_some() || ptr.motion_sync.is_some() {
            println!(
                "  Sensor   : lod={:?}, debounce={:?}, motion_sync={:?}",
                ptr.lod, ptr.debounce_ms, ptr.motion_sync
            );
        }
    }
    if let Some(he) = &report.hall_effect {
        let min_act = he
            .keys
            .iter()
            .map(|k| k.actuation)
            .min()
            .unwrap_or(TravelHundredthsMm::from_hundredths(0));
        let max_act = he
            .keys
            .iter()
            .map(|k| k.actuation)
            .max()
            .unwrap_or(TravelHundredthsMm::from_hundredths(0));
        let rt_on = he
            .keys
            .iter()
            .filter(|k| k.rapid_trigger.is_enabled())
            .count();
        let dz = he
            .keys
            .first()
            .map_or((None, None), |k| (k.top_deadzone, k.bottom_deadzone));
        println!(
            "  HallEff  : keys={}, actuation={}..{}, rt_enabled={}/{}, socd_pairs={}, dks={}, calibration={:?}",
            he.keys.len(),
            min_act,
            max_act,
            rt_on,
            he.keys.len(),
            he.socd_pairs.len(),
            he.dks_bindings.len(),
            he.calibration
        );
        if dz.0.is_some() || dz.1.is_some() {
            println!("  Deadzone : top={:?}, bottom={:?}", dz.0, dz.1);
        }
    }
    if let Some(rgb) = &report.rgb {
        for zone in &rgb.zones {
            println!(
                "  RGB Z{}  : {:?} @ {}%",
                zone.zone.value(),
                zone.effect,
                zone.brightness.percent()
            );
        }
    }
    if let Some(prof) = &report.profiles {
        println!(
            "  Profiles : active={}, mode={}, slots={}",
            prof.active_slot,
            prof.mode,
            prof.profiles.len()
        );
    }
    for warn in &report.warnings {
        println!("  Warning  : {warn}");
    }
}

fn run_set(
    vid: u16,
    pid: u16,
    params: SetParams,
    dry_run: bool,
    sink: &Arc<dyn PacketSink>,
) -> Result<()> {
    eprintln!(
        "Reminder: Fully exit official vendor apps (G Hub, ATK HUB, Lamzu software) before live HID writes (dry_run={dry_run})."
    );

    let enumerator = HidapiEnumerator::new();
    let devices = enumerator
        .enumerate()
        .context("failed to enumerate Windows HID interfaces")?;
    let registry = build_registry();

    let mut target = None;
    for desc in &devices {
        if desc.vid != vid || desc.pid != pid {
            continue;
        }
        let Some(matched) = registry.match_interface(&desc.to_match_descriptor()) else {
            continue;
        };
        if let Ok(transport) = enumerator.open_path(desc)
            && let Ok(peripheral) =
                open_matched_driver(&matched.driver_id, transport, dry_run, Arc::clone(sink))
        {
            target = Some(peripheral);
            break;
        }
    }

    let mut peripheral = target.with_context(|| {
        format!("no supported peripheral answered at VID=0x{vid:04X} PID=0x{pid:04X}")
    })?;

    if let Some(dpi) = params.dpi {
        let ptr = peripheral
            .as_pointer_mut()
            .context("device does not expose PointerDevice capability")?;
        let stage = DpiStage::new(0, DpiValue::new(dpi)?);
        ptr.set_dpi_stages(&[stage], 0)?;
        println!("Applied DPI={dpi} (dry_run={dry_run}).");
    }

    if let Some(hz) = params.polling_rate {
        let ptr = peripheral
            .as_pointer_mut()
            .context("device does not expose PointerDevice capability")?;
        let rate = PollingRateHz::from_hz(hz)?;
        ptr.set_polling_rate(rate)?;
        println!("Applied polling_rate={rate} (dry_run={dry_run}).");
    }

    if let Some(lod) = params.lod_tenths_mm {
        let ptr = peripheral
            .as_pointer_mut()
            .context("device does not expose PointerDevice capability")?;
        ptr.set_lod(LiftOffDistance::TenthsMm(lod))?;
        println!(
            "Applied LOD={}.{} mm (dry_run={dry_run}).",
            lod / 10,
            lod % 10
        );
    }

    if let Some(ms) = params.debounce_ms {
        let ptr = peripheral
            .as_pointer_mut()
            .context("device does not expose PointerDevice capability")?;
        ptr.set_debounce_ms(DebounceMs::new(ms))?;
        println!("Applied debounce={ms} ms (dry_run={dry_run}).");
    }

    if let Some(enabled) = params.motion_sync {
        let ptr = peripheral
            .as_pointer_mut()
            .context("device does not expose PointerDevice capability")?;
        ptr.set_motion_sync(enabled)?;
        println!("Applied motion_sync={enabled} (dry_run={dry_run}).");
    }

    if let Some(slot) = params.profile_slot {
        let prof = peripheral
            .as_profiles_mut()
            .context("device does not expose ProfileManager capability")?;
        prof.set_active_profile(ProfileSlot::new(slot))?;
        println!("Applied active profile slot={slot} (dry_run={dry_run}).");
    }

    apply_hall_effect_set_params(peripheral.as_mut(), params, dry_run)
}

fn apply_hall_effect_set_params(
    peripheral: &mut dyn Peripheral,
    params: SetParams,
    dry_run: bool,
) -> Result<()> {
    if let Some(hundredths) = params.actuation_hundredths_mm {
        let he = peripheral
            .as_hall_effect_mut()
            .context("device does not expose HallEffectKeyboard capability")?;
        let point = TravelHundredthsMm::from_hundredths(hundredths);
        if let Some(key_ix) = params.key_index {
            he.set_key_actuation(KeyIndex::new(key_ix), point)?;
            println!("Applied key[{key_ix}] actuation={point} (dry_run={dry_run}).");
        } else {
            he.set_global_actuation(point)?;
            println!("Applied global actuation={point} (dry_run={dry_run}).");
        }
    }

    if let Some(rt_enabled) = params.rapid_trigger {
        let he = peripheral
            .as_hall_effect_mut()
            .context("device does not expose HallEffectKeyboard capability")?;
        let key_ix = params
            .key_index
            .context("--rapid-trigger requires --key-index")?;
        let press =
            TravelHundredthsMm::from_hundredths(params.rt_press_hundredths_mm.unwrap_or(20));
        let release =
            TravelHundredthsMm::from_hundredths(params.rt_release_hundredths_mm.unwrap_or(20));
        he.set_rapid_trigger(
            KeyIndex::new(key_ix),
            RapidTriggerConfig::new(rt_enabled, rt_enabled, press, release),
        )?;
        println!(
            "Applied key[{key_ix}] rapid_trigger={rt_enabled} (press={press}, release={release}, dry_run={dry_run})."
        );
    }

    Ok(())
}

fn parse_u16_hex(input: &str) -> Result<u16, String> {
    let trimmed = input
        .trim()
        .strip_prefix("0x")
        .or_else(|| input.trim().strip_prefix("0X"))
        .unwrap_or_else(|| input.trim());
    u16::from_str_radix(trimmed, 16).map_err(|err| format!("invalid hex u16 '{input}': {err}"))
}

fn truncate(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        value.to_owned()
    } else if max_chars <= 1 {
        "…".to_owned()
    } else {
        let mut s: String = value.chars().take(max_chars - 1).collect();
        s.push('…');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_ids_with_or_without_prefix() {
        assert_eq!(parse_u16_hex("0x046D").unwrap(), 0x046D);
        assert_eq!(parse_u16_hex("0X373b").unwrap(), 0x373B);
        assert_eq!(parse_u16_hex("ff01").unwrap(), 0xFF01);
        assert!(parse_u16_hex("ZZZZ").is_err());
    }

    #[test]
    fn truncates_strings_safely_on_char_boundaries() {
        assert_eq!(truncate("Logitech", 10), "Logitech");
        assert_eq!(truncate("Logitech G Pro X", 10), "Logitech …");
    }

    #[test]
    fn registry_registers_all_three_vendor_drivers() {
        let registry = build_registry();
        assert_eq!(registry.drivers().len(), 3);
    }
}
