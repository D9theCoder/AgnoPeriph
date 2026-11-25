//! MCHOSE V3 (A7 V3 / A5 V3 / K7 V2 / K5 / R7 generation) `0x4D` output/input codecs.
//!
//! Evidence: `OpenMouse-Project/mouse-protocol` `src/mchose/v3.ts`
//! (`mchoseV3Encode`, `mchoseV3Payload`, `mchoseV3DecodeDeviceInfo`,
//! `mchoseV3DecodeSettings`, `mchoseV3RateIndexToOption`, `mchoseV3OptionToRateIndex`,
//! `MCHOSE_V3_SENSOR_*`, `mchoseV3DecodeDpi`, `mchoseV3EncodeDpiTable`,
//! `mchoseV3EncodeSingleDpi`, `mchoseV3EncodeLiftOff`, `mchoseV3IsRejection`,
//! `mchoseV3IsBusy`, `MCHOSE_V3_PRODUCTS`) and `src/drivers/mchose/v3-hid.ts`
//! (`MchoseV3HidClient`). See `docs/protocols/mchose.md` §2.2, §3, §4.
//!
//! Upstream notes its V3 *encoders* have not been exercised against hardware; the
//! decoders were confirmed on an A7 V3 Ultra+.

use agnoperiph_core::{CoreError, CoreResult, PollingRateHz};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub use crate::protocol_v2::{CONFIG_USAGE as USAGE, CONFIG_USAGE_PAGE as USAGE_PAGE, MCHOSE_VID};

pub const REPORT_ID: u8 = 0x4D;
pub const FRAME_LEN: usize = 64;
pub const BODY_LEN: usize = FRAME_LEN - 1;
/// `7` header bytes + data + `1` checksum byte must fit in the 63-byte body.
pub const MAX_DATA_LEN: usize = BODY_LEN - 8;

const VERSION_OFFSET: usize = 0;
const FLAGS_OFFSET: usize = 1;
const LENGTH_OFFSET: usize = 2;
const COMMAND_LO_OFFSET: usize = 3;
const COMMAND_HI_OFFSET: usize = 4;
const SEQUENCE_OFFSET: usize = 6;
const DATA_OFFSET: usize = 7;

const PROTOCOL_VERSION: u8 = 0x01;
const FLAG_CHECKSUM: u8 = 0x01;

pub const DPI_STAGES: usize = 6;
pub const DPI_MIN: u32 = 200;
pub const DPI_STEP: u32 = 50;
pub const DEBOUNCE_MAX_MS: u8 = 20;
pub const AXIS_X: u8 = 0;
/// `0x0901` target byte selecting the mouse rather than the receiver.
pub const VERSION_TARGET_MOUSE: u8 = 0;

pub mod command {
    pub const READ_SETTINGS: u16 = 0x0002;
    pub const READ_DPI: u16 = 0x0003;
    pub const READ_LIFT_OFF: u16 = 0x0009;
    pub const READ_DEVICE_INFO: u16 = 0x0900;
    pub const READ_VERSION: u16 = 0x0901;
    pub const WRITE_SETTINGS: u16 = 0x0102;
    pub const WRITE_DPI: u16 = 0x0103;
    pub const WRITE_SINGLE_DPI: u16 = 0x0104;
    /// Factory reset. Never sent: blocked by [`super::v3_validator`].
    pub const FACTORY_RESET: u16 = 0x0105;
    pub const WRITE_LIFT_OFF: u16 = 0x0109;
}

pub const fn is_allowed(cmd: u16) -> bool {
    matches!(
        cmd,
        command::READ_SETTINGS
            | command::READ_DPI
            | command::READ_LIFT_OFF
            | command::READ_DEVICE_INFO
            | command::READ_VERSION
            | command::WRITE_SETTINGS
            | command::WRITE_DPI
            | command::WRITE_SINGLE_DPI
            | command::WRITE_LIFT_OFF
    )
}

