//! ATK RS7 ("DuckBread") Hall Effect keyboard wire protocol codecs and safety validator.
//!
//! All opcodes, offsets, and unit conversions are proven in `docs/protocols/atk-rs7.md`
//! from official ATK HUB v3.2.33 WebHID JS (`api-NHsRvjW6.js`, `demoMode.store-x0QG9XUG.js`).

use agnoperiph_core::{
    CoreError, CoreResult, DksBinding, DksStageAction, DksTriggerDirection, KeyIndex,
    PollingRateHz, RapidTriggerConfig, RgbColor, RgbDirection, RgbEffect, SocdMode, SocdPair,
    TravelHundredthsMm,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

/// Shenzhen Yizhita / ATK USB Vendor ID.
pub const ATK_VID: u16 = 0x373B;

/// ATK RS7 RGB Wired USB Product ID.
pub const ATK_RS7_PID: u16 = 0x1044;

/// Vendor Raw HID collection coordinates for ATK RS7 configuration (`UP=0xFF60`, `U=0x0061`).
pub const ATK_RS7_USAGE_PAGE: u16 = 0xFF60;
pub const ATK_RS7_USAGE_ID: u16 = 0x0061;

/// Unnumbered Raw HID Report ID (`0x00`), 32-byte payload, 33-byte Windows `hidapi` TX frame.
pub const RS7_REPORT_ID: u8 = 0x00;
pub const RS7_PAYLOAD_LEN: usize = 32;
pub const RS7_TX_REPORT_LEN: usize = 33;

/// ATK RS7 physical key matrix dimensions (`6 rows × 16 cols = 96` logical matrix slots).
pub const RS7_ROWS: u8 = 6;
pub const RS7_COLS: u8 = 16;
pub const RS7_KEY_COUNT: u16 = (RS7_ROWS as u16) * (RS7_COLS as u16);

/// Travel bounds from `TRAVEL_INFO_MAP["ATK RS7"]` in hundredths of a millimeter.
pub const RS7_TRAVEL_MIN: TravelHundredthsMm = TravelHundredthsMm::from_hundredths(10); // 0.10 mm
pub const RS7_TRAVEL_MAX: TravelHundredthsMm = TravelHundredthsMm::from_hundredths(340); // 3.40 mm
pub const RS7_STEP_NORMAL: TravelHundredthsMm = TravelHundredthsMm::from_hundredths(2); // 0.02 mm
pub const RS7_STEP_EXTREME: TravelHundredthsMm = TravelHundredthsMm::from_hundredths(1); // 0.01 mm
pub const RS7_RT_MIN: TravelHundredthsMm = TravelHundredthsMm::from_hundredths(2); // 0.02 mm
pub const RS7_RT_MAX: TravelHundredthsMm = TravelHundredthsMm::from_hundredths(340); // 3.40 mm

/// Maximum DKS and RS/SOCD hardware slots (`0..20`).
pub const RS7_MAX_DKS_SLOTS: u8 = 20;
pub const RS7_MAX_SOCD_SLOTS: u8 = 20;

/// VIA-compatible top-level command IDs (`p[0]`).
pub mod cmd {
    pub const GET_KEYBOARD_VALUE: u8 = 0x02;
    pub const SET_KEYBOARD_VALUE: u8 = 0x03;
    pub const DYNAMIC_KEYMAP_RESET: u8 = 0x06;
    pub const LIGHTING_SET_VALUE: u8 = 0x07;
    pub const LIGHTING_GET_VALUE: u8 = 0x08;
    pub const EEPROM_RESET: u8 = 0x0A;
    pub const BOOTLOADER_JUMP: u8 = 0x0B;
    pub const MACRO_RESET: u8 = 0x10;
}

/// DuckBread custom sub-command IDs (`p[1] == 0x96`, `p[2] == subcmd`).
pub mod sub {
    pub const CUSTOM_ID: u8 = 0x96; // 150
    pub const ONE_TOG_TH: u8 = 0x03;
    pub const ONE_RT: u8 = 0x07;
    pub const DEAD_BAND: u8 = 0x0B; // 11
    pub const BUFFER_TOG_TH: u8 = 0x0D; // 13
    pub const BUFFER_RT: u8 = 0x0E; // 14
    pub const DKS: u8 = 0x0F; // 15
    pub const LAYER: u8 = 0x13; // 19
    pub const RESET_ALL: u8 = 0x15; // 21 (BLOCKED by safety validator)
    pub const CALIBRATION_START: u8 = 0x18; // 24 (BLOCKED by safety validator)
    pub const CALIBRATION_FINISH: u8 = 0x19; // 25 (BLOCKED by safety validator)
    pub const GAME_MODE: u8 = 0x1E; // 30
    pub const CALIBRATION_STATUS: u8 = 0x1F; // 31
    pub const RS_SOCD: u8 = 0x20; // 32
    pub const BOX_LIGHT: u8 = 0x21; // 33
    pub const REPORT_RATE: u8 = 0x22; // 34
    pub const LIGHT_INFO: u8 = 0x41; // 65 (used with LIGHTING_GET/SET_VALUE)
    pub const IAP: u8 = 0x98; // 152 (BLOCKED by safety validator)
}

/// Flash bulk write operation flags (`_t` enum in `api-NHsRvjW6.js`).
pub mod flash_op {
    pub const NORMAL: u8 = 0;
    pub const ERASE: u8 = 1;
    pub const WRITE: u8 = 2;
}

/// SOCD / Rappy Snappy mode wire values (`G` enum in `api-NHsRvjW6.js`).
pub mod socd_wire {
    pub const NONE: u8 = 0;
    pub const RS: u8 = 1;
    pub const SOCD_LAST: u8 = 2;
    pub const SOCD_KEY1: u8 = 3;
    pub const SOCD_KEY2: u8 = 4;
    pub const SOCD_BALANCE: u8 = 5;
    pub const OKS: u8 = 6;
}

/// Pre-transmission safety validator that enforces exact 33-byte Report-0 framing AND
/// hard-blocks all bootloader, IAP, EEPROM wipe, and factory reset opcodes.
pub fn verify_atk_rs7_safety(_kind: ReportKind, report: &[u8]) -> Result<(), String> {
    if report.len() != RS7_TX_REPORT_LEN {
        return Err(format!(
            "expected {RS7_TX_REPORT_LEN} bytes (Report ID 0x00 + 32-byte payload), got {}",
            report.len()
        ));
    }
    if report[0] != RS7_REPORT_ID {
        return Err(format!("disallowed Report ID 0x{:02X}", report[0]));
    }
    let top_cmd = report[1];
    if matches!(
        top_cmd,
        cmd::DYNAMIC_KEYMAP_RESET | cmd::EEPROM_RESET | cmd::BOOTLOADER_JUMP | cmd::MACRO_RESET
    ) {
        return Err(format!(
            "blocked dangerous VIA bootloader/reset command 0x{top_cmd:02X}"
        ));
    }
    if top_cmd == cmd::SET_KEYBOARD_VALUE && report[2] == sub::CUSTOM_ID {
        let sub_cmd = report[3];
        if matches!(
            sub_cmd,
            sub::RESET_ALL | sub::IAP | sub::CALIBRATION_START | sub::CALIBRATION_FINISH
        ) {
            return Err(format!(
                "blocked dangerous DuckBread command 0x96/0x{sub_cmd:02X}"
            ));
        }
    }
    Ok(())
}

/// Builds the strict [`FixedReportValidator`] for ATK RS7 traffic.
pub fn atk_rs7_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![RS7_TX_REPORT_LEN], vec![RS7_REPORT_ID])
        .with_checksum(verify_atk_rs7_safety)
}

