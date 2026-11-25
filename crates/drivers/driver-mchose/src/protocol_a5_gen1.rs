//! MCHOSE A5 Gen1 (A5 Pro Max, VID `0x2023`) XVI feature-report codecs.
//!
//! Evidence: `OpenMouse-Project/mouse-protocol` `src/mchose/a5-gen1.ts`
//! (`mchoseA5Gen1EncodeRequest`, `mchoseA5Gen1DecodeReply`, `mchoseA5Gen1ReplyMatches`,
//! `mchoseA5Gen1DecodeFirmware`, `mchoseA5Gen1DecodeDpi`, `mchoseA5Gen1EncodeDpi`,
//! `MCHOSE_A5_GEN1_*` constants) and `src/drivers/mchose/a5-gen1-hid.ts`
//! (`MchoseA5ProMaxHidClient`: the `(len, page, cmd)` table below and `POLLING_CODES`).
//! See `docs/protocols/mchose.md` §2.3.

use agnoperiph_core::{CoreError, CoreResult, PollingRateHz};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub const VID: u16 = 0x2023;
pub const USAGE_PAGE: u16 = 0xFFFF;
pub const REPORT_ID: u8 = 0x00;
pub const BODY_LEN: usize = 64;
pub const FEATURE_LEN: usize = BODY_LEN + 1;
pub const ROUTE: u8 = 0x02;
pub const REPLY_MAGIC: u8 = 0xA1;
pub const MAX_DATA_LEN: usize = 57;

pub const PROFILE_COUNT: u8 = 3;
pub const DPI_STAGES: usize = 6;
pub const DPI_MIN: u32 = 50;
pub const DPI_MAX: u32 = 26_000;
pub const DPI_STEP: u32 = 50;
pub const DEBOUNCE_MAX_MS: u8 = 15;

pub const WIRED_PID: u16 = 0xF019;
pub const RECEIVER_1K_PID: u16 = 0xF013;
/// Listed upstream but marked unverified on hardware.
pub const RECEIVER_4K_PID: u16 = 0xF015;

/// One documented `(len, page, cmd)` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Command {
    pub len: u8,
    pub page: u8,
    pub cmd: u8,
}

const fn c(len: u8, page: u8, cmd: u8) -> Command {
    Command { len, page, cmd }
}

pub mod command {
    use super::{Command, c};

    pub const READ_FIRMWARE: Command = c(16, 0, 0x81);
    pub const READ_BATTERY: Command = c(2, 0, 0x83);
    pub const READ_PROFILE: Command = c(1, 0, 0x85);
    pub const READ_SLEEP: Command = c(2, 0, 0x87);
    pub const READ_DEBOUNCE: Command = c(2, 0, 0x88);
    pub const READ_POLLING: Command = c(1, 1, 0x80);
    pub const READ_DPI: Command = c(10, 1, 0x81);
    pub const READ_ACTIVE_STAGE: Command = c(2, 1, 0x82);
    pub const READ_ANGLE_SNAP: Command = c(1, 1, 0x84);
    pub const READ_LOD: Command = c(1, 1, 0x88);
    pub const READ_MOTION_SYNC: Command = c(1, 1, 0x89);
    pub const READ_RIPPLE: Command = c(1, 1, 0x8A);

    pub const SET_PROFILE: Command = c(1, 0, 0x05);
    pub const SET_DEBOUNCE: Command = c(2, 0, 0x08);
    pub const SET_POLLING: Command = c(1, 1, 0x00);
    /// Length is the encoded table length (`2 + 4 × stages`).
    pub const SET_DPI_PAGE: u8 = 1;
    pub const SET_DPI_CMD: u8 = 0x01;
    pub const SET_ACTIVE_STAGE: Command = c(2, 1, 0x02);
    pub const SET_ANGLE_SNAP: Command = c(1, 1, 0x04);
    pub const SET_LOD: Command = c(1, 1, 0x08);
    pub const SET_MOTION_SYNC: Command = c(1, 1, 0x09);
    pub const SET_RIPPLE: Command = c(1, 1, 0x0A);
}

/// `(page, cmd)` pairs this driver may send.
const ALLOWED: &[(u8, u8)] = &[
    (0, 0x81),
    (0, 0x83),
    (0, 0x85),
    (0, 0x87),
    (0, 0x88),
    (1, 0x80),
    (1, 0x81),
    (1, 0x82),
    (1, 0x84),
    (1, 0x88),
    (1, 0x89),
    (1, 0x8A),
    (0, 0x05),
    (0, 0x08),
    (1, 0x00),
    (1, 0x01),
    (1, 0x02),
    (1, 0x04),
    (1, 0x08),
    (1, 0x09),
    (1, 0x0A),
];

