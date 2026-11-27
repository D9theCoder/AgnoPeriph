//! Wooting Hall Effect analog keyboard driver (`VID 0x31E3`).

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    Brightness, CalibrationStatus, Capabilities, ConnectionKind, CoreError, CoreResult, DeviceId,
    DeviceInfo, DeviceMatchRule, DriverDescriptor, HallEffectCapabilities, HallEffectKeyboard,
    HallEffectSnapshot, KeyHallEffectConfig, KeyIndex, Peripheral, ProfileCapabilities,
    ProfileInfo, ProfileManager, ProfileMode, ProfileSlot, ProfileSnapshot, RapidTriggerConfig,
    RgbCapabilities, RgbColor, RgbController, RgbEffect, RgbEffectKind, RgbSnapshot, RgbZoneId,
    RgbZoneInfo, RgbZoneState, TravelHundredthsMm, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, ReadMatch};

use crate::protocol::{
    WOOTING_CONFIG_USAGE_ID, WOOTING_CONFIG_USAGE_PAGE, WOOTING_CONFIG_USAGE_PAGE_LEGACY,
    WOOTING_PRODUCTS, WOOTING_VID, WootingProductProfile, cmd, decode_active_profile,
    decode_layout, decode_profile_actuation, decode_version, encode_command, encode_single_color,
    lookup_product, normalize_reply, wooting_validator,
};

const READ_TIMEOUT: Duration = Duration::from_millis(400);
const MAX_READ_ATTEMPTS: usize = 32;
const PROFILE_SLOT_COUNT: u8 = 4;

/// Declarative `DriverDescriptor` claiming Wooting HE keyboards on `0xFF55:0x0001` & `0x1337`.
pub fn driver_descriptor() -> DriverDescriptor {
    let mut rules = Vec::with_capacity(WOOTING_PRODUCTS.len() * 6);
    for product in WOOTING_PRODUCTS {
        for alt in 0u16..=2 {
            let pid = product.base_pid | alt;
            rules.push(
                DeviceMatchRule::new(WOOTING_VID, ConnectionKind::WiredUsb, product.model_name)
                    .with_pid(pid)
                    .with_usage_page(WOOTING_CONFIG_USAGE_PAGE)
                    .with_usage_id(WOOTING_CONFIG_USAGE_ID),
            );
            rules.push(
                DeviceMatchRule::new(WOOTING_VID, ConnectionKind::WiredUsb, product.model_name)
                    .with_pid(pid)
                    .with_usage_page(WOOTING_CONFIG_USAGE_PAGE_LEGACY),
            );
        }
    }
    DriverDescriptor::new("wooting-he", VendorKind::Wooting, rules)
}

/// Active Wooting Hall Effect keyboard backed by a validated [`GuardedTransport`].
pub struct WootingDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    profile: WootingProductProfile,
    multi_report: bool,
    info: DeviceInfo,
    capabilities: Capabilities,
    he_caps: HallEffectCapabilities,
    profile_caps: ProfileCapabilities,
    rgb_caps: Option<RgbCapabilities>,
    cached_rgb: Vec<RgbZoneState>,
    layout: Option<&'static str>,
}

