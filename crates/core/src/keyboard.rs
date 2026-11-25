//! Brand-agnostic keyboard remapping, onboard macro, and advanced keyboard configuration traits.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::hall_effect::KeyIndex;
use crate::pointer::PollingRateHz;

/// Two-byte QMK/VIA-compatible key action (`key_class`, `key_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeyCode {
    pub class: u8,
    pub id: u8,
}

impl KeyCode {
    pub const TRANSPARENT: Self = Self::new(0x00, 0x01);

    pub const fn new(class: u8, id: u8) -> Self {
        Self { class, id }
    }

    pub const fn standard(hid_usage: u8) -> Self {
        Self {
            class: 0x00,
            id: hid_usage,
        }
    }

    pub const fn macro_slot(slot_index: u8) -> Self {
        Self {
            class: 0x77,
            id: slot_index,
        }
    }

    pub const fn as_tuple(self) -> (u8, u8) {
        (self.class, self.id)
    }
}

impl fmt::Display for KeyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:02X}:0x{:02X}", self.class, self.id)
    }
}

/// Capabilities of a keyboard's dynamic keymap layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeymapCapabilities {
    pub layer_names: Vec<String>,
    pub rows: u8,
    pub cols: u8,
    pub supports_reset: bool,
}

impl KeymapCapabilities {
    pub fn new(layer_names: Vec<String>, rows: u8, cols: u8, supports_reset: bool) -> Self {
        Self {
            layer_names,
            rows,
            cols,
            supports_reset,
        }
    }

    pub fn validate_layer(&self, layer: u8) -> CoreResult<()> {
        if usize::from(layer) >= self.layer_names.len() {
            return Err(CoreError::OutOfRange {
                field: "keymap_layer",
                actual: layer.to_string(),
                expected: format!("0..{}", self.layer_names.len()),
            });
        }
        Ok(())
    }
}

/// Read-only snapshot of all keymap layers (`layers[layer][key_index]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeymapSnapshot {
    pub layers: Vec<Vec<KeyCode>>,
    pub active_layer: u8,
}

impl KeymapSnapshot {
    pub fn new(layers: Vec<Vec<KeyCode>>, active_layer: u8) -> Self {
        Self {
            layers,
            active_layer,
        }
    }
}

/// Capability trait for keyboards supporting dynamic key remapping across layers.
pub trait KeymapController: Send + Sync {
    fn keymap_capabilities(&self) -> KeymapCapabilities;
    fn read_keymap(&mut self) -> CoreResult<KeymapSnapshot>;
    fn set_key_binding(&mut self, layer: u8, key: KeyIndex, code: KeyCode) -> CoreResult<()>;
    fn reset_keymap(&mut self) -> CoreResult<()> {
        Err(CoreError::UnsupportedCapability("keymap_reset"))
    }
}

/// Kind of event inside an onboard macro step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MacroActionKind {
    KeyDown,
    KeyUp,
    KeyTap,
}

/// Single step in an onboard keyboard macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MacroStep {
    pub kind: MacroActionKind,
    pub hid_keycode: u8,
    pub delay_ms: u16,
}

impl MacroStep {
    pub const fn new(kind: MacroActionKind, hid_keycode: u8, delay_ms: u16) -> Self {
        Self {
            kind,
            hid_keycode,
            delay_ms,
        }
    }
}

/// One onboard macro slot (`M1..M16`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MacroSlot {
    pub index: u8,
    pub name: String,
    pub steps: Vec<MacroStep>,
}

impl MacroSlot {
    pub fn new(index: u8, name: impl Into<String>, steps: Vec<MacroStep>) -> Self {
        Self {
            index,
            name: name.into(),
            steps,
        }
    }
}

/// Hardware limits of onboard macro storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MacroCapabilities {
    pub max_slots: u8,
    pub max_buffer_bytes: u16,
}

impl MacroCapabilities {
    pub const fn new(max_slots: u8, max_buffer_bytes: u16) -> Self {
        Self {
            max_slots,
            max_buffer_bytes,
        }
    }
}

/// Capability trait for peripherals with onboard macro storage.
pub trait MacroController: Send + Sync {
    fn macro_capabilities(&self) -> MacroCapabilities;
    fn read_macros(&mut self) -> CoreResult<Vec<MacroSlot>>;
    fn write_macros(&mut self, macros: &[MacroSlot]) -> CoreResult<()>;
}

/// Supported advanced hardware configuration switches on a keyboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeyboardConfigCapabilities {
    pub supports_mac_mode: bool,
    pub supports_wasd_swap: bool,
    pub supports_win_lock: bool,
    pub supports_six_key_rollover: bool,
    pub supports_anti_accidental_touch: bool,
    pub supports_extreme_mode: bool,
    #[serde(default)]
    pub supports_fn_inversion: bool,
    #[serde(default)]
    pub supports_game_mode: bool,
    pub supported_polling_rates: Vec<PollingRateHz>,
}

impl KeyboardConfigCapabilities {
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub fn new(
        supports_mac_mode: bool,
        supports_wasd_swap: bool,
        supports_win_lock: bool,
        supports_six_key_rollover: bool,
        supports_anti_accidental_touch: bool,
        supports_extreme_mode: bool,
        supported_polling_rates: Vec<PollingRateHz>,
    ) -> Self {
        Self {
            supports_mac_mode,
            supports_wasd_swap,
            supports_win_lock,
            supports_six_key_rollover,
            supports_anti_accidental_touch,
            supports_extreme_mode,
            supports_fn_inversion: false,
            supports_game_mode: false,
            supported_polling_rates,
        }
    }

    pub const fn with_fn_inversion(mut self, enabled: bool) -> Self {
        self.supports_fn_inversion = enabled;
        self
    }

    pub const fn with_game_mode(mut self, enabled: bool) -> Self {
        self.supports_game_mode = enabled;
        self
    }
}

/// Active advanced hardware configuration flags on a keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeyboardConfigSnapshot {
    pub mac_mode: bool,
    pub exchange_wasd: bool,
    pub win_lock: bool,
    pub six_key_rollover: bool,
    pub anti_accidental_touch: bool,
    pub extreme_mode: bool,
    #[serde(default)]
    pub fn_inversion: bool,
    #[serde(default)]
    pub game_mode: bool,
    pub polling_rate: PollingRateHz,
}

impl KeyboardConfigSnapshot {
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub const fn new(
        mac_mode: bool,
        exchange_wasd: bool,
        win_lock: bool,
        six_key_rollover: bool,
        anti_accidental_touch: bool,
        extreme_mode: bool,
        polling_rate: PollingRateHz,
    ) -> Self {
        Self {
            mac_mode,
            exchange_wasd,
            win_lock,
            six_key_rollover,
            anti_accidental_touch,
            extreme_mode,
            fn_inversion: false,
            game_mode: false,
            polling_rate,
        }
    }

    pub const fn with_fn_inversion(mut self, enabled: bool) -> Self {
        self.fn_inversion = enabled;
        self
    }

    pub const fn with_game_mode(mut self, enabled: bool) -> Self {
        self.game_mode = enabled;
        self
    }
}

/// Capability trait for keyboard system toggles and USB polling rate.
pub trait KeyboardConfigController: Send + Sync {
    fn keyboard_config_capabilities(&self) -> KeyboardConfigCapabilities;
    fn read_keyboard_config(&mut self) -> CoreResult<KeyboardConfigSnapshot>;
    fn set_keyboard_config(&mut self, config: &KeyboardConfigSnapshot) -> CoreResult<()>;
}