pub const fn is_mutating(cmd: u16) -> bool {
    matches!(
        cmd,
        command::WRITE_SETTINGS
            | command::WRITE_DPI
            | command::WRITE_SINGLE_DPI
            | command::WRITE_LIFT_OFF
            | command::FACTORY_RESET
    )
}

/// XOR of `body[from..=to]`.
fn checksum(body: &[u8], from: usize, to: usize) -> u8 {
    body[from..=to].iter().fold(0, |acc, b| acc ^ b)
}

/// Encodes the full 64-byte frame `[0x4D, body[63]]` with trailing XOR checksum.
pub fn encode_frame(cmd: u16, data: &[u8]) -> CoreResult<[u8; FRAME_LEN]> {
    if data.len() > MAX_DATA_LEN {
        return Err(CoreError::Validation(format!(
            "MCHOSE V3 command 0x{cmd:04X} data of {} bytes exceeds {MAX_DATA_LEN}",
            data.len()
        )));
    }
    let mut frame = [0u8; FRAME_LEN];
    frame[0] = REPORT_ID;
    let body = &mut frame[1..];
    body[VERSION_OFFSET] = PROTOCOL_VERSION;
    body[FLAGS_OFFSET] = FLAG_CHECKSUM;
    body[LENGTH_OFFSET] = u8::try_from(data.len()).unwrap_or(0);
    let [lo, hi] = cmd.to_le_bytes();
    body[COMMAND_LO_OFFSET] = lo;
    body[COMMAND_HI_OFFSET] = hi;
    body[DATA_OFFSET..DATA_OFFSET + data.len()].copy_from_slice(data);
    body[DATA_OFFSET + data.len()] = checksum(body, FLAGS_OFFSET, SEQUENCE_OFFSET + data.len());
    Ok(frame)
}

/// Strips the `0x4D` report id from an input report; other report ids (motion, battery
/// pushes) are not part of this protocol and yield `None`.
pub fn body_of(frame: &[u8]) -> Option<&[u8]> {
    match frame.split_first() {
        Some((&REPORT_ID, body)) if body.len() > COMMAND_HI_OFFSET => Some(body),
        _ => None,
    }
}

pub fn reply_command(body: &[u8]) -> Option<u16> {
    if body.len() <= COMMAND_HI_OFFSET {
        return None;
    }
    Some(u16::from_le_bytes([
        body[COMMAND_LO_OFFSET],
        body[COMMAND_HI_OFFSET],
    ]))
}

/// Returns the data slice of a reply to `cmd`, verifying the checksum when flagged.
pub fn payload(body: &[u8], cmd: u16) -> Option<Vec<u8>> {
    if reply_command(body)? != cmd {
        return None;
    }
    let len = usize::from(body[LENGTH_OFFSET]);
    if DATA_OFFSET + len > body.len() {
        return None;
    }
    if body[FLAGS_OFFSET] == FLAG_CHECKSUM {
        let expected = checksum(body, FLAGS_OFFSET, SEQUENCE_OFFSET + len);
        if body.get(DATA_OFFSET + len) != Some(&expected) {
            return None;
        }
    }
    Some(body[DATA_OFFSET..DATA_OFFSET + len].to_vec())
}

/// Firmware refusal: command `0x0000`, flags clear, zero length.
pub fn is_rejection(body: &[u8]) -> bool {
    reply_command(body) == Some(0x0000) && body[FLAGS_OFFSET] == 0 && body[LENGTH_OFFSET] == 0
}

/// A lone `0xFF` payload means "ask again" (receiver present, mouse unreachable).
pub fn is_busy(payload: &[u8]) -> bool {
    payload == [0xFF]
}

/// `0x0900` identity and power.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceInfo {
    pub vid: u16,
    /// Not a reliable model id (an A7 V3 Ultra+ reports `0x4026`); used only as fallback.
    pub pid: u16,
    pub profile_count: u8,
    pub connected: bool,
    pub charging: bool,
    pub battery_percent: u8,
    pub game_mode: u8,
}

