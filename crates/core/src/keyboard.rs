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

/// Standard mouse button addressable in onboard mouse macros.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub const ALL: [Self; 5] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Back,
        Self::Forward,
    ];

    /// Standard USB HID mouse button bitmask (`0x01`..=`0x10`).
    pub const fn bitmask(self) -> u8 {
        match self {
            Self::Left => 0x01,
            Self::Right => 0x02,
            Self::Middle => 0x04,
            Self::Back => 0x08,
            Self::Forward => 0x10,
        }
    }

    /// Decodes a single-button HID bitmask into a [`MouseButton`].
    pub const fn from_bitmask(mask: u8) -> Option<Self> {
        match mask {
            0x01 => Some(Self::Left),
            0x02 => Some(Self::Right),
            0x04 => Some(Self::Middle),
            0x08 => Some(Self::Back),
            0x10 => Some(Self::Forward),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left Click",
            Self::Right => "Right Click",
            Self::Middle => "Middle Click",
            Self::Back => "Back",
            Self::Forward => "Forward",
        }
    }
}

impl fmt::Display for MouseButton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Kind of event inside an onboard macro step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MacroActionKind {
    KeyDown,
    KeyUp,
    KeyTap,
    MouseDown,
    MouseUp,
    MouseClick,
    MediaDown,
    MediaUp,
    MediaTap,
    PointerMove,
}

impl MacroActionKind {
    pub const fn is_keyboard(self) -> bool {
        matches!(self, Self::KeyDown | Self::KeyUp | Self::KeyTap)
    }

    pub const fn is_mouse_button(self) -> bool {
        matches!(self, Self::MouseDown | Self::MouseUp | Self::MouseClick)
    }

    pub const fn is_media(self) -> bool {
        matches!(self, Self::MediaDown | Self::MediaUp | Self::MediaTap)
    }

    pub const fn is_pointer_move(self) -> bool {
        matches!(self, Self::PointerMove)
    }
}

/// Single step in an onboard peripheral macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MacroStep {
    pub kind: MacroActionKind,
    pub hid_keycode: u8,
    /// Target button for `MouseDown` / `MouseUp` / `MouseClick` steps.
    #[serde(default)]
    pub mouse_button: Option<MouseButton>,
    /// USB HID Consumer Page (`0x0C`) usage for `MediaDown` / `MediaUp` / `MediaTap` steps.
    #[serde(default)]
    pub consumer_usage: u16,
    /// Relative horizontal delta (`-128..=127`) for `PointerMove` steps.
    #[serde(default)]
    pub move_x: i8,
    /// Relative vertical delta (`-128..=127`) for `PointerMove` steps.
    #[serde(default)]
    pub move_y: i8,
    pub delay_ms: u16,
}

impl MacroStep {
    pub const fn new(kind: MacroActionKind, hid_keycode: u8, delay_ms: u16) -> Self {
        Self {
            kind,
            hid_keycode,
            mouse_button: None,
            consumer_usage: 0,
            move_x: 0,
            move_y: 0,
            delay_ms,
        }
    }

    /// Creates a mouse-button down, up, or click step.
    pub const fn mouse(kind: MacroActionKind, button: MouseButton, delay_ms: u16) -> Self {
        Self {
            kind,
            hid_keycode: 0,
            mouse_button: Some(button),
            consumer_usage: 0,
            move_x: 0,
            move_y: 0,
            delay_ms,
        }
    }

    /// Creates a USB HID Consumer Page (`0x0C`) media key step.
    pub const fn media(kind: MacroActionKind, consumer_usage: u16, delay_ms: u16) -> Self {
        Self {
            kind,
            hid_keycode: 0,
            mouse_button: None,
            consumer_usage,
            move_x: 0,
            move_y: 0,
            delay_ms,
        }
    }

    /// Creates a relative pointer movement step (`MoveXY`).
    pub const fn pointer_move(dx: i8, dy: i8, delay_ms: u16) -> Self {
        Self {
            kind: MacroActionKind::PointerMove,
            hid_keycode: 0,
            mouse_button: None,
            consumer_usage: 0,
            move_x: dx,
            move_y: dy,
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
    /// How many times the macro replays when triggered; `1` plays it once.
    #[serde(default = "default_macro_repeat")]
    pub repeat: u8,
    /// The device stores events this driver cannot re-encode (mouse buttons, media keys),
    /// so the slot is listed by name with no steps and must be saved back unchanged.
    #[serde(default)]
    pub is_readonly: bool,
}

const fn default_macro_repeat() -> u8 {
    1
}

impl MacroSlot {
    pub fn new(index: u8, name: impl Into<String>, steps: Vec<MacroStep>) -> Self {
        Self {
            index,
            name: name.into(),
            steps,
            repeat: 1,
            is_readonly: false,
        }
    }

    pub const fn with_repeat(mut self, repeat: u8) -> Self {
        self.repeat = repeat;
        self
    }

    pub const fn with_readonly(mut self, is_readonly: bool) -> Self {
        self.is_readonly = is_readonly;
        self
    }
}

/// Hardware limits of onboard macro storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MacroCapabilities {
    pub max_slots: u8,
    pub max_buffer_bytes: u16,
    /// Whether each slot stores a replay count (`MacroSlot::repeat`) on the device.
    #[serde(default)]
    pub supports_repeat: bool,
    /// Whether macro steps can press, release, or click mouse buttons.
    #[serde(default)]
    pub supports_mouse_buttons: bool,
    /// Whether macro steps can emit USB HID consumer/media usages.
    #[serde(default)]
    pub supports_media_keys: bool,
    /// Whether macro steps can emit relative pointer movement (`MoveXY`).
    #[serde(default)]
    pub supports_pointer_move: bool,
}

impl MacroCapabilities {
    pub const fn new(max_slots: u8, max_buffer_bytes: u16) -> Self {
        Self {
            max_slots,
            max_buffer_bytes,
            supports_repeat: false,
            supports_mouse_buttons: false,
            supports_media_keys: false,
            supports_pointer_move: false,
        }
    }

    pub const fn with_repeat_support(mut self, supports_repeat: bool) -> Self {
        self.supports_repeat = supports_repeat;
        self
    }

    pub const fn with_mouse_buttons(mut self, supports_mouse_buttons: bool) -> Self {
        self.supports_mouse_buttons = supports_mouse_buttons;
        self
    }

    pub const fn with_media_keys(mut self, supports_media_keys: bool) -> Self {
        self.supports_media_keys = supports_media_keys;
        self
    }

    pub const fn with_pointer_move(mut self, supports_pointer_move: bool) -> Self {
        self.supports_pointer_move = supports_pointer_move;
        self
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
