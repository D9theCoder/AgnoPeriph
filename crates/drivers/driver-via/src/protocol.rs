//! QMK / VIA Raw HID keyboard codec (`Usage Page 0xFF60`, `Usage ID 0x0061`).
//!
//! Verified directly from canonical `qmk/qmk_firmware` (`quantum/via.h`, `quantum/via.c`)
//! and documented in `docs/protocols/qmk-via.md`.

use agnoperiph_core::{
    KeyCode, MacroActionKind, MacroSlot, MacroStep, RgbColor, RgbEffect, RgbPresetInfo, VendorKind,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub const VIA_USAGE_PAGE: u16 = 0xFF60;
pub const VIA_USAGE_ID: u16 = 0x0061;
pub const VIA_REPORT_ID: u8 = 0x00;
pub const VIA_PAYLOAD_LEN: usize = 32;
pub const VIA_TX_LEN: usize = VIA_PAYLOAD_LEN + 1;
pub const VIA_MAX_CHUNK: usize = 28;

/// Well-known keyboard brands shipping QMK / VIA firmware on `0xFF60:0x0061`.
pub const VIA_VENDORS: &[(u16, VendorKind, &str)] = &[
    (0x3434, VendorKind::Keychron, "Keychron VIA Keyboard"),
    (0x362D, VendorKind::Keychron, "Lemokey VIA Keyboard"),
    (0x19F5, VendorKind::QmkVia, "NuPhy QMK/VIA Keyboard"),
    (0x3151, VendorKind::QmkVia, "MonsGeek / Akko QMK Keyboard"),
    (0x320F, VendorKind::QmkVia, "Glorious GMMK QMK Keyboard"),
    (0x04D8, VendorKind::QmkVia, "Drop QMK/VIA Keyboard"),
    (0xFEED, VendorKind::QmkVia, "QMK / VIA Keyboard"),
];

/// Keychron mouse/receiver PIDs that share `0x3434` + `0xFF60:0x0061` and are not keyboards.
pub const EXCLUDED_KEYCHRON_MOUSE_PIDS: &[u16] = &[0x0440, 0xD026, 0xD029];

pub fn vendor_for_vid(vid: u16) -> (VendorKind, &'static str) {
    VIA_VENDORS.iter().find(|&&(v, _, _)| v == vid).map_or(
        (VendorKind::QmkVia, "QMK / VIA Keyboard"),
        |&(_, kind, name)| (kind, name),
    )
}

/// Canonical `enum via_command_id` from `qmk/qmk_firmware/quantum/via.h`.
pub mod cmd {
    pub const GET_PROTOCOL_VERSION: u8 = 0x01;
    pub const GET_KEYBOARD_VALUE: u8 = 0x02;
    pub const SET_KEYBOARD_VALUE: u8 = 0x03;
    pub const DYNAMIC_KEYMAP_GET_KEYCODE: u8 = 0x04;
    pub const DYNAMIC_KEYMAP_SET_KEYCODE: u8 = 0x05;
    pub const DYNAMIC_KEYMAP_RESET: u8 = 0x06;
    pub const CUSTOM_SET_VALUE: u8 = 0x07;
    pub const CUSTOM_GET_VALUE: u8 = 0x08;
    pub const CUSTOM_SAVE: u8 = 0x09;
    pub const FORBIDDEN_EEPROM_RESET: u8 = 0x0A;
    pub const FORBIDDEN_BOOTLOADER_JUMP: u8 = 0x0B;
    pub const MACRO_GET_COUNT: u8 = 0x0C;
    pub const MACRO_GET_BUFFER_SIZE: u8 = 0x0D;
    pub const MACRO_GET_BUFFER: u8 = 0x0E;
    pub const MACRO_SET_BUFFER: u8 = 0x0F;
    pub const MACRO_RESET: u8 = 0x10;
    pub const DYNAMIC_KEYMAP_GET_LAYER_COUNT: u8 = 0x11;
    pub const DYNAMIC_KEYMAP_GET_BUFFER: u8 = 0x12;
    pub const DYNAMIC_KEYMAP_SET_BUFFER: u8 = 0x13;
    pub const UNHANDLED: u8 = 0xFF;
}

pub mod kb_val {
    pub const UPTIME: u8 = 0x01;
    pub const LAYOUT_OPTIONS: u8 = 0x02;
    pub const FIRMWARE_VERSION: u8 = 0x04;
}

pub mod channel {
    pub const QMK_BACKLIGHT: u8 = 1;
    pub const QMK_RGBLIGHT: u8 = 2;
    pub const QMK_RGB_MATRIX: u8 = 3;
}

pub mod rgb_val {
    pub const BRIGHTNESS: u8 = 1;
    pub const EFFECT: u8 = 2;
    pub const EFFECT_SPEED: u8 = 3;
    pub const COLOR: u8 = 4;
}

/// Wraps a 32-byte VIA payload with leading `0x00` Report ID for `hidapi`.
pub fn wrap_tx(payload: [u8; VIA_PAYLOAD_LEN]) -> [u8; VIA_TX_LEN] {
    let mut out = [0u8; VIA_TX_LEN];
    out[0] = VIA_REPORT_ID;
    out[1..].copy_from_slice(&payload);
    out
}

/// Strips optional leading `0x00` Report ID from a 32-byte or 33-byte VIA reply.
pub fn unwrap_rx(raw: &[u8]) -> Option<[u8; VIA_PAYLOAD_LEN]> {
    let mut out = [0u8; VIA_PAYLOAD_LEN];
    if raw.len() == VIA_PAYLOAD_LEN {
        out.copy_from_slice(raw);
        Some(out)
    } else if raw.len() == VIA_TX_LEN && raw[0] == VIA_REPORT_ID {
        out.copy_from_slice(&raw[1..]);
        Some(out)
    } else {
        None
    }
}

/// Strict pre-transmission validator enforcing 33-byte Output reports (`Report ID 0x00`)
/// and blocking `id_eeprom_reset` (`0x0A`) and `id_bootloader_jump` (`0x0B`).
pub fn via_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![VIA_TX_LEN], vec![VIA_REPORT_ID]).with_checksum(
        |kind, report| {
            if kind != ReportKind::Output {
                return Err(format!(
                    "QMK/VIA Raw HID requires Output reports, got {kind}"
                ));
            }
            let cmd_id = report[1];
            if matches!(
                cmd_id,
                cmd::FORBIDDEN_EEPROM_RESET | cmd::FORBIDDEN_BOOTLOADER_JUMP
            ) {
                return Err(format!(
                    "blocked destructive/bootloader VIA command 0x{cmd_id:02X}"
                ));
            }
            Ok(())
        },
    )
}