impl Command {
    /// Reads have the high bit set; writes are the same id without it.
    pub const fn is_mutating(self) -> bool {
        self.cmd & 0x80 == 0
    }
}

pub fn is_a5_device(vid: u16, pid: u16) -> bool {
    vid == VID && matches!(pid, WIRED_PID | RECEIVER_1K_PID | RECEIVER_4K_PID)
}

/// Encodes `[0x00, body[64]]` with `body[2] = route`, `body[3] = len`, `body[4] = page`,
/// `body[5] = cmd`, `body[6..] = data`.
pub fn encode_request(command: Command, data: &[u8]) -> CoreResult<[u8; FEATURE_LEN]> {
    if data.len() > MAX_DATA_LEN {
        return Err(CoreError::Validation(format!(
            "MCHOSE A5 data of {} bytes exceeds {MAX_DATA_LEN}",
            data.len()
        )));
    }
    if !ALLOWED.contains(&(command.page, command.cmd)) {
        return Err(CoreError::Validation(format!(
            "undocumented MCHOSE A5 page 0x{:02X} command 0x{:02X}",
            command.page, command.cmd
        )));
    }
    let mut frame = [0u8; FEATURE_LEN];
    frame[0] = REPORT_ID;
    let body = &mut frame[1..];
    body[2] = ROUTE;
    body[3] = command.len;
    body[4] = command.page;
    body[5] = command.cmd;
    body[6..6 + data.len()].copy_from_slice(data);
    Ok(frame)
}

/// Normalizes a `GET_FEATURE` buffer to the 64-byte body (strips a leading report id).
pub fn decode_reply(raw: &[u8]) -> &[u8] {
    if raw.len() == FEATURE_LEN && raw[0] == REPORT_ID {
        &raw[1..]
    } else {
        &raw[..raw.len().min(BODY_LEN)]
    }
}

pub fn reply_matches(body: &[u8], command: Command) -> bool {
    body.len() >= 6 && body[0] == REPLY_MAGIC && body[4] == command.page && body[5] == command.cmd
}

