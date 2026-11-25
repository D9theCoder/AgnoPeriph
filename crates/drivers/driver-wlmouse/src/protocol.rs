//! WLmouse CompX 64-byte page/command HID Feature report codec.
//!
//! Verified against `docs/protocols/wlmouse-hid.md` and
//! `OpenMouse-Project/mouse-protocol` (`src/compx/codec.ts`, `src/wlmouse/index.ts`, `src/drivers/wlmouse/hid.ts`).

use agnoperiph_core::{
    BatteryStatus, ChargingState, ConnectionKind, CoreError, CoreResult, DpiStage, DpiValue,
    LiftOffDistance, PollingRateHz,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub const WLMOUSE_VID: u16 = 0x36A7;
pub const WLMOUSE_REPORT_ID: u8 = 0x00;
/// 64-byte payload + 1 leading Report ID byte (`0x00`) for `hidapi`.
pub const WLMOUSE_PACKET_LEN: usize = 64;
pub const WLMOUSE_HID_BUFFER_LEN: usize = WLMOUSE_PACKET_LEN + 1;
pub const WLMOUSE_HEADER_LEN: usize = 6;

pub const DPI_MIN: u32 = 50;
pub const DPI_MAX: u32 = 30_000;
pub const DPI_STEP: u32 = 50;
pub const MAX_DPI_STAGES: u8 = 6;
pub const MAX_DEBOUNCE_MS: u8 = 15;
pub const PROFILE_COUNT: u8 = 4;

pub mod status {
    pub const REQUEST: u8 = 0x00;
    pub const PENDING: u8 = 0xA0;
    pub const OK: u8 = 0xA1;
    pub const UNSUPPORTED: u8 = 0xA2;
    pub const BUSY: u8 = 0xA3;
}

pub mod target {
    pub const DONGLE: u8 = 0x00;
    pub const PAIRING: u8 = 0x01;
    pub const MOUSE: u8 = 0x02;
}

pub mod page {
    pub const DEVICE: u8 = 0x00;
    pub const PROFILE: u8 = 0x01;
    pub const BUTTONS: u8 = 0x03;
}

pub mod cmd {
    pub const READ_FIRMWARE: u8 = 0x81;
    pub const READ_SERIAL: u8 = 0x82;
    pub const READ_BATTERY: u8 = 0x83;
    pub const READ_ACTIVE_PROFILE: u8 = 0x85;
    pub const WRITE_ACTIVE_PROFILE: u8 = 0x05;
    pub const READ_DEBOUNCE: u8 = 0x88;
    pub const WRITE_DEBOUNCE: u8 = 0x08;
    pub const READ_PAIRED_PRODUCT: u8 = 0x8B;

    pub const READ_POLLING_RATE: u8 = 0x80;
    pub const WRITE_POLLING_RATE: u8 = 0x00;
    pub const READ_DPI_STAGES: u8 = 0x81;
    pub const WRITE_DPI_STAGES: u8 = 0x01;
    pub const READ_ACTIVE_STAGE: u8 = 0x82;
    pub const WRITE_ACTIVE_STAGE: u8 = 0x02;
    pub const READ_ANGLE_SNAPPING: u8 = 0x84;
    pub const WRITE_ANGLE_SNAPPING: u8 = 0x04;
    pub const READ_LOD: u8 = 0x88;
    pub const WRITE_LOD: u8 = 0x08;
    pub const READ_MOTION_SYNC: u8 = 0x89;
    pub const WRITE_MOTION_SYNC: u8 = 0x09;
    pub const READ_RIPPLE_CONTROL: u8 = 0x8A;
    pub const WRITE_RIPPLE_CONTROL: u8 = 0x0A;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WLmouseProductProfile {
    pub pid: u16,
    pub model_name: &'static str,
    pub connection: ConnectionKind,
    pub max_polling_hz: u16,
}

/// Verified WLmouse CompX 64-byte PID catalog (excludes `0xA887` Beast X 4K).
pub const WLMOUSE_PRODUCTS: &[WLmouseProductProfile] = &[
    WLmouseProductProfile {
        pid: 0xA860,
        model_name: "WLmouse Beast G (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA861,
        model_name: "WLmouse Beast G (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA863,
        model_name: "WLmouse Huan (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA864,
        model_name: "WLmouse Huan (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA866,
        model_name: "WLmouse Beast Miao (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA867,
        model_name: "WLmouse Beast Miao (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA868,
        model_name: "WLmouse Beast Mini Pro (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA869,
        model_name: "WLmouse Beast Mini Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA870,
        model_name: "WLmouse Beast X Pro (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA871,
        model_name: "WLmouse Beast X Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA872,
        model_name: "WLmouse Strider (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA873,
        model_name: "WLmouse Strider (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA874,
        model_name: "WLmouse Ying (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA875,
        model_name: "WLmouse Ying (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA878,
        model_name: "WLmouse Sword X (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA879,
        model_name: "WLmouse Sword X (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA880,
        model_name: "WLmouse Beast Max (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA881,
        model_name: "WLmouse Beast Max (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA882,
        model_name: "WLmouse 1K Receiver",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 1000,
    },
    WLmouseProductProfile {
        pid: 0xA883,
        model_name: "WLmouse Beast X (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA884,
        model_name: "WLmouse Beast X (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA885,
        model_name: "WLmouse Beast Mini (Wireless)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        max_polling_hz: 8000,
    },
    WLmouseProductProfile {
        pid: 0xA886,
        model_name: "WLmouse Beast Mini (Wired)",
        connection: ConnectionKind::WiredUsb,
        max_polling_hz: 8000,
    },
];

pub fn lookup_product(pid: u16) -> Option<WLmouseProductProfile> {
    WLMOUSE_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

/// Encodes a 65-byte `hidapi` Feature Report (`[0x00, status=0x00, 0x00, target, length, page, cmd, args...]`).
pub fn encode_request(
    tgt: u8,
    length: u8,
    pg: u8,
    command: u8,
    args: &[u8],
) -> CoreResult<[u8; WLMOUSE_HID_BUFFER_LEN]> {
    if args.len() > WLMOUSE_PACKET_LEN - WLMOUSE_HEADER_LEN {
        return Err(CoreError::Validation(
            "WLmouse command arguments exceed 58 bytes".to_owned(),
        ));
    }
    let mut buf = [0u8; WLMOUSE_HID_BUFFER_LEN];
    buf[0] = WLMOUSE_REPORT_ID;
    let pkt = &mut buf[1..];
    pkt[0] = status::REQUEST;
    pkt[1] = 0x00;
    pkt[2] = tgt;
    pkt[3] = length;
    pkt[4] = pg;
    pkt[5] = command;
    pkt[WLMOUSE_HEADER_LEN..WLMOUSE_HEADER_LEN + args.len()].copy_from_slice(args);
    Ok(buf)
}

/// Strict validator enforcing 65-byte Feature reports with Report ID `0x00` and request status `0x00`.
pub fn wlmouse_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![WLMOUSE_HID_BUFFER_LEN], vec![WLMOUSE_REPORT_ID]).with_checksum(
        |kind, report| {
            if kind != ReportKind::Feature {
                return Err(format!("WLmouse requires Feature reports, got {kind}"));
            }
            if report[1] != status::REQUEST {
                return Err(format!(
                    "WLmouse outgoing status byte must be 0x00, got 0x{:02X}",
                    report[1]
                ));
            }
            Ok(())
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WLmouseReply {
    Ok(Vec<u8>),
    Pending,
    Unsupported,
    NoMatch,
}

/// Decodes a 64-byte (or 65-byte with leading `0x00`) WLmouse Feature reply.
pub fn decode_reply(raw: &[u8], expected_page: u8, expected_cmd: u8) -> WLmouseReply {
    for offset in [0usize, 1usize] {
        if raw.len() < offset + WLMOUSE_HEADER_LEN {
            continue;
        }
        if raw[offset + 4] != expected_page || raw[offset + 5] != expected_cmd {
            continue;
        }
        match raw[offset] {
            status::OK => {
                let start = offset + WLMOUSE_HEADER_LEN;
                let max_avail = raw.len().saturating_sub(start);
                let declared = usize::from(raw[offset + 3]).min(max_avail);
                return WLmouseReply::Ok(raw[start..start + declared].to_vec());
            }
            status::UNSUPPORTED => return WLmouseReply::Unsupported,
            status::PENDING | status::BUSY => return WLmouseReply::Pending,
            _ => {}
        }
    }
    WLmouseReply::NoMatch
}

pub fn decode_firmware(payload: &[u8]) -> Option<String> {
    if payload.len() < 4 {
        return None;
    }
    Some(format!("v{}.{}", payload[2], payload[3]))
}

pub fn decode_battery(payload: &[u8]) -> BatteryStatus {
    let charging = payload.first().copied().unwrap_or(0) == 1;
    let pct = payload.get(1).copied().filter(|&p| p <= 100);
    let state = if charging {
        ChargingState::Charging
    } else {
        ChargingState::Discharging
    };
    BatteryStatus::new(pct, None, state, pct.is_some())
}

pub fn decode_dpi_stages(payload: &[u8]) -> CoreResult<Vec<DpiStage>> {
    if payload.len() < 2 {
        return Err(CoreError::Protocol(
            "WLmouse DPI stage payload too short".to_owned(),
        ));
    }
    let count = usize::from(payload[1]).min(usize::from(MAX_DPI_STAGES));
    let mut stages = Vec::with_capacity(count);
    for ix in 0..count {
        let offset = 2 + ix * 4;
        if offset + 3 >= payload.len() {
            break;
        }
        let x = u32::from(u16::from_be_bytes([payload[offset], payload[offset + 1]]));
        let y = u32::from(u16::from_be_bytes([
            payload[offset + 2],
            payload[offset + 3],
        ]));
        let mut stage = DpiStage::new(u8::try_from(ix).unwrap_or(0), DpiValue::new(x)?);
        if y > 0 && y != x {
            stage = stage.with_y(DpiValue::new(y)?);
        }
        stages.push(stage);
    }
    if stages.is_empty() {
        return Err(CoreError::Protocol(
            "WLmouse reported zero DPI stages".to_owned(),
        ));
    }
    Ok(stages)
}

pub fn encode_dpi_stages(profile_1based: u8, stages: &[DpiStage]) -> CoreResult<Vec<u8>> {
    if stages.is_empty() || stages.len() > usize::from(MAX_DPI_STAGES) {
        return Err(CoreError::OutOfRange {
            field: "dpi_stages",
            actual: stages.len().to_string(),
            expected: format!("1..={MAX_DPI_STAGES}"),
        });
    }
    let mut out = Vec::with_capacity(2 + stages.len() * 4);
    out.push(profile_1based);
    out.push(u8::try_from(stages.len()).unwrap_or(1));
    for stage in stages {
        let x = u16::try_from(stage.x.value())
            .map_err(|_| CoreError::Validation("WLmouse DPI X exceeds u16".to_owned()))?;
        let y = u16::try_from(stage.effective_y().value())
            .map_err(|_| CoreError::Validation("WLmouse DPI Y exceeds u16".to_owned()))?;
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
    }
    Ok(out)
}

pub fn decode_polling_rate(code: u8) -> CoreResult<PollingRateHz> {
    match code {
        0x08 => Ok(PollingRateHz::Hz125),
        0x04 => Ok(PollingRateHz::Hz250),
        0x02 => Ok(PollingRateHz::Hz500),
        0x01 | 0x10 => Ok(PollingRateHz::Hz1000),
        0x20 => Ok(PollingRateHz::Hz2000),
        0x40 => Ok(PollingRateHz::Hz4000),
        0x80 => Ok(PollingRateHz::Hz8000),
        other => Err(CoreError::Protocol(format!(
            "unknown WLmouse polling code 0x{other:02X}"
        ))),
    }
}

pub fn encode_polling_rate(rate: PollingRateHz) -> u8 {
    match rate {
        PollingRateHz::Hz125 => 0x08,
        PollingRateHz::Hz250 => 0x04,
        PollingRateHz::Hz500 => 0x02,
        PollingRateHz::Hz1000 => 0x01,
        PollingRateHz::Hz2000 => 0x20,
        PollingRateHz::Hz4000 => 0x40,
        PollingRateHz::Hz8000 => 0x80,
    }
}

pub fn decode_lod(raw: u8) -> Option<LiftOffDistance> {
    match raw {
        0 => None,
        v if (v & 0x80) != 0 => Some(LiftOffDistance::TenthsMm(v & 0x7F)),
        1 => Some(LiftOffDistance::TenthsMm(10)),
        2 => Some(LiftOffDistance::TenthsMm(20)),
        _ => None,
    }
}

pub fn encode_lod(lod: LiftOffDistance) -> CoreResult<u8> {
    match lod {
        LiftOffDistance::Low | LiftOffDistance::TenthsMm(7) => Ok(0x87),
        LiftOffDistance::Medium | LiftOffDistance::TenthsMm(10) => Ok(0x01),
        LiftOffDistance::High | LiftOffDistance::TenthsMm(20) => Ok(0x02),
        other @ LiftOffDistance::TenthsMm(_) => Err(CoreError::OutOfRange {
            field: "lod",
            actual: other.to_string(),
            expected: "0.7 mm, 1.0 mm, or 2.0 mm".to_owned(),
        }),
    }
}