pub fn req_protocol_version() -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::GET_PROTOCOL_VERSION;
    p
}

pub fn req_firmware_version() -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = kb_val::FIRMWARE_VERSION;
    p
}

pub fn req_layer_count() -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::DYNAMIC_KEYMAP_GET_LAYER_COUNT;
    p
}

pub fn req_keymap_get_buffer(offset: u16, size: u8) -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::DYNAMIC_KEYMAP_GET_BUFFER;
    let [hi, lo] = offset.to_be_bytes();
    p[1] = hi;
    p[2] = lo;
    p[3] = size.min(u8::try_from(VIA_MAX_CHUNK).unwrap_or(28));
    p
}

pub fn req_keymap_set_keycode(layer: u8, row: u8, col: u8, code: KeyCode) -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::DYNAMIC_KEYMAP_SET_KEYCODE;
    p[1] = layer;
    p[2] = row;
    p[3] = col;
    p[4] = code.class;
    p[5] = code.id;
    p
}

pub fn req_keymap_reset() -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::DYNAMIC_KEYMAP_RESET;
    p
}

pub fn req_macro_count() -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::MACRO_GET_COUNT;
    p
}

pub fn req_macro_buffer_size() -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::MACRO_GET_BUFFER_SIZE;
    p
}

pub fn req_macro_get_buffer(offset: u16, size: u8) -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::MACRO_GET_BUFFER;
    let [hi, lo] = offset.to_be_bytes();
    p[1] = hi;
    p[2] = lo;
    p[3] = size.min(u8::try_from(VIA_MAX_CHUNK).unwrap_or(28));
    p
}