impl<T: HidTransport> fmt::Debug for WootingDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WootingDevice")
            .field("profile", &self.profile)
            .field("info", &self.info)
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> WootingDevice<T> {
    /// Connects to a Wooting keyboard configuration interface and reads firmware & layout.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != WOOTING_VID {
            return Err(CoreError::Validation(format!(
                "expected Wooting VID 0x{WOOTING_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        let profile = lookup_product(desc.pid).ok_or_else(|| {
            CoreError::Validation(format!("unsupported Wooting PID 0x{:04X}", desc.pid))
        })?;
        let multi_report = desc.usage_page == WOOTING_CONFIG_USAGE_PAGE;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(wooting_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "wooting:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let he_caps = HallEffectCapabilities::new(
            profile.key_count(),
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(400),
            TravelHundredthsMm::from_hundredths(5),
        )
        .with_rapid_trigger_range(
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(235),
            true,
        );

        let (rgb_caps, cached_rgb) = if profile.has_rgb {
            let brightness =
                Brightness::new(100).unwrap_or_else(|_| unreachable!("100 is valid brightness"));
            (
                Some(RgbCapabilities::new(
                    vec![RgbZoneInfo::new(RgbZoneId::new(0), "Per-Key Backlight")],
                    vec![RgbEffectKind::Off, RgbEffectKind::Static],
                    true,
                )),
                vec![RgbZoneState::new(
                    RgbZoneId::new(0),
                    RgbEffect::Static {
                        color: RgbColor::WHITE,
                    },
                    brightness,
                )],
            )
        } else {
            (None, Vec::new())
        };

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Wooting,
            profile.model_name,
            ConnectionKind::WiredUsb,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let mut dev = Self {
            transport: guarded,
            profile,
            multi_report,
            info,
            capabilities: Capabilities::empty()
                .with_hall_effect(true)
                .with_profiles(true)
                .with_rgb(profile.has_rgb),
            he_caps,
            profile_caps: ProfileCapabilities::new(PROFILE_SLOT_COUNT, false, false),
            rgb_caps,
            cached_rgb,
            layout: None,
        };

        if let Ok(reply) = dev.query_command(cmd::GET_VERSION, 0, 0, 0, 0)
            && let Some(fw) = decode_version(&reply)
        {
            dev.info.firmware_version = Some(fw);
        }
        if let Ok(reply) = dev.query_command(cmd::GET_DEVICE_CONFIG, 0, 0, 0, 0) {
            dev.layout = decode_layout(&reply);
        }

        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    pub const fn layout(&self) -> Option<&'static str> {
        self.layout
    }

    fn query_command(
        &mut self,
        command_id: u8,
        p0: u8,
        p1: u8,
        p2: u8,
        p3: u8,
    ) -> CoreResult<Vec<u8>> {
        let frame = encode_command(command_id, p0, p1, p2, p3, self.multi_report);
        let _ = self.transport.drain_input_reports();
        self.transport.send_query_feature_report(&frame)?;
        let mut buf = [0u8; 256];
        match self.transport.read_matching_input(
            &mut buf,
            READ_TIMEOUT,
            MAX_READ_ATTEMPTS,
            |raw| {
                normalize_reply(raw)
                    .is_some_and(|reply| reply.get(2) == Some(&command_id))
                    .then(|| raw.to_vec())
            },
        )? {
            ReadMatch::Matched(reply) => Ok(reply),
            ReadMatch::TimedOut => Err(CoreError::NotConnected(format!(
                "timed out waiting for Wooting reply to 0x{command_id:02X}"
            ))),
            ReadMatch::WindowExhausted => Err(CoreError::Protocol(format!(
                "no matching Wooting reply for 0x{command_id:02X}"
            ))),
        }
    }

    fn send_rgb_feature(&mut self, frame: [u8; 8]) -> CoreResult<()> {
        self.transport.send_feature_report(&frame)?;
        Ok(())
    }
}

impl<T: HidTransport> Peripheral for WootingDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let reply = self.query_command(cmd::GET_VERSION, 0, 0, 0, 0)?;
        if let Some(fw) = decode_version(&reply) {
            self.info.firmware_version = Some(fw);
        }
        Ok(())
    }

    fn as_hall_effect(&self) -> Option<&dyn HallEffectKeyboard> {
        Some(self)
    }

    fn as_hall_effect_mut(&mut self) -> Option<&mut dyn HallEffectKeyboard> {
        Some(self)
    }

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        Some(self)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        Some(self)
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        self.capabilities.has_rgb().then_some(self)
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        self.capabilities.has_rgb().then_some(self)
    }
}

impl<T: HidTransport> HallEffectKeyboard for WootingDevice<T> {
    fn hall_effect_capabilities(&self) -> HallEffectCapabilities {
        self.he_caps.clone()
    }

    fn read_hall_effect_state(&mut self) -> CoreResult<HallEffectSnapshot> {
        // Query active profile (`0x27`, selector `0xFFFF` in p3/p2) for global actuation.
        let actuation = self
            .query_command(cmd::GET_KEYBOARD_PROFILE, 0, 0, 0xFF, 0xFF)
            .ok()
            .and_then(|raw| decode_profile_actuation(&raw))
            .unwrap_or_else(|| TravelHundredthsMm::from_hundredths(200));

        let keys = (0..self.profile.key_count())
            .map(|ix| {
                KeyHallEffectConfig::new(
                    KeyIndex::new(ix),
                    actuation,
                    RapidTriggerConfig::disabled(),
                )
            })
            .collect();

        Ok(HallEffectSnapshot::new(
            keys,
            Vec::new(),
            Vec::new(),
            CalibrationStatus::Calibrated,
        ))
    }

