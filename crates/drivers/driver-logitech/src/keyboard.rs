//! Fn-inversion (`0x40A0` / `0x40A2` / `0x40A3`), Game Mode (`0x4522`) and keyboard
//! polling rate (`0x8060`) support.

use agnoperiph_core::{
    CoreError, CoreResult, KeyboardConfigCapabilities, KeyboardConfigController,
    KeyboardConfigSnapshot, PollingRateHz,
};
use agnoperiph_transport::HidTransport;

use crate::LogitechHidppDevice;
use crate::protocol::{
    GAME_MODE_USAGES, MAX_DISABLE_KEY_USAGES, feature, polling_rate_from_ms, polling_rate_to_ms,
    validate_key_usages,
};

const CURRENT_HOST: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FnInversionVariant {
    K375s,
    New,
    Legacy,
}

impl FnInversionVariant {
    pub(crate) fn detect<T: HidTransport>(dev: &LogitechHidppDevice<T>) -> Option<Self> {
        if dev.cached_feature(feature::K375S_FN_INVERSION).is_some() {
            Some(Self::K375s)
        } else if dev.cached_feature(feature::NEW_FN_INVERSION).is_some() {
            Some(Self::New)
        } else if dev.cached_feature(feature::FN_INVERSION).is_some() {
            Some(Self::Legacy)
        } else {
            None
        }
    }
}

impl<T: HidTransport> LogitechHidppDevice<T> {
    /// Reads the `DisableKeysByUsage` (`0x4522`) usage capacity; `0` is treated as the
    /// 16-usage request limit.
    pub(crate) fn discover_disable_keys_capacity(&mut self) -> CoreResult<Option<usize>> {
        if self
            .cached_feature(feature::DISABLE_KEYS_BY_USAGE)
            .is_none()
        {
            return Ok(None);
        }
        let resp = self.call_feature(feature::DISABLE_KEYS_BY_USAGE, 0, &[], false)?;
        let capacity = match usize::from(resp[0]) {
            0 => MAX_DISABLE_KEY_USAGES,
            n => n.min(MAX_DISABLE_KEY_USAGES),
        };
        Ok(Some(capacity))
    }

    fn disable_keys_capacity(&self) -> CoreResult<usize> {
        self.disable_keys_capacity
            .ok_or(CoreError::UnsupportedCapability("disable_keys_by_usage"))
    }

    pub(crate) fn has_game_mode(&self) -> bool {
        self.disable_keys_capacity
            .is_some_and(|cap| cap >= GAME_MODE_USAGES.len())
    }

    /// Disables the listed 8-bit HID key usages (`0x4522` fn `1`).
    pub fn disable_keys_by_usage(&mut self, usages: &[u8]) -> CoreResult<()> {
        validate_key_usages(usages, self.disable_keys_capacity()?)?;
        self.call_feature(feature::DISABLE_KEYS_BY_USAGE, 1, usages, true)?;
        Ok(())
    }

    /// Re-enables the listed 8-bit HID key usages (`0x4522` fn `2`).
    pub fn enable_keys_by_usage(&mut self, usages: &[u8]) -> CoreResult<()> {
        validate_key_usages(usages, self.disable_keys_capacity()?)?;
        self.call_feature(feature::DISABLE_KEYS_BY_USAGE, 2, usages, true)?;
        Ok(())
    }

    /// Re-enables every key disabled through `0x4522` (fn `3`).
    pub fn enable_all_keys(&mut self) -> CoreResult<()> {
        self.disable_keys_capacity()?;
        self.call_feature(feature::DISABLE_KEYS_BY_USAGE, 3, &[], true)?;
        self.is_game_mode_enabled = false;
        Ok(())
    }

    fn read_fn_inversion(&mut self) -> CoreResult<bool> {
        let state = match self.fn_inversion {
            Some(FnInversionVariant::K375s) => {
                self.call_feature(feature::K375S_FN_INVERSION, 0, &[CURRENT_HOST], false)?[1] & 0x01
            }
            Some(FnInversionVariant::New) => {
                self.call_feature(feature::NEW_FN_INVERSION, 0, &[], false)?[0] & 0x01
            }
            Some(FnInversionVariant::Legacy) => {
                self.call_feature(feature::FN_INVERSION, 0, &[], false)?[0] & 0x0F
            }
            None => return Ok(false),
        };
        Ok(state == 1)
    }

