//! Razer 90-byte HID Feature report peripheral driver.

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Brightness, Capabilities, CoreError, CoreResult, DeviceId,
    DeviceInfo, DeviceMatchRule, DpiStage, DriverDescriptor, LiftOffDistance, Peripheral,
    PointerCapabilities, PointerDevice, PointerSnapshot, PollingRateHz, RgbCapabilities,
    RgbController, RgbEffect, RgbEffectKind, RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState,
    VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink};

use crate::protocol::{
    RAZER_DPI_STEP, RAZER_HID_BUFFER_LEN, RAZER_MAX_DPI_STAGES, RAZER_MIN_DPI, RAZER_PRODUCTS,
    RAZER_REPORT_ID, RAZER_USAGE_ID_MOUSE, RAZER_USAGE_PAGE_MOUSE, RAZER_VID, RazerCommand,
    RazerLightingFamily, RazerProductProfile, RazerReply, TX_1F, cmd_read_battery,
    cmd_read_charging, cmd_read_dpi, cmd_read_dpi_stages, cmd_read_extended_brightness,
    cmd_read_firmware, cmd_read_lift_off, cmd_read_polling_extended, cmd_read_polling_legacy,
    cmd_read_serial, cmd_read_standard_brightness, cmd_set_dpi, cmd_set_dpi_stages,
    cmd_set_extended_brightness, cmd_set_extended_effect, cmd_set_polling_extended,
    cmd_set_polling_legacy, cmd_set_standard_brightness, cmd_set_standard_effect,
    cmd_set_tracking_distance, decode_battery, decode_brightness_percent, decode_dpi_stages,
    decode_dpi_xy, decode_firmware, decode_lift_off, decode_polling_extended,
    decode_polling_legacy, decode_response, decode_serial, default_static_color, encode_request,
    led, lookup_product, razer_validator, supported_rates_for,
};

const MAX_EXCHANGE_ATTEMPTS: usize = 4;
const MAX_BUSY_POLLS: usize = 6;

/// Declarative `DriverDescriptor` claiming audited Razer wired & HyperSpeed receiver PIDs.
pub fn driver_descriptor() -> DriverDescriptor {
    let mut rules = Vec::with_capacity(RAZER_PRODUCTS.len() * 2);
    for profile in RAZER_PRODUCTS {
        rules.push(
            DeviceMatchRule::new(RAZER_VID, profile.connection, profile.model_name)
                .with_pid(profile.pid)
                .with_usage_page(RAZER_USAGE_PAGE_MOUSE)
                .with_usage_id(RAZER_USAGE_ID_MOUSE),
        );
        if profile.vendor_control_interface {
            rules.push(
                DeviceMatchRule::new(RAZER_VID, profile.connection, profile.model_name)
                    .with_pid(profile.pid)
                    .with_usage_page(0xFF00),
            );
        }
    }
    DriverDescriptor::new("razer-hid", VendorKind::Razer, rules)
}

/// Active Razer mouse/receiver backed by a checksum-validated [`GuardedTransport`].
pub struct RazerDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    profile: RazerProductProfile,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
    rgb_caps: Option<RgbCapabilities>,
    cached_rgb: Vec<RgbZoneState>,
}

