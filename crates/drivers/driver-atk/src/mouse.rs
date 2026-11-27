//! ATK / VGN / VXE gaming mouse driver over the shared `0x08` 16-byte EEPROM HID protocol.
//!
//! All framing, checksum, DPI encoding, and register addresses are documented in
//! `docs/protocols/atk-mice.md` (verified from `OpenMouse-Project/mouse-protocol`).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, ChargingState, ConnectionKind, CoreError,
    CoreResult, DebounceMs, DeviceId, DeviceInfo, DeviceMatchRule, DpiStage, DpiValue,
    LiftOffDistance, Peripheral, PointerCapabilities, PointerDevice, PointerSnapshot,
    PollingRateHz, ProfileCapabilities, ProfileInfo, ProfileManager, ProfileMode, ProfileSlot,
    ProfileSnapshot, VendorKind,
};
use agnoperiph_transport::{
    FixedReportValidator, GuardedTransport, HidTransport, PacketSink, ReadMatch, ReportKind,
};

/// USB Vendor ID for Shenzhen Yizhita / ATK / VXE hardware.
pub const ATK_MOUSE_VID: u16 = 0x373B;
/// USB Vendor ID shared by CompX / VGN / VXE R1 & F1 mice.
pub const VGN_COMPX_VID: u16 = 0x3554;

/// Vendor HID collection coordinates for `0x08` EEPROM mice.
pub const ATK_MOUSE_USAGE_PAGE: u16 = 0xFF02;
pub const ATK_MOUSE_USAGE_ID: u16 = 0x0002;

/// Report ID and fixed frame dimensions (`17` bytes TX with Report ID `0x08`, `16` bytes payload).
pub const ATK_MOUSE_REPORT_ID: u8 = 0x08;
pub const ATK_MOUSE_PAYLOAD_LEN: usize = 16;
pub const ATK_MOUSE_TX_LEN: usize = 17;
pub const ATK_MOUSE_MAX_CHUNK: usize = 10;
const CHECKSUM_TARGET: u8 = 0x55;

const READ_TIMEOUT: Duration = Duration::from_millis(600);
const MAX_READ_ATTEMPTS: usize = 64;

/// Wire command IDs for the `0x08` EEPROM protocol.
pub mod cmd {
    pub const HANDSHAKE: u8 = 0x01;
    pub const ONLINE: u8 = 0x03;
    pub const BATTERY: u8 = 0x04;
    pub const WRITE_EEPROM: u8 = 0x07;
    pub const READ_EEPROM: u8 = 0x08;
    pub const RESTORE_FACTORY: u8 = 0x09; // BLOCKED by safety validator
    pub const ENTER_UPGRADE: u8 = 0x0D; // BLOCKED by safety validator
    pub const GET_PROFILE: u8 = 0x0E;
    pub const SET_PROFILE: u8 = 0x0F;
    pub const GET_CID_MID: u8 = 0x10;
    pub const GET_FIRMWARE: u8 = 0x12;
}

/// Configuration EEPROM register addresses.
pub mod reg {
    pub const POLLING_RATE: u16 = 0x0000;
    pub const DPI_STAGE_COUNT: u16 = 0x0002;
    pub const ACTIVE_DPI_STAGE: u16 = 0x0004;
    pub const LOD: u16 = 0x000A;
    pub const DPI_STAGES: u16 = 0x000C;
    pub const DEBOUNCE_MS: u16 = 0x00A9;
    pub const MOTION_SYNC: u16 = 0x00AB;
    pub const SLEEP_TIMEOUT: u16 = 0x00AD;
    pub const ANGLE_SNAPPING: u16 = 0x00AF;
    pub const RIPPLE_CONTROL: u16 = 0x00B1;
}

/// Sensor DPI encoding family for ATK / VGN / VXE mice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtkMouseSensor {
    /// PAW3950 Ultra (`10..=42000`, 10 DPI steps up to 10K, 50 up to 30K, 100 above).
    Paw3950Ultra,
    /// PAW3950 (`50..=36000`, 50 DPI steps up to 30K, 100 above).
    Paw3950,
    /// PAW3395 / PAW3395 Ultra (`100..=30000`, 50 DPI steps).
    Paw3395,
    /// PAW3395 SE (`200..=18000`, 50 DPI steps).
    Paw3395Se,
    /// PAW3311 (`200..=15000`, 100 DPI steps).
    Paw3311,
    /// VGN F2 / CORE26K (`50..=26000`, 50 DPI steps).
    Core26K,
}

impl AtkMouseSensor {
    pub const fn dpi_bounds(self) -> (u32, u32, u32) {
        match self {
            Self::Paw3950Ultra => (50, 42_000, 50),
            Self::Paw3950 => (50, 36_000, 50),
            Self::Paw3395 => (100, 30_000, 50),
            Self::Paw3395Se => (200, 18_000, 50),
            Self::Paw3311 => (200, 15_000, 100),
            Self::Core26K => (50, 26_000, 50),
        }
    }
}

/// Lift-Off Distance register encoding variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LodEncoding {
    /// VGN F2 / ATK X1 / PAW3950: `3`=Low, `1`=Medium, `2`=High.
    TriLevel,
    /// ATK A9 family: `code = tenths_mm - 6` (`1`=0.7 mm, `4`=1.0 mm, `11`=1.7 mm).
    TenthsOffset6,
}

