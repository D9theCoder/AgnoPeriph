//! Finalmouse UltralightX & Starlight X peripheral driver.

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, CoreError, CoreResult, DeviceId, DeviceInfo,
    DeviceMatchRule, DpiStage, DpiValue, DriverDescriptor, LiftOffDistance, Peripheral,
    PointerCapabilities, PointerDevice, PointerSnapshot, PollingRateHz, ProfileCapabilities,
    ProfileInfo, ProfileManager, ProfileMode, ProfileSlot, ProfileSnapshot, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::protocol::{
    DPI_MAX, DPI_MIN, DPI_STEP, FINALMOUSE_PACKET_LEN, FINALMOUSE_PRODUCTS,
    FINALMOUSE_ULX_USAGE_ID, FINALMOUSE_USAGE_PAGE, FINALMOUSE_VID, FinalmouseFamily,
    FinalmouseProductProfile, FinalmouseTelemetry, cmd, encode_paw_lod, encode_report,
    finalmouse_validator, lookup_product,
};

const READ_TIMEOUT: Duration = Duration::from_millis(120);
const MAX_BURST_READS: usize = 24;
const SLX_MAX_PROFILES: u8 = 5;

/// Declarative `DriverDescriptor` for Finalmouse UltralightX & Starlight X dongles and cables.
pub fn driver_descriptor() -> DriverDescriptor {
    let rules = FINALMOUSE_PRODUCTS
        .iter()
        .map(|profile| {
            let mut rule =
                DeviceMatchRule::new(FINALMOUSE_VID, profile.connection, profile.model_name)
                    .with_pid(profile.pid)
                    .with_usage_page(FINALMOUSE_USAGE_PAGE);
            if profile.family == FinalmouseFamily::UltralightX {
                rule = rule.with_usage_id(FINALMOUSE_ULX_USAGE_ID);
            }
            rule
        })
        .collect();
    DriverDescriptor::new("finalmouse-hid", VendorKind::Finalmouse, rules)
}

/// Active Finalmouse UltralightX or Starlight X peripheral backed by [`GuardedTransport`].
pub struct FinalmouseDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    profile: FinalmouseProductProfile,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
    telemetry: FinalmouseTelemetry,
}

impl<T: HidTransport> fmt::Debug for FinalmouseDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FinalmouseDevice")
            .field("profile", &self.profile)
            .field("info", &self.info)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> FinalmouseDevice<T> {
    /// Connects to a Finalmouse ULX or Starlight X interface and reads initial telemetry.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != FINALMOUSE_VID {
            return Err(CoreError::Validation(format!(
                "expected Finalmouse VID 0x{FINALMOUSE_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        let profile = lookup_product(desc.pid).ok_or_else(|| {
            CoreError::Validation(format!("unsupported Finalmouse PID 0x{:04X}", desc.pid))
        })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(finalmouse_validator(profile.family)))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "finalmouse:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let pointer_caps = match profile.family {
            FinalmouseFamily::UltralightX => {
                PointerCapabilities::new(DPI_MIN, DPI_MAX, DPI_STEP, 1)
                    .with_polling_rates(vec![
                        PollingRateHz::Hz500,
                        PollingRateHz::Hz1000,
                        PollingRateHz::Hz2000,
                        PollingRateHz::Hz4000,
                        PollingRateHz::Hz8000,
                    ])
                    .with_lods(vec![
                        LiftOffDistance::TenthsMm(10),
                        LiftOffDistance::TenthsMm(20),
                    ])
                    .with_motion_sync(true)
            }
            FinalmouseFamily::StarlightX => PointerCapabilities::new(DPI_MIN, DPI_MAX, DPI_STEP, 1)
                .with_lods(vec![
                    LiftOffDistance::TenthsMm(7),
                    LiftOffDistance::TenthsMm(10),
                    LiftOffDistance::TenthsMm(20),
                ]),
        };

        let capabilities = Capabilities::empty()
            .with_pointer(true)
            .with_battery(true)
            .with_profiles(profile.family == FinalmouseFamily::StarlightX);

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Finalmouse,
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
            telemetry: FinalmouseTelemetry::default(),
        };

        let _ = dev.poll_telemetry();
        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn send_query(&mut self, report_id: u8, command: u8) -> CoreResult<()> {
        let pkt = encode_report(report_id, command, &[])?;
        self.transport.send_query_output_report(&pkt)?;
        Ok(())
    }

    fn send_write(&mut self, command: u8, payload: &[u8]) -> CoreResult<()> {
        let pkt = encode_report(self.profile.tx_report_id(), command, payload)?;
        self.transport.send_output_report(&pkt)?;
        Ok(())
    }

    fn poll_telemetry(&mut self) -> CoreResult<()> {
        let _ = self.transport.drain_input_reports();
        let tx = self.profile.tx_report_id();
        match self.profile.family {
            FinalmouseFamily::UltralightX => {
                self.send_query(tx, cmd::LIFT_OFF_DISTANCE)?;
            }
            FinalmouseFamily::StarlightX => {
                self.send_query(tx, cmd::PROFILE)?;
                self.send_query(tx, cmd::PAW_LOD)?;
            }
        }
        self.send_query(tx, cmd::WAKE_ALL)?;
        self.send_query(self.profile.dongle_tx_report_id(), cmd::DONGLE_INFO)?;

        let mut buf = [0u8; FINALMOUSE_PACKET_LEN];
        let mut received = 0usize;
        for _ in 0..MAX_BURST_READS {
            match self.transport.read_input_report(&mut buf, READ_TIMEOUT) {
                Ok(n) if n > 0 => {
                    if self.telemetry.apply_report(&buf[..n]) {
                        received += 1;
                    }
                }
                Ok(_) | Err(TransportError::Timeout { .. }) => break,
                Err(err) => return Err(err.into()),
            }
        }

        if let Some(fw) = &self.telemetry.mouse_firmware {
            self.info.firmware_version = Some(fw.clone());
        } else if let Some(fw) = &self.telemetry.dongle_usb_firmware {
            self.info.firmware_version = Some(format!("Dongle {fw}"));
        }
        if let Some(serial) = &self.telemetry.mouse_serial {
            self.info.serial_number = Some(serial.clone());
        }

        if received == 0 && self.telemetry.dpi.is_none() {
            return Err(CoreError::NotConnected(
                "Finalmouse did not reply to status telemetry queries".to_owned(),
            ));
        }
        Ok(())
    }
}