impl<T: HidTransport> fmt::Debug for RazerDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RazerDevice")
            .field("profile", &self.profile)
            .field("info", &self.info)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> RazerDevice<T> {
    /// Connects to a Razer mouse or wireless receiver, enforces 91-byte XOR-checksum validation,
    /// and reads firmware version & serial number.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != RAZER_VID {
            return Err(CoreError::Validation(format!(
                "expected Razer VID 0x{RAZER_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        let profile = lookup_product(desc.pid).ok_or_else(|| {
            CoreError::Validation(format!("unsupported Razer PID 0x{:04X}", desc.pid))
        })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(razer_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "razer:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let mut pointer_caps = PointerCapabilities::new(
            RAZER_MIN_DPI,
            profile.max_dpi,
            RAZER_DPI_STEP,
            RAZER_MAX_DPI_STAGES,
        )
        .with_xy_independent_dpi(true)
        .with_polling_rates(supported_rates_for(profile));
        if profile.supports_lift_off {
            pointer_caps = pointer_caps.with_lods(vec![
                LiftOffDistance::Low,
                LiftOffDistance::Medium,
                LiftOffDistance::High,
            ]);
        }

        let (rgb_caps, cached_rgb) = build_rgb_state(profile.lighting);
        let capabilities = Capabilities::empty()
            .with_pointer(true)
            .with_battery(profile.has_battery)
            .with_rgb(rgb_caps.is_some());

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Razer,
            profile.model_name,
            profile.connection,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let mut dev = Self {
            transport: guarded,
            profile,
            info,
            capabilities,
            pointer_caps,
            rgb_caps,
            cached_rgb,
        };

        // Read-only identity probe on open.
        if let Ok(fw_args) = dev.exchange(&cmd_read_firmware(), None)
            && let Some(fw) = decode_firmware(&fw_args)
        {
            dev.info.firmware_version = Some(fw);
        }
        if let Ok(serial_args) = dev.exchange(&cmd_read_serial(), None)
            && let Some(serial) = decode_serial(&serial_args)
        {
            dev.info.serial_number = Some(serial);
        }

        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn exchange(&mut self, command: &RazerCommand, tx_override: Option<u8>) -> CoreResult<Vec<u8>> {
        let tx_id = tx_override.unwrap_or(self.profile.transaction_id);
        let request = encode_request(command, tx_id)?;

        if !command.is_getter() && self.transport.is_dry_run() {
            self.transport.send_feature_report(&request)?;
            return Ok(Vec::new());
        }

        for _ in 0..MAX_EXCHANGE_ATTEMPTS {
            if command.is_getter() {
                self.transport.send_query_feature_report(&request)?;
            } else {
                self.transport.send_feature_report(&request)?;
            }

            let mut retry_send = false;
            for _ in 0..MAX_BUSY_POLLS {
                let mut buf = [0u8; RAZER_HID_BUFFER_LEN];
                buf[0] = RAZER_REPORT_ID;
                let n = self.transport.get_feature_report(&mut buf)?;
                match decode_response(&buf[..n], command) {
                    Ok(RazerReply::Ok(args)) => return Ok(args),
                    Ok(RazerReply::Busy) => {}
                    Ok(RazerReply::Unsupported) => {
                        return Err(CoreError::UnsupportedCapability("razer_command"));
                    }
                    Ok(RazerReply::Stale) => {
                        if command.is_getter() {
                            retry_send = true;
                            break;
                        }
                        return Err(CoreError::Protocol(format!(
                            "Razer setter 0x{:02X}/0x{:02X} answered by stale reply",
                            command.command_class, command.command_id
                        )));
                    }
                    Ok(RazerReply::Error(code)) => {
                        return Err(CoreError::Protocol(format!(
                            "Razer command 0x{:02X}/0x{:02X} failed with status 0x{code:02X}",
                            command.command_class, command.command_id
                        )));
                    }
                    Err(_) if command.is_getter() => {
                        retry_send = true;
                        break;
                    }
                    Err(err) => return Err(err),
                }
            }
            if !retry_send {
                break;
            }
        }

        Err(CoreError::NotConnected(format!(
            "Razer device stayed busy or unanswered for 0x{:02X}/0x{:02X}",
            command.command_class, command.command_id
        )))
    }

    fn read_polling_rate(&mut self) -> CoreResult<PollingRateHz> {
        if self.profile.high_rate_polling {
            if let Ok(args) = self.exchange(&cmd_read_polling_extended(), None)
                && let Ok(rate) = decode_polling_extended(&args)
            {
                return Ok(rate);
            }
            let args = self.exchange(&cmd_read_polling_legacy(), None)?;
            decode_polling_legacy(&args)
        } else {
            if let Ok(args) = self.exchange(&cmd_read_polling_legacy(), None)
                && let Ok(rate) = decode_polling_legacy(&args)
            {
                return Ok(rate);
            }
            let args = self.exchange(&cmd_read_polling_extended(), None)?;
            decode_polling_extended(&args)
        }
    }
}

fn build_rgb_state(family: RazerLightingFamily) -> (Option<RgbCapabilities>, Vec<RgbZoneState>) {
    let default_brightness =
        Brightness::new(100).unwrap_or_else(|_| unreachable!("100 is valid brightness"));
    let default_effect = RgbEffect::Static {
        color: default_static_color(),
    };
    match family {
        RazerLightingFamily::None => (None, Vec::new()),
        RazerLightingFamily::StandardMatrix => {
            let zones = vec![RgbZoneInfo::new(RgbZoneId::new(led::ALL), "Mouse")];
            let effects = vec![
                RgbEffectKind::Off,
                RgbEffectKind::Static,
                RgbEffectKind::Breathing,
                RgbEffectKind::SpectrumCycle,
                RgbEffectKind::Wave,
                RgbEffectKind::Reactive,
            ];
            let states = vec![RgbZoneState::new(
                RgbZoneId::new(led::ALL),
                default_effect,
                default_brightness,
            )];
            (Some(RgbCapabilities::new(zones, effects, false)), states)
        }
        RazerLightingFamily::ExtendedMatrix => {
            let zones = vec![
                RgbZoneInfo::new(RgbZoneId::new(led::ALL), "Mouse"),
                RgbZoneInfo::new(RgbZoneId::new(led::SCROLL_WHEEL), "Scroll Wheel"),
                RgbZoneInfo::new(RgbZoneId::new(led::LOGO), "Logo"),
            ];
            let effects = vec![
                RgbEffectKind::Off,
                RgbEffectKind::Static,
                RgbEffectKind::Breathing,
                RgbEffectKind::SpectrumCycle,
                RgbEffectKind::Wave,
                RgbEffectKind::Reactive,
            ];
            let states = zones
                .iter()
                .map(|z| RgbZoneState::new(z.id, default_effect, default_brightness))
                .collect();
            (Some(RgbCapabilities::new(zones, effects, false)), states)
        }
    }
}

impl<T: HidTransport> Peripheral for RazerDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let fw = self.exchange(&cmd_read_firmware(), None)?;
        if let Some(version) = decode_firmware(&fw) {
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
        self.capabilities.has_battery().then_some(self)
    }

    fn as_battery_mut(&mut self) -> Option<&mut dyn BatteryMonitor> {
        self.capabilities.has_battery().then_some(self)
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        self.capabilities.has_rgb().then_some(self)
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        self.capabilities.has_rgb().then_some(self)
    }
}

