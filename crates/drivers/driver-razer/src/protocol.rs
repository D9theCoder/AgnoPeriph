//! Razer 90-byte HID Feature report codec, XOR checksum, and per-PID capability catalog.
//!
//! Wire layout verified against `docs/protocols/razer-hid.md` and
//! `OpenMouse-Project/mouse-protocol` (`src/razer/codec.ts`, `src/razer/devices.ts`).

use agnoperiph_core::{
    BatteryStatus, ChargingState, ConnectionKind, CoreError, CoreResult, DpiStage, DpiValue,
    LiftOffDistance, PollingRateHz, RgbColor, RgbEffect,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

/// Razer USB Vendor ID.
pub const RAZER_VID: u16 = 0x1532;
/// Unnumbered HID Feature Report ID (`0x00`).
pub const RAZER_REPORT_ID: u8 = 0x00;
/// Wire payload size (90 bytes, excluding the leading `hidapi` Report ID byte).
pub const RAZER_PACKET_LEN: usize = 90;
/// Full `hidapi` feature buffer size (`[0x00, packet[0..90]]`).
pub const RAZER_HID_BUFFER_LEN: usize = RAZER_PACKET_LEN + 1;

pub const RAZER_USAGE_PAGE_MOUSE: u16 = 0x0001;
pub const RAZER_USAGE_ID_MOUSE: u16 = 0x0002;

pub const TX_1F: u8 = 0x1F;
pub const TX_3F: u8 = 0x3F;
pub const TX_FF: u8 = 0xFF;

pub const RAZER_STORAGE_VARSTORE: u8 = 0x01;
pub const RAZER_STORAGE_NOSTORE: u8 = 0x00;

pub const RAZER_MAX_DPI_STAGES: u8 = 5;
pub const RAZER_MIN_DPI: u32 = 100;
pub const RAZER_DPI_STEP: u32 = 50;

const ARGS_OFFSET: usize = 8;
const CHECKSUM_INDEX: usize = 88;
const STAGE_OFFSET: usize = 3;
const STAGE_LENGTH: usize = 7;

/// Status bytes reported at byte `0` of a 90-byte Razer reply.
pub mod status {
    pub const REQUEST: u8 = 0x00;
    pub const BUSY: u8 = 0x01;
    pub const OK: u8 = 0x02;
    pub const FAILURE: u8 = 0x03;
    pub const TIMEOUT: u8 = 0x04;
    pub const UNSUPPORTED: u8 = 0x05;
}

/// LED identifiers in OpenRazer's extended/standard matrix families.
pub mod led {
    pub const ALL: u8 = 0x00;
    pub const SCROLL_WHEEL: u8 = 0x01;
    pub const LOGO: u8 = 0x04;
    pub const BACKLIGHT: u8 = 0x05;
}

/// Lighting command family implemented by a Razer mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RazerLightingFamily {
    None,
    StandardMatrix,
    ExtendedMatrix,
}

/// Static capability descriptor for one Razer mouse/receiver PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RazerProductProfile {
    pub pid: u16,
    pub model_name: &'static str,
    pub connection: ConnectionKind,
    pub transaction_id: u8,
    pub max_dpi: u32,
    pub has_battery: bool,
    pub high_rate_polling: bool,
    pub max_polling_hz: u16,
    pub extended_polling_commit_tx: Option<u8>,
    pub supports_lift_off: bool,
    pub dpi_storage: u8,
    pub lighting: RazerLightingFamily,
    pub vendor_control_interface: bool,
}