    fn read_calibration_status(&mut self) -> CoreResult<CalibrationStatus> {
        Ok(CalibrationStatus::Calibrated)
    }

    fn set_key_actuation(&mut self, _key: KeyIndex, _point: TravelHundredthsMm) -> CoreResult<()> {
        Err(CoreError::UnsupportedCapability(
            "wooting_actuation_write_readonly",
        ))
    }

    fn set_global_actuation(&mut self, _point: TravelHundredthsMm) -> CoreResult<()> {
        Err(CoreError::UnsupportedCapability(
            "wooting_actuation_write_readonly",
        ))
    }

    fn set_rapid_trigger(&mut self, _key: KeyIndex, _config: RapidTriggerConfig) -> CoreResult<()> {
        Err(CoreError::UnsupportedCapability(
            "wooting_rapid_trigger_write_readonly",
        ))
    }
}

impl<T: HidTransport> ProfileManager for WootingDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        self.profile_caps.clone()
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let raw = self.query_command(cmd::GET_CURRENT_PROFILE_INDEX, 0, 0, 0, 0)?;
        let active_ix = decode_active_profile(&raw)
            .unwrap_or(0)
            .min(PROFILE_SLOT_COUNT.saturating_sub(1));
        let profiles = (0..PROFILE_SLOT_COUNT)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect();
        Ok(ProfileSnapshot::new(
            ProfileSlot::new(active_ix),
            ProfileMode::OnboardMemory,
            profiles,
        ))
    }

    fn set_active_profile(&mut self, _slot: ProfileSlot) -> CoreResult<()> {
        Err(CoreError::UnsupportedCapability(
            "wooting_profile_write_readonly",
        ))
    }
}

impl<T: HidTransport> RgbController for WootingDevice<T> {
    fn rgb_capabilities(&self) -> RgbCapabilities {
        self.rgb_caps
            .clone()
            .unwrap_or_else(|| RgbCapabilities::new(Vec::new(), Vec::new(), false))
    }

    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot> {
        if !self.profile.has_rgb {
            return Err(CoreError::UnsupportedCapability("rgb"));
        }
        Ok(RgbSnapshot::new(self.cached_rgb.clone()))
    }

    fn set_zone_effect(
        &mut self,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()> {
        if !self.profile.has_rgb {
            return Err(CoreError::UnsupportedCapability("rgb"));
        }
        match effect {
            RgbEffect::Off => {
                self.send_rgb_feature(encode_command(
                    cmd::RESET_ALL,
                    0,
                    0,
                    0,
                    0,
                    self.multi_report,
                ))?;
            }
            RgbEffect::Static { color } => {
                let scale = |c: u8| {
                    u8::try_from((u16::from(c) * u16::from(brightness.percent()) + 50) / 100)
                        .unwrap_or(0)
                };
                let scaled = RgbColor::new(scale(color.r), scale(color.g), scale(color.b));
                self.send_rgb_feature(encode_command(
                    cmd::COLOR_INIT,
                    0,
                    0,
                    0,
                    0,
                    self.multi_report,
                ))?;
                for row in 0..self.profile.rows {
                    for col in 0..self.profile.cols {
                        self.send_rgb_feature(encode_single_color(
                            row,
                            col,
                            scaled,
                            self.multi_report,
                        ))?;
                    }
                }
            }
            _ => return Err(CoreError::UnsupportedCapability("wooting_rgb_effect")),
        }

        if let Some(entry) = self.cached_rgb.iter_mut().find(|z| z.zone == zone) {
            entry.effect = effect;
            entry.brightness = brightness;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{WOOTING_MAGIC_MULTI, WOOTING_MAGIC_WORD_1, WOOTING_STATUS_OK};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome, PacketValidator,
        ReportKind,
    };

    fn wooting_60he_plus_desc() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            WOOTING_VID,
            0x1322,
            WOOTING_CONFIG_USAGE_PAGE,
            WOOTING_CONFIG_USAGE_ID,
            1,
            "mock://wooting/60he-plus",
        )
    }