impl<T: HidTransport> BatteryMonitor for RazerDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let level = self.exchange(&cmd_read_battery(), None)?;
        let charging = self.exchange(&cmd_read_charging(), None).ok();
        Ok(decode_battery(&level, charging.as_deref()))
    }
}

impl<T: HidTransport> PointerDevice for RazerDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        let (active_stage, stages) = if let Ok(args) =
            self.exchange(&cmd_read_dpi_stages(self.profile.dpi_storage), None)
            && let Ok(decoded) = decode_dpi_stages(&args)
        {
            decoded
        } else {
            let dpi_args = self.exchange(&cmd_read_dpi(self.profile.dpi_storage), None)?;
            let (x, y) = decode_dpi_xy(&dpi_args)?;
            let mut stage = DpiStage::new(0, x);
            if y != x {
                stage = stage.with_y(y);
            }
            (0, vec![stage])
        };

        let polling_rate = self.read_polling_rate()?;
        let mut snap = PointerSnapshot::new(active_stage, stages, polling_rate);

        if self.profile.supports_lift_off
            && let Ok(lod_args) = self.exchange(&cmd_read_lift_off(), None)
            && let Some(lod) = decode_lift_off(&lod_args)
        {
            snap = snap.with_lod(lod);
        }

        Ok(snap)
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        let cmd = cmd_set_dpi_stages(stages, active_ix, self.profile.dpi_storage)?;
        match self.exchange(&cmd, None) {
            Ok(_) => Ok(()),
            Err(CoreError::UnsupportedCapability(_)) => {
                // Fallback for older Razer models without stage-table writes.
                let active = stages[usize::from(active_ix)];
                let x = u16::try_from(active.x.value()).unwrap_or(800);
                let y = u16::try_from(active.effective_y().value()).unwrap_or(x);
                self.exchange(&cmd_set_dpi(x, y, self.profile.dpi_storage), None)?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        let args = self.exchange(&cmd_read_dpi_stages(self.profile.dpi_storage), None)?;
        let (_, stages) = decode_dpi_stages(&args)?;
        if usize::from(stage_ix) >= stages.len() {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{}", stages.len()),
            });
        }
        let cmd = cmd_set_dpi_stages(&stages, stage_ix, self.profile.dpi_storage)?;
        self.exchange(&cmd, None)?;
        Ok(())
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        if self.profile.high_rate_polling {
            self.exchange(&cmd_set_polling_extended(rate, 0x00)?, None)?;
            if let Some(commit_tx) = self.profile.extended_polling_commit_tx {
                self.exchange(&cmd_set_polling_extended(rate, 0x01)?, Some(commit_tx))?;
            }
        } else {
            self.exchange(&cmd_set_polling_legacy(rate)?, None)?;
        }
        Ok(())
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        self.exchange(&cmd_set_tracking_distance(lod)?, None)?;
        Ok(())
    }
}

