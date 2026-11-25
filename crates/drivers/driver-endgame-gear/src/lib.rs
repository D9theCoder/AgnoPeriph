//! Endgame Gear OP1/XM2 8K & 4K Wireless peripheral driver.

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;

use agnoperiph_core::{
    Capabilities, CoreError, CoreResult, DeviceId, DeviceInfo, DeviceMatchRule, DpiStage,
    DriverDescriptor, LiftOffDistance, Peripheral, PointerCapabilities, PointerDevice,
    PointerSnapshot, PollingRateHz, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink};

use crate::protocol::{
    EGG_COMMAND_SIZE, EGG_CONFIG_SIZE, EGG_PRODUCTS, EGG_VID, EggConfigFamily, EggProductProfile,
    MAX_CPI_STAGES, decode_cpi_stages, decode_firmware, decode_lod, decode_polling_rate,
    egg_validator, encode_command, encode_lod, encode_polling_divider, encode_sensor_block_4k,
    lookup_product, offset, op, report_id, status, write_cpi_stages,
};

const MAX_POLL_ATTEMPTS: usize = 6;

/// Declarative `DriverDescriptor` for Endgame Gear OP1 / XM2 8K & 4K wired/dongle PIDs.
pub fn driver_descriptor() -> DriverDescriptor {
    let rules = EGG_PRODUCTS
        .iter()
        .map(|profile| {
            DeviceMatchRule::new(EGG_VID, profile.connection, profile.model_name)
                .with_pid(profile.pid)
                .with_usage_page(0xFF00)
        })
        .collect();
    DriverDescriptor::new("endgame-gear-hid", VendorKind::EndgameGear, rules)
}

/// Active Endgame Gear OP1 / XM2 mouse backed by [`GuardedTransport`].
pub struct EndgameGearDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    profile: EggProductProfile,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
}

