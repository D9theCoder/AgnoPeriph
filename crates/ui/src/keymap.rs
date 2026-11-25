//! Physical ATK RS7 75% layout on its `6 x 16` switch matrix.
//!
//! Widths and leading gaps are in hundredths of a key unit (`100` = 1u) so layout data
//! stays integral; rows are rendered verbatim and left-aligned, so their total widths
//! differ slightly, exactly as the source layout specifies.

use agnoperiph_core::KeyIndex;

/// Matrix columns per row; `KeyIndex = row * MATRIX_COLS + col`.
pub const MATRIX_COLS: u16 = 16;

/// One physical keycap.
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

    use agnoperiph_core::KeyIndex;

    use super::{RS7_LAYOUT, key_label};

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
}
