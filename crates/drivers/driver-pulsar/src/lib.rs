//! Pulsar Gaming Gears mouse driver (Report-8 flash protocol + XS-1 codec).

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, ConnectionKind, CoreError, CoreResult, DebounceMs,
    DeviceId, DeviceInfo, DeviceMatchRule, DpiStage, DriverDescriptor, LiftOffDistance, Peripheral,
    PointerCapabilities, PointerDevice, PointerSnapshot, PollingRateHz, ProfileCapabilities,
    ProfileInfo, ProfileManager, ProfileMode, ProfileSlot, ProfileSnapshot, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::protocol::{
    Command, PULSAR_MAX_PAYLOAD, PULSAR_PACKET_LEN, PULSAR_USAGE_ID, PULSAR_USAGE_PAGE, PULSAR_VID,
    PulsarHandshakeInfo, PulsarReply, decode_battery, decode_dpi_stage, decode_firmware,
    decode_lod, decode_polling_rate, decode_reply, encode_dpi_stage, encode_lod,
    encode_polling_rate, encode_request, flash, is_checksum_valid, pulsar_validator, seal_field,
};

const READ_TIMEOUT: Duration = Duration::from_millis(600);
const MAX_READ_ATTEMPTS: usize = 6;
const MAX_ONLINE_POLLS: usize = 10;

/// Declarative `DriverDescriptor` for Pulsar Report-8 (`VID 0x3710`, `0xFF02:0x0002`) peripherals.
pub fn driver_descriptor() -> DriverDescriptor {
    DriverDescriptor::new(
        "pulsar-hid",
        VendorKind::Pulsar,
        vec![
            DeviceMatchRule::new(
                PULSAR_VID,
                ConnectionKind::WirelessDongle24Ghz,
                "Pulsar Mouse",
            )
            .with_usage_page(PULSAR_USAGE_PAGE)
            .with_usage_id(PULSAR_USAGE_ID),
        ],
    )
}

/// Active Pulsar mouse/receiver backed by a checksum-validated [`GuardedTransport`].
pub struct PulsarDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    handshake: PulsarHandshakeInfo,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
    profile_caps: ProfileCapabilities,
}

