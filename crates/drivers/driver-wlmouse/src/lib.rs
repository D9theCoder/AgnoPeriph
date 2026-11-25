//! WLmouse CompX 64-byte HID Feature report peripheral driver.

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, CoreError, CoreResult, DebounceMs, DeviceId,
    DeviceInfo, DeviceMatchRule, DpiStage, DriverDescriptor, LiftOffDistance, Peripheral,
    PointerCapabilities, PointerDevice, PointerSnapshot, PollingRateHz, ProfileCapabilities,
    ProfileInfo, ProfileManager, ProfileMode, ProfileSlot, ProfileSnapshot, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink};

use crate::protocol::{
    DPI_MAX, DPI_MIN, DPI_STEP, MAX_DEBOUNCE_MS, MAX_DPI_STAGES, PROFILE_COUNT,
    WLMOUSE_HID_BUFFER_LEN, WLMOUSE_PRODUCTS, WLMOUSE_REPORT_ID, WLMOUSE_VID,
    WLmouseProductProfile, WLmouseReply, cmd, decode_battery, decode_dpi_stages, decode_firmware,
    decode_lod, decode_polling_rate, decode_reply, encode_dpi_stages, encode_lod,
    encode_polling_rate, encode_request, lookup_product, page, target, wlmouse_validator,
};

const MAX_EXCHANGE_ATTEMPTS: usize = 6;

/// Declarative `DriverDescriptor` for WLmouse CompX wired & wireless mice.
pub fn driver_descriptor() -> DriverDescriptor {
    let rules = WLMOUSE_PRODUCTS
        .iter()
        .map(|profile| {
            DeviceMatchRule::new(WLMOUSE_VID, profile.connection, profile.model_name)
                .with_pid(profile.pid)
                .with_usage_page(0xFF00)
        })
        .collect();
    DriverDescriptor::new("wlmouse-hid", VendorKind::WLmouse, rules)
}

/// Active WLmouse peripheral backed by a validated [`GuardedTransport`].
pub struct WLmouseDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    profile: WLmouseProductProfile,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
    profile_caps: ProfileCapabilities,
    active_profile_1based: u8,
}

