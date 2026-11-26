//! ATK RS7 ("DuckBread") Hall Effect keyboard wire protocol codecs and safety validator.
//!
//! All opcodes, offsets, and unit conversions are proven in `docs/protocols/atk-rs7.md`
//! from official ATK HUB v3.2.33 WebHID JS (`api-NHsRvjW6.js`, `demoMode.store-x0QG9XUG.js`).

use agnoperiph_core::{
    CoreError, CoreResult, DksBinding, DksStageAction, DksTriggerDirection, KeyCode, KeyIndex,
    MacroActionKind, MacroSlot, MacroStep, PollingRateHz, RapidSwitchPair, RapidTriggerConfig,
    RgbColor, RgbEffect, RgbPresetInfo, SocdMode, SocdPair, TravelHundredthsMm,
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

/// Maximum DKS, RS/SOCD, MT, TGL, and Macro slots on the ATK RS7.
pub const RS7_MAX_DKS_SLOTS: u8 = 20;
pub const RS7_MAX_SOCD_SLOTS: u8 = 20;
pub const RS7_MAX_MT_SLOTS: u8 = 20;
pub const RS7_MAX_TGL_SLOTS: u8 = 20;
pub const RS7_MAX_MACRO_SLOTS: u8 = 16;
pub const RS7_LAYERS: u8 = 4;

/// VIA-compatible top-level command IDs (`p[0]`).
pub mod cmd {
    pub const GET_KEYBOARD_VALUE: u8 = 0x02;
    pub const SET_KEYBOARD_VALUE: u8 = 0x03;
    pub const DYNAMIC_KEYMAP_GET_KEYCODE: u8 = 0x04;
    pub const DYNAMIC_KEYMAP_SET_KEYCODE: u8 = 0x05;
    pub const DYNAMIC_KEYMAP_RESET: u8 = 0x06;
    pub const LIGHTING_SET_VALUE: u8 = 0x07;
    pub const LIGHTING_GET_VALUE: u8 = 0x08;
    pub const EEPROM_RESET: u8 = 0x0A;
    pub const BOOTLOADER_JUMP: u8 = 0x0B;
    pub const MACRO_GET_BUFFER_SIZE: u8 = 0x0D;
    pub const MACRO_GET_BUFFER: u8 = 0x0E;
    pub const MACRO_SET_BUFFER: u8 = 0x0F;
    pub const MACRO_RESET: u8 = 0x10;
    pub const DYNAMIC_KEYMAP_GET_BUFFER: u8 = 0x12;
    pub const DYNAMIC_KEYMAP_SET_BUFFER: u8 = 0x13;
    pub const VIAL_PREFIX: u8 = 0xFE;
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
    pub const FEATURE: u8 = 0x11; // 17 (MAC mode, WASD swap, Win lock, 6KRO)
    pub const LAYER: u8 = 0x13; // 19
    pub const RESET_ALL: u8 = 0x15; // 21 (BLOCKED by safety validator)
    pub const CALIBRATION_START: u8 = 0x18; // 24
    pub const CALIBRATION_FINISH: u8 = 0x19; // 25
    pub const CALIBRATION_LIVE_BUFFER: u8 = 0x1C; // 28
    pub const BOTTOM_OPTIMIZE_SWITCH: u8 = 0x1D; // 29 (Anti-Accidental Touch)
    pub const GAME_MODE: u8 = 0x1E; // 30
    pub const CALIBRATION_STATUS: u8 = 0x1F; // 31
    pub const RS_SOCD: u8 = 0x20; // 32
    pub const BOX_LIGHT: u8 = 0x21; // 33
    pub const REPORT_RATE: u8 = 0x22; // 34
    pub const LIGHT_INFO: u8 = 0x41; // 65 (used with LIGHTING_GET/SET_VALUE)
    pub const VIAL_ENTRY_OP: u8 = 0x0D; // 13 (used with VIAL_PREFIX)
    pub const TAP_DANCE_GET: u8 = 0x01;
    pub const TAP_DANCE_SET: u8 = 0x02;
    pub const IAP: u8 = 0x98; // 152 (BLOCKED by safety validator)
}

/// Key class bytes (`x` enum in `api-NHsRvjW6.js`).
pub mod key_class {
    pub const GENERAL: u8 = 0x00;
    pub const LAYER: u8 = 0x52;
    pub const TAP_DANCE: u8 = 0x57; // 0..=19 = MT, 20..=39 = TGL
    pub const MACRO: u8 = 0x77; // 0..=15 = M1..M16
    pub const LIGHT: u8 = 0x78;
    pub const SYSTEM: u8 = 0x7C;
    pub const KB_AND_USER: u8 = 0x7E; // 5..=24 = DKS 0..19
}

/// Vial/QMK macro action opcodes (`Y` enum in `api-NHsRvjW6.js`).
pub mod macro_wire {
    pub const PREFIX: u8 = 0x01;
    pub const SS_TAP_CODE: u8 = 0x01;
    pub const SS_DOWN_CODE: u8 = 0x02;
    pub const SS_UP_CODE: u8 = 0x03;
    pub const SS_DELAY_CODE: u8 = 0x04;
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
        cmd::EEPROM_RESET | cmd::BOOTLOADER_JUMP | cmd::MACRO_RESET
    ) {
        return Err(format!(
            "blocked dangerous VIA bootloader/reset command 0x{top_cmd:02X}"
        ));
    }
    if top_cmd == cmd::SET_KEYBOARD_VALUE && report[2] == sub::CUSTOM_ID {
        let sub_cmd = report[3];
        if matches!(sub_cmd, sub::RESET_ALL | sub::IAP) {
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
        let [pt_hi, pt_lo] = pair.trigger_point.as_hundredths().to_be_bytes();
        p[9] = pt_hi;
        p[10] = pt_lo;
        // Verified in useTglLogic-yjkMKXkT.js: DuckBread SOCD/RS pairs always send withGlobal = 0
        // and carry their own per-pair trigger travel (`point`) and Rapid Trigger flag (`rt`).
        p[11] = 0; // withGlobal
        p[12] = u8::from(pair.rapid_trigger); // rt (0 = off, 1 = on)
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
    let raw_point = u16::from_be_bytes([p[9], p[10]]);
    let trigger_point = if raw_point > 0 {
        TravelHundredthsMm::from_hundredths(raw_point)
    } else {
        agnoperiph_core::DEFAULT_PAIR_TRIGGER_POINT
    };
    let rapid_trigger = p[12] != 0;
    let key_a = row_col_to_key(p[13], p[14]);
    let key_b = row_col_to_key(p[15], p[16]);
    SocdPair::new(key_a, key_b, mode).ok().map(|pair| {
        pair.with_trigger_point(trigger_point)
            .with_rapid_trigger(rapid_trigger)
    })
}

pub fn req_backlight_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::LIGHTING_GET_VALUE;
    p[1] = sub::LIGHT_INFO;
    p
}

/// All 21 official Back Lamp presets on the ATK RS7 (`da` in `api-NHsRvjW6.js`).
pub fn back_lamp_presets() -> Vec<RgbPresetInfo> {
    vec![
        RgbPresetInfo::new(0, "Close", false, false, false),
        RgbPresetInfo::new(1, "Customization", true, false, false),
        RgbPresetInfo::new(2, "Single Color Constant", true, false, false),
        RgbPresetInfo::new(3, "Focus", true, false, false),
        RgbPresetInfo::new(4, "Up-Down Rainbow", true, false, false),
        RgbPresetInfo::new(5, "Left-Right Rainbow", true, false, false),
        RgbPresetInfo::new(6, "Monochrome Breath", true, true, false),
        RgbPresetInfo::new(7, "Fluttering Ribbons", true, true, false),
        RgbPresetInfo::new(8, "Laser", true, true, false),
        RgbPresetInfo::new(9, "Pinwheel", true, true, false),
        RgbPresetInfo::new(11, "Vortex", true, true, false),
        RgbPresetInfo::new(13, "Spectral Cycle", false, true, false),
        RgbPresetInfo::new(19, "Colorful Swirls", false, true, false),
        RgbPresetInfo::new(23, "Double Vortex", false, true, false),
        RgbPresetInfo::new(25, "Christmas", false, true, false),
        RgbPresetInfo::new(26, "Spectrum Breathing", true, true, false),
        RgbPresetInfo::new(28, "Spectrum Float", true, true, false),
        RgbPresetInfo::new(30, "Digital Rain", false, false, false),
        RgbPresetInfo::new(31, "Single Light Up", true, true, false),
        RgbPresetInfo::new(36, "Crucifix", true, true, false),
        RgbPresetInfo::new(39, "Fingertip Rainbow", false, true, false),
    ]
}

/// All 5 official Logo Light presets on the ATK RS7 (`ga` in `api-NHsRvjW6.js`).
pub fn logo_light_presets() -> Vec<RgbPresetInfo> {
    vec![
        RgbPresetInfo::new(0, "Close", false, false, false),
        RgbPresetInfo::new(3, "Always On", true, false, false),
        RgbPresetInfo::new(1, "Streamer", true, true, true),
        RgbPresetInfo::new(2, "Spectral Cycle", false, true, false),
        RgbPresetInfo::new(4, "Monochrome Breath", true, true, true),
    ]
}

pub fn req_backlight_set(effect: RgbEffect, brightness_pct: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let (mode_id, speed, color) = match effect {
        RgbEffect::Off => (0u8, 0u8, RgbColor::BLACK),
        RgbEffect::Static { color } => (2u8, 128u8, color),
        RgbEffect::Breathing { color, speed } => (6u8, speed, color),
        RgbEffect::SpectrumCycle { speed } => (13u8, speed, RgbColor::WHITE),
        RgbEffect::Wave { speed, .. } => (19u8, speed, RgbColor::WHITE),
        RgbEffect::Reactive { color, speed } => (31u8, speed, color),
        RgbEffect::Preset {
            id, color, speed, ..
        } => (id, speed, color),
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
    let effect = if mode_id == 0 {
        RgbEffect::Off
    } else {
        RgbEffect::Preset {
            id: mode_id,
            color,
            speed,
            colorful: false,
        }
    };
    (effect, brightness)
}

pub fn req_logo_light_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::BOX_LIGHT;
    p
}

pub fn req_logo_light_set(effect: RgbEffect, brightness_pct: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let (mode_id, colorful, speed, color) = match effect {
        RgbEffect::Off => (0u8, false, 0u8, RgbColor::BLACK),
        RgbEffect::Static { color } => (3u8, false, 0u8, color),
        RgbEffect::Breathing { color, speed } => (4u8, false, speed.min(40), color),
        RgbEffect::SpectrumCycle { speed } => (2u8, false, speed.min(40), RgbColor::WHITE),
        RgbEffect::Wave { speed, .. } => (1u8, true, speed.min(40), RgbColor::WHITE),
        RgbEffect::Reactive { color, speed } => (1u8, false, speed.min(40), color),
        RgbEffect::Preset {
            id,
            color,
            speed,
            colorful,
        } => (id, colorful, speed.min(40), color),
    };
    let max_raw: u16 = if mode_id == 4 { 100 } else { 160 };
    let raw_brightness =
        u8::try_from((u16::from(brightness_pct.min(100)) * max_raw + 50) / 100).unwrap_or(100);
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::BOX_LIGHT;
    p[3] = mode_id;
    p[4] = u8::from(colorful);
    p[5] = raw_brightness;
    p[6] = speed;
    p[7] = color.r;
    p[8] = color.g;
    p[9] = color.b;
    p
}

pub fn decode_logo_light(p: &[u8; RS7_PAYLOAD_LEN]) -> (RgbEffect, u8) {
    let mode_id = p[3];
    let colorful = p[4] != 0;
    let raw_brightness = p[5];
    let speed = p[6].min(40);
    let color = RgbColor::new(p[7], p[8], p[9]);
    let max_raw: u16 = if mode_id == 4 { 100 } else { 160 };
    let brightness_pct =
        u8::try_from(((u16::from(raw_brightness) * 100 + max_raw / 2) / max_raw).min(100))
            .unwrap_or(100);
    let effect = if mode_id == 0 {
        RgbEffect::Off
    } else {
        RgbEffect::Preset {
            id: mode_id,
            color,
            speed,
            colorful,
        }
    };
    (effect, brightness_pct)
}

pub fn req_deadzone_set(
    top: TravelHundredthsMm,
    bottom: TravelHundredthsMm,
) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::DEAD_BAND;
    let [t_hi, t_lo] = top.as_hundredths().to_be_bytes();
    let [b_hi, b_lo] = bottom.as_hundredths().to_be_bytes();
    p[3] = t_hi;
    p[4] = t_lo;
    p[5] = b_hi;
    p[6] = b_lo;
    p
}

