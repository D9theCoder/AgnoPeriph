//! MCHOSE A5 Gen1 session logic: XVI feature request / polled `0xA1` acknowledgement.

use std::time::Duration;

use agnoperiph_core::{
    BatteryStatus, ChargingState, CoreError, CoreResult, DebounceMs, DpiStage, LiftOffDistance,
    PointerSnapshot, PollingRateHz, ProfileInfo, ProfileMode, ProfileSlot, ProfileSnapshot,
};
use agnoperiph_transport::HidTransport;

use crate::protocol_a5_gen1::{self as a5, Command, command};
use crate::{MchoseDevice, Toggle, delay_ms, pause};

const FIRST_REPLY_DELAY: Duration = delay_ms(34);
const RETRY_DELAY: Duration = delay_ms(30);
const REPLY_ATTEMPTS: usize = 8;
/// Upstream re-sends the request once after this many unmatched reads.
const RESEND_AFTER: usize = 3;

impl<T: HidTransport> MchoseDevice<T> {
    fn a5_send(&mut self, frame: &[u8], mutating: bool) -> CoreResult<()> {
        if mutating {
            self.transport.send_feature_report(frame)?;
        } else {
            self.transport.send_query_feature_report(frame)?;
        }
        Ok(())
    }

    /// Sends one request and polls for its `0xA1` acknowledgement. Returns `None` for a
    /// write in dry-run mode (validated and logged, never transmitted).
    fn a5_request(&mut self, cmd: Command, data: &[u8]) -> CoreResult<Option<Vec<u8>>> {
        let frame = a5::encode_request(cmd, data)?;
        let mutating = cmd.is_mutating();
        self.a5_send(&frame, mutating)?;
        if mutating && self.transport.is_dry_run() {
            return Ok(None);
        }
        pause(FIRST_REPLY_DELAY);
        for attempt in 0..REPLY_ATTEMPTS {
            let mut buf = [0u8; a5::FEATURE_LEN];
            buf[0] = a5::REPORT_ID;
            if let Ok(read) = self.transport.get_feature_report(&mut buf) {
                let body = a5::decode_reply(&buf[..read]);
                if a5::reply_matches(body, cmd) {
                    return Ok(Some(body.to_vec()));
                }
            }
            if attempt == RESEND_AFTER {
                self.a5_send(&frame, mutating)?;
            }
            pause(RETRY_DELAY);
        }
        Err(CoreError::NotConnected(format!(
            "MCHOSE A5 did not acknowledge page 0x{:02X} command 0x{:02X}",
            cmd.page, cmd.cmd
        )))
    }

    fn a5_read(&mut self, cmd: Command, data: &[u8]) -> CoreResult<Vec<u8>> {
        self.a5_request(cmd, data)?
            .ok_or_else(|| CoreError::Validation("MCHOSE A5 read classified as write".to_owned()))
    }

    fn a5_byte(body: &[u8], ix: usize) -> u8 {
        body.get(ix).copied().unwrap_or(0)
    }

    /// Active profile, 1-based on the wire.
    fn a5_active_profile(&mut self) -> CoreResult<u8> {
        let body = self.a5_read(command::READ_PROFILE, &[])?;
        Ok(Self::a5_byte(&body, 6).max(1))
    }

    pub(crate) fn a5_read_firmware(&mut self) -> CoreResult<String> {
        let body = self.a5_read(command::READ_FIRMWARE, &[])?;
        Ok(a5::decode_firmware(&body))
    }

    pub(crate) fn a5_probe(&mut self) {
        if let Ok(version) = self.a5_read_firmware() {
            self.info.firmware_version = Some(version);
        }
    }

    fn a5_read_dpi(&mut self, profile: u8) -> CoreResult<(u8, Vec<u16>, u8)> {
        let stages_arg = u8::try_from(a5::DPI_STAGES).unwrap_or(6);
        let table = self.a5_read(command::READ_DPI, &[profile, stages_arg])?;
        let (count, stages) = a5::decode_dpi(&table);
        let active = self.a5_read(command::READ_ACTIVE_STAGE, &[profile])?;
        let active_ix = Self::a5_byte(&active, 7).clamp(1, count) - 1;
        Ok((count, stages, active_ix))
    }

    fn a5_flag(&mut self, cmd: Command) -> Option<bool> {
        self.a5_read(cmd, &[])
            .ok()
            .map(|b| Self::a5_byte(&b, 6) > 0)
    }

