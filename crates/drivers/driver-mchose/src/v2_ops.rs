//! MCHOSE V2 session logic: polled feature-report queries and read-modify-write updates.

use std::time::Duration;

use agnoperiph_core::{
    BatteryStatus, ChargingState, CoreError, CoreResult, DebounceMs, DpiStage, LiftOffDistance,
    PointerSnapshot, ProfileInfo, ProfileMode, ProfileSlot, ProfileSnapshot,
};
use agnoperiph_transport::HidTransport;

use crate::protocol_v2::{
    self as v2, Config, ConfigChanges, LinkByte, PerformanceChanges, command,
};
use crate::{MchoseDevice, Model, Toggle, delay_ms, pause};

/// A reply can arrive before the firmware has filled it in, so reads are polled until two
/// consecutive identical payloads arrive.
const POLL_DELAY: Duration = delay_ms(90);
const POLL_ATTEMPTS: usize = 20;
const WRITE_SETTLE: Duration = delay_ms(400);
const PROFILE_SETTLE: Duration = delay_ms(600);
/// The performance command (`0x42`) is the slowest to apply and is re-sent if a read-back
/// still shows the previous state.
const PERFORMANCE_SETTLE: Duration = delay_ms(2000);
const PERFORMANCE_ATTEMPTS: usize = 3;

/// Routes a link byte edit to the half (wired or wireless) the host is talking over.
fn link_change(wired: bool, link: LinkByte, mut changes: ConfigChanges) -> ConfigChanges {
    if wired {
        changes.wired = Some(link);
    } else {
        changes.wireless = Some(link);
    }
    changes
}

fn non_zero(payload: &[u8], len: usize) -> bool {
    payload.iter().take(len).any(|b| *b != 0)
}

impl<T: HidTransport> MchoseDevice<T> {
    fn v2_link_is_wired(&self) -> bool {
        matches!(self.model, Model::V2 { wired: true, .. })
    }