    fn write_fn_inversion(&mut self, enabled: bool) -> CoreResult<()> {
        let state = u8::from(enabled);
        match self.fn_inversion {
            Some(FnInversionVariant::K375s) => {
                self.call_feature(feature::K375S_FN_INVERSION, 1, &[CURRENT_HOST, state], true)?;
            }
            Some(FnInversionVariant::New) => {
                self.call_feature(feature::NEW_FN_INVERSION, 1, &[state], true)?;
            }
            Some(FnInversionVariant::Legacy) => {
                self.call_feature(feature::FN_INVERSION, 1, &[state], true)?;
            }
            None => return Err(CoreError::UnsupportedCapability("fn_inversion")),
        }
        Ok(())
    }

    fn has_report_rate(&self) -> bool {
        self.cached_feature(feature::REPORT_RATE).is_some()
    }
}

fn reject_unsupported_toggle(requested: bool, current: bool, name: &'static str) -> CoreResult<()> {
    if requested != current {
        return Err(CoreError::UnsupportedCapability(name));
    }
    Ok(())
}

impl<T: HidTransport> KeyboardConfigController for LogitechHidppDevice<T> {
    fn keyboard_config_capabilities(&self) -> KeyboardConfigCapabilities {
        let rates = if self.has_report_rate() {
            self.report_rates.clone()
        } else {
            Vec::new()
        };
        KeyboardConfigCapabilities::new(
            false,
            false,
            self.has_game_mode(),
            false,
            false,
            false,
            rates,
        )
        .with_fn_inversion(self.fn_inversion.is_some())
        .with_game_mode(self.has_game_mode())
    }

    fn read_keyboard_config(&mut self) -> CoreResult<KeyboardConfigSnapshot> {
        let fn_inversion = self.read_fn_inversion()?;
        let polling_rate = if self.has_report_rate() {
            let resp = self.call_feature(feature::REPORT_RATE, 1, &[], false)?;
            polling_rate_from_ms(resp[0])?
        } else {
            PollingRateHz::Hz1000
        };
        let game_mode = self.is_game_mode_enabled;
        Ok(
            KeyboardConfigSnapshot::new(false, false, game_mode, false, false, false, polling_rate)
                .with_fn_inversion(fn_inversion)
                .with_game_mode(game_mode),
        )
    }

    fn set_keyboard_config(&mut self, config: &KeyboardConfigSnapshot) -> CoreResult<()> {
        let caps = self.keyboard_config_capabilities();
        let current = self.read_keyboard_config()?;

        reject_unsupported_toggle(config.mac_mode, current.mac_mode, "mac_mode")?;
        reject_unsupported_toggle(config.exchange_wasd, current.exchange_wasd, "wasd_swap")?;
        reject_unsupported_toggle(
            config.six_key_rollover,
            current.six_key_rollover,
            "six_key_rollover",
        )?;
        reject_unsupported_toggle(
            config.anti_accidental_touch,
            current.anti_accidental_touch,
            "anti_accidental_touch",
        )?;
        reject_unsupported_toggle(config.extreme_mode, current.extreme_mode, "extreme_mode")?;

        // Game Mode and Win Lock drive the same 0x4522 usage list; whichever flag changed wins.
        let desired_game_mode = if config.game_mode == current.game_mode {
            config.win_lock
        } else {
            config.game_mode
        };
        let is_game_mode_change = desired_game_mode != current.game_mode;
        if is_game_mode_change && !caps.supports_game_mode {
            return Err(CoreError::UnsupportedCapability("game_mode"));
        }

        let is_fn_change = config.fn_inversion != current.fn_inversion;
        if is_fn_change && !caps.supports_fn_inversion {
            return Err(CoreError::UnsupportedCapability("fn_inversion"));
        }

        let is_rate_change = config.polling_rate != current.polling_rate;
        let rate_ms = if is_rate_change {
            if !caps.supported_polling_rates.contains(&config.polling_rate) {
                return Err(CoreError::OutOfRange {
                    field: "polling_rate",
                    actual: config.polling_rate.to_string(),
                    expected: format!("{:?}", caps.supported_polling_rates),
                });
            }
            Some(polling_rate_to_ms(config.polling_rate)?)
        } else {
            None
        };

        if is_fn_change {
            self.write_fn_inversion(config.fn_inversion)?;
        }
        if is_game_mode_change {
            if desired_game_mode {
                self.disable_keys_by_usage(&GAME_MODE_USAGES)?;
            } else {
                self.enable_keys_by_usage(&GAME_MODE_USAGES)?;
            }
            self.is_game_mode_enabled = desired_game_mode;
        }
        if let Some(ms) = rate_ms {
            self.call_feature(feature::REPORT_RATE, 2, &[ms], true)?;
        }
        Ok(())
    }
}