impl<T: HidTransport> fmt::Debug for WLmouseDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WLmouseDevice")
            .field("profile", &self.profile)
            .field("info", &self.info)
            .field("active_profile_1based", &self.active_profile_1based)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> WLmouseDevice<T> {
    /// Connects to a WLmouse wired or wireless interface and reads firmware & active profile.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != WLMOUSE_VID {
            return Err(CoreError::Validation(format!(
                "expected WLmouse VID 0x{WLMOUSE_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        let profile = lookup_product(desc.pid).ok_or_else(|| {
            CoreError::Validation(format!("unsupported WLmouse PID 0x{:04X}", desc.pid))
        })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(wlmouse_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "wlmouse:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let supported_rates: Vec<PollingRateHz> = [
            PollingRateHz::Hz125,
            PollingRateHz::Hz250,
            PollingRateHz::Hz500,
            PollingRateHz::Hz1000,
            PollingRateHz::Hz2000,
            PollingRateHz::Hz4000,
            PollingRateHz::Hz8000,
        ]
        .into_iter()
        .filter(|r| r.as_hz() <= profile.max_polling_hz)
        .collect();

        let pointer_caps = PointerCapabilities::new(DPI_MIN, DPI_MAX, DPI_STEP, MAX_DPI_STAGES)
            .with_xy_independent_dpi(true)
            .with_polling_rates(supported_rates)
            .with_lods(vec![
                LiftOffDistance::TenthsMm(7),
                LiftOffDistance::TenthsMm(10),
                LiftOffDistance::TenthsMm(20),
            ])
            .with_debounce_range_ms(0, MAX_DEBOUNCE_MS)
            .with_motion_sync(true)
            .with_angle_snapping(true)
            .with_ripple_control(true);

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::WLmouse,
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
            profile_caps: ProfileCapabilities::new(PROFILE_COUNT, false, false),
            active_profile_1based: 1,
        };

        if let Ok(fw) = dev.exchange(target::MOUSE, 0x10, page::DEVICE, cmd::READ_FIRMWARE, &[])
            && let Some(ver) = decode_firmware(&fw)
        {
            dev.info.firmware_version = Some(ver);
        }
        if let Ok(prof) = dev.exchange(
            target::MOUSE,
            0x01,
            page::DEVICE,
            cmd::READ_ACTIVE_PROFILE,
            &[],
        ) && let Some(&p) = prof.first()
        {
            dev.active_profile_1based = p.clamp(1, PROFILE_COUNT);
        }

        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn exchange(
        &mut self,
        tgt: u8,
        length: u8,
        pg: u8,
        command: u8,
        args: &[u8],
    ) -> CoreResult<Vec<u8>> {
        let is_read = (command & 0x80) != 0;
        let req = encode_request(tgt, length, pg, command, args)?;

        if !is_read && self.transport.is_dry_run() {
            self.transport.send_feature_report(&req)?;
            return Ok(Vec::new());
        }

        for _ in 0..MAX_EXCHANGE_ATTEMPTS {
            if is_read {
                self.transport.send_query_feature_report(&req)?;
            } else {
                self.transport.send_feature_report(&req)?;
            }

            let mut buf = [0u8; WLMOUSE_HID_BUFFER_LEN];
            buf[0] = WLMOUSE_REPORT_ID;
            let n = self.transport.get_feature_report(&mut buf)?;
            match decode_reply(&buf[..n], pg, command) {
                WLmouseReply::Ok(payload) => return Ok(payload),
                WLmouseReply::Unsupported => {
                    return Err(CoreError::UnsupportedCapability("wlmouse_command"));
                }
                WLmouseReply::Pending | WLmouseReply::NoMatch => {}
            }
        }

        Err(CoreError::NotConnected(format!(
            "WLmouse did not answer page 0x{pg:02X} cmd 0x{command:02X}"
        )))
    }

    fn write_profile_field(&mut self, pg: u8, command: u8, values: &[u8]) -> CoreResult<()> {
        let mut args = Vec::with_capacity(1 + values.len());
        args.push(self.active_profile_1based);
        args.extend_from_slice(values);
        let len = u8::try_from(args.len()).unwrap_or(1);
        self.exchange(target::MOUSE, len, pg, command, &args)?;
        Ok(())
    }
}

impl<T: HidTransport> Peripheral for WLmouseDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let fw = self.exchange(target::MOUSE, 0x10, page::DEVICE, cmd::READ_FIRMWARE, &[])?;
        if let Some(ver) = decode_firmware(&fw) {
            self.info.firmware_version = Some(ver);
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

impl<T: HidTransport> BatteryMonitor for WLmouseDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let payload = self.exchange(target::MOUSE, 0x02, page::DEVICE, cmd::READ_BATTERY, &[])?;
        Ok(decode_battery(&payload))
    }
}