/// Declarative descriptor table entry for one ATK / VGN / VXE mouse PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtkMouseDescriptor {
    pub vid: u16,
    pub pid: u16,
    pub connection: ConnectionKind,
    pub model_name: &'static str,
    pub sensor: AtkMouseSensor,
    pub max_polling_hz: u16,
    pub max_dpi_stages: u8,
    pub has_profiles: bool,
    pub lod_encoding: LodEncoding,
}

/// Verified ATK / VGN / VXE mouse descriptors from `docs/protocols/atk-mice.md`.
pub const ATK_MOUSE_DESCRIPTORS: &[AtkMouseDescriptor] = &[
    // --- VGN Dragonfly F2 Master+ (0x3554) ---
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xFB56,
        connection: ConnectionKind::WirelessDongle24Ghz,
        model_name: "VGN Dragonfly F2 Master+ (8K Receiver)",
        sensor: AtkMouseSensor::Core26K,
        max_polling_hz: 8000,
        max_dpi_stages: 8,
        has_profiles: false,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xFB57,
        connection: ConnectionKind::WiredUsb,
        model_name: "VGN Dragonfly F2 Master+",
        sensor: AtkMouseSensor::Core26K,
        max_polling_hz: 8000,
        max_dpi_stages: 8,
        has_profiles: false,
        lod_encoding: LodEncoding::TriLevel,
    },
    // --- VXE R1 COMPX family (0x3554) ---
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xF58A,
        connection: ConnectionKind::WirelessDongle24Ghz,
        model_name: "VXE R1 Pro Max (4K Receiver)",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 4000,
        max_dpi_stages: 8,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xF58C,
        connection: ConnectionKind::WiredUsb,
        model_name: "VXE R1 Pro / Pro Max",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 4000,
        max_dpi_stages: 8,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xF58E,
        connection: ConnectionKind::WirelessDongle24Ghz,
        model_name: "VXE R1 (Receiver)",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 2000,
        max_dpi_stages: 8,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xF58F,
        connection: ConnectionKind::WiredUsb,
        model_name: "VXE R1",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 2000,
        max_dpi_stages: 8,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    // --- ATK Dragonfly F1 COMPX (0x3554) ---
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xF502,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK Dragonfly F1 Pro",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 4000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xF506,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK Dragonfly F1 Pro Max / MOBA",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 4000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: VGN_COMPX_VID,
        pid: 0xF50A,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK Dragonfly F1S",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 4000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    // --- ATK / VXE 0x373B Mice & Receivers ---
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1014,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK F1 Pro",
        sensor: AtkMouseSensor::Paw3950,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1015,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK F1 Pro Max",
        sensor: AtkMouseSensor::Paw3950,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1016,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK X1 Pro",
        sensor: AtkMouseSensor::Paw3950,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1017,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK X1 Pro Max",
        sensor: AtkMouseSensor::Paw3950,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1018,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK Z1 Pro",
        sensor: AtkMouseSensor::Paw3950,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1019,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK Z1 Pro Max",
        sensor: AtkMouseSensor::Paw3950,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x102E,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK F1 Ultimate",
        sensor: AtkMouseSensor::Paw3950Ultra,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x102F,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK X1 Ultra",
        sensor: AtkMouseSensor::Paw3950Ultra,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x103E,
        connection: ConnectionKind::WiredUsb,
        model_name: "VXE Mad R Major",
        sensor: AtkMouseSensor::Paw3950,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x103F,
        connection: ConnectionKind::WiredUsb,
        model_name: "VXE Mad R",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1045,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK F1 Extreme",
        sensor: AtkMouseSensor::Paw3950Ultra,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1085,
        connection: ConnectionKind::WirelessDongle24Ghz,
        model_name: "VXE R1 SE+ (1K Receiver)",
        sensor: AtkMouseSensor::Paw3395Se,
        max_polling_hz: 1000,
        max_dpi_stages: 6,
        has_profiles: false,
        lod_encoding: LodEncoding::TriLevel,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x10C9,
        connection: ConnectionKind::WirelessDongle24Ghz,
        model_name: "ATK NearLink Mouse Receiver",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 1000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TenthsOffset6,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1115,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK A9 Plus NearLink",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 1000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TenthsOffset6,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1117,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK A9 Pro Max",
        sensor: AtkMouseSensor::Paw3395,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TenthsOffset6,
    },
    AtkMouseDescriptor {
        vid: ATK_MOUSE_VID,
        pid: 0x1119,
        connection: ConnectionKind::WiredUsb,
        model_name: "ATK A9 Ultra",
        sensor: AtkMouseSensor::Paw3950Ultra,
        max_polling_hz: 8000,
        max_dpi_stages: 6,
        has_profiles: true,
        lod_encoding: LodEncoding::TenthsOffset6,
    },
];

/// Looks up a mouse descriptor by `(vid, pid)`.
pub fn find_mouse_descriptor(vid: u16, pid: u16) -> Option<&'static AtkMouseDescriptor> {
    ATK_MOUSE_DESCRIPTORS
        .iter()
        .find(|d| d.vid == vid && d.pid == pid)
}