/// Converts a linear [`KeyIndex`] (`0..96`) into `(row, col)` coordinates.
pub fn key_to_row_col(key: KeyIndex) -> CoreResult<(u8, u8)> {
    if key.value() >= RS7_KEY_COUNT {
        return Err(CoreError::OutOfRange {
            field: "key_index",
            actual: key.value().to_string(),
            expected: format!("0..{RS7_KEY_COUNT}"),
        });
    }
    let row = u8::try_from(key.value() / u16::from(RS7_COLS)).unwrap_or(0);
    let col = u8::try_from(key.value() % u16::from(RS7_COLS)).unwrap_or(0);
    Ok((row, col))
}

/// Converts `(row, col)` matrix coordinates into a linear [`KeyIndex`].
pub const fn row_col_to_key(row: u8, col: u8) -> KeyIndex {
    KeyIndex::new((row as u16) * (RS7_COLS as u16) + (col as u16))
}

/// Wraps a 32-byte payload with leading Report ID `0x00` for Windows `hidapi::write`.
pub fn wrap_tx(payload: [u8; RS7_PAYLOAD_LEN]) -> [u8; RS7_TX_REPORT_LEN] {
    let mut tx = [0u8; RS7_TX_REPORT_LEN];
    tx[0] = RS7_REPORT_ID;
    tx[1..].copy_from_slice(&payload);
    tx
}

