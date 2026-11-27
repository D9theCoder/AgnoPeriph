//! Logitech HID++ 1.0 / 2.0 wire protocol constants, framing, and decoding helpers.

use agnoperiph_core::{
    BatteryStatus, Brightness, ChargingState, CoreError, CoreResult, PollingRateHz, ProfileMode,
    ProfileSlot, RgbColor, RgbDirection, RgbEffect, RgbEffectKind,
};
use agnoperiph_transport::FixedReportValidator;

/// Logitech USB Vendor ID.
pub const LOGITECH_VID: u16 = 0x046D;

/// HID++ Vendor Usage Page on USB devices.
pub const HIDPP_USAGE_PAGE_USB: u16 = 0xFF00;

/// HID++ Vendor Usage Page on Bluetooth LE devices.
pub const HIDPP_USAGE_PAGE_BLE: u16 = 0xFF43;

/// Short HID++ report ID (7 bytes total).
pub const REPORT_ID_SHORT: u8 = 0x10;
pub const SHORT_REPORT_LEN: usize = 7;

/// Long HID++ report ID (20 bytes total).
pub const REPORT_ID_LONG: u8 = 0x11;
pub const LONG_REPORT_LEN: usize = 20;

/// Non-zero software ID nibble used by AgnoPeriph to match our own replies
/// (distinct from G HUB `0x0D` and Solaar `0x0B`).
pub const SOFTWARE_ID: u8 = 0x0C;

/// Error sub-IDs returned in byte 2.
pub const HIDPP10_ERROR_SUB_ID: u8 = 0x8F;
pub const HIDPP20_ERROR_SUB_ID: u8 = 0xFF;

/// Standard HID++ 2.0 error codes returned in byte 5 of an `0xFF` error frame.
pub const HIDPP20_ERR_INVALID_ARGUMENT: u8 = 0x02;
pub const HIDPP20_ERR_BUSY: u8 = 0x08;

/// Human-readable name for a HID++ 2.0 error code.
pub const fn hidpp20_error_name(code: u8) -> &'static str {
    match code {
        0x01 => "UNKNOWN",
        0x02 => "INVALID_ARGUMENT",
        0x03 => "OUT_OF_RANGE",
        0x04 => "HW_ERROR",
        0x05 => "NOT_ALLOWED",
        0x06 => "INVALID_FEATURE_INDEX",
        0x07 => "INVALID_FUNCTION_ID",
        0x08 => "BUSY",
        0x09 => "UNSUPPORTED",
        0x0A => "UNKNOWN_DEVICE",
        _ => "UNKNOWN_ERROR",
    }
}

/// Decodes one `DeviceFWVersion` (`0x0003`, func `1`) entity response into `(entity_kind, version)`.
///
/// Only `Firmware` (`0x00`) and `Bootloader` (`0x01`) entities carry the 3-character ASCII
/// prefix and `major.minor.build` version tuple.
pub fn decode_firmware_entity(params: &[u8; 16]) -> Option<(u8, String)> {
    let kind = params[0] & 0x0F;
    if kind != 0x00 && kind != 0x01 {
        return None;
    }
    let prefix = String::from_utf8_lossy(&params[1..4])
        .trim_matches('\0')
        .trim()
        .to_owned();
    if prefix.is_empty() {
        return None;
    }
    let build = u16::from_be_bytes([params[6], params[7]]);
    Some((
        kind,
        format!("{prefix} {:02X}.{:02X}.{build:04X}", params[4], params[5]),
    ))
}

/// One entry in a Logitech `OnboardProfiles` (`0x8100`) sector directory (`0x0000` writeable or `0x0100` ROM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardProfileEntry {
    /// Big-endian 16-bit sector address (`(mem_type << 8) | page`, e.g. `0x0001` writeable or `0x0101` ROM).
    pub sector: u16,
    /// Whether this profile slot is enabled in the device's directory.
    pub enabled: bool,
}