/// Builds declarative `DeviceMatchRule`s for all supported ATK / VGN / VXE mice.
pub fn mouse_match_rules() -> Vec<DeviceMatchRule> {
    let mut rules: Vec<DeviceMatchRule> = ATK_MOUSE_DESCRIPTORS
        .iter()
        .map(|d| {
            DeviceMatchRule::new(d.vid, d.connection, d.model_name)
                .with_pid(d.pid)
                .with_usage_page(ATK_MOUSE_USAGE_PAGE)
                .with_usage_id(ATK_MOUSE_USAGE_ID)
        })
        .collect();
    // Catch-all for unlisted 0x373B ATK/VXE mouse PIDs on the 0xFF02:0x0002 collection.
    rules.push(
        DeviceMatchRule::new(ATK_MOUSE_VID, ConnectionKind::WiredUsb, "ATK / VXE Mouse")
            .with_usage_page(ATK_MOUSE_USAGE_PAGE)
            .with_usage_id(ATK_MOUSE_USAGE_ID),
    );
    rules
}

/// Computes the `0x55`-sum checksum for a 15-byte payload slice (prepended with Report ID `0x08`).
pub fn frame_checksum(payload_15: &[u8]) -> u8 {
    let mut sum = ATK_MOUSE_REPORT_ID;
    for &b in payload_15 {
        sum = sum.wrapping_add(b);
    }
    CHECKSUM_TARGET.wrapping_sub(sum)
}

/// Returns `true` if a 16-byte payload satisfies `(0x08 + sum(payload)) & 0xFF == 0x55`.
pub fn is_valid_payload_checksum(payload: &[u8]) -> bool {
    if payload.len() != ATK_MOUSE_PAYLOAD_LEN {
        return false;
    }
    let sum = payload
        .iter()
        .fold(ATK_MOUSE_REPORT_ID, |acc, &b| acc.wrapping_add(b));
    sum == CHECKSUM_TARGET
}

/// Validates outgoing ATK / VGN / VXE mouse TX reports (17 bytes, Report ID `0x08`,
/// valid `0x55` checksum, and no factory-reset / bootloader-upgrade opcodes).
pub fn verify_mouse_tx(_kind: ReportKind, report: &[u8]) -> Result<(), String> {
    if report.len() != ATK_MOUSE_TX_LEN {
        return Err(format!(
            "expected {ATK_MOUSE_TX_LEN}-byte Report 0x08 frame, got {} bytes",
            report.len()
        ));
    }
    if report[0] != ATK_MOUSE_REPORT_ID {
        return Err(format!("disallowed Report ID 0x{:02X}", report[0]));
    }
    if matches!(report[1], cmd::RESTORE_FACTORY | cmd::ENTER_UPGRADE) {
        return Err(format!(
            "blocked dangerous ATK/VGN mouse command 0x{:02X}",
            report[1]
        ));
    }
    if !is_valid_payload_checksum(&report[1..]) {
        return Err("invalid 0x55 frame checksum on ATK/VGN mouse TX report".to_owned());
    }
    Ok(())
}

/// Builds the strict `FixedReportValidator` for ATK / VGN / VXE mouse traffic.
pub fn atk_mouse_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![ATK_MOUSE_TX_LEN], vec![ATK_MOUSE_REPORT_ID])
        .with_checksum(verify_mouse_tx)
}

/// Encodes a 17-byte TX report `[0x08, payload[16]]` with computed trailing checksum.
pub fn encode_frame(cmd_id: u8, address: u16, data: &[u8]) -> CoreResult<[u8; ATK_MOUSE_TX_LEN]> {
    if data.len() > ATK_MOUSE_MAX_CHUNK {
        return Err(CoreError::OutOfRange {
            field: "atk_mouse_data_len",
            actual: data.len().to_string(),
            expected: format!("0..={ATK_MOUSE_MAX_CHUNK}"),
        });
    }
    let mut tx = [0u8; ATK_MOUSE_TX_LEN];
    tx[0] = ATK_MOUSE_REPORT_ID;
    tx[1] = cmd_id;
    let [hi, lo] = address.to_be_bytes();
    tx[3] = hi;
    tx[4] = lo;
    tx[5] = u8::try_from(data.len()).unwrap_or(0);
    tx[6..6 + data.len()].copy_from_slice(data);
    tx[16] = frame_checksum(&tx[1..16]);
    Ok(tx)
}

/// Encodes a read-EEPROM request for `address` and `len` (`1..=10`).
pub fn encode_read_eeprom(address: u16, len: u8) -> CoreResult<[u8; ATK_MOUSE_TX_LEN]> {
    if len == 0 || usize::from(len) > ATK_MOUSE_MAX_CHUNK {
        return Err(CoreError::OutOfRange {
            field: "eeprom_read_len",
            actual: len.to_string(),
            expected: format!("1..={ATK_MOUSE_MAX_CHUNK}"),
        });
    }
    let mut tx = [0u8; ATK_MOUSE_TX_LEN];
    tx[0] = ATK_MOUSE_REPORT_ID;
    tx[1] = cmd::READ_EEPROM;
    let [hi, lo] = address.to_be_bytes();
    tx[3] = hi;
    tx[4] = lo;
    tx[5] = len;
    tx[16] = frame_checksum(&tx[1..16]);
    Ok(tx)
}

