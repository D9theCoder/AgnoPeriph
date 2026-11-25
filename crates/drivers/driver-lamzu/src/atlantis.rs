//! Lamzu Atlantis (CompX Report-8 flash protocol) wire framing, checksums, and encoders/decoders.
//!
//! Verified from `OpenMouse-Project/mouse-protocol` (`src/lamzu/atlantis.ts`) and
//! `LeadSun/lamzu-cfg` (`src/atlantis.rs`, `src/atlantis/hid.rs`).

use agnoperiph_core::{
    BatteryStatus, ChargingState, ConnectionKind, CoreError, CoreResult, DpiStage, DpiValue,
    LiftOffDistance, PollingRateHz, RgbColor,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

/// CompX USB Vendor ID used by Lamzu Atlantis generation mice and receivers.
pub const LAMZU_ATLANTIS_VID: u16 = 0x3554;

/// Vendor HID collection coordinates for Atlantis configuration reports.
pub const LAMZU_ATLANTIS_USAGE_PAGE: u16 = 0xFF02;
pub const LAMZU_ATLANTIS_USAGE_ID: u16 = 0x0002;

/// Report ID and exact packet length (including Report ID byte 0).
pub const ATLANTIS_REPORT_ID: u8 = 0x08;
pub const ATLANTIS_PACKET_LEN: usize = 17;
pub const ATLANTIS_MAX_PAYLOAD: usize = 10;

/// Verified checksum invariant target: `sum(all_bytes) & 0xFF == 0x55`.
pub const CHECKSUM_TARGET: u8 = 0x55;

/// Verified Lamzu Atlantis product profiles (wired cable vs. 1K/4K wireless dongles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlantisRateFamily {
    /// Wired cable and 1K receiver (`0x01` = 1000 Hz, max 1000 Hz).
    Wired,
    /// CompX 4K receiver (`0x01` = 1000 Hz, `0x10` = 2000 Hz, `0x20` = 4000 Hz, `0x40` = 8000 Hz).
    Pulsar4k,
}

/// Metadata for a known Lamzu Atlantis USB Product ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlantisProductProfile {
    pub pid: u16,
    pub model_name: &'static str,
    pub connection: ConnectionKind,
    pub rate_family: AtlantisRateFamily,
    pub max_polling_hz: u16,
}

/// Verified Atlantis PID table (separating wired USB and wireless receivers).
pub const ATLANTIS_PRODUCTS: &[AtlantisProductProfile] = &[
    AtlantisProductProfile {
        pid: 0xF50F,
        model_name: "Lamzu Atlantis (Wired)",
        connection: ConnectionKind::WiredUsb,
        rate_family: AtlantisRateFamily::Wired,
        max_polling_hz: 1000,
    },
    AtlantisProductProfile {
        pid: 0xF50D,
        model_name: "Lamzu Atlantis (1K Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        rate_family: AtlantisRateFamily::Wired,
        max_polling_hz: 1000,
    },
    AtlantisProductProfile {
        pid: 0xF510,
        model_name: "Lamzu Atlantis (4K Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        rate_family: AtlantisRateFamily::Pulsar4k,
        max_polling_hz: 4000,
    },
];

/// Looks up a known Lamzu Atlantis product profile by USB PID.
pub fn lookup_product(pid: u16) -> Option<AtlantisProductProfile> {
    ATLANTIS_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

/// Supported CompX Report-8 command opcodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    ReadBattery = 0x04,
    WriteFlash = 0x07,
    ReadFlash = 0x08,
    ReadActiveProfile = 0x0E,
    WriteActiveProfile = 0x0F,
    ReadFirmwareVersion = 0x12,
}

impl Command {
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0x04 => Some(Self::ReadBattery),
            0x07 => Some(Self::WriteFlash),
            0x08 => Some(Self::ReadFlash),
            0x0E => Some(Self::ReadActiveProfile),
            0x0F => Some(Self::WriteActiveProfile),
            0x12 => Some(Self::ReadFirmwareVersion),
            _ => None,
        }
    }
}