/// Parses 4-byte directory entries `[mem_type, page, enabled, reserved]` from an `0x8100`
/// directory sector (`0x0000` writeable or `0x0100` ROM), stopping at `0xFFFF` or `max_entries`.
///
/// Returns an empty `Vec` when the sector begins with all `0x00` or all `0xFF` bytes so the
/// caller can fall back from writeable memory (`0x0000`) to ROM (`0x0100`).
pub fn parse_profile_directory(raw: &[u8], max_entries: usize) -> Vec<OnboardProfileEntry> {
    if raw.len() < 4 || raw[..4] == [0x00; 4] || raw[..4] == [0xFF; 4] {
        return Vec::new();
    }
    let mut entries = Vec::new();
    let (chunks, _) = raw.as_chunks::<4>();
    for &[bank, page, enabled, _reserved] in chunks {
        if entries.len() >= max_entries {
            break;
        }
        let sector = u16::from_be_bytes([bank, page]);
        if sector == 0xFFFF || sector == 0x0000 {
            break;
        }
        // Only Writeable (0x00) and ROM (0x01) memory banks with 1-based page indices are valid.
        if (bank != 0x00 && bank != 0x01) || page == 0 {
            break;
        }
        entries.push(OnboardProfileEntry {
            sector,
            enabled: enabled != 0,
        });
    }
    entries
}

/// Maps an `0x8100` `GetCurrentProfile` sector address (`0x0001`, `0x0101`, ...) onto a zero-based [`ProfileSlot`].
pub fn resolve_active_profile_slot(
    active_sector: u16,
    entries: &[OnboardProfileEntry],
    slot_count: u8,
) -> ProfileSlot {
    let max_ix = slot_count.saturating_sub(1);
    if active_sector != 0
        && let Some(ix) = entries.iter().position(|e| e.sector == active_sector)
    {
        return ProfileSlot::new(u8::try_from(ix).unwrap_or(0).min(max_ix));
    }
    if active_sector == 0 {
        let first_enabled = entries
            .iter()
            .position(|e| e.enabled)
            .and_then(|ix| u8::try_from(ix).ok())
            .unwrap_or(0);
        return ProfileSlot::new(first_enabled.min(max_ix));
    }
    let page = u8::try_from(active_sector & 0x00FF).unwrap_or(1);
    ProfileSlot::new(page.saturating_sub(1).min(max_ix))
}

/// Standard HID++ 2.0 Feature IDs.
pub mod feature {
    pub const ROOT: u16 = 0x0000;
    pub const DEVICE_FW_VERSION: u16 = 0x0003;
    pub const DEVICE_NAME_TYPE: u16 = 0x0005;
    pub const BATTERY_STATUS: u16 = 0x1000;
    pub const BATTERY_VOLTAGE: u16 = 0x1001;
    pub const UNIFIED_BATTERY: u16 = 0x1004;
    pub const ADJUSTABLE_DPI: u16 = 0x2201;
    pub const REPROG_CONTROLS_V4: u16 = 0x1B04;
    pub const FN_INVERSION: u16 = 0x40A0;
    pub const NEW_FN_INVERSION: u16 = 0x40A2;
    pub const K375S_FN_INVERSION: u16 = 0x40A3;
    pub const DISABLE_KEYS_BY_USAGE: u16 = 0x4522;
    pub const REPORT_RATE: u16 = 0x8060;
    pub const COLOR_LED_EFFECTS: u16 = 0x8070;
    pub const RGB_EFFECTS: u16 = 0x8071;
    pub const ONBOARD_PROFILES: u16 = 0x8100;

    /// Optional keyboard features probed (in this exact order) after the mouse features.
    pub const KEYBOARD_PROBE_ORDER: [u16; 7] = [
        RGB_EFFECTS,
        COLOR_LED_EFFECTS,
        K375S_FN_INVERSION,
        NEW_FN_INVERSION,
        FN_INVERSION,
        DISABLE_KEYS_BY_USAGE,
        REPROG_CONTROLS_V4,
    ];
}

/// `ColorLEDEffects` (`0x8070`) / `RGBEffects` (`0x8071`) effect IDs.
pub mod led_effect {
    pub const OFF: u16 = 0x00;
    pub const STATIC: u16 = 0x01;
    pub const CYCLE: u16 = 0x03;
    pub const WAVE: u16 = 0x04;
    pub const BREATHING: u16 = 0x0A;
    pub const CYCLE_SATURATION: u16 = 0x15;
    pub const WAVE_SATURATION: u16 = 0x16;
}