impl<T: HidTransport> PointerDevice for WLmouseDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        let p = self.active_profile_1based;
        let stages_raw = self.exchange(
            target::MOUSE,
            0x0A,
            page::PROFILE,
            cmd::READ_DPI_STAGES,
            &[p, MAX_DPI_STAGES],
        )?;
        let stages = decode_dpi_stages(&stages_raw)?;

        let active_raw = self.exchange(
            target::MOUSE,
            0x02,
            page::PROFILE,
            cmd::READ_ACTIVE_STAGE,
            &[p],
        )?;
        let active_stage = active_raw
            .get(1)
            .copied()
            .unwrap_or(1)
            .saturating_sub(1)
            .min(u8::try_from(stages.len().saturating_sub(1)).unwrap_or(0));

        let poll_raw = self.exchange(
            target::MOUSE,
            0x02,
            page::PROFILE,
            cmd::READ_POLLING_RATE,
            &[p],
        )?;
        let polling_rate = decode_polling_rate(poll_raw.get(1).copied().unwrap_or(0))?;

        let mut snap = PointerSnapshot::new(active_stage, stages, polling_rate);

        if let Ok(lod_raw) = self.exchange(target::MOUSE, 0x02, page::PROFILE, cmd::READ_LOD, &[p])
            && let Some(lod) = decode_lod(lod_raw.get(1).copied().unwrap_or(0))
        {
            snap = snap.with_lod(lod);
        }
        if let Ok(deb_raw) =
            self.exchange(target::MOUSE, 0x02, page::DEVICE, cmd::READ_DEBOUNCE, &[p])
            && let Some(&ms) = deb_raw.get(1)
        {
            snap = snap.with_debounce_ms(DebounceMs::new(ms));
        }
        if let Ok(ms_raw) = self.exchange(
            target::MOUSE,
            0x02,
            page::PROFILE,
            cmd::READ_MOTION_SYNC,
            &[p],
        ) {
            snap = snap.with_motion_sync(ms_raw.get(1).copied().unwrap_or(0) == 1);
        }
        if let Ok(as_raw) = self.exchange(
            target::MOUSE,
            0x02,
            page::PROFILE,
            cmd::READ_ANGLE_SNAPPING,
            &[p],
        ) {
            snap = snap.with_angle_snapping(as_raw.get(1).copied().unwrap_or(0) == 1);
        }
        if let Ok(rc_raw) = self.exchange(
            target::MOUSE,
            0x02,
            page::PROFILE,
            cmd::READ_RIPPLE_CONTROL,
            &[p],
        ) {
            snap = snap.with_ripple_control(rc_raw.get(1).copied().unwrap_or(0) == 1);
        }

        Ok(snap)
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        let args = encode_dpi_stages(self.active_profile_1based, stages)?;
        let len = u8::try_from(args.len()).unwrap_or(2);
        self.exchange(
            target::MOUSE,
            len,
            page::PROFILE,
            cmd::WRITE_DPI_STAGES,
            &args,
        )?;
        self.write_profile_field(
            page::PROFILE,
            cmd::WRITE_ACTIVE_STAGE,
            &[active_ix.saturating_add(1)],
        )
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if stage_ix >= MAX_DPI_STAGES {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{MAX_DPI_STAGES}"),
            });
        }
        self.write_profile_field(
            page::PROFILE,
            cmd::WRITE_ACTIVE_STAGE,
            &[stage_ix.saturating_add(1)],
        )
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        self.write_profile_field(
            page::PROFILE,
            cmd::WRITE_POLLING_RATE,
            &[encode_polling_rate(rate)],
        )
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        self.write_profile_field(page::PROFILE, cmd::WRITE_LOD, &[encode_lod(lod)?])
    }

    fn set_debounce_ms(&mut self, debounce: DebounceMs) -> CoreResult<()> {
        self.pointer_caps.validate_debounce(debounce)?;
        self.write_profile_field(page::DEVICE, cmd::WRITE_DEBOUNCE, &[debounce.value()])
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_profile_field(page::PROFILE, cmd::WRITE_MOTION_SYNC, &[u8::from(enabled)])
    }

    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_profile_field(
            page::PROFILE,
            cmd::WRITE_ANGLE_SNAPPING,
            &[u8::from(enabled)],
        )
    }

    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_profile_field(
            page::PROFILE,
            cmd::WRITE_RIPPLE_CONTROL,
            &[u8::from(enabled)],
        )
    }
}