impl<T: HidTransport> fmt::Debug for PulsarDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PulsarDevice")
            .field("handshake", &self.handshake)
            .field("info", &self.info)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> PulsarDevice<T> {
    /// Connects to a Pulsar Report-8 mouse/receiver, runs the `EncryptionData` (`0x01`)
    /// identification handshake, and reads firmware version.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != PULSAR_VID {
            return Err(CoreError::Validation(format!(
                "expected Pulsar VID 0x{PULSAR_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(pulsar_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "pulsar:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let mut dev = Self {
            transport: guarded,
            handshake: PulsarHandshakeInfo {
                cid: 0,
                mid: 0,
                device_type: 0,
                dongle_type: 0,
                connection: ConnectionKind::WirelessDongle24Ghz,
                max_polling_hz: 1000,
            },
            info: DeviceInfo::new(
                stable_id,
                VendorKind::Pulsar,
                desc.product
                    .clone()
                    .unwrap_or_else(|| "Pulsar Mouse".to_owned()),
                ConnectionKind::WirelessDongle24Ghz,
                desc.vid,
                desc.pid,
            ),
            capabilities: Capabilities::empty()
                .with_pointer(true)
                .with_battery(true)
                .with_profiles(true),
            pointer_caps: build_pointer_caps(1000),
            profile_caps: ProfileCapabilities::new(flash::PROFILE_COUNT, false, false),
        };

        if let Some(serial) = desc.serial_number {
            dev.info = dev.info.with_serial_number(serial);
        }

        // Fixed deterministic read-only challenge for `EncryptionData` identification.
        let challenge = [0x11, 0x22, 0x33, 0x44, 0x00, 0x00, 0x00, 0x00];
        if let Ok(reply) = dev.send_command(Command::EncryptionData, 0, &challenge, true) {
            let hs = PulsarHandshakeInfo::from_reply_payload(&reply.payload);
            dev.handshake = hs;
            dev.info.connection = hs.connection;
            if let Some(model) = hs.model_hint() {
                model.clone_into(&mut dev.info.model_name);
            }
            dev.pointer_caps = build_pointer_caps(hs.max_polling_hz);
        }

        if let Ok(reply) = dev.send_command(Command::ReadVersionId, 0, &[], true)
            && let Some(fw) = decode_firmware(&reply.payload)
        {
            dev.info.firmware_version = Some(fw);
        }

        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn send_command(
        &mut self,
        cmd: Command,
        address: u16,
        payload: &[u8],
        ignore_len: bool,
    ) -> CoreResult<PulsarReply> {
        let req = encode_request(cmd, address, payload)?;
        let is_mutating = matches!(cmd, Command::WriteFlash | Command::SetCurrentConfig);

        if is_mutating {
            self.transport.send_output_report(&req)?;
            if self.transport.is_dry_run() {
                return Ok(PulsarReply {
                    command: cmd,
                    error: 0,
                    address,
                    declared_len: payload.len(),
                    payload: [0u8; PULSAR_MAX_PAYLOAD],
                });
            }
        } else {
            self.transport.send_query_output_report(&req)?;
        }

        let mut buf = [0u8; PULSAR_PACKET_LEN];
        for _ in 0..MAX_READ_ATTEMPTS {
            let read_len = match self.transport.read_input_report(&mut buf, READ_TIMEOUT) {
                Ok(n) => n,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for Pulsar reply to {cmd:?}"
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
                        "Pulsar returned error status 0x{:02X} for {cmd:?}",
                        reply.error
                    )));
                }
                return Ok(reply);
            }
        }

        Err(CoreError::Protocol(format!(
            "no matching Pulsar reply for {cmd:?}"
        )))
    }

    fn set_device_online(&mut self, enabled: bool) -> CoreResult<()> {
        for _ in 0..MAX_ONLINE_POLLS {
            let reply = self.send_command(Command::DeviceOnline, 0, &[u8::from(enabled)], true)?;
            // `payload[4]` == 1 means receiver busy.
            if reply.payload[4] == 1 {
                continue;
            }
            if enabled && reply.payload[0] != 1 {
                return Err(CoreError::NotConnected(
                    "Pulsar mouse is offline or asleep".to_owned(),
                ));
            }
            return Ok(());
        }
        Err(CoreError::NotConnected(
            "Pulsar receiver remained busy".to_owned(),
        ))
    }

    fn with_device_control<R, F>(&mut self, op: F) -> CoreResult<R>
    where
        F: FnOnce(&mut Self) -> CoreResult<R>,
    {
        if self.transport.is_dry_run() {
            return op(self);
        }
        self.set_device_online(true)?;
        let result = op(self);
        let _ = self.set_device_online(false);
        result
    }

    fn read_flash_raw(&mut self, address: u16, len: usize) -> CoreResult<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let chunk_len = (len - out.len()).min(PULSAR_MAX_PAYLOAD);
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
        for chunk in data.chunks(PULSAR_MAX_PAYLOAD) {
            self.send_command(Command::WriteFlash, address, chunk, false)?;
            address = address.saturating_add(u16::try_from(chunk.len()).unwrap_or(0));
        }
        Ok(())
    }

    fn read_flash_u8(&mut self, address: u16) -> CoreResult<u8> {
        let raw = self.read_flash_raw(address, 2)?;
        if !is_checksum_valid(&raw) {
            return Err(CoreError::Protocol(format!(
                "Pulsar flash checksum mismatch at 0x{address:04X}"
            )));
        }
        Ok(raw[0])
    }

    fn write_flash_u8(&mut self, address: u16, value: u8) -> CoreResult<()> {
        let sealed = seal_field(&[value]);
        self.write_flash_raw(address, &sealed)
    }
}

