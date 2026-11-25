//! MCHOSE V2 ("M HUB" A7 V2 / L7 family) bit-inverted feature-report codecs.
//!
//! Evidence: `OpenMouse-Project/mouse-protocol` `src/mchose/index.ts`
//! (`mchoseEncodeCommand`, `mchoseDecodeReply`, `mchoseDecodeConfig`,
//! `mchoseEncodeConfigWrite`, `mchoseEncodePerformance`, `mchoseEncodeSleep`,
//! `MCHOSE_PRODUCTS`, `MCHOSE_POLLING_RATES`) and `src/drivers/mchose/hid.ts`
//! (`MchoseHidClient`). See `docs/protocols/mchose.md` §2.1, §3, §4.

use agnoperiph_core::{CoreError, CoreResult, PollingRateHz};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub const MCHOSE_VID: u16 = 0x3837;
/// MCHOSE's older "RealTek" VID, only ever matched together with explicit PIDs.
pub const REALTEK_VID: u16 = 0x5253;
pub const CONFIG_USAGE_PAGE: u16 = 0xFF01;
pub const CONFIG_USAGE: u16 = 0x0001;

pub const SHORT_REPORT_ID: u8 = 0x11;
pub const SHORT_TOKENS: usize = 20;
pub const LONG_REPORT_ID: u8 = 0x12;
pub const LONG_TOKENS: usize = 64;
pub const BODY_LEN: usize = 64;
/// `[report_id, body[64]]`.
pub const FEATURE_LEN: usize = BODY_LEN + 1;

pub const DPI_STAGES: usize = 6;
pub const DPI_MIN: u32 = 50;
pub const DPI_STEP: u32 = 50;
pub const DEBOUNCE_MAX_MS: u8 = 20;
pub const PROFILE_COUNT: u8 = 3;
pub const SLEEP_MAX_MINUTES: u8 = 0xFF;

/// Bytes of the `0x67` reply that carry named fields; the rest is preserved verbatim.
pub const CONFIG_MIN_LEN: usize = 20;
pub const ANGLE_TUNING_OFFSET: usize = 49;
pub const BATTERY_MIN_LEN: usize = 11;

pub mod command {
    pub const IDENTITY: u8 = 0x03;
    pub const VERSION: u8 = 0x04;
    pub const BATTERY: u8 = 0x06;
    pub const SET_SLEEP: u8 = 0x0A;
    pub const SET_PERFORMANCE: u8 = 0x42;
    pub const WRITE_CONFIG: u8 = 0x57;
    pub const SET_PROFILE: u8 = 0x58;
    pub const CONFIG: u8 = 0x67;
    pub const READ_PROFILE_NAME: u8 = 0x68;
}

pub const SENSOR_LOD_MASK: u8 = 0x03;
pub const SENSOR_RIPPLE: u8 = 0x04;
pub const SENSOR_ANGLE_SNAP: u8 = 0x08;
pub const SENSOR_MOTION_SYNC: u8 = 0x10;

/// Short (`0x11`, 20 tokens) or long (`0x12`, 64 tokens) feature channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Short,
    Long,
}

impl Channel {
    pub const fn report_id(self) -> u8 {
        match self {
            Self::Short => SHORT_REPORT_ID,
            Self::Long => LONG_REPORT_ID,
        }
    }

    pub const fn token_count(self) -> usize {
        match self {
            Self::Short => SHORT_TOKENS,
            Self::Long => LONG_TOKENS,
        }
    }

    pub const fn from_report_id(report_id: u8) -> Option<Self> {
        match report_id {
            SHORT_REPORT_ID => Some(Self::Short),
            LONG_REPORT_ID => Some(Self::Long),
            _ => None,
        }
    }
}

