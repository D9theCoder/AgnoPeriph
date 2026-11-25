//! Logitech HID++ 1.0 / 2.0 wire protocol constants, framing, and decoding helpers.

use agnoperiph_core::{
    BatteryStatus, ChargingState, CoreError, CoreResult, PollingRateHz, ProfileMode,
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

/// Standard HID++ 2.0 Feature IDs.
pub mod feature {
    pub const ROOT: u16 = 0x0000;
    pub const DEVICE_FW_VERSION: u16 = 0x0003;
    pub const DEVICE_NAME_TYPE: u16 = 0x0005;
    pub const BATTERY_STATUS: u16 = 0x1000;
    pub const BATTERY_VOLTAGE: u16 = 0x1001;
    pub const UNIFIED_BATTERY: u16 = 0x1004;
    pub const ADJUSTABLE_DPI: u16 = 0x2201;
    pub const REPORT_RATE: u16 = 0x8060;
    pub const ONBOARD_PROFILES: u16 = 0x8100;
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