/// Audited Razer mouse & HyperSpeed/HyperPolling receiver catalog.
pub const RAZER_PRODUCTS: &[RazerProductProfile] = &[
    RazerProductProfile {
        pid: 0x00C0,
        model_name: "Razer Viper V3 Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 35_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: true,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00C1,
        model_name: "Razer Viper V3 Pro (8K Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 35_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 8000,
        extended_polling_commit_tx: Some(TX_1F),
        supports_lift_off: true,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00A5,
        model_name: "Razer Viper V2 Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: true,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00A6,
        model_name: "Razer Viper V2 Pro (Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: true,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00B6,
        model_name: "Razer DeathAdder V3 Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00B7,
        model_name: "Razer DeathAdder V3 Pro (Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00C2,
        model_name: "Razer DeathAdder V3 Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00C3,
        model_name: "Razer DeathAdder V3 Pro (Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00B2,
        model_name: "Razer DeathAdder V3 (Wired 8K)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: false,
        high_rate_polling: true,
        max_polling_hz: 8000,
        extended_polling_commit_tx: Some(TX_1F),
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x00BE,
        model_name: "Razer DeathAdder V4 Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 45_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 8000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00BF,
        model_name: "Razer DeathAdder V4 Pro (8K Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 45_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 8000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00B8,
        model_name: "Razer Viper V3 HyperSpeed",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: true,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x0091,
        model_name: "Razer Viper 8KHz",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_FF,
        max_dpi: 20_000,
        has_battery: false,
        high_rate_polling: true,
        max_polling_hz: 8000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x00B3,
        model_name: "Razer HyperPolling Wireless Dongle",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 8000,
        extended_polling_commit_tx: Some(TX_FF),
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x0099,
        model_name: "Razer Basilisk V3",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 26_000,
        has_battery: false,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::ExtendedMatrix,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x00CB,
        model_name: "Razer Basilisk V3 35K",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 35_000,
        has_battery: false,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::ExtendedMatrix,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x00AA,
        model_name: "Razer Basilisk V3 Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00AB,
        model_name: "Razer Basilisk V3 Pro (Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00A7,
        model_name: "Razer Naga V2 Pro (Wired)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x00A8,
        model_name: "Razer Naga V2 Pro (Receiver)",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 30_000,
        has_battery: true,
        high_rate_polling: true,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: false,
    },
    RazerProductProfile {
        pid: 0x0094,
        model_name: "Razer Orochi V2",
        connection: ConnectionKind::WirelessDongle24Ghz,
        transaction_id: TX_1F,
        max_dpi: 18_000,
        has_battery: true,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x0084,
        model_name: "Razer DeathAdder V2",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_3F,
        max_dpi: 20_000,
        has_battery: false,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_NOSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x006E,
        model_name: "Razer DeathAdder Essential",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_3F,
        max_dpi: 6_400,
        has_battery: false,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x0098,
        model_name: "Razer DeathAdder Essential (2021)",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_3F,
        max_dpi: 6_400,
        has_battery: false,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::None,
        vendor_control_interface: true,
    },
    RazerProductProfile {
        pid: 0x004C,
        model_name: "Razer Diamondback Chroma",
        connection: ConnectionKind::WiredUsb,
        transaction_id: TX_FF,
        max_dpi: 16_000,
        has_battery: false,
        high_rate_polling: false,
        max_polling_hz: 1000,
        extended_polling_commit_tx: None,
        supports_lift_off: false,
        dpi_storage: RAZER_STORAGE_VARSTORE,
        lighting: RazerLightingFamily::StandardMatrix,
        vendor_control_interface: true,
    },
];

/// Looks up a Razer product profile by USB PID.
pub fn lookup_product(pid: u16) -> Option<RazerProductProfile> {
    RAZER_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

/// Command specification for one Razer feature exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RazerCommand {
    pub command_class: u8,
    pub command_id: u8,
    pub data_size: u8,
    pub args: Vec<u8>,
}

impl RazerCommand {
    pub fn new(command_class: u8, command_id: u8, data_size: u8, args: Vec<u8>) -> Self {
        Self {
            command_class,
            command_id,
            data_size,
            args,
        }
    }

    pub const fn is_getter(&self) -> bool {
        (self.command_id & 0x80) != 0
    }
}

/// Computes the Razer XOR checksum over bytes `2..88` of a 90-byte packet.
pub fn compute_checksum(packet: &[u8]) -> u8 {
    packet
        .get(2..CHECKSUM_INDEX)
        .unwrap_or(&[])
        .iter()
        .fold(0u8, |acc, &b| acc ^ b)
}

