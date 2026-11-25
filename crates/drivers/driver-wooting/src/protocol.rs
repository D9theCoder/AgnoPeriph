//! Wooting ARM / Hall Effect keyboard USB HID codec.
//!
//! Verified against `docs/protocols/wooting-hid.md`, official `WootingKb/wooting-rgb-sdk`
//! (`src/wooting-usb.h`, `src/wooting-usb.c`), and `OpenMouse-Project/mouse-protocol` (`src/wooting/index.ts`).

use agnoperiph_core::{RgbColor, TravelHundredthsMm};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub const WOOTING_VID: u16 = 0x31E3;
pub const WOOTING_CONFIG_USAGE_PAGE: u16 = 0xFF55;
pub const WOOTING_CONFIG_USAGE_PAGE_LEGACY: u16 = 0x1337;
pub const WOOTING_CONFIG_USAGE_ID: u16 = 0x0001;

pub const WOOTING_COMMAND_SIZE: usize = 8;
pub const WOOTING_MAGIC_SINGLE: u8 = 0xD0;
pub const WOOTING_MAGIC_MULTI: u8 = 0xD1;
pub const WOOTING_MAGIC_WORD_1: u8 = 0xDA;
pub const WOOTING_STATUS_OK: u8 = 0x88;

/// Actuation travel linear formula from Wooting `get_keyboard_profile` (`0x27`):
/// `raw = 16384 + mm * 4096` (`0x4000` = 0.00 mm, `0x8000` = 4.00 mm).
pub const ACTUATION_ZERO: u32 = 16_384;
pub const ACTUATION_UNITS_PER_MM: u32 = 4_096;

pub mod cmd {
    // Verified read-only commands
    pub const GET_VERSION: u8 = 0x01;
    pub const GET_SERIAL: u8 = 0x03;
    pub const GET_CURRENT_PROFILE_INDEX: u8 = 0x0B;
    pub const GET_DEVICE_CONFIG: u8 = 0x13;
    pub const GET_KEYBOARD_PROFILE: u8 = 0x27;
    pub const GET_SETTINGS: u8 = 0x33;

    // Verified official wooting-rgb-sdk direct RGB commands
    pub const SINGLE_COLOR: u8 = 0x1E;
    pub const SINGLE_RESET: u8 = 0x1F;
    pub const RESET_ALL: u8 = 0x20;
    pub const COLOR_INIT: u8 = 0x21;

    // Forbidden destructive / bootloader commands (blocked by PacketValidator)
    pub const FORBIDDEN_RESET_TO_BOOTLOADER: u8 = 0x02;
    pub const FORBIDDEN_KEYS_OFF: u8 = 0x15;
    pub const FORBIDDEN_SOFT_RESET: u8 = 0x19;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WootingProductProfile {
    pub base_pid: u16,
    pub model_name: &'static str,
    pub rows: u8,
    pub cols: u8,
    pub has_rgb: bool,
}

impl WootingProductProfile {
    pub const fn key_count(self) -> u16 {
        (self.rows as u16) * (self.cols as u16)
    }
}

/// Official `WootingKb/wooting-rgb-sdk` Hall Effect keyboard base PIDs.
/// Each board enumerates with `base_pid | 0`, `| 1`, or `| 2` depending on gamepad mode.
pub const WOOTING_PRODUCTS: &[WootingProductProfile] = &[
    WootingProductProfile {
        base_pid: 0x1220,
        model_name: "Wooting Two HE",
        rows: 6,
        cols: 21,
        has_rgb: true,
    },
    WootingProductProfile {
        base_pid: 0x1230,
        model_name: "Wooting Two HE (ARM)",
        rows: 6,
        cols: 21,
        has_rgb: true,
    },
    WootingProductProfile {
        base_pid: 0x1300,
        model_name: "Wooting 60HE",
        rows: 6,
        cols: 14,
        has_rgb: true,
    },
    WootingProductProfile {
        base_pid: 0x1310,
        model_name: "Wooting 60HE (ARM)",
        rows: 6,
        cols: 14,
        has_rgb: true,
    },
    WootingProductProfile {
        base_pid: 0x1320,
        model_name: "Wooting 60HE+",
        rows: 6,
        cols: 14,
        has_rgb: true,
    },
    WootingProductProfile {
        base_pid: 0x1340,
        model_name: "Wooting 60HE V2",
        rows: 6,
        cols: 14,
        has_rgb: true,
    },
    WootingProductProfile {
        base_pid: 0x1400,
        model_name: "Wooting 80HE",
        rows: 6,
        cols: 17,
        has_rgb: true,
    },
    WootingProductProfile {
        base_pid: 0x1500,
        model_name: "Wooting UwU",
        rows: 5,
        cols: 7,
        has_rgb: false,
    },
    WootingProductProfile {
        base_pid: 0x1510,
        model_name: "Wooting UwU RGB",
        rows: 5,
        cols: 7,
        has_rgb: true,
    },
];

pub fn lookup_product(pid: u16) -> Option<WootingProductProfile> {
    let base = pid & 0xFFF0;
    let alt = pid & 0x000F;
    if alt > 2 {
        return None;
    }
    WOOTING_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.base_pid == base)
}

