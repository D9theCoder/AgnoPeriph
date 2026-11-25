//! ATK RS7 Hall Effect keyboard driver implementing `Peripheral`, `HallEffectKeyboard`,
//! and `RgbController` over the verified DuckBread Raw HID protocol (`0x373B:0x1044`).

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    Brightness, CalibrationStatus, Capabilities, ConnectionKind, CoreError, CoreResult, DeviceId,
    DeviceInfo, DeviceMatchRule, DksBinding, DriverDescriptor, HallEffectCapabilities,
    HallEffectKeyboard, HallEffectSnapshot, KeyHallEffectConfig, KeyIndex, Peripheral,
    PollingRateHz, RapidTriggerConfig, RgbCapabilities, RgbController, RgbEffect, RgbEffectKind,
    RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState, SocdPair, TravelHundredthsMm, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::protocol::{
    ATK_RS7_PID, ATK_RS7_USAGE_ID, ATK_RS7_USAGE_PAGE, ATK_VID, RS7_KEY_COUNT, RS7_MAX_DKS_SLOTS,
    RS7_MAX_SOCD_SLOTS, RS7_PAYLOAD_LEN, RS7_RT_MAX, RS7_RT_MIN, RS7_STEP_EXTREME, RS7_STEP_NORMAL,
    RS7_TRAVEL_MAX, RS7_TRAVEL_MIN, RS7_TX_REPORT_LEN, atk_rs7_validator, decode_backlight,
    decode_deadzone, decode_dks_slot, decode_firmware_version, decode_report_rate,
    decode_socd_slot, flash_op, key_to_row_col, req_backlight_get, req_backlight_set,
    req_calibration_init_status_get, req_deadzone_get, req_dks_get, req_dks_set,
    req_firmware_version, req_game_mode, req_report_rate, req_rt_buffer_get,
    req_single_actuation_set, req_single_rt_set, req_socd_get, req_socd_set, req_travel_buffer_get,
    req_travel_buffer_set, unwrap_rx, wrap_tx,
};

const READ_TIMEOUT: Duration = Duration::from_millis(800);
const MAX_READ_ATTEMPTS: usize = 8;

/// Returns the declarative `DriverDescriptor` registering ATK RS7 (`0x373B:0x1044`, `0xFF60:0x0061`).
pub fn driver_descriptor() -> DriverDescriptor {
    DriverDescriptor::new(
        "atk-rs7",
        VendorKind::Atk,
        vec![
            DeviceMatchRule::new(ATK_VID, ConnectionKind::WiredUsb, "ATK RS7 Hall Effect")
                .with_pid(ATK_RS7_PID)
                .with_usage_page(ATK_RS7_USAGE_PAGE)
                .with_usage_id(ATK_RS7_USAGE_ID),
        ],
    )
}

/// Active ATK RS7 Hall Effect keyboard backed by a safety-guarded [`GuardedTransport`].
pub struct AtkRs7Device<T: HidTransport> {
    transport: GuardedTransport<T>,
    info: DeviceInfo,
    capabilities: Capabilities,
    he_caps: HallEffectCapabilities,
    extreme_mode: bool,
    polling_rate: Option<PollingRateHz>,
    /// Hardware SOCD slot indices (`0..RS7_MAX_SOCD_SLOTS`) observed on the last read/write.
    socd_slots: Option<Vec<u8>>,
    /// Hardware slots occupied by non-SOCD modes (e.g. Rappy Snappy / OKS) that must never be overwritten.
    reserved_rs_slots: Vec<u8>,
}