/// Normalizes an incoming `hidapi` read buffer (32 or 33 bytes) into a 32-byte payload
/// matching `expected_cmd`.
pub fn unwrap_rx(raw: &[u8], expected_cmd: u8) -> Option<[u8; RS7_PAYLOAD_LEN]> {
    let slice = if raw.len() >= RS7_PAYLOAD_LEN && raw[0] == expected_cmd {
        &raw[..RS7_PAYLOAD_LEN]
    } else if raw.len() >= RS7_TX_REPORT_LEN && raw[0] == RS7_REPORT_ID && raw[1] == expected_cmd {
        &raw[1..RS7_TX_REPORT_LEN]
    } else {
        return None;
    };
    let mut out = [0u8; RS7_PAYLOAD_LEN];
    out.copy_from_slice(slice);
    Some(out)
}

// --- Command Builders ---

pub fn req_firmware_version() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p
}

pub fn decode_firmware_version(p: &[u8; RS7_PAYLOAD_LEN]) -> String {
    format!("{:x}.{:02x}", p[5], p[6])
}

pub fn req_game_mode() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::GAME_MODE;
    p
}

pub fn req_report_rate() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::REPORT_RATE;
    p
}

pub fn decode_report_rate(raw: u8) -> Option<PollingRateHz> {
    match raw {
        1 => Some(PollingRateHz::Hz1000),
        2 => Some(PollingRateHz::Hz2000),
        4 => Some(PollingRateHz::Hz4000),
        8 => Some(PollingRateHz::Hz8000),
        _ => None,
    }
}

pub fn req_deadzone_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::DEAD_BAND;
    p
}

pub fn decode_deadzone(p: &[u8; RS7_PAYLOAD_LEN]) -> (TravelHundredthsMm, TravelHundredthsMm) {
    let top = TravelHundredthsMm::from_hundredths(u16::from_be_bytes([p[3], p[4]]));
    let bottom = TravelHundredthsMm::from_hundredths(u16::from_be_bytes([p[5], p[6]]));
    (top, bottom)
}

pub fn req_travel_buffer_get(offset: u16, count: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::BUFFER_TOG_TH;
    let [hi, lo] = offset.to_be_bytes();
    p[5] = hi;
    p[6] = lo;
    p[7] = count;
    p
}

