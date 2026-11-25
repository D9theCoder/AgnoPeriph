//! MCHOSE peripheral driver implementing `Peripheral`, `PointerDevice`, `BatteryMonitor`,
//! and `ProfileManager` across MCHOSE V2 (`0x11`/`0x12`), V3 (`0x4D`), and A5 Gen1 (`0x2023`)
//! HID protocols.
//!
//! Wire formats are documented in `docs/protocols/mchose.md` and ported from
//! `OpenMouse-Project/mouse-protocol` (`src/mchose/*`, `src/drivers/mchose/*`).

#![forbid(unsafe_code)]

pub mod protocol_a5_gen1;
pub mod protocol_v2;
pub mod protocol_v3;

mod a5_ops;
mod v2_ops;
mod v3_ops;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, ConnectionKind, CoreError, CoreResult, DebounceMs,
    DeviceId, DeviceInfo, DeviceMatchRule, DpiStage, DriverDescriptor, LiftOffDistance, Peripheral,
    PointerCapabilities, PointerDevice, PointerSnapshot, PollingRateHz, ProfileCapabilities,
    ProfileManager, ProfileSlot, ProfileSnapshot, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidDeviceDescriptor, HidTransport, PacketSink};

use crate::protocol_a5_gen1 as a5;
use crate::protocol_v2 as v2;
use crate::protocol_v3 as v3;

/// MCHOSE MagDock (`UP=0xFF00`, WCH dock-lighting protocol); never claimed as a mouse.
pub const MAGDOCK_PID: u16 = 0x1012;

pub const DRIVER_ID: &str = "mchose-hid";

#[cfg(not(test))]
pub(crate) const fn delay_ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

#[cfg(test)]
pub(crate) const fn delay_ms(_ms: u64) -> Duration {
    Duration::ZERO
}

pub(crate) fn pause(duration: Duration) {
    if !duration.is_zero() {
        std::thread::sleep(duration);
    }
}

/// Wire protocol generation spoken by a `(vid, pid)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolFamily {
    V2,
    V3,
    A5Gen1,
}

/// Classifies a USB identity; MagDock and unlisted PIDs yield `None`.
pub fn classify(vid: u16, pid: u16) -> Option<ProtocolFamily> {
    if vid == v2::MCHOSE_VID && pid == MAGDOCK_PID {
        return None;
    }
    if v3::is_v3_device(vid, pid) {
        Some(ProtocolFamily::V3)
    } else if v2::is_v2_device(vid, pid) {
        Some(ProtocolFamily::V2)
    } else if a5::is_a5_device(vid, pid) {
        Some(ProtocolFamily::A5Gen1)
    } else {
        None
    }
}

fn config_rule(vid: u16, pid: u16, connection: ConnectionKind, hint: &str) -> DeviceMatchRule {
    DeviceMatchRule::new(vid, connection, hint)
        .with_pid(pid)
        .with_usage_page(v2::CONFIG_USAGE_PAGE)
        .with_usage_id(v2::CONFIG_USAGE)
}