/// Encodes a 2-byte scalar `[value, (0x55 - value) & 0xFF]` write to EEPROM.
pub fn encode_write_scalar(address: u16, value: u8) -> CoreResult<[u8; ATK_MOUSE_TX_LEN]> {
    encode_frame(
        cmd::WRITE_EEPROM,
        address,
        &[value, CHECKSUM_TARGET.wrapping_sub(value)],
    )
}

/// Unpacks a `[value, parity]` scalar pair, returning `None` if `(value + parity) != 0x55`.
pub const fn unpack_scalar(value: u8, parity: u8) -> Option<u8> {
    if value.wrapping_add(parity) == CHECKSUM_TARGET {
        Some(value)
    } else {
        None
    }
}

/// Normalizes an incoming 16- or 17-byte RX buffer into a validated 16-byte payload.
pub fn unwrap_mouse_rx(raw: &[u8], expected_cmd: u8) -> Option<[u8; ATK_MOUSE_PAYLOAD_LEN]> {
    let slice = if raw.len() >= ATK_MOUSE_TX_LEN && raw[0] == ATK_MOUSE_REPORT_ID {
        &raw[1..ATK_MOUSE_TX_LEN]
    } else if raw.len() >= ATK_MOUSE_PAYLOAD_LEN {
        &raw[..ATK_MOUSE_PAYLOAD_LEN]
    } else {
        return None;
    };
    if !is_valid_payload_checksum(slice) || slice[0] != expected_cmd || slice[1] != 0 {
        return None;
    }
    let mut out = [0u8; ATK_MOUSE_PAYLOAD_LEN];
    out.copy_from_slice(slice);
    Some(out)
}

/// Encodes polling rate into the `0x0000` EEPROM byte.
pub fn encode_polling_rate(rate: PollingRateHz) -> u8 {
    match rate {
        PollingRateHz::Hz125 => 0x08,
        PollingRateHz::Hz250 => 0x04,
        PollingRateHz::Hz500 => 0x02,
        PollingRateHz::Hz1000 => 0x01,
        PollingRateHz::Hz2000 => 0x10,
        PollingRateHz::Hz4000 => 0x20,
        PollingRateHz::Hz8000 => 0x40,
    }
}

/// Decodes the `0x0000` EEPROM polling rate byte.
pub const fn decode_polling_rate(raw: u8) -> Option<PollingRateHz> {
    match raw {
        0x08 => Some(PollingRateHz::Hz125),
        0x04 => Some(PollingRateHz::Hz250),
        0x02 => Some(PollingRateHz::Hz500),
        0x01 => Some(PollingRateHz::Hz1000),
        0x10 => Some(PollingRateHz::Hz2000),
        0x20 => Some(PollingRateHz::Hz4000),
        0x40 => Some(PollingRateHz::Hz8000),
        _ => None,
    }
}

/// Encodes one 4-byte DPI stage `[x_lo, y_lo, mode, chk]` for `sensor`.
pub fn encode_dpi_stage(sensor: AtkMouseSensor, x: u32, y: u32) -> [u8; 4] {
    let encode_axis = |dpi: u32| -> (u8, u8) {
        match sensor {
            AtkMouseSensor::Paw3950Ultra => {
                let (val, doubled) = if dpi > 30_000 {
                    (dpi / 2, 1u8)
                } else {
                    (dpi, 0u8)
                };
                if val <= 10_000 {
                    let code = (val / 10).saturating_sub(1);
                    let byte = u8::try_from(code & 0xFF).unwrap_or(0);
                    let hi = u8::try_from((code >> 8) & 0x03).unwrap_or(0);
                    (byte, (hi << 2) | doubled)
                } else {
                    let code = val.saturating_sub(10_050) / 50;
                    let byte = u8::try_from(code & 0xFF).unwrap_or(0);
                    let hi = u8::try_from((code >> 8) & 0x03).unwrap_or(0);
                    (byte, (hi << 2) | 0x02 | doubled)
                }
            }
            AtkMouseSensor::Paw3311 => {
                let count = dpi.saturating_sub(200) / 100;
                let byte = u8::try_from(count & 0xFF).unwrap_or(0);
                let hi = u8::try_from((count >> 8) & 0x03).unwrap_or(0);
                (byte, hi << 2)
            }
            AtkMouseSensor::Paw3950
            | AtkMouseSensor::Paw3395
            | AtkMouseSensor::Paw3395Se
            | AtkMouseSensor::Core26K => {
                let doubled = u8::from(dpi > 30_000);
                let step = if doubled != 0 { 100 } else { 50 };
                let count = (dpi / step).saturating_sub(1);
                let byte = u8::try_from(count & 0xFF).unwrap_or(0);
                let hi = u8::try_from((count >> 8) & 0x03).unwrap_or(0);
                (byte, (hi << 2) | doubled)
            }
        }
    };

    let (xb, xn) = encode_axis(x);
    let (yb, yn) = encode_axis(y);
    let mode = ((yn & 0x0F) << 4) | (xn & 0x0F);
    let chk = CHECKSUM_TARGET.wrapping_sub(xb.wrapping_add(yb).wrapping_add(mode));
    [xb, yb, mode, chk]
}