pub fn decode_device_info(data: &[u8]) -> Option<DeviceInfo> {
    if data.len() < 14 {
        return None;
    }
    Some(DeviceInfo {
        vid: u16::from_le_bytes([data[0], data[1]]),
        pid: u16::from_le_bytes([data[2], data[3]]),
        profile_count: data[4],
        connected: data[10] != 0,
        charging: data[11] != 0,
        battery_percent: data[12],
        game_mode: data[13],
    })
}

/// Maps a device rate slot to the model's option index (slot `1` is hidden).
pub const fn slot_to_option(slot: u8) -> u8 {
    if slot > 1 { slot - 1 } else { slot }
}

/// Maps a model option index to the device rate slot written on the wire.
pub const fn option_to_slot(option: u8) -> u8 {
    if option > 0 { option + 1 } else { option }
}

pub const SENSOR_MODE_MASK: u8 = 0x03;
pub const SENSOR_RIPPLE: u8 = 0x04;
pub const SENSOR_ANGLE_SNAP: u8 = 0x08;
pub const SENSOR_MOTION_SYNC: u8 = 0x10;
pub const SENSOR_LOD_MASK: u8 = 0x60;
pub const SENSOR_LOD_SHIFT: u8 = 5;
pub const SENSOR_GLASS: u8 = 0x80;

/// `0x0002` settings block, kept as raw bytes so writes echo everything not edited.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Settings {
    pub profile_index: u8,
    /// Raw `rate_slot << 4 | dpi_index` byte for the wired link.
    pub wired: u8,
    /// Raw `rate_slot << 4 | dpi_index` byte for the wireless link.
    pub wireless: u8,
    pub sleep_minutes: u8,
    pub sleep_mode: u8,
    pub sensor: u8,
    pub angle_tuning: u8,
    pub left_debounce_ms: u8,
    pub right_debounce_ms: u8,
    /// Unknown trailing bytes (likely other buttons' debounce), echoed verbatim.
    pub extra: Vec<u8>,
}

impl Settings {
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 9 {
            return None;
        }
        Some(Self {
            profile_index: data[0],
            wired: data[1],
            wireless: data[2],
            sleep_minutes: data[3],
            sleep_mode: data[4],
            sensor: data[5],
            angle_tuning: data[6],
            left_debounce_ms: data[7],
            right_debounce_ms: data[8],
            extra: data[9..].to_vec(),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![
            self.profile_index,
            self.wired,
            self.wireless,
            self.sleep_minutes,
            self.sleep_mode,
            self.sensor,
            self.angle_tuning,
            self.left_debounce_ms,
            self.right_debounce_ms,
        ];
        out.extend_from_slice(&self.extra);
        out
    }

    const fn link_byte(&self, wired: bool) -> u8 {
        if wired { self.wired } else { self.wireless }
    }

    /// Option index into the model's rate list for the live link half.
    pub const fn rate_option(&self, wired: bool) -> u8 {
        slot_to_option(self.link_byte(wired) >> 4)
    }

    /// Rewrites only the rate nibble of the live link half; the DPI nibble and the other
    /// link's byte stay verbatim.
    pub fn set_rate_option(&mut self, wired: bool, option: u8) -> CoreResult<()> {
        let slot = option_to_slot(option);
        if slot > 0x0F {
            return Err(CoreError::OutOfRange {
                field: "polling_rate_slot",
                actual: slot.to_string(),
                expected: "0..=15".to_owned(),
            });
        }
        let target = if wired {
            &mut self.wired
        } else {
            &mut self.wireless
        };
        *target = (slot << 4) | (*target & 0x0F);
        Ok(())
    }

    pub const fn lod_bits(&self) -> u8 {
        (self.sensor & SENSOR_LOD_MASK) >> SENSOR_LOD_SHIFT
    }

    pub fn set_lod_bits(&mut self, index: u8) -> CoreResult<()> {
        let max = SENSOR_LOD_MASK >> SENSOR_LOD_SHIFT;
        if index > max {
            return Err(CoreError::OutOfRange {
                field: "lod_index",
                actual: index.to_string(),
                expected: format!("0..={max}"),
            });
        }
        self.sensor = (self.sensor & !SENSOR_LOD_MASK) | (index << SENSOR_LOD_SHIFT);
        Ok(())
    }

    pub const fn has_flag(&self, mask: u8) -> bool {
        self.sensor & mask != 0
    }

    /// Sets or clears one sensor flag, preserving every other bit.
    pub fn set_flag(&mut self, mask: u8, enabled: bool) {
        if enabled {
            self.sensor |= mask;
        } else {
            self.sensor &= !mask;
        }
    }
}

/// `0x0003` one-axis DPI table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DpiTable {
    pub profile_index: u8,
    pub axis: u8,
    pub stage_count: u8,
    pub active_stage: u8,
    pub has_separate_y: bool,
    pub stages: [u16; DPI_STAGES],
}