pub fn req_macro_set_buffer(offset: u16, chunk: &[u8]) -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    let len = chunk.len().min(VIA_MAX_CHUNK);
    p[0] = cmd::MACRO_SET_BUFFER;
    let [hi, lo] = offset.to_be_bytes();
    p[1] = hi;
    p[2] = lo;
    p[3] = u8::try_from(len).unwrap_or(0);
    p[4..4 + len].copy_from_slice(&chunk[..len]);
    p
}

pub fn req_custom_get(chan: u8, val_id: u8) -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::CUSTOM_GET_VALUE;
    p[1] = chan;
    p[2] = val_id;
    p
}

pub fn req_custom_set(chan: u8, val_id: u8, data: &[u8]) -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::CUSTOM_SET_VALUE;
    p[1] = chan;
    p[2] = val_id;
    let len = data.len().min(VIA_PAYLOAD_LEN - 3);
    p[3..3 + len].copy_from_slice(&data[..len]);
    p
}

pub fn req_custom_save(chan: u8) -> [u8; VIA_PAYLOAD_LEN] {
    let mut p = [0u8; VIA_PAYLOAD_LEN];
    p[0] = cmd::CUSTOM_SAVE;
    p[1] = chan;
    p
}

/// Converts QMK 8-bit HSV (`h: 0..=255`, `s: 0..=255`, `v = 255`) to 24-bit sRGB.
pub fn qmk_hs_to_rgb(hue: u8, sat: u8) -> RgbColor {
    if sat == 0 {
        return RgbColor::WHITE;
    }
    let scaled_hue = u32::from(hue) * 6;
    let region = scaled_hue / 255;
    let frac = scaled_hue % 255;
    let sat_val = u32::from(sat);
    let floor = 255 - sat_val;
    let falling = 255 - (sat_val * frac) / 255;
    let rising = 255 - (sat_val * (255 - frac)) / 255;
    let (red, green, blue) = match region {
        0 => (255, rising, floor),
        1 => (falling, 255, floor),
        2 => (floor, 255, rising),
        3 => (floor, falling, 255),
        4 => (rising, floor, 255),
        5 => (255, floor, falling),
        _ => (255, floor, floor),
    };
    RgbColor::new(
        u8::try_from(red.min(255)).unwrap_or(255),
        u8::try_from(green.min(255)).unwrap_or(255),
        u8::try_from(blue.min(255)).unwrap_or(255),
    )
}

