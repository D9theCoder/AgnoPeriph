//! Standard USB HID keyboard-page usages offered by the keybind, macro and paired-key pickers.
//!
//! Only class `0x00` (standard HID usage) and class `0x77` (macro slot) codes are sent;
//! both are documented in `docs/protocols/atk-rs7.md`. Vendor classes whose ids are not
//! documented are never offered.

use agnoperiph_core::KeyCode;

/// Picker group for a key, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyGroup {
    Letters,
    Numbers,
    Function,
    Editing,
    Navigation,
    Symbols,
    Modifiers,
    Numpad,
    Media,
}

impl KeyGroup {
    pub const ALL: [Self; 9] = [
        Self::Letters,
        Self::Numbers,
        Self::Function,
        Self::Editing,
        Self::Navigation,
        Self::Symbols,
        Self::Modifiers,
        Self::Numpad,
        Self::Media,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Letters => "Letters",
            Self::Numbers => "Numbers",
            Self::Function => "Function keys",
            Self::Editing => "Editing",
            Self::Navigation => "Navigation",
            Self::Symbols => "Symbols",
            Self::Modifiers => "Modifiers",
            Self::Numpad => "Numpad",
            Self::Media => "Volume",
        }
    }
}

/// One selectable HID keyboard-page usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HidKey {
    pub usage: u8,
    pub name: &'static str,
    pub group: KeyGroup,
}

const fn k(usage: u8, name: &'static str, group: KeyGroup) -> HidKey {
    HidKey { usage, name, group }
}

use KeyGroup::{
    Editing, Function, Letters, Media, Modifiers, Navigation, Numbers, Numpad, Symbols,
};

/// Every usage offered by the pickers, grouped and ordered for display.
pub const HID_KEYS: &[HidKey] = &[
    k(0x04, "A", Letters),
    k(0x05, "B", Letters),
    k(0x06, "C", Letters),
    k(0x07, "D", Letters),
    k(0x08, "E", Letters),
    k(0x09, "F", Letters),
    k(0x0A, "G", Letters),
    k(0x0B, "H", Letters),
    k(0x0C, "I", Letters),
    k(0x0D, "J", Letters),
    k(0x0E, "K", Letters),
    k(0x0F, "L", Letters),
    k(0x10, "M", Letters),
    k(0x11, "N", Letters),
    k(0x12, "O", Letters),
    k(0x13, "P", Letters),
    k(0x14, "Q", Letters),
    k(0x15, "R", Letters),
    k(0x16, "S", Letters),
    k(0x17, "T", Letters),
    k(0x18, "U", Letters),
    k(0x19, "V", Letters),
    k(0x1A, "W", Letters),
    k(0x1B, "X", Letters),
    k(0x1C, "Y", Letters),
    k(0x1D, "Z", Letters),
    k(0x1E, "1", Numbers),
    k(0x1F, "2", Numbers),
    k(0x20, "3", Numbers),
    k(0x21, "4", Numbers),
    k(0x22, "5", Numbers),
    k(0x23, "6", Numbers),
    k(0x24, "7", Numbers),
    k(0x25, "8", Numbers),
    k(0x26, "9", Numbers),
    k(0x27, "0", Numbers),
    k(0x3A, "F1", Function),
    k(0x3B, "F2", Function),
    k(0x3C, "F3", Function),
    k(0x3D, "F4", Function),
    k(0x3E, "F5", Function),
    k(0x3F, "F6", Function),
    k(0x40, "F7", Function),
    k(0x41, "F8", Function),
    k(0x42, "F9", Function),
    k(0x43, "F10", Function),
    k(0x44, "F11", Function),
    k(0x45, "F12", Function),
    k(0x68, "F13", Function),
    k(0x69, "F14", Function),
    k(0x6A, "F15", Function),
    k(0x6B, "F16", Function),
    k(0x6C, "F17", Function),
    k(0x6D, "F18", Function),
    k(0x6E, "F19", Function),
    k(0x6F, "F20", Function),
    k(0x70, "F21", Function),
    k(0x71, "F22", Function),
    k(0x72, "F23", Function),
    k(0x73, "F24", Function),
    k(0x29, "Esc", Editing),
    k(0x28, "Enter", Editing),
    k(0x2A, "Backspace", Editing),
    k(0x2B, "Tab", Editing),
    k(0x2C, "Space", Editing),
    k(0x39, "Caps Lock", Editing),
    k(0x46, "Print Screen", Editing),
    k(0x47, "Scroll Lock", Editing),
    k(0x48, "Pause", Editing),
    k(0x65, "Menu", Editing),
    k(0x49, "Insert", Navigation),
    k(0x4C, "Delete", Navigation),
    k(0x4A, "Home", Navigation),
    k(0x4D, "End", Navigation),
    k(0x4B, "Page Up", Navigation),
    k(0x4E, "Page Down", Navigation),
    k(0x52, "Up", Navigation),
    k(0x51, "Down", Navigation),
    k(0x50, "Left", Navigation),
    k(0x4F, "Right", Navigation),
    k(0x35, "`", Symbols),
    k(0x2D, "-", Symbols),
    k(0x2E, "=", Symbols),
    k(0x2F, "[", Symbols),
    k(0x30, "]", Symbols),
    k(0x31, "\\", Symbols),
    k(0x33, ";", Symbols),
    k(0x34, "'", Symbols),
    k(0x36, ",", Symbols),
    k(0x37, ".", Symbols),
    k(0x38, "/", Symbols),
    k(0xE0, "Left Ctrl", Modifiers),
    k(0xE1, "Left Shift", Modifiers),
    k(0xE2, "Left Alt", Modifiers),
    k(0xE3, "Left Win", Modifiers),
    k(0xE4, "Right Ctrl", Modifiers),
    k(0xE5, "Right Shift", Modifiers),
    k(0xE6, "Right Alt", Modifiers),
    k(0xE7, "Right Win", Modifiers),
    k(0x53, "Num Lock", Numpad),
    k(0x54, "Num /", Numpad),
    k(0x55, "Num *", Numpad),
    k(0x56, "Num -", Numpad),
    k(0x57, "Num +", Numpad),
    k(0x58, "Num Enter", Numpad),
    k(0x59, "Num 1", Numpad),
    k(0x5A, "Num 2", Numpad),
    k(0x5B, "Num 3", Numpad),
    k(0x5C, "Num 4", Numpad),
    k(0x5D, "Num 5", Numpad),
    k(0x5E, "Num 6", Numpad),
    k(0x5F, "Num 7", Numpad),
    k(0x60, "Num 8", Numpad),
    k(0x61, "Num 9", Numpad),
    k(0x62, "Num 0", Numpad),
    k(0x63, "Num .", Numpad),
    k(0x7F, "Mute", Media),
    k(0x80, "Volume Up", Media),
    k(0x81, "Volume Down", Media),
];