/// Returns the declarative `DriverDescriptor` with explicit PID rules for every MCHOSE
/// V2, V3, and A5 Gen1 interface. MagDock (`0x3837:0x1012`) is deliberately absent.
pub fn driver_descriptor() -> DriverDescriptor {
    let mut rules = Vec::new();

    for &pid in v2::WIRED_MCHOSE_PIDS {
        let name = v2::product_by_pid(pid).map_or("MCHOSE V2 Mouse", |p| p.name);
        rules.push(config_rule(
            v2::MCHOSE_VID,
            pid,
            ConnectionKind::WiredUsb,
            &format!("MCHOSE {name}"),
        ));
    }
    for &pid in v2::WIRED_REALTEK_PIDS {
        let name = v2::product_by_pid(pid).map_or("MCHOSE V2 Mouse", |p| p.name);
        rules.push(config_rule(
            v2::REALTEK_VID,
            pid,
            ConnectionKind::WiredUsb,
            &format!("MCHOSE {name}"),
        ));
    }
    rules.push(config_rule(
        v2::MCHOSE_VID,
        v2::RECEIVER_24G_PID,
        ConnectionKind::WirelessDongle24Ghz,
        "MCHOSE V2 2.4G Receiver",
    ));
    rules.push(config_rule(
        v2::MCHOSE_VID,
        v2::BLUETOOTH_PID,
        ConnectionKind::Bluetooth,
        "MCHOSE V2 Bluetooth",
    ));
    for vid in [v2::MCHOSE_VID, v2::REALTEK_VID] {
        rules.push(config_rule(
            vid,
            v2::RECEIVER_8K_PID,
            ConnectionKind::WirelessDongle24Ghz,
            "MCHOSE V2 8K Receiver",
        ));
    }

    for product in v3::PRODUCTS {
        rules.push(config_rule(
            v3::MCHOSE_VID,
            product.pid,
            ConnectionKind::WiredUsb,
            &format!("MCHOSE {}", product.name),
        ));
    }
    for (pid, hint) in [
        (v3::RECEIVER_24G_PID, "MCHOSE V3 2.4G Receiver"),
        (v3::RECEIVER_1K_PID, "MCHOSE V3 1K Receiver"),
        (v3::RECEIVER_8K_PID, "MCHOSE V3 8K Receiver"),
    ] {
        rules.push(config_rule(
            v3::MCHOSE_VID,
            pid,
            ConnectionKind::WirelessDongle24Ghz,
            hint,
        ));
    }

    for (pid, connection, hint) in [
        (a5::WIRED_PID, ConnectionKind::WiredUsb, "MCHOSE A5 Pro Max"),
        (
            a5::RECEIVER_1K_PID,
            ConnectionKind::WirelessDongle24Ghz,
            "MCHOSE A5 Pro Max (1K Receiver)",
        ),
        (
            a5::RECEIVER_4K_PID,
            ConnectionKind::WirelessDongle24Ghz,
            "MCHOSE A5 Pro Max (4K Receiver)",
        ),
    ] {
        rules.push(
            DeviceMatchRule::new(a5::VID, connection, hint)
                .with_pid(pid)
                .with_usage_page(a5::USAGE_PAGE),
        );
    }

    DriverDescriptor::new(DRIVER_ID, VendorKind::Mchose, rules)
}

/// Resolved model and link for the opened interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Model {
    V2 {
        product: Option<&'static v2::Product>,
        wired: bool,
        bluetooth: bool,
    },
    V3 {
        product: Option<&'static v3::Product>,
        wired: bool,
    },
    A5 {
        wired: bool,
    },
}

/// Resolves the link and (for wired interfaces) the model from USB identity alone.
fn initial_model(family: ProtocolFamily, desc: &HidDeviceDescriptor) -> (Model, ConnectionKind) {
    match family {
        ProtocolFamily::V2 => {
            let wired = v2::WIRED_MCHOSE_PIDS.contains(&desc.pid)
                || v2::WIRED_REALTEK_PIDS.contains(&desc.pid);
            let bluetooth = desc.pid == v2::BLUETOOTH_PID;
            let product = if wired {
                v2::product_by_pid(desc.pid)
            } else {
                None
            };
            let connection = if wired {
                ConnectionKind::WiredUsb
            } else if bluetooth {
                ConnectionKind::Bluetooth
            } else {
                ConnectionKind::WirelessDongle24Ghz
            };
            (
                Model::V2 {
                    product,
                    wired,
                    bluetooth,
                },
                connection,
            )
        }
        ProtocolFamily::V3 => {
            let wired = !v3::is_receiver(desc.pid);
            let product =
                v3::find_product(desc.product.as_deref(), wired.then_some(desc.pid), None);
            let connection = if wired {
                ConnectionKind::WiredUsb
            } else {
                ConnectionKind::WirelessDongle24Ghz
            };
            (Model::V3 { product, wired }, connection)
        }
        ProtocolFamily::A5Gen1 => {
            let wired = desc.pid == a5::WIRED_PID;
            let connection = if wired {
                ConnectionKind::WiredUsb
            } else {
                ConnectionKind::WirelessDongle24Ghz
            };
            (Model::A5 { wired }, connection)
        }
    }
}

/// Active MCHOSE mouse (V2, V3, or A5 Gen1) behind a strictly validated [`GuardedTransport`].
pub struct MchoseDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    model: Model,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
    profile_caps: ProfileCapabilities,
}

