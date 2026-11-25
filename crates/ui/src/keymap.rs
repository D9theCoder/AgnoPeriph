//! Capability-driven keymap layouts for Hall Effect keyboards.
//!
//! Devices with a known physical layout (such as the ATK RS7 75% on its `6 x 16` switch matrix)
//! render their physical keycaps; any other Hall Effect keyboard automatically falls back to a
//! generic grid driven by `HallEffectCapabilities::key_count`.

use std::sync::{Arc, LazyLock};

use agnoperiph_core::{DeviceInfo, HallEffectCapabilities, KeyIndex};
use agnoperiph_driver_atk::protocol::{ATK_RS7_PID, ATK_VID, RS7_KEY_COUNT};
use gpui_kit::SharedString;

/// Matrix columns per row for the RS7 layout and generic fallback grid.
pub const MATRIX_COLS: u16 = 16;

/// One physical keycap in a static layout table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyCap {
    pub row: u8,
    pub col: u8,
    pub label: &'static str,
    /// Keycap width in hundredths of a unit.
    pub width: u16,
    /// Empty space before the keycap in hundredths of a unit.
    pub gap: u16,
}

impl KeyCap {
    pub const fn index(&self) -> KeyIndex {
        KeyIndex::new(self.row as u16 * MATRIX_COLS + self.col as u16)
    }
}

/// Resolved keycap rendered by the Hall Effect matrix for any keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayoutKey {
    pub index: KeyIndex,
    pub label: SharedString,
    /// Keycap width in hundredths of a unit (`100` = 1u).
    pub width: u16,
    /// Empty space before the keycap in hundredths of a unit.
    pub gap: u16,
}

/// Resolved visual matrix layout for a connected Hall Effect keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyLayout {
    pub rows: Vec<Vec<LayoutKey>>,
}

static CACHED_RS7_LAYOUT: LazyLock<Arc<KeyLayout>> = LazyLock::new(|| {
    let rows = RS7_LAYOUT
        .iter()
        .map(|row| {
            row.iter()
                .map(|cap| LayoutKey {
                    index: cap.index(),
                    label: SharedString::new_static(cap.label),
                    width: cap.width,
                    gap: cap.gap,
                })
                .collect()
        })
        .collect();
    Arc::new(KeyLayout { rows })
});

impl KeyLayout {
    /// Builds the best layout for `info` and `caps`, falling back to a generic capability-driven
    /// grid whenever the device has no dedicated physical layout.
    pub fn for_device(info: &DeviceInfo, caps: &HallEffectCapabilities) -> Arc<Self> {
        if info.vid == ATK_VID && info.pid == ATK_RS7_PID && caps.key_count == RS7_KEY_COUNT {
            return Arc::clone(&CACHED_RS7_LAYOUT);
        }
        Arc::new(Self::generic(caps.key_count))
    }

    /// Physical ATK RS7 75% layout (cached across frames).
    pub fn rs7() -> Arc<Self> {
        Arc::clone(&CACHED_RS7_LAYOUT)
    }

    /// Generic grid layout covering `0..key_count` in rows of [`MATRIX_COLS`].
    pub fn generic(key_count: u16) -> Self {
        let mut rows = Vec::new();
        let mut current = Vec::new();
        for ix in 0..key_count {
            let row = ix / MATRIX_COLS;
            let col = ix % MATRIX_COLS;
            if col == 0 && !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            current.push(LayoutKey {
                index: KeyIndex::new(ix),
                label: format!("R{row}C{col}").into(),
                width: 100,
                gap: 0,
            });
        }
        if !current.is_empty() {
            rows.push(current);
        }
        Self { rows }
    }

    /// Human-readable label for `key` within this layout, falling back to matrix coordinates.
    pub fn label(&self, key: KeyIndex) -> SharedString {
        for row in &self.rows {
            if let Some(cap) = row.iter().find(|cap| cap.index == key) {
                return cap.label.clone();
            }
        }
        format!(
            "R{}C{}",
            key.value() / MATRIX_COLS,
            key.value() % MATRIX_COLS
        )
        .into()
    }
}

const fn key(row: u8, col: u8, label: &'static str) -> KeyCap {
    KeyCap {
        row,
        col,
        label,
        width: 100,
        gap: 0,
    }
}

const fn wide(row: u8, col: u8, label: &'static str, width: u16) -> KeyCap {
    KeyCap {
        row,
        col,
        label,
        width,
        gap: 0,
    }
}

const fn gapped(row: u8, col: u8, label: &'static str, gap: u16) -> KeyCap {
    KeyCap {
        row,
        col,
        label,
        width: 100,
        gap,
    }
}

const ROW_0: &[KeyCap] = &[
    key(0, 0, "Esc"),
    gapped(0, 1, "F1", 75),
    key(0, 2, "F2"),
    key(0, 3, "F3"),
    key(0, 4, "F4"),
    gapped(0, 6, "F5", 75),
    key(0, 7, "F6"),
    key(0, 8, "F7"),
    key(0, 9, "F8"),
    gapped(0, 11, "F9", 75),
    key(0, 12, "F10"),
    key(0, 13, "F11"),
    key(0, 14, "F12"),
    gapped(0, 15, "Del", 75),
];