pub fn req_rs_pair_set(
    slot: u8,
    pair: Option<&RapidSwitchPair>,
) -> CoreResult<[u8; RS7_PAYLOAD_LEN]> {
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
        p[7] = socd_wire::RS;
        let [pt_hi, pt_lo] = pair.trigger_point.as_hundredths().to_be_bytes();
        p[9] = pt_hi;
        p[10] = pt_lo;
        p[11] = 0; // withGlobal = 0 (per-pair travel + RT)
        p[12] = u8::from(pair.rapid_trigger);
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

pub fn decode_rs_slot(p: &[u8; RS7_PAYLOAD_LEN]) -> Option<RapidSwitchPair> {
    if p[7] != socd_wire::RS {
        return None;
    }
    let raw_point = u16::from_be_bytes([p[9], p[10]]);
    let trigger_point = if raw_point > 0 {
        TravelHundredthsMm::from_hundredths(raw_point)
    } else {
        agnoperiph_core::DEFAULT_PAIR_TRIGGER_POINT
    };
    let rapid_trigger = p[12] != 0;
    let key_a = row_col_to_key(p[13], p[14]);
    let key_b = row_col_to_key(p[15], p[16]);
    RapidSwitchPair::new(key_a, key_b).ok().map(|pair| {
        pair.with_trigger_point(trigger_point)
            .with_rapid_trigger(rapid_trigger)
    })
}

pub fn req_tap_dance_get(slot: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::VIAL_PREFIX;
    p[1] = sub::VIAL_ENTRY_OP;
    p[2] = sub::TAP_DANCE_GET;
    p[3] = slot;
    p
}

pub fn req_tap_dance_set(
    slot: u8,
    tap: (u8, u8),
    hold: (u8, u8),
    timer_ms: u16,
) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::VIAL_PREFIX;
    p[1] = sub::VIAL_ENTRY_OP;
    p[2] = sub::TAP_DANCE_SET;
    p[3] = slot;
    p[4] = tap.0;
    p[5] = tap.1;
    p[6] = hold.0;
    p[7] = hold.1;
    let [hi, lo] = timer_ms.to_be_bytes();
    p[8] = hi;
    p[9] = lo;
    p
}