impl<T: HidTransport> fmt::Debug for AtkRs7Device<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtkRs7Device")
            .field("info", &self.info)
            .field("extreme_mode", &self.extreme_mode)
            .field("polling_rate", &self.polling_rate)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> AtkRs7Device<T> {
    /// Connects to an ATK RS7 Raw HID (`0xFF60:0x0061`) interface, installs the bootloader/reset
    /// blocking validator, and performs read-only metadata discovery.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != ATK_VID || desc.pid != ATK_RS7_PID {
            return Err(CoreError::Validation(format!(
                "expected ATK RS7 0x{ATK_VID:04X}:0x{ATK_RS7_PID:04X}, got 0x{:04X}:0x{:04X}",
                desc.vid, desc.pid
            )));
        }

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(atk_rs7_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "atk:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Atk,
            desc.product.unwrap_or_else(|| "ATK RS7 RGB".to_owned()),
            ConnectionKind::WiredUsb,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let mut dev = Self {
            transport: guarded,
            info,
            capabilities: Capabilities::empty().with_hall_effect(true).with_rgb(true),
            he_caps: build_he_caps(false),
            extreme_mode: false,
            polling_rate: None,
            socd_slots: None,
            reserved_rs_slots: Vec::new(),
        };

        dev.discover_readonly()?;
        Ok(dev)
    }

    /// Returns the underlying [`GuardedTransport`] (for mock & dry-run verification).
    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    /// Returns whether Extreme (`0.01 mm` step) performance mode is enabled on the RS7.
    pub const fn is_extreme_mode(&self) -> bool {
        self.extreme_mode
    }

    /// Returns the keyboard's reported USB polling rate (1K / 2K / 4K / 8K Hz).
    pub const fn polling_rate(&self) -> Option<PollingRateHz> {
        self.polling_rate
    }

    fn exchange(
        &mut self,
        payload: [u8; RS7_PAYLOAD_LEN],
        mutating: bool,
    ) -> CoreResult<[u8; RS7_PAYLOAD_LEN]> {
        let tx = wrap_tx(payload);
        let expected_cmd = payload[0];

        if mutating {
            self.transport.send_output_report(&tx)?;
            if self.transport.is_dry_run() {
                return Ok(payload);
            }
        } else {
            self.transport.send_query_output_report(&tx)?;
        }

        let mut rx = [0u8; RS7_TX_REPORT_LEN];
        for _ in 0..MAX_READ_ATTEMPTS {
            let n = match self.transport.read_input_report(&mut rx, READ_TIMEOUT) {
                Ok(len) => len,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for ATK RS7 reply to cmd 0x{expected_cmd:02X}"
                    )));
                }
                Err(err) => return Err(err.into()),
            };
            if let Some(decoded) = unwrap_rx(&rx[..n], expected_cmd) {
                return Ok(decoded);
            }
        }

        Err(CoreError::Protocol(format!(
            "no matching ATK RS7 reply for cmd 0x{expected_cmd:02X}"
        )))
    }

    fn discover_readonly(&mut self) -> CoreResult<()> {
        let fw = self.exchange(req_firmware_version(), false)?;
        self.info.firmware_version = Some(format!("v{}", decode_firmware_version(&fw)));

        if let Ok(gm) = self.exchange(req_game_mode(), false) {
            self.extreme_mode = gm[3] == 1;
            self.he_caps = build_he_caps(self.extreme_mode);
        }

        if let Ok(rr) = self.exchange(req_report_rate(), false) {
            self.polling_rate = decode_report_rate(rr[3]);
        }

        Ok(())
    }

    fn scan_socd_slots(&mut self) -> CoreResult<(Vec<SocdPair>, Vec<u8>, Vec<u8>)> {
        let mut pairs = Vec::new();
        let mut socd_slots = Vec::new();
        let mut reserved_rs_slots = Vec::new();
        for slot in 0..RS7_MAX_SOCD_SLOTS {
            let resp = self.exchange(req_socd_get(slot), false)?;
            if let Some(pair) = decode_socd_slot(&resp) {
                pairs.push(pair);
                socd_slots.push(slot);
            } else if resp[7] != protocol::socd_wire::NONE {
                reserved_rs_slots.push(slot);
            }
        }
        Ok((pairs, socd_slots, reserved_rs_slots))
    }
}

