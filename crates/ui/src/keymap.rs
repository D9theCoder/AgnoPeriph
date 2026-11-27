//! Capability-driven keymap layouts for Hall Effect keyboards.
//!
//! Devices with a known physical layout (such as the ATK RS7 75% on its `6 x 16` switch matrix)
//! render their physical keycaps; any other keyboard falls back to a grid of `key_count` keys
//! wrapped at the device's matrix width (from its product table or keymap capabilities).

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use agnoperiph_core::{DeviceInfo, HallEffectCapabilities, KeyIndex};
use agnoperiph_driver_atk::protocol::{ATK_RS7_PID, ATK_VID, RS7_KEY_COUNT};
use agnoperiph_driver_wooting::protocol::{WOOTING_VID, lookup_product as wooting_product};
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
    /// A rotary knob rather than a keycap; its press is still a matrix position.
    pub is_knob: bool,
}

/// Resolved visual matrix layout for a connected Hall Effect keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyLayout {
    pub rows: Vec<Vec<LayoutKey>>,
    /// Matrix columns per row, used to name keys the layout does not draw.
    pub cols: u16,
    /// Extra space under the first (function) row, in hundredths of a unit.
    pub row_gap: u16,
}

/// Space between the function row and the number row on 75% boards.
const FUNCTION_ROW_GAP: u16 = 20;
const KEYCHRON_VID: u16 = 0x3434;
const Q1_KNOB_PID: u16 = 0x0107;
const Q1_KNOB_POSITION: u16 = 15;

static CACHED_RS7_LAYOUT: LazyLock<Arc<KeyLayout>> = LazyLock::new(|| {
    Arc::new(KeyLayout::physical(
        &RS7_LAYOUT,
        MATRIX_COLS,
        FUNCTION_ROW_GAP,
    ))
});

impl KeyLayout {
    /// Builds the best layout for `info` and `caps`, falling back to a generic capability-driven
    /// grid whenever the device has no dedicated physical layout.
    pub fn for_device(info: &DeviceInfo, caps: &HallEffectCapabilities) -> Arc<Self> {
        if info.vid == ATK_VID && info.pid == ATK_RS7_PID && caps.key_count == RS7_KEY_COUNT {
            return Arc::clone(&CACHED_RS7_LAYOUT);
        }
        if info.vid == WOOTING_VID
            && let Some(product) = wooting_product(info.pid)
            && product.key_count() == caps.key_count
        {
            return Arc::new(if (product.rows, product.cols) == (6, 14) {
                Self::physical(&WOOTING_60_ROWS, u16::from(product.cols), 0)
            } else {
                Self::grid(caps.key_count, u16::from(product.cols))
            });
        }
        Arc::new(Self::generic(caps.key_count))
    }

    /// Layout for a keyboard known only through its keymap matrix. Keycap names are left to
    /// the caller, which reads them from the keymap.
    pub fn for_keymap(info: &DeviceInfo, rows: u8, cols: u8) -> Self {
        let grid = Self::grid(u16::from(rows) * u16::from(cols), u16::from(cols));
        if info.vid != KEYCHRON_VID || info.pid != Q1_KNOB_PID || (rows, cols) != (6, 16) {
            return grid;
        }
        let mut layout = Self::physical(&Q1_ROWS, u16::from(cols), FUNCTION_ROW_GAP);
        for cap in layout.rows.iter_mut().flatten() {
            cap.label = grid.label(cap.index);
            cap.is_knob = cap.index.value() == Q1_KNOB_POSITION;
        }
        layout
    }