/// HID usages disabled by `DisableKeysByUsage` (`0x4522`) for Game Mode / Win Lock:
/// Left GUI, Right GUI, Application.
pub const GAME_MODE_USAGES: [u8; 3] = [0xE3, 0xE7, 0x65];

/// Maximum number of 8-bit usages that fit in one `DisableKeysByUsage` request.
pub const MAX_DISABLE_KEY_USAGES: usize = 16;

/// `ReprogControlsV4` (`0x1B04`) control flag: the control can be remapped.
pub const KEY_FLAG_REPROGRAMMABLE: u16 = 0x0010;

/// Validates a `DisableKeysByUsage` (`0x4522`) usage list before it reaches the wire.
///
/// `0x00` is rejected because zero padding terminates the list on the device.
pub fn validate_key_usages(usages: &[u8], capacity: usize) -> CoreResult<()> {
    let limit = capacity.min(MAX_DISABLE_KEY_USAGES);
    if usages.is_empty() || usages.len() > limit {
        return Err(CoreError::OutOfRange {
            field: "key_usages",
            actual: usages.len().to_string(),
            expected: format!("1..={limit} usages"),
        });
    }
    if usages.contains(&0x00) {
        return Err(CoreError::Validation(
            "HID usage 0x00 is not a valid key usage".to_owned(),
        ));
    }
    Ok(())
}

/// Maps a Logitech LED effect ID onto the brand-agnostic [`RgbEffectKind`].
pub const fn led_effect_kind(effect_id: u16) -> Option<RgbEffectKind> {
    match effect_id {
        led_effect::OFF => Some(RgbEffectKind::Off),
        led_effect::STATIC => Some(RgbEffectKind::Static),
        led_effect::CYCLE | led_effect::CYCLE_SATURATION => Some(RgbEffectKind::SpectrumCycle),
        led_effect::WAVE | led_effect::WAVE_SATURATION => Some(RgbEffectKind::Wave),
        led_effect::BREATHING => Some(RgbEffectKind::Breathing),
        _ => None,
    }
}

/// Effect IDs to try for a requested kind, most preferred first.
///
/// `0x16` is preferred over `0x04` because the `0x04` Wave parameter layout is only
/// assumed upstream (Solaar borrows it from `0x16`).
pub const fn led_effect_candidates(kind: RgbEffectKind) -> &'static [u16] {
    match kind {
        RgbEffectKind::Off => &[led_effect::OFF],
        RgbEffectKind::Static => &[led_effect::STATIC],
        RgbEffectKind::Breathing => &[led_effect::BREATHING],
        RgbEffectKind::SpectrumCycle => &[led_effect::CYCLE, led_effect::CYCLE_SATURATION],
        RgbEffectKind::Wave => &[led_effect::WAVE_SATURATION, led_effect::WAVE],
        RgbEffectKind::Reactive | RgbEffectKind::Preset => &[],
    }
}

const PERIOD_FASTEST_MS: u16 = 1_000;
const PERIOD_STEP_MS: u16 = 40;
const PERIOD_SLOWEST_MS: u16 = PERIOD_FASTEST_MS + PERIOD_STEP_MS * 255;

/// Host-side mapping of the brand-agnostic speed (`255` = fastest) to an effect period in ms.
pub fn speed_to_period_ms(speed: u8) -> u16 {
    PERIOD_SLOWEST_MS - PERIOD_STEP_MS * u16::from(speed)
}

/// Inverse of [`speed_to_period_ms`]; periods outside the mapped range are clamped.
pub fn period_ms_to_speed(period_ms: u16) -> u8 {
    let clamped = period_ms.clamp(PERIOD_FASTEST_MS, PERIOD_SLOWEST_MS);
    u8::try_from((PERIOD_SLOWEST_MS - clamped) / PERIOD_STEP_MS).unwrap_or(u8::MAX)
}

const fn direction_to_wire(direction: RgbDirection) -> u8 {
    match direction {
        RgbDirection::LeftToRight => 1,
        RgbDirection::TopToBottom => 2,
        RgbDirection::RightToLeft => 6,
        RgbDirection::BottomToTop => 7,
    }
}