fn build_pointer_caps(max_polling_hz: u16) -> PointerCapabilities {
    let all_rates = [
        PollingRateHz::Hz125,
        PollingRateHz::Hz250,
        PollingRateHz::Hz500,
        PollingRateHz::Hz1000,
        PollingRateHz::Hz2000,
        PollingRateHz::Hz4000,
        PollingRateHz::Hz8000,
    ];
    let supported: Vec<PollingRateHz> = all_rates
        .into_iter()
        .filter(|r| r.as_hz() <= max_polling_hz)
        .collect();

    PointerCapabilities::new(
        flash::MIN_DPI,
        flash::MAX_DPI,
        flash::DPI_STEP,
        flash::MAX_DPI_STAGES,
    )
    .with_polling_rates(supported)
    .with_lods(vec![
        LiftOffDistance::Low,
        LiftOffDistance::Medium,
        LiftOffDistance::High,
    ])
    .with_debounce_range_ms(0, flash::MAX_DEBOUNCE_MS)
    .with_motion_sync(true)
    .with_angle_snapping(true)
    .with_ripple_control(true)
}

impl<T: HidTransport> Peripheral for PulsarDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let reply = self.send_command(Command::ReadVersionId, 0, &[], true)?;
        if let Some(fw) = decode_firmware(&reply.payload) {
            self.info.firmware_version = Some(fw);
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
        Some(self)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        Some(self)
    }
}

impl<T: HidTransport> BatteryMonitor for PulsarDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let reply = self.send_command(Command::BatteryLevel, 0, &[], true)?;
        Ok(decode_battery(&reply.payload))
    }
}

impl<T: HidTransport> PointerDevice for PulsarDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        self.with_device_control(|this| {
            let polling_rate = decode_polling_rate(this.read_flash_u8(flash::POLL_RATE)?)?;
            let stage_count = this
                .read_flash_u8(flash::DPI_STAGE_COUNT)
                .unwrap_or(1)
                .clamp(1, flash::MAX_DPI_STAGES);
            let active_stage = this
                .read_flash_u8(flash::ACTIVE_DPI_STAGE)?
                .min(stage_count.saturating_sub(1));

            let mut stages = Vec::with_capacity(usize::from(stage_count));
            for ix in 0..stage_count {
                let offset = u16::from(ix) * flash::DPI_STAGE_STRIDE;
                let raw = this.read_flash_raw(flash::DPI_STAGES + offset, 4)?;
                stages.push(decode_dpi_stage(ix, &raw)?);
            }

            let mut snap = PointerSnapshot::new(active_stage, stages, polling_rate);
            if let Ok(raw_lod) = this.read_flash_u8(flash::LIFT_OFF_DISTANCE)
                && let Some(lod) = decode_lod(raw_lod)
            {
                snap = snap.with_lod(lod);
            }
            if let Ok(debounce) = this.read_flash_u8(flash::DEBOUNCE_MS) {
                snap = snap.with_debounce_ms(DebounceMs::new(debounce));
            }
            if let Ok(ms) = this.read_flash_u8(flash::MOTION_SYNC) {
                snap = snap.with_motion_sync(ms != 0);
            }
            if let Ok(asnap) = this.read_flash_u8(flash::ANGLE_SNAPPING) {
                snap = snap.with_angle_snapping(asnap != 0);
            }
            if let Ok(rc) = this.read_flash_u8(flash::RIPPLE_CONTROL) {
                snap = snap.with_ripple_control(rc != 0);
            }
            Ok(snap)
        })
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        self.with_device_control(|this| {
            let count = u8::try_from(stages.len()).unwrap_or(1);
            this.write_flash_u8(flash::DPI_STAGE_COUNT, count)?;
            for (ix, stage) in stages.iter().enumerate() {
                let offset = u16::try_from(ix).unwrap_or(0) * flash::DPI_STAGE_STRIDE;
                let encoded = encode_dpi_stage(stage)?;
                this.write_flash_raw(flash::DPI_STAGES + offset, &encoded)?;
            }
            this.write_flash_u8(flash::ACTIVE_DPI_STAGE, active_ix)
        })
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if stage_ix >= flash::MAX_DPI_STAGES {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{}", flash::MAX_DPI_STAGES),
            });
        }
        self.with_device_control(|this| this.write_flash_u8(flash::ACTIVE_DPI_STAGE, stage_ix))
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        let encoded = encode_polling_rate(rate)?;
        self.with_device_control(|this| this.write_flash_u8(flash::POLL_RATE, encoded))
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        let encoded = encode_lod(lod)?;
        self.with_device_control(|this| this.write_flash_u8(flash::LIFT_OFF_DISTANCE, encoded))
    }

    fn set_debounce_ms(&mut self, debounce: DebounceMs) -> CoreResult<()> {
        self.pointer_caps.validate_debounce(debounce)?;
        self.with_device_control(|this| this.write_flash_u8(flash::DEBOUNCE_MS, debounce.value()))
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        self.with_device_control(|this| this.write_flash_u8(flash::MOTION_SYNC, u8::from(enabled)))
    }

    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        self.with_device_control(|this| {
            this.write_flash_u8(flash::ANGLE_SNAPPING, u8::from(enabled))
        })
    }

    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        self.with_device_control(|this| {
            this.write_flash_u8(flash::RIPPLE_CONTROL, u8::from(enabled))
        })
    }
}