impl<T: HidTransport> Peripheral for FinalmouseDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        self.poll_telemetry()
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
        self.capabilities.has_profiles().then_some(self)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        self.capabilities.has_profiles().then_some(self)
    }
}

impl<T: HidTransport> BatteryMonitor for FinalmouseDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let _ = self.poll_telemetry();
        Ok(self.telemetry.battery_status())
    }
}

impl<T: HidTransport> PointerDevice for FinalmouseDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        if self.telemetry.dpi.is_none() {
            self.poll_telemetry()?;
        }
        let dpi = self.telemetry.dpi.ok_or_else(|| {
            CoreError::Protocol("Finalmouse telemetry did not include DPI".to_owned())
        })?;
        let rate = self.telemetry.polling_rate.unwrap_or(PollingRateHz::Hz1000);
        let stage = DpiStage::new(0, DpiValue::new(dpi)?);
        let mut snap = PointerSnapshot::new(0, vec![stage], rate);
        if let Some(lod) = self.telemetry.effective_lod() {
            snap = snap.with_lod(lod);
        }
        if let Some(ms) = self.telemetry.motion_sync {
            snap = snap.with_motion_sync(ms);
        }
        Ok(snap)
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        let dpi = u16::try_from(stages[usize::from(active_ix)].x.value())
            .map_err(|_| CoreError::Validation("Finalmouse DPI exceeds u16".to_owned()))?;
        self.send_write(cmd::DPI, &dpi.to_le_bytes())?;
        self.telemetry.dpi = Some(u32::from(dpi));
        Ok(())
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if stage_ix != 0 {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: "0..1".to_owned(),
            });
        }
        Ok(())
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        self.send_write(cmd::POLLING_RATE, &rate.as_hz().to_le_bytes())?;
        self.telemetry.polling_rate = Some(rate);
        Ok(())
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        match self.profile.family {
            FinalmouseFamily::UltralightX => {
                let mm = match lod {
                    LiftOffDistance::TenthsMm(10) => 1u8,
                    LiftOffDistance::TenthsMm(20) => 2u8,
                    other => {
                        return Err(CoreError::OutOfRange {
                            field: "lod",
                            actual: other.to_string(),
                            expected: "1.0 mm or 2.0 mm".to_owned(),
                        });
                    }
                };
                self.send_write(cmd::LIFT_OFF_DISTANCE, &[mm])?;
                self.telemetry.lift_off_mm = Some(mm);
            }
            FinalmouseFamily::StarlightX => {
                let raw = encode_paw_lod(lod)?;
                self.send_write(cmd::PAW_LOD, &[raw])?;
                self.telemetry.paw_lod_tenths_mm = protocol::decode_paw_lod_tenths(raw);
            }
        }
        Ok(())
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        if !self.pointer_caps.supports_motion_sync {
            return Err(CoreError::UnsupportedCapability("motion_sync"));
        }
        self.send_write(cmd::MOTION_SYNC, &[u8::from(enabled)])?;
        self.telemetry.motion_sync = Some(enabled);
        Ok(())
    }
}