/// Encodes a Razer command into a 91-byte `hidapi` Feature Report buffer (`[0x00, packet[0..90]]`).
pub fn encode_request(
    command: &RazerCommand,
    transaction_id: u8,
) -> CoreResult<[u8; RAZER_HID_BUFFER_LEN]> {
    if command.args.len() > 80 || usize::from(command.data_size) > 80 {
        return Err(CoreError::Validation(
            "Razer command payload exceeds 80 bytes".to_owned(),
        ));
    }
    let mut buf = [0u8; RAZER_HID_BUFFER_LEN];
    buf[0] = RAZER_REPORT_ID;
    let pkt = &mut buf[1..];
    pkt[0] = status::REQUEST;
    pkt[1] = transaction_id;
    pkt[5] = command.data_size;
    pkt[6] = command.command_class;
    pkt[7] = command.command_id;
    pkt[ARGS_OFFSET..ARGS_OFFSET + command.args.len()].copy_from_slice(&command.args);
    pkt[CHECKSUM_INDEX] = compute_checksum(pkt);
    Ok(buf)
}

/// Strict pre-transmission validator enforcing 91-byte Feature reports with Report ID `0x00`
/// and a matching XOR checksum at packet offset `88` (buffer index `89`).
pub fn razer_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![RAZER_HID_BUFFER_LEN], vec![RAZER_REPORT_ID]).with_checksum(
        |kind, report| {
            if kind != ReportKind::Feature {
                return Err(format!(
                    "Razer commands must use Feature reports, got {kind}"
                ));
            }
            let pkt = &report[1..];
            let expected = compute_checksum(pkt);
            let actual = pkt[CHECKSUM_INDEX];
            if actual != expected {
                return Err(format!(
                    "Razer XOR checksum mismatch: expected 0x{expected:02X}, got 0x{actual:02X}"
                ));
            }
            Ok(())
        },
    )
}

/// Decoded status of a Razer reply frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RazerReply {
    Ok(Vec<u8>),
    Busy,
    Unsupported,
    Stale,
    Error(u8),
}

/// Decodes a 90-byte (or 91-byte with leading `0x00` report ID) Razer Feature reply.
pub fn decode_response(raw: &[u8], command: &RazerCommand) -> CoreResult<RazerReply> {
    let pkt = if raw.len() == RAZER_HID_BUFFER_LEN && raw[0] == RAZER_REPORT_ID {
        &raw[1..]
    } else if raw.len() == RAZER_PACKET_LEN {
        raw
    } else {
        return Err(CoreError::Protocol(format!(
            "unexpected Razer reply length {} (expected 90 or 91)",
            raw.len()
        )));
    };

    let expected_crc = compute_checksum(pkt);
    if pkt[CHECKSUM_INDEX] != expected_crc {
        return Err(CoreError::Protocol(format!(
            "Razer reply checksum mismatch: expected 0x{expected_crc:02X}, got 0x{:02X}",
            pkt[CHECKSUM_INDEX]
        )));
    }

    match pkt[0] {
        status::BUSY => Ok(RazerReply::Busy),
        status::UNSUPPORTED => Ok(RazerReply::Unsupported),
        status::OK => {
            if pkt[6] != command.command_class || pkt[7] != command.command_id {
                return Ok(RazerReply::Stale);
            }
            let len = usize::from(pkt[5]).min(RAZER_PACKET_LEN - ARGS_OFFSET);
            Ok(RazerReply::Ok(pkt[ARGS_OFFSET..ARGS_OFFSET + len].to_vec()))
        }
        other => Ok(RazerReply::Error(other)),
    }
}

pub fn cmd_read_firmware() -> RazerCommand {
    RazerCommand::new(0x00, 0x81, 0x02, vec![])
}

pub fn cmd_read_serial() -> RazerCommand {
    RazerCommand::new(0x00, 0x82, 0x16, vec![])
}

pub fn cmd_read_battery() -> RazerCommand {
    RazerCommand::new(0x07, 0x80, 0x02, vec![])
}

pub fn cmd_read_charging() -> RazerCommand {
    RazerCommand::new(0x07, 0x84, 0x02, vec![])
}

pub fn cmd_read_dpi(storage: u8) -> RazerCommand {
    RazerCommand::new(0x04, 0x85, 0x07, vec![storage])
}

pub fn cmd_set_dpi(x: u16, y: u16, storage: u8) -> RazerCommand {
    let [xh, xl] = x.to_be_bytes();
    let [yh, yl] = y.to_be_bytes();
    RazerCommand::new(0x04, 0x05, 0x07, vec![storage, xh, xl, yh, yl, 0, 0])
}