const ROW_1: &[KeyCap] = &[
    key(1, 0, "`"),
    key(1, 1, "1"),
    key(1, 2, "2"),
    key(1, 3, "3"),
    key(1, 4, "4"),
    key(1, 5, "5"),
    key(1, 6, "6"),
    key(1, 7, "7"),
    key(1, 8, "8"),
    key(1, 9, "9"),
    key(1, 10, "0"),
    key(1, 11, "-"),
    key(1, 12, "="),
    wide(1, 14, "Bksp", 200),
    key(1, 15, "Ins"),
];

const ROW_2: &[KeyCap] = &[
    wide(2, 0, "Tab", 150),
    key(2, 1, "Q"),
    key(2, 2, "W"),
    key(2, 3, "E"),
    key(2, 4, "R"),
    key(2, 5, "T"),
    key(2, 6, "Y"),
    key(2, 7, "U"),
    key(2, 8, "I"),
    key(2, 9, "O"),
    key(2, 10, "P"),
    key(2, 11, "["),
    key(2, 12, "]"),
    wide(2, 14, "\\", 150),
    key(2, 15, "PgUp"),
];

const ROW_3: &[KeyCap] = &[
    wide(3, 0, "Caps", 175),
    key(3, 1, "A"),
    key(3, 2, "S"),
    key(3, 3, "D"),
    key(3, 4, "F"),
    key(3, 5, "G"),
    key(3, 6, "H"),
    key(3, 7, "J"),
    key(3, 8, "K"),
    key(3, 9, "L"),
    key(3, 10, ";"),
    key(3, 11, "'"),
    wide(3, 14, "Enter", 200),
    key(3, 15, "PgDn"),
];

const ROW_4: &[KeyCap] = &[
    wide(4, 0, "Shift", 200),
    key(4, 2, "Z"),
    key(4, 3, "X"),
    key(4, 4, "C"),
    key(4, 5, "V"),
    key(4, 6, "B"),
    key(4, 7, "N"),
    key(4, 8, "M"),
    key(4, 9, ","),
    key(4, 10, "."),
    key(4, 11, "/"),
    wide(4, 12, "Shift", 220),
    key(4, 14, "Up"),
];

const ROW_5: &[KeyCap] = &[
    wide(5, 0, "Ctrl", 125),
    wide(5, 1, "Win", 125),
    wide(5, 2, "Alt", 125),
    wide(5, 6, "Space", 700),
    wide(5, 10, "Alt", 125),
    wide(5, 11, "Fn", 125),
    gapped(5, 12, "Left", 30),
    key(5, 14, "Down"),
    key(5, 15, "Right"),
];

/// Keycap rows from the function row down.
pub const RS7_LAYOUT: [&[KeyCap]; 6] = [ROW_0, ROW_1, ROW_2, ROW_3, ROW_4, ROW_5];

/// Finds the keycap wired to `key`, if the layout uses that matrix position.
pub fn keycap(key: KeyIndex) -> Option<&'static KeyCap> {
    RS7_LAYOUT
        .iter()
        .flat_map(|row| row.iter())
        .find(|cap| cap.index() == key)
}

/// Human-readable key name, falling back to its matrix position.
pub fn key_label(key: KeyIndex) -> String {
    match keycap(key) {
        Some(cap) => cap.label.to_owned(),
        None => format!(
            "R{}C{}",
            key.value() / MATRIX_COLS,
            key.value() % MATRIX_COLS
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use agnoperiph_core::{
        ConnectionKind, DeviceId, DeviceInfo, HallEffectCapabilities, KeyIndex, TravelHundredthsMm,
        VendorKind,
    };

    use super::{KeyLayout, RS7_LAYOUT, key_label};

    #[test]
    fn matrix_positions_are_unique_and_in_range() {
        let mut seen = HashSet::new();
        for cap in RS7_LAYOUT.iter().flat_map(|row| row.iter()) {
            assert!(cap.col < 16 && cap.row < 6);
            assert!(seen.insert(cap.index()), "duplicate {}", cap.label);
        }
        assert_eq!(seen.len(), 80);
        assert_eq!(key_label(KeyIndex::new(2 * 16 + 2)), "W");
        assert_eq!(key_label(KeyIndex::new(4 * 16 + 1)), "R4C1");
    }

    #[test]
    fn unknown_hall_effect_device_uses_generic_capability_grid() {
        let info = DeviceInfo::new(
            DeviceId::new("other:1234:5678"),
            VendorKind::Unknown(0x1234),
            "Generic 60% HE",
            ConnectionKind::WiredUsb,
            0x1234,
            0x5678,
        );
        let caps = HallEffectCapabilities::new(
            61,
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(400),
            TravelHundredthsMm::from_hundredths(10),
        );
        let layout = KeyLayout::for_device(&info, &caps);
        let keys: Vec<KeyIndex> = layout
            .rows
            .iter()
            .flat_map(|r| r.iter().map(|k| k.index))
            .collect();
        assert_eq!(keys.len(), 61);
        assert_eq!(keys[0], KeyIndex::new(0));
        assert_eq!(keys[60], KeyIndex::new(60));
        assert_eq!(layout.label(KeyIndex::new(17)), "R1C1");
    }
}