/// Documented channel for every command this driver may send (allow-list).
pub const fn channel_for(cmd: u8) -> Option<Channel> {
    match cmd {
        command::IDENTITY
        | command::VERSION
        | command::BATTERY
        | command::SET_SLEEP
        | command::SET_PERFORMANCE
        | command::SET_PROFILE => Some(Channel::Short),
        command::CONFIG | command::WRITE_CONFIG | command::READ_PROFILE_NAME => Some(Channel::Long),
        _ => None,
    }
}

pub const fn is_mutating(cmd: u8) -> bool {
    matches!(
        cmd,
        command::WRITE_CONFIG
            | command::SET_PROFILE
            | command::SET_PERFORMANCE
            | command::SET_SLEEP
    )
}

/// Encodes `[report_id, ~tokens[..token_count], 0...]` for `SET_FEATURE`.
///
/// Every token slot up to the channel's token count is inverted (missing tokens are `0x00`,
/// so they go out as `0xFF`); the remainder of the 64-byte body stays `0x00`.
pub fn encode_command(tokens: &[u8]) -> CoreResult<[u8; FEATURE_LEN]> {
    let Some(&cmd) = tokens.first() else {
        return Err(CoreError::Validation("empty MCHOSE V2 command".to_owned()));
    };
    let channel = channel_for(cmd).ok_or_else(|| {
        CoreError::Validation(format!("undocumented MCHOSE V2 command 0x{cmd:02X}"))
    })?;
    if tokens.len() > channel.token_count() {
        return Err(CoreError::Validation(format!(
            "MCHOSE V2 command 0x{cmd:02X} has {} tokens, channel allows {}",
            tokens.len(),
            channel.token_count()
        )));
    }
    let mut frame = [0u8; FEATURE_LEN];
    frame[0] = channel.report_id();
    for ix in 0..channel.token_count() {
        frame[1 + ix] = tokens.get(ix).copied().unwrap_or(0) ^ 0xFF;
    }
    Ok(frame)
}

/// Un-inverted reply from a `GET_FEATURE` buffer `[report_id, ~cmd, ~payload...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reply {
    pub command: u8,
    pub payload: Vec<u8>,
}

pub fn decode_reply(raw: &[u8]) -> Option<Reply> {
    if raw.len() < 3 {
        return None;
    }
    Some(Reply {
        command: raw[1] ^ 0xFF,
        payload: raw[2..].iter().map(|b| b ^ 0xFF).collect(),
    })
}

fn u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([
        data.get(offset).copied().unwrap_or(0),
        data.get(offset + 1).copied().unwrap_or(0),
    ])
}

/// `0x03` receiver identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Identity {
    pub bonded: bool,
    pub vid: u16,
    pub pid: u16,
    pub connected: bool,
    pub game_mode: u8,
}

pub fn decode_identity(payload: &[u8]) -> Option<Identity> {
    if payload.len() < 7 {
        return None;
    }
    Some(Identity {
        bonded: payload[0] != 0,
        vid: u16_le(payload, 1),
        pid: u16_le(payload, 3),
        connected: payload[5] != 0,
        game_mode: payload[6],
    })
}

/// `0x04` firmware version: length-prefixed ASCII.
pub fn decode_version(payload: &[u8]) -> Option<String> {
    let len = usize::from(*payload.first()?);
    if len == 0 || payload.len() < 1 + len {
        return None;
    }
    let text = String::from_utf8_lossy(&payload[1..=len]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// `0x06` mouse identity plus battery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Battery {
    pub vid: u16,
    /// The mouse's own model PID, even when read through a receiver.
    pub mouse_pid: u16,
    pub connected: bool,
    pub percent: u8,
    pub charging: bool,
}

pub fn decode_battery(payload: &[u8]) -> Option<Battery> {
    if payload.len() < BATTERY_MIN_LEN {
        return None;
    }
    Some(Battery {
        vid: u16_le(payload, 0),
        mouse_pid: u16_le(payload, 2),
        connected: (payload[8] >> 4) & 0x01 != 0,
        percent: payload[9],
        charging: payload[10] != 0,
    })
}

/// Per-link `rate_index << 4 | dpi_index` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LinkByte {
    pub rate_index: u8,
    pub dpi_index: u8,
}