pub fn cmd_read_dpi_stages(storage: u8) -> RazerCommand {
    RazerCommand::new(0x04, 0x86, 0x26, vec![storage])
}

pub fn cmd_set_dpi_stages(
    stages: &[DpiStage],
    active_zero_based: u8,
    storage: u8,
) -> CoreResult<RazerCommand> {
    if stages.is_empty() || stages.len() > usize::from(RAZER_MAX_DPI_STAGES) {
        return Err(CoreError::OutOfRange {
            field: "dpi_stages",
            actual: stages.len().to_string(),
            expected: format!("1..={RAZER_MAX_DPI_STAGES}"),
        });
    }
    let mut args = vec![0u8; 0x26];
    args[0] = storage;
    args[1] = active_zero_based.saturating_add(1);
    args[2] = u8::try_from(stages.len()).unwrap_or(1);
    for (ix, stage) in stages.iter().enumerate() {
        let offset = STAGE_OFFSET + ix * STAGE_LENGTH;
        let x = u16::try_from(stage.x.value()).map_err(|_| {
            CoreError::Validation(format!("Razer DPI X {} exceeds u16", stage.x.value()))
        })?;
        let y = u16::try_from(stage.effective_y().value()).map_err(|_| {
            CoreError::Validation(format!(
                "Razer DPI Y {} exceeds u16",
                stage.effective_y().value()
            ))
        })?;
        let [xh, xl] = x.to_be_bytes();
        let [yh, yl] = y.to_be_bytes();
        args[offset] = u8::try_from(ix).unwrap_or(0);
        args[offset + 1] = xh;
        args[offset + 2] = xl;
        args[offset + 3] = yh;
        args[offset + 4] = yl;
    }
    Ok(RazerCommand::new(0x04, 0x06, 0x26, args))
}

pub fn cmd_read_polling_legacy() -> RazerCommand {
    RazerCommand::new(0x00, 0x85, 0x01, vec![])
}

pub fn cmd_set_polling_legacy(rate: PollingRateHz) -> CoreResult<RazerCommand> {
    let hz = rate.as_hz();
    if hz == 0 || 1000 % hz != 0 {
        return Err(CoreError::OutOfRange {
            field: "polling_rate",
            actual: rate.to_string(),
            expected: "125, 250, 500, or 1000 Hz".to_owned(),
        });
    }
    let divisor = u8::try_from(1000 / hz).unwrap_or(1);
    Ok(RazerCommand::new(0x00, 0x05, 0x01, vec![divisor]))
}

pub fn cmd_read_polling_extended() -> RazerCommand {
    RazerCommand::new(0x00, 0xC0, 0x02, vec![0x00])
}

pub fn cmd_set_polling_extended(rate: PollingRateHz, selector: u8) -> CoreResult<RazerCommand> {
    let hz = rate.as_hz();
    if hz == 0 || 8000 % hz != 0 {
        return Err(CoreError::OutOfRange {
            field: "polling_rate",
            actual: rate.to_string(),
            expected: "divisor of 8000 Hz".to_owned(),
        });
    }
    let divisor = u8::try_from(8000 / hz).unwrap_or(1);
    Ok(RazerCommand::new(0x00, 0x40, 0x02, vec![selector, divisor]))
}

pub fn cmd_read_lift_off() -> RazerCommand {
    RazerCommand::new(0x0B, 0x85, 0x05, vec![])
}

pub fn cmd_set_tracking_distance(lod: LiftOffDistance) -> CoreResult<RazerCommand> {
    let level = match lod {
        LiftOffDistance::Low => 0x00,
        LiftOffDistance::Medium => 0x01,
        LiftOffDistance::High => 0x02,
        other @ LiftOffDistance::TenthsMm(_) => {
            return Err(CoreError::OutOfRange {
                field: "lod",
                actual: other.to_string(),
                expected: "Low, Medium, or High".to_owned(),
            });
        }
    };
    Ok(RazerCommand::new(
        0x0B,
        0x0B,
        0x04,
        vec![0x00, 0x04, 0x01, level],
    ))
}

pub fn cmd_read_standard_brightness() -> RazerCommand {
    RazerCommand::new(
        0x03,
        0x83,
        0x03,
        vec![RAZER_STORAGE_VARSTORE, led::BACKLIGHT],
    )
}