const fn build_he_caps(extreme_mode: bool) -> HallEffectCapabilities {
    let step = if extreme_mode {
        RS7_STEP_EXTREME
    } else {
        RS7_STEP_NORMAL
    };
    HallEffectCapabilities::new(RS7_KEY_COUNT, RS7_TRAVEL_MIN, RS7_TRAVEL_MAX, step)
        .with_rapid_trigger_range(RS7_RT_MIN, RS7_RT_MAX, true)
        .with_max_dks_bindings(RS7_MAX_DKS_SLOTS)
        .with_max_socd_pairs(RS7_MAX_SOCD_SLOTS)
        .with_calibration_status(true)
}

impl<T: HidTransport> Peripheral for AtkRs7Device<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        self.discover_readonly()
    }

    fn as_hall_effect(&self) -> Option<&dyn HallEffectKeyboard> {
        Some(self)
    }

    fn as_hall_effect_mut(&mut self) -> Option<&mut dyn HallEffectKeyboard> {
        Some(self)
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        Some(self)
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        Some(self)
    }
}

impl<T: HidTransport> HallEffectKeyboard for AtkRs7Device<T> {
    fn hall_effect_capabilities(&self) -> HallEffectCapabilities {
        self.he_caps.clone()
    }

    fn read_hall_effect_state(&mut self) -> CoreResult<HallEffectSnapshot> {
        // 1. Global deadzone
        let dz_raw = self.exchange(req_deadzone_get(), false)?;
        let (top_dz, bottom_dz) = decode_deadzone(&dz_raw);

        // 2. Bulk actuation buffer (12 keys per chunk × 8 chunks = 96 keys)
        let mut actuations = Vec::with_capacity(usize::from(RS7_KEY_COUNT));
        for chunk_ix in 0..(RS7_KEY_COUNT / 12) {
            let offset = chunk_ix * 12;
            let resp = self.exchange(req_travel_buffer_get(offset, 12), false)?;
            for k in 0..12usize {
                let raw = u16::from_be_bytes([resp[8 + k * 2], resp[9 + k * 2]]);
                actuations.push(TravelHundredthsMm::from_hundredths(raw));
            }
        }

        // 3. Bulk Rapid Trigger buffer (4 keys per chunk × 24 chunks = 96 keys)
        let mut rts = Vec::with_capacity(usize::from(RS7_KEY_COUNT));
        for chunk_ix in 0..(RS7_KEY_COUNT / 4) {
            let offset = chunk_ix * 4;
            let resp = self.exchange(req_rt_buffer_get(offset, 4), false)?;
            for k in 0..4usize {
                let base = 8 + k * 5;
                let enabled = resp[base] != 0;
                let rel = u16::from_be_bytes([resp[base + 1], resp[base + 2]]);
                let press = u16::from_be_bytes([resp[base + 3], resp[base + 4]]);
                rts.push(RapidTriggerConfig::new(
                    enabled,
                    enabled,
                    TravelHundredthsMm::from_hundredths(press),
                    TravelHundredthsMm::from_hundredths(rel),
                ));
            }
        }

        let keys: Vec<KeyHallEffectConfig> = (0..usize::from(RS7_KEY_COUNT))
            .map(|ix| {
                let key = KeyIndex::new(u16::try_from(ix).unwrap_or(0));
                KeyHallEffectConfig::new(key, actuations[ix], rts[ix])
                    .with_deadzones(Some(top_dz), Some(bottom_dz))
            })
            .collect();

        // 4. Read active SOCD pairs across hardware slots (0..20), remembering occupied and
        // non-SOCD (RS/OKS) slots so writes update in place and clear removed pairs safely.
        let (socd_pairs, socd_slots, reserved_rs_slots) = self.scan_socd_slots()?;
        self.socd_slots = Some(socd_slots);
        self.reserved_rs_slots = reserved_rs_slots;

        // 5. Read active DKS bindings across hardware slots (0..20)
        let mut dks_bindings = Vec::new();
        for slot in 0..RS7_MAX_DKS_SLOTS {
            if let Ok(resp) = self.exchange(req_dks_get(slot), false)
                && let Some(binding) = decode_dks_slot(slot, &resp)
            {
                dks_bindings.push(binding);
            }
        }

        // 6. Calibration init status
        let calibration = self.read_calibration_status()?;

        Ok(HallEffectSnapshot::new(
            keys,
            dks_bindings,
            socd_pairs,
            calibration,
        ))
    }