impl<T: HidTransport> fmt::Debug for EndgameGearDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndgameGearDevice")
            .field("profile", &self.profile)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> EndgameGearDevice<T> {
    /// Connects to an Endgame Gear OP1/XM2 8K or 4K interface and reads firmware info.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != EGG_VID {
            return Err(CoreError::Validation(format!(
                "expected Endgame Gear VID 0x{EGG_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        let profile = lookup_product(desc.pid).ok_or_else(|| {
            CoreError::Validation(format!("unsupported Endgame Gear PID 0x{:04X}", desc.pid))
        })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(egg_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "endgame-gear:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let supported_rates = if profile.wireless_4k {
            match profile.config_family {
                EggConfigFamily::V1 => vec![
                    PollingRateHz::Hz1000,
                    PollingRateHz::Hz2000,
                    PollingRateHz::Hz4000,
                ],
                EggConfigFamily::V2 => vec![
                    PollingRateHz::Hz125,
                    PollingRateHz::Hz1000,
                    PollingRateHz::Hz2000,
                    PollingRateHz::Hz4000,
                ],
            }
        } else {
            vec![
                PollingRateHz::Hz125,
                PollingRateHz::Hz250,
                PollingRateHz::Hz500,
                PollingRateHz::Hz1000,
                PollingRateHz::Hz2000,
                PollingRateHz::Hz4000,
                PollingRateHz::Hz8000,
            ]
        };

        let lods = if profile.wireless_4k && profile.config_family == EggConfigFamily::V1 {
            vec![LiftOffDistance::TenthsMm(10), LiftOffDistance::TenthsMm(20)]
        } else if profile.config_family == EggConfigFamily::V1 {
            vec![
                LiftOffDistance::TenthsMm(7),
                LiftOffDistance::TenthsMm(10),
                LiftOffDistance::TenthsMm(20),
            ]
        } else {
            (7..=17).map(LiftOffDistance::TenthsMm).collect()
        };

        let pointer_caps = PointerCapabilities::new(
            profile.cpi_min,
            profile.cpi_max,
            profile.cpi_step,
            MAX_CPI_STAGES,
        )
        .with_xy_independent_dpi(true)
        .with_polling_rates(supported_rates)
        .with_lods(lods)
        .with_motion_sync(true)
        .with_angle_snapping(true)
        .with_ripple_control(true);

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::EndgameGear,
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
            capabilities: Capabilities::empty().with_pointer(true),
            pointer_caps,
        };

        if let Ok(fw) = dev.read_firmware() {
            dev.info.firmware_version = Some(fw);
        }
        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn read_firmware(&mut self) -> CoreResult<String> {
        let req = encode_command(op::FIRMWARE, &[], &[]);
        self.transport.send_query_feature_report(&req)?;
        for _ in 0..MAX_POLL_ATTEMPTS {
            let mut buf = [0u8; EGG_COMMAND_SIZE];
            buf[0] = report_id::COMMAND;
            let n = self.transport.get_feature_report(&mut buf)?;
            if buf[1] == status::OK
                && let Some(ver) = decode_firmware(&buf[..n])
            {
                return Ok(ver);
            }
            if buf[1] != status::BUSY {
                break;
            }
        }
        Err(CoreError::NotConnected(
            "Endgame Gear did not return firmware version".to_owned(),
        ))
    }

    fn read_config(&mut self) -> CoreResult<[u8; EGG_CONFIG_SIZE]> {
        let req = encode_command(op::LOAD, &[], &[]);
        self.transport.send_query_feature_report(&req)?;
        for _ in 0..MAX_POLL_ATTEMPTS {
            let mut buf = [0u8; EGG_CONFIG_SIZE];
            buf[0] = report_id::CONFIG;
            let n = self.transport.get_feature_report(&mut buf)?;
            if n >= 131 && buf[1] == status::OK {
                return Ok(buf);
            }
            if buf[1] != status::BUSY {
                break;
            }
        }
        Err(CoreError::NotConnected(
            "Endgame Gear did not return a valid configuration blob".to_owned(),
        ))
    }

    fn write_config(&mut self, mut config: [u8; EGG_CONFIG_SIZE]) -> CoreResult<()> {
        if self.profile.wireless_4k {
            let block = encode_sensor_block_4k(&config);
            self.transport.send_feature_report(&block)?;
        } else {
            config[0] = report_id::CONFIG;
            config[1] = op::STORE;
            self.transport.send_feature_report(&config)?;
        }
        if self.transport.is_dry_run() {
            return Ok(());
        }
        for _ in 0..MAX_POLL_ATTEMPTS {
            let mut ack = [0u8; EGG_COMMAND_SIZE];
            ack[0] = report_id::COMMAND;
            let _ = self.transport.get_feature_report(&mut ack)?;
            if ack[1] == status::OK {
                return Ok(());
            }
            if ack[1] != status::BUSY {
                return Err(CoreError::Protocol(format!(
                    "Endgame Gear rejected config write (status 0x{:02X})",
                    ack[1]
                )));
            }
        }
        Err(CoreError::NotConnected(
            "Endgame Gear did not acknowledge config write".to_owned(),
        ))
    }

    fn update_config<F>(&mut self, mutate: F) -> CoreResult<()>
    where
        F: FnOnce(&mut [u8; EGG_CONFIG_SIZE]) -> CoreResult<()>,
    {
        let mut config = self.read_config()?;
        mutate(&mut config)?;
        self.write_config(config)
    }
}

impl<T: HidTransport> Peripheral for EndgameGearDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let fw = self.read_firmware()?;
        self.info.firmware_version = Some(fw);
        Ok(())
    }

    fn as_pointer(&self) -> Option<&dyn PointerDevice> {
        Some(self)
    }

    fn as_pointer_mut(&mut self) -> Option<&mut dyn PointerDevice> {
        Some(self)
    }
}