    pub(crate) fn a5_read_pointer(&mut self) -> CoreResult<PointerSnapshot> {
        let profile = self.a5_active_profile()?;
        let (_, stages, active) = self.a5_read_dpi(profile)?;
        let polling = self.a5_read(command::READ_POLLING, &[])?;
        let rate = a5::decode_polling(Self::a5_byte(&polling, 6)).ok_or_else(|| {
            CoreError::Protocol(format!(
                "unknown MCHOSE A5 polling code 0x{:02X}",
                Self::a5_byte(&polling, 6)
            ))
        })?;

        let mut snapshot = PointerSnapshot::new(active, Self::stages_from_wire(&stages)?, rate);
        if let Ok(body) = self.a5_read(command::READ_DEBOUNCE, &[profile]) {
            snapshot = snapshot.with_debounce_ms(DebounceMs::new(Self::a5_byte(&body, 7)));
        }
        if let Ok(body) = self.a5_read(command::READ_LOD, &[]) {
            let lod = if Self::a5_byte(&body, 6) == 2 {
                LiftOffDistance::High
            } else {
                LiftOffDistance::Low
            };
            snapshot = snapshot.with_lod(lod);
        }
        if let Some(on) = self.a5_flag(command::READ_MOTION_SYNC) {
            snapshot = snapshot.with_motion_sync(on);
        }
        if let Some(on) = self.a5_flag(command::READ_ANGLE_SNAP) {
            snapshot = snapshot.with_angle_snapping(on);
        }
        if let Some(on) = self.a5_flag(command::READ_RIPPLE) {
            snapshot = snapshot.with_ripple_control(on);
        }
        Ok(snapshot)
    }

    pub(crate) fn a5_set_dpi_stages(&mut self, stages: &[DpiStage], active: u8) -> CoreResult<()> {
        let values = stages
            .iter()
            .map(|s| Self::dpi_to_wire(s.x.value()))
            .collect::<CoreResult<Vec<u16>>>()?;
        let profile = self.a5_active_profile()?;
        let data = a5::encode_dpi(profile, &values)?;
        let live = self
            .a5_request(a5::dpi_write_command(data.len()), &data)?
            .is_some();
        self.a5_request(command::SET_ACTIVE_STAGE, &[profile, active + 1])?;
        if !live {
            return Ok(());
        }
        let (_, confirmed, confirmed_active) = self.a5_read_dpi(profile)?;
        if confirmed == values && confirmed_active == active {
            Ok(())
        } else {
            Err(Self::not_confirmed("DPI stages"))
        }
    }

    pub(crate) fn a5_set_active_stage(&mut self, stage: u8) -> CoreResult<()> {
        let profile = self.a5_active_profile()?;
        let (count, _, _) = self.a5_read_dpi(profile)?;
        if stage >= count {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage.to_string(),
                expected: format!("0..{count}"),
            });
        }
        self.a5_request(command::SET_ACTIVE_STAGE, &[profile, stage + 1])?;
        Ok(())
    }

    pub(crate) fn a5_set_polling(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        let code = a5::encode_polling(rate).ok_or(CoreError::OutOfRange {
            field: "polling_rate",
            actual: rate.to_string(),
            expected: "125, 250, 500, or 1000 Hz".to_owned(),
        })?;
        if self.a5_request(command::SET_POLLING, &[code])?.is_none() {
            return Ok(());
        }
        let confirmed = self.a5_read(command::READ_POLLING, &[])?;
        if Self::a5_byte(&confirmed, 6) == code {
            Ok(())
        } else {
            Err(Self::not_confirmed("polling rate"))
        }
    }

    pub(crate) fn a5_set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        let value = if lod == LiftOffDistance::High { 2 } else { 1 };
        self.a5_request(command::SET_LOD, &[value])?;
        Ok(())
    }

    pub(crate) fn a5_set_debounce(&mut self, ms: u8) -> CoreResult<()> {
        let profile = self.a5_active_profile()?;
        self.a5_request(command::SET_DEBOUNCE, &[profile, ms])?;
        Ok(())
    }

    pub(crate) fn a5_set_toggle(&mut self, toggle: Toggle, enabled: bool) -> CoreResult<()> {
        let cmd = match toggle {
            Toggle::MotionSync => command::SET_MOTION_SYNC,
            Toggle::AngleSnapping => command::SET_ANGLE_SNAP,
            Toggle::RippleControl => command::SET_RIPPLE,
        };
        self.a5_request(cmd, &[u8::from(enabled)])?;
        Ok(())
    }

    pub(crate) fn a5_read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let body = self.a5_read(command::READ_BATTERY, &[])?;
        let state = if Self::a5_byte(&body, 6) != 0 {
            ChargingState::Charging
        } else {
            ChargingState::Discharging
        };
        Ok(BatteryStatus::new(
            Some(Self::a5_byte(&body, 7).min(100)),
            None,
            state,
            true,
        ))
    }

    pub(crate) fn a5_read_profiles(&mut self) -> CoreResult<ProfileSnapshot> {
        let active = ProfileSlot::new(self.a5_active_profile()? - 1);
        self.profile_caps.validate_slot(active)?;
        let profiles = (0..a5::PROFILE_COUNT)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect();
        Ok(ProfileSnapshot::new(
            active,
            ProfileMode::OnboardMemory,
            profiles,
        ))
    }

    pub(crate) fn a5_set_profile(&mut self, index: u8) -> CoreResult<()> {
        if self
            .a5_request(command::SET_PROFILE, &[index + 1])?
            .is_none()
        {
            return Ok(());
        }
        if self.a5_active_profile()? == index + 1 {
            Ok(())
        } else {
            Err(Self::not_confirmed("profile"))
        }
    }
}
