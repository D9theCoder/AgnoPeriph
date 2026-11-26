//! LAMZU Crystal Wireless packed flash image (CompX Report-8, `Mouse Drive 2.1.0.43`).
//!
//! The device exposes a 6912-byte packed window at address 0. It is not the 10428-byte
//! `FlashDataMap` struct used by the vendor app in memory: macros and key bindings are
//! stored with 4- and 5-byte records plus one trailing `0x55`-sum checksum per field.
//! Macro storage is bound to the key-function table: key slot `i` holds a `[0x06, i,
//! repeat]` trigger at `KEY_FUN_MAP + i * KEY_SLOT_STRIDE` and its record at
//! `MACRO_SLOTS + i * MACRO_SLOT_STRIDE`.
//!
//! See `docs/protocols/lamzu-crystall.md` for the evidence behind every offset.

use agnoperiph_core::{CoreError, CoreResult, MacroActionKind, MacroSlot, MacroStep};

/// Offsets inside the 6912-byte packed flash window.
pub mod layout {
    pub const KEY_FUN_MAP: u16 = 0x60;
    pub const KEY_SLOT_STRIDE: u16 = 4;
    pub const KEY_SLOT_COUNT: u8 = 16;

    pub const MACRO_SLOTS: u16 = 0x300;
    pub const MACRO_SLOT_STRIDE: u16 = 0x180;
    pub const MACRO_SLOT_COUNT: u8 = 16;

    pub const NAME_LENGTH_OFFSET: u16 = 0;
    pub const NAME_OFFSET: u16 = 1;
    pub const NAME_MAX: usize = 30;
    pub const CONTEXT_COUNT_OFFSET: u16 = 31;
    pub const CONTEXT_OFFSET: u16 = 32;
    pub const CONTEXT_MAX: usize = 70;
    pub const CONTEXT_LEN: usize = 5;

    /// First byte of the name block, used for the "slot is empty" probe.
    pub const SLOT_NAME_BLOCK_LEN: usize = NAME_MAX + 1;
    /// Count byte plus the maximum context payload.
    pub const SLOT_CONTEXT_BLOCK_MAX: usize = 1 + CONTEXT_LEN * CONTEXT_MAX;
}

/// Key-function action IDs shared with the CompX action triplets.
pub mod key_class {
    pub const DISABLED: u8 = 0;
    pub const MACRO: u8 = 6;
}

/// Event types stored in the low nibble of a packed context.
pub mod hid_code_type {
    pub const MODIFIER: u8 = 0;
    pub const KEYBOARD: u8 = 1;
    pub const MEDIA: u8 = 2;
    pub const POWER: u8 = 3;
    pub const MOUSE_BUTTON: u8 = 4;
    pub const MOVE_XY: u8 = 5;
}

/// Event states stored in the top two bits of a packed context.
pub mod key_state {
    pub const DOWN: u8 = 0;
    pub const UP: u8 = 1;
    pub const NONE: u8 = 2;

    pub const DOWN_FLAG: u8 = 0x80;
    pub const UP_FLAG: u8 = 0x40;
}

/// Absolute flash address of key slot `index`'s trigger triplet.
pub fn key_slot_address(index: u8) -> CoreResult<u16> {
    check_index(index)?;
    Ok(layout::KEY_FUN_MAP + u16::from(index) * layout::KEY_SLOT_STRIDE)
}

/// Absolute flash address of macro slot `index`'s record.
pub fn macro_slot_address(index: u8) -> CoreResult<u16> {
    check_index(index)?;
    Ok(layout::MACRO_SLOTS + u16::from(index) * layout::MACRO_SLOT_STRIDE)
}

fn check_index(index: u8) -> CoreResult<()> {
    if index >= layout::MACRO_SLOT_COUNT {
        return Err(CoreError::Validation(format!(
            "Lamzu macro slot {index} is out of range 0..{}",
            layout::MACRO_SLOT_COUNT
        )));
    }
    Ok(())
}

