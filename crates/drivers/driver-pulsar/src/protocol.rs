//! Pulsar Report-8 flash & XS-1 wire protocol codecs.
//!
//! Verified against `docs/protocols/pulsar-hid.md` and
//! `OpenMouse-Project/mouse-protocol` (`src/pulsar/index.ts`, `src/drivers/pulsar/pulsar-hid.ts`).

use agnoperiph_core::{
    BatteryStatus, ChargingState, ConnectionKind, CoreError, CoreResult, DpiStage, DpiValue,
    LiftOffDistance, PollingRateHz,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

/// Pulsar Gaming Gears USB Vendor ID.
pub const PULSAR_VID: u16 = 0x3710;
/// Report-8 configuration usage page & usage ID.
pub const PULSAR_USAGE_PAGE: u16 = 0xFF02;
pub const PULSAR_USAGE_ID: u16 = 0x0002;
pub const PULSAR_REPORT_ID: u8 = 0x08;
/// 17-byte Report-8 packet including the leading Report ID byte (`0x08`).
pub const PULSAR_PACKET_LEN: usize = 17;
pub const PULSAR_MAX_PAYLOAD: usize = 10;

/// XS-1 (Pulsar X3 family) constants.
pub const PULSAR_XS1_USAGE_PAGE: u16 = 0xFFFF;
pub const PULSAR_XS1_USAGE_ID: u16 = 0x0001;
pub const PULSAR_XS1_PACKET_LEN: usize = 64;
pub const PULSAR_XS1_PIDS: &[u16] = &[0x3404, 0x3409, 0x3410, 0x5402, 0x5403];

/// Pulsar Report-8 command IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    EncryptionData = 0x01,
    DeviceOnline = 0x03,
    BatteryLevel = 0x04,
    WriteFlash = 0x07,
    ReadFlash = 0x08,
    GetCurrentConfig = 0x0E,
    SetCurrentConfig = 0x0F,
    ReadVersionId = 0x12,
    GetDongleVersion = 0x1D,
}

impl Command {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::EncryptionData),
            0x03 => Some(Self::DeviceOnline),
            0x04 => Some(Self::BatteryLevel),
            0x07 => Some(Self::WriteFlash),
            0x08 => Some(Self::ReadFlash),
            0x0E => Some(Self::GetCurrentConfig),
            0x0F => Some(Self::SetCurrentConfig),
            0x12 => Some(Self::ReadVersionId),
            0x1D => Some(Self::GetDongleVersion),
            _ => None,
        }
    }
}

/// Verified Pulsar Report-8 flash offsets.
pub mod flash {
    pub const POLL_RATE: u16 = 0;
    pub const DPI_STAGE_COUNT: u16 = 2;
    pub const ACTIVE_DPI_STAGE: u16 = 4;
    pub const LIFT_OFF_DISTANCE: u16 = 10;
    pub const DPI_STAGES: u16 = 12;
    pub const DPI_STAGE_STRIDE: u16 = 4;
    pub const DEBOUNCE_MS: u16 = 169;
    pub const MOTION_SYNC: u16 = 171;
    pub const ANGLE_SNAPPING: u16 = 175;
    pub const RIPPLE_CONTROL: u16 = 177;

    pub const MIN_DPI: u32 = 10;
    pub const MAX_DPI: u32 = 32_000;
    pub const DPI_STEP: u32 = 10;
    pub const MAX_DPI_STAGES: u8 = 8;
    pub const MAX_DEBOUNCE_MS: u8 = 15;
    pub const PROFILE_COUNT: u8 = 4;
}

/// Device metadata parsed from `EncryptionData` (`0x01`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulsarHandshakeInfo {
    pub cid: u8,
    pub mid: u8,
    pub device_type: u8,
    pub dongle_type: u8,
    pub connection: ConnectionKind,
    pub max_polling_hz: u16,
}

impl PulsarHandshakeInfo {
    /// Decodes the 10-byte payload of an `EncryptionData` (`0x01`) reply.
    ///
    /// Note: in `pulsar-hid.ts`, `response[0]` is command byte (after Report ID),
    /// so `response[9..=12]` correspond to payload offsets `[4..=7]`.
    pub fn from_reply_payload(payload: &[u8]) -> Self {
        let cid = payload.get(4).copied().unwrap_or(0);
        let mid = payload.get(5).copied().unwrap_or(0);
        let device_type = payload.get(6).copied().unwrap_or(0);
        let dongle_type = payload.get(7).copied().unwrap_or(0);
        let connection = if device_type == 2 || device_type == 3 {
            ConnectionKind::WiredUsb
        } else {
            ConnectionKind::WirelessDongle24Ghz
        };
        let max_polling_hz = match device_type {
            1 => 4000,
            3 | 5 => 8000,
            4 => 2000,
            _ => 1000,
        };
        Self {
            cid,
            mid,
            device_type,
            dongle_type,
            connection,
            max_polling_hz,
        }
    }

