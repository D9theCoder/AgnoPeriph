//! Lamzu peripheral driver (Atlantis CompX Report-8 + extensible Aurora codec seam).

#![forbid(unsafe_code)]

pub mod atlantis;
pub mod aurora;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, CoreError, CoreResult, DebounceMs, DeviceId,
    DeviceInfo, DeviceMatchRule, DpiStage, DriverDescriptor, LiftOffDistance, Peripheral,
    PointerCapabilities, PointerDevice, PointerSnapshot, PollingRateHz, ProfileCapabilities,
    ProfileInfo, ProfileManager, ProfileMode, ProfileSlot, ProfileSnapshot, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::atlantis::{
    ATLANTIS_MAX_PAYLOAD, ATLANTIS_PACKET_LEN, ATLANTIS_PRODUCTS, AtlantisProductProfile,
    AtlantisRateFamily, AtlantisReply, Command, LAMZU_ATLANTIS_USAGE_ID, LAMZU_ATLANTIS_USAGE_PAGE,
    LAMZU_ATLANTIS_VID, atlantis_validator, decode_battery, decode_dpi_color, decode_dpi_stage,
    decode_firmware, decode_lod, decode_polling_rate, decode_reply, encode_dpi_stage, encode_lod,
    encode_polling_rate, encode_request, flash, is_checksum_valid, lookup_product, seal_field,
};

const READ_TIMEOUT: Duration = Duration::from_millis(600);
const MAX_READ_ATTEMPTS: usize = 6;

/// Returns the declarative `DriverDescriptor` for registering Lamzu Atlantis wired and dongle PIDs.
pub fn driver_descriptor() -> DriverDescriptor {
    let rules = ATLANTIS_PRODUCTS
        .iter()
        .map(|profile| {
            DeviceMatchRule::new(LAMZU_ATLANTIS_VID, profile.connection, profile.model_name)
                .with_pid(profile.pid)
                .with_usage_page(LAMZU_ATLANTIS_USAGE_PAGE)
                .with_usage_id(LAMZU_ATLANTIS_USAGE_ID)
        })
        .collect();

    DriverDescriptor::new("lamzu-atlantis", VendorKind::Lamzu, rules)
}

/// Active Lamzu Atlantis peripheral backed by a checksum-validated [`GuardedTransport`].
pub struct LamzuAtlantisDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    profile: AtlantisProductProfile,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
    profile_caps: ProfileCapabilities,
}