/// Encodes a macro name into the `[length][bytes]` block (length byte excluded from `NAME_MAX`).
pub fn encode_name(name: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(layout::NAME_MAX + 1);
    for ch in name.chars() {
        if bytes.len() == layout::NAME_MAX {
            break;
        }
        bytes.push(if ch.is_ascii() { ch as u8 } else { b'?' });
    }
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(u8::try_from(bytes.len()).unwrap_or(0));
    out.extend_from_slice(&bytes);
    out
}

/// Encodes steps into the `[count][flags, value_le16, delay_be16]...` block. A tap takes
/// two contexts, and a macro over `CONTEXT_MAX` contexts is refused rather than cut short.
pub fn encode_contexts(steps: &[MacroStep]) -> CoreResult<Vec<u8>> {
    let mut contexts: Vec<[u8; layout::CONTEXT_LEN]> = Vec::new();
    for step in steps {
        match step.kind {
            MacroActionKind::KeyDown => push_key(&mut contexts, key_state::DOWN, *step),
            MacroActionKind::KeyUp => push_key(&mut contexts, key_state::UP, *step),
            MacroActionKind::KeyTap => {
                push_key(&mut contexts, key_state::DOWN, *step);
                push_key(&mut contexts, key_state::UP, *step);
            }
        }
    }

    if contexts.len() > layout::CONTEXT_MAX {
        return Err(CoreError::OutOfRange {
            field: "lamzu_macro_events",
            actual: contexts.len().to_string(),
            expected: format!("0..={}", layout::CONTEXT_MAX),
        });
    }
    let mut out = Vec::with_capacity(1 + contexts.len() * layout::CONTEXT_LEN);
    out.push(u8::try_from(contexts.len()).unwrap_or(0));
    for context in contexts {
        out.extend_from_slice(&context);
    }
    Ok(out)
}

/// Header byte as a length, or `0` when it is above `max` (`0xFF` on erased flash).
pub fn header_len(raw: u8, max: usize) -> usize {
    let len = usize::from(raw);
    if len > max { 0 } else { len }
}

fn push_key(contexts: &mut Vec<[u8; layout::CONTEXT_LEN]>, state: u8, step: MacroStep) {
    let code = u16::from(step.hid_keycode).to_le_bytes();
    // The packed record stores the delay big-endian, unlike every other multi-byte field.
    let delay = step.delay_ms.to_be_bytes();
    contexts.push([
        state_flag(state) | hid_code_type::KEYBOARD,
        code[0],
        code[1],
        delay[0],
        delay[1],
    ]);
}

const fn state_flag(state: u8) -> u8 {
    match state {
        key_state::DOWN => key_state::DOWN_FLAG,
        key_state::UP => key_state::UP_FLAG,
        _ => 0,
    }
}

/// Decodes a macro slot from its raw name and context blocks plus trigger repeat.
pub fn decode_slot(
    index: u8,
    name_block: &[u8],
    context_block: &[u8],
    repeat: u8,
) -> CoreResult<MacroSlot> {
    check_index(index)?;
    let name_len = header_len(*name_block.first().unwrap_or(&0), layout::NAME_MAX);
    let name_bytes = name_block.get(1..1 + name_len).ok_or_else(|| {
        CoreError::Protocol(format!("Lamzu macro slot {index} name is truncated"))
    })?;
    let name = String::from_utf8_lossy(name_bytes).into_owned();

    let mut steps = Vec::new();
    let count = header_len(*context_block.first().unwrap_or(&0), layout::CONTEXT_MAX);
    if count > 0 {
        let needed = 1 + count * layout::CONTEXT_LEN;
        let raw = context_block.get(..needed).ok_or_else(|| {
            CoreError::Protocol(format!(
                "Lamzu macro slot {index} context block is truncated"
            ))
        })?;
        for event in raw[1..].as_chunks::<{ layout::CONTEXT_LEN }>().0 {
            let flags = event[0];
            let kind = flags & 0x0F;
            let state = match flags & 0xC0 {
                key_state::DOWN_FLAG => key_state::DOWN,
                key_state::UP_FLAG => key_state::UP,
                _ => key_state::NONE,
            };
            // MoveXY pairs and every other non-keyboard event cannot be re-encoded, so
            // the whole slot is reported instead of loading with those events missing.
            if kind != hid_code_type::KEYBOARD {
                return Err(CoreError::UnsupportedCapability(
                    "lamzu_macro_non_keyboard_event",
                ));
            }
            let code = u16::from_le_bytes([event[1], event[2]]);
            let keycode = u8::try_from(code).map_err(|_| {
                CoreError::Protocol(format!(
                    "Lamzu macro slot {index} uses keyboard code 0x{code:04X}"
                ))
            })?;
            let delay_ms = u16::from_be_bytes([event[3], event[4]]);
            let action = match state {
                key_state::DOWN => MacroActionKind::KeyDown,
                key_state::UP => MacroActionKind::KeyUp,
                other => {
                    return Err(CoreError::Protocol(format!(
                        "Lamzu macro slot {index} has unknown key state {other}"
                    )));
                }
            };
            steps.push(MacroStep::new(action, keycode, delay_ms));
        }
    }

    Ok(MacroSlot::new(index, name, steps).with_repeat(repeat.max(1)))
}