impl DpiTable {
    /// Read layout: `[profile, axis, count, active, sep_y, 6×u16 LE]`.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 5 + DPI_STAGES * 2 {
            return None;
        }
        let mut stages = [0u16; DPI_STAGES];
        for (ix, slot) in stages.iter_mut().enumerate() {
            *slot = u16::from_le_bytes([data[5 + ix * 2], data[6 + ix * 2]]);
        }
        Some(Self {
            profile_index: data[0],
            axis: data[1],
            stage_count: data[2],
            active_stage: data[3],
            has_separate_y: data[4] == 1,
            stages,
        })
    }

    /// Write layout: `[profile, axis, sep_y, count, active, 6×u16 LE]`.
    pub fn encode_write(&self) -> CoreResult<Vec<u8>> {
        let count = usize::from(self.stage_count);
        if !(1..=DPI_STAGES).contains(&count) {
            return Err(CoreError::OutOfRange {
                field: "dpi_stages",
                actual: count.to_string(),
                expected: format!("1..={DPI_STAGES}"),
            });
        }
        if self.active_stage >= self.stage_count {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: self.active_stage.to_string(),
                expected: format!("0..{}", self.stage_count),
            });
        }
        let mut out = vec![
            self.profile_index,
            self.axis,
            u8::from(self.has_separate_y),
            self.stage_count,
            self.active_stage,
        ];
        for stage in self.stages {
            out.extend_from_slice(&stage.to_le_bytes());
        }
        Ok(out)
    }
}

/// `0x0104` single-stage write: `[profile, stage, axis, dpi_lo, dpi_hi]`.
pub fn encode_single_dpi(profile: u8, stage: u8, axis: u8, dpi: u16) -> CoreResult<Vec<u8>> {
    if usize::from(stage) >= DPI_STAGES {
        return Err(CoreError::OutOfRange {
            field: "dpi_stage",
            actual: stage.to_string(),
            expected: format!("0..{DPI_STAGES}"),
        });
    }
    let [lo, hi] = dpi.to_le_bytes();
    Ok(vec![profile, stage, axis, lo, hi])
}

/// `0x0109` lift-off write for five-step models: `[profile, lod_index]`.
pub const fn encode_lift_off(profile: u8, index: u8) -> [u8; 2] {
    [profile, index]
}

/// One V3-generation model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Product {
    pub name: &'static str,
    pub pid: u16,
    pub dpi_max: u32,
    /// Lift-off steps in tenths of a millimetre, in firmware index order.
    pub lods_tenths_mm: &'static [u8],
    pub max_polling_hz: u16,
    /// `true` when lift-off lives behind `0x0009`/`0x0109` instead of sensor bits 5..6.
    pub lod_command: bool,
}

const LOD_TWO: &[u8] = &[10, 20];
const LOD_THREE: &[u8] = &[7, 10, 20];
const LOD_FIVE: &[u8] = &[7, 9, 12, 14, 17];