pub fn decode_tap_dance(p: &[u8; RS7_PAYLOAD_LEN]) -> ((u8, u8), (u8, u8), u16) {
    let tap = (p[4], p[5]);
    let hold = (p[6], p[7]);
    let timer = u16::from_be_bytes([p[8], p[9]]);
    (tap, hold, timer)
}

pub fn req_calibration_start() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::CALIBRATION_START;
    p
}

pub fn req_calibration_finish() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::CALIBRATION_FINISH;
    p
}

pub fn req_calibration_live_status_get(offset: u16, count: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::CALIBRATION_LIVE_BUFFER;
    let [hi, lo] = offset.to_be_bytes();
    p[5] = hi;
    p[6] = lo;
    p[7] = count;
    p
}

pub fn req_feature_flags_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::FEATURE;
    p
}

#[allow(clippy::fn_params_excessive_bools)]
pub fn req_feature_flags_set(
    rgb_area: u8,
    exchange_wasd: bool,
    mac_mode: bool,
    win_lock: bool,
    six_key_rollover: bool,
) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::FEATURE;
    p[3] = rgb_area;
    p[4] = u8::from(exchange_wasd);
    p[5] = u8::from(mac_mode);
    p[6] = u8::from(win_lock);
    // Verified in duckbread_readable.js:2714-2718: wire byte = 1 - sixKeyNoPunch
    p[7] = u8::from(!six_key_rollover);
    p
}