pub fn cmd_set_standard_brightness(percent: u8) -> RazerCommand {
    let raw = u8::try_from((u16::from(percent.min(100)) * 255 + 50) / 100).unwrap_or(255);
    RazerCommand::new(
        0x03,
        0x03,
        0x03,
        vec![RAZER_STORAGE_VARSTORE, led::BACKLIGHT, raw],
    )
}

pub fn cmd_set_standard_effect(effect: RgbEffect) -> CoreResult<RazerCommand> {
    match effect {
        RgbEffect::Off => Ok(RazerCommand::new(0x03, 0x0A, 0x01, vec![0x00])),
        RgbEffect::Wave { .. } => Ok(RazerCommand::new(0x03, 0x0A, 0x02, vec![0x01, 0x01])),
        RgbEffect::Reactive { color, speed } => {
            let s = speed.clamp(1, 4);
            Ok(RazerCommand::new(
                0x03,
                0x0A,
                0x05,
                vec![0x02, s, color.r, color.g, color.b],
            ))
        }
        RgbEffect::Breathing { color, .. } => Ok(RazerCommand::new(
            0x03,
            0x0A,
            0x08,
            vec![0x03, 0x01, color.r, color.g, color.b],
        )),
        RgbEffect::SpectrumCycle { .. } => Ok(RazerCommand::new(0x03, 0x0A, 0x01, vec![0x04])),
        RgbEffect::Static { color } => Ok(RazerCommand::new(
            0x03,
            0x0A,
            0x04,
            vec![0x06, color.r, color.g, color.b],
        )),
        RgbEffect::Preset { .. } => Err(CoreError::UnsupportedCapability("rgb_preset")),
    }
}

pub fn cmd_read_extended_brightness(led_id: u8) -> RazerCommand {
    RazerCommand::new(0x0F, 0x84, 0x03, vec![RAZER_STORAGE_VARSTORE, led_id])
}

pub fn cmd_set_extended_brightness(led_id: u8, percent: u8) -> RazerCommand {
    let raw = u8::try_from((u16::from(percent.min(100)) * 255 + 50) / 100).unwrap_or(255);
    RazerCommand::new(0x0F, 0x04, 0x03, vec![RAZER_STORAGE_VARSTORE, led_id, raw])
}

pub fn cmd_set_extended_effect(led_id: u8, effect: RgbEffect) -> CoreResult<RazerCommand> {
    let mut args = vec![RAZER_STORAGE_VARSTORE, led_id];
    match effect {
        RgbEffect::Off => args.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]),
        RgbEffect::Static { color } => {
            args.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, color.r, color.g, color.b]);
        }
        RgbEffect::Breathing { color, .. } => {
            args.extend_from_slice(&[0x02, 0x01, 0x00, 0x01, color.r, color.g, color.b]);
        }
        RgbEffect::SpectrumCycle { .. } => args.extend_from_slice(&[0x03, 0x00, 0x00, 0x00]),
        RgbEffect::Wave { .. } => args.extend_from_slice(&[0x04, 0x01, 0x28, 0x00]),
        RgbEffect::Reactive { color, speed } => {
            let s = speed.clamp(1, 4);
            args.extend_from_slice(&[0x05, 0x00, s, 0x01, color.r, color.g, color.b]);
        }
        RgbEffect::Preset { .. } => return Err(CoreError::UnsupportedCapability("rgb_preset")),
    }
    let data_size = u8::try_from(args.len()).unwrap_or(6);
    Ok(RazerCommand::new(0x0F, 0x02, data_size, args))
}

pub fn decode_firmware(args: &[u8]) -> Option<String> {
    if args.len() < 2 {
        return None;
    }
    Some(format!("v{}.{}", args[0], args[1]))
}