const fn direction_from_wire(raw: u8) -> RgbDirection {
    match raw {
        2 => RgbDirection::TopToBottom,
        6 => RgbDirection::RightToLeft,
        7 => RgbDirection::BottomToTop,
        _ => RgbDirection::LeftToRight,
    }
}

fn scale_color(color: RgbColor, brightness: Brightness) -> RgbColor {
    let scale = |c: u8| {
        u8::try_from(u16::from(c) * u16::from(brightness.percent()) / 100).unwrap_or(u8::MAX)
    };
    RgbColor::new(scale(color.r), scale(color.g), scale(color.b))
}

fn put_period(params: &mut [u8; 10], offset: usize, speed: u8) {
    params[offset..offset + 2].copy_from_slice(&speed_to_period_ms(speed).to_be_bytes());
}

/// Encodes the 10-byte effect parameter block that follows the effect ordinal byte.
///
/// Offsets follow Solaar `hidpp20.py` `LEDEffects`. Static has no intensity field, so the
/// brightness is applied to the color on the host.
pub fn encode_led_effect_params(
    effect_id: u16,
    effect: &RgbEffect,
    brightness: Brightness,
) -> CoreResult<[u8; 10]> {
    let mut params = [0u8; 10];
    let intensity = brightness.percent();
    match (effect_id, effect) {
        (led_effect::OFF, RgbEffect::Off) => {}
        (led_effect::STATIC, RgbEffect::Static { color }) => {
            let scaled = scale_color(*color, brightness);
            params[..3].copy_from_slice(&[scaled.r, scaled.g, scaled.b]);
            params[3] = 0x02;
        }
        (led_effect::CYCLE, RgbEffect::SpectrumCycle { speed }) => {
            put_period(&mut params, 5, *speed);
            params[7] = intensity;
        }
        (led_effect::CYCLE_SATURATION, RgbEffect::SpectrumCycle { speed }) => {
            params[1] = 0xFF;
            put_period(&mut params, 6, *speed);
            params[8] = intensity;
        }
        (led_effect::WAVE, RgbEffect::Wave { speed, direction }) => {
            put_period(&mut params, 6, *speed);
            params[9] = direction_to_wire(*direction);
        }
        (led_effect::WAVE_SATURATION, RgbEffect::Wave { speed, direction }) => {
            params[1] = 0xFF;
            put_period(&mut params, 6, *speed);
            params[8] = intensity;
            params[9] = direction_to_wire(*direction);
        }
        (led_effect::BREATHING, RgbEffect::Breathing { color, speed }) => {
            params[..3].copy_from_slice(&[color.r, color.g, color.b]);
            put_period(&mut params, 3, *speed);
            params[6] = intensity;
        }
        _ => {
            return Err(CoreError::Validation(format!(
                "effect {:?} cannot be encoded as Logitech LED effect 0x{effect_id:02X}",
                effect.kind()
            )));
        }
    }
    Ok(params)
}

fn read_speed(params: &[u8], offset: usize) -> u8 {
    period_ms_to_speed(u16::from_be_bytes([params[offset], params[offset + 1]]))
}

fn read_brightness(raw: u8) -> Brightness {
    Brightness::new(raw.min(100)).unwrap_or_else(|_| full_brightness())
}

/// Returns a 100% [`Brightness`].
pub fn full_brightness() -> Brightness {
    Brightness::new(100).expect("100 is a valid brightness")
}