    fn read_calibration_status(&mut self) -> CoreResult<CalibrationStatus> {
        let mut calibrated_count = 0usize;
        let mut total_checked = 0usize;
        for chunk_ix in 0..(RS7_KEY_COUNT / 24) {
            let offset = chunk_ix * 24;
            let resp = self.exchange(req_calibration_init_status_get(offset, 24), false)?;
            for k in 0..24usize {
                total_checked += 1;
                if resp[8 + k] != 0 {
                    calibrated_count += 1;
                }
            }
        }

        if total_checked == 0 {
            Ok(CalibrationStatus::Unknown)
        } else if calibrated_count > 0 {
            Ok(CalibrationStatus::Calibrated)
        } else {
            Ok(CalibrationStatus::NeedsCalibration)
        }
    }

    fn set_key_actuation(&mut self, key: KeyIndex, point: TravelHundredthsMm) -> CoreResult<()> {
        self.he_caps.validate_key(key)?;
        self.he_caps.validate_actuation(point)?;
        let (row, col) = key_to_row_col(key)?;
        self.exchange(req_single_actuation_set(row, col, point), true)?;
        Ok(())
    }

    fn set_global_actuation(&mut self, point: TravelHundredthsMm) -> CoreResult<()> {
        self.he_caps.validate_actuation(point)?;
        let all = vec![point; usize::from(RS7_KEY_COUNT)];
        let chunks: Vec<&[TravelHundredthsMm]> = all.chunks(11).collect();
        let last_ix = chunks.len().saturating_sub(1);
        for (ix, chunk) in chunks.iter().enumerate() {
            let op = if ix == 0 {
                flash_op::ERASE
            } else if ix == last_ix {
                flash_op::WRITE
            } else {
                flash_op::NORMAL
            };
            let offset = u16::try_from(ix * 11).unwrap_or(0);
            let count = u8::try_from(chunk.len()).unwrap_or(11);
            self.exchange(req_travel_buffer_set(offset, count, op, chunk), true)?;
            if !self.transport.is_dry_run() && (ix == 0 || ix == last_ix) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        Ok(())
    }

    fn set_rapid_trigger(&mut self, key: KeyIndex, config: RapidTriggerConfig) -> CoreResult<()> {
        self.he_caps.validate_key(key)?;
        self.he_caps.validate_rapid_trigger(&config)?;
        let (row, col) = key_to_row_col(key)?;
        self.exchange(req_single_rt_set(row, col, &config), true)?;
        Ok(())
    }

    fn set_dks_binding(&mut self, binding: DksBinding) -> CoreResult<()> {
        let slot = u8::try_from(binding.key.value()).map_err(|_| CoreError::OutOfRange {
            field: "dks_slot",
            actual: binding.key.value().to_string(),
            expected: format!("0..{RS7_MAX_DKS_SLOTS}"),
        })?;
        if slot >= RS7_MAX_DKS_SLOTS {
            return Err(CoreError::OutOfRange {
                field: "dks_slot",
                actual: slot.to_string(),
                expected: format!("0..{RS7_MAX_DKS_SLOTS}"),
            });
        }
        self.exchange(req_dks_set(slot, &binding), true)?;
        Ok(())
    }

    fn clear_dks_binding(&mut self, key: KeyIndex) -> CoreResult<()> {
        let empty = DksBinding::new(key, [None, None, None, None]);
        self.set_dks_binding(empty)
    }

    fn set_socd_pairs(&mut self, pairs: &[SocdPair]) -> CoreResult<()> {
        self.he_caps.validate_socd_pairs(pairs)?;

        // Before mutating real hardware, ensure we know which slots hold SOCD pairs and which
        // hold non-SOCD modes (RS/OKS) so we never duplicate pairs or overwrite RS/OKS slots.
        if self.socd_slots.is_none() && !self.transport.is_dry_run() {
            let (_, socd_slots, reserved_rs_slots) = self.scan_socd_slots()?;
            self.socd_slots = Some(socd_slots);
            self.reserved_rs_slots = reserved_rs_slots;
        }

        let previous_slots = self.socd_slots.clone().unwrap_or_default();
        let mut target_slots: Vec<u8> = Vec::with_capacity(pairs.len());

        // Reuse previously occupied SOCD slots in order first, then claim lowest free non-reserved slots.
        for &slot in &previous_slots {
            if target_slots.len() < pairs.len() {
                target_slots.push(slot);
            }
        }
        for slot in 0..RS7_MAX_SOCD_SLOTS {
            if target_slots.len() >= pairs.len() {
                break;
            }
            if !target_slots.contains(&slot) && !self.reserved_rs_slots.contains(&slot) {
                target_slots.push(slot);
            }
        }
        if target_slots.len() < pairs.len() {
            return Err(CoreError::OutOfRange {
                field: "socd_pairs",
                actual: pairs.len().to_string(),
                expected: format!(
                    "at most {} free SOCD slots ({} occupied by non-SOCD modes)",
                    usize::from(RS7_MAX_SOCD_SLOTS).saturating_sub(self.reserved_rs_slots.len()),
                    self.reserved_rs_slots.len()
                ),
            });
        }

        // Write updated/new pairs, then clear any previously occupied SOCD slots no longer used.
        let write_result = (|| -> CoreResult<()> {
            for (&slot, pair) in target_slots.iter().zip(pairs.iter()) {
                let payload = req_socd_set(slot, Some(pair))?;
                self.exchange(payload, true)?;
            }
            for &old_slot in &previous_slots {
                if !target_slots.contains(&old_slot) {
                    let payload = req_socd_set(old_slot, None)?;
                    self.exchange(payload, true)?;
                }
            }
            Ok(())
        })();

        match write_result {
            Ok(()) => {
                if !self.transport.is_dry_run() {
                    self.socd_slots = Some(target_slots);
                }
                Ok(())
            }
            Err(err) => {
                // Force a fresh slot scan on the next attempt since partial writes may have landed.
                self.socd_slots = None;
                Err(err)
            }
        }
    }
}

impl<T: HidTransport> RgbController for AtkRs7Device<T> {
    fn rgb_capabilities(&self) -> RgbCapabilities {
        RgbCapabilities::new(
            vec![RgbZoneInfo::new(RgbZoneId::new(0), "Key Backlight")],
            vec![
                RgbEffectKind::Off,
                RgbEffectKind::Static,
                RgbEffectKind::Breathing,
                RgbEffectKind::SpectrumCycle,
                RgbEffectKind::Wave,
                RgbEffectKind::Reactive,
            ],
            true,
        )
    }

    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot> {
        let resp = self.exchange(req_backlight_get(), false)?;
        let (effect, brightness_pct) = decode_backlight(&resp);
        let brightness = Brightness::new(brightness_pct)?;
        Ok(RgbSnapshot::new(vec![RgbZoneState::new(
            RgbZoneId::new(0),
            effect,
            brightness,
        )]))
    }