impl<T: HidTransport> ProfileManager for FinalmouseDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        let count = self.telemetry.profile_count.unwrap_or(SLX_MAX_PROFILES);
        ProfileCapabilities::new(count.clamp(1, SLX_MAX_PROFILES), false, false)
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let caps = self.profile_capabilities();
        let active = ProfileSlot::new(self.telemetry.profile_active.unwrap_or(0));
        let profiles = (0..caps.slot_count)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect();
        Ok(ProfileSnapshot::new(
            active,
            ProfileMode::OnboardMemory,
            profiles,
        ))
    }

    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()> {
        let caps = self.profile_capabilities();
        caps.validate_slot(slot)?;
        self.send_write(cmd::PROFILE, &[slot.index()])?;
        self.telemetry.profile_active = Some(slot.index());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{battery_percent_from_mv, report_id};
    use agnoperiph_core::ChargingState;
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    fn in_report(rid: u8, command: u8, payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; FINALMOUSE_PACKET_LEN];
        buf[0] = rid;
        buf[1] = u8::try_from(2 + payload.len()).unwrap_or(2);
        buf[2] = command;
        buf[3] = u8::try_from(payload.len()).unwrap_or(0);
        buf[4..4 + payload.len()].copy_from_slice(payload);
        buf
    }

    fn ulx_desc() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            FINALMOUSE_VID,
            0x0100,
            FINALMOUSE_USAGE_PAGE,
            FINALMOUSE_ULX_USAGE_ID,
            0,
            "mock://finalmouse/ulx",
        )
    }

    #[test]
    fn framing_and_battery_curve_match_spec() {
        let pkt = encode_report(report_id::ULX_MAIN_OUT, cmd::DPI, &800u16.to_le_bytes()).unwrap();
        assert_eq!(&pkt[..6], &[0x04, 0x04, 0x90, 0x02, 0x20, 0x03]);
        assert_eq!(battery_percent_from_mv(3880), 50);
        assert_eq!(battery_percent_from_mv(4380), 100);
        assert_eq!(battery_percent_from_mv(3000), 0);
    }

    #[test]
    fn reads_ulx_telemetry_burst_and_suppresses_dry_run_writes() {
        let mut mock = MockHidTransport::new(ulx_desc());
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 3, &1600u16.to_le_bytes()));
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 4, &4000u16.to_le_bytes()));
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 5, &4170u16.to_le_bytes()));
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 12, b"1.2.6\0"));
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 14, b"ULX-0042\0"));
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 18, &[1]));
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 21, &[1]));
        mock.push_input_response(in_report(report_id::ULX_MAIN_IN, 37, &[0]));

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = FinalmouseDevice::open(mock, true, Some(sink.clone())).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("1.2.6"));
        assert_eq!(dev.info().serial_number.as_deref(), Some("ULX-0042"));

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.dpi_stages[0].x.value(), 1600);
        assert_eq!(snap.polling_rate, PollingRateHz::Hz4000);
        assert_eq!(snap.lod, Some(LiftOffDistance::TenthsMm(10)));
        assert_eq!(snap.motion_sync, Some(true));

        let batt = dev.read_battery().unwrap();
        assert_eq!(batt.percentage, Some(75));
        assert_eq!(batt.voltage_mv, Some(4170));
        assert_eq!(batt.state, ChargingState::Discharging);

        // Mutating writes under dry-run pass validation and are suppressed.
        dev.set_dpi_stages(&[DpiStage::new(0, DpiValue::new(800).unwrap())], 0)
            .unwrap();
        dev.set_polling_rate(PollingRateHz::Hz8000).unwrap();
        dev.set_lod(LiftOffDistance::TenthsMm(20)).unwrap();
        dev.set_motion_sync(false).unwrap();

        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 4);
    }
}