const fn product(
    name: &'static str,
    pid: u16,
    dpi_max: u32,
    lods_tenths_mm: &'static [u8],
    max_polling_hz: u16,
    lod_command: bool,
) -> Product {
    Product {
        name,
        pid,
        dpi_max,
        lods_tenths_mm,
        max_polling_hz,
        lod_command,
    }
}

pub const PRODUCTS: &[Product] = &[
    product("A5 V3 Pro", 0x4035, 26_000, LOD_TWO, 8000, false),
    product("A5 V3 Ultra+", 0x4026, 42_000, LOD_THREE, 8000, false),
    product("A5 V3 Ultra+ (3955)", 0x4034, 50_000, LOD_FIVE, 8000, true),
    product("K7 V2 Pro+", 0x4027, 42_000, LOD_THREE, 8000, false),
    product("K7 V2 Ultra+", 0x4028, 50_000, LOD_FIVE, 8000, true),
    product("A7 V3", 0x4030, 26_000, LOD_TWO, 8000, false),
    product("A7 V3 Pro", 0x4031, 42_000, LOD_THREE, 8000, false),
    product("A7 V3 Pro+", 0x4032, 42_000, LOD_THREE, 8000, false),
    product("A7 V3 Ultra+", 0x4033, 50_000, LOD_FIVE, 8000, true),
    product("K5 Pro", 0x4037, 26_000, LOD_TWO, 8000, false),
    product("K5 Ultra", 0x4038, 50_000, LOD_FIVE, 8000, true),
    product("R7 Ultra", 0x4036, 50_000, LOD_FIVE, 8000, true),
    product("V7", 0x402A, 26_000, LOD_TWO, 1000, false),
    product("G3 V3", 0x4029, 12_000, LOD_TWO, 1000, false),
];

pub const RECEIVER_24G_PID: u16 = 0x1014;
pub const RECEIVER_1K_PID: u16 = 0x1016;
pub const RECEIVER_8K_PID: u16 = 0x1018;
pub const RECEIVER_PIDS: &[u16] = &[RECEIVER_24G_PID, RECEIVER_1K_PID, RECEIVER_8K_PID];

pub fn is_v3_device(vid: u16, pid: u16) -> bool {
    vid == MCHOSE_VID && (RECEIVER_PIDS.contains(&pid) || PRODUCTS.iter().any(|p| p.pid == pid))
}

pub fn is_receiver(pid: u16) -> bool {
    RECEIVER_PIDS.contains(&pid)
}

/// Resolves the model by USB product string first (longest name wins), then by the
/// wired host PID, then by the `0x0900` PID.
pub fn find_product(
    product_name: Option<&str>,
    host_pid: Option<u16>,
    info_pid: Option<u16>,
) -> Option<&'static Product> {
    if let Some(name) = product_name.map(|n| n.trim().to_uppercase())
        && !name.is_empty()
    {
        let mut candidates: Vec<&'static Product> = PRODUCTS.iter().collect();
        candidates.sort_by_key(|p| std::cmp::Reverse(p.name.len()));
        if let Some(found) = candidates
            .into_iter()
            .find(|p| name.contains(&p.name.to_uppercase()))
        {
            return Some(found);
        }
    }
    [host_pid, info_pid]
        .into_iter()
        .flatten()
        .find_map(|pid| PRODUCTS.iter().find(|p| p.pid == pid))
}

const RATES_1K: &[PollingRateHz] = &[
    PollingRateHz::Hz125,
    PollingRateHz::Hz500,
    PollingRateHz::Hz1000,
];
const RATES_8K: &[PollingRateHz] = &[
    PollingRateHz::Hz125,
    PollingRateHz::Hz500,
    PollingRateHz::Hz1000,
    PollingRateHz::Hz2000,
    PollingRateHz::Hz4000,
    PollingRateHz::Hz8000,
];

pub fn polling_rates(max_polling_hz: u16) -> &'static [PollingRateHz] {
    if max_polling_hz >= 8000 {
        RATES_8K
    } else {
        RATES_1K
    }
}