impl<T: HidTransport> RgbController for RazerDevice<T> {
    fn rgb_capabilities(&self) -> RgbCapabilities {
        self.rgb_caps
            .clone()
            .unwrap_or_else(|| RgbCapabilities::new(Vec::new(), Vec::new(), false))
    }

    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot> {
        match self.profile.lighting {
            RazerLightingFamily::None => Err(CoreError::UnsupportedCapability("rgb")),
            RazerLightingFamily::StandardMatrix => {
                if let Ok(args) = self.exchange(&cmd_read_standard_brightness(), None)
                    && let Some(pct) = decode_brightness_percent(&args)
                    && let Ok(b) = Brightness::new(pct)
                    && let Some(zone) = self.cached_rgb.first_mut()
                {
                    zone.brightness = b;
                }
                Ok(RgbSnapshot::new(self.cached_rgb.clone()))
            }
            RazerLightingFamily::ExtendedMatrix => {
                let zone_ids: Vec<u8> = self.cached_rgb.iter().map(|z| z.zone.value()).collect();
                for (ix, led_id) in zone_ids.into_iter().enumerate() {
                    if let Ok(args) =
                        self.exchange(&cmd_read_extended_brightness(led_id), Some(TX_1F))
                        && let Some(pct) = decode_brightness_percent(&args)
                        && let Ok(b) = Brightness::new(pct)
                    {
                        self.cached_rgb[ix].brightness = b;
                    }
                }
                Ok(RgbSnapshot::new(self.cached_rgb.clone()))
            }
        }
    }