    fn set_zone_effect(
        &mut self,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()> {
        if zone.value() != 0 {
            return Err(CoreError::OutOfRange {
                field: "rgb_zone",
                actual: zone.value().to_string(),
                expected: "0".to_owned(),
            });
        }
        self.exchange(req_backlight_set(effect, brightness.percent()), true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::{RgbColor, SocdMode};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome, PacketValidator,
        ReportKind,
    };

    fn rs7_descriptor() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            ATK_VID,
            ATK_RS7_PID,
            ATK_RS7_USAGE_PAGE,
            ATK_RS7_USAGE_ID,
            1,
            "mock://atk/rs7",
        )
        .with_strings(
            Some("Shenzhen Yizhita Technology".to_owned()),
            Some("ATK RS7 RGB".to_owned()),
            Some("ATK RS HE".to_owned()),
        )
    }

    fn push_open_replies(mock: &mut MockHidTransport, extreme: bool) {
        // Firmware v1.12 (0x01, 0x12)
        let mut fw = vec![0u8; RS7_PAYLOAD_LEN];
        fw[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        fw[1] = protocol::sub::CUSTOM_ID;
        fw[5] = 0x01;
        fw[6] = 0x12;
        mock.push_input_response(fw);

        // Game mode
        let mut gm = vec![0u8; RS7_PAYLOAD_LEN];
        gm[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        gm[1] = protocol::sub::CUSTOM_ID;
        gm[2] = protocol::sub::GAME_MODE;
        gm[3] = u8::from(extreme);
        mock.push_input_response(gm);

        // Polling rate = 8000 Hz (8)
        let mut rr = vec![0u8; RS7_PAYLOAD_LEN];
        rr[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        rr[1] = protocol::sub::CUSTOM_ID;
        rr[2] = protocol::sub::REPORT_RATE;
        rr[3] = 8;
        mock.push_input_response(rr);
    }

    #[test]
    fn safety_validator_hard_blocks_bootloader_iap_and_reset_commands() {
        let validator = atk_rs7_validator();

        // Valid single-key actuation write passes
        let ok_tx = wrap_tx(req_single_actuation_set(
            2,
            1,
            TravelHundredthsMm::from_hundredths(40),
        ));
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &ok_tx)
                .is_ok()
        );

        // Bootloader jump (0x0B) MUST be rejected
        let mut boot = [0u8; RS7_TX_REPORT_LEN];
        boot[1] = protocol::cmd::BOOTLOADER_JUMP;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &boot)
                .is_err()
        );