impl LinkByte {
    pub const fn unpack(raw: u8) -> Self {
        Self {
            rate_index: (raw >> 4) & 0x0F,
            dpi_index: raw & 0x0F,
        }
    }

    pub const fn pack(self) -> u8 {
        ((self.rate_index & 0x0F) << 4) | (self.dpi_index & 0x0F)
    }
}

/// Full `0x67` config payload, kept verbatim so writes echo every untouched byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    raw: Vec<u8>,
}

impl Config {
    /// Accepts a `0x67` payload only when it carries a plausible first DPI stage
    /// (guards against the shared reply buffer handing back another command's data).
    pub fn decode(payload: &[u8]) -> Option<Self> {
        if !looks_like_config(payload) {
            return None;
        }
        Some(Self {
            raw: payload.to_vec(),
        })
    }

    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    pub fn profile_index(&self) -> u8 {
        self.raw[0]
    }

    pub fn wired(&self) -> LinkByte {
        LinkByte::unpack(self.raw[1])
    }

    pub fn wireless(&self) -> LinkByte {
        LinkByte::unpack(self.raw[2])
    }

    pub fn link(&self, wired: bool) -> LinkByte {
        if wired { self.wired() } else { self.wireless() }
    }

    pub fn dpi_stages(&self) -> [u16; DPI_STAGES] {
        let mut out = [0u16; DPI_STAGES];
        for (ix, slot) in out.iter_mut().enumerate() {
            *slot = u16_le(&self.raw, 4 + ix * 2);
        }
        out
    }

    pub fn stage_count(&self) -> u8 {
        self.raw[16]
    }

    pub fn sensor(&self) -> u8 {
        self.raw[17]
    }

    pub fn lod_index(&self) -> u8 {
        self.sensor() & SENSOR_LOD_MASK
    }

    pub fn ripple_control(&self) -> bool {
        self.sensor() & SENSOR_RIPPLE != 0
    }

    pub fn angle_snapping(&self) -> bool {
        self.sensor() & SENSOR_ANGLE_SNAP != 0
    }

    pub fn motion_sync(&self) -> bool {
        self.sensor() & SENSOR_MOTION_SYNC != 0
    }

    pub fn debounce_ms(&self) -> u8 {
        self.raw[18]
    }

    pub fn sleep_minutes(&self) -> u8 {
        self.raw[19]
    }

    pub fn angle_tuning(&self) -> Option<i8> {
        self.raw
            .get(ANGLE_TUNING_OFFSET)
            .map(|b| i8::from_le_bytes([*b]))
    }
}

pub fn looks_like_config(payload: &[u8]) -> bool {
    if payload.len() < CONFIG_MIN_LEN {
        return false;
    }
    let first = u16_le(payload, 4);
    (50..=42_000).contains(&first)
}

/// Fields a `0x57` write may change; everything else is echoed from the `0x67` read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConfigChanges {
    pub wired: Option<LinkByte>,
    pub wireless: Option<LinkByte>,
    pub dpi_stages: Option<[u16; DPI_STAGES]>,
    pub stage_count: Option<u8>,
    pub debounce_ms: Option<u8>,
}