/// Encodes an 8-byte Wooting Feature command (`[report_idx, magic0, 0xDA, cmd, p3, p2, p1, p0]`).
pub const fn encode_command(
    command_id: u8,
    p0: u8,
    p1: u8,
    p2: u8,
    p3: u8,
    multi_report: bool,
) -> [u8; WOOTING_COMMAND_SIZE] {
    [
        if multi_report { 1 } else { 0 },
        if multi_report {
            WOOTING_MAGIC_MULTI
        } else {
            WOOTING_MAGIC_SINGLE
        },
        WOOTING_MAGIC_WORD_1,
        command_id,
        p3,
        p2,
        p1,
        p0,
    ]
}

/// Strict pre-transmission validator enforcing 8-byte Wooting Feature frames, valid magic,
/// and blocking bootloader/reset opcodes (`0x02`, `0x15`, `0x19`).
pub fn wooting_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![WOOTING_COMMAND_SIZE], vec![0x00, 0x01]).with_checksum(
        |kind, report| {
            if kind != ReportKind::Feature {
                return Err(format!(
                    "Wooting commands require Feature reports, got {kind}"
                ));
            }
            if (report[1] != WOOTING_MAGIC_SINGLE && report[1] != WOOTING_MAGIC_MULTI)
                || report[2] != WOOTING_MAGIC_WORD_1
            {
                return Err(format!(
                    "invalid Wooting magic bytes 0x{:02X} 0x{:02X}",
                    report[1], report[2]
                ));
            }
            let cmd_id = report[3];
            if matches!(
                cmd_id,
                cmd::FORBIDDEN_RESET_TO_BOOTLOADER
                    | cmd::FORBIDDEN_KEYS_OFF
                    | cmd::FORBIDDEN_SOFT_RESET
            ) {
                return Err(format!(
                    "blocked destructive/bootloader Wooting command 0x{cmd_id:02X}"
                ));
            }
            Ok(())
        },
    )
}

/// Strips optional leading `hidapi` report ID byte (`0x00`/`0x01`) from a Wooting input reply.
pub fn normalize_reply(raw: &[u8]) -> Option<&[u8]> {
    if raw.len() >= 4
        && (raw[0] == WOOTING_MAGIC_SINGLE || raw[0] == WOOTING_MAGIC_MULTI)
        && raw[1] == WOOTING_MAGIC_WORD_1
    {
        return Some(raw);
    }
    if raw.len() >= 5
        && (raw[1] == WOOTING_MAGIC_SINGLE || raw[1] == WOOTING_MAGIC_MULTI)
        && raw[2] == WOOTING_MAGIC_WORD_1
    {
        return Some(&raw[1..]);
    }
    None
}

/// Decodes firmware version (`v{major}.{minor}.{patch}`) from a `GetVersion` (`0x01`) reply.
pub fn decode_version(raw: &[u8]) -> Option<String> {
    let reply = normalize_reply(raw)?;
    if reply.len() < 9 || reply[2] != cmd::GET_VERSION {
        return None;
    }
    let (major, minor, patch) = (reply[6], reply[7], reply[8]);
    if major == 0 && minor == 0 && patch == 0 {
        return None;
    }
    Some(format!("v{major}.{minor}.{patch}"))
}

/// Decodes physical board layout name from `GetDeviceConfig` (`0x13`).
pub fn decode_layout(raw: &[u8]) -> Option<&'static str> {
    let reply = normalize_reply(raw)?;
    let layout = *reply.get(9)?;
    match layout {
        0 => Some("ANSI"),
        1 => Some("ISO"),
        2 => Some("JIS"),
        3 => Some("ANSI Split"),
        4 => Some("ISO Split"),
        _ => None,
    }
}

/// Decodes active onboard profile index (`0..=3`) from `GetCurrentProfileIndex` (`0x0B`).
pub fn decode_active_profile(raw: &[u8]) -> Option<u8> {
    let reply = normalize_reply(raw)?;
    if reply.len() < 5 || reply[2] != cmd::GET_CURRENT_PROFILE_INDEX {
        return None;
    }
    Some(reply[4])
}