    pub fn model_hint(self) -> Option<&'static str> {
        match (self.cid, self.mid) {
            (0x57, 0x04) => Some("Pulsar X2 CrazyLight"),
            _ => None,
        }
    }
}

/// Computes the `0x55`-sum checksum byte: `(0x55 - sum(bytes)) & 0xFF`.
pub fn compute_checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    0x55u8.wrapping_sub(sum)
}

/// Returns `true` if `sum(bytes) & 0xFF == 0x55`.
pub fn is_checksum_valid(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0x55
}

/// Appends the trailing `0x55`-sum checksum byte to a flash field.
pub fn seal_field(values: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() + 1);
    out.extend_from_slice(values);
    out.push(compute_checksum(values));
    out
}

/// Encodes a 17-byte Pulsar Report-8 packet.
pub fn encode_request(
    cmd: Command,
    address: u16,
    payload: &[u8],
) -> CoreResult<[u8; PULSAR_PACKET_LEN]> {
    if payload.len() > PULSAR_MAX_PAYLOAD {
        return Err(CoreError::OutOfRange {
            field: "pulsar_payload_len",
            actual: payload.len().to_string(),
            expected: format!("0..={PULSAR_MAX_PAYLOAD}"),
        });
    }
    let mut pkt = [0u8; PULSAR_PACKET_LEN];
    pkt[0] = PULSAR_REPORT_ID;
    pkt[1] = cmd as u8;
    pkt[2] = 0x00;
    let [hi, lo] = address.to_be_bytes();
    pkt[3] = hi;
    pkt[4] = lo;
    pkt[5] = u8::try_from(payload.len()).unwrap_or(0);
    pkt[6..6 + payload.len()].copy_from_slice(payload);
    pkt[16] = compute_checksum(&pkt[..16]);
    Ok(pkt)
}

/// Strict pre-transmission validator for Pulsar 17-byte Report-8 frames.
pub fn pulsar_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![PULSAR_PACKET_LEN], vec![PULSAR_REPORT_ID]).with_checksum(
        |kind, report| {
            if kind != ReportKind::Output {
                return Err(format!(
                    "Pulsar Report-8 requires Output reports, got {kind}"
                ));
            }
            if !is_checksum_valid(report) {
                return Err("Pulsar Report-8 0x55 checksum mismatch".to_owned());
            }
            Ok(())
        },
    )
}

/// Decoded 17-byte Pulsar reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulsarReply {
    pub command: Command,
    pub error: u8,
    pub address: u16,
    pub declared_len: usize,
    pub payload: [u8; PULSAR_MAX_PAYLOAD],
}

/// Decodes a 17-byte (or 16-byte without Report ID) Pulsar Report-8 input reply.
pub fn decode_reply(raw: &[u8]) -> Option<PulsarReply> {
    let mut buf = [0u8; PULSAR_PACKET_LEN];
    if raw.len() == PULSAR_PACKET_LEN && raw[0] == PULSAR_REPORT_ID {
        buf.copy_from_slice(raw);
    } else if raw.len() == PULSAR_PACKET_LEN - 1 {
        buf[0] = PULSAR_REPORT_ID;
        buf[1..].copy_from_slice(raw);
    } else {
        return None;
    }

    if !is_checksum_valid(&buf) {
        return None;
    }
    let command = Command::from_u8(buf[1])?;
    let mut payload = [0u8; PULSAR_MAX_PAYLOAD];
    payload.copy_from_slice(&buf[6..16]);
    Some(PulsarReply {
        command,
        error: buf[2],
        address: u16::from_be_bytes([buf[3], buf[4]]),
        declared_len: usize::from(buf[5]),
        payload,
    })
}

/// Decodes Pulsar `0x3710` `dpiEx` 4-byte flash record into a [`DpiStage`].
pub fn decode_dpi_stage(index: u8, data: &[u8]) -> CoreResult<DpiStage> {
    if data.len() < 4 || !is_checksum_valid(&data[..4]) {
        return Err(CoreError::Protocol(
            "invalid Pulsar DPI stage checksum".to_owned(),
        ));
    }
    let low = u32::from(data[0]);
    let flags = u32::from(data[2]);
    let raw = low + (((flags & 0x0C) >> 2) << 8);
    let mut dpi = (raw + 1) * 10;
    if (flags & 0x02) != 0 {
        dpi = dpi * 5 + 10_000;
    }
    if (flags & 0x01) != 0 {
        dpi *= 2;
    }
    Ok(DpiStage::new(index, DpiValue::new(dpi)?))
}