        // EEPROM reset (0x0A) MUST be rejected
        let mut eeprom = [0u8; RS7_TX_REPORT_LEN];
        eeprom[1] = protocol::cmd::EEPROM_RESET;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &eeprom)
                .is_err()
        );

        // DuckBread resetAll (0x96 / 0x15) MUST be rejected
        let mut reset_all = [0u8; RS7_TX_REPORT_LEN];
        reset_all[1] = protocol::cmd::SET_KEYBOARD_VALUE;
        reset_all[2] = protocol::sub::CUSTOM_ID;
        reset_all[3] = protocol::sub::RESET_ALL;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &reset_all)
                .is_err()
        );

        // DuckBread IAP (0x96 / 0x98) MUST be rejected
        let mut iap = [0u8; RS7_TX_REPORT_LEN];
        iap[1] = protocol::cmd::SET_KEYBOARD_VALUE;
        iap[2] = protocol::sub::CUSTOM_ID;
        iap[3] = protocol::sub::IAP;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &iap)
                .is_err()
        );
    }

    #[test]
    fn reads_rs7_discovery_actuation_rt_socd_calibration_and_rgb() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, true);

        // Deadzone: top 0.10mm (10), bottom 0.15mm (15)
        let mut dz = vec![0u8; RS7_PAYLOAD_LEN];
        dz[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        dz[3..5].copy_from_slice(&10u16.to_be_bytes());
        dz[5..7].copy_from_slice(&15u16.to_be_bytes());
        mock.push_input_response(dz);

        // 8 actuation chunks (12 keys each), all set to 1.50 mm (150 = 0x0096)
        for _ in 0..8 {
            let mut chunk = vec![0u8; RS7_PAYLOAD_LEN];
            chunk[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            for k in 0..12 {
                chunk[8 + k * 2..10 + k * 2].copy_from_slice(&150u16.to_be_bytes());
            }
            mock.push_input_response(chunk);
        }

        // 24 RT chunks (4 keys each), enabled with 0.20 mm press / 0.25 mm release
        for _ in 0..24 {
            let mut chunk = vec![0u8; RS7_PAYLOAD_LEN];
            chunk[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            for k in 0..4 {
                let b = 8 + k * 5;
                chunk[b] = 1;
                chunk[b + 1..b + 3].copy_from_slice(&25u16.to_be_bytes());
                chunk[b + 3..b + 5].copy_from_slice(&20u16.to_be_bytes());
            }
            mock.push_input_response(chunk);
        }

        // 20 SOCD slots: slot 0 = A(2,1) + D(2,3) LastInputPriority; slots 1..20 empty
        for slot in 0..20 {
            let mut s = vec![0u8; RS7_PAYLOAD_LEN];
            s[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            if slot == 0 {
                s[7] = protocol::socd_wire::SOCD_LAST;
                s[13] = 2;
                s[14] = 1;
                s[15] = 2;
                s[16] = 3;
            }
            mock.push_input_response(s);
        }

        // 20 DKS slots: empty
        for _ in 0..20 {
            let mut d = vec![0u8; RS7_PAYLOAD_LEN];
            d[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            mock.push_input_response(d);
        }

        // 4 calibration status chunks (24 keys each): calibrated
        for _ in 0..4 {
            let mut c = vec![0u8; RS7_PAYLOAD_LEN];
            c[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            for k in 0..24 {
                c[8 + k] = 1;
            }
            mock.push_input_response(c);
        }

        // Backlight RGB: Static cyan @ 80%
        let mut rgb = vec![0u8; RS7_PAYLOAD_LEN];
        rgb[0] = protocol::cmd::LIGHTING_GET_VALUE;
        rgb[2] = 2; // Static
        rgb[5] = 0x00;
        rgb[6] = 0xE5;
        rgb[7] = 0xFF;
        rgb[8] = 80;
        mock.push_input_response(rgb);

        let mut dev = AtkRs7Device::open(mock, false, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.12"));
        assert!(dev.is_extreme_mode());
        assert_eq!(dev.polling_rate(), Some(PollingRateHz::Hz8000));
        assert_eq!(
            dev.hall_effect_capabilities().actuation_step,
            TravelHundredthsMm::from_hundredths(1)
        );

        let snap = dev.read_hall_effect_state().unwrap();
        assert_eq!(snap.keys.len(), 96);
        assert_eq!(snap.keys[0].actuation.as_hundredths(), 150);
        assert_eq!(
            snap.keys[0].top_deadzone,
            Some(TravelHundredthsMm::from_hundredths(10))
        );
        assert!(snap.keys[0].rapid_trigger.is_enabled());
        assert_eq!(
            snap.keys[0].rapid_trigger.press_sensitivity.as_hundredths(),
            20
        );
        assert_eq!(
            snap.keys[0]
                .rapid_trigger
                .release_sensitivity
                .as_hundredths(),
            25
        );
        assert_eq!(snap.socd_pairs.len(), 1);
        assert_eq!(snap.socd_pairs[0].mode, SocdMode::LastInputPriority);
        assert_eq!(snap.calibration, CalibrationStatus::Calibrated);

        let light = dev.read_rgb_state().unwrap();
        assert_eq!(light.zones[0].brightness.percent(), 80);
        assert_eq!(
            light.zones[0].effect,
            RgbEffect::Static {
                color: RgbColor::new(0x00, 0xE5, 0xFF)
            }
        );
    }

    #[test]
    fn dry_run_validates_and_suppresses_actuation_rt_and_socd_writes() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, false);

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = AtkRs7Device::open(mock, true, Some(sink.clone())).unwrap();

        // Out-of-step (0.11 mm in normal 0.02 mm step mode) must fail validation before TX.
        assert!(
            dev.set_key_actuation(KeyIndex::new(33), TravelHundredthsMm::from_hundredths(11))
                .is_err()
        );

        // Valid writes must pass validator and be suppressed by dry-run.
        dev.set_key_actuation(KeyIndex::new(33), TravelHundredthsMm::from_hundredths(40))
            .unwrap();
        dev.set_rapid_trigger(
            KeyIndex::new(33),
            RapidTriggerConfig::new(
                true,
                true,
                TravelHundredthsMm::from_hundredths(20),
                TravelHundredthsMm::from_hundredths(20),
            ),
        )
        .unwrap();
        let pair = SocdPair::new(
            protocol::row_col_to_key(2, 1),
            protocol::row_col_to_key(2, 3),
            SocdMode::LastInputPriority,
        )
        .unwrap();
        dev.set_socd_pairs(&[pair]).unwrap();

        // Only the 3 read-only open() discovery packets touched the mock transport.
        assert_eq!(dev.transport().inner().recorded_writes().len(), 3);

        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 3);
    }

    #[test]
    fn socd_preserves_sparse_slots_skips_non_socd_and_clears_removed_slots() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, false);

        // Scan 20 slots:
        // - slot 0 holds non-SOCD Rappy Snappy (mode=1) -> must never be overwritten
        // - slot 4 holds SOCD A(3,1) + D(3,3) LastInputPriority -> must be updated in place
        for slot in 0..20u8 {
            let mut s = vec![0u8; RS7_PAYLOAD_LEN];
            s[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            if slot == 0 {
                s[7] = protocol::socd_wire::RS;
            } else if slot == 4 {
                s[7] = protocol::socd_wire::SOCD_LAST;
                s[13] = 3;
                s[14] = 1;
                s[15] = 3;
                s[16] = 3;
            }
            mock.push_input_response(s);
        }

        // Replies for adding W/S (writes slot 4 + slot 1, skipping reserved slot 0)
        for _ in 0..2 {
            let mut ack = vec![0u8; RS7_PAYLOAD_LEN];
            ack[0] = protocol::cmd::SET_KEYBOARD_VALUE;
            mock.push_input_response(ack);
        }
        // Replies for removing W/S (writes slot 4 + clears slot 1 with mode=NONE)
        for _ in 0..2 {
            let mut ack = vec![0u8; RS7_PAYLOAD_LEN];
            ack[0] = protocol::cmd::SET_KEYBOARD_VALUE;
            mock.push_input_response(ack);
        }

        let mut dev = AtkRs7Device::open(mock, false, None).unwrap();
        let ad = SocdPair::new(
            protocol::row_col_to_key(3, 1),
            protocol::row_col_to_key(3, 3),
            SocdMode::Neutral,
        )
        .unwrap();
        let ws = SocdPair::new(
            protocol::row_col_to_key(2, 2),
            protocol::row_col_to_key(3, 2),
            SocdMode::LastInputPriority,
        )
        .unwrap();

        // 1. Add W/S and change A/D mode -> updates slot 4 in place and claims slot 1 (skipping slot 0)
        dev.set_socd_pairs(&[ad, ws]).unwrap();
        // 2. Remove W/S -> keeps slot 4 and clears slot 1 with mode=NONE
        dev.set_socd_pairs(&[ad]).unwrap();

        let writes = dev.transport().inner().recorded_writes();
        // Filter SET_KEYBOARD_VALUE + RS_SOCD frames (byte 0 = Report ID 0x00, byte 1 = cmd 0x03)
        let socd_sets: Vec<&[u8]> = writes
            .iter()
            .map(|w| w.bytes.as_slice())
            .filter(|w| {
                w.len() == RS7_TX_REPORT_LEN
                    && w[1] == protocol::cmd::SET_KEYBOARD_VALUE
                    && w[2] == protocol::sub::CUSTOM_ID
                    && w[3] == protocol::sub::RS_SOCD
            })
            .collect();
        assert_eq!(socd_sets.len(), 4);
        // Write 0: slot 4 updated to Neutral (mode=5)
        assert_eq!(socd_sets[0][7], 4);
        assert_eq!(socd_sets[0][8], protocol::socd_wire::SOCD_BALANCE);
        // Write 1: slot 1 claimed for W/S (slot 0 preserved for RS!)
        assert_eq!(socd_sets[1][7], 1);
        assert_eq!(socd_sets[1][8], protocol::socd_wire::SOCD_LAST);
        // Write 2: slot 4 rewritten with A/D
        assert_eq!(socd_sets[2][7], 4);
        assert_eq!(socd_sets[2][8], protocol::socd_wire::SOCD_BALANCE);
        // Write 3: slot 1 explicitly cleared with NONE (0)
        assert_eq!(socd_sets[3][7], 1);
        assert_eq!(socd_sets[3][8], protocol::socd_wire::NONE);
    }
}