    #[test]
    fn validator_blocks_bootloader_and_soft_reset_commands() {
        let validator = wooting_validator();
        let safe = encode_command(cmd::GET_VERSION, 0, 0, 0, 0, true);
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &safe)
                .is_ok()
        );

        for forbidden in [
            cmd::FORBIDDEN_RESET_TO_BOOTLOADER,
            cmd::FORBIDDEN_KEYS_OFF,
            cmd::FORBIDDEN_SOFT_RESET,
        ] {
            let bad = encode_command(forbidden, 0, 0, 0, 0, true);
            assert!(
                validator
                    .validate_outgoing(ReportKind::Feature, &bad)
                    .is_err()
            );
        }
    }

    #[test]
    fn reads_60he_plus_version_layout_profile_and_actuation() {
        let mut mock = MockHidTransport::new(wooting_60he_plus_desc());
        // Confirmed hardware fixture: d1 da 01 88 03 00 02 0d 00 => v2.13.0
        mock.push_input_response(vec![
            WOOTING_MAGIC_MULTI,
            WOOTING_MAGIC_WORD_1,
            cmd::GET_VERSION,
            WOOTING_STATUS_OK,
            0x03,
            0x00,
            0x02,
            0x0D,
            0x00,
        ]);
        // Device config reply with ANSI layout at offset 9
        mock.push_input_response(vec![
            WOOTING_MAGIC_MULTI,
            WOOTING_MAGIC_WORD_1,
            cmd::GET_DEVICE_CONFIG,
            WOOTING_STATUS_OK,
            0x07,
            0x00,
            0,
            0,
            0,
            0x00,
        ]);
        // Active profile = 2
        mock.push_input_response(vec![
            WOOTING_MAGIC_MULTI,
            WOOTING_MAGIC_WORD_1,
            cmd::GET_CURRENT_PROFILE_INDEX,
            WOOTING_STATUS_OK,
            0x02,
        ]);
        // Keyboard profile protobuf: field 1 (0x0A len=4) -> field 1 varint 18432 (0.50 mm = 0x80 0x90 0x01)
        let nested = [0x08u8, 0x80, 0x90, 0x01];
        let mut prof_reply = vec![
            WOOTING_MAGIC_MULTI,
            WOOTING_MAGIC_WORD_1,
            cmd::GET_KEYBOARD_PROFILE,
            WOOTING_STATUS_OK,
            6,
            0,
            0x0A,
            4,
        ];
        prof_reply.extend_from_slice(&nested);
        mock.push_input_response(prof_reply);

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = WootingDevice::open(mock, true, Some(sink.clone())).unwrap();
        assert_eq!(dev.info().model_name, "Wooting 60HE+");
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v2.13.0"));
        assert_eq!(dev.layout(), Some("ANSI"));

        let prof = dev.read_profile_state().unwrap();
        assert_eq!(prof.active_slot, ProfileSlot::new(2));

        let he = dev.read_hall_effect_state().unwrap();
        assert_eq!(he.keys.len(), 84);
        assert_eq!(he.keys[0].actuation.as_hundredths(), 50);

        // Actuation writes are safely read-only until verified write packets exist.
        assert!(
            dev.set_global_actuation(TravelHundredthsMm::from_hundredths(100))
                .is_err()
        );

        // Official RGB SDK ResetAll under dry-run is validated and suppressed.
        dev.set_zone_effect(
            RgbZoneId::new(0),
            RgbEffect::Off,
            Brightness::new(100).unwrap(),
        )
        .unwrap();
        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 1);
    }

    #[test]
    fn drains_stale_reports_and_skips_unmatched_frames_before_matching_reply() {
        let mut mock = MockHidTransport::new(wooting_60he_plus_desc());
        for _ in 0..10 {
            mock.push_stale_input_report(vec![0x00; 16]);
        }
        // Unrelated frame mid-flight followed by the real GetVersion reply.
        mock.push_input_response(vec![
            WOOTING_MAGIC_MULTI,
            WOOTING_MAGIC_WORD_1,
            cmd::GET_SERIAL,
            WOOTING_STATUS_OK,
            0,
            0,
        ]);
        mock.push_input_response(vec![
            WOOTING_MAGIC_MULTI,
            WOOTING_MAGIC_WORD_1,
            cmd::GET_VERSION,
            WOOTING_STATUS_OK,
            0x03,
            0x00,
            0x02,
            0x0D,
            0x00,
        ]);
        mock.push_input_response(vec![
            WOOTING_MAGIC_MULTI,
            WOOTING_MAGIC_WORD_1,
            cmd::GET_DEVICE_CONFIG,
            WOOTING_STATUS_OK,
            0x07,
            0x00,
            0,
            0,
            0,
            0x01,
        ]);

        let dev = WootingDevice::open(mock, true, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v2.13.0"));
        assert_eq!(dev.layout(), Some("ISO"));
    }
}