/// Decodes a 10-byte effect parameter block (inverse of [`encode_led_effect_params`]).
pub fn decode_led_effect_params(effect_id: u16, params: &[u8]) -> Option<(RgbEffect, Brightness)> {
    if params.len() < 10 {
        return None;
    }
    let color = RgbColor::new(params[0], params[1], params[2]);
    let decoded = match effect_id {
        led_effect::OFF => (RgbEffect::Off, full_brightness()),
        led_effect::STATIC => (RgbEffect::Static { color }, full_brightness()),
        led_effect::CYCLE => (
            RgbEffect::SpectrumCycle {
                speed: read_speed(params, 5),
            },
            read_brightness(params[7]),
        ),
        led_effect::CYCLE_SATURATION => (
            RgbEffect::SpectrumCycle {
                speed: read_speed(params, 6),
            },
            read_brightness(params[8]),
        ),
        led_effect::WAVE => (
            RgbEffect::Wave {
                speed: read_speed(params, 6),
                direction: direction_from_wire(params[9]),
            },
            full_brightness(),
        ),
        led_effect::WAVE_SATURATION => (
            RgbEffect::Wave {
                speed: read_speed(params, 6),
                direction: direction_from_wire(params[9]),
            },
            read_brightness(params[8]),
        ),
        led_effect::BREATHING => (
            RgbEffect::Breathing {
                color,
                speed: read_speed(params, 3),
            },
            read_brightness(params[6]),
        ),
        _ => return None,
    };
    Some(decoded)
}

/// Display name of an LED zone location (Solaar `LEDZoneLocations`).
pub fn led_zone_name(location: u16, index: u8) -> String {
    match location {
        0x01 => "Primary".to_owned(),
        0x02 => "Logo".to_owned(),
        0x03 => "Left Side".to_owned(),
        0x04 => "Right Side".to_owned(),
        0x05 => "Combined".to_owned(),
        0x06..=0x0B => format!("Primary {}", location - 0x05),
        _ => format!("Zone {}", u16::from(index) + 1),
    }
}

/// Returns `true` when `pid` belongs to a Logitech webcam range (`0x0800..=0x09FF`)
/// that must NEVER be probed with HID++ commands.
pub const fn is_excluded_webcam_pid(pid: u16) -> bool {
    pid >= 0x0800 && pid <= 0x09FF
}

/// Returns `true` when `pid` is a Logitech wireless receiver (Lightspeed, Bolt, Unifying, Nano).
pub const fn is_receiver_pid(pid: u16) -> bool {
    pid >= 0xC500 && pid <= 0xC5FF
}

/// Builds the strict `FixedReportValidator` for Logitech HID++ frames (7 or 20 bytes, IDs `0x10`/`0x11`).
pub fn hidpp_validator() -> FixedReportValidator {
    FixedReportValidator::new(
        vec![SHORT_REPORT_LEN, LONG_REPORT_LEN],
        vec![REPORT_ID_SHORT, REPORT_ID_LONG],
    )
}

/// Encodes a 20-byte HID++ 2.0 long request frame.
pub fn encode_long_request(
    device_index: u8,
    feature_index: u8,
    function: u8,
    params: &[u8],
) -> [u8; LONG_REPORT_LEN] {
    let mut frame = [0u8; LONG_REPORT_LEN];
    frame[0] = REPORT_ID_LONG;
    frame[1] = device_index;
    frame[2] = feature_index;
    frame[3] = ((function & 0x0F) << 4) | (SOFTWARE_ID & 0x0F);
    let copy_len = params.len().min(LONG_REPORT_LEN - 4);
    frame[4..4 + copy_len].copy_from_slice(&params[..copy_len]);
    frame
}

/// Decoded HID++ 2.0 response payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidppReply {
    Ok([u8; 16]),
    Hidpp20Error(u8),
    Hidpp10Error(u8),
}

/// Checks whether a raw HID input frame matches a pending `(device_index, feature_index, function)` request.
pub fn match_reply(
    raw: &[u8],
    device_index: u8,
    feature_index: u8,
    function: u8,
) -> Option<HidppReply> {
    if raw.len() < SHORT_REPORT_LEN {
        return None;
    }
    if raw[0] != REPORT_ID_SHORT && raw[0] != REPORT_ID_LONG {
        return None;
    }
    // Direct USB devices sometimes reply with device_index 0x00 vs 0xFF.
    if raw[1] != device_index && raw[1] != (device_index ^ 0xFF) {
        return None;
    }

    let func_sw = ((function & 0x0F) << 4) | (SOFTWARE_ID & 0x0F);
    if raw[2] == HIDPP20_ERROR_SUB_ID && raw[3] == feature_index && raw[4] == func_sw {
        return Some(HidppReply::Hidpp20Error(raw[5]));
    }
    if raw[2] == HIDPP10_ERROR_SUB_ID && raw[3] == feature_index && raw[4] == func_sw {
        return Some(HidppReply::Hidpp10Error(raw[5]));
    }
    if raw[2] == feature_index && raw[3] == func_sw {
        let mut params = [0u8; 16];
        let payload = &raw[4..raw.len().min(LONG_REPORT_LEN)];
        params[..payload.len()].copy_from_slice(payload);
        return Some(HidppReply::Ok(params));
    }
    None
}