/// Decodes `(rgb_area, exchange_wasd, mac_mode, win_lock, six_key_rollover)`.
pub fn decode_feature_flags(p: &[u8; RS7_PAYLOAD_LEN]) -> (u8, bool, bool, bool, bool) {
    (p[3], p[4] != 0, p[5] != 0, p[6] != 0, p[7] == 0)
}

pub fn req_anti_accidental_touch_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::BOTTOM_OPTIMIZE_SWITCH;
    p
}

pub fn req_anti_accidental_touch_set(enabled: bool) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::BOTTOM_OPTIMIZE_SWITCH;
    p[3] = u8::from(enabled);
    p
}

pub fn req_game_mode_set(extreme: bool) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::GAME_MODE;
    p[3] = u8::from(extreme);
    p
}

pub fn req_report_rate_set(rate: PollingRateHz) -> CoreResult<[u8; RS7_PAYLOAD_LEN]> {
    let raw = match rate {
        PollingRateHz::Hz1000 => 1u8,
        PollingRateHz::Hz2000 => 2u8,
        PollingRateHz::Hz4000 => 4u8,
        PollingRateHz::Hz8000 => 8u8,
        other => {
            return Err(CoreError::OutOfRange {
                field: "polling_rate",
                actual: other.to_string(),
                expected: "1000, 2000, 4000, or 8000 Hz".to_owned(),
            });
        }
    };
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::SET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::REPORT_RATE;
    p[3] = raw;
    Ok(p)
}

