//! Extensible codec seam for newer Lamzu Aurora models (`0x373E` / `0x37B0`) using
//! the 64-byte CompX page/command feature-report protocol (`OpenMouse-Project/mouse-protocol` `src/compx/codec.ts`).

use agnoperiph_core::{CoreError, CoreResult, DpiStage, DpiValue, LiftOffDistance};

/// Known USB Vendor IDs for newer Lamzu Aurora generation mice.
pub const LAMZU_AURORA_VIDS: &[u16] = &[0x373E, 0x37B0];

/// 64-byte CompX page/command packet framing constants.
pub const AURORA_REPORT_ID: u8 = 0x00;
pub const AURORA_PACKET_LEN: usize = 64;
pub const AURORA_HEADER_LEN: usize = 6;

/// Status bytes returned in byte 0 of an Aurora page/command packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AuroraStatus {
    Request = 0x00,
    Pending = 0xA0,
    Ok = 0xA1,
    Unsupported = 0xA2,
    Busy = 0xA3,
}

impl AuroraStatus {
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0x00 => Some(Self::Request),
            0xA0 => Some(Self::Pending),
            0xA1 => Some(Self::Ok),
            0xA2 => Some(Self::Unsupported),
            0xA3 => Some(Self::Busy),
            _ => None,
        }
    }
}

/// Encodes a 64-byte Aurora page/command request payload (excluding leading HID Report ID `0x00`).
pub fn encode_page_request(
    target: u8,
    length: u8,
    page: u8,
    command: u8,
    args: &[u8],
) -> [u8; AURORA_PACKET_LEN] {
    let mut packet = [0u8; AURORA_PACKET_LEN];
    packet[0] = AuroraStatus::Request as u8;
    packet[2] = target;
    packet[3] = length;
    packet[4] = page;
    packet[5] = command;
    let copy_len = args.len().min(AURORA_PACKET_LEN - AURORA_HEADER_LEN);
    packet[AURORA_HEADER_LEN..AURORA_HEADER_LEN + copy_len].copy_from_slice(&args[..copy_len]);
    packet
}

/// Decodes DPI stages from an Aurora page payload (`[_, count, x0_hi, x0_lo, y0_hi, y0_lo, ...]`).
pub fn decode_dpi_stages(payload: &[u8]) -> CoreResult<Vec<DpiStage>> {
    if payload.len() < 2 {
        return Err(CoreError::Protocol(
            "Aurora DPI payload too short".to_owned(),
        ));
    }
    let count = usize::from(payload[1]);
    let mut stages = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = 2 + idx * 4;
        if offset + 3 >= payload.len() {
            break;
        }
        let x = u32::from(u16::from_be_bytes([payload[offset], payload[offset + 1]]));
        let y = u32::from(u16::from_be_bytes([
            payload[offset + 2],
            payload[offset + 3],
        ]));
        let stage_ix = u8::try_from(idx).unwrap_or(u8::MAX);
        let mut stage = DpiStage::new(stage_ix, DpiValue::new(x)?);
        if y != x {
            stage = stage.with_y(DpiValue::new(y)?);
        }
        stages.push(stage);
    }
    Ok(stages)
}

/// Decodes an Aurora lift-off byte (`bit 7`: tenths of mm; otherwise whole mm).
pub fn decode_lift_off(raw: u8) -> Option<LiftOffDistance> {
    if raw == 0 {
        return None;
    }
    if (raw & 0x80) != 0 {
        Some(LiftOffDistance::TenthsMm(raw & 0x7F))
    } else {
        Some(LiftOffDistance::TenthsMm(raw.saturating_mul(10)))
    }
}

/// Formats an Aurora firmware payload (`payload[2].payload[3]`).
pub fn decode_firmware(payload: &[u8]) -> Option<String> {
    if payload.len() < 4 {
        return None;
    }
    Some(format!("v{}.{}", payload[2], payload[3]))
}