impl<T: HidTransport> fmt::Debug for LamzuAtlantisDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LamzuAtlantisDevice")
            .field("profile", &self.profile)
            .field("info", &self.info)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> LamzuAtlantisDevice<T> {
    /// Connects to a Lamzu Atlantis wired or wireless receiver HID interface,
    /// enforces strict Report-8 17-byte + checksum validation, and reads firmware info.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != LAMZU_ATLANTIS_VID {
            return Err(CoreError::Validation(format!(
                "expected Lamzu Atlantis VID 0x{LAMZU_ATLANTIS_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        let profile = lookup_product(desc.pid).ok_or_else(|| {
            CoreError::Validation(format!("unsupported Lamzu Atlantis PID 0x{:04X}", desc.pid))
        })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(atlantis_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "lamzu:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let supported_rates = match profile.rate_family {
            AtlantisRateFamily::Wired => vec![
                PollingRateHz::Hz125,
                PollingRateHz::Hz250,
                PollingRateHz::Hz500,
                PollingRateHz::Hz1000,
            ],
            AtlantisRateFamily::Pulsar4k => vec![
                PollingRateHz::Hz125,
                PollingRateHz::Hz250,
                PollingRateHz::Hz500,
                PollingRateHz::Hz1000,
                PollingRateHz::Hz2000,
                PollingRateHz::Hz4000,
            ],
        };

        let pointer_caps = PointerCapabilities::new(
            flash::MIN_DPI,
            flash::MAX_DPI,
            flash::DPI_STEP,
            flash::MAX_DPI_STAGES,
        )
        .with_xy_independent_dpi(true)
        .with_polling_rates(supported_rates)
        .with_lods(vec![
            LiftOffDistance::TenthsMm(10),
            LiftOffDistance::TenthsMm(20),
        ])
        .with_debounce_range_ms(0, flash::MAX_DEBOUNCE_MS)
        .with_motion_sync(true)
        .with_angle_snapping(true)
        .with_ripple_control(true);

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Lamzu,
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
            capabilities: Capabilities::empty()
                .with_pointer(true)
                .with_battery(true)
                .with_profiles(true),
            pointer_caps,
            profile_caps: ProfileCapabilities::new(flash::PROFILE_COUNT, false, false),
        };

        // Read-only firmware query during open (non-fatal if dongle reports sleeping mouse).
        if let Ok(reply) = dev.send_command(Command::ReadFirmwareVersion, 0, &[], true) {
            dev.info.firmware_version = Some(decode_firmware(&reply.payload));
        }

        Ok(dev)
    }

    /// Returns the underlying [`GuardedTransport`] for inspecting traces or mock recordings.
    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn send_command(
        &mut self,
        cmd: Command,
        address: u16,
        payload: &[u8],
        ignore_len: bool,
    ) -> CoreResult<AtlantisReply> {
        let req = encode_request(cmd, address, payload)?;
        let is_mutating = matches!(cmd, Command::WriteFlash | Command::WriteActiveProfile);

        if is_mutating {
            self.transport.send_output_report(&req)?;
            if self.transport.is_dry_run() {
                return Ok(AtlantisReply {
                    command: cmd,
                    error: 0,
                    address,
                    declared_len: payload.len(),
                    payload: [0u8; ATLANTIS_MAX_PAYLOAD],
                });
            }
        } else {
            self.transport.send_query_output_report(&req)?;
        }

        let mut buf = [0u8; ATLANTIS_PACKET_LEN];
        for _ in 0..MAX_READ_ATTEMPTS {
            let read_len = match self.transport.read_input_report(&mut buf, READ_TIMEOUT) {
                Ok(n) => n,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for Lamzu Atlantis reply to {cmd:?}"
                    )));
                }
                Err(err) => return Err(err.into()),
            };
            if let Some(reply) = decode_reply(&buf[..read_len])
                && reply.command == cmd
                && (ignore_len || reply.declared_len == payload.len())
            {
                if reply.error != 0 {
                    return Err(CoreError::Protocol(format!(
                        "Lamzu Atlantis returned error status 0x{:02X} for {cmd:?}",
                        reply.error
                    )));
                }
                return Ok(reply);
            }
        }

        Err(CoreError::Protocol(format!(
            "no matching Lamzu Atlantis reply for {cmd:?}"
        )))
    }

    fn read_flash_raw(&mut self, address: u16, len: usize) -> CoreResult<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let chunk_len = (len - out.len()).min(ATLANTIS_MAX_PAYLOAD);
            let offset = u16::try_from(out.len()).unwrap_or(0);
            let zeros = vec![0u8; chunk_len];
            let reply = self.send_command(
                Command::ReadFlash,
                address.saturating_add(offset),
                &zeros,
                false,
            )?;
            out.extend_from_slice(&reply.payload[..chunk_len]);
        }
        Ok(out)
    }

    fn write_flash_raw(&mut self, mut address: u16, data: &[u8]) -> CoreResult<()> {
        for chunk in data.chunks(ATLANTIS_MAX_PAYLOAD) {
            self.send_command(Command::WriteFlash, address, chunk, false)?;
            address = address.saturating_add(u16::try_from(chunk.len()).unwrap_or(0));
        }
        Ok(())
    }

    fn read_flash_checked(&mut self, address: u16, data_len: usize) -> CoreResult<Vec<u8>> {
        let mut raw = self.read_flash_raw(address, data_len + 1)?;
        if !is_checksum_valid(&raw) {
            return Err(CoreError::Protocol(format!(
                "Lamzu flash field checksum mismatch at address 0x{address:04X}"
            )));
        }
        raw.pop();
        Ok(raw)
    }

    fn write_flash_checked(&mut self, address: u16, values: &[u8]) -> CoreResult<()> {
        let sealed = seal_field(values);
        self.write_flash_raw(address, &sealed)
    }

    fn read_flash_u8(&mut self, address: u16) -> CoreResult<u8> {
        Ok(self.read_flash_checked(address, 1)?[0])
    }

    fn write_flash_u8(&mut self, address: u16, value: u8) -> CoreResult<()> {
        self.write_flash_checked(address, &[value])
    }
}