pub fn req_travel_buffer_set(
    offset: u16,
    count: u8,
    flash_operation: u8,
    values: &[TravelHundredthsMm],
) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::BUFFER_TOG_TH;
    let [hi, lo] = offset.to_be_bytes();
    p[5] = hi;
    p[6] = lo;
    p[7] = count;
    p[8] = flash_operation;
    for (ix, val) in values.iter().take(11).enumerate() {
        let [v_hi, v_lo] = val.as_hundredths().to_be_bytes();
        p[9 + ix * 2] = v_hi;
        p[10 + ix * 2] = v_lo;
    }
    p
}

pub fn req_single_actuation_set(
    row: u8,
    col: u8,
    point: TravelHundredthsMm,
) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::ONE_TOG_TH;
    p[3] = row;
    p[4] = col;
    let [hi, lo] = point.as_hundredths().to_be_bytes();
    p[5] = hi;
    p[6] = lo;
    p
}

pub fn req_rt_buffer_get(offset: u16, count: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::BUFFER_RT;
    let [hi, lo] = offset.to_be_bytes();
    p[5] = hi;
    p[6] = lo;
    p[7] = count;
    p
}

pub fn req_single_rt_set(row: u8, col: u8, config: &RapidTriggerConfig) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::ONE_RT;
    p[3] = row;
    p[4] = col;
    p[5] = u8::from(config.is_enabled());
    let [rel_hi, rel_lo] = config.release_sensitivity.as_hundredths().to_be_bytes();
    let [press_hi, press_lo] = config.press_sensitivity.as_hundredths().to_be_bytes();
    p[6] = rel_hi;
    p[7] = rel_lo;
    p[8] = press_hi;
    p[9] = press_lo;
    p
}

pub fn req_calibration_init_status_get(offset: u16, count: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::CALIBRATION_STATUS;
    let [hi, lo] = offset.to_be_bytes();
    p[5] = hi;
    p[6] = lo;
    p[7] = count;
    p
}

pub fn req_dks_get(slot: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::DKS;
    p[5] = 0x01; // Ks.get
    p[6] = slot;
    p
}

pub fn req_dks_set(slot: u8, binding: &DksBinding) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::DKS;
    p[5] = 0x03; // Ks.set
    p[6] = slot;
    for (ix, stage_opt) in binding.stages.iter().enumerate() {
        let base = 7 + ix * 4;
        if let Some(stage) = stage_opt {
            let status_bits: u16 = match (stage.direction, stage.hold) {
                (DksTriggerDirection::Press, false) => 0b00_0000_0001,
                (DksTriggerDirection::Press, true) => 0b00_0000_0111,
                (DksTriggerDirection::Release, false) => 0b00_0100_0000,
                (DksTriggerDirection::Release, true) => 0b01_1100_0000,
            };
            let [s_hi, s_lo] = status_bits.to_be_bytes();
            let [kc_hi, kc_lo] = stage.hid_keycode.to_be_bytes();
            p[base] = s_hi;
            p[base + 1] = s_lo;
            p[base + 2] = kc_hi;
            p[base + 3] = kc_lo;
        }
    }
    p
}

pub fn decode_dks_slot(slot: u8, p: &[u8; RS7_PAYLOAD_LEN]) -> Option<DksBinding> {
    let mut stages: [Option<DksStageAction>; 4] = [None, None, None, None];
    let mut any_active = false;
    for (ix, slot_ref) in stages.iter_mut().enumerate() {
        let base = 7 + ix * 4;
        let status = u16::from_be_bytes([p[base], p[base + 1]]);
        let keycode = u16::from_be_bytes([p[base + 2], p[base + 3]]);
        if status != 0 && keycode != 0 {
            any_active = true;
            let direction = if (status & 0x001F) != 0 {
                DksTriggerDirection::Press
            } else {
                DksTriggerDirection::Release
            };
            let hold = status.count_ones() > 1;
            *slot_ref = Some(DksStageAction::new(
                direction,
                TravelHundredthsMm::from_hundredths(100),
                keycode,
                hold,
            ));
        }
    }
    any_active.then(|| DksBinding::new(KeyIndex::new(u16::from(slot)), stages))
}