/// Trigger triplet that binds key slot `index` to macro slot `index`.
pub fn macro_binding(index: u8, repeat: u8) -> [u8; 3] {
    [
        key_class::MACRO,
        index,
        if repeat == 0 { 1 } else { repeat },
    ]
}

/// Returns the repeat count when `triplet` binds key slot `index` to its macro slot.
pub fn binding_repeat(index: u8, triplet: &[u8; 3]) -> Option<u8> {
    if triplet[0] == key_class::MACRO && triplet[1] == index {
        Some(if triplet[2] == 0 { 1 } else { triplet[2] })
    } else {
        None
    }
}

/// True when the context block holds an event this driver cannot re-encode losslessly.
pub fn has_unsupported_context(block: &[u8]) -> bool {
    let count = header_len(*block.first().unwrap_or(&0), layout::CONTEXT_MAX);
    if count == 0 {
        return false;
    }
    let Some(raw) = block.get(..1 + count * layout::CONTEXT_LEN) else {
        return true;
    };
    raw[1..]
        .as_chunks::<{ layout::CONTEXT_LEN }>()
        .0
        .iter()
        .any(|event| {
            let kind = event[0] & 0x0F;
            let state = event[0] & 0xC0;
            kind != hid_code_type::KEYBOARD
                || !matches!(state, key_state::DOWN_FLAG | key_state::UP_FLAG)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_slot_offsets_match_the_packed_flash_window() {
        assert_eq!(key_slot_address(0).unwrap(), 0x60);
        assert_eq!(key_slot_address(15).unwrap(), 0x9C);
        assert_eq!(macro_slot_address(0).unwrap(), 0x300);
        assert_eq!(macro_slot_address(1).unwrap(), 0x480);
        assert_eq!(macro_slot_address(15).unwrap(), 0x1980);
        assert!(macro_slot_address(16).is_err());
    }

    #[test]
    fn name_is_truncated_to_thirty_ascii_bytes() {
        let block = encode_name("0123456789012345678901234567890123456789");
        assert_eq!(block[0], 30);
        assert_eq!(block.len(), 31);
        let unicode = encode_name("é");
        assert_eq!(unicode, vec![1, b'?']);
    }

    #[test]
    fn context_layout_packs_state_type_value_and_big_endian_delay() {
        let steps = vec![
            MacroStep::new(MacroActionKind::KeyDown, 0x04, 10),
            MacroStep::new(MacroActionKind::KeyUp, 0x04, 20),
            MacroStep::new(MacroActionKind::KeyTap, 0x05, 500),
        ];
        let block = encode_contexts(&steps).unwrap();
        assert_eq!(block[0], 4);
        assert_eq!(&block[1..6], &[0x81, 0x04, 0x00, 0x00, 0x0A]);
        assert_eq!(&block[6..11], &[0x41, 0x04, 0x00, 0x00, 0x14]);
        assert_eq!(&block[11..16], &[0x81, 0x05, 0x00, 0x01, 0xF4]);
        assert_eq!(&block[16..21], &[0x41, 0x05, 0x00, 0x01, 0xF4]);
    }

    #[test]
    fn slots_round_trip() {
        let steps = vec![
            MacroStep::new(MacroActionKind::KeyDown, 0x04, 10),
            MacroStep::new(MacroActionKind::KeyUp, 0x04, 25),
        ];
        let slot = MacroSlot::new(3, "burst", steps.clone()).with_repeat(4);
        let name = encode_name(&slot.name);
        let contexts = encode_contexts(&slot.steps).unwrap();
        let decoded = decode_slot(3, &name, &contexts, slot.repeat).unwrap();
        assert_eq!(decoded, slot);
    }

    #[test]
    fn non_keyboard_events_are_reported_instead_of_dropped() {
        let mut block = vec![1, hid_code_type::MEDIA, 0x01, 0x00, 0, 0];
        assert!(has_unsupported_context(&block));
        assert!(decode_slot(0, &[0], &block, 1).is_err());
        block[1] = key_state::DOWN_FLAG | hid_code_type::KEYBOARD;
        assert!(!has_unsupported_context(&block));
        assert!(decode_slot(0, &[0], &block, 1).is_ok());
    }

    #[test]
    fn captured_mouse_button_macro_and_move_xy_are_unsupported() {
        // `spam click` from the live 6912-byte dump: left button down, then up, 10 ms each.
        let spam_click = [
            0x02, 0x84, 0x01, 0x00, 0x00, 0x0A, 0x44, 0x01, 0x00, 0x00, 0x0A,
        ];
        let sum = spam_click.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
        assert_eq!(0x55u8.wrapping_sub(sum), 0x75, "checksum from the capture");
        assert!(has_unsupported_context(&spam_click));
        assert!(matches!(
            decode_slot(
                5,
                b"
spam click",
                &spam_click,
                1
            ),
            Err(CoreError::UnsupportedCapability(_))
        ));

        // A MoveXY pair beside keyboard events must not load as the keyboard part alone.
        let mixed = [
            3,
            key_state::DOWN_FLAG | hid_code_type::KEYBOARD,
            0x04,
            0,
            0,
            10,
            hid_code_type::MOVE_XY,
            0x05,
            0xFB,
            0,
            10,
            key_state::UP_FLAG | hid_code_type::KEYBOARD,
            0x04,
            0,
            0,
            10,
        ];
        assert!(has_unsupported_context(&mixed));
        assert!(decode_slot(0, &[0], &mixed, 1).is_err());
    }

    #[test]
    fn erased_headers_read_as_empty_instead_of_full() {
        assert_eq!(header_len(0xFF, layout::CONTEXT_MAX), 0);
        assert_eq!(header_len(70, layout::CONTEXT_MAX), 70);
        assert_eq!(header_len(31, layout::NAME_MAX), 0);
        let slot = decode_slot(2, &[0xFF], &[0xFF], 1).unwrap();
        assert_eq!(slot.name, "");
        assert_eq!(slot.steps, Vec::<MacroStep>::new());
        assert!(!has_unsupported_context(&[0xFF]));
    }

    #[test]
    fn macros_over_the_context_limit_are_refused() {
        let taps = vec![MacroStep::new(MacroActionKind::KeyTap, 0x04, 10); 35];
        assert_eq!(encode_contexts(&taps).unwrap()[0], 70);
        let mut over = taps;
        over.push(MacroStep::new(MacroActionKind::KeyDown, 0x04, 10));
        assert!(matches!(
            encode_contexts(&over),
            Err(CoreError::OutOfRange { .. })
        ));
    }

    #[test]
    fn bindings_encode_slot_and_repeat() {
        assert_eq!(macro_binding(3, 4), [6, 3, 4]);
        assert_eq!(binding_repeat(3, &[6, 3, 4]), Some(4));
        assert_eq!(binding_repeat(3, &[6, 3, 0]), Some(1));
        assert_eq!(binding_repeat(3, &[6, 2, 4]), None);
        assert_eq!(binding_repeat(3, &[1, 3, 0]), None);
    }
}