/// Decodes one 4-byte DPI stage `[x_lo, y_lo, mode, chk]` for `sensor`.
pub fn decode_dpi_stage(sensor: AtkMouseSensor, bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 {
        return None;
    }
    let sum = bytes[0]
        .wrapping_add(bytes[1])
        .wrapping_add(bytes[2])
        .wrapping_add(bytes[3]);
    if sum != CHECKSUM_TARGET {
        return None;
    }
    let decode_axis = |byte: u8, nibble: u8| -> u32 {
        let code = (u32::from((nibble >> 2) & 0x03) << 8) | u32::from(byte);
        match sensor {
            AtkMouseSensor::Paw3950Ultra => {
                let base = if (nibble & 0x02) != 0 {
                    10_050 + code * 50
                } else {
                    (code + 1) * 10
                };
                if (nibble & 0x01) != 0 { base * 2 } else { base }
            }
            AtkMouseSensor::Paw3311 => 200 + code * 100,
            AtkMouseSensor::Paw3950
            | AtkMouseSensor::Paw3395
            | AtkMouseSensor::Paw3395Se
            | AtkMouseSensor::Core26K => {
                let dpi = (code + 1) * 50;
                if (nibble & 0x01) != 0 { dpi * 2 } else { dpi }
            }
        }
    };
    let x = decode_axis(bytes[0], bytes[2] & 0x0F);
    let y = decode_axis(bytes[1], (bytes[2] >> 4) & 0x0F);
    Some((x, y))
}

/// Active ATK / VGN / VXE mouse backed by [`GuardedTransport`].
pub struct AtkMouseDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    info: DeviceInfo,
    spec: AtkMouseDescriptor,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
}

impl<T: HidTransport> fmt::Debug for AtkMouseDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtkMouseDevice")
            .field("info", &self.info)
            .field("sensor", &self.spec.sensor)
            .finish_non_exhaustive()
    }
}

fn build_mouse_pointer_caps(spec: AtkMouseDescriptor) -> PointerCapabilities {
    let (dpi_min, dpi_max, dpi_step) = spec.sensor.dpi_bounds();
    let all_rates = [
        PollingRateHz::Hz125,
        PollingRateHz::Hz250,
        PollingRateHz::Hz500,
        PollingRateHz::Hz1000,
        PollingRateHz::Hz2000,
        PollingRateHz::Hz4000,
        PollingRateHz::Hz8000,
    ];
    let rates: Vec<PollingRateHz> = all_rates
        .into_iter()
        .filter(|r| r.as_hz() <= spec.max_polling_hz)
        .collect();
    let lods = match spec.lod_encoding {
        LodEncoding::TriLevel => vec![
            LiftOffDistance::Low,
            LiftOffDistance::Medium,
            LiftOffDistance::High,
        ],
        LodEncoding::TenthsOffset6 => vec![
            LiftOffDistance::TenthsMm(7),
            LiftOffDistance::TenthsMm(10),
            LiftOffDistance::TenthsMm(17),
        ],
    };
    PointerCapabilities::new(dpi_min, dpi_max, dpi_step, spec.max_dpi_stages)
        .with_xy_independent_dpi(true)
        .with_polling_rates(rates)
        .with_lods(lods)
        .with_debounce_range_ms(0, 20)
        .with_motion_sync(true)
        .with_angle_snapping(true)
        .with_ripple_control(true)
}

impl<T: HidTransport> AtkMouseDevice<T> {
    /// Opens an ATK / VGN / VXE mouse over its `0xFF02:0x0002` Report-`0x08` interface.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        let spec = find_mouse_descriptor(desc.vid, desc.pid)
            .copied()
            .unwrap_or_else(|| {
                let is_wireless = desc.product.as_deref().is_some_and(|p| {
                    let lower = p.to_ascii_lowercase();
                    lower.contains("dongle") || lower.contains("receiver")
                });
                AtkMouseDescriptor {
                    vid: desc.vid,
                    pid: desc.pid,
                    connection: if is_wireless {
                        ConnectionKind::WirelessDongle24Ghz
                    } else {
                        ConnectionKind::WiredUsb
                    },
                    model_name: "ATK / VXE Mouse",
                    sensor: AtkMouseSensor::Paw3395,
                    max_polling_hz: 4000,
                    max_dpi_stages: 6,
                    has_profiles: true,
                    lod_encoding: LodEncoding::TriLevel,
                }
            });