pub fn decode_serial(args: &[u8]) -> Option<String> {
    let end = args.iter().position(|&b| b == 0).unwrap_or(args.len());
    let text = String::from_utf8_lossy(&args[..end]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

pub fn decode_battery(level_args: &[u8], charging_args: Option<&[u8]>) -> BatteryStatus {
    let raw = level_args.get(1).copied().unwrap_or(0);
    let pct = u8::try_from((u16::from(raw) * 100 + 127) / 255).unwrap_or(100);
    let state = match charging_args.and_then(|a| a.get(1).copied()) {
        Some(1) => ChargingState::Charging,
        Some(_) => ChargingState::Discharging,
        None => ChargingState::Unknown,
    };
    BatteryStatus::new(Some(pct.min(100)), None, state, true)
}

pub fn decode_dpi_xy(args: &[u8]) -> CoreResult<(DpiValue, DpiValue)> {
    if args.len() < 5 {
        return Err(CoreError::Protocol(
            "Razer DPI response shorter than 5 bytes".to_owned(),
        ));
    }
    let x = u32::from(u16::from_be_bytes([args[1], args[2]]));
    let y = u32::from(u16::from_be_bytes([args[3], args[4]]));
    Ok((DpiValue::new(x)?, DpiValue::new(y)?))
}

pub fn decode_dpi_stages(args: &[u8]) -> CoreResult<(u8, Vec<DpiStage>)> {
    if args.len() < 3 {
        return Err(CoreError::Protocol(
            "Razer DPI stage table response shorter than 3 bytes".to_owned(),
        ));
    }
    let active_1based = args[1];
    let count = usize::from(args[2]).min(usize::from(RAZER_MAX_DPI_STAGES));
    let mut stages = Vec::with_capacity(count);
    for ix in 0..count {
        let offset = STAGE_OFFSET + ix * STAGE_LENGTH;
        if offset + 4 >= args.len() {
            break;
        }
        let x = u32::from(u16::from_be_bytes([args[offset + 1], args[offset + 2]]));
        let y = u32::from(u16::from_be_bytes([args[offset + 3], args[offset + 4]]));
        if x == 0 {
            continue;
        }
        let mut stage = DpiStage::new(u8::try_from(ix).unwrap_or(0), DpiValue::new(x)?);
        if y > 0 && y != x {
            stage = stage.with_y(DpiValue::new(y)?);
        }
        stages.push(stage);
    }
    if stages.is_empty() {
        return Err(CoreError::Protocol(
            "Razer DPI stage table contained no valid stages".to_owned(),
        ));
    }
    let active = active_1based
        .saturating_sub(1)
        .min(u8::try_from(stages.len().saturating_sub(1)).unwrap_or(0));
    Ok((active, stages))
}

pub fn decode_polling_legacy(args: &[u8]) -> CoreResult<PollingRateHz> {
    let divisor = args.first().copied().unwrap_or(0);
    if divisor == 0 {
        return Err(CoreError::Protocol(
            "Razer legacy polling divisor was 0".to_owned(),
        ));
    }
    PollingRateHz::from_hz(1000 / u16::from(divisor))
}

pub fn decode_polling_extended(args: &[u8]) -> CoreResult<PollingRateHz> {
    let divisor = args.get(1).copied().unwrap_or(0);
    if divisor == 0 {
        return Err(CoreError::Protocol(
            "Razer extended polling divisor was 0".to_owned(),
        ));
    }
    PollingRateHz::from_hz(8000 / u16::from(divisor))
}

pub fn decode_lift_off(args: &[u8]) -> Option<LiftOffDistance> {
    match args.get(2).copied() {
        Some(0) => Some(LiftOffDistance::Low),
        Some(1) => Some(LiftOffDistance::Medium),
        Some(2) => Some(LiftOffDistance::High),
        _ => None,
    }
}

pub fn decode_brightness_percent(args: &[u8]) -> Option<u8> {
    let raw = args.get(2).copied()?;
    let pct = u8::try_from((u16::from(raw) * 100 + 127) / 255).unwrap_or(100);
    Some(pct.min(100))
}

pub fn supported_rates_for(profile: RazerProductProfile) -> Vec<PollingRateHz> {
    if profile.max_polling_hz >= 8000 {
        vec![
            PollingRateHz::Hz125,
            PollingRateHz::Hz500,
            PollingRateHz::Hz1000,
            PollingRateHz::Hz2000,
            PollingRateHz::Hz4000,
            PollingRateHz::Hz8000,
        ]
    } else {
        vec![
            PollingRateHz::Hz125,
            PollingRateHz::Hz500,
            PollingRateHz::Hz1000,
        ]
    }
}

pub fn default_static_color() -> RgbColor {
    RgbColor::new(0x00, 0xFF, 0x00)
}