impl<T: HidTransport> Peripheral for LamzuAtlantisDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let reply = self.send_command(Command::ReadFirmwareVersion, 0, &[], true)?;
        self.info.firmware_version = Some(decode_firmware(&reply.payload));
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
        Some(self)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        Some(self)
    }
}

impl<T: HidTransport> BatteryMonitor for LamzuAtlantisDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let reply = self.send_command(Command::ReadBattery, 0, &[], true)?;
        Ok(decode_battery(&reply.payload))
    }
}

impl<T: HidTransport> PointerDevice for LamzuAtlantisDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        let raw_rate = self.read_flash_u8(flash::POLL_RATE)?;
        let polling_rate = decode_polling_rate(raw_rate, self.profile.rate_family)?;

        let stage_count = self.read_flash_u8(flash::DPI_STAGE_COUNT)?;
        if !(1..=flash::MAX_DPI_STAGES).contains(&stage_count) {
            return Err(CoreError::Protocol(format!(
                "invalid Atlantis DPI stage count {stage_count}"
            )));
        }
        let active_stage = self.read_flash_u8(flash::ACTIVE_DPI_STAGE)?;

        let mut stages = Vec::with_capacity(usize::from(stage_count));
        for ix in 0..stage_count {
            let offset = u16::from(ix) * flash::DPI_STAGE_STRIDE;
            let raw_dpi = self.read_flash_raw(flash::DPI_STAGES + offset, 4)?;
            let mut stage = decode_dpi_stage(ix, &raw_dpi)?;
            if let Ok(raw_color) = self.read_flash_raw(flash::DPI_COLORS + offset, 4)
                && let Ok(color) = decode_dpi_color(&raw_color)
            {
                stage = stage.with_indicator_color(color);
            }
            stages.push(stage);
        }

        let lod = decode_lod(self.read_flash_u8(flash::LIFT_OFF_DISTANCE)?)?;
        let debounce = DebounceMs::new(self.read_flash_u8(flash::DEBOUNCE_MS)?);
        let motion_sync = self.read_flash_u8(flash::MOTION_SYNC)? != 0;
        let angle_snapping = self.read_flash_u8(flash::ANGLE_SNAPPING)? != 0;
        let ripple_control = self.read_flash_u8(flash::RIPPLE_CONTROL)? != 0;

        Ok(PointerSnapshot::new(active_stage, stages, polling_rate)
            .with_lod(lod)
            .with_debounce_ms(debounce)
            .with_motion_sync(motion_sync)
            .with_angle_snapping(angle_snapping)
            .with_ripple_control(ripple_control))
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        let count = u8::try_from(stages.len()).unwrap_or(1);
        self.write_flash_u8(flash::DPI_STAGE_COUNT, count)?;
        for (ix, stage) in stages.iter().enumerate() {
            let offset = u16::try_from(ix).unwrap_or(0) * flash::DPI_STAGE_STRIDE;
            let encoded = encode_dpi_stage(stage)?;
            self.write_flash_raw(flash::DPI_STAGES + offset, &encoded)?;
            if let Some(color) = stage.indicator_color {
                self.write_flash_checked(flash::DPI_COLORS + offset, &[color.r, color.g, color.b])?;
            }
        }
        self.write_flash_u8(flash::ACTIVE_DPI_STAGE, active_ix)
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if stage_ix >= flash::MAX_DPI_STAGES {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{}", flash::MAX_DPI_STAGES),
            });
        }
        self.write_flash_u8(flash::ACTIVE_DPI_STAGE, stage_ix)
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        let raw = encode_polling_rate(rate, self.profile.rate_family)?;
        self.write_flash_u8(flash::POLL_RATE, raw)
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        let raw = encode_lod(lod)?;
        self.write_flash_u8(flash::LIFT_OFF_DISTANCE, raw)
    }

    fn set_debounce_ms(&mut self, debounce: DebounceMs) -> CoreResult<()> {
        self.pointer_caps.validate_debounce(debounce)?;
        self.write_flash_u8(flash::DEBOUNCE_MS, debounce.value())
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_flash_u8(flash::MOTION_SYNC, u8::from(enabled))
    }

    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_flash_u8(flash::ANGLE_SNAPPING, u8::from(enabled))
    }

    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_flash_u8(flash::RIPPLE_CONTROL, u8::from(enabled))
    }
}