    fn set_zone_effect(
        &mut self,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()> {
        match self.profile.lighting {
            RazerLightingFamily::None => Err(CoreError::UnsupportedCapability("rgb")),
            RazerLightingFamily::StandardMatrix => {
                self.exchange(&cmd_set_standard_effect(effect)?, None)?;
                self.exchange(&cmd_set_standard_brightness(brightness.percent()), None)?;
                if let Some(entry) = self.cached_rgb.iter_mut().find(|z| z.zone == zone) {
                    entry.effect = effect;
                    entry.brightness = brightness;
                }
                Ok(())
            }
            RazerLightingFamily::ExtendedMatrix => {
                let led_id = zone.value();
                self.exchange(&cmd_set_extended_effect(led_id, effect)?, Some(TX_1F))?;
                self.exchange(
                    &cmd_set_extended_brightness(led_id, brightness.percent()),
                    Some(TX_1F),
                )?;
                if let Some(entry) = self.cached_rgb.iter_mut().find(|z| z.zone == zone) {
                    entry.effect = effect;
                    entry.brightness = brightness;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{compute_checksum, status};
    use agnoperiph_core::{ChargingState, DpiValue, RgbColor};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    fn make_reply(cmd_class: u8, cmd_id: u8, tx_id: u8, args: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; RAZER_HID_BUFFER_LEN];
        buf[0] = RAZER_REPORT_ID;
        let pkt = &mut buf[1..];
        pkt[0] = status::OK;
        pkt[1] = tx_id;
        pkt[5] = u8::try_from(args.len()).unwrap_or(0);
        pkt[6] = cmd_class;
        pkt[7] = cmd_id;
        pkt[8..8 + args.len()].copy_from_slice(args);
        pkt[88] = compute_checksum(pkt);
        buf
    }

    fn viper_v3_pro_dongle_desc() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            RAZER_VID,
            0x00C1,
            RAZER_USAGE_PAGE_MOUSE,
            RAZER_USAGE_ID_MOUSE,
            0,
            "mock://razer/viper-v3-pro-8k",
        )
    }

    #[test]
    fn checksum_worked_example_matches_spec() {
        let req = encode_request(&cmd_read_firmware(), TX_1F).unwrap();
        assert_eq!(req[0], 0x00);
        assert_eq!(req[1], 0x00); // status
        assert_eq!(req[2], TX_1F);
        assert_eq!(req[6], 0x02); // data_size
        assert_eq!(req[7], 0x00); // class
        assert_eq!(req[8], 0x81); // cmd
        assert_eq!(req[89], 0x83); // checksum at pkt[88]
    }

    #[test]
    fn reads_viper_v3_pro_identity_battery_and_pointer_state() {
        let mut mock = MockHidTransport::new(viper_v3_pro_dongle_desc());
        // Open: firmware 1.12, serial "RZ-TEST-01"
        mock.push_feature_response(make_reply(0x00, 0x81, TX_1F, &[1, 12]));
        let mut serial_bytes = vec![0u8; 0x16];
        serial_bytes[..10].copy_from_slice(b"RZ-TEST-01");
        mock.push_feature_response(make_reply(0x00, 0x82, TX_1F, &serial_bytes));

        // Battery: 0xCC (~80%), charging = 1
        mock.push_feature_response(make_reply(0x07, 0x80, TX_1F, &[0x00, 0xCC]));
        mock.push_feature_response(make_reply(0x07, 0x84, TX_1F, &[0x00, 0x01]));

        // DPI stages: active=2 (1-based => index 1), count=2: 800 DPI, 1600x1800 DPI
        let mut stages = vec![0u8; 0x26];
        stages[0] = 0x01;
        stages[1] = 2;
        stages[2] = 2;
        stages[3] = 0;
        stages[4..6].copy_from_slice(&800u16.to_be_bytes());
        stages[6..8].copy_from_slice(&800u16.to_be_bytes());
        stages[10] = 1;
        stages[11..13].copy_from_slice(&1600u16.to_be_bytes());
        stages[13..15].copy_from_slice(&1800u16.to_be_bytes());
        mock.push_feature_response(make_reply(0x04, 0x86, TX_1F, &stages));
        // Extended polling: 8000 / 2 = 4000 Hz
        mock.push_feature_response(make_reply(0x00, 0xC0, TX_1F, &[0x00, 0x02]));
        // Lift-off: tracking = 1 (Medium)
        mock.push_feature_response(make_reply(0x0B, 0x85, TX_1F, &[0, 0, 1, 15, 10]));

        let mut dev = RazerDevice::open(mock, false, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.12"));
        assert_eq!(dev.info().serial_number.as_deref(), Some("RZ-TEST-01"));

        let batt = dev.read_battery().unwrap();
        assert_eq!(batt.percentage, Some(80));
        assert_eq!(batt.state, ChargingState::Charging);

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.active_dpi_stage, 1);
        assert_eq!(snap.dpi_stages.len(), 2);
        assert_eq!(snap.dpi_stages[0].x.value(), 800);
        assert_eq!(snap.dpi_stages[1].x.value(), 1600);
        assert_eq!(snap.dpi_stages[1].effective_y().value(), 1800);
        assert_eq!(snap.polling_rate, PollingRateHz::Hz4000);
        assert_eq!(snap.lod, Some(LiftOffDistance::Medium));
    }

    #[test]
    fn dry_run_validates_and_suppresses_razer_writes() {
        let mut mock = MockHidTransport::new(viper_v3_pro_dongle_desc());
        mock.push_feature_response(make_reply(0x00, 0x81, TX_1F, &[1, 12]));
        mock.push_feature_response(make_reply(0x00, 0x82, TX_1F, b"RZ-DRY\0"));

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = RazerDevice::open(mock, true, Some(sink.clone())).unwrap();

        let stages = vec![DpiStage::new(0, DpiValue::new(1600).unwrap())];
        dev.set_dpi_stages(&stages, 0).unwrap();
        dev.set_polling_rate(PollingRateHz::Hz8000).unwrap();
        dev.set_lod(LiftOffDistance::Low).unwrap();

        // Only the two read-only queries during open() reached hardware.
        assert_eq!(dev.transport().inner().recorded_writes().len(), 2);

        // 1 DPI stage write + 2 extended polling writes (selector 0x00 + commit 0x01) + 1 LOD write = 4 suppressed.
        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 4);
    }

    #[test]
    fn basilisk_v3_extended_matrix_rgb_reads_and_writes() {
        let desc = HidDeviceDescriptor::new(
            RAZER_VID,
            0x0099,
            RAZER_USAGE_PAGE_MOUSE,
            RAZER_USAGE_ID_MOUSE,
            0,
            "mock://razer/basilisk-v3",
        );
        let mut mock = MockHidTransport::new(desc);
        mock.push_feature_response(make_reply(0x00, 0x81, TX_1F, &[2, 0]));
        mock.push_feature_response(make_reply(0x00, 0x82, TX_1F, b"BASILISK\0"));
        // Brightness reads for All, Scroll Wheel, Logo
        mock.push_feature_response(make_reply(0x0F, 0x84, TX_1F, &[0x01, 0x00, 0xFF]));
        mock.push_feature_response(make_reply(0x0F, 0x84, TX_1F, &[0x01, 0x01, 0x80]));
        mock.push_feature_response(make_reply(0x0F, 0x84, TX_1F, &[0x01, 0x04, 0x40]));

        let mut dev = RazerDevice::open(mock, true, None).unwrap();
        assert!(dev.capabilities().has_rgb());
        let snap = dev.read_rgb_state().unwrap();
        assert_eq!(snap.zones.len(), 3);
        assert_eq!(snap.zones[0].brightness.percent(), 100);
        assert_eq!(snap.zones[1].brightness.percent(), 50);

        dev.set_zone_effect(
            RgbZoneId::new(led::LOGO),
            RgbEffect::Static {
                color: RgbColor::new(0xFF, 0x00, 0x80),
            },
            Brightness::new(75).unwrap(),
        )
        .unwrap();
    }
}