    fn v2_product(&self) -> Option<&'static v2::Product> {
        match self.model {
            Model::V2 { product, .. } => product,
            _ => None,
        }
    }

    /// Sends a read-only command and polls `GET_FEATURE` until a stable matching reply.
    pub(crate) fn v2_query(
        &mut self,
        tokens: &[u8],
        accept: &dyn Fn(&[u8]) -> bool,
    ) -> CoreResult<Vec<u8>> {
        let cmd = tokens[0];
        if v2::is_mutating(cmd) {
            return Err(CoreError::Validation(format!(
                "MCHOSE V2 command 0x{cmd:02X} is a write, not a query"
            )));
        }
        let frame = v2::encode_command(tokens)?;
        let report_id = frame[0];
        let mut previous: Option<Vec<u8>> = None;
        let mut last_error: Option<String> = None;

        for _ in 0..POLL_ATTEMPTS {
            self.transport.send_query_feature_report(&frame)?;
            pause(POLL_DELAY);
            let mut buf = [0u8; v2::FEATURE_LEN];
            buf[0] = report_id;
            let read = match self.transport.get_feature_report(&mut buf) {
                Ok(n) => n,
                Err(err) => {
                    last_error = Some(err.to_string());
                    continue;
                }
            };
            let Some(reply) = v2::decode_reply(&buf[..read]) else {
                continue;
            };
            if reply.command != cmd || !accept(&reply.payload) {
                continue;
            }
            if previous.as_deref() == Some(reply.payload.as_slice()) {
                return Ok(reply.payload);
            }
            previous = Some(reply.payload);
        }

        Err(CoreError::NotConnected(format!(
            "no stable MCHOSE V2 reply to command 0x{cmd:02X}{}",
            last_error.map_or_default(|e| format!(" (last error: {e})"))
        )))
    }

    /// Sends a mutating command. Returns `false` in dry-run mode (validated and logged only).
    fn v2_write(&mut self, tokens: &[u8], settle: Duration) -> CoreResult<bool> {
        if !v2::is_mutating(tokens[0]) {
            return Err(CoreError::Validation(format!(
                "MCHOSE V2 command 0x{:02X} is not a write",
                tokens[0]
            )));
        }
        let frame = v2::encode_command(tokens)?;
        self.transport.send_feature_report(&frame)?;
        if self.transport.is_dry_run() {
            return Ok(false);
        }
        pause(settle);
        Ok(true)
    }

    fn v2_read_config(&mut self) -> CoreResult<Config> {
        let payload = self.v2_query(&[command::CONFIG], &v2::looks_like_config)?;
        Config::decode(&payload).ok_or_else(|| {
            CoreError::Protocol("MCHOSE V2 returned an implausible 0x67 config".to_owned())
        })
    }

    /// Read-modify-write of the full `0x67` blob through `0x57`.
    fn v2_update_config(
        &mut self,
        build: &dyn Fn(&Config) -> CoreResult<ConfigChanges>,
        confirm: &dyn Fn(&Config) -> bool,
        what: &str,
    ) -> CoreResult<()> {
        let before = self.v2_read_config()?;
        let changes = build(&before)?;
        let tokens = v2::encode_config_write(&before, &changes)?;
        if !self.v2_write(&tokens, WRITE_SETTLE)? {
            return Ok(());
        }
        let after = self.v2_read_config()?;
        if confirm(&after) {
            Ok(())
        } else {
            Err(Self::not_confirmed(what))
        }
    }

    fn v2_write_performance(
        &mut self,
        tokens: &[u8],
        confirm: &dyn Fn(&Config) -> bool,
        what: &str,
    ) -> CoreResult<()> {
        for _ in 0..PERFORMANCE_ATTEMPTS {
            if !self.v2_write(tokens, PERFORMANCE_SETTLE)? {
                return Ok(());
            }
            if confirm(&self.v2_read_config()?) {
                return Ok(());
            }
        }
        Err(Self::not_confirmed(what))
    }

    pub(crate) fn v2_read_firmware(&mut self) -> CoreResult<Option<String>> {
        let payload = self.v2_query(&[command::VERSION], &|p| non_zero(p, 2))?;
        Ok(v2::decode_version(&payload))
    }

    /// Open-time read-only probe: firmware and (for receivers) the mouse's own model PID.
    pub(crate) fn v2_probe(&mut self, usb_product: Option<&str>) {
        if let Ok(Some(version)) = self.v2_read_firmware() {
            self.info.firmware_version = Some(version);
        }
        if let Model::V2 {
            product: None,
            wired,
            bluetooth,
        } = self.model
        {
            let mouse_pid = self
                .v2_query(&[command::BATTERY], &|p| non_zero(p, v2::BATTERY_MIN_LEN))
                .ok()
                .and_then(|p| v2::decode_battery(&p))
                .map(|b| b.mouse_pid);
            let product = v2::find_product(mouse_pid, usb_product);
            if product.is_none() {
                tracing::warn!(
                    "MCHOSE V2 model unresolved; lift-off control disabled and DPI capped at 26000"
                );
            }
            self.model = Model::V2 {
                product,
                wired,
                bluetooth,
            };
        }
    }

    pub(crate) fn v2_read_pointer(&mut self) -> CoreResult<PointerSnapshot> {
        let config = self.v2_read_config()?;
        let link = config.link(self.v2_link_is_wired());
        let count = usize::from(config.stage_count()).clamp(1, v2::DPI_STAGES);
        let stages = Self::stages_from_wire(&config.dpi_stages()[..count])?;
        let rate = self.rate_from_index(link.rate_index)?;

        let mut snapshot = PointerSnapshot::new(link.dpi_index, stages, rate)
            .with_debounce_ms(DebounceMs::new(config.debounce_ms()))
            .with_motion_sync(config.motion_sync())
            .with_angle_snapping(config.angle_snapping())
            .with_ripple_control(config.ripple_control());
        if let Some(product) = self.v2_product()
            && let Some(pos) = config.lod_index().checked_sub(product.lod_index_base)
            && let Some(tenths) = product.lods_tenths_mm.get(usize::from(pos))
        {
            snapshot = snapshot.with_lod(LiftOffDistance::TenthsMm(*tenths));
        }
        Ok(snapshot)
    }

    pub(crate) fn v2_set_dpi_stages(&mut self, stages: &[DpiStage], active: u8) -> CoreResult<()> {
        let wired = self.v2_link_is_wired();
        let values = stages
            .iter()
            .map(|s| Self::dpi_to_wire(s.x.value()))
            .collect::<CoreResult<Vec<u16>>>()?;
        let count = u8::try_from(values.len()).unwrap_or(1);
        let build = |config: &Config| -> CoreResult<ConfigChanges> {
            let mut table = config.dpi_stages();
            table[..values.len()].copy_from_slice(&values);
            let link = LinkByte {
                rate_index: config.link(wired).rate_index,
                dpi_index: active,
            };
            Ok(link_change(
                wired,
                link,
                ConfigChanges {
                    dpi_stages: Some(table),
                    stage_count: Some(count),
                    ..ConfigChanges::default()
                },
            ))
        };
        let confirm = |config: &Config| {
            config.dpi_stages()[..values.len()] == values[..]
                && config.stage_count() == count
                && config.link(wired).dpi_index == active
        };
        self.v2_update_config(&build, &confirm, "DPI stages")
    }

    pub(crate) fn v2_set_active_stage(&mut self, stage: u8) -> CoreResult<()> {
        let wired = self.v2_link_is_wired();
        let build = |config: &Config| -> CoreResult<ConfigChanges> {
            if stage >= config.stage_count() {
                return Err(CoreError::OutOfRange {
                    field: "active_dpi_stage",
                    actual: stage.to_string(),
                    expected: format!("0..{}", config.stage_count()),
                });
            }
            let link = LinkByte {
                rate_index: config.link(wired).rate_index,
                dpi_index: stage,
            };
            Ok(link_change(wired, link, ConfigChanges::default()))
        };
        let confirm = |config: &Config| config.link(wired).dpi_index == stage;
        self.v2_update_config(&build, &confirm, "active DPI stage")
    }

    pub(crate) fn v2_set_polling(&mut self, rate_index: u8) -> CoreResult<()> {
        let wired = self.v2_link_is_wired();
        let build = |config: &Config| -> CoreResult<ConfigChanges> {
            let link = LinkByte {
                rate_index,
                dpi_index: config.link(wired).dpi_index,
            };
            Ok(link_change(wired, link, ConfigChanges::default()))
        };
        let confirm = |config: &Config| config.link(wired).rate_index == rate_index;
        self.v2_update_config(&build, &confirm, "polling rate")
    }

    pub(crate) fn v2_set_debounce(&mut self, ms: u8) -> CoreResult<()> {
        let build = |_: &Config| -> CoreResult<ConfigChanges> {
            Ok(ConfigChanges {
                debounce_ms: Some(ms),
                ..ConfigChanges::default()
            })
        };
        let confirm = |config: &Config| config.debounce_ms() == ms;
        self.v2_update_config(&build, &confirm, "debounce")
    }

    pub(crate) fn v2_set_lod(&mut self, position: u8) -> CoreResult<()> {
        let product = self
            .v2_product()
            .ok_or(CoreError::UnsupportedCapability("lift_off_distance"))?;
        let index = position + product.lod_index_base;
        let tokens = v2::encode_performance(index, PerformanceChanges::default())?;
        self.v2_write_performance(&tokens, &|c| c.lod_index() == index, "lift-off distance")
    }

    pub(crate) fn v2_set_toggle(&mut self, toggle: Toggle, enabled: bool) -> CoreResult<()> {
        let current_lod = self.v2_read_config()?.lod_index();
        let changes = match toggle {
            Toggle::MotionSync => PerformanceChanges {
                motion_sync: Some(enabled),
                ..PerformanceChanges::default()
            },
            Toggle::AngleSnapping => PerformanceChanges {
                angle_snapping: Some(enabled),
                ..PerformanceChanges::default()
            },
            Toggle::RippleControl => PerformanceChanges {
                ripple_control: Some(enabled),
                ..PerformanceChanges::default()
            },
        };
        let tokens = v2::encode_performance(current_lod, changes)?;
        let confirm = move |c: &Config| match toggle {
            Toggle::MotionSync => c.motion_sync() == enabled,
            Toggle::AngleSnapping => c.angle_snapping() == enabled,
            Toggle::RippleControl => c.ripple_control() == enabled,
        };
        self.v2_write_performance(&tokens, &confirm, "sensor toggle")
    }

    pub(crate) fn v2_read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let wired = self.v2_link_is_wired();
        let payload =
            match self.v2_query(&[command::BATTERY], &|p| non_zero(p, v2::BATTERY_MIN_LEN)) {
                Ok(payload) => payload,
                Err(CoreError::NotConnected(_)) if !wired => {
                    return Ok(BatteryStatus::new(
                        None,
                        None,
                        ChargingState::Unknown,
                        false,
                    ));
                }
                Err(err) => return Err(err),
            };
        let battery = v2::decode_battery(&payload)
            .ok_or_else(|| CoreError::Protocol("short MCHOSE V2 battery reply".to_owned()))?;
        let connected = wired || battery.connected;
        if !connected {
            return Ok(BatteryStatus::new(
                None,
                None,
                ChargingState::Unknown,
                false,
            ));
        }
        let state = if battery.charging {
            ChargingState::Charging
        } else {
            ChargingState::Discharging
        };
        Ok(BatteryStatus::new(
            Some(battery.percent.min(100)),
            None,
            state,
            connected,
        ))
    }

    pub(crate) fn v2_read_profiles(&mut self) -> CoreResult<ProfileSnapshot> {
        let config = self.v2_read_config()?;
        let active = ProfileSlot::new(config.profile_index());
        self.profile_caps.validate_slot(active)?;

        let mut labels: Vec<Option<String>> = Vec::new();
        for ix in 0..v2::PROFILE_COUNT {
            let tokens = v2::encode_read_profile_name(ix)?;
            let accept =
                move |p: &[u8]| p.first() == Some(&ix) && p.get(1).is_some_and(|b| *b != 0);
            match self.v2_query(&tokens, &accept) {
                Ok(payload) => labels.push(v2::decode_profile_name(&payload)),
                // A profile whose name will not come back is left unnamed rather than guessed.
                Err(_) => break,
            }
        }

        let profiles = (0..v2::PROFILE_COUNT)
            .map(|ix| {
                let info = ProfileInfo::new(ProfileSlot::new(ix), true);
                match labels.get(usize::from(ix)).cloned().flatten() {
                    Some(label) => info.with_label(label),
                    None => info,
                }
            })
            .collect();
        Ok(ProfileSnapshot::new(
            active,
            ProfileMode::OnboardMemory,
            profiles,
        ))
    }

    pub(crate) fn v2_set_profile(&mut self, index: u8) -> CoreResult<()> {
        let tokens = v2::encode_set_profile(index)?;
        if !self.v2_write(&tokens, PROFILE_SETTLE)? {
            return Ok(());
        }
        if self.v2_read_config()?.profile_index() == index {
            Ok(())
        } else {
            Err(Self::not_confirmed("profile"))
        }
    }
}