        if desc.vid != ATK_MOUSE_VID && desc.vid != VGN_COMPX_VID {
            return Err(CoreError::Validation(format!(
                "expected ATK/VGN mouse VID 0x{ATK_MOUSE_VID:04X} or 0x{VGN_COMPX_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(atk_mouse_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "atk-mouse:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));
        let model = desc
            .product
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(|| spec.model_name.to_owned(), str::to_owned);
        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Atk,
            model,
            spec.connection,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let pointer_caps = build_mouse_pointer_caps(spec);
        let capabilities = Capabilities::empty()
            .with_pointer(true)
            .with_battery(true)
            .with_profiles(spec.has_profiles);

        let mut dev = Self {
            transport: guarded,
            info,
            spec,
            capabilities,
            pointer_caps,
        };
        dev.discover_readonly()?;
        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn exchange(
        &mut self,
        tx: [u8; ATK_MOUSE_TX_LEN],
        mutating: bool,
    ) -> CoreResult<[u8; ATK_MOUSE_PAYLOAD_LEN]> {
        let expected_cmd = tx[1];
        if !mutating || !self.transport.is_dry_run() {
            let _ = self.transport.drain_input_reports();
        }
        if mutating {
            self.transport.send_output_report(&tx)?;
            if self.transport.is_dry_run() {
                let mut echoed = [0u8; ATK_MOUSE_PAYLOAD_LEN];
                echoed.copy_from_slice(&tx[1..]);
                return Ok(echoed);
            }
        } else {
            self.transport.send_query_output_report(&tx)?;
        }

        let mut rx = [0u8; ATK_MOUSE_TX_LEN];
        match self.transport.read_matching_input(
            &mut rx,
            READ_TIMEOUT,
            MAX_READ_ATTEMPTS,
            |frame| unwrap_mouse_rx(frame, expected_cmd),
        )? {
            ReadMatch::Matched(payload) => Ok(payload),
            ReadMatch::TimedOut => Err(CoreError::NotConnected(format!(
                "timed out waiting for ATK/VGN mouse reply to cmd 0x{expected_cmd:02X}"
            ))),
            ReadMatch::WindowExhausted => Err(CoreError::Protocol(format!(
                "no matching ATK/VGN mouse reply for cmd 0x{expected_cmd:02X}"
            ))),
        }
    }

    fn discover_readonly(&mut self) -> CoreResult<()> {
        if let Ok(fw) = self.exchange(encode_frame(cmd::GET_FIRMWARE, 0, &[])?, false) {
            let major = fw[5];
            let minor = fw[6];
            if major != 0 || minor != 0 {
                self.info.firmware_version = Some(format!("v{major}.{minor:02}"));
            }
        }
        Ok(())
    }

    fn read_eeprom(&mut self, address: u16, len: u8) -> CoreResult<Vec<u8>> {
        let resp = self.exchange(encode_read_eeprom(address, len)?, false)?;
        let count = usize::from(resp[4]).min(ATK_MOUSE_MAX_CHUNK);
        Ok(resp[5..5 + count].to_vec())
    }

    fn read_scalar(&mut self, address: u16) -> CoreResult<Option<u8>> {
        let bytes = self.read_eeprom(address, 2)?;
        if bytes.len() < 2 {
            return Ok(None);
        }
        Ok(unpack_scalar(bytes[0], bytes[1]))
    }

    fn write_scalar(&mut self, address: u16, value: u8) -> CoreResult<()> {
        self.exchange(encode_write_scalar(address, value)?, true)?;
        Ok(())
    }
}

impl<T: HidTransport> Peripheral for AtkMouseDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        self.discover_readonly()
    }

    fn as_pointer(&self) -> Option<&dyn PointerDevice> {
        Some(self)
    }

    fn as_pointer_mut(&mut self) -> Option<&mut dyn PointerDevice> {
        Some(self)
    }

    fn as_battery(&self) -> Option<&dyn BatteryMonitor> {
        Some(self)
    }

    fn as_battery_mut(&mut self) -> Option<&mut dyn BatteryMonitor> {
        Some(self)
    }

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        self.spec
            .has_profiles
            .then_some(self as &dyn ProfileManager)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        self.spec
            .has_profiles
            .then_some(self as &mut dyn ProfileManager)
    }
}

impl<T: HidTransport> BatteryMonitor for AtkMouseDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let resp = self.exchange(encode_frame(cmd::BATTERY, 0, &[])?, false)?;
        let pct = (resp[5] <= 100).then_some(resp[5]);
        let charging = resp[6] == 1;
        let voltage_mv = u16::from_be_bytes([resp[7], resp[8]]);
        let state = if charging {
            if pct.is_some_and(|p| p >= 99) {
                ChargingState::Full
            } else {
                ChargingState::Charging
            }
        } else if pct.is_some() {
            ChargingState::Discharging
        } else {
            ChargingState::Unknown
        };
        Ok(BatteryStatus::new(
            pct,
            (voltage_mv > 0).then_some(voltage_mv),
            state,
            true,
        ))
    }
}