pub fn req_active_layer_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::GET_KEYBOARD_VALUE;
    p[1] = sub::CUSTOM_ID;
    p[2] = sub::LAYER;
    p
}

pub fn decode_active_layer(raw: u8) -> u8 {
    match raw {
        4 | 8 => 2,
        _ => 0,
    }
}

pub fn req_keymap_get_buffer(offset: u16, length: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::DYNAMIC_KEYMAP_GET_BUFFER;
    let [hi, lo] = offset.to_be_bytes();
    p[1] = hi;
    p[2] = lo;
    p[3] = length;
    p
}

pub fn req_keymap_set_keycode(layer: u8, row: u8, col: u8, code: KeyCode) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::DYNAMIC_KEYMAP_SET_KEYCODE;
    p[1] = layer;
    p[2] = row;
    p[3] = col;
    p[4] = code.class;
    p[5] = code.id;
    p
}

pub fn req_keymap_reset() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::DYNAMIC_KEYMAP_RESET;
    p
}

pub fn req_macro_buffer_size_get() -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::MACRO_GET_BUFFER_SIZE;
    p
}

pub fn req_macro_buffer_get(offset: u16, size: u8) -> [u8; RS7_PAYLOAD_LEN] {
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::MACRO_GET_BUFFER;
    let [hi, lo] = offset.to_be_bytes();
    p[1] = hi;
    p[2] = lo;
    p[3] = size;
    p
}