    fn physical(table: &[&[KeyCap]], cols: u16, row_gap: u16) -> Self {
        let rows = table
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cap| LayoutKey {
                        index: KeyIndex::new(u16::from(cap.row) * cols + u16::from(cap.col)),
                        label: SharedString::new_static(cap.label),
                        width: cap.width,
                        gap: cap.gap,
                        is_knob: false,
                    })
                    .collect()
            })
            .collect();
        Self {
            rows,
            cols,
            row_gap,
        }
    }

    /// Width of the widest row, in hundredths of a unit.
    pub fn width(&self) -> u16 {
        self.rows
            .iter()
            .map(|row| row.iter().map(|cap| cap.gap + cap.width).sum())
            .max()
            .unwrap_or(0)
    }

    /// Each key's horizontal centre as a share of the layout's width.
    pub(crate) fn key_centers(&self) -> BTreeMap<KeyIndex, f32> {
        let width = f32::from(self.width().max(1));
        self.rows
            .iter()
            .flat_map(|row| {
                row.iter().scan(0u16, |x, cap| {
                    *x += cap.gap;
                    let center = (f32::from(*x) + f32::from(cap.width) / 2.0) / width;
                    *x += cap.width;
                    Some((cap.index, center))
                })
            })
            .collect()
    }

    /// Physical ATK RS7 75% layout (cached across frames).
    pub fn rs7() -> Arc<Self> {
        Arc::clone(&CACHED_RS7_LAYOUT)
    }

    /// Generic grid layout covering `0..key_count` in rows of [`MATRIX_COLS`].
    pub fn generic(key_count: u16) -> Self {
        Self::grid(key_count, MATRIX_COLS)
    }

    /// Grid layout covering `0..key_count` in rows of `cols` matrix positions.
    pub fn grid(key_count: u16, cols: u16) -> Self {
        let cols = cols.max(1);
        let mut rows = Vec::new();
        let mut current = Vec::new();
        for ix in 0..key_count {
            let row = ix / cols;
            let col = ix % cols;
            if col == 0 && !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            current.push(LayoutKey {
                index: KeyIndex::new(ix),
                label: format!("R{row}C{col}").into(),
                width: 100,
                gap: 0,
                is_knob: false,
            });
        }
        if !current.is_empty() {
            rows.push(current);
        }
        Self {
            rows,
            cols,
            row_gap: 0,
        }
    }

    /// Human-readable label for `key` within this layout, falling back to matrix coordinates.
    pub fn label(&self, key: KeyIndex) -> SharedString {
        for row in &self.rows {
            if let Some(cap) = row.iter().find(|cap| cap.index == key) {
                return cap.label.clone();
            }
        }
        let cols = self.cols.max(1);
        format!("R{}C{}", key.value() / cols, key.value() % cols).into()
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
    gapped(0, 1, "F1", 50),
    key(0, 2, "F2"),
    key(0, 3, "F3"),
    key(0, 4, "F4"),
    gapped(0, 6, "F5", 50),
    key(0, 7, "F6"),
    key(0, 8, "F7"),
    key(0, 9, "F8"),
    gapped(0, 11, "F9", 50),
    key(0, 12, "F10"),
    key(0, 13, "F11"),
    key(0, 14, "F12"),
    gapped(0, 15, "Del", 50),
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
    wide(3, 14, "Enter", 225),
    key(3, 15, "PgDn"),
];

const ROW_4: &[KeyCap] = &[
    wide(4, 0, "Shift", 225),
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
    wide(4, 12, "Shift", 175),
    key(4, 14, "Up"),
];

const ROW_5: &[KeyCap] = &[
    wide(5, 0, "Ctrl", 125),
    wide(5, 1, "Win", 125),
    wide(5, 2, "Alt", 125),
    wide(5, 6, "Space", 625),
    key(5, 10, "Alt"),
    key(5, 11, "Fn"),
    gapped(5, 12, "Left", 100),
    key(5, 14, "Down"),
    key(5, 15, "Right"),
];

/// Keychron Q1 v2 ANSI with the knob (PID `0x0107`), from QMK
/// `keyboards/keychron/q1v2/ansi_encoder/keyboard.json` (`LAYOUT_ansi_82`). Positions and
/// widths only: keycap names come from what the device reports at each position.
const Q1_ROWS: [&[KeyCap]; 6] = [
    &[
        key(0, 0, ""),
        gapped(0, 1, "", 25),
        key(0, 2, ""),
        key(0, 3, ""),
        key(0, 4, ""),
        gapped(0, 5, "", 25),
        key(0, 6, ""),
        key(0, 7, ""),
        key(0, 8, ""),
        gapped(0, 9, "", 25),
        key(0, 10, ""),
        key(0, 11, ""),
        key(0, 12, ""),
        gapped(0, 13, "", 25),
        key(0, 15, ""),
    ],
    &[
        key(1, 0, ""),
        key(1, 1, ""),
        key(1, 2, ""),
        key(1, 3, ""),
        key(1, 4, ""),
        key(1, 5, ""),
        key(1, 6, ""),
        key(1, 7, ""),
        key(1, 8, ""),
        key(1, 9, ""),
        key(1, 10, ""),
        key(1, 11, ""),
        key(1, 12, ""),
        wide(1, 13, "", 200),
        key(1, 15, ""),
    ],
    &[
        wide(2, 0, "", 150),
        key(2, 1, ""),
        key(2, 2, ""),
        key(2, 3, ""),
        key(2, 4, ""),
        key(2, 5, ""),
        key(2, 6, ""),
        key(2, 7, ""),
        key(2, 8, ""),
        key(2, 9, ""),
        key(2, 10, ""),
        key(2, 11, ""),
        key(2, 12, ""),
        wide(2, 13, "", 150),
        key(2, 15, ""),
    ],
    &[
        wide(3, 0, "", 175),
        key(3, 1, ""),
        key(3, 2, ""),
        key(3, 3, ""),
        key(3, 4, ""),
        key(3, 5, ""),
        key(3, 6, ""),
        key(3, 7, ""),
        key(3, 8, ""),
        key(3, 9, ""),
        key(3, 10, ""),
        key(3, 11, ""),
        wide(3, 13, "", 225),
        key(3, 15, ""),
    ],
    &[
        wide(4, 0, "", 225),
        key(4, 2, ""),
        key(4, 3, ""),
        key(4, 4, ""),
        key(4, 5, ""),
        key(4, 6, ""),
        key(4, 7, ""),
        key(4, 8, ""),
        key(4, 9, ""),
        key(4, 10, ""),
        key(4, 11, ""),
        wide(4, 13, "", 175),
        key(4, 14, ""),
    ],
    &[
        wide(5, 0, "", 125),
        wide(5, 1, "", 125),
        wide(5, 2, "", 125),
        wide(5, 6, "", 625),
        key(5, 10, ""),
        key(5, 11, ""),
        key(5, 12, ""),
        key(5, 13, ""),
        key(5, 14, ""),
        key(5, 15, ""),
    ],
];