impl<T: HidTransport> PointerDevice for AtkMouseDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        // Read system block [0x0000..0x0006]: polling rate, stage count, active stage.
        let sys = self.read_eeprom(reg::POLLING_RATE, 6)?;
        let polling_rate = sys
            .get(0..2)
            .and_then(|s| unpack_scalar(s[0], s[1]))
            .and_then(decode_polling_rate)
            .unwrap_or(PollingRateHz::Hz1000);
        let stage_count = sys
            .get(2..4)
            .and_then(|s| unpack_scalar(s[0], s[1]))
            .unwrap_or(4)
            .clamp(1, self.spec.max_dpi_stages);
        let active_stage = sys
            .get(4..6)
            .and_then(|s| unpack_scalar(s[0], s[1]))
            .unwrap_or(0)
            .min(stage_count.saturating_sub(1));

        let mut stages = Vec::with_capacity(usize::from(stage_count));
        for ix in 0..stage_count {
            let addr = reg::DPI_STAGES + u16::from(ix) * 4;
            let raw = self.read_eeprom(addr, 4)?;
            let (x, y) = decode_dpi_stage(self.spec.sensor, &raw).unwrap_or((800, 800));
            let mut st = DpiStage::new(ix, DpiValue::new(x.max(50))?);
            if y != x {
                st = st.with_y(DpiValue::new(y.max(50))?);
            }
            stages.push(st);
        }

        let mut snap = PointerSnapshot::new(active_stage, stages, polling_rate);

        if let Some(lod_raw) = self.read_scalar(reg::LOD)? {
            let lod = match self.spec.lod_encoding {
                LodEncoding::TriLevel => match lod_raw {
                    3 => Some(LiftOffDistance::Low),
                    1 => Some(LiftOffDistance::Medium),
                    2 => Some(LiftOffDistance::High),
                    _ => None,
                },
                LodEncoding::TenthsOffset6 => {
                    (lod_raw > 0).then(|| LiftOffDistance::TenthsMm(lod_raw.saturating_add(6)))
                }
            };
            if let Some(lod) = lod {
                snap = snap.with_lod(lod);
            }
        }

        // Read advanced block [0x00A9..0x00B3]: debounce, motion sync, sleep, angle, ripple.
        let adv = self.read_eeprom(reg::DEBOUNCE_MS, 10)?;
        if let Some(db) = adv.get(0..2).and_then(|s| unpack_scalar(s[0], s[1])) {
            snap = snap.with_debounce_ms(DebounceMs::new(db));
        }
        if let Some(ms) = adv.get(2..4).and_then(|s| unpack_scalar(s[0], s[1])) {
            snap = snap.with_motion_sync(ms != 0);
        }
        if let Some(asnap) = adv.get(6..8).and_then(|s| unpack_scalar(s[0], s[1])) {
            snap = snap.with_angle_snapping(asnap != 0);
        }
        if let Some(rc) = adv.get(8..10).and_then(|s| unpack_scalar(s[0], s[1])) {
            snap = snap.with_ripple_control(rc != 0);
        }

        Ok(snap)
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        let count = u8::try_from(stages.len()).unwrap_or(1);
        self.write_scalar(reg::DPI_STAGE_COUNT, count)?;
        for (ix, stage) in stages.iter().enumerate() {
            let addr = reg::DPI_STAGES + u16::try_from(ix).unwrap_or(0) * 4;
            let encoded = encode_dpi_stage(
                self.spec.sensor,
                stage.x.value(),
                stage.effective_y().value(),
            );
            self.exchange(encode_frame(cmd::WRITE_EEPROM, addr, &encoded)?, true)?;
        }
        self.write_scalar(reg::ACTIVE_DPI_STAGE, active_ix)?;
        Ok(())
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if stage_ix >= self.spec.max_dpi_stages {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{}", self.spec.max_dpi_stages),
            });
        }
        self.write_scalar(reg::ACTIVE_DPI_STAGE, stage_ix)
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        self.write_scalar(reg::POLLING_RATE, encode_polling_rate(rate))
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        let raw = match (self.spec.lod_encoding, lod) {
            (LodEncoding::TriLevel, LiftOffDistance::Low) => 3,
            (LodEncoding::TriLevel, LiftOffDistance::Medium) => 1,
            (LodEncoding::TriLevel, LiftOffDistance::High) => 2,
            (LodEncoding::TenthsOffset6, LiftOffDistance::TenthsMm(t)) => t.saturating_sub(6),
            _ => {
                return Err(CoreError::OutOfRange {
                    field: "lod",
                    actual: lod.to_string(),
                    expected: format!("{:?}", self.pointer_caps.supported_lods),
                });
            }
        };
        self.write_scalar(reg::LOD, raw)
    }

    fn set_debounce_ms(&mut self, debounce: DebounceMs) -> CoreResult<()> {
        self.pointer_caps.validate_debounce(debounce)?;
        self.write_scalar(reg::DEBOUNCE_MS, debounce.value())
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_scalar(reg::MOTION_SYNC, u8::from(enabled))
    }

    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_scalar(reg::ANGLE_SNAPPING, u8::from(enabled))
    }

    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_scalar(reg::RIPPLE_CONTROL, u8::from(enabled))
    }
}