/// Looks up the picker entry for a standard HID usage.
pub fn hid_key(usage: u8) -> Option<&'static HidKey> {
    HID_KEYS.iter().find(|key| key.usage == usage)
}

/// Short display name of a standard HID usage, or its hex value.
pub fn hid_name(usage: u16) -> String {
    u8::try_from(usage)
        .ok()
        .and_then(hid_key)
        .map_or_else(|| format!("0x{usage:02X}"), |key| key.name.to_owned())
}

/// Macro slot key class (`0x77`, ids `0..=15` = M1..M16).
pub const MACRO_CLASS: u8 = 0x77;

/// Human-readable name of any keymap action the keybind view may show.
pub fn keycode_name(code: KeyCode) -> String {
    match code.class {
        0x00 if code == KeyCode::TRANSPARENT => "Default".to_owned(),
        0x00 if code.id == 0 => "None".to_owned(),
        0x00 => hid_name(u16::from(code.id)),
        MACRO_CLASS => format!("M{}", u16::from(code.id) + 1),
        0x57 if code.id < 20 => format!("MT {}", code.id + 1),
        0x57 => format!("TGL {}", code.id - 19),
        0x7E if (5..25).contains(&code.id) => format!("DKS {}", code.id - 4),
        0x52 => format!("Fn {}", code.id),
        _ => code.to_string(),
    }
}

/// Case-insensitive search over key names.
pub fn matches_search(name: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn usages_are_unique_and_named() {
        let mut seen = HashSet::new();
        for key in HID_KEYS {
            assert!(seen.insert(key.usage), "duplicate usage {:02X}", key.usage);
            assert_ne!(key.name, "");
        }
        assert_eq!(hid_name(0x1A), "W");
        assert_eq!(keycode_name(KeyCode::macro_slot(3)), "M4");
        assert_eq!(keycode_name(KeyCode::new(0x7E, 5)), "DKS 1");
        assert_eq!(keycode_name(KeyCode::TRANSPARENT), "Default");
        assert!(matches_search("Page Up", "page"));
        assert!(!matches_search("Page Up", "home"));
    }
}