/// Verified active-profile flash byte offsets.
pub mod flash {
    pub const POLL_RATE: u16 = 0;
    pub const DPI_STAGE_COUNT: u16 = 2;
    pub const ACTIVE_DPI_STAGE: u16 = 4;
    pub const LIFT_OFF_DISTANCE: u16 = 10;
    pub const DPI_STAGES: u16 = 12;
    pub const DPI_STAGE_STRIDE: u16 = 4;
    pub const DPI_COLORS: u16 = 44;
    pub const DEBOUNCE_MS: u16 = 169;
    pub const MOTION_SYNC: u16 = 171;
    pub const ANGLE_SNAPPING: u16 = 175;
    pub const RIPPLE_CONTROL: u16 = 177;

    pub const MAX_DPI_STAGES: u8 = 8;
    pub const MIN_DPI: u32 = 50;
    pub const MAX_DPI: u32 = 26_000;
    pub const DPI_STEP: u32 = 50;
    pub const MAX_DEBOUNCE_MS: u8 = 15;
    pub const PROFILE_COUNT: u8 = 4;
}

/// Computes the trailing CompX `0x55`-sum checksum byte: `(0x55 - sum(data)) & 0xFF`.
pub fn compute_checksum(data: &[u8]) -> u8 {
    let sum = data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    CHECKSUM_TARGET.wrapping_sub(sum)
}

/// Verifies that a byte slice including its trailing checksum sums to `0x55 (mod 256)`.
pub fn is_checksum_valid(bytes_with_checksum: &[u8]) -> bool {
    let sum = bytes_with_checksum
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    sum == CHECKSUM_TARGET
}

/// Seals a flash field value slice by appending its trailing `0x55`-sum checksum byte.
pub fn seal_field(values: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() + 1);
    out.extend_from_slice(values);
    out.push(compute_checksum(values));
    out
}

/// Packet validator callback enforcing the 17-byte Report-8 checksum invariant before transmission.
pub fn verify_packet_checksum(_kind: ReportKind, report: &[u8]) -> Result<(), String> {
    if report.len() != ATLANTIS_PACKET_LEN {
        return Err(format!(
            "expected {ATLANTIS_PACKET_LEN} bytes, got {}",
            report.len()
        ));
    }
    let expected = compute_checksum(&report[..ATLANTIS_PACKET_LEN - 1]);
    let actual = report[ATLANTIS_PACKET_LEN - 1];
    if actual != expected {
        return Err(format!(
            "CompX checksum mismatch: expected 0x{expected:02X}, got 0x{actual:02X}"
        ));
    }
    Ok(())
}

/// Builds the strict `FixedReportValidator` for Lamzu Atlantis Report-8 traffic.
pub fn atlantis_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![ATLANTIS_PACKET_LEN], vec![ATLANTIS_REPORT_ID])
        .with_checksum(verify_packet_checksum)
}

/// Encodes a 17-byte CompX Report-8 request frame including Report ID `0x08` and checksum.
pub fn encode_request(
    cmd: Command,
    address: u16,
    payload: &[u8],
) -> CoreResult<[u8; ATLANTIS_PACKET_LEN]> {
    if payload.len() > ATLANTIS_MAX_PAYLOAD {
        return Err(CoreError::Validation(format!(
            "Atlantis payload length {} exceeds max {ATLANTIS_MAX_PAYLOAD}",
            payload.len()
        )));
    }
    let mut frame = [0u8; ATLANTIS_PACKET_LEN];
    frame[0] = ATLANTIS_REPORT_ID;
    frame[1] = cmd as u8;
    frame[2] = 0x00;
    let [addr_hi, addr_lo] = address.to_be_bytes();
    frame[3] = addr_hi;
    frame[4] = addr_lo;
    frame[5] = u8::try_from(payload.len()).unwrap_or(0);
    frame[6..6 + payload.len()].copy_from_slice(payload);
    frame[16] = compute_checksum(&frame[..16]);
    Ok(frame)
}

/// Decoded 17-byte CompX Report-8 reply frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlantisReply {
    pub command: Command,
    pub error: u8,
    pub address: u16,
    pub declared_len: usize,
    pub payload: [u8; ATLANTIS_MAX_PAYLOAD],
}