pub fn req_socd_get(slot: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::RS_SOCD;
    p[5] = 0x01; // Vs.get
    p[6] = slot;
    p[8] = if slot >= 26 { slot - 26 } else { slot };
    p
}

pub fn req_socd_set(slot: u8, pair: Option<&SocdPair>) -> CoreResult<[u8; RS7_PAYLOAD_LEN]> {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::RS_SOCD;
    p[5] = 0x03; // Vs.set
    p[6] = slot;
    p[8] = if slot >= 26 { slot - 26 } else { slot };

    if let Some(pair) = pair {
        let (r1, c1) = key_to_row_col(pair.key_a)?;
        let (r2, c2) = key_to_row_col(pair.key_b)?;
        p[7] = match pair.mode {
            SocdMode::LastInputPriority => socd_wire::SOCD_LAST,
            SocdMode::KeyAPriority => socd_wire::SOCD_KEY1,
            SocdMode::KeyBPriority => socd_wire::SOCD_KEY2,
            SocdMode::Neutral => socd_wire::SOCD_BALANCE,
        };
        // Default global actuation & layer 0
        p[11] = 1; // withGlobal
        p[13] = r1;
        p[14] = c1;
        p[15] = r2;
        p[16] = c2;
        p[17] = 0; // layer 0
    } else {
        p[7] = socd_wire::NONE;
    }

    Ok(p)
}

pub fn decode_socd_slot(p: &[u8; RS7_PAYLOAD_LEN]) -> Option<SocdPair> {
    let mode = match p[7] {
        socd_wire::SOCD_LAST => SocdMode::LastInputPriority,
        socd_wire::SOCD_KEY1 => SocdMode::KeyAPriority,
        socd_wire::SOCD_KEY2 => SocdMode::KeyBPriority,
        socd_wire::SOCD_BALANCE => SocdMode::Neutral,
        _ => return None,
    };
    let key_a = row_col_to_key(p[13], p[14]);
    let key_b = row_col_to_key(p[15], p[16]);
    SocdPair::new(key_a, key_b, mode).ok()
}

pub fn req_backlight_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::LIGHTING_GET_VALUE;
    p[1] = sub::LIGHT_INFO;
    p
}

pub fn req_backlight_set(effect: RgbEffect, brightness_pct: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let (mode_id, speed, color) = match effect {
        RgbEffect::Off => (0u8, 0u8, RgbColor::BLACK),
        RgbEffect::Static { color } => (2u8, 128u8, color),
        RgbEffect::Breathing { color, speed } => (6u8, speed, color),
        RgbEffect::SpectrumCycle { speed } => (13u8, speed, RgbColor::WHITE),
        RgbEffect::Wave { speed, .. } => (19u8, speed, RgbColor::WHITE),
        RgbEffect::Reactive { color, speed } => (31u8, speed, color),
    };
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::LIGHTING_SET_VALUE;
    p[1] = sub::LIGHT_INFO;
    p[2] = mode_id;
    p[4] = speed;
    p[5] = color.r;
    p[6] = color.g;
    p[7] = color.b;
    p[8] = brightness_pct.min(100);
    p
}

pub fn decode_backlight(p: &[u8; RS7_PAYLOAD_LEN]) -> (RgbEffect, u8) {
    let mode_id = p[2];
    let speed = p[4];
    let color = RgbColor::new(p[5], p[6], p[7]);
    let brightness = p[8].min(100);
    let effect = match mode_id {
        0 => RgbEffect::Off,
        6 | 26 => RgbEffect::Breathing { color, speed },
        13 => RgbEffect::SpectrumCycle { speed },
        4 | 5 | 7 | 19 => RgbEffect::Wave {
            speed,
            direction: RgbDirection::LeftToRight,
        },
        31 | 36 | 39 => RgbEffect::Reactive { color, speed },
        _ => RgbEffect::Static { color },
    };
    (effect, brightness)
}