/// Minimal wire-format protobuf varint/length-delimited reader for Wooting profile replies.
fn read_varint(data: &[u8], mut idx: usize) -> Option<(u64, usize)> {
    let mut result = 0u64;
    let mut shift = 0u32;
    while idx < data.len() && shift < 64 {
        let byte = data[idx];
        idx += 1;
        result |= u64::from(byte & 0x7F) << shift;
        if (byte & 0x80) == 0 {
            return Some((result, idx));
        }
        shift += 7;
    }
    None
}

fn find_nested_field_bytes(data: &[u8], target_field: u64) -> Option<&[u8]> {
    let mut idx = 0usize;
    while idx < data.len() {
        let (tag, next) = read_varint(data, idx)?;
        if tag == 0 {
            break;
        }
        idx = next;
        let field = tag >> 3;
        let wire = tag & 7;
        match wire {
            0 => {
                let (_, n) = read_varint(data, idx)?;
                idx = n;
            }
            2 => {
                let (len, n) = read_varint(data, idx)?;
                idx = n;
                let len = usize::try_from(len).ok()?;
                if idx + len > data.len() {
                    break;
                }
                if field == target_field {
                    return Some(&data[idx..idx + len]);
                }
                idx += len;
            }
            5 => {
                if idx + 4 > data.len() {
                    break;
                }
                idx += 4;
            }
            1 => {
                if idx + 8 > data.len() {
                    break;
                }
                idx += 8;
            }
            _ => break,
        }
    }
    None
}

fn find_varint_field(data: &[u8], target_field: u64) -> Option<u64> {
    let mut idx = 0usize;
    while idx < data.len() {
        let (tag, next) = read_varint(data, idx)?;
        if tag == 0 {
            break;
        }
        idx = next;
        let field = tag >> 3;
        let wire = tag & 7;
        match wire {
            0 => {
                let (val, n) = read_varint(data, idx)?;
                if field == target_field {
                    return Some(val);
                }
                idx = n;
            }
            2 => {
                let (len, n) = read_varint(data, idx)?;
                idx = n;
                let len = usize::try_from(len).ok()?;
                if idx + len > data.len() {
                    break;
                }
                idx += len;
            }
            5 => {
                if idx + 4 > data.len() {
                    break;
                }
                idx += 4;
            }
            1 => {
                if idx + 8 > data.len() {
                    break;
                }
                idx += 8;
            }
            _ => break,
        }
    }
    None
}

/// Decodes global actuation travel from a `GetKeyboardProfile` (`0x27`) reply.
///
/// Confirmed on Wooting 60HE+: `raw = 16384 + mm * 4096` (1.00 mm = 20480, 0.50 mm = 18432).
pub fn decode_profile_actuation(raw: &[u8]) -> Option<TravelHundredthsMm> {
    let reply = normalize_reply(raw)?;
    if reply.len() < 6 || reply[2] != cmd::GET_KEYBOARD_PROFILE {
        return None;
    }
    let body_len = usize::from(u16::from_le_bytes([reply[4], reply[5]]));
    let body = reply.get(6..6 + body_len)?;
    let nested = find_nested_field_bytes(body, 1)?;
    let raw_val = u32::try_from(find_varint_field(nested, 1)?).ok()?;
    if raw_val < ACTUATION_ZERO {
        return None;
    }
    let delta = raw_val - ACTUATION_ZERO;
    let hundredths = (u64::from(delta) * 100 + u64::from(ACTUATION_UNITS_PER_MM) / 2)
        / u64::from(ACTUATION_UNITS_PER_MM);
    let clamped = u16::try_from(hundredths.clamp(10, 400)).ok()?;
    Some(TravelHundredthsMm::from_hundredths(clamped))
}

/// Converts 24-bit RGB to 16-bit RGB565 as used by `WootingKb/wooting-rgb-sdk`.
pub const fn encode_rgb565(color: RgbColor) -> u16 {
    let r = ((color.r as u16) & 0xF8) << 8;
    let g = ((color.g as u16) & 0xFC) << 3;
    let b = (color.b as u16) >> 3;
    r | g | b
}

/// Encodes `WOOTING_SINGLE_COLOR_COMMAND` (`0x1E`) for one matrix position `(row, col)`.
pub const fn encode_single_color(
    row: u8,
    col: u8,
    color: RgbColor,
    multi_report: bool,
) -> [u8; WOOTING_COMMAND_SIZE] {
    let matrix_id = ((row & 0x07) << 5) | (col & 0x1F);
    let rgb565 = encode_rgb565(color);
    let lo = (rgb565 & 0xFF) as u8;
    let hi = (rgb565 >> 8) as u8;
    encode_command(cmd::SINGLE_COLOR, hi, lo, matrix_id, 0, multi_report)
}