/// Decodes `AdjustableDPI` (`0x2201`, func `1`) sensor DPI list into `(min, max, step)`.
///
/// Follows the HID++ 2.0 specification (`v >> 13 == 0b111` indicates a step marker `v & 0x1FFF`
/// between the previous and next entries).
pub fn decode_dpi_list(params: &[u8; 16]) -> CoreResult<(u32, u32, u32)> {
    let mut raw_values = Vec::with_capacity(7);
    let (pairs, _) = params[1..15].as_chunks::<2>();
    for &[hi, lo] in pairs {
        let val = u16::from_be_bytes([hi, lo]);
        if val == 0 {
            break;
        }
        raw_values.push(val);
    }

    if raw_values.is_empty() {
        return Err(CoreError::Protocol(
            "empty AdjustableDPI list returned by device".to_owned(),
        ));
    }

    let mut min_dpi = u32::MAX;
    let mut max_dpi = 0u32;
    let mut step = 50u32;
    let mut ix = 0usize;

    while ix < raw_values.len() {
        let v = raw_values[ix];
        if (v >> 13) == 0b111 {
            let parsed_step = u32::from(v & 0x1FFF);
            if parsed_step > 0 {
                step = parsed_step;
            }
            if let Some(&hi) = raw_values.get(ix + 1) {
                let hi_u32 = u32::from(hi);
                min_dpi = min_dpi.min(hi_u32);
                max_dpi = max_dpi.max(hi_u32);
                ix += 2;
                continue;
            }
            break;
        }
        let val_u32 = u32::from(v);
        min_dpi = min_dpi.min(val_u32);
        max_dpi = max_dpi.max(val_u32);
        ix += 1;
    }

    if min_dpi == u32::MAX || max_dpi == 0 {
        return Err(CoreError::Protocol(
            "unable to determine DPI bounds from AdjustableDPI response".to_owned(),
        ));
    }
    Ok((min_dpi, max_dpi, step))
}

/// Decodes `ReportRate` (`0x8060`, func `0`) bitmask into supported [`PollingRateHz`] values.
pub fn decode_report_rate_mask(mask: u8) -> Vec<PollingRateHz> {
    let mut rates = Vec::new();
    for (interval_ms, rate) in [
        (8u8, PollingRateHz::Hz125),
        (4u8, PollingRateHz::Hz250),
        (2u8, PollingRateHz::Hz500),
        (1u8, PollingRateHz::Hz1000),
    ] {
        if (mask & (1 << (interval_ms - 1))) != 0 {
            rates.push(rate);
        }
    }
    rates
}

/// Converts a `ReportRate` (`0x8060`) interval byte in milliseconds to [`PollingRateHz`].
pub fn polling_rate_from_ms(ms: u8) -> CoreResult<PollingRateHz> {
    match ms {
        1 => Ok(PollingRateHz::Hz1000),
        2 => Ok(PollingRateHz::Hz500),
        4 => Ok(PollingRateHz::Hz250),
        8 => Ok(PollingRateHz::Hz125),
        other => Err(CoreError::Protocol(format!(
            "unsupported Logitech ReportRate interval {other} ms"
        ))),
    }
}

/// Converts a [`PollingRateHz`] to the `ReportRate` (`0x8060`) millisecond interval byte.
pub fn polling_rate_to_ms(rate: PollingRateHz) -> CoreResult<u8> {
    match rate {
        PollingRateHz::Hz1000 => Ok(1),
        PollingRateHz::Hz500 => Ok(2),
        PollingRateHz::Hz250 => Ok(4),
        PollingRateHz::Hz125 => Ok(8),
        other => Err(CoreError::OutOfRange {
            field: "polling_rate",
            actual: other.to_string(),
            expected: "125, 250, 500, or 1000 Hz (feature 0x8060)".to_owned(),
        }),
    }
}