fn verify_v3_frame(kind: ReportKind, report: &[u8]) -> Result<(), String> {
    if kind != ReportKind::Output {
        return Err(format!("MCHOSE V3 only sends output reports, got {kind}"));
    }
    let body = &report[1..];
    if body[VERSION_OFFSET] != PROTOCOL_VERSION {
        return Err(format!(
            "MCHOSE V3 protocol version must be 0x01, got 0x{:02X}",
            body[VERSION_OFFSET]
        ));
    }
    if body[FLAGS_OFFSET] != FLAG_CHECKSUM {
        return Err("MCHOSE V3 frames must carry the trailing checksum flag".to_owned());
    }
    let len = usize::from(body[LENGTH_OFFSET]);
    if len > MAX_DATA_LEN {
        return Err(format!(
            "MCHOSE V3 data length {len} exceeds {MAX_DATA_LEN}"
        ));
    }
    let cmd = u16::from_le_bytes([body[COMMAND_LO_OFFSET], body[COMMAND_HI_OFFSET]]);
    if cmd == command::FACTORY_RESET {
        return Err("MCHOSE V3 factory reset (0x0105) is blocked by AgnoPeriph".to_owned());
    }
    if !is_allowed(cmd) {
        return Err(format!(
            "undocumented MCHOSE V3 command 0x{cmd:04X} blocked"
        ));
    }
    let expected = checksum(body, FLAGS_OFFSET, SEQUENCE_OFFSET + len);
    if body[DATA_OFFSET + len] != expected {
        return Err(format!(
            "MCHOSE V3 XOR checksum mismatch: got 0x{:02X}, expected 0x{expected:02X}",
            body[DATA_OFFSET + len]
        ));
    }
    Ok(())
}