/// Builds the `0x57` token list: the command byte followed by the full `0x67` payload,
/// with only the requested offsets edited.
///
/// Refuses a short read: missing tokens would go out as `0x00` and wipe the blob's tail
/// (button mappings, macros, angle tuning).
pub fn encode_config_write(current: &Config, changes: &ConfigChanges) -> CoreResult<Vec<u8>> {
    if current.raw.len() != LONG_TOKENS - 1 {
        return Err(CoreError::Protocol(format!(
            "MCHOSE V2 config read returned {} bytes, expected {}; refusing a partial write",
            current.raw.len(),
            LONG_TOKENS - 1
        )));
    }
    let mut out = Vec::with_capacity(LONG_TOKENS);
    out.push(command::WRITE_CONFIG);
    out.extend_from_slice(&current.raw);
    if let Some(link) = changes.wired {
        out[1 + 1] = link.pack();
    }
    if let Some(link) = changes.wireless {
        out[1 + 2] = link.pack();
    }
    if let Some(stages) = changes.dpi_stages {
        for (ix, dpi) in stages.iter().enumerate() {
            let [lo, hi] = dpi.to_le_bytes();
            out[1 + 4 + ix * 2] = lo;
            out[1 + 4 + ix * 2 + 1] = hi;
        }
    }
    if let Some(count) = changes.stage_count {
        out[1 + 16] = count;
    }
    if let Some(debounce) = changes.debounce_ms {
        if debounce > DEBOUNCE_MAX_MS {
            return Err(CoreError::OutOfRange {
                field: "debounce_ms",
                actual: debounce.to_string(),
                expected: format!("0..={DEBOUNCE_MAX_MS}"),
            });
        }
        out[1 + 18] = debounce;
    }
    Ok(out)
}

/// Processing toggles for `0x42`; `None` leaves the toggle unchanged on the device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PerformanceChanges {
    pub ripple_control: Option<bool>,
    pub angle_snapping: Option<bool>,
    pub motion_sync: Option<bool>,
}