impl<T: HidTransport> ProfileManager for WLmouseDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        self.profile_caps.clone()
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let reply = self.exchange(
            target::MOUSE,
            0x01,
            page::DEVICE,
            cmd::READ_ACTIVE_PROFILE,
            &[],
        )?;
        let slot_1based = reply.first().copied().unwrap_or(1).clamp(1, PROFILE_COUNT);
        self.active_profile_1based = slot_1based;
        let active = ProfileSlot::new(slot_1based - 1);
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
        let slot_1based = slot.index().saturating_add(1);
        self.exchange(
            target::MOUSE,
            0x01,
            page::DEVICE,
            cmd::WRITE_ACTIVE_PROFILE,
            &[slot_1based],
        )?;
        self.active_profile_1based = slot_1based;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::status;
    use agnoperiph_core::{ChargingState, DpiValue};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    fn make_reply(pg: u8, command: u8, payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; WLMOUSE_HID_BUFFER_LEN];
        buf[0] = WLMOUSE_REPORT_ID;
        buf[1] = status::OK;
        buf[3] = target::MOUSE;
        buf[4] = u8::try_from(payload.len()).unwrap_or(0);
        buf[5] = pg;
        buf[6] = command;
        buf[7..7 + payload.len()].copy_from_slice(payload);
        buf
    }

    fn beast_x_pro_desc() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            WLMOUSE_VID,
            0xA870,
            0xFF00,
            0x0001,
            1,
            "mock://wlmouse/beast-x-pro",
        )
    }

    #[test]
    fn reads_beast_x_pro_state_and_suppresses_dry_run_writes() {
        let mut mock = MockHidTransport::new(beast_x_pro_desc());
        mock.push_feature_response(make_reply(page::DEVICE, cmd::READ_FIRMWARE, &[0, 0, 2, 14]));
        mock.push_feature_response(make_reply(page::DEVICE, cmd::READ_ACTIVE_PROFILE, &[2]));
        mock.push_feature_response(make_reply(page::DEVICE, cmd::READ_BATTERY, &[0, 91]));
        // DPI stages: profile=2, count=2, 800x800 & 1600x1600
        mock.push_feature_response(make_reply(
            page::PROFILE,
            cmd::READ_DPI_STAGES,
            &[2, 2, 0x03, 0x20, 0x03, 0x20, 0x06, 0x40, 0x06, 0x40],
        ));
        mock.push_feature_response(make_reply(page::PROFILE, cmd::READ_ACTIVE_STAGE, &[2, 2]));
        mock.push_feature_response(make_reply(
            page::PROFILE,
            cmd::READ_POLLING_RATE,
            &[2, 0x80],
        ));
        mock.push_feature_response(make_reply(page::PROFILE, cmd::READ_LOD, &[2, 0x87]));
        mock.push_feature_response(make_reply(page::DEVICE, cmd::READ_DEBOUNCE, &[2, 1]));
        mock.push_feature_response(make_reply(page::PROFILE, cmd::READ_MOTION_SYNC, &[2, 1]));
        mock.push_feature_response(make_reply(page::PROFILE, cmd::READ_ANGLE_SNAPPING, &[2, 0]));
        mock.push_feature_response(make_reply(page::PROFILE, cmd::READ_RIPPLE_CONTROL, &[2, 0]));

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = WLmouseDevice::open(mock, true, Some(sink.clone())).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v2.14"));

        let batt = dev.read_battery().unwrap();
        assert_eq!(batt.percentage, Some(91));
        assert_eq!(batt.state, ChargingState::Discharging);

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.active_dpi_stage, 1);
        assert_eq!(snap.dpi_stages.len(), 2);
        assert_eq!(snap.dpi_stages[0].x.value(), 800);
        assert_eq!(snap.dpi_stages[1].x.value(), 1600);
        assert_eq!(snap.polling_rate, PollingRateHz::Hz8000);
        assert_eq!(snap.lod, Some(LiftOffDistance::TenthsMm(7)));
        assert_eq!(snap.debounce_ms, Some(DebounceMs::new(1)));
        assert_eq!(snap.motion_sync, Some(true));

        dev.set_dpi_stages(&[DpiStage::new(0, DpiValue::new(1200).unwrap())], 0)
            .unwrap();
        dev.set_polling_rate(PollingRateHz::Hz4000).unwrap();
        dev.set_lod(LiftOffDistance::TenthsMm(10)).unwrap();
        dev.set_debounce_ms(DebounceMs::new(2)).unwrap();
        dev.set_motion_sync(false).unwrap();
        dev.set_active_profile(ProfileSlot::new(0)).unwrap();

        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 7);
    }
}