impl<T: HidTransport> fmt::Debug for MchoseDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MchoseDevice")
            .field("model", &self.model)
            .field("info", &self.info)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> MchoseDevice<T> {
    /// Opens a MCHOSE configuration interface, installs the family's packet validator,
    /// and resolves the model with read-only queries (non-fatal if the mouse is asleep).
    ///
    /// The MagDock (`0x3837:0x1012`) is refused.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid == v2::MCHOSE_VID && desc.pid == MAGDOCK_PID {
            return Err(CoreError::Validation(
                "MCHOSE MagDock 0x3837:0x1012 uses a dock-lighting protocol and is not a mouse"
                    .to_owned(),
            ));
        }
        let family = classify(desc.vid, desc.pid).ok_or_else(|| {
            CoreError::Validation(format!(
                "unsupported MCHOSE device 0x{:04X}:0x{:04X}",
                desc.vid, desc.pid
            ))
        })?;

        let usage_ok = match family {
            ProtocolFamily::V2 | ProtocolFamily::V3 => {
                desc.usage_page == v2::CONFIG_USAGE_PAGE && desc.usage_id == v2::CONFIG_USAGE
            }
            ProtocolFamily::A5Gen1 => desc.usage_page == a5::USAGE_PAGE,
        };
        if !usage_ok {
            return Err(CoreError::Validation(format!(
                "MCHOSE {family:?} interface must be the vendor configuration collection, got UP=0x{:04X} U=0x{:04X}",
                desc.usage_page, desc.usage_id
            )));
        }

        let validator = match family {
            ProtocolFamily::V2 => v2::v2_validator(),
            ProtocolFamily::V3 => v3::v3_validator(),
            ProtocolFamily::A5Gen1 => a5::a5_validator(),
        };
        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(validator))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let (model, connection) = initial_model(family, &desc);

        let stable_id = DeviceId::new(format!(
            "mchose:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));
        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Mchose,
            "MCHOSE Mouse",
            connection,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number.clone() {
            info = info.with_serial_number(serial);
        }

        let mut dev = Self {
            transport: guarded,
            model,
            info,
            capabilities: Capabilities::empty(),
            pointer_caps: PointerCapabilities::new(1, 1, 1, 1),
            profile_caps: ProfileCapabilities::new(0, false, false),
        };

        dev.probe();
        Ok(dev)
    }

    /// Runs the family's read-only identity probe and rebuilds model name and capabilities.
    fn probe(&mut self) {
        let usb_product = self.transport.descriptor().product.clone();
        let host_pid = self.info.pid;
        let profile_count = match self.model {
            Model::V2 { .. } => {
                self.v2_probe(usb_product.as_deref());
                v2::PROFILE_COUNT
            }
            Model::V3 { .. } => self.v3_probe(usb_product.as_deref(), host_pid),
            Model::A5 { .. } => {
                self.a5_probe();
                a5::PROFILE_COUNT
            }
        };

        self.info.model_name = self.model_name(usb_product.as_deref());
        self.pointer_caps = self.build_pointer_caps();
        self.profile_caps =
            ProfileCapabilities::new(profile_count, false, matches!(self.model, Model::V2 { .. }));
        self.capabilities = Capabilities::empty()
            .with_pointer(true)
            .with_battery(true)
            .with_profiles(profile_count > 0);
    }

    /// `true` when a receiver was opened while its mouse was asleep and the model (or V3
    /// profile count) could not be resolved yet.
    const fn needs_reprobe(&self) -> bool {
        match self.model {
            Model::V2 { product, .. } => product.is_none(),
            Model::V3 { product, .. } => product.is_none() || self.profile_caps.slot_count == 0,
            Model::A5 { .. } => false,
        }
    }

    /// Returns the underlying [`GuardedTransport`] for inspecting traces or mock recordings.
    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    /// Returns the wire protocol generation spoken by this interface.
    pub const fn protocol_family(&self) -> ProtocolFamily {
        match self.model {
            Model::V2 { .. } => ProtocolFamily::V2,
            Model::V3 { .. } => ProtocolFamily::V3,
            Model::A5 { .. } => ProtocolFamily::A5Gen1,
        }
    }

    fn model_name(&self, usb_product: Option<&str>) -> String {
        let resolved = match self.model {
            Model::V2 { product, .. } => product.map(|p| p.name),
            Model::V3 { product, .. } => product.map(|p| p.name),
            Model::A5 { .. } => Some("A5 Pro Max"),
        };
        if let Some(name) = resolved {
            return format!("MCHOSE {name}");
        }
        match usb_product.map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) if name.to_uppercase().starts_with("MCHOSE") => name.to_owned(),
            Some(name) => format!("MCHOSE {name}"),
            None => "MCHOSE Mouse".to_owned(),
        }
    }

    fn build_pointer_caps(&self) -> PointerCapabilities {
        let to_lods = |tenths: &[u8]| -> Vec<LiftOffDistance> {
            tenths
                .iter()
                .map(|t| LiftOffDistance::TenthsMm(*t))
                .collect()
        };
        match self.model {
            Model::V2 {
                product, bluetooth, ..
            } => {
                let dpi_max = product.map_or(26_000, |p| p.dpi_max);
                let max_hz = product.map_or(8000, |p| p.max_polling_hz);
                PointerCapabilities::new(
                    v2::DPI_MIN,
                    dpi_max,
                    v2::DPI_STEP,
                    u8::try_from(v2::DPI_STAGES).unwrap_or(6),
                )
                .with_polling_rates(v2::polling_rates(max_hz, bluetooth).to_vec())
                .with_lods(product.map_or_default(|p| to_lods(p.lods_tenths_mm)))
                .with_debounce_range_ms(0, v2::DEBOUNCE_MAX_MS)
                .with_motion_sync(true)
                .with_angle_snapping(true)
                .with_ripple_control(true)
            }
            Model::V3 { product, wired } => {
                let dpi_max = product.map_or(26_000, |p| p.dpi_max);
                let rates = match product {
                    Some(p) => v3::polling_rates(p.max_polling_hz).to_vec(),
                    None if wired => Vec::new(),
                    None => {
                        let pid = self.info.pid;
                        if pid == v3::RECEIVER_1K_PID {
                            v3::polling_rates(1000).to_vec()
                        } else {
                            v3::polling_rates(8000).to_vec()
                        }
                    }
                };
                PointerCapabilities::new(
                    v3::DPI_MIN,
                    dpi_max,
                    v3::DPI_STEP,
                    u8::try_from(v3::DPI_STAGES).unwrap_or(6),
                )
                .with_polling_rates(rates)
                .with_lods(product.map_or_default(|p| to_lods(p.lods_tenths_mm)))
                .with_debounce_range_ms(0, v3::DEBOUNCE_MAX_MS)
                .with_motion_sync(true)
                .with_angle_snapping(true)
                .with_ripple_control(true)
            }
            Model::A5 { wired } => PointerCapabilities::new(
                a5::DPI_MIN,
                a5::DPI_MAX,
                a5::DPI_STEP,
                u8::try_from(a5::DPI_STAGES).unwrap_or(6),
            )
            .with_polling_rates(a5::polling_rates(wired))
            .with_lods(vec![LiftOffDistance::Low, LiftOffDistance::High])
            .with_debounce_range_ms(0, a5::DEBOUNCE_MAX_MS)
            .with_motion_sync(true)
            .with_angle_snapping(true)
            .with_ripple_control(true),
        }
    }

    /// Converts validated domain DPI to the 16-bit wire value.
    pub(crate) fn dpi_to_wire(dpi: u32) -> CoreResult<u16> {
        u16::try_from(dpi).map_err(|_| CoreError::OutOfRange {
            field: "dpi",
            actual: dpi.to_string(),
            expected: "<= 65535".to_owned(),
        })
    }

    pub(crate) fn stages_from_wire(values: &[u16]) -> CoreResult<Vec<DpiStage>> {
        values
            .iter()
            .enumerate()
            .map(|(ix, v)| {
                let dpi = agnoperiph_core::DpiValue::new(u32::from(*v)).map_err(|_| {
                    CoreError::Protocol(format!("MCHOSE reported zero DPI for stage {ix}"))
                })?;
                Ok(DpiStage::new(u8::try_from(ix).unwrap_or(0), dpi))
            })
            .collect()
    }

    pub(crate) fn rate_index(&self, rate: PollingRateHz) -> CoreResult<u8> {
        self.pointer_caps.validate_polling_rate(rate)?;
        let ix = self
            .pointer_caps
            .supported_polling_rates
            .iter()
            .position(|r| *r == rate)
            .unwrap_or(0);
        Ok(u8::try_from(ix).unwrap_or(0))
    }

    pub(crate) fn rate_from_index(&self, index: u8) -> CoreResult<PollingRateHz> {
        self.pointer_caps
            .supported_polling_rates
            .get(usize::from(index))
            .copied()
            .ok_or_else(|| {
                CoreError::Protocol(format!(
                    "MCHOSE polling index {index} is outside this model's rate list"
                ))
            })
    }

    pub(crate) fn lod_position(&self, lod: LiftOffDistance) -> CoreResult<u8> {
        self.pointer_caps.validate_lod(lod)?;
        let pos = self
            .pointer_caps
            .supported_lods
            .iter()
            .position(|l| *l == lod)
            .unwrap_or(0);
        Ok(u8::try_from(pos).unwrap_or(0))
    }

    pub(crate) fn not_confirmed(what: &str) -> CoreError {
        CoreError::Protocol(format!("MCHOSE mouse did not confirm the {what} change"))
    }
}