/// Strict 64-byte `0x4D` output validator: framing, checksum, and command allow-list
/// (factory reset `0x0105` is always rejected).
pub fn v3_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![FRAME_LEN], vec![REPORT_ID]).with_checksum(verify_v3_frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_transport::PacketValidator;

    #[test]
    fn worked_example_read_settings_frame_matches_spec() {
        let frame = encode_frame(command::READ_SETTINGS, &[]).unwrap();
        assert_eq!(
            &frame[..9],
            &[0x4D, 0x01, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03]
        );
        assert!(frame[9..].iter().all(|b| *b == 0));
        assert!(
            v3_validator()
                .validate_outgoing(ReportKind::Output, &frame)
                .is_ok()
        );
    }

    #[test]
    fn validator_blocks_factory_reset_and_malformed_frames() {
        let validator = v3_validator();
        let reset = encode_frame(command::FACTORY_RESET, &[0x00]).unwrap();
        let err = validator
            .validate_outgoing(ReportKind::Output, &reset)
            .unwrap_err();
        assert!(err.to_string().contains("0x0105"));

        let mut bad_sum = encode_frame(command::READ_DPI, &[0, 0]).unwrap();
        bad_sum[10] ^= 0x01;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &bad_sum)
                .is_err()
        );

        let buttons = encode_frame(0x0101, &[0, 0, 6]).unwrap();
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &buttons)
                .is_err()
        );

        let ok = encode_frame(command::READ_DEVICE_INFO, &[]).unwrap();
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &ok)
                .is_err()
        );
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &ok[..63])
                .is_err()
        );
        let mut wrong_id = ok;
        wrong_id[0] = 0x4C;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &wrong_id)
                .is_err()
        );
        assert!(encode_frame(command::WRITE_SETTINGS, &[0u8; 56]).is_err());
    }

    #[test]
    fn reply_payload_checks_command_and_checksum() {
        let frame = encode_frame(command::READ_LIFT_OFF, &[3]).unwrap();
        let body = body_of(&frame).unwrap();
        assert_eq!(payload(body, command::READ_LIFT_OFF), Some(vec![3]));
        assert_eq!(payload(body, command::READ_DPI), None);
        let mut corrupt = frame;
        corrupt[9] ^= 0xFF;
        assert_eq!(
            payload(body_of(&corrupt).unwrap(), command::READ_LIFT_OFF),
            None
        );

        let mut rejection = [0u8; 64];
        rejection[0] = REPORT_ID;
        rejection[1] = 0x01;
        rejection[7] = 0xFF;
        assert!(is_rejection(body_of(&rejection).unwrap()));
        assert!(is_busy(&[0xFF]));
        assert!(!is_busy(&[0xFF, 0x01]));
    }

    #[test]
    fn rate_slot_mapping_skips_slot_one() {
        assert_eq!(option_to_slot(0), 0);
        assert_eq!(option_to_slot(1), 2);
        assert_eq!(option_to_slot(5), 6);
        assert_eq!(slot_to_option(0), 0);
        assert_eq!(slot_to_option(1), 1);
        assert_eq!(slot_to_option(2), 1);
        assert_eq!(slot_to_option(6), 5);
    }

    #[test]
    fn settings_edits_preserve_other_link_extra_and_unknown_bits() {
        let data = [
            0x01,
            0x32,
            0x63,
            5,
            0,
            0b1010_0101,
            0xF1,
            8,
            8,
            8,
            8,
            8,
            8,
            8,
            8,
            8,
            8,
            8,
            8,
        ];
        let mut settings = Settings::decode(&data).unwrap();
        assert_eq!(settings.rate_option(true), 2);
        assert_eq!(settings.rate_option(false), 5);
        assert_eq!(settings.lod_bits(), 1);
        assert!(settings.has_flag(SENSOR_RIPPLE));

        settings.set_rate_option(false, 1).unwrap();
        settings.set_flag(SENSOR_MOTION_SYNC, true);
        settings.set_lod_bits(2).unwrap();
        let encoded = settings.encode();
        assert_eq!(encoded[1], 0x32, "wired byte untouched");
        assert_eq!(encoded[2], 0x23, "only rate nibble of wireless changed");
        assert_eq!(encoded[5], 0b1101_0101, "glass + mode bits preserved");
        assert_eq!(&encoded[9..], &data[9..], "extra echoed verbatim");
        assert!(settings.set_lod_bits(4).is_err());
    }

    #[test]
    fn dpi_table_read_and_write_orders_differ() {
        let mut data = vec![1, 0, 4, 2, 1];
        for dpi in [400u16, 800, 1600, 3200, 6400, 12_800] {
            data.extend_from_slice(&dpi.to_le_bytes());
        }
        let table = DpiTable::decode(&data).unwrap();
        assert_eq!(table.stage_count, 4);
        assert_eq!(table.active_stage, 2);
        assert!(table.has_separate_y);
        assert_eq!(table.stages[5], 12_800);
        let write = table.encode_write().unwrap();
        assert_eq!(&write[..5], &[1, 0, 1, 4, 2]);
        assert_eq!(&write[5..], &data[5..]);

        let mut bad = table;
        bad.active_stage = 4;
        assert!(bad.encode_write().is_err());
        assert_eq!(
            encode_single_dpi(0, 3, AXIS_X, 1600).unwrap(),
            vec![0, 3, 0, 0x40, 0x06]
        );
    }

    #[test]
    fn product_lookup_prefers_name() {
        let found = find_product(Some("MCHOSE A7 V3 Ultra+"), None, Some(0x4026)).unwrap();
        assert_eq!(found.pid, 0x4033);
        assert!(found.lod_command);
        let by_pid = find_product(Some("2.4G Receiver"), None, Some(0x4031)).unwrap();
        assert_eq!(by_pid.name, "A7 V3 Pro");
        assert!(find_product(None, None, None).is_none());
        assert_eq!(polling_rates(1000).len(), 3);
        assert!(is_v3_device(MCHOSE_VID, RECEIVER_8K_PID));
        assert!(!is_v3_device(MCHOSE_VID, 0x1012));
    }
}