impl<T: HidTransport> ProfileManager for AtkMouseDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        ProfileCapabilities::new(4, false, false)
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let resp = self.exchange(encode_frame(cmd::GET_PROFILE, 0, &[])?, false)?;
        let active = resp[5].min(3);
        let profiles = (0..4)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect();
        Ok(ProfileSnapshot::new(
            ProfileSlot::new(active),
            ProfileMode::OnboardMemory,
            profiles,
        ))
    }

    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()> {
        self.profile_capabilities().validate_slot(slot)?;
        self.exchange(encode_frame(cmd::SET_PROFILE, 0, &[slot.index()])?, true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_transport::{HidDeviceDescriptor, MockHidTransport, PacketValidator};

    fn make_reply(cmd_id: u8, address: u16, data: &[u8]) -> Vec<u8> {
        encode_frame(cmd_id, address, data).unwrap().to_vec()
    }

    #[test]
    fn checksum_and_dpi_codecs_round_trip() {
        let tx = encode_frame(cmd::BATTERY, 0, &[]).unwrap();
        assert_eq!(tx[0], 0x08);
        assert_eq!(tx[1], 0x04);
        assert_eq!(tx[16], 0x49);
        assert!(is_valid_payload_checksum(&tx[1..]));

        for &(sensor, dpi) in &[
            (AtkMouseSensor::Paw3950Ultra, 800u32),
            (AtkMouseSensor::Paw3950Ultra, 16_000),
            (AtkMouseSensor::Paw3950Ultra, 42_000),
            (AtkMouseSensor::Paw3950, 3200),
            (AtkMouseSensor::Paw3950, 36_000),
            (AtkMouseSensor::Core26K, 1600),
            (AtkMouseSensor::Paw3311, 1200),
        ] {
            let encoded = encode_dpi_stage(sensor, dpi, dpi);
            assert_eq!(decode_dpi_stage(sensor, &encoded), Some((dpi, dpi)));
        }
    }

    #[test]
    fn validator_blocks_factory_reset_and_upgrade_mode() {
        let validator = atk_mouse_validator();
        let ok = encode_frame(cmd::BATTERY, 0, &[]).unwrap();
        assert!(validator.validate_outgoing(ReportKind::Output, &ok).is_ok());

        let reset = encode_frame(cmd::RESTORE_FACTORY, 0, &[]).unwrap();
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &reset)
                .is_err()
        );

        let upgrade = encode_frame(cmd::ENTER_UPGRADE, 0, &[]).unwrap();
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &upgrade)
                .is_err()
        );
    }

    #[test]
    fn reads_vgn_f2_battery_pointer_and_writes_settings() {
        let desc = HidDeviceDescriptor::new(
            VGN_COMPX_VID,
            0xFB56,
            ATK_MOUSE_USAGE_PAGE,
            ATK_MOUSE_USAGE_ID,
            1,
            "mock://vgn/f2",
        );
        let mut mock = MockHidTransport::new(desc);
        // Firmware reply v1.30
        mock.push_input_response(make_reply(cmd::GET_FIRMWARE, 0, &[1, 30]));
        // Battery reply: 87%, discharging, 4050 mV (0x0FD2)
        mock.push_input_response(make_reply(cmd::BATTERY, 0, &[87, 0, 0x0F, 0xD2]));
        // System block: 4000 Hz (0x20), 2 stages, active stage 1
        let sys = [
            0x20,
            0x55u8.wrapping_sub(0x20),
            2,
            0x55u8.wrapping_sub(2),
            1,
            0x55u8.wrapping_sub(1),
        ];
        mock.push_input_response(make_reply(cmd::READ_EEPROM, reg::POLLING_RATE, &sys));
        // Stage 0: 800 DPI, Stage 1: 1600 DPI
        mock.push_input_response(make_reply(
            cmd::READ_EEPROM,
            reg::DPI_STAGES,
            &encode_dpi_stage(AtkMouseSensor::Core26K, 800, 800),
        ));
        mock.push_input_response(make_reply(
            cmd::READ_EEPROM,
            reg::DPI_STAGES + 4,
            &encode_dpi_stage(AtkMouseSensor::Core26K, 1600, 1600),
        ));
        // LOD = Low (3)
        mock.push_input_response(make_reply(
            cmd::READ_EEPROM,
            reg::LOD,
            &[3, 0x55u8.wrapping_sub(3)],
        ));
        // Advanced: debounce 2ms, motion sync on, sleep 6 (60s), angle snap off, ripple on
        let adv = [
            2,
            0x55u8.wrapping_sub(2),
            1,
            0x55u8.wrapping_sub(1),
            6,
            0x55u8.wrapping_sub(6),
            0,
            0x55,
            1,
            0x55u8.wrapping_sub(1),
        ];
        mock.push_input_response(make_reply(cmd::READ_EEPROM, reg::DEBOUNCE_MS, &adv));
        // Polling rate write reply (8000 Hz = 0x40)
        mock.push_input_response(make_reply(
            cmd::WRITE_EEPROM,
            reg::POLLING_RATE,
            &[0x40, 0x55u8.wrapping_sub(0x40)],
        ));

        let mut dev = AtkMouseDevice::open(mock, false, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.30"));
        let batt = dev.read_battery().unwrap();
        assert_eq!(batt.percentage, Some(87));
        assert_eq!(batt.voltage_mv, Some(4050));

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.polling_rate, PollingRateHz::Hz4000);
        assert_eq!(snap.dpi_stages.len(), 2);
        assert_eq!(snap.dpi_stages[1].x.value(), 1600);
        assert_eq!(snap.lod, Some(LiftOffDistance::Low));
        assert_eq!(snap.debounce_ms, Some(DebounceMs::new(2)));
        assert_eq!(snap.motion_sync, Some(true));
        assert_eq!(snap.angle_snapping, Some(false));
        assert_eq!(snap.ripple_control, Some(true));

        dev.set_polling_rate(PollingRateHz::Hz8000).unwrap();
    }
}