impl<T: HidTransport> Peripheral for MchoseDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        if self.needs_reprobe() {
            self.probe();
        }
        let firmware = match self.model {
            Model::V2 { .. } => self.v2_read_firmware()?,
            Model::V3 { .. } => self.v3_refresh()?,
            Model::A5 { .. } => Some(self.a5_read_firmware()?),
        };
        if let Some(version) = firmware {
            self.info.firmware_version = Some(version);
        }
        Ok(())
    }

    fn as_pointer(&self) -> Option<&dyn PointerDevice> {
        Some(self)
    }

    fn as_pointer_mut(&mut self) -> Option<&mut dyn PointerDevice> {
        Some(self)
    }

    fn as_battery(&self) -> Option<&dyn BatteryMonitor> {
        Some(self)
    }

    fn as_battery_mut(&mut self) -> Option<&mut dyn BatteryMonitor> {
        Some(self)
    }

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        self.capabilities
            .has_profiles()
            .then_some(self as &dyn ProfileManager)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        if self.capabilities.has_profiles() {
            Some(self)
        } else {
            None
        }
    }
}

impl<T: HidTransport> BatteryMonitor for MchoseDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        match self.model {
            Model::V2 { .. } => self.v2_read_battery(),
            Model::V3 { .. } => self.v3_read_battery(),
            Model::A5 { .. } => self.a5_read_battery(),
        }
    }
}

