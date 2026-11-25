//! Endgame Gear OP1/XM2 8K & 4K Wireless HID Feature report codec.
//!
//! Verified against `docs/protocols/endgame-gear-hid.md` and
//! `OpenMouse-Project/mouse-protocol` (`src/endgame-gear/op1.ts`, `src/drivers/endgame/egg-op1-hid.ts`).

use agnoperiph_core::{
    ConnectionKind, CoreError, CoreResult, DpiStage, DpiValue, LiftOffDistance, PollingRateHz,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub const EGG_VID: u16 = 0x3367;
pub const EGG_4K_DONGLE_PID: u16 = 0x1970;

pub const EGG_CONFIG_SIZE: usize = 1041;
pub const EGG_COMMAND_SIZE: usize = 64;
pub const MAX_CPI_STAGES: u8 = 4;

pub mod report_id {
    pub const CONFIG: u8 = 0xA0;
    pub const COMMAND: u8 = 0xA1;
}

pub mod op {
    pub const FIRMWARE: u8 = 0x02;
    pub const MOUSE_INFO: u8 = 0x0E;
    pub const STORE: u8 = 0x11;
    pub const LOAD: u8 = 0x12;
    pub const WRITE_SENSOR: u8 = 0x14;
}

pub mod status {
    pub const OK: u8 = 0x01;
    pub const BUSY: u8 = 0x03;
    pub const REJECTED: u8 = 0x07;
    pub const RF_SLEEP: u8 = 0x08;
}

pub mod offset {
    pub const POLLING_DIVIDER: usize = 21;
    pub const LOD: usize = 25;
    pub const ANGLE_SNAPPING: usize = 26;
    pub const RIPPLE_CONTROL: usize = 27;
    pub const MOTION_SYNC: usize = 28;
    pub const CPI_LEVELS: usize = 30;
    pub const FIRST_CPI_SPLIT: usize = 51;
    pub const GLASS_MODE: usize = 127;
    pub const POWER_SAVE_POLLING: u8 = 0x80;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EggConfigFamily {
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EggProductProfile {
    pub pid: u16,
    pub model_name: &'static str,
    pub connection: ConnectionKind,
    pub config_family: EggConfigFamily,
    pub wireless_4k: bool,
    pub cpi_min: u32,
    pub cpi_max: u32,
    pub cpi_step: u32,
    pub max_polling_hz: u16,
    pub motion_sync_at_8k: bool,
}

pub const EGG_PRODUCTS: &[EggProductProfile] = &[
    EggProductProfile {
        pid: 0x1964,
        model_name: "Endgame Gear OP1 8K",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V1,
        wireless_4k: false,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step: 50,
        max_polling_hz: 8000,
        motion_sync_at_8k: false,
    },
    EggProductProfile {
        pid: 0x1966,
        model_name: "Endgame Gear XM2 8K",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V1,
        wireless_4k: false,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step: 50,
        max_polling_hz: 8000,
        motion_sync_at_8k: false,
    },
    EggProductProfile {
        pid: 0x1976,
        model_name: "Endgame Gear OP1 8K Purple Frost",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V1,
        wireless_4k: false,
        cpi_min: 50,
        cpi_max: 30_000,
        cpi_step: 50,
        max_polling_hz: 8000,
        motion_sync_at_8k: true,
    },
    EggProductProfile {
        pid: 0x1978,
        model_name: "Endgame Gear OP1 8K v2",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V2,
        wireless_4k: false,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step: 10,
        max_polling_hz: 8000,
        motion_sync_at_8k: true,
    },
    EggProductProfile {
        pid: 0x1980,
        model_name: "Endgame Gear XM2 8K v2",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V2,
        wireless_4k: false,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step: 10,
        max_polling_hz: 8000,
        motion_sync_at_8k: true,
    },
    EggProductProfile {
        pid: 0x1972,
        model_name: "Endgame Gear OP1w 4K (Wired)",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V1,
        wireless_4k: true,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step: 50,
        max_polling_hz: 4000,
        motion_sync_at_8k: true,
    },
    EggProductProfile {
        pid: 0x1968,
        model_name: "Endgame Gear XM2w 4K (Wired)",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V1,
        wireless_4k: true,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step: 50,
        max_polling_hz: 4000,
        motion_sync_at_8k: true,
    },
    EggProductProfile {
        pid: 0x1984,
        model_name: "Endgame Gear OP1w 4K v2 (Wired)",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V2,
        wireless_4k: true,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step: 10,
        max_polling_hz: 4000,
        motion_sync_at_8k: true,
    },
    EggProductProfile {
        pid: 0x1982,
        model_name: "Endgame Gear XM2w 4K v2 (Wired)",
        connection: ConnectionKind::WiredUsb,
        config_family: EggConfigFamily::V2,
        wireless_4k: true,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step: 10,
        max_polling_hz: 4000,
        motion_sync_at_8k: true,
    },
    EggProductProfile {
        pid: EGG_4K_DONGLE_PID,
        model_name: "Endgame Gear OP1w/XM2w 4K Dongle",
        connection: ConnectionKind::WirelessDongle24Ghz,
        config_family: EggConfigFamily::V2,
        wireless_4k: true,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step: 10,
        max_polling_hz: 4000,
        motion_sync_at_8k: true,
    },
];

pub fn lookup_product(pid: u16) -> Option<EggProductProfile> {
    EGG_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

/// Builds a 64-byte `0xA1` command feature report.
pub fn encode_command(operation: u8, header: &[u8], payload: &[u8]) -> [u8; EGG_COMMAND_SIZE] {
    let mut buf = [0u8; EGG_COMMAND_SIZE];
    buf[0] = report_id::COMMAND;
    buf[1] = operation;
    let hlen = header.len().min(14);
    buf[2..2 + hlen].copy_from_slice(&header[..hlen]);
    if !payload.is_empty() {
        let plen = payload.len().min(EGG_COMMAND_SIZE - 16);
        buf[16..16 + plen].copy_from_slice(&payload[..plen]);
    }
    buf
}

/// Builds the 28-byte `WriteSensor` (`0x14`) block write payload for OP1w/XM2w 4K mice.
pub fn encode_sensor_block_4k(config: &[u8; EGG_CONFIG_SIZE]) -> [u8; EGG_COMMAND_SIZE] {
    let at = |off: usize| config[16 + off];
    let mut sensor = [0u8; 28];
    sensor[0] = at(0x07);
    sensor[1] = at(0x08);
    sensor[2] = at(0x09);
    sensor[3] = at(0x0A);
    sensor[4] = at(0x0B);
    sensor[5] = at(0x01);
    sensor[6] = at(0x0E);
    sensor[7] = at(0x0D);
    sensor[8..28].copy_from_slice(&config[16 + 0x23..16 + 0x37]);
    encode_command(op::WRITE_SENSOR, &[0x0F, 28, 0, 0, 0], &sensor)
}

/// Strict validator allowing only 64-byte `0xA1` command and 1041-byte `0xA0` config Feature reports.
pub fn egg_validator() -> FixedReportValidator {
    FixedReportValidator::new(
        vec![EGG_COMMAND_SIZE, EGG_CONFIG_SIZE],
        vec![report_id::COMMAND, report_id::CONFIG],
    )
    .with_checksum(|kind, report| {
        if kind != ReportKind::Feature {
            return Err(format!("Endgame Gear requires Feature reports, got {kind}"));
        }
        if report[0] == report_id::COMMAND && report.len() != EGG_COMMAND_SIZE {
            return Err("Endgame Gear 0xA1 report must be 64 bytes".to_owned());
        }
        if report[0] == report_id::CONFIG && report.len() != EGG_CONFIG_SIZE {
            return Err("Endgame Gear 0xA0 config report must be 1041 bytes".to_owned());
        }
        Ok(())
    })
}

fn decode_bcd(value: u8) -> Option<u8> {
    let hi = value >> 4;
    let lo = value & 0x0F;
    (hi <= 9 && lo <= 9).then_some(hi * 10 + lo)
}

/// Formats Endgame Gear BCD `[patch, major]` firmware version bytes (`V{major}.{patch:02}`).
pub fn decode_firmware(bytes: &[u8]) -> Option<String> {
    for &off in &[17usize, 16, 18, 2, 4, 6] {
        if off + 1 >= bytes.len() {
            continue;
        }
        if let (Some(patch), Some(major)) = (decode_bcd(bytes[off]), decode_bcd(bytes[off + 1]))
            && major > 0
        {
            return Some(format!("V{major}.{patch:02}"));
        }
    }
    None
}

pub fn decode_polling_rate(divider: u8, wireless_4k: bool) -> CoreResult<PollingRateHz> {
    if divider == 0 {
        return Err(CoreError::Protocol(
            "Endgame Gear polling divider was 0".to_owned(),
        ));
    }
    if wireless_4k && divider == offset::POWER_SAVE_POLLING {
        return Ok(PollingRateHz::Hz1000);
    }
    PollingRateHz::from_hz(8000 / u16::from(divider))
}

pub fn encode_polling_divider(rate: PollingRateHz) -> u8 {
    u8::try_from(8000 / rate.as_hz()).unwrap_or(8)
}

pub fn decode_cpi_stages(config: &[u8]) -> CoreResult<Vec<DpiStage>> {
    if config.len() < offset::FIRST_CPI_SPLIT + 20 {
        return Err(CoreError::Protocol(
            "Endgame Gear config blob too short".to_owned(),
        ));
    }
    let count = usize::from(config[offset::CPI_LEVELS].clamp(1, MAX_CPI_STAGES));
    let mut stages = Vec::with_capacity(count);
    for ix in 0..count {
        let base = offset::FIRST_CPI_SPLIT + ix * 5;
        let x = u32::from(u16::from_le_bytes([config[base + 1], config[base + 2]]));
        let y = u32::from(u16::from_le_bytes([config[base + 3], config[base + 4]]));
        let mut stage = DpiStage::new(u8::try_from(ix).unwrap_or(0), DpiValue::new(x)?);
        if y > 0 && y != x {
            stage = stage.with_y(DpiValue::new(y)?);
        }
        stages.push(stage);
    }
    Ok(stages)
}

pub fn write_cpi_stages(config: &mut [u8; EGG_CONFIG_SIZE], stages: &[DpiStage]) -> CoreResult<()> {
    if stages.is_empty() || stages.len() > usize::from(MAX_CPI_STAGES) {
        return Err(CoreError::OutOfRange {
            field: "dpi_stages",
            actual: stages.len().to_string(),
            expected: format!("1..={MAX_CPI_STAGES}"),
        });
    }
    config[offset::CPI_LEVELS] = u8::try_from(stages.len()).unwrap_or(1);
    for (ix, stage) in stages.iter().enumerate() {
        let base = offset::FIRST_CPI_SPLIT + ix * 5;
        let x = u16::try_from(stage.x.value())
            .map_err(|_| CoreError::Validation("Endgame Gear CPI X exceeds u16".to_owned()))?;
        let y = u16::try_from(stage.effective_y().value())
            .map_err(|_| CoreError::Validation("Endgame Gear CPI Y exceeds u16".to_owned()))?;
        config[base] = u8::from(x != y);
        let [xl, xh] = x.to_le_bytes();
        let [yl, yh] = y.to_le_bytes();
        config[base + 1] = xl;
        config[base + 2] = xh;
        config[base + 3] = yl;
        config[base + 4] = yh;
    }
    Ok(())
}

pub fn decode_lod(raw: u8, profile: EggProductProfile) -> Option<LiftOffDistance> {
    if profile.wireless_4k && profile.config_family == EggConfigFamily::V1 {
        return match raw {
            1 => Some(LiftOffDistance::TenthsMm(10)),
            2 => Some(LiftOffDistance::TenthsMm(20)),
            _ => None,
        };
    }
    match profile.config_family {
        EggConfigFamily::V1 => match raw {
            0 => Some(LiftOffDistance::TenthsMm(7)),
            1 => Some(LiftOffDistance::TenthsMm(10)),
            2 => Some(LiftOffDistance::TenthsMm(20)),
            _ => None,
        },
        EggConfigFamily::V2 => (raw <= 10).then(|| LiftOffDistance::TenthsMm(7 + raw)),
    }
}

pub fn encode_lod(lod: LiftOffDistance, profile: EggProductProfile) -> CoreResult<u8> {
    if profile.wireless_4k && profile.config_family == EggConfigFamily::V1 {
        return match lod {
            LiftOffDistance::TenthsMm(10) | LiftOffDistance::Medium => Ok(1),
            LiftOffDistance::TenthsMm(20) | LiftOffDistance::High => Ok(2),
            other => Err(CoreError::OutOfRange {
                field: "lod",
                actual: other.to_string(),
                expected: "1.0 mm or 2.0 mm".to_owned(),
            }),
        };
    }
    match profile.config_family {
        EggConfigFamily::V1 => match lod {
            LiftOffDistance::TenthsMm(7) | LiftOffDistance::Low => Ok(0),
            LiftOffDistance::TenthsMm(10) | LiftOffDistance::Medium => Ok(1),
            LiftOffDistance::TenthsMm(20) | LiftOffDistance::High => Ok(2),
            other @ LiftOffDistance::TenthsMm(_) => Err(CoreError::OutOfRange {
                field: "lod",
                actual: other.to_string(),
                expected: "0.7 mm, 1.0 mm, or 2.0 mm".to_owned(),
            }),
        },
        EggConfigFamily::V2 => match lod {
            LiftOffDistance::TenthsMm(t @ 7..=17) => Ok(t - 7),
            LiftOffDistance::Low => Ok(0),
            LiftOffDistance::Medium => Ok(3),
            LiftOffDistance::High => Ok(10),
            other @ LiftOffDistance::TenthsMm(_) => Err(CoreError::OutOfRange {
                field: "lod",
                actual: other.to_string(),
                expected: "0.7..=1.7 mm".to_owned(),
            }),
        },
    }
}