impl<T: HidTransport> ProfileManager for LamzuAtlantisDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        self.profile_caps.clone()
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let reply = self.send_command(Command::ReadActiveProfile, 0, &[], true)?;
        let active = ProfileSlot::new(reply.payload[0]);
        self.profile_caps.validate_slot(active)?;
        let profiles = (0..self.profile_caps.slot_count)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect();
        Ok(ProfileSnapshot::new(
            active,
            ProfileMode::OnboardMemory,
            profiles,
        ))
    }

    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()> {
        self.profile_caps.validate_slot(slot)?;
        self.send_command(Command::WriteActiveProfile, 0, &[slot.index()], false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::{ChargingState, DpiValue, RgbColor};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    fn reply_frame(cmd: Command, addr: u16, declared_len: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; ATLANTIS_PACKET_LEN];
        frame[0] = crate::atlantis::ATLANTIS_REPORT_ID;
        frame[1] = cmd as u8;
        frame[2] = 0x00;
        let [hi, lo] = addr.to_be_bytes();
        frame[3] = hi;
        frame[4] = lo;
        frame[5] = declared_len;
        frame[6..6 + payload.len()].copy_from_slice(payload);
        frame[16] = crate::atlantis::compute_checksum(&frame[..16]);
        frame
    }

    fn atlantis_4k_descriptor() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            LAMZU_ATLANTIS_VID,
            0xF510,
            LAMZU_ATLANTIS_USAGE_PAGE,
            LAMZU_ATLANTIS_USAGE_ID,
            1,
            "mock://lamzu/atlantis-4k",
        )
    }

    #[test]
    fn checksum_worked_example_and_dpi_stage_roundtrip() {
        // Worked example from docs/protocols/lamzu-compx.md
        let batt_req = encode_request(Command::ReadBattery, 0, &[]).unwrap();
        assert_eq!(batt_req[0], 0x08);
        assert_eq!(batt_req[1], 0x04);
        assert_eq!(batt_req[16], 0x49);
        assert!(is_checksum_valid(&batt_req));

        // 1600 DPI X / 3200 DPI Y round-trip across 10-bit flags byte
        let stage =
            DpiStage::new(0, DpiValue::new(1600).unwrap()).with_y(DpiValue::new(3200).unwrap());
        let encoded = encode_dpi_stage(&stage).unwrap();
        assert!(is_checksum_valid(&encoded));
        let decoded = decode_dpi_stage(0, &encoded).unwrap();
        assert_eq!(decoded.x.value(), 1600);
        assert_eq!(decoded.effective_y().value(), 3200);

        // High DPI > 12800 uses upper 2 bits in flags byte (e.g., 26000 DPI => raw 519 = 0x207)
        let high = DpiStage::new(1, DpiValue::new(26_000).unwrap());
        let high_enc = encode_dpi_stage(&high).unwrap();
        let high_dec = decode_dpi_stage(1, &high_enc).unwrap();
        assert_eq!(high_dec.x.value(), 26_000);
    }

    #[test]
    fn reads_full_atlantis_pointer_battery_and_profile_state() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        // Open firmware version -> v1.24
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x01, 0x24],
        ));
        // Battery -> 95%, charging, 4239 mV (0x108F)
        mock.push_input_response(reply_frame(
            Command::ReadBattery,
            0,
            2,
            &[95, 1, 0x10, 0x8F],
        ));
        // Polling rate -> 0x20 (4000 Hz on Pulsar4k receiver)
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::POLL_RATE,
            2,
            &seal_field(&[0x20]),
        ));
        // Stage count -> 1, Active stage -> 0
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DPI_STAGE_COUNT,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::ACTIVE_DPI_STAGE,
            2,
            &seal_field(&[0]),
        ));
        // Stage 0 -> 800 DPI + cyan indicator LED
        let stage0 = encode_dpi_stage(&DpiStage::new(0, DpiValue::new(800).unwrap())).unwrap();
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DPI_STAGES,
            4,
            &stage0,
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DPI_COLORS,
            4,
            &seal_field(&[0x00, 0xFF, 0xCC]),
        ));
        // LOD 1mm, Debounce 2ms, MotionSync on, AngleSnapping off, Ripple off
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::LIFT_OFF_DISTANCE,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DEBOUNCE_MS,
            2,
            &seal_field(&[2]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::MOTION_SYNC,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::ANGLE_SNAPPING,
            2,
            &seal_field(&[0]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::RIPPLE_CONTROL,
            2,
            &seal_field(&[0]),
        ));
        // Active profile -> slot 2
        mock.push_input_response(reply_frame(Command::ReadActiveProfile, 0, 1, &[2]));

        let mut dev = LamzuAtlantisDevice::open(mock, false, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.24"));

        let batt = dev.read_battery().unwrap();
        assert_eq!(batt.percentage, Some(95));
        assert_eq!(batt.voltage_mv, Some(4239));
        assert_eq!(batt.state, ChargingState::Charging);

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.polling_rate, PollingRateHz::Hz4000);
        assert_eq!(snap.dpi_stages.len(), 1);
        assert_eq!(snap.dpi_stages[0].x.value(), 800);
        assert_eq!(
            snap.dpi_stages[0].indicator_color,
            Some(RgbColor::new(0x00, 0xFF, 0xCC))
        );
        assert_eq!(snap.lod, Some(LiftOffDistance::TenthsMm(10)));
        assert_eq!(snap.debounce_ms, Some(DebounceMs::new(2)));
        assert_eq!(snap.motion_sync, Some(true));

        let prof = dev.read_profile_state().unwrap();
        assert_eq!(prof.active_slot, ProfileSlot::new(2));
    }

    #[test]
    fn dry_run_validates_and_suppresses_flash_writes() {
        let mock = MockHidTransport::new(atlantis_4k_descriptor());
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LamzuAtlantisDevice::open(mock, true, Some(sink.clone())).unwrap();

        dev.set_polling_rate(PollingRateHz::Hz2000).unwrap();
        dev.set_lod(LiftOffDistance::TenthsMm(20)).unwrap();
        dev.set_debounce_ms(DebounceMs::new(4)).unwrap();
        dev.set_motion_sync(true).unwrap();
        dev.set_active_profile(ProfileSlot::new(1)).unwrap();

        // Only the read-only firmware query during open() reached the mock transport;
        // zero mutating flash/profile writes were transmitted.
        assert_eq!(dev.transport().inner().recorded_writes().len(), 1);
        assert_eq!(
            dev.transport().inner().recorded_writes()[0].bytes[1],
            Command::ReadFirmwareVersion as u8
        );

        // Every write was validated and logged as DryRunSuppressed.
        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert!(suppressed >= 5);
    }

    #[test]
    fn aurora_codec_encodes_and_decodes_cleanly() {
        let req = aurora::encode_page_request(0x01, 0x04, 0x02, 0x08, &[0xAA, 0xBB]);
        assert_eq!(req.len(), 64);
        assert_eq!(&req[..8], &[0x00, 0x00, 0x01, 0x04, 0x02, 0x08, 0xAA, 0xBB]);

        let dpi_payload = [0x00, 0x02, 0x03, 0x20, 0x03, 0x20, 0x06, 0x40, 0x06, 0x40];
        let stages = aurora::decode_dpi_stages(&dpi_payload).unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].x.value(), 800);
        assert_eq!(stages[1].x.value(), 1600);
        assert_eq!(
            aurora::decode_lift_off(0x87),
            Some(LiftOffDistance::TenthsMm(7))
        );
        assert_eq!(
            aurora::decode_firmware(&[0, 0, 2, 14]).as_deref(),
            Some("v2.14")
        );
    }
}