impl<T: HidTransport> PointerDevice for MchoseDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        match self.model {
            Model::V2 { .. } => self.v2_read_pointer(),
            Model::V3 { .. } => self.v3_read_pointer(),
            Model::A5 { .. } => self.a5_read_pointer(),
        }
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        match self.model {
            Model::V2 { .. } => self.v2_set_dpi_stages(stages, active_ix),
            Model::V3 { .. } => self.v3_set_dpi_stages(stages, active_ix),
            Model::A5 { .. } => self.a5_set_dpi_stages(stages, active_ix),
        }
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if usize::from(stage_ix) >= usize::from(self.pointer_caps.max_dpi_stages) {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{}", self.pointer_caps.max_dpi_stages),
            });
        }
        match self.model {
            Model::V2 { .. } => self.v2_set_active_stage(stage_ix),
            Model::V3 { .. } => self.v3_set_active_stage(stage_ix),
            Model::A5 { .. } => self.a5_set_active_stage(stage_ix),
        }
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        let index = self.rate_index(rate)?;
        match self.model {
            Model::V2 { .. } => self.v2_set_polling(index),
            Model::V3 { .. } => self.v3_set_polling(index),
            Model::A5 { .. } => self.a5_set_polling(rate),
        }
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        let position = self.lod_position(lod)?;
        match self.model {
            Model::V2 { .. } => self.v2_set_lod(position),
            Model::V3 { .. } => self.v3_set_lod(position),
            Model::A5 { .. } => self.a5_set_lod(lod),
        }
    }

    fn set_debounce_ms(&mut self, debounce: DebounceMs) -> CoreResult<()> {
        self.pointer_caps.validate_debounce(debounce)?;
        match self.model {
            Model::V2 { .. } => self.v2_set_debounce(debounce.value()),
            Model::V3 { .. } => self.v3_set_debounce(debounce.value()),
            Model::A5 { .. } => self.a5_set_debounce(debounce.value()),
        }
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        self.set_toggle(Toggle::MotionSync, enabled)
    }

    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        self.set_toggle(Toggle::AngleSnapping, enabled)
    }

    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        self.set_toggle(Toggle::RippleControl, enabled)
    }
}

/// Sensor processing toggle shared by all three protocol families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Toggle {
    MotionSync,
    AngleSnapping,
    RippleControl,
}

impl<T: HidTransport> MchoseDevice<T> {
    fn set_toggle(&mut self, toggle: Toggle, enabled: bool) -> CoreResult<()> {
        match self.model {
            Model::V2 { .. } => self.v2_set_toggle(toggle, enabled),
            Model::V3 { .. } => self.v3_set_toggle(toggle, enabled),
            Model::A5 { .. } => self.a5_set_toggle(toggle, enabled),
        }
    }
}

impl<T: HidTransport> ProfileManager for MchoseDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        self.profile_caps.clone()
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        match self.model {
            Model::V2 { .. } => self.v2_read_profiles(),
            Model::V3 { .. } => self.v3_read_profiles(),
            Model::A5 { .. } => self.a5_read_profiles(),
        }
    }

    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()> {
        self.profile_caps.validate_slot(slot)?;
        match self.model {
            Model::V2 { .. } => self.v2_set_profile(slot.index()),
            Model::V3 { .. } => self.v3_set_profile(slot.index()),
            Model::A5 { .. } => self.a5_set_profile(slot.index()),
        }
    }
}

#[cfg(test)]
mod tests;