/// Converts 24-bit sRGB to QMK 8-bit `(hue, sat)`.
pub fn rgb_to_qmk_hs(color: RgbColor) -> (u8, u8) {
    let r = u16::from(color.r);
    let g = u16::from(color.g);
    let b = u16::from(color.b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max == 0 || max == min {
        return (0, 0);
    }
    let delta = max - min;
    let sat = (delta * 255) / max;
    let hue = if max == r {
        if g >= b {
            (43 * (g - b)) / delta
        } else {
            255u16.saturating_sub((43 * (b - g)) / delta)
        }
    } else if max == g {
        85 + (43 * (b.saturating_sub(r))) / delta - (43 * (r.saturating_sub(b))) / delta
    } else {
        171 + (43 * (r.saturating_sub(g))) / delta - (43 * (g.saturating_sub(r))) / delta
    };
    (
        u8::try_from(hue.min(255)).unwrap_or(0),
        u8::try_from(sat.min(255)).unwrap_or(0),
    )
}

/// Standard QMK RGB Matrix effect presets.
pub fn rgb_matrix_presets() -> Vec<RgbPresetInfo> {
    vec![
        RgbPresetInfo::new(0, "Off", false, false, false),
        RgbPresetInfo::new(1, "Solid Color", true, false, false),
        RgbPresetInfo::new(2, "Alphas Mods", true, true, false),
        RgbPresetInfo::new(3, "Gradient Up Down", true, true, false),
        RgbPresetInfo::new(4, "Gradient Left Right", true, true, false),
        RgbPresetInfo::new(5, "Breathing", true, true, false),
        RgbPresetInfo::new(6, "Band Sat", true, true, false),
        RgbPresetInfo::new(13, "Cycle All", false, true, true),
        RgbPresetInfo::new(14, "Cycle Left Right", false, true, true),
        RgbPresetInfo::new(15, "Cycle Up Down", false, true, true),
        RgbPresetInfo::new(16, "Rainbow Moving Chevron", false, true, true),
    ]
}

/// Encodes VIA v3 NUL-delimited macro slots (`0x01 0x01 <kc>` tap, `0x01 0x02 <kc>` down, `0x01 0x03 <kc>` up).
pub fn encode_via_macros(slots: &[MacroSlot], max_bytes: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for slot in slots {
        for step in &slot.steps {
            let op = match step.kind {
                MacroActionKind::KeyTap => 0x01u8,
                MacroActionKind::KeyDown => 0x02u8,
                MacroActionKind::KeyUp => 0x03u8,
                _ => continue,
            };
            if out.len() + 4 > max_bytes {
                break;
            }
            out.push(0x01);
            out.push(op);
            out.push(step.hid_keycode);
        }
        if out.len() < max_bytes {
            out.push(0x00);
        }
    }
    out
}

/// Decodes VIA v3 NUL-delimited macro buffer into [`MacroSlot`] entries.
pub fn decode_via_macros(buf: &[u8], max_slots: u8) -> Vec<MacroSlot> {
    let mut slots = Vec::new();
    let mut idx = 0usize;
    for slot_ix in 0..max_slots {
        let mut steps = Vec::new();
        while idx < buf.len() {
            let b = buf[idx];
            idx += 1;
            if b == 0x00 {
                break;
            }
            if b == 0x01 && idx + 1 < buf.len() {
                let op = buf[idx];
                let kc = buf[idx + 1];
                idx += 2;
                let kind = match op {
                    0x02 => MacroActionKind::KeyDown,
                    0x03 => MacroActionKind::KeyUp,
                    _ => MacroActionKind::KeyTap,
                };
                steps.push(MacroStep::new(kind, kc, 0));
            }
        }
        slots.push(MacroSlot::new(
            slot_ix,
            format!("M{}", u16::from(slot_ix) + 1),
            steps,
        ));
    }
    slots
}

/// Maps a QMK RGB effect mode byte to AgnoPeriph's [`RgbEffect`].
pub const fn decode_rgb_effect(mode: u8, color: RgbColor, speed: u8) -> RgbEffect {
    match mode {
        0 => RgbEffect::Off,
        1 => RgbEffect::Static { color },
        5 => RgbEffect::Breathing { color, speed },
        13 => RgbEffect::SpectrumCycle { speed },
        id => RgbEffect::Preset {
            id,
            color,
            speed,
            colorful: id >= 13,
        },
    }
}

/// Maps AgnoPeriph's [`RgbEffect`] to `(mode_id, optional_color, optional_speed)`.
pub const fn encode_rgb_effect(effect: RgbEffect) -> (u8, Option<RgbColor>, Option<u8>) {
    match effect {
        RgbEffect::Off => (0, None, None),
        RgbEffect::Static { color } => (1, Some(color), None),
        RgbEffect::Breathing { color, speed } => (5, Some(color), Some(speed)),
        RgbEffect::SpectrumCycle { speed } => (13, None, Some(speed)),
        RgbEffect::Wave { speed, .. } => (14, None, Some(speed)),
        RgbEffect::Reactive { color, speed } => (26, Some(color), Some(speed)),
        RgbEffect::Preset {
            id, color, speed, ..
        } => (id, Some(color), Some(speed)),
    }
}