pub fn decode_firmware(body: &[u8]) -> String {
    (6..10)
        .map(|ix| format!("{:02}", body.get(ix).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(".")
}

/// DPI table: `body[7] = count`, then 4 bytes per stage starting at `body[8]` (X big-endian
/// first). Returns `(count, stages)`.
pub fn decode_dpi(body: &[u8]) -> (u8, Vec<u16>) {
    let raw_count = body.get(7).copied().unwrap_or(0);
    let count = if raw_count == 0 {
        DPI_STAGES
    } else {
        usize::from(raw_count).clamp(1, DPI_STAGES)
    };
    let stages = (0..count)
        .map(|ix| {
            let offset = 8 + ix * 4;
            u16::from_be_bytes([
                body.get(offset).copied().unwrap_or(0),
                body.get(offset + 1).copied().unwrap_or(0),
            ])
        })
        .collect();
    (u8::try_from(count).unwrap_or(1), stages)
}

/// `[profile, count, (x_hi, x_lo, x_hi, x_lo) × count]` — upstream writes the value twice.
pub fn encode_dpi(profile: u8, stages: &[u16]) -> CoreResult<Vec<u8>> {
    if stages.is_empty() || stages.len() > DPI_STAGES {
        return Err(CoreError::OutOfRange {
            field: "dpi_stages",
            actual: stages.len().to_string(),
            expected: format!("1..={DPI_STAGES}"),
        });
    }
    let mut out = vec![profile, u8::try_from(stages.len()).unwrap_or(1)];
    for dpi in stages {
        let [hi, lo] = dpi.to_be_bytes();
        out.extend_from_slice(&[hi, lo, hi, lo]);
    }
    Ok(out)
}

pub fn dpi_write_command(data_len: usize) -> Command {
    let len = u8::try_from(data_len.min(MAX_DATA_LEN)).unwrap_or(0);
    c(len, command::SET_DPI_PAGE, command::SET_DPI_CMD)
}

pub const fn encode_polling(rate: PollingRateHz) -> Option<u8> {
    match rate {
        PollingRateHz::Hz125 => Some(0x08),
        PollingRateHz::Hz250 => Some(0x04),
        PollingRateHz::Hz500 => Some(0x02),
        PollingRateHz::Hz1000 => Some(0x01),
        _ => None,
    }
}

pub const fn decode_polling(code: u8) -> Option<PollingRateHz> {
    match code {
        0x08 => Some(PollingRateHz::Hz125),
        0x04 => Some(PollingRateHz::Hz250),
        0x02 => Some(PollingRateHz::Hz500),
        0x01 => Some(PollingRateHz::Hz1000),
        _ => None,
    }
}

pub fn polling_rates(wired: bool) -> Vec<PollingRateHz> {
    if wired {
        vec![
            PollingRateHz::Hz125,
            PollingRateHz::Hz500,
            PollingRateHz::Hz1000,
        ]
    } else {
        vec![
            PollingRateHz::Hz125,
            PollingRateHz::Hz250,
            PollingRateHz::Hz500,
            PollingRateHz::Hz1000,
        ]
    }
}

fn verify_a5_frame(kind: ReportKind, report: &[u8]) -> Result<(), String> {
    if kind != ReportKind::Feature {
        return Err(format!("MCHOSE A5 only sends feature reports, got {kind}"));
    }
    let body = &report[1..];
    if body[2] != ROUTE {
        return Err(format!(
            "MCHOSE A5 route must be 0x02, got 0x{:02X}",
            body[2]
        ));
    }
    if usize::from(body[3]) > MAX_DATA_LEN {
        return Err(format!(
            "MCHOSE A5 length byte {} exceeds {MAX_DATA_LEN}",
            body[3]
        ));
    }
    if !ALLOWED.contains(&(body[4], body[5])) {
        return Err(format!(
            "undocumented MCHOSE A5 page 0x{:02X} command 0x{:02X} blocked",
            body[4], body[5]
        ));
    }
    Ok(())
}

/// Strict 65-byte report-`0x00` feature validator with a `(page, cmd)` allow-list.
pub fn a5_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![FEATURE_LEN], vec![REPORT_ID]).with_checksum(verify_a5_frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_transport::PacketValidator;

    #[test]
    fn request_layout_and_validator() {
        let frame = encode_request(command::READ_DPI, &[1, 6]).unwrap();
        assert_eq!(frame.len(), 65);
        assert_eq!(&frame[..9], &[0x00, 0, 0, 0x02, 10, 1, 0x81, 1, 6]);
        let validator = a5_validator();
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &frame)
                .is_ok()
        );
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &frame)
                .is_err()
        );

        let mut bad_route = frame;
        bad_route[3] = 0x01;
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &bad_route)
                .is_err()
        );
        let mut unknown = frame;
        unknown[6] = 0x7F;
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &unknown)
                .is_err()
        );
        assert!(encode_request(c(1, 2, 0x00), &[]).is_err());
    }

    #[test]
    fn dpi_codec_is_big_endian() {
        let data = encode_dpi(1, &[800, 26_000]).unwrap();
        assert_eq!(
            data,
            vec![1, 2, 0x03, 0x20, 0x03, 0x20, 0x65, 0x90, 0x65, 0x90]
        );

        let mut body = vec![0u8; 64];
        body[0] = REPLY_MAGIC;
        body[4] = 1;
        body[5] = 0x81;
        body[7] = 2;
        body[8..16].copy_from_slice(&data[2..]);
        let (count, stages) = decode_dpi(&body);
        assert_eq!(count, 2);
        assert_eq!(stages, vec![800, 26_000]);
        assert!(reply_matches(&body, command::READ_DPI));
        assert!(!reply_matches(&body, command::READ_ACTIVE_STAGE));
    }

    #[test]
    fn reply_normalization_and_firmware() {
        let mut raw = vec![0u8; 65];
        raw[1] = REPLY_MAGIC;
        raw[7..11].copy_from_slice(&[1, 2, 3, 14]);
        let body = decode_reply(&raw);
        assert_eq!(body.len(), 64);
        assert_eq!(decode_firmware(body), "01.02.03.14");
        assert!(command::SET_POLLING.is_mutating());
        assert!(!command::READ_POLLING.is_mutating());
        assert_eq!(decode_polling(0x04), Some(PollingRateHz::Hz250));
        assert_eq!(encode_polling(PollingRateHz::Hz8000), None);
    }
}