/// Wooting 60HE ANSI on its `6 x 14` matrix, whose first row a 60% board leaves unused,
/// from OpenRGB `RGBController_WootingKeyboard.cpp` (`led_names_60HE`).
const WOOTING_60_ROWS: [&[KeyCap]; 5] = [
    &[
        key(1, 0, "Esc"),
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
        wide(1, 13, "Bksp", 200),
    ],
    &[
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
        wide(2, 13, "\\", 150),
    ],
    &[
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
        wide(3, 13, "Enter", 225),
    ],
    &[
        wide(4, 0, "Shift", 225),
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
        wide(4, 13, "Shift", 275),
    ],
    &[
        wide(5, 0, "Ctrl", 125),
        wide(5, 1, "Win", 125),
        wide(5, 2, "Alt", 125),
        wide(5, 6, "Space", 625),
        wide(5, 10, "Alt", 125),
        wide(5, 11, "Menu", 125),
        wide(5, 12, "Ctrl", 125),
        wide(5, 13, "Fn", 125),
    ],
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

/// The compact name printed on a keycap.
pub fn legend(name: &str) -> &str {
    match name {
        "Bksp" => "Backspace",
        "Caps Lock" => "Caps",
        "Left Shift" | "Right Shift" => "Shift",
        "Left Ctrl" | "Right Ctrl" => "Ctrl",
        "Left Alt" | "Right Alt" => "Alt",
        "Left Win" | "Right Win" => "Win",
        "Insert" => "Ins",
        "Delete" => "Del",
        "Page Up" => "PgUp",
        "Page Down" => "PgDn",
        "Up" => "↑",
        "Down" => "↓",
        "Left" => "←",
        "Right" => "→",
        other => other,
    }
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

    #[test]
    fn wooting_board_wraps_at_its_matrix_width() {
        let info = DeviceInfo::new(
            DeviceId::new("wooting:31E3:1302"),
            VendorKind::Wooting,
            "Wooting 60HE",
            ConnectionKind::WiredUsb,
            0x31E3,
            0x1302,
        );
        let caps = HallEffectCapabilities::new(
            84,
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(400),
            TravelHundredthsMm::from_hundredths(5),
        );
        let layout = KeyLayout::for_device(&info, &caps);
        // ANSI 60%: the matrix's function row is not populated.
        assert_eq!(layout.rows.len(), 5);
        assert_eq!(layout.rows[0][0].index, KeyIndex::new(14));
        assert_eq!(layout.width(), 1500);
        assert_eq!(layout.label(KeyIndex::new(2 * 14 + 2)), "W");
    }

    #[test]
    fn q1_knob_layout_covers_every_documented_position() {
        let info = DeviceInfo::new(
            DeviceId::new("via:3434:0107"),
            VendorKind::Keychron,
            "Keychron Q1",
            ConnectionKind::WiredUsb,
            0x3434,
            0x0107,
        );
        let layout = KeyLayout::for_keymap(&info, 6, 16);
        let keys: HashSet<KeyIndex> = layout.rows.iter().flatten().map(|k| k.index).collect();
        assert_eq!(keys.len(), 82);
        assert_eq!(layout.width(), 1600);
        assert_eq!(layout.label(KeyIndex::new(17)), "R1C1");
        let knob = layout.rows[0].last().unwrap();
        assert!(knob.is_knob && knob.index == KeyIndex::new(15));
        // An unlisted matrix keeps the grid.
        assert_eq!(KeyLayout::for_keymap(&info, 6, 15).rows[0].len(), 15);
    }

    #[test]
    fn grid_follows_keymap_columns() {
        let layout = KeyLayout::grid(6 * 15, 15);
        assert_eq!(layout.rows.len(), 6);
        assert_eq!(layout.rows[2][0].index, KeyIndex::new(30));
        assert_eq!(layout.label(KeyIndex::new(31)), "R2C1");
    }
}