impl<T: HidTransport> PointerDevice for EndgameGearDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        let config = self.read_config()?;
        let stages = decode_cpi_stages(&config)?;
        let polling_rate =
            decode_polling_rate(config[offset::POLLING_DIVIDER], self.profile.wireless_4k)?;

        let mut snap = PointerSnapshot::new(0, stages, polling_rate)
            .with_motion_sync(config[offset::MOTION_SYNC] != 0)
            .with_angle_snapping(config[offset::ANGLE_SNAPPING] != 0)
            .with_ripple_control(config[offset::RIPPLE_CONTROL] != 0);

        if let Some(lod) = decode_lod(config[offset::LOD], self.profile) {
            snap = snap.with_lod(lod);
        }
        Ok(snap)
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        self.update_config(|config| write_cpi_stages(config, stages))
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if stage_ix >= MAX_CPI_STAGES {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{MAX_CPI_STAGES}"),
            });
        }
        // Endgame Gear wired/4K firmware selects active CPI stage via the bottom hardware button.
        Ok(())
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        let divider = encode_polling_divider(rate);
        let disable_ms = rate == PollingRateHz::Hz8000 && !self.profile.motion_sync_at_8k;
        self.update_config(|config| {
            config[offset::POLLING_DIVIDER] = divider;
            if disable_ms {
                config[offset::MOTION_SYNC] = 0;
            }
            Ok(())
        })
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        let raw = encode_lod(lod, self.profile)?;
        self.update_config(|config| {
            config[offset::LOD] = raw;
            Ok(())
        })
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        let motion_sync_at_8k = self.profile.motion_sync_at_8k;
        let wireless_4k = self.profile.wireless_4k;
        self.update_config(|config| {
            if enabled
                && !motion_sync_at_8k
                && decode_polling_rate(config[offset::POLLING_DIVIDER], wireless_4k)?
                    == PollingRateHz::Hz8000
            {
                return Err(CoreError::Validation(
                    "Motion Sync cannot be enabled at 8000 Hz on this Endgame Gear model"
                        .to_owned(),
                ));
            }
            config[offset::MOTION_SYNC] = u8::from(enabled);
            Ok(())
        })
    }

    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        self.update_config(|config| {
            config[offset::ANGLE_SNAPPING] = u8::from(enabled);
            Ok(())
        })
    }

    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        self.update_config(|config| {
            config[offset::RIPPLE_CONTROL] = u8::from(enabled);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::DpiValue;
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    fn make_fw_reply(major_bcd: u8, patch_bcd: u8) -> Vec<u8> {
        let mut buf = vec![0u8; EGG_COMMAND_SIZE];
        buf[0] = report_id::COMMAND;
        buf[1] = status::OK;
        buf[17] = patch_bcd;
        buf[18] = major_bcd;
        buf
    }

    fn make_config_blob() -> Vec<u8> {
        let mut cfg = [0u8; EGG_CONFIG_SIZE];
        cfg[0] = report_id::CONFIG;
        cfg[1] = status::OK;
        cfg[offset::POLLING_DIVIDER] = 1; // 8000 Hz
        cfg[offset::LOD] = 1; // 1.0 mm on v1
        cfg[offset::ANGLE_SNAPPING] = 0;
        cfg[offset::RIPPLE_CONTROL] = 1;
        cfg[offset::MOTION_SYNC] = 0;
        let stages = [
            DpiStage::new(0, DpiValue::new(800).unwrap()),
            DpiStage::new(1, DpiValue::new(1600).unwrap()).with_y(DpiValue::new(1800).unwrap()),
        ];
        write_cpi_stages(&mut cfg, &stages).unwrap();
        cfg.to_vec()
    }

    fn op1_8k_desc() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            EGG_VID,
            0x1964,
            0xFF00,
            0x0001,
            1,
            "mock://endgame-gear/op1-8k",
        )
    }

    #[test]
    fn reads_op1_8k_config_and_suppresses_dry_run_writes() {
        let mut mock = MockHidTransport::new(op1_8k_desc());
        mock.push_feature_response(make_fw_reply(0x01, 0x37)); // V1.37
        mock.push_feature_response(make_config_blob()); // read_pointer_state
        mock.push_feature_response(make_config_blob()); // set_polling_rate read
        mock.push_feature_response(make_config_blob()); // set_lod read

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = EndgameGearDevice::open(mock, true, Some(sink.clone())).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("V1.37"));

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.polling_rate, PollingRateHz::Hz8000);
        assert_eq!(snap.dpi_stages.len(), 2);
        assert_eq!(snap.dpi_stages[0].x.value(), 800);
        assert_eq!(snap.dpi_stages[1].effective_y().value(), 1800);
        assert_eq!(snap.lod, Some(LiftOffDistance::TenthsMm(10)));
        assert_eq!(snap.ripple_control, Some(true));

        dev.set_polling_rate(PollingRateHz::Hz4000).unwrap();
        dev.set_lod(LiftOffDistance::TenthsMm(7)).unwrap();

        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 2);
    }
}