/// Decodes a 17-byte Report-8 input packet. Returns `None` for non-matching/unsolicited packets.
pub fn decode_reply(raw: &[u8]) -> Option<AtlantisReply> {
    if raw.len() < ATLANTIS_PACKET_LEN || raw[0] != ATLANTIS_REPORT_ID {
        return None;
    }
    let frame = &raw[..ATLANTIS_PACKET_LEN];
    if !is_checksum_valid(frame) {
        return None;
    }
    let command = Command::from_u8(frame[1])?;
    let mut payload = [0u8; ATLANTIS_MAX_PAYLOAD];
    payload.copy_from_slice(&frame[6..16]);
    Some(AtlantisReply {
        command,
        error: frame[2],
        address: u16::from_be_bytes([frame[3], frame[4]]),
        declared_len: usize::from(frame[5]).min(ATLANTIS_MAX_PAYLOAD),
        payload,
    })
}

/// Decodes a 4-byte sealed DPI stage flash record `[x_lo, y_lo, flags, csum]`.
pub fn decode_dpi_stage(index: u8, field: &[u8]) -> CoreResult<DpiStage> {
    if field.len() < 4 || !is_checksum_valid(&field[..4]) {
        return Err(CoreError::Protocol(format!(
            "corrupt DPI stage {index} flash field"
        )));
    }
    let flags = field[2];
    let decode_axis = |low: u8, high_bits: u8| -> u32 {
        let raw = (u32::from(high_bits & 0x03) << 8) + u32::from(low) + 1;
        raw * flash::DPI_STEP
    };
    let x = DpiValue::new(decode_axis(field[0], flags >> 2))?;
    let y = DpiValue::new(decode_axis(field[1], flags >> 6))?;
    let mut stage = DpiStage::new(index, x);
    if y != x {
        stage = stage.with_y(y);
    }
    Ok(stage)
}

/// Encodes one DPI stage into a 4-byte sealed flash record `[x_lo, y_lo, flags, csum]`.
pub fn encode_dpi_stage(stage: &DpiStage) -> CoreResult<[u8; 4]> {
    let encode_axis = |dpi: DpiValue| -> CoreResult<(u8, u8)> {
        let v = dpi.value();
        if !(flash::MIN_DPI..=flash::MAX_DPI).contains(&v) || !v.is_multiple_of(flash::DPI_STEP) {
            return Err(CoreError::OutOfRange {
                field: "dpi",
                actual: v.to_string(),
                expected: format!(
                    "{}..={} in steps of {}",
                    flash::MIN_DPI,
                    flash::MAX_DPI,
                    flash::DPI_STEP
                ),
            });
        }
        let raw = (v / flash::DPI_STEP) - 1;
        let low = u8::try_from(raw & 0xFF).unwrap_or(0);
        let high = u8::try_from((raw >> 8) & 0x03).unwrap_or(0);
        Ok((low, high))
    };

    let (x_lo, x_hi) = encode_axis(stage.x)?;
    let (y_lo, y_hi) = encode_axis(stage.effective_y())?;
    let flags = (x_hi << 2) | (y_hi << 6);
    let sealed = seal_field(&[x_lo, y_lo, flags]);
    Ok([sealed[0], sealed[1], sealed[2], sealed[3]])
}

/// Decodes a 4-byte sealed DPI LED color `[r, g, b, csum]`.
pub fn decode_dpi_color(field: &[u8]) -> CoreResult<RgbColor> {
    if field.len() < 4 || !is_checksum_valid(&field[..4]) {
        return Err(CoreError::Protocol(
            "corrupt DPI color flash field".to_owned(),
        ));
    }
    Ok(RgbColor::new(field[0], field[1], field[2]))
}

