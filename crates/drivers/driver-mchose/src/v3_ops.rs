//! MCHOSE V3 session logic: `0x4D` request/reply pairing over input reports and
//! whole-block read-modify-write updates.

use std::time::{Duration, Instant};

use agnoperiph_core::{
    BatteryStatus, ChargingState, CoreError, CoreResult, DebounceMs, DpiStage, LiftOffDistance,
    PointerSnapshot, ProfileInfo, ProfileMode, ProfileSlot, ProfileSnapshot,
};
use agnoperiph_transport::{HidTransport, TransportError};

use crate::protocol_v3::{self as v3, DpiTable, Settings, command};
use crate::{MchoseDevice, Model, Toggle, delay_ms, pause};

/// `0x0901` (and `0x0900` with an unreachable mouse) answer after a fixed ~1.001 s deferral.
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);
const READ_ATTEMPTS: usize = 3;
const BUSY_ATTEMPTS: usize = 2;
const BUSY_RETRY: Duration = delay_ms(30);
const WRITE_SETTLE: Duration = delay_ms(400);
/// Upper bound on unrelated input reports (motion, pushes) skipped while awaiting one reply.
const MAX_FRAMES_PER_ATTEMPT: usize = 64;

enum Outcome {
    Payload(Vec<u8>),
    Busy,
    Rejected,
    Timeout,
}

impl<T: HidTransport> MchoseDevice<T> {
    fn v3_wired(&self) -> bool {
        matches!(self.model, Model::V3 { wired: true, .. })
    }