impl<T: HidTransport> ProfileManager for PulsarDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        self.profile_caps.clone()
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let reply = self.send_command(Command::GetCurrentConfig, 0, &[], true)?;
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
        self.send_command(Command::SetCurrentConfig, 0, &[slot.index()], false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PULSAR_REPORT_ID, compute_checksum, xs1_checksum, xs1_encode_request};
    use agnoperiph_core::{ChargingState, DpiValue};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    fn reply(cmd: Command, addr: u16, declared_len: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; PULSAR_PACKET_LEN];
        frame[0] = PULSAR_REPORT_ID;
        frame[1] = cmd as u8;
        let [hi, lo] = addr.to_be_bytes();
        frame[3] = hi;
        frame[4] = lo;
        frame[5] = declared_len;
        frame[6..6 + payload.len()].copy_from_slice(payload);
        frame[16] = compute_checksum(&frame[..16]);
        frame
    }

    fn pulsar_desc() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            PULSAR_VID,
            0x5401,
            PULSAR_USAGE_PAGE,
            PULSAR_USAGE_ID,
            1,
            "mock://pulsar/x2-crazylight",
        )
    }

    #[test]
    fn pulsar_dpi_ex_roundtrip_across_all_three_ranges() {
        for dpi in [10, 800, 1600, 10_000, 10_050, 26_000, 30_100, 32_000] {
            let stage = DpiStage::new(0, DpiValue::new(dpi).unwrap());
            let encoded = encode_dpi_stage(&stage).unwrap();
            assert!(is_checksum_valid(&encoded));
            let decoded = decode_dpi_stage(0, &encoded).unwrap();
            assert_eq!(decoded.x.value(), dpi);
        }
        // Spec worked example: 800 DPI -> [0x4F, 0x4F, 0x00, 0xB7]
        let enc_800 = encode_dpi_stage(&DpiStage::new(0, DpiValue::new(800).unwrap())).unwrap();
        assert_eq!(enc_800, [0x4F, 0x4F, 0x00, 0xB7]);

        // XS-1 checksum sanity
        let xs1 = xs1_encode_request(&[0x01, 0x02, 0x03]);
        let crc = u16::from_le_bytes([xs1[62], xs1[63]]);
        assert_eq!(crc, xs1_checksum(&xs1));
        assert_eq!(crc, 6);
    }

    #[test]
    fn reads_crazylight_handshake_battery_pointer_and_profiles() {
        let mut mock = MockHidTransport::new(pulsar_desc());
        // EncryptionData reply: CID 0x57, MID 0x04, type 5 (Wireless 8000 Hz)
        mock.push_input_response(reply(
            Command::EncryptionData,
            0,
            8,
            &[0, 0, 0, 0, 0x57, 0x04, 5, 3],
        ));
        // Firmware v1.18
        mock.push_input_response(reply(Command::ReadVersionId, 0, 2, &[0x01, 0x18]));
        // Battery 88%, discharging, 4010 mV
        mock.push_input_response(reply(Command::BatteryLevel, 0, 4, &[88, 0, 0x0F, 0xAA]));
        // DeviceOnline(true)
        mock.push_input_response(reply(Command::DeviceOnline, 0, 5, &[1, 0, 0, 0, 0]));
        // Flash reads: poll=0x40 (8000 Hz), count=1, active=0, stage0=1600, lod=3 (Low), debounce=2, ms=1, as=0, rc=1
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::POLL_RATE,
            2,
            &seal_field(&[0x40]),
        ));
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::DPI_STAGE_COUNT,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::ACTIVE_DPI_STAGE,
            2,
            &seal_field(&[0]),
        ));
        let s0 = encode_dpi_stage(&DpiStage::new(0, DpiValue::new(1600).unwrap())).unwrap();
        mock.push_input_response(reply(Command::ReadFlash, flash::DPI_STAGES, 4, &s0));
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::LIFT_OFF_DISTANCE,
            2,
            &seal_field(&[3]),
        ));
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::DEBOUNCE_MS,
            2,
            &seal_field(&[2]),
        ));
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::MOTION_SYNC,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::ANGLE_SNAPPING,
            2,
            &seal_field(&[0]),
        ));
        mock.push_input_response(reply(
            Command::ReadFlash,
            flash::RIPPLE_CONTROL,
            2,
            &seal_field(&[1]),
        ));
        // DeviceOnline(false)
        mock.push_input_response(reply(Command::DeviceOnline, 0, 5, &[0, 0, 0, 0, 0]));
        // Active profile = 1
        mock.push_input_response(reply(Command::GetCurrentConfig, 0, 1, &[1]));

        let mut dev = PulsarDevice::open(mock, false, None).unwrap();
        assert_eq!(dev.info().model_name, "Pulsar X2 CrazyLight");
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.18"));

        let batt = dev.read_battery().unwrap();
        assert_eq!(batt.percentage, Some(88));
        assert_eq!(batt.voltage_mv, Some(4010));
        assert_eq!(batt.state, ChargingState::Discharging);

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.polling_rate, PollingRateHz::Hz8000);
        assert_eq!(snap.dpi_stages[0].x.value(), 1600);
        assert_eq!(snap.lod, Some(LiftOffDistance::Low));
        assert_eq!(snap.debounce_ms, Some(DebounceMs::new(2)));
        assert_eq!(snap.motion_sync, Some(true));
        assert_eq!(snap.ripple_control, Some(true));

        let prof = dev.read_profile_state().unwrap();
        assert_eq!(prof.active_slot, ProfileSlot::new(1));
    }

    #[test]
    fn dry_run_suppresses_pulsar_flash_writes() {
        let mut mock = MockHidTransport::new(pulsar_desc());
        mock.push_input_response(reply(
            Command::EncryptionData,
            0,
            8,
            &[0, 0, 0, 0, 0x57, 0x04, 5, 3],
        ));
        mock.push_input_response(reply(Command::ReadVersionId, 0, 2, &[0x01, 0x18]));

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = PulsarDevice::open(mock, true, Some(sink.clone())).unwrap();

        dev.set_polling_rate(PollingRateHz::Hz4000).unwrap();
        dev.set_lod(LiftOffDistance::High).unwrap();
        dev.set_debounce_ms(DebounceMs::new(4)).unwrap();
        dev.set_motion_sync(true).unwrap();
        dev.set_angle_snapping(false).unwrap();
        dev.set_ripple_control(true).unwrap();
        dev.set_active_profile(ProfileSlot::new(2)).unwrap();

        // Only open's 2 read-only queries hit mock transport.
        assert_eq!(dev.transport().inner().recorded_writes().len(), 2);
        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 7);
    }
}