/// Decodes a polling rate flash byte according to the product's [`AtlantisRateFamily`].
pub fn decode_polling_rate(raw: u8, family: AtlantisRateFamily) -> CoreResult<PollingRateHz> {
    match (family, raw) {
        (_, 0x08) => Ok(PollingRateHz::Hz125),
        (_, 0x04) => Ok(PollingRateHz::Hz250),
        (_, 0x02) => Ok(PollingRateHz::Hz500),
        (_, 0x01) | (AtlantisRateFamily::Wired, 0x10) => Ok(PollingRateHz::Hz1000),
        (AtlantisRateFamily::Pulsar4k, 0x10) => Ok(PollingRateHz::Hz2000),
        (AtlantisRateFamily::Pulsar4k, 0x20) => Ok(PollingRateHz::Hz4000),
        (AtlantisRateFamily::Pulsar4k, 0x40) => Ok(PollingRateHz::Hz8000),
        _ => Err(CoreError::Protocol(format!(
            "unknown Atlantis polling rate byte 0x{raw:02X} for {family:?}"
        ))),
    }
}

/// Encodes a [`PollingRateHz`] into a flash byte for the product's [`AtlantisRateFamily`].
pub fn encode_polling_rate(rate: PollingRateHz, family: AtlantisRateFamily) -> CoreResult<u8> {
    match (family, rate) {
        (_, PollingRateHz::Hz125) => Ok(0x08),
        (_, PollingRateHz::Hz250) => Ok(0x04),
        (_, PollingRateHz::Hz500) => Ok(0x02),
        (_, PollingRateHz::Hz1000) => Ok(0x01),
        (AtlantisRateFamily::Pulsar4k, PollingRateHz::Hz2000) => Ok(0x10),
        (AtlantisRateFamily::Pulsar4k, PollingRateHz::Hz4000) => Ok(0x20),
        (AtlantisRateFamily::Pulsar4k, PollingRateHz::Hz8000) => Ok(0x40),
        _ => Err(CoreError::OutOfRange {
            field: "polling_rate",
            actual: rate.to_string(),
            expected: format!("supported rates for {family:?}"),
        }),
    }
}

/// Decodes the Atlantis lift-off distance byte (`1` = 1.0 mm, `2` = 2.0 mm).
pub fn decode_lod(raw: u8) -> CoreResult<LiftOffDistance> {
    match raw {
        1 => Ok(LiftOffDistance::TenthsMm(10)),
        2 => Ok(LiftOffDistance::TenthsMm(20)),
        other => Err(CoreError::Protocol(format!(
            "unknown Atlantis LOD byte 0x{other:02X}"
        ))),
    }
}

/// Encodes a [`LiftOffDistance`] into the Atlantis flash byte (`1` or `2`).
pub fn encode_lod(lod: LiftOffDistance) -> CoreResult<u8> {
    match lod {
        LiftOffDistance::Low | LiftOffDistance::TenthsMm(10) => Ok(1),
        LiftOffDistance::High | LiftOffDistance::TenthsMm(20) => Ok(2),
        other @ LiftOffDistance::TenthsMm(_) => Err(CoreError::OutOfRange {
            field: "lod",
            actual: other.to_string(),
            expected: "1.0 mm (Low) or 2.0 mm (High)".to_owned(),
        }),
    }
}

/// Decodes the `ReadBattery` (`0x04`) 4-byte payload `[percent, charging, mv_hi, mv_lo]`.
pub fn decode_battery(payload: &[u8; ATLANTIS_MAX_PAYLOAD]) -> BatteryStatus {
    let pct = (payload[0] <= 100).then_some(payload[0]);
    let charging = payload[1] == 1;
    let mv_raw = u16::from_be_bytes([payload[2], payload[3]]);
    let voltage_mv = (mv_raw > 0).then_some(mv_raw);
    let state = if charging {
        if pct == Some(100) {
            ChargingState::Full
        } else {
            ChargingState::Charging
        }
    } else {
        ChargingState::Discharging
    };
    BatteryStatus::new(pct, voltage_mv, state, true)
}

/// Decodes the `ReadFirmwareVersion` (`0x12`) payload `[major, minor_bcd]`.
pub fn decode_firmware(payload: &[u8; ATLANTIS_MAX_PAYLOAD]) -> String {
    format!("v{}.{:02x}", payload[0], payload[1])
}