    fn v3_product(&self) -> Option<&'static v3::Product> {
        match self.model {
            Model::V3 { product, .. } => product,
            _ => None,
        }
    }

    fn v3_await(&mut self, cmd: u16) -> CoreResult<Outcome> {
        let deadline = Instant::now() + REPLY_TIMEOUT;
        for _ in 0..MAX_FRAMES_PER_ATTEMPT {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let mut buf = [0u8; v3::FRAME_LEN];
            let read = match self.transport.read_input_report(&mut buf, remaining) {
                Ok(0) | Err(TransportError::Timeout { .. }) => return Ok(Outcome::Timeout),
                Ok(n) => n,
                Err(err) => return Err(err.into()),
            };
            let Some(body) = v3::body_of(&buf[..read]) else {
                continue;
            };
            if let Some(payload) = v3::payload(body, cmd) {
                return Ok(if v3::is_busy(&payload) {
                    Outcome::Busy
                } else {
                    Outcome::Payload(payload)
                });
            }
            if v3::is_rejection(body) {
                return Ok(Outcome::Rejected);
            }
        }
        Ok(Outcome::Timeout)
    }

    /// Sends a read-only query and waits for the input report carrying the same command id.
    pub(crate) fn v3_query(&mut self, cmd: u16, data: &[u8]) -> CoreResult<Vec<u8>> {
        if v3::is_mutating(cmd) {
            return Err(CoreError::Validation(format!(
                "MCHOSE V3 command 0x{cmd:04X} is a write, not a query"
            )));
        }
        let frame = v3::encode_frame(cmd, data)?;
        let mut busy = 0;
        let mut timeouts = 0;
        while timeouts < READ_ATTEMPTS {
            self.transport.send_query_output_report(&frame)?;
            match self.v3_await(cmd)? {
                Outcome::Payload(payload) => return Ok(payload),
                Outcome::Rejected => {
                    return Err(CoreError::Protocol(format!(
                        "MCHOSE V3 firmware rejected command 0x{cmd:04X}"
                    )));
                }
                Outcome::Busy => {
                    busy += 1;
                    if busy >= BUSY_ATTEMPTS {
                        return Err(CoreError::NotConnected(format!(
                            "MCHOSE V3 receiver reports the mouse unreachable (0x{cmd:04X})"
                        )));
                    }
                    pause(BUSY_RETRY);
                }
                Outcome::Timeout => timeouts += 1,
            }
        }
        Err(CoreError::NotConnected(format!(
            "timed out waiting for MCHOSE V3 reply to 0x{cmd:04X}"
        )))
    }

    /// Sends a whole-block write. Returns `false` in dry-run mode (validated and logged only).
    fn v3_write(&mut self, cmd: u16, data: &[u8]) -> CoreResult<bool> {
        if !v3::is_mutating(cmd) {
            return Err(CoreError::Validation(format!(
                "MCHOSE V3 command 0x{cmd:04X} is not a write"
            )));
        }
        let frame = v3::encode_frame(cmd, data)?;
        self.transport.send_output_report(&frame)?;
        if self.transport.is_dry_run() {
            return Ok(false);
        }
        pause(WRITE_SETTLE);
        Ok(true)
    }

    fn v3_read_settings(&mut self) -> CoreResult<Settings> {
        let payload = self.v3_query(command::READ_SETTINGS, &[])?;
        Settings::decode(&payload)
            .ok_or_else(|| CoreError::Protocol("short MCHOSE V3 settings block".to_owned()))
    }

    fn v3_read_table(&mut self, profile: u8) -> CoreResult<DpiTable> {
        let payload = self.v3_query(command::READ_DPI, &[profile, v3::AXIS_X])?;
        DpiTable::decode(&payload)
            .ok_or_else(|| CoreError::Protocol("short MCHOSE V3 DPI table".to_owned()))
    }

    /// Reads `0x0002`, applies `edit` to the decoded block (unknown bytes untouched), writes
    /// it back with `0x0102`, and confirms with a fresh read.
    fn v3_update_settings(
        &mut self,
        edit: &dyn Fn(&mut Settings) -> CoreResult<()>,
        confirm: &dyn Fn(&Settings) -> bool,
        what: &str,
    ) -> CoreResult<()> {
        let mut settings = self.v3_read_settings()?;
        edit(&mut settings)?;
        if !self.v3_write(command::WRITE_SETTINGS, &settings.encode())? {
            return Ok(());
        }
        if confirm(&self.v3_read_settings()?) {
            Ok(())
        } else {
            Err(Self::not_confirmed(what))
        }
    }

    fn v3_update_table(
        &mut self,
        edit: &dyn Fn(&mut DpiTable) -> CoreResult<()>,
        confirm: &dyn Fn(&DpiTable) -> bool,
        what: &str,
    ) -> CoreResult<()> {
        let profile = self.v3_read_settings()?.profile_index;
        let mut table = self.v3_read_table(profile)?;
        edit(&mut table)?;
        if !self.v3_write(command::WRITE_DPI, &table.encode_write()?)? {
            return Ok(());
        }
        if confirm(&self.v3_read_table(profile)?) {
            Ok(())
        } else {
            Err(Self::not_confirmed(what))
        }
    }

    /// Open-time read-only probe; returns the onboard profile count (0 when unknown).
    pub(crate) fn v3_probe(&mut self, usb_product: Option<&str>, host_pid: u16) -> u8 {
        let info = self
            .v3_query(command::READ_DEVICE_INFO, &[])
            .ok()
            .and_then(|p| v3::decode_device_info(&p));
        if let Model::V3 {
            product: None,
            wired,
        } = self.model
        {
            let product =
                v3::find_product(usb_product, wired.then_some(host_pid), info.map(|i| i.pid));
            if product.is_none() {
                tracing::warn!(
                    "MCHOSE V3 model unresolved; lift-off control disabled and DPI capped at 26000"
                );
            }
            self.model = Model::V3 { product, wired };
        }
        info.map_or(0, |i| {
            if i.profile_count <= 16 {
                i.profile_count
            } else {
                0
            }
        })
    }

    pub(crate) fn v3_refresh(&mut self) -> CoreResult<Option<String>> {
        self.v3_query(command::READ_DEVICE_INFO, &[])?;
        // `0x0901` needs the target byte; some units refuse it outright, which is not fatal.
        let version = self
            .v3_query(command::READ_VERSION, &[v3::VERSION_TARGET_MOUSE])
            .ok()
            .filter(|p| p.len() >= 2)
            .and_then(|p| {
                let raw = format!("{:02x}{:02x}", p[0], p[1]);
                let trimmed = raw.trim_start_matches('0');
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            });
        Ok(version)
    }

    fn v3_read_lod_index(&mut self, settings: &Settings) -> CoreResult<Option<u8>> {
        match self.v3_product() {
            Some(product) if product.lod_command => {
                let payload = self.v3_query(command::READ_LIFT_OFF, &[settings.profile_index])?;
                Ok(payload.first().copied())
            }
            Some(_) => Ok(Some(settings.lod_bits())),
            None => Ok(None),
        }
    }

    pub(crate) fn v3_read_pointer(&mut self) -> CoreResult<PointerSnapshot> {
        let settings = self.v3_read_settings()?;
        let table = self.v3_read_table(settings.profile_index)?;
        let count = usize::from(table.stage_count).clamp(1, v3::DPI_STAGES);
        let stages = Self::stages_from_wire(&table.stages[..count])?;
        let rate = self.rate_from_index(settings.rate_option(self.v3_wired()))?;

        let mut snapshot = PointerSnapshot::new(table.active_stage, stages, rate)
            .with_debounce_ms(DebounceMs::new(settings.left_debounce_ms))
            .with_motion_sync(settings.has_flag(v3::SENSOR_MOTION_SYNC))
            .with_angle_snapping(settings.has_flag(v3::SENSOR_ANGLE_SNAP))
            .with_ripple_control(settings.has_flag(v3::SENSOR_RIPPLE));
        if let Some(index) = self.v3_read_lod_index(&settings)?
            && let Some(product) = self.v3_product()
            && let Some(tenths) = product.lods_tenths_mm.get(usize::from(index))
        {
            snapshot = snapshot.with_lod(LiftOffDistance::TenthsMm(*tenths));
        }
        Ok(snapshot)
    }

    pub(crate) fn v3_set_dpi_stages(&mut self, stages: &[DpiStage], active: u8) -> CoreResult<()> {
        let values = stages
            .iter()
            .map(|s| Self::dpi_to_wire(s.x.value()))
            .collect::<CoreResult<Vec<u16>>>()?;
        let count = u8::try_from(values.len()).unwrap_or(1);
        let edit = |table: &mut DpiTable| -> CoreResult<()> {
            table.stages[..values.len()].copy_from_slice(&values);
            table.stage_count = count;
            table.active_stage = active;
            Ok(())
        };
        let confirm = |table: &DpiTable| {
            table.stages[..values.len()] == values[..]
                && table.stage_count == count
                && table.active_stage == active
        };
        self.v3_update_table(&edit, &confirm, "DPI stages")
    }

    pub(crate) fn v3_set_active_stage(&mut self, stage: u8) -> CoreResult<()> {
        let edit = |table: &mut DpiTable| -> CoreResult<()> {
            if stage >= table.stage_count {
                return Err(CoreError::OutOfRange {
                    field: "active_dpi_stage",
                    actual: stage.to_string(),
                    expected: format!("0..{}", table.stage_count),
                });
            }
            table.active_stage = stage;
            Ok(())
        };
        self.v3_update_table(&edit, &|t| t.active_stage == stage, "active DPI stage")
    }

    pub(crate) fn v3_set_polling(&mut self, option: u8) -> CoreResult<()> {
        let wired = self.v3_wired();
        self.v3_update_settings(
            &|s| s.set_rate_option(wired, option),
            &|s| s.rate_option(wired) == option,
            "polling rate",
        )
    }

    pub(crate) fn v3_set_debounce(&mut self, ms: u8) -> CoreResult<()> {
        // The firmware stores left and right separately; the single control drives both.
        self.v3_update_settings(
            &|s| {
                s.left_debounce_ms = ms;
                s.right_debounce_ms = ms;
                Ok(())
            },
            &|s| s.left_debounce_ms == ms && s.right_debounce_ms == ms,
            "debounce",
        )
    }

    pub(crate) fn v3_set_lod(&mut self, index: u8) -> CoreResult<()> {
        let product = self
            .v3_product()
            .ok_or(CoreError::UnsupportedCapability("lift_off_distance"))?;
        if !product.lod_command {
            return self.v3_update_settings(
                &|s| s.set_lod_bits(index),
                &|s| s.lod_bits() == index,
                "lift-off distance",
            );
        }
        let settings = self.v3_read_settings()?;
        let data = v3::encode_lift_off(settings.profile_index, index);
        if !self.v3_write(command::WRITE_LIFT_OFF, &data)? {
            return Ok(());
        }
        if self.v3_read_lod_index(&settings)? == Some(index) {
            Ok(())
        } else {
            Err(Self::not_confirmed("lift-off distance"))
        }
    }

    pub(crate) fn v3_set_toggle(&mut self, toggle: Toggle, enabled: bool) -> CoreResult<()> {
        let mask = match toggle {
            Toggle::MotionSync => v3::SENSOR_MOTION_SYNC,
            Toggle::AngleSnapping => v3::SENSOR_ANGLE_SNAP,
            Toggle::RippleControl => v3::SENSOR_RIPPLE,
        };
        self.v3_update_settings(
            &|s| {
                s.set_flag(mask, enabled);
                Ok(())
            },
            &|s| s.has_flag(mask) == enabled,
            "sensor toggle",
        )
    }

    pub(crate) fn v3_read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let payload = match self.v3_query(command::READ_DEVICE_INFO, &[]) {
            Ok(payload) => payload,
            Err(CoreError::NotConnected(_)) if !self.v3_wired() => {
                return Ok(BatteryStatus::new(
                    None,
                    None,
                    ChargingState::Unknown,
                    false,
                ));
            }
            Err(err) => return Err(err),
        };
        let info = v3::decode_device_info(&payload)
            .ok_or_else(|| CoreError::Protocol("short MCHOSE V3 device info".to_owned()))?;
        let connected = self.v3_wired() || info.connected;
        if !connected {
            return Ok(BatteryStatus::new(
                None,
                None,
                ChargingState::Unknown,
                false,
            ));
        }
        let state = if info.charging {
            ChargingState::Charging
        } else {
            ChargingState::Discharging
        };
        Ok(BatteryStatus::new(
            Some(info.battery_percent.min(100)),
            None,
            state,
            connected,
        ))
    }

    pub(crate) fn v3_read_profiles(&mut self) -> CoreResult<ProfileSnapshot> {
        let settings = self.v3_read_settings()?;
        let active = ProfileSlot::new(settings.profile_index);
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

    pub(crate) fn v3_set_profile(&mut self, index: u8) -> CoreResult<()> {
        self.v3_update_settings(
            &|s| {
                s.profile_index = index;
                Ok(())
            },
            &|s| s.profile_index == index,
            "profile",
        )
    }
}