/// Decodes `UnifiedBattery` (`0x1004`, func `1`) status bytes.
pub fn decode_unified_battery(params: &[u8; 16]) -> BatteryStatus {
    let pct = if params[0] > 0 && params[0] <= 100 {
        Some(params[0])
    } else {
        match params[1] {
            8 => Some(90),
            4 => Some(50),
            2 => Some(20),
            1 => Some(5),
            _ => None,
        }
    };
    let state = match params[2] {
        0 => ChargingState::Discharging,
        1 | 2 | 4 => ChargingState::Charging,
        3 => ChargingState::Full,
        5 | 6 => ChargingState::Error,
        _ => ChargingState::Unknown,
    };
    BatteryStatus::new(pct, None, state, true)
}

/// Decodes `BatteryStatus` (`0x1000`, func `0`) status bytes.
pub fn decode_battery_status(params: &[u8; 16]) -> BatteryStatus {
    let pct = if params[0] > 0 && params[0] <= 100 {
        Some(params[0])
    } else if params[2] == 3 {
        Some(100)
    } else {
        None
    };
    let state = match params[2] {
        0 => ChargingState::Discharging,
        1 | 2 | 4 => ChargingState::Charging,
        3 => ChargingState::Full,
        5 | 6 => ChargingState::Error,
        _ => ChargingState::Unknown,
    };
    BatteryStatus::new(pct, None, state, true)
}

/// Solaar's 13-point Li-ion discharge curve for `BatteryVoltage` (`0x1001`).
const VOLTAGE_CURVE: [(u16, u8); 13] = [
    (4186, 100),
    (4067, 90),
    (3989, 80),
    (3922, 70),
    (3859, 60),
    (3811, 50),
    (3778, 40),
    (3751, 30),
    (3717, 20),
    (3671, 10),
    (3646, 5),
    (3579, 2),
    (3500, 0),
];

/// Converts millivolts into a percentage using Solaar's piecewise-linear discharge curve.
pub fn voltage_mv_to_percent(mv: u16) -> u8 {
    if mv >= VOLTAGE_CURVE[0].0 {
        return 100;
    }
    for window in VOLTAGE_CURVE.windows(2) {
        let (hi_mv, hi_p) = window[0];
        let (lo_mv, lo_p) = window[1];
        if mv >= lo_mv {
            let num = u32::from(mv - lo_mv) * u32::from(hi_p - lo_p);
            let den = u32::from(hi_mv - lo_mv);
            let interpolated = u32::from(lo_p) + (num / den);
            return u8::try_from(interpolated).unwrap_or(100);
        }
    }
    0
}

/// Decodes `BatteryVoltage` (`0x1001`, func `0`) status bytes.
pub fn decode_battery_voltage(params: &[u8; 16]) -> BatteryStatus {
    let mv = u16::from_be_bytes([params[0], params[1]]);
    let flags = params[2];
    let external_power = (flags & 0x80) != 0;
    let is_full = external_power && (flags & 0x01) != 0;
    let state = if is_full {
        ChargingState::Full
    } else if external_power {
        ChargingState::Charging
    } else {
        ChargingState::Discharging
    };
    let pct = if is_full {
        Some(100)
    } else {
        Some(voltage_mv_to_percent(mv))
    };
    BatteryStatus::new(pct, Some(mv), state, true)
}

/// Decodes `OnboardProfiles` (`0x8100`, func `2`) mode byte.
pub fn decode_profile_mode(byte: u8) -> CoreResult<ProfileMode> {
    match byte {
        1 => Ok(ProfileMode::OnboardMemory),
        2 => Ok(ProfileMode::HostControlled),
        other => Err(CoreError::Protocol(format!(
            "unknown Logitech OnboardProfiles mode 0x{other:02X}"
        ))),
    }
}

/// Encodes [`ProfileMode`] into the `OnboardProfiles` (`0x8100`, func `1`) mode byte.
pub const fn encode_profile_mode(mode: ProfileMode) -> u8 {
    match mode {
        ProfileMode::OnboardMemory => 1,
        ProfileMode::HostControlled => 2,
    }
}