pub fn req_macro_buffer_set(offset: u16, data: &[u8]) -> [u8; RS7_PAYLOAD_LEN] {
    let len = data.len().min(28);
    let mut p = [0u8; RS7_PAYLOAD_LEN];
    p[0] = cmd::MACRO_SET_BUFFER;
    let [hi, lo] = offset.to_be_bytes();
    p[1] = hi;
    p[2] = lo;
    p[3] = u8::try_from(len).unwrap_or(28);
    p[4..4 + len].copy_from_slice(&data[..len]);
    p
}

/// Encodes up to `RS7_MAX_MACRO_SLOTS` macros into the null-terminated QMK/Vial macro stream.
pub fn encode_macros(macros: &[MacroSlot]) -> Vec<u8> {
    let mut out = Vec::new();
    for slot_ix in 0..usize::from(RS7_MAX_MACRO_SLOTS) {
        if let Some(slot) = macros.iter().find(|m| usize::from(m.index) == slot_ix) {
            for step in &slot.steps {
                let op = match step.kind {
                    MacroActionKind::KeyTap => macro_wire::SS_TAP_CODE,
                    MacroActionKind::KeyDown => macro_wire::SS_DOWN_CODE,
                    MacroActionKind::KeyUp => macro_wire::SS_UP_CODE,
                    _ => continue,
                };
                out.push(macro_wire::PREFIX);
                out.push(op);
                out.push(step.hid_keycode);
                if step.delay_ms > 0 {
                    let [lo, hi] = step.delay_ms.to_le_bytes();
                    out.push(macro_wire::PREFIX);
                    out.push(macro_wire::SS_DELAY_CODE);
                    out.push(lo.saturating_add(1));
                    out.push(hi.saturating_add(1));
                }
            }
        }
        out.push(0x00);
    }
    out
}

/// Decodes the null-terminated QMK/Vial macro buffer into `RS7_MAX_MACRO_SLOTS` slots.
pub fn decode_macros(raw: &[u8]) -> Vec<MacroSlot> {
    let mut slots = Vec::with_capacity(usize::from(RS7_MAX_MACRO_SLOTS));
    let mut chunks = raw.split(|&b| b == 0x00);
    for slot_ix in 0..RS7_MAX_MACRO_SLOTS {
        let mut steps: Vec<MacroStep> = Vec::new();
        if let Some(bytes) = chunks.next() {
            let mut pos = 0usize;
            while pos + 2 < bytes.len() {
                if bytes[pos] != macro_wire::PREFIX {
                    pos += 1;
                    continue;
                }
                let op = bytes[pos + 1];
                match op {
                    macro_wire::SS_TAP_CODE | macro_wire::SS_DOWN_CODE | macro_wire::SS_UP_CODE => {
                        let kind = match op {
                            macro_wire::SS_TAP_CODE => MacroActionKind::KeyTap,
                            macro_wire::SS_DOWN_CODE => MacroActionKind::KeyDown,
                            _ => MacroActionKind::KeyUp,
                        };
                        let keycode = bytes[pos + 2];
                        pos += 3;
                        let mut delay_ms = 0u16;
                        if pos + 3 < bytes.len()
                            && bytes[pos] == macro_wire::PREFIX
                            && bytes[pos + 1] == macro_wire::SS_DELAY_CODE
                        {
                            let lo = bytes[pos + 2].saturating_sub(1);
                            let hi = bytes[pos + 3].saturating_sub(1);
                            delay_ms = u16::from_le_bytes([lo, hi]);
                            pos += 4;
                        }
                        steps.push(MacroStep::new(kind, keycode, delay_ms));
                    }
                    macro_wire::SS_DELAY_CODE | 5..=7 if pos + 3 < bytes.len() => {
                        pos += 4;
                    }
                    _ => {
                        pos += 1;
                    }
                }
            }
        }
        slots.push(MacroSlot::new(slot_ix, format!("M{}", slot_ix + 1), steps));
    }
    slots
}