const fn toggle(value: Option<bool>) -> u8 {
    match value {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

/// `[0x42, lod, ripple, linear, motion_sync, 0, 0, mode, rot_en, rot_val]`.
///
/// Lift-off must always be supplied because index `0` is a real level; mode and angle
/// tuning are sent as `0` ("leave unchanged").
pub fn encode_performance(lod_index: u8, changes: PerformanceChanges) -> CoreResult<Vec<u8>> {
    if lod_index > SENSOR_LOD_MASK {
        return Err(CoreError::OutOfRange {
            field: "lod_index",
            actual: lod_index.to_string(),
            expected: format!("0..={SENSOR_LOD_MASK}"),
        });
    }
    Ok(vec![
        command::SET_PERFORMANCE,
        lod_index,
        toggle(changes.ripple_control),
        toggle(changes.angle_snapping),
        toggle(changes.motion_sync),
        0,
        0,
        0,
        0,
        0,
    ])
}

pub fn encode_sleep(minutes: u8) -> Vec<u8> {
    vec![command::SET_SLEEP, u8::from(minutes > 0), minutes]
}

pub fn encode_set_profile(index: u8) -> CoreResult<Vec<u8>> {
    if index >= PROFILE_COUNT {
        return Err(CoreError::OutOfRange {
            field: "profile_slot",
            actual: index.to_string(),
            expected: format!("0..{PROFILE_COUNT}"),
        });
    }
    Ok(vec![command::SET_PROFILE, index])
}

pub fn encode_read_profile_name(index: u8) -> CoreResult<Vec<u8>> {
    if index >= PROFILE_COUNT {
        return Err(CoreError::OutOfRange {
            field: "profile_slot",
            actual: index.to_string(),
            expected: format!("0..{PROFILE_COUNT}"),
        });
    }
    Ok(vec![command::READ_PROFILE_NAME, index])
}

/// `0x68` reply: `[index, NUL-terminated UTF-8]`.
pub fn decode_profile_name(payload: &[u8]) -> Option<String> {
    let rest = payload.get(1..)?;
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    let name = String::from_utf8_lossy(&rest[..end]).trim().to_owned();
    if name.is_empty() {
        return None;
    }
    // Factory names are Chinese ("默认板载N" = "Default onboard N").
    if let Some(num) = name.strip_prefix("默认板载")
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
    {
        return Some(format!("Default onboard {num}"));
    }
    Some(name)
}

/// One A7 V2-family model (the mouse's own PID, reported inside `0x06`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Product {
    pub name: &'static str,
    pub pid: u16,
    pub dpi_max: u32,
    /// Lift-off steps in tenths of a millimetre, in firmware index order.
    pub lods_tenths_mm: &'static [u8],
    /// Firmware index of the first lift-off step.
    pub lod_index_base: u8,
    pub max_polling_hz: u16,
}

pub const PRODUCTS: &[Product] = &[
    Product {
        name: "A7 V2 Pro",
        pid: 0x4018,
        dpi_max: 26_000,
        lods_tenths_mm: &[10, 20],
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    Product {
        name: "A7 V2 Pro+",
        pid: 0x4023,
        dpi_max: 26_000,
        lods_tenths_mm: &[10, 20],
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    Product {
        name: "A7 V2 Ultra",
        pid: 0x4019,
        dpi_max: 42_000,
        lods_tenths_mm: &[7, 10, 20],
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    Product {
        name: "A7 V2 Ultra+",
        pid: 0x4021,
        dpi_max: 42_000,
        lods_tenths_mm: &[7, 10, 20],
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    Product {
        name: "A7 Ultra",
        pid: 0x1011,
        dpi_max: 42_000,
        lods_tenths_mm: &[7, 10, 20],
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    Product {
        name: "L7 Pro",
        pid: 0x00B0,
        dpi_max: 26_000,
        lods_tenths_mm: &[10, 20],
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    Product {
        name: "L7 Pro+",
        pid: 0x4015,
        dpi_max: 26_000,
        lods_tenths_mm: &[10, 20],
        lod_index_base: 1,
        max_polling_hz: 8000,
    },
];

pub const RECEIVER_24G_PID: u16 = 0x100B;
pub const BLUETOOTH_PID: u16 = 0x100A;
pub const RECEIVER_8K_PID: u16 = 0x1020;

/// Wired PIDs on `0x3837`.
pub const WIRED_MCHOSE_PIDS: &[u16] = &[0x4018, 0x4023, 0x4019, 0x4021, 0x4015];
/// Wired PIDs on `0x5253`.
pub const WIRED_REALTEK_PIDS: &[u16] = &[0x1011, 0x00B0];

/// Returns `true` when `(vid, pid)` is a documented V2 host-facing interface.
pub fn is_v2_device(vid: u16, pid: u16) -> bool {
    match vid {
        MCHOSE_VID => {
            WIRED_MCHOSE_PIDS.contains(&pid)
                || matches!(pid, RECEIVER_24G_PID | BLUETOOTH_PID | RECEIVER_8K_PID)
        }
        REALTEK_VID => WIRED_REALTEK_PIDS.contains(&pid) || pid == RECEIVER_8K_PID,
        _ => false,
    }
}

pub fn product_by_pid(pid: u16) -> Option<&'static Product> {
    PRODUCTS.iter().find(|p| p.pid == pid)
}

/// Resolves the model from the `0x06` mouse PID, falling back to the USB product string
/// (longest name first so "A7 V2 Ultra+" is not swallowed by "A7 V2 Ultra").
pub fn find_product(
    mouse_pid: Option<u16>,
    product_name: Option<&str>,
) -> Option<&'static Product> {
    if let Some(found) = mouse_pid.and_then(product_by_pid) {
        return Some(found);
    }
    let name = product_name?.trim().to_uppercase();
    if name.is_empty() {
        return None;
    }
    let mut candidates: Vec<&'static Product> = PRODUCTS.iter().collect();
    candidates.sort_by_key(|p| std::cmp::Reverse(p.name.len()));
    candidates
        .into_iter()
        .find(|p| name.contains(&p.name.to_uppercase()))
}

const RATES_1K: &[PollingRateHz] = &[
    PollingRateHz::Hz125,
    PollingRateHz::Hz500,
    PollingRateHz::Hz1000,
];
const RATES_4K: &[PollingRateHz] = &[
    PollingRateHz::Hz125,
    PollingRateHz::Hz500,
    PollingRateHz::Hz1000,
    PollingRateHz::Hz2000,
    PollingRateHz::Hz4000,
];
const RATES_8K: &[PollingRateHz] = &[
    PollingRateHz::Hz125,
    PollingRateHz::Hz500,
    PollingRateHz::Hz1000,
    PollingRateHz::Hz2000,
    PollingRateHz::Hz4000,
    PollingRateHz::Hz8000,
];

/// Rate list for the link; the stored rate nibble is a plain index into it (no skipped slot).
pub fn polling_rates(max_polling_hz: u16, bluetooth: bool) -> &'static [PollingRateHz] {
    if bluetooth {
        return RATES_1K;
    }
    match max_polling_hz {
        4000 => RATES_4K,
        8000 => RATES_8K,
        _ => RATES_1K,
    }
}

fn verify_v2_frame(kind: ReportKind, report: &[u8]) -> Result<(), String> {
    if kind != ReportKind::Feature {
        return Err(format!("MCHOSE V2 only sends feature reports, got {kind}"));
    }
    let channel = Channel::from_report_id(report[0])
        .ok_or_else(|| format!("disallowed MCHOSE V2 report id 0x{:02X}", report[0]))?;
    if report[1 + channel.token_count()..].iter().any(|b| *b != 0) {
        return Err("MCHOSE V2 padding past the token count must be zero".to_owned());
    }
    let cmd = report[1] ^ 0xFF;
    match channel_for(cmd) {
        Some(expected) if expected == channel => Ok(()),
        Some(_) => Err(format!(
            "MCHOSE V2 command 0x{cmd:02X} sent on the wrong channel 0x{:02X}",
            report[0]
        )),
        None => Err(format!(
            "undocumented MCHOSE V2 command 0x{cmd:02X} blocked"
        )),
    }
}

/// Strict 65-byte `0x11`/`0x12` feature validator with a documented-command allow-list.
pub fn v2_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![FEATURE_LEN], vec![SHORT_REPORT_ID, LONG_REPORT_ID])
        .with_checksum(verify_v2_frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_transport::PacketValidator;

    #[test]
    fn worked_example_battery_frame_matches_spec() {
        let frame = encode_command(&[command::BATTERY]).unwrap();
        assert_eq!(frame[0], 0x11);
        assert_eq!(frame[1], 0xF9);
        assert!(frame[2..=20].iter().all(|b| *b == 0xFF));
        assert!(frame[21..].iter().all(|b| *b == 0x00));
        assert_eq!(frame.len(), 65);
        assert!(
            v2_validator()
                .validate_outgoing(ReportKind::Feature, &frame)
                .is_ok()
        );
    }

    #[test]
    fn reply_decoding_un_inverts_command_and_payload() {
        let mut raw = vec![0x11, 0x06 ^ 0xFF];
        let mut payload = [0u8; 63];
        payload[9] = 77;
        payload[10] = 1;
        payload[8] = 0x10;
        raw.extend(payload.iter().map(|b| b ^ 0xFF));
        let reply = decode_reply(&raw).unwrap();
        assert_eq!(reply.command, 0x06);
        let batt = decode_battery(&reply.payload).unwrap();
        assert_eq!(batt.percent, 77);
        assert!(batt.charging);
        assert!(batt.connected);
    }

    #[test]
    fn config_write_echoes_untouched_bytes() {
        let mut payload: Vec<u8> = (0..63u8).map(|b| b.wrapping_mul(7)).collect();
        payload[4] = 0x40; // 1600 DPI LE
        payload[5] = 0x06;
        let config = Config::decode(&payload).unwrap();
        let tokens = encode_config_write(
            &config,
            &ConfigChanges {
                wired: Some(LinkByte {
                    rate_index: 5,
                    dpi_index: 2,
                }),
                debounce_ms: Some(4),
                ..ConfigChanges::default()
            },
        )
        .unwrap();
        assert_eq!(tokens.len(), 64);
        assert_eq!(tokens[0], 0x57);
        assert_eq!(tokens[2], 0x52);
        assert_eq!(tokens[19], 4);
        for (ix, byte) in payload.iter().enumerate() {
            if ix != 1 && ix != 18 {
                assert_eq!(tokens[ix + 1], *byte, "offset {ix} must be echoed");
            }
        }
        assert!(encode_command(&tokens).is_ok());
    }

    #[test]
    fn sensor_bits_and_performance_tokens() {
        let mut payload = [0u8; 63];
        payload[4] = 0x20;
        payload[5] = 0x03;
        payload[17] = 0b1011_1110;
        let config = Config::decode(&payload).unwrap();
        assert_eq!(config.lod_index(), 2);
        assert!(config.ripple_control());
        assert!(config.angle_snapping());
        assert!(config.motion_sync());

        let tokens = encode_performance(
            1,
            PerformanceChanges {
                motion_sync: Some(false),
                ..PerformanceChanges::default()
            },
        )
        .unwrap();
        assert_eq!(tokens, vec![0x42, 1, 0, 0, 2, 0, 0, 0, 0, 0]);
        assert!(encode_performance(4, PerformanceChanges::default()).is_err());
    }

    #[test]
    fn config_rejects_implausible_stage() {
        let payload = vec![0u8; 63];
        assert!(Config::decode(&payload).is_none());
    }

    #[test]
    fn validator_rejects_undocumented_and_malformed_frames() {
        let validator = v2_validator();
        let mut frame = encode_command(&[command::IDENTITY]).unwrap();
        frame[1] = 0x52 ^ 0xFF; // setButton is not allow-listed
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &frame)
                .is_err()
        );

        let mut padded = encode_command(&[command::IDENTITY]).unwrap();
        padded[40] = 0x01;
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &padded)
                .is_err()
        );

        let mut wrong_channel = encode_command(&[command::CONFIG]).unwrap();
        wrong_channel[0] = SHORT_REPORT_ID;
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &wrong_channel)
                .is_err()
        );

        let ok = encode_command(&[command::IDENTITY]).unwrap();
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &ok)
                .is_err()
        );
        assert!(
            validator
                .validate_outgoing(ReportKind::Feature, &ok[..64])
                .is_err()
        );
        assert!(encode_command(&[0x52]).is_err());
    }

    #[test]
    fn product_resolution_and_rates() {
        assert_eq!(
            find_product(Some(0x4021), None).unwrap().name,
            "A7 V2 Ultra+"
        );
        assert_eq!(
            find_product(None, Some("MCHOSE A7 V2 Ultra+ 8K"))
                .unwrap()
                .pid,
            0x4021
        );
        assert!(find_product(Some(0x9999), Some("Unknown")).is_none());
        assert_eq!(polling_rates(8000, false).len(), 6);
        assert_eq!(polling_rates(8000, true), RATES_1K);
        assert!(is_v2_device(REALTEK_VID, 0x00B0));
        assert!(!is_v2_device(REALTEK_VID, RECEIVER_24G_PID));
        assert!(!is_v2_device(MCHOSE_VID, 0x1012));
    }

    #[test]
    fn profile_name_decoding() {
        let mut payload = vec![1u8];
        payload.extend_from_slice("默认板载2".as_bytes());
        payload.push(0);
        payload.extend_from_slice(b"junk");
        assert_eq!(
            decode_profile_name(&payload).as_deref(),
            Some("Default onboard 2")
        );
        assert_eq!(decode_profile_name(&[0, 0]), None);
        assert_eq!(decode_version(b"\x085.46.2.4").as_deref(), Some("5.46.2.4"));
    }
}