/// Encodes a [`DpiStage`] into Pulsar `0x3710` `dpiEx` 4-byte flash format.
pub fn encode_dpi_stage(stage: &DpiStage) -> CoreResult<[u8; 4]> {
    let dpi = stage.x.value();
    let (raw, dpi_ex): (u32, u8) = if dpi >= 30_100 {
        if dpi > flash::MAX_DPI || !dpi.is_multiple_of(100) {
            return Err(CoreError::OutOfRange {
                field: "dpi",
                actual: dpi.to_string(),
                expected: "30100..=32000 in steps of 100".to_owned(),
            });
        }
        ((dpi / 2 - 10_050) / 50, 0x33)
    } else if dpi >= 10_050 {
        if !dpi.is_multiple_of(50) {
            return Err(CoreError::OutOfRange {
                field: "dpi",
                actual: dpi.to_string(),
                expected: "10050..=30000 in steps of 50".to_owned(),
            });
        }
        ((dpi - 10_050) / 50, 0x22)
    } else {
        if dpi < flash::MIN_DPI || !dpi.is_multiple_of(10) {
            return Err(CoreError::OutOfRange {
                field: "dpi",
                actual: dpi.to_string(),
                expected: "10..=10000 in steps of 10".to_owned(),
            });
        }
        (dpi / 10 - 1, 0x00)
    };

    let low = u8::try_from(raw & 0xFF).unwrap_or(0);
    let high = u8::try_from((raw >> 8) & 0x03).unwrap_or(0);
    let flags = (high << 2) | (high << 6) | dpi_ex | (dpi_ex << 4);
    let csum = compute_checksum(&[low, low, flags]);
    Ok([low, low, flags, csum])
}

pub fn decode_polling_rate(encoded: u8) -> CoreResult<PollingRateHz> {
    if encoded == 0 {
        return Err(CoreError::Protocol(
            "Pulsar polling rate byte was 0".to_owned(),
        ));
    }
    let hz = if encoded >= 16 {
        u16::from(encoded / 16) * 2000
    } else {
        1000 / u16::from(encoded)
    };
    PollingRateHz::from_hz(hz)
}

pub fn encode_polling_rate(rate: PollingRateHz) -> CoreResult<u8> {
    let hz = rate.as_hz();
    let code = match hz {
        125 => 0x08,
        250 => 0x04,
        500 => 0x02,
        1000 => 0x01,
        2000 => 0x10,
        4000 => 0x20,
        8000 => 0x40,
        other => {
            return Err(CoreError::OutOfRange {
                field: "polling_rate",
                actual: other.to_string(),
                expected: "125..=8000 Hz".to_owned(),
            });
        }
    };
    Ok(code)
}

pub fn decode_lod(raw: u8) -> Option<LiftOffDistance> {
    match raw {
        3 => Some(LiftOffDistance::Low),
        1 => Some(LiftOffDistance::Medium),
        2 => Some(LiftOffDistance::High),
        _ => None,
    }
}

pub fn encode_lod(lod: LiftOffDistance) -> CoreResult<u8> {
    match lod {
        LiftOffDistance::Low | LiftOffDistance::TenthsMm(7) => Ok(3),
        LiftOffDistance::Medium | LiftOffDistance::TenthsMm(10) => Ok(1),
        LiftOffDistance::High | LiftOffDistance::TenthsMm(20) => Ok(2),
        other @ LiftOffDistance::TenthsMm(_) => Err(CoreError::OutOfRange {
            field: "lod",
            actual: other.to_string(),
            expected: "Low, Medium, or High".to_owned(),
        }),
    }
}

pub fn decode_battery(payload: &[u8]) -> BatteryStatus {
    let pct = payload.first().copied().filter(|&p| p <= 100);
    let state = match payload.get(1).copied() {
        Some(1) => ChargingState::Charging,
        Some(0) => ChargingState::Discharging,
        _ => ChargingState::Unknown,
    };
    let voltage_mv = payload
        .get(2..4)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .filter(|&mv| mv > 0);
    BatteryStatus::new(pct, voltage_mv, state, pct.is_some())
}

pub fn decode_firmware(payload: &[u8]) -> Option<String> {
    let major = payload.first().copied()?;
    let minor = payload.get(1).copied()?;
    if major == 0 && minor == 0 {
        return None;
    }
    Some(format!("v{major}.{minor:02x}"))
}

/// Computes the 16-bit little-endian sum checksum for Pulsar XS-1 (X3 family) 64-byte feature packets.
pub fn xs1_checksum(packet: &[u8]) -> u16 {
    packet
        .iter()
        .take(PULSAR_XS1_PACKET_LEN - 2)
        .fold(0u16, |acc, &b| acc.wrapping_add(u16::from(b)))
}

/// Encodes a Pulsar XS-1 64-byte request packet with u16LE checksum at bytes `62..64`.
pub fn xs1_encode_request(command: &[u8]) -> [u8; PULSAR_XS1_PACKET_LEN] {
    let mut packet = [0u8; PULSAR_XS1_PACKET_LEN];
    let len = command.len().min(PULSAR_XS1_PACKET_LEN - 3);
    packet[1..=len].copy_from_slice(&command[..len]);
    let crc = xs1_checksum(&packet);
    let [lo, hi] = crc.to_le_bytes();
    packet[62] = lo;
    packet[63] = hi;
    packet
}
