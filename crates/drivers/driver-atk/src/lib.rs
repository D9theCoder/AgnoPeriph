//! Universal ATK / VGN / VXE peripheral driver covering three modular protocol families:
//!
//! 1. **DuckBread (`0xFF60:0x0061`) Hall Effect Keyboards** (ATK RS7 and parameterized siblings)
//! 2. **ATK68 / ATK75 (`0xFF01` GTech) Hall Effect Keyboards** (`ff01`)
//! 3. **ATK / VGN / VXE (`0xFF02:0x0002`, Report `0x08`) Gaming Mice** (`mouse`)

#![forbid(unsafe_code)]

pub mod ff01;
pub mod mouse;
pub mod protocol;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agnoperiph_core::{
    BatteryMonitor, Brightness, CalibrationStatus, Capabilities, ConnectionKind, CoreError,
    CoreResult, DeviceId, DeviceInfo, DeviceMatchRule, DksBinding, DriverDescriptor,
    HallEffectCapabilities, HallEffectKeyboard, HallEffectSnapshot, KeyCode, KeyHallEffectConfig,
    KeyIndex, KeyTravelSample, KeyboardConfigCapabilities, KeyboardConfigController,
    KeyboardConfigSnapshot, KeymapCapabilities, KeymapController, KeymapSnapshot,
    MacroCapabilities, MacroController, MacroSlot, ModTapBinding, Peripheral, PointerDevice,
    PollingRateHz, ProfileManager, RapidSwitchPair, RapidTriggerConfig, RgbCapabilities,
    RgbController, RgbEffect, RgbEffectKind, RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState,
    SocdPair, ToggleKeyBinding, TravelHundredthsMm, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::ff01::{ATK_FF01_USAGE_PAGE, AtkFf01Keyboard, ff01_match_rules};
use crate::mouse::{ATK_MOUSE_USAGE_PAGE, AtkMouseDevice, mouse_match_rules};

use crate::protocol::{
    ATK_RS7_PID, ATK_RS7_USAGE_ID, ATK_RS7_USAGE_PAGE, ATK_VID, RS7_COLS, RS7_KEY_COUNT,
    RS7_LAYERS, RS7_MAX_DKS_SLOTS, RS7_MAX_MACRO_SLOTS, RS7_MAX_MT_SLOTS, RS7_MAX_SOCD_SLOTS,
    RS7_MAX_TGL_SLOTS, RS7_PAYLOAD_LEN, RS7_ROWS, RS7_RT_MAX, RS7_RT_MIN, RS7_STEP_EXTREME,
    RS7_STEP_NORMAL, RS7_TRAVEL_MAX, RS7_TRAVEL_MIN, RS7_TX_REPORT_LEN, atk_rs7_validator,
    back_lamp_presets, decode_active_layer, decode_backlight, decode_deadzone, decode_dks_slot,
    decode_feature_flags, decode_firmware_version, decode_logo_light, decode_macros,
    decode_report_rate, decode_rs_slot, decode_socd_slot, decode_tap_dance, encode_macros,
    flash_op, key_class, key_to_row_col, logo_light_presets, req_active_layer_get,
    req_anti_accidental_touch_get, req_anti_accidental_touch_set, req_backlight_get,
    req_backlight_set, req_calibration_finish, req_calibration_init_status_get,
    req_calibration_live_status_get, req_calibration_start, req_deadzone_get, req_deadzone_set,
    req_dks_get, req_dks_set, req_feature_flags_get, req_feature_flags_set, req_firmware_version,
    req_game_mode, req_game_mode_set, req_keymap_get_buffer, req_keymap_reset,
    req_keymap_set_keycode, req_logo_light_get, req_logo_light_set, req_macro_buffer_get,
    req_macro_buffer_set, req_macro_buffer_size_get, req_report_rate, req_report_rate_set,
    req_rs_pair_set, req_rt_buffer_get, req_single_actuation_set, req_single_rt_set, req_socd_get,
    req_socd_set, req_tap_dance_get, req_tap_dance_set, req_travel_buffer_get,
    req_travel_buffer_set, unwrap_rx, wrap_tx,
};

const READ_TIMEOUT: Duration = Duration::from_millis(800);
/// Maximum unrelated/unsolicited input reports skipped while waiting for one ATK RS7 reply.
pub const RS7_MAX_READ_ATTEMPTS: usize = 64;

struct PairedSlotScan {
    socd_pairs: Vec<SocdPair>,
    socd_slots: Vec<u8>,
    rs_pairs: Vec<RapidSwitchPair>,
    rs_slots: Vec<u8>,
    other_reserved: Vec<u8>,
}

/// Decoded Tap-Dance slot entry (`tap`, `hold`, `hold_delay_ms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapDanceEntry {
    pub tap: (u8, u8),
    pub hold: (u8, u8),
    pub hold_delay_ms: u16,
}

/// Declarative hardware profile for a DuckBread Raw HID (`0xFF60:0x0061`) Hall Effect keyboard.
///
/// Sibling boards sharing the DuckBread protocol can be added here declaratively without
/// duplicating transport or codec logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuckBreadBoardSpec {
    pub vid: u16,
    pub pid: u16,
    pub model_name: &'static str,
    pub rows: u8,
    pub cols: u8,
    pub layers: u8,
    pub travel_min: TravelHundredthsMm,
    pub travel_max: TravelHundredthsMm,
    pub step_normal: TravelHundredthsMm,
    pub step_extreme: TravelHundredthsMm,
    pub rt_min: TravelHundredthsMm,
    pub rt_max: TravelHundredthsMm,
    pub has_logo_light: bool,
}

impl DuckBreadBoardSpec {
    pub const fn key_count(self) -> u16 {
        (self.rows as u16) * (self.cols as u16)
    }

    pub fn key_to_row_col(self, key: KeyIndex) -> CoreResult<(u8, u8)> {
        if key.value() >= self.key_count() {
            return Err(CoreError::OutOfRange {
                field: "key_index",
                actual: key.value().to_string(),
                expected: format!("0..{}", self.key_count()),
            });
        }
        let cols = u16::from(self.cols);
        let row = u8::try_from(key.value() / cols).unwrap_or(0);
        let col = u8::try_from(key.value() % cols).unwrap_or(0);
        Ok((row, col))
    }
}

/// Verified DuckBread (`0xFF60:0x0061`) Hall Effect keyboard board table.
pub const DUCKBREAD_BOARDS: &[DuckBreadBoardSpec] = &[DuckBreadBoardSpec {
    vid: ATK_VID,
    pid: ATK_RS7_PID,
    model_name: "ATK RS7 RGB",
    rows: RS7_ROWS,
    cols: RS7_COLS,
    layers: RS7_LAYERS,
    travel_min: RS7_TRAVEL_MIN,
    travel_max: RS7_TRAVEL_MAX,
    step_normal: RS7_STEP_NORMAL,
    step_extreme: RS7_STEP_EXTREME,
    rt_min: RS7_RT_MIN,
    rt_max: RS7_RT_MAX,
    has_logo_light: true,
}];

/// Looks up a DuckBread board specification by `(vid, pid)`.
pub fn find_duckbread_board(vid: u16, pid: u16) -> Option<&'static DuckBreadBoardSpec> {
    DUCKBREAD_BOARDS
        .iter()
        .find(|b| b.vid == vid && b.pid == pid)
}

/// Returns the declarative `DriverDescriptor` registering all ATK / VGN / VXE families:
/// DuckBread (`0xFF60`), ATK68/ATK75 (`0xFF01`), and ATK/VGN/VXE mice (`0xFF02`).
pub fn driver_descriptor() -> DriverDescriptor {
    let mut rules: Vec<DeviceMatchRule> = DUCKBREAD_BOARDS
        .iter()
        .map(|board| {
            DeviceMatchRule::new(board.vid, ConnectionKind::WiredUsb, board.model_name)
                .with_pid(board.pid)
                .with_usage_page(ATK_RS7_USAGE_PAGE)
                .with_usage_id(ATK_RS7_USAGE_ID)
        })
        .collect();
    rules.extend(ff01_match_rules());
    rules.extend(mouse_match_rules());
    DriverDescriptor::new("atk-rs7", VendorKind::Atk, rules)
}

/// Unified ATK / VGN / VXE peripheral handle dispatching across DuckBread (`0xFF60`),
/// GTech (`0xFF01`), and EEPROM mouse (`0xFF02`) protocol families.
#[derive(Debug)]
pub enum AtkDevice<T: HidTransport> {
    DuckBread(Box<AtkRs7Device<T>>),
    Ff01Keyboard(Box<AtkFf01Keyboard<T>>),
    Mouse(Box<AtkMouseDevice<T>>),
}

impl<T: HidTransport> AtkDevice<T> {
    /// Opens any matched ATK / VGN / VXE interface by dispatching on its HID usage page.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let usage_page = transport.descriptor().usage_page;
        match usage_page {
            ATK_RS7_USAGE_PAGE => Ok(Self::DuckBread(Box::new(AtkRs7Device::open(
                transport, dry_run, sink,
            )?))),
            ATK_FF01_USAGE_PAGE => Ok(Self::Ff01Keyboard(Box::new(AtkFf01Keyboard::open(
                transport, dry_run, sink,
            )?))),
            ATK_MOUSE_USAGE_PAGE => Ok(Self::Mouse(Box::new(AtkMouseDevice::open(
                transport, dry_run, sink,
            )?))),
            other => Err(CoreError::Validation(format!(
                "unsupported ATK/VGN/VXE usage page 0x{other:04X}"
            ))),
        }
    }
}

impl<T: HidTransport> Peripheral for AtkDevice<T> {
    fn info(&self) -> &DeviceInfo {
        match self {
            Self::DuckBread(d) => d.info(),
            Self::Ff01Keyboard(d) => d.info(),
            Self::Mouse(d) => d.info(),
        }
    }

    fn capabilities(&self) -> Capabilities {
        match self {
            Self::DuckBread(d) => d.capabilities(),
            Self::Ff01Keyboard(d) => d.capabilities(),
            Self::Mouse(d) => d.capabilities(),
        }
    }

    fn refresh(&mut self) -> CoreResult<()> {
        match self {
            Self::DuckBread(d) => d.refresh(),
            Self::Ff01Keyboard(d) => d.refresh(),
            Self::Mouse(d) => d.refresh(),
        }
    }

    fn as_pointer(&self) -> Option<&dyn PointerDevice> {
        match self {
            Self::Mouse(d) => d.as_pointer(),
            _ => None,
        }
    }

    fn as_pointer_mut(&mut self) -> Option<&mut dyn PointerDevice> {
        match self {
            Self::Mouse(d) => d.as_pointer_mut(),
            _ => None,
        }
    }

    fn as_hall_effect(&self) -> Option<&dyn HallEffectKeyboard> {
        match self {
            Self::DuckBread(d) => d.as_hall_effect(),
            Self::Ff01Keyboard(d) => d.as_hall_effect(),
            Self::Mouse(_) => None,
        }
    }

    fn as_hall_effect_mut(&mut self) -> Option<&mut dyn HallEffectKeyboard> {
        match self {
            Self::DuckBread(d) => d.as_hall_effect_mut(),
            Self::Ff01Keyboard(d) => d.as_hall_effect_mut(),
            Self::Mouse(_) => None,
        }
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        match self {
            Self::DuckBread(d) => d.as_rgb(),
            Self::Ff01Keyboard(d) => d.as_rgb(),
            Self::Mouse(_) => None,
        }
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        match self {
            Self::DuckBread(d) => d.as_rgb_mut(),
            Self::Ff01Keyboard(d) => d.as_rgb_mut(),
            Self::Mouse(_) => None,
        }
    }

    fn as_battery(&self) -> Option<&dyn BatteryMonitor> {
        match self {
            Self::Mouse(d) => d.as_battery(),
            _ => None,
        }
    }

    fn as_battery_mut(&mut self) -> Option<&mut dyn BatteryMonitor> {
        match self {
            Self::Mouse(d) => d.as_battery_mut(),
            _ => None,
        }
    }

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        match self {
            Self::Ff01Keyboard(d) => d.as_profiles(),
            Self::Mouse(d) => d.as_profiles(),
            Self::DuckBread(_) => None,
        }
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        match self {
            Self::Ff01Keyboard(d) => d.as_profiles_mut(),
            Self::Mouse(d) => d.as_profiles_mut(),
            Self::DuckBread(_) => None,
        }
    }

    fn as_keymap(&self) -> Option<&dyn KeymapController> {
        match self {
            Self::DuckBread(d) => d.as_keymap(),
            _ => None,
        }
    }

    fn as_keymap_mut(&mut self) -> Option<&mut dyn KeymapController> {
        match self {
            Self::DuckBread(d) => d.as_keymap_mut(),
            _ => None,
        }
    }

    fn as_macros(&self) -> Option<&dyn MacroController> {
        match self {
            Self::DuckBread(d) => d.as_macros(),
            _ => None,
        }
    }

    fn as_macros_mut(&mut self) -> Option<&mut dyn MacroController> {
        match self {
            Self::DuckBread(d) => d.as_macros_mut(),
            _ => None,
        }
    }

    fn as_keyboard_config(&self) -> Option<&dyn KeyboardConfigController> {
        match self {
            Self::DuckBread(d) => d.as_keyboard_config(),
            Self::Ff01Keyboard(d) => d.as_keyboard_config(),
            Self::Mouse(_) => None,
        }
    }

    fn as_keyboard_config_mut(&mut self) -> Option<&mut dyn KeyboardConfigController> {
        match self {
            Self::DuckBread(d) => d.as_keyboard_config_mut(),
            Self::Ff01Keyboard(d) => d.as_keyboard_config_mut(),
            Self::Mouse(_) => None,
        }
    }
}

/// Active DuckBread (`0xFF60`) Hall Effect keyboard backed by a safety-guarded [`GuardedTransport`].
pub struct AtkRs7Device<T: HidTransport> {
    transport: GuardedTransport<T>,
    spec: DuckBreadBoardSpec,
    info: DeviceInfo,
    capabilities: Capabilities,
    he_caps: HallEffectCapabilities,
    extreme_mode: bool,
    polling_rate: Option<PollingRateHz>,
    /// Hardware SOCD slot indices (`0..RS7_MAX_SOCD_SLOTS`) observed on the last read/write.
    socd_slots: Option<Vec<u8>>,
    /// Hardware Rapid Switch (RS) slot indices observed on the last read/write.
    rs_slots: Option<Vec<u8>>,
    /// Hardware slots occupied by other non-SOCD/non-RS modes (e.g. OKS) that must never be overwritten.
    reserved_rs_slots: Vec<u8>,
    /// Last read `rgbArea` byte from feature flags so writes preserve it.
    rgb_area: u8,
    /// Rotating 4-key chunk cursor for interleaved high-rate live travel polling.
    live_scan_chunk: u16,
}

impl<T: HidTransport> fmt::Debug for AtkRs7Device<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtkRs7Device")
            .field("info", &self.info)
            .field("extreme_mode", &self.extreme_mode)
            .field("polling_rate", &self.polling_rate)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> AtkRs7Device<T> {
    /// Connects to an ATK RS7 Raw HID (`0xFF60:0x0061`) interface, installs the bootloader/reset
    /// blocking validator, and performs read-only metadata discovery.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        let spec = find_duckbread_board(desc.vid, desc.pid)
            .copied()
            .ok_or_else(|| {
                CoreError::Validation(format!(
                    "expected DuckBread ATK board (e.g. 0x{ATK_VID:04X}:0x{ATK_RS7_PID:04X}), got 0x{:04X}:0x{:04X}",
                    desc.vid, desc.pid
                ))
            })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(atk_rs7_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "atk:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Atk,
            desc.product.unwrap_or_else(|| spec.model_name.to_owned()),
            ConnectionKind::WiredUsb,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let mut dev = Self {
            transport: guarded,
            spec,
            info,
            capabilities: Capabilities::empty()
                .with_hall_effect(true)
                .with_rgb(true)
                .with_keymap(true)
                .with_macros(true)
                .with_keyboard_config(true),
            he_caps: build_he_caps(&spec, false),
            extreme_mode: false,
            polling_rate: None,
            socd_slots: None,
            rs_slots: None,
            reserved_rs_slots: Vec::new(),
            rgb_area: 0,
            live_scan_chunk: 0,
        };

        dev.discover_readonly()?;
        Ok(dev)
    }

    /// Returns the board specification for this DuckBread keyboard.
    pub const fn board_spec(&self) -> DuckBreadBoardSpec {
        self.spec
    }

    /// Returns the underlying [`GuardedTransport`] (for mock & dry-run verification).
    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    /// Returns whether Extreme (`0.01 mm` step) performance mode is enabled on the RS7.
    pub const fn is_extreme_mode(&self) -> bool {
        self.extreme_mode
    }

    /// Returns the keyboard's reported USB polling rate (1K / 2K / 4K / 8K Hz).
    pub const fn polling_rate(&self) -> Option<PollingRateHz> {
        self.polling_rate
    }

    fn exchange(
        &mut self,
        payload: [u8; RS7_PAYLOAD_LEN],
        mutating: bool,
    ) -> CoreResult<[u8; RS7_PAYLOAD_LEN]> {
        self.exchange_with_drain(payload, mutating, true)
    }

    fn exchange_with_drain(
        &mut self,
        payload: [u8; RS7_PAYLOAD_LEN],
        mutating: bool,
        drain: bool,
    ) -> CoreResult<[u8; RS7_PAYLOAD_LEN]> {
        let tx = wrap_tx(payload);
        let expected_cmd = payload[0];

        // Flush any stale or unsolicited input reports accumulated in the OS HID ring
        // buffer while the handle sat idle (e.g. onboard Fn+RGB/WinLock notifications),
        // so the real reply is never buried behind older frames.
        if drain && (!mutating || !self.transport.is_dry_run()) {
            let _ = self.transport.drain_input_reports();
        }

        if mutating {
            self.transport.send_output_report(&tx)?;
            if self.transport.is_dry_run() {
                return Ok(payload);
            }
        } else {
            self.transport.send_query_output_report(&tx)?;
        }

        let deadline = Instant::now() + READ_TIMEOUT;
        let mut rx = [0u8; RS7_TX_REPORT_LEN];
        for _ in 0..RS7_MAX_READ_ATTEMPTS {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CoreError::NotConnected(format!(
                    "timed out waiting for ATK RS7 reply to cmd 0x{expected_cmd:02X}"
                )));
            }
            let n = match self.transport.read_input_report(&mut rx, remaining) {
                Ok(len) => len,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for ATK RS7 reply to cmd 0x{expected_cmd:02X}"
                    )));
                }
                Err(err) => return Err(err.into()),
            };
            if let Some(decoded) = unwrap_rx(&rx[..n], expected_cmd)
                && matches_expected_subcmd(&payload, &decoded)
            {
                return Ok(decoded);
            }
        }

        Err(CoreError::Protocol(format!(
            "no matching ATK RS7 reply for cmd 0x{expected_cmd:02X}"
        )))
    }

    fn discover_readonly(&mut self) -> CoreResult<()> {
        let fw = self.exchange(req_firmware_version(), false)?;
        self.info.firmware_version = Some(format!("v{}", decode_firmware_version(&fw)));

        if let Ok(gm) = self.exchange(req_game_mode(), false) {
            self.extreme_mode = gm[3] == 1;
            self.he_caps = build_he_caps(&self.spec, self.extreme_mode);
        }

        if let Ok(rr) = self.exchange(req_report_rate(), false) {
            self.polling_rate = decode_report_rate(rr[3]);
        }

        Ok(())
    }

    fn scan_paired_slots(&mut self) -> CoreResult<PairedSlotScan> {
        let mut socd_pairs = Vec::new();
        let mut socd_slots = Vec::new();
        let mut rs_pairs = Vec::new();
        let mut rs_slots = Vec::new();
        let mut other_reserved = Vec::new();
        for slot in 0..RS7_MAX_SOCD_SLOTS {
            let resp = self.exchange(req_socd_get(slot), false)?;
            if let Some(pair) = decode_socd_slot(&resp) {
                socd_pairs.push(pair);
                socd_slots.push(slot);
            } else if let Some(pair) = decode_rs_slot(&resp) {
                rs_pairs.push(pair);
                rs_slots.push(slot);
            } else if resp[7] != protocol::socd_wire::NONE {
                other_reserved.push(slot);
            }
        }
        Ok(PairedSlotScan {
            socd_pairs,
            socd_slots,
            rs_pairs,
            rs_slots,
            other_reserved,
        })
    }

    fn read_live_travel_chunk(
        &mut self,
        chunk_ix: u16,
        drain: bool,
        samples: &mut Vec<KeyTravelSample>,
    ) -> CoreResult<()> {
        let offset = chunk_ix * 4;
        let resp =
            self.exchange_with_drain(req_calibration_live_status_get(offset, 4), false, drain)?;
        for k in 0..4usize {
            let base = 8 + k * 5;
            let travel_raw = u16::from_be_bytes([resp[base + 2], resp[base + 3]]);
            if travel_raw > 0 {
                let key_ix = offset + u16::try_from(k).unwrap_or(0);
                samples.push(KeyTravelSample::new(
                    KeyIndex::new(key_ix),
                    TravelHundredthsMm::from_hundredths(travel_raw),
                ));
            }
        }
        Ok(())
    }

    fn read_calibrated_keys_init(&mut self) -> CoreResult<Vec<bool>> {
        let mut flags = Vec::with_capacity(usize::from(RS7_KEY_COUNT));
        for chunk_ix in 0..(RS7_KEY_COUNT / 24) {
            let offset = chunk_ix * 24;
            let resp = self.exchange(req_calibration_init_status_get(offset, 24), false)?;
            for k in 0..24usize {
                flags.push(resp[8 + k] != 0);
            }
        }
        Ok(flags)
    }
}

const fn matches_expected_subcmd(
    request: &[u8; RS7_PAYLOAD_LEN],
    reply: &[u8; RS7_PAYLOAD_LEN],
) -> bool {
    match request[0] {
        protocol::cmd::GET_KEYBOARD_VALUE | protocol::cmd::SET_KEYBOARD_VALUE
            if request[1] == protocol::sub::CUSTOM_ID =>
        {
            reply[1] != protocol::sub::CUSTOM_ID || reply[2] == request[2]
        }
        protocol::cmd::LIGHTING_GET_VALUE | protocol::cmd::LIGHTING_SET_VALUE
            if request[1] == protocol::sub::LIGHT_INFO =>
        {
            reply[1] == 0 || reply[1] == protocol::sub::LIGHT_INFO
        }
        _ => true,
    }
}

const fn build_he_caps(spec: &DuckBreadBoardSpec, extreme_mode: bool) -> HallEffectCapabilities {
    let step = if extreme_mode {
        spec.step_extreme
    } else {
        spec.step_normal
    };
    HallEffectCapabilities::new(spec.key_count(), spec.travel_min, spec.travel_max, step)
        .with_rapid_trigger_range(spec.rt_min, spec.rt_max, true)
        .with_max_dks_bindings(RS7_MAX_DKS_SLOTS)
        .with_max_socd_pairs(RS7_MAX_SOCD_SLOTS)
        .with_calibration_status(true)
        .with_live_travel(true)
}

impl<T: HidTransport> Peripheral for AtkRs7Device<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        self.discover_readonly()
    }

    fn as_hall_effect(&self) -> Option<&dyn HallEffectKeyboard> {
        Some(self)
    }

    fn as_hall_effect_mut(&mut self) -> Option<&mut dyn HallEffectKeyboard> {
        Some(self)
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        Some(self)
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        Some(self)
    }

    fn as_keymap(&self) -> Option<&dyn KeymapController> {
        Some(self)
    }

    fn as_keymap_mut(&mut self) -> Option<&mut dyn KeymapController> {
        Some(self)
    }

    fn as_macros(&self) -> Option<&dyn MacroController> {
        Some(self)
    }

    fn as_macros_mut(&mut self) -> Option<&mut dyn MacroController> {
        Some(self)
    }

    fn as_keyboard_config(&self) -> Option<&dyn KeyboardConfigController> {
        Some(self)
    }

    fn as_keyboard_config_mut(&mut self) -> Option<&mut dyn KeyboardConfigController> {
        Some(self)
    }
}

impl<T: HidTransport> HallEffectKeyboard for AtkRs7Device<T> {
    fn hall_effect_capabilities(&self) -> HallEffectCapabilities {
        self.he_caps.clone()
    }

    fn read_hall_effect_state(&mut self) -> CoreResult<HallEffectSnapshot> {
        // 1. Global deadzone
        let dz_raw = self.exchange(req_deadzone_get(), false)?;
        let (top_dz, bottom_dz) = decode_deadzone(&dz_raw);

        // 2. Bulk actuation buffer (12 keys per chunk × 8 chunks = 96 keys)
        let mut actuations = Vec::with_capacity(usize::from(RS7_KEY_COUNT));
        for chunk_ix in 0..(RS7_KEY_COUNT / 12) {
            let offset = chunk_ix * 12;
            let resp = self.exchange(req_travel_buffer_get(offset, 12), false)?;
            for k in 0..12usize {
                let raw = u16::from_be_bytes([resp[8 + k * 2], resp[9 + k * 2]]);
                actuations.push(TravelHundredthsMm::from_hundredths(raw));
            }
        }

        // 3. Bulk Rapid Trigger buffer (4 keys per chunk × 24 chunks = 96 keys)
        let mut rts = Vec::with_capacity(usize::from(RS7_KEY_COUNT));
        for chunk_ix in 0..(RS7_KEY_COUNT / 4) {
            let offset = chunk_ix * 4;
            let resp = self.exchange(req_rt_buffer_get(offset, 4), false)?;
            for k in 0..4usize {
                let base = 8 + k * 5;
                let enabled = resp[base] != 0;
                let rel = u16::from_be_bytes([resp[base + 1], resp[base + 2]]);
                let press = u16::from_be_bytes([resp[base + 3], resp[base + 4]]);
                rts.push(RapidTriggerConfig::new(
                    enabled,
                    enabled,
                    TravelHundredthsMm::from_hundredths(press),
                    TravelHundredthsMm::from_hundredths(rel),
                ));
            }
        }

        let keys: Vec<KeyHallEffectConfig> = (0..usize::from(RS7_KEY_COUNT))
            .map(|ix| {
                let key = KeyIndex::new(u16::try_from(ix).unwrap_or(0));
                KeyHallEffectConfig::new(key, actuations[ix], rts[ix])
                    .with_deadzones(Some(top_dz), Some(bottom_dz))
            })
            .collect();

        // 4. Read active SOCD and RS pairs across hardware slots (0..20), remembering occupied and
        // non-SOCD slots so writes update in place and clear removed pairs safely.
        let scan = self.scan_paired_slots()?;
        let socd_pairs = scan.socd_pairs;
        let rs_pairs = scan.rs_pairs;
        self.socd_slots = Some(scan.socd_slots);
        self.rs_slots = Some(scan.rs_slots);
        self.reserved_rs_slots = scan.other_reserved;

        // 5. Read active DKS bindings across hardware slots (0..20)
        let mut dks_bindings = Vec::new();
        for slot in 0..RS7_MAX_DKS_SLOTS {
            if let Ok(resp) = self.exchange(req_dks_get(slot), false)
                && let Some(binding) = decode_dks_slot(slot, &resp)
            {
                dks_bindings.push(binding);
            }
        }

        // 6. Calibration init status (per-key flags + aggregate status)
        let calibrated_keys = self.read_calibrated_keys_init()?;
        let calibration = if calibrated_keys.is_empty() {
            CalibrationStatus::Unknown
        } else if calibrated_keys.iter().any(|&v| v) {
            CalibrationStatus::Calibrated
        } else {
            CalibrationStatus::NeedsCalibration
        };

        Ok(
            HallEffectSnapshot::new(keys, dks_bindings, socd_pairs, calibration)
                .with_rapid_switch_pairs(rs_pairs)
                .with_calibrated_keys(calibrated_keys),
        )
    }

    fn read_calibration_status(&mut self) -> CoreResult<CalibrationStatus> {
        let calibrated_keys = self.read_calibrated_keys_init()?;
        if calibrated_keys.is_empty() {
            Ok(CalibrationStatus::Unknown)
        } else if calibrated_keys.iter().any(|&v| v) {
            Ok(CalibrationStatus::Calibrated)
        } else {
            Ok(CalibrationStatus::NeedsCalibration)
        }
    }

    fn set_key_actuation(&mut self, key: KeyIndex, point: TravelHundredthsMm) -> CoreResult<()> {
        self.he_caps.validate_key(key)?;
        self.he_caps.validate_actuation(point)?;
        let (row, col) = key_to_row_col(key)?;
        self.exchange(req_single_actuation_set(row, col, point), true)?;
        Ok(())
    }

    fn set_global_actuation(&mut self, point: TravelHundredthsMm) -> CoreResult<()> {
        self.he_caps.validate_actuation(point)?;
        let all = vec![point; usize::from(RS7_KEY_COUNT)];
        let chunks: Vec<&[TravelHundredthsMm]> = all.chunks(11).collect();
        let last_ix = chunks.len().saturating_sub(1);
        for (ix, chunk) in chunks.iter().enumerate() {
            let op = if ix == 0 {
                flash_op::ERASE
            } else if ix == last_ix {
                flash_op::WRITE
            } else {
                flash_op::NORMAL
            };
            let offset = u16::try_from(ix * 11).unwrap_or(0);
            let count = u8::try_from(chunk.len()).unwrap_or(11);
            self.exchange(req_travel_buffer_set(offset, count, op, chunk), true)?;
            if !self.transport.is_dry_run() && (ix == 0 || ix == last_ix) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        Ok(())
    }

    fn set_rapid_trigger(&mut self, key: KeyIndex, config: RapidTriggerConfig) -> CoreResult<()> {
        self.he_caps.validate_key(key)?;
        self.he_caps.validate_rapid_trigger(&config)?;
        let (row, col) = key_to_row_col(key)?;
        self.exchange(req_single_rt_set(row, col, &config), true)?;
        Ok(())
    }

    fn set_dks_binding(&mut self, binding: DksBinding) -> CoreResult<()> {
        let slot = u8::try_from(binding.key.value()).map_err(|_| CoreError::OutOfRange {
            field: "dks_slot",
            actual: binding.key.value().to_string(),
            expected: format!("0..{RS7_MAX_DKS_SLOTS}"),
        })?;
        if slot >= RS7_MAX_DKS_SLOTS {
            return Err(CoreError::OutOfRange {
                field: "dks_slot",
                actual: slot.to_string(),
                expected: format!("0..{RS7_MAX_DKS_SLOTS}"),
            });
        }
        self.exchange(req_dks_set(slot, &binding), true)?;
        Ok(())
    }

    fn clear_dks_binding(&mut self, key: KeyIndex) -> CoreResult<()> {
        let empty = DksBinding::new(key, [None, None, None, None]);
        self.set_dks_binding(empty)
    }

    fn set_deadzones(
        &mut self,
        top: TravelHundredthsMm,
        bottom: TravelHundredthsMm,
    ) -> CoreResult<()> {
        if top.as_hundredths() > 100 {
            return Err(CoreError::OutOfRange {
                field: "top_deadzone",
                actual: top.to_string(),
                expected: "0.00..=1.00 mm".to_owned(),
            });
        }
        if bottom.as_hundredths() > 100 {
            return Err(CoreError::OutOfRange {
                field: "bottom_deadzone",
                actual: bottom.to_string(),
                expected: "0.00..=1.00 mm".to_owned(),
            });
        }
        self.exchange(req_deadzone_set(top, bottom), true)?;
        Ok(())
    }

    fn set_socd_pairs(&mut self, pairs: &[SocdPair]) -> CoreResult<()> {
        self.he_caps.validate_socd_pairs(pairs)?;

        // Before mutating real hardware, ensure we know which slots hold SOCD pairs and which
        // hold RS/OKS so we never duplicate pairs or overwrite RS/OKS slots.
        if self.socd_slots.is_none() && !self.transport.is_dry_run() {
            let scan = self.scan_paired_slots()?;
            self.socd_slots = Some(scan.socd_slots);
            self.rs_slots = Some(scan.rs_slots);
            self.reserved_rs_slots = scan.other_reserved;
        }

        let previous_slots = self.socd_slots.clone().unwrap_or_default();
        let rs_occupied = self.rs_slots.clone().unwrap_or_default();
        let mut target_slots: Vec<u8> = Vec::with_capacity(pairs.len());

        // Reuse previously occupied SOCD slots in order first, then claim lowest free non-reserved slots.
        for &slot in &previous_slots {
            if target_slots.len() < pairs.len() {
                target_slots.push(slot);
            }
        }
        for slot in 0..RS7_MAX_SOCD_SLOTS {
            if target_slots.len() >= pairs.len() {
                break;
            }
            if !target_slots.contains(&slot)
                && !rs_occupied.contains(&slot)
                && !self.reserved_rs_slots.contains(&slot)
            {
                target_slots.push(slot);
            }
        }
        if target_slots.len() < pairs.len() {
            return Err(CoreError::OutOfRange {
                field: "socd_pairs",
                actual: pairs.len().to_string(),
                expected: format!(
                    "at most {} free SOCD slots",
                    usize::from(RS7_MAX_SOCD_SLOTS)
                        .saturating_sub(rs_occupied.len() + self.reserved_rs_slots.len()),
                ),
            });
        }

        // Write updated/new pairs, then clear any previously occupied SOCD slots no longer used.
        let write_result = (|| -> CoreResult<()> {
            for (&slot, pair) in target_slots.iter().zip(pairs.iter()) {
                let payload = req_socd_set(slot, Some(pair))?;
                self.exchange(payload, true)?;
            }
            for &old_slot in &previous_slots {
                if !target_slots.contains(&old_slot) {
                    let payload = req_socd_set(old_slot, None)?;
                    self.exchange(payload, true)?;
                }
            }
            Ok(())
        })();

        match write_result {
            Ok(()) => {
                if !self.transport.is_dry_run() {
                    self.socd_slots = Some(target_slots);
                }
                Ok(())
            }
            Err(err) => {
                // Force a fresh slot scan on the next attempt since partial writes may have landed.
                self.socd_slots = None;
                Err(err)
            }
        }
    }

    fn set_rapid_switch_pairs(&mut self, pairs: &[RapidSwitchPair]) -> CoreResult<()> {
        self.he_caps.validate_rapid_switch_pairs(pairs)?;
        if self.rs_slots.is_none() && !self.transport.is_dry_run() {
            let scan = self.scan_paired_slots()?;
            self.socd_slots = Some(scan.socd_slots);
            self.rs_slots = Some(scan.rs_slots);
            self.reserved_rs_slots = scan.other_reserved;
        }

        let previous_slots = self.rs_slots.clone().unwrap_or_default();
        let socd_occupied = self.socd_slots.clone().unwrap_or_default();
        let mut target_slots: Vec<u8> = Vec::with_capacity(pairs.len());

        for &slot in &previous_slots {
            if target_slots.len() < pairs.len() {
                target_slots.push(slot);
            }
        }
        for slot in 0..RS7_MAX_SOCD_SLOTS {
            if target_slots.len() >= pairs.len() {
                break;
            }
            if !target_slots.contains(&slot)
                && !socd_occupied.contains(&slot)
                && !self.reserved_rs_slots.contains(&slot)
            {
                target_slots.push(slot);
            }
        }
        if target_slots.len() < pairs.len() {
            return Err(CoreError::OutOfRange {
                field: "rapid_switch_pairs",
                actual: pairs.len().to_string(),
                expected: format!(
                    "at most {} free slots",
                    usize::from(RS7_MAX_SOCD_SLOTS)
                        .saturating_sub(socd_occupied.len() + self.reserved_rs_slots.len())
                ),
            });
        }

        let write_result = (|| -> CoreResult<()> {
            for (&slot, pair) in target_slots.iter().zip(pairs.iter()) {
                let payload = req_rs_pair_set(slot, Some(pair))?;
                self.exchange(payload, true)?;
            }
            for &old_slot in &previous_slots {
                if !target_slots.contains(&old_slot) {
                    let payload = req_rs_pair_set(old_slot, None)?;
                    self.exchange(payload, true)?;
                }
            }
            Ok(())
        })();

        match write_result {
            Ok(()) => {
                if !self.transport.is_dry_run() {
                    self.rs_slots = Some(target_slots);
                }
                Ok(())
            }
            Err(err) => {
                self.rs_slots = None;
                Err(err)
            }
        }
    }

    fn set_mod_tap_binding(&mut self, binding: ModTapBinding) -> CoreResult<()> {
        if binding.slot >= RS7_MAX_MT_SLOTS {
            return Err(CoreError::OutOfRange {
                field: "mt_slot",
                actual: binding.slot.to_string(),
                expected: format!("0..{RS7_MAX_MT_SLOTS}"),
            });
        }
        let (row, col) = key_to_row_col(binding.key)?;
        self.exchange(
            req_tap_dance_set(
                binding.slot,
                binding.tap_keycode,
                binding.hold_keycode,
                binding.hold_delay_ms,
            ),
            true,
        )?;
        self.exchange(
            req_keymap_set_keycode(
                0,
                row,
                col,
                KeyCode::new(key_class::TAP_DANCE, binding.slot),
            ),
            true,
        )?;
        Ok(())
    }

    fn clear_mod_tap_binding(&mut self, slot: u8, key: KeyIndex) -> CoreResult<()> {
        if slot >= RS7_MAX_MT_SLOTS {
            return Err(CoreError::OutOfRange {
                field: "mt_slot",
                actual: slot.to_string(),
                expected: format!("0..{RS7_MAX_MT_SLOTS}"),
            });
        }
        let _ = key_to_row_col(key)?;
        self.exchange(req_tap_dance_set(slot, (0, 0), (0, 0), 200), true)?;
        Ok(())
    }

    fn set_toggle_binding(&mut self, binding: ToggleKeyBinding) -> CoreResult<()> {
        if binding.slot >= RS7_MAX_TGL_SLOTS {
            return Err(CoreError::OutOfRange {
                field: "tgl_slot",
                actual: binding.slot.to_string(),
                expected: format!("0..{RS7_MAX_TGL_SLOTS}"),
            });
        }
        let (row, col) = key_to_row_col(binding.key)?;
        let hw_slot = binding.slot + RS7_MAX_MT_SLOTS;
        self.exchange(
            req_tap_dance_set(hw_slot, binding.keycode, (0, 0), binding.delay_ms),
            true,
        )?;
        self.exchange(
            req_keymap_set_keycode(0, row, col, KeyCode::new(key_class::TAP_DANCE, hw_slot)),
            true,
        )?;
        Ok(())
    }

    fn clear_toggle_binding(&mut self, slot: u8, key: KeyIndex) -> CoreResult<()> {
        if slot >= RS7_MAX_TGL_SLOTS {
            return Err(CoreError::OutOfRange {
                field: "tgl_slot",
                actual: slot.to_string(),
                expected: format!("0..{RS7_MAX_TGL_SLOTS}"),
            });
        }
        let _ = key_to_row_col(key)?;
        let hw_slot = slot + RS7_MAX_MT_SLOTS;
        self.exchange(req_tap_dance_set(hw_slot, (0, 0), (0, 0), 200), true)?;
        Ok(())
    }

    fn start_calibration(&mut self) -> CoreResult<()> {
        self.exchange(req_calibration_start(), true)?;
        Ok(())
    }

    fn finish_calibration(&mut self) -> CoreResult<()> {
        self.exchange(req_calibration_finish(), true)?;
        Ok(())
    }

    fn read_calibration_key_progress(&mut self) -> CoreResult<Vec<bool>> {
        let key_count = self.spec.key_count();
        let mut flags = Vec::with_capacity(usize::from(key_count));
        for chunk_ix in 0..(key_count / 4) {
            let offset = chunk_ix * 4;
            let resp = self.exchange(req_calibration_live_status_get(offset, 4), false)?;
            for k in 0..4usize {
                flags.push(resp[8 + k * 5 + 4] != 0);
            }
        }
        Ok(flags)
    }

    fn read_live_key_travel(&mut self) -> CoreResult<Vec<KeyTravelSample>> {
        let chunk_count = self.spec.key_count() / 4;
        let mut samples = Vec::new();
        for chunk_ix in 0..chunk_count {
            self.read_live_travel_chunk(chunk_ix, chunk_ix == 0, &mut samples)?;
        }
        Ok(samples)
    }

    fn read_live_key_travel_focused(
        &mut self,
        focus_keys: &[KeyIndex],
    ) -> CoreResult<Vec<KeyTravelSample>> {
        let chunk_count = (self.spec.key_count() / 4).max(1);
        let mut focus_chunks: Vec<u16> = Vec::with_capacity(focus_keys.len());
        for key in focus_keys {
            if key.value() < self.spec.key_count() && !focus_chunks.contains(&(key.value() / 4)) {
                focus_chunks.push(key.value() / 4);
            }
        }
        let mut samples = Vec::new();

        if focus_chunks.is_empty() {
            let first = self.live_scan_chunk % chunk_count;
            self.live_scan_chunk = (first + 1) % chunk_count;
            self.read_live_travel_chunk(first, true, &mut samples)?;
            if chunk_count > 1 {
                let second = self.live_scan_chunk % chunk_count;
                self.live_scan_chunk = (second + 1) % chunk_count;
                self.read_live_travel_chunk(second, false, &mut samples)?;
            }
        } else {
            for (i, &chunk) in focus_chunks.iter().enumerate() {
                self.read_live_travel_chunk(chunk, i == 0, &mut samples)?;
            }
            // One background chunk per call, skipping chunks the focus reads already covered.
            for _ in 0..chunk_count {
                let scan_chunk = self.live_scan_chunk % chunk_count;
                self.live_scan_chunk = (scan_chunk + 1) % chunk_count;
                if !focus_chunks.contains(&scan_chunk) {
                    self.read_live_travel_chunk(scan_chunk, false, &mut samples)?;
                    break;
                }
            }
        }
        Ok(samples)
    }
}

impl<T: HidTransport> RgbController for AtkRs7Device<T> {
    fn rgb_capabilities(&self) -> RgbCapabilities {
        RgbCapabilities::new(
            vec![
                RgbZoneInfo::new(RgbZoneId::new(0), "Back Lamp")
                    .with_presets(back_lamp_presets())
                    .with_max_speed(255),
                RgbZoneInfo::new(RgbZoneId::new(1), "Logo Light")
                    .with_presets(logo_light_presets())
                    .with_max_speed(40),
            ],
            vec![
                RgbEffectKind::Off,
                RgbEffectKind::Static,
                RgbEffectKind::Breathing,
                RgbEffectKind::SpectrumCycle,
                RgbEffectKind::Wave,
                RgbEffectKind::Reactive,
                RgbEffectKind::Preset,
            ],
            true,
        )
    }

    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot> {
        let resp = self.exchange(req_backlight_get(), false)?;
        let (effect, brightness_pct) = decode_backlight(&resp);
        let brightness = Brightness::new(brightness_pct)?;
        let mut zones = vec![RgbZoneState::new(RgbZoneId::new(0), effect, brightness)];

        if let Ok(logo_resp) = self.exchange(req_logo_light_get(), false) {
            let (logo_effect, logo_brightness_pct) = decode_logo_light(&logo_resp);
            let logo_brightness = Brightness::new(logo_brightness_pct)?;
            zones.push(RgbZoneState::new(
                RgbZoneId::new(1),
                logo_effect,
                logo_brightness,
            ));
        }
        Ok(RgbSnapshot::new(zones))
    }

    fn set_zone_effect(
        &mut self,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()> {
        match zone.value() {
            0 => {
                self.exchange(req_backlight_set(effect, brightness.percent()), true)?;
                Ok(())
            }
            1 => {
                self.exchange(req_logo_light_set(effect, brightness.percent()), true)?;
                Ok(())
            }
            other => Err(CoreError::OutOfRange {
                field: "rgb_zone",
                actual: other.to_string(),
                expected: "0 (Back Lamp) or 1 (Logo Light)".to_owned(),
            }),
        }
    }
}

impl<T: HidTransport> KeymapController for AtkRs7Device<T> {
    fn keymap_capabilities(&self) -> KeymapCapabilities {
        KeymapCapabilities::new(
            vec![
                "Win Base".to_owned(),
                "Win Fn".to_owned(),
                "Mac Base".to_owned(),
                "Mac Fn".to_owned(),
            ],
            RS7_ROWS,
            RS7_COLS,
            true,
        )
    }

    fn read_keymap(&mut self) -> CoreResult<KeymapSnapshot> {
        let total_keys = usize::from(RS7_LAYERS) * usize::from(RS7_KEY_COUNT);
        let total_bytes = total_keys * 2; // 768 bytes
        let mut flat: Vec<KeyCode> = Vec::with_capacity(total_keys);
        let mut offset = 0usize;
        while offset < total_bytes {
            let chunk_len = (total_bytes - offset).min(28);
            let resp = self.exchange(
                req_keymap_get_buffer(
                    u16::try_from(offset).unwrap_or(0),
                    u8::try_from(chunk_len).unwrap_or(28),
                ),
                false,
            )?;
            for &[class, id] in resp[4..4 + chunk_len].as_chunks::<2>().0 {
                flat.push(KeyCode::new(class, id));
            }
            offset += chunk_len;
        }
        let layers: Vec<Vec<KeyCode>> = flat
            .chunks(usize::from(RS7_KEY_COUNT))
            .map(<[KeyCode]>::to_vec)
            .collect();
        let active_layer = self
            .exchange(req_active_layer_get(), false)
            .map_or(0, |r| decode_active_layer(r[3]));
        Ok(KeymapSnapshot::new(layers, active_layer))
    }

    fn set_key_binding(&mut self, layer: u8, key: KeyIndex, code: KeyCode) -> CoreResult<()> {
        self.keymap_capabilities().validate_layer(layer)?;
        let (row, col) = key_to_row_col(key)?;
        self.exchange(req_keymap_set_keycode(layer, row, col, code), true)?;
        Ok(())
    }

    fn reset_keymap(&mut self) -> CoreResult<()> {
        self.exchange(req_keymap_reset(), true)?;
        Ok(())
    }
}

impl<T: HidTransport> MacroController for AtkRs7Device<T> {
    fn macro_capabilities(&self) -> MacroCapabilities {
        MacroCapabilities::new(RS7_MAX_MACRO_SLOTS, 512)
    }

    fn read_macros(&mut self) -> CoreResult<Vec<MacroSlot>> {
        let sz_resp = self.exchange(req_macro_buffer_size_get(), false)?;
        let total_size = usize::from(u16::from_be_bytes([sz_resp[1], sz_resp[2]])).min(2048);
        let mut raw = Vec::with_capacity(total_size);
        let mut offset = 0usize;
        while offset < total_size {
            let chunk = (total_size - offset).min(28);
            let resp = self.exchange(
                req_macro_buffer_get(
                    u16::try_from(offset).unwrap_or(0),
                    u8::try_from(chunk).unwrap_or(28),
                ),
                false,
            )?;
            raw.extend_from_slice(&resp[4..4 + chunk]);
            let terminators = raw.iter().fold(0usize, |acc, &b| acc + usize::from(b == 0));
            if terminators > usize::from(RS7_MAX_MACRO_SLOTS) {
                break;
            }
            offset += chunk;
        }
        Ok(decode_macros(&raw))
    }

    fn write_macros(&mut self, macros: &[MacroSlot]) -> CoreResult<()> {
        let encoded = encode_macros(macros);
        for (ix, chunk) in encoded.chunks(28).enumerate() {
            let offset = u16::try_from(ix * 28).unwrap_or(0);
            self.exchange(req_macro_buffer_set(offset, chunk), true)?;
        }
        Ok(())
    }
}

impl<T: HidTransport> KeyboardConfigController for AtkRs7Device<T> {
    fn keyboard_config_capabilities(&self) -> KeyboardConfigCapabilities {
        KeyboardConfigCapabilities::new(
            true,
            true,
            true,
            true,
            true,
            true,
            vec![
                PollingRateHz::Hz1000,
                PollingRateHz::Hz2000,
                PollingRateHz::Hz4000,
                PollingRateHz::Hz8000,
            ],
        )
    }

    fn read_keyboard_config(&mut self) -> CoreResult<KeyboardConfigSnapshot> {
        let feat = self.exchange(req_feature_flags_get(), false)?;
        let (rgb_area, exchange_wasd, mac_mode, win_lock, six_key_rollover) =
            decode_feature_flags(&feat);
        self.rgb_area = rgb_area;

        let anti_accidental_touch = self
            .exchange(req_anti_accidental_touch_get(), false)
            .is_ok_and(|r| r[3] != 0);
        let extreme_mode = self
            .exchange(req_game_mode(), false)
            .map_or(self.extreme_mode, |r| r[3] == 1);
        let polling_rate = self
            .exchange(req_report_rate(), false)
            .ok()
            .and_then(|r| decode_report_rate(r[3]))
            .or(self.polling_rate)
            .unwrap_or(PollingRateHz::Hz1000);

        self.extreme_mode = extreme_mode;
        self.he_caps = build_he_caps(&self.spec, extreme_mode);
        self.polling_rate = Some(polling_rate);

        Ok(KeyboardConfigSnapshot::new(
            mac_mode,
            exchange_wasd,
            win_lock,
            six_key_rollover,
            anti_accidental_touch,
            extreme_mode,
            polling_rate,
        ))
    }

    fn set_keyboard_config(&mut self, config: &KeyboardConfigSnapshot) -> CoreResult<()> {
        let rate_req = req_report_rate_set(config.polling_rate)?;
        self.exchange(
            req_feature_flags_set(
                self.rgb_area,
                config.exchange_wasd,
                config.mac_mode,
                config.win_lock,
                config.six_key_rollover,
            ),
            true,
        )?;
        self.exchange(
            req_anti_accidental_touch_set(config.anti_accidental_touch),
            true,
        )?;
        self.exchange(req_game_mode_set(config.extreme_mode), true)?;
        self.exchange(rate_req, true)?;
        if !self.transport.is_dry_run() {
            self.extreme_mode = config.extreme_mode;
            self.he_caps = build_he_caps(&self.spec, config.extreme_mode);
            self.polling_rate = Some(config.polling_rate);
        }
        Ok(())
    }
}

/// Reads one Tap-Dance hardware slot (`0..19` MT, `20..39` TGL).
pub fn read_tap_dance_slot<T: HidTransport>(
    dev: &mut AtkRs7Device<T>,
    slot: u8,
) -> CoreResult<TapDanceEntry> {
    let resp = dev.exchange(req_tap_dance_get(slot), false)?;
    let (tap, hold, hold_delay_ms) = decode_tap_dance(&resp);
    Ok(TapDanceEntry {
        tap,
        hold,
        hold_delay_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::{RgbColor, SocdMode};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome, PacketValidator,
        ReportKind,
    };

    fn rs7_descriptor() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            ATK_VID,
            ATK_RS7_PID,
            ATK_RS7_USAGE_PAGE,
            ATK_RS7_USAGE_ID,
            1,
            "mock://atk/rs7",
        )
        .with_strings(
            Some("Shenzhen Yizhita Technology".to_owned()),
            Some("ATK RS7 RGB".to_owned()),
            Some("ATK RS HE".to_owned()),
        )
    }

    fn push_open_replies(mock: &mut MockHidTransport, extreme: bool) {
        // Firmware v1.12 (0x01, 0x12)
        let mut fw = vec![0u8; RS7_PAYLOAD_LEN];
        fw[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        fw[1] = protocol::sub::CUSTOM_ID;
        fw[5] = 0x01;
        fw[6] = 0x12;
        mock.push_input_response(fw);

        // Game mode
        let mut gm = vec![0u8; RS7_PAYLOAD_LEN];
        gm[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        gm[1] = protocol::sub::CUSTOM_ID;
        gm[2] = protocol::sub::GAME_MODE;
        gm[3] = u8::from(extreme);
        mock.push_input_response(gm);

        // Polling rate = 8000 Hz (8)
        let mut rr = vec![0u8; RS7_PAYLOAD_LEN];
        rr[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        rr[1] = protocol::sub::CUSTOM_ID;
        rr[2] = protocol::sub::REPORT_RATE;
        rr[3] = 8;
        mock.push_input_response(rr);
    }

    #[test]
    fn safety_validator_hard_blocks_bootloader_iap_and_reset_commands() {
        let validator = atk_rs7_validator();

        // Valid single-key actuation write passes
        let ok_tx = wrap_tx(req_single_actuation_set(
            2,
            1,
            TravelHundredthsMm::from_hundredths(40),
        ));
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &ok_tx)
                .is_ok()
        );

        // Bootloader jump (0x0B) MUST be rejected
        let mut boot = [0u8; RS7_TX_REPORT_LEN];
        boot[1] = protocol::cmd::BOOTLOADER_JUMP;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &boot)
                .is_err()
        );

        // EEPROM reset (0x0A) MUST be rejected
        let mut eeprom = [0u8; RS7_TX_REPORT_LEN];
        eeprom[1] = protocol::cmd::EEPROM_RESET;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &eeprom)
                .is_err()
        );

        // DuckBread resetAll (0x96 / 0x15) MUST be rejected
        let mut reset_all = [0u8; RS7_TX_REPORT_LEN];
        reset_all[1] = protocol::cmd::SET_KEYBOARD_VALUE;
        reset_all[2] = protocol::sub::CUSTOM_ID;
        reset_all[3] = protocol::sub::RESET_ALL;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &reset_all)
                .is_err()
        );

        // DuckBread IAP (0x96 / 0x98) MUST be rejected
        let mut iap = [0u8; RS7_TX_REPORT_LEN];
        iap[1] = protocol::cmd::SET_KEYBOARD_VALUE;
        iap[2] = protocol::sub::CUSTOM_ID;
        iap[3] = protocol::sub::IAP;
        assert!(
            validator
                .validate_outgoing(ReportKind::Output, &iap)
                .is_err()
        );
    }

    #[test]
    fn reads_rs7_discovery_actuation_rt_socd_calibration_and_rgb() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, true);

        // Deadzone: top 0.10mm (10), bottom 0.15mm (15)
        let mut dz = vec![0u8; RS7_PAYLOAD_LEN];
        dz[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        dz[3..5].copy_from_slice(&10u16.to_be_bytes());
        dz[5..7].copy_from_slice(&15u16.to_be_bytes());
        mock.push_input_response(dz);

        // 8 actuation chunks (12 keys each), all set to 1.50 mm (150 = 0x0096)
        for _ in 0..8 {
            let mut chunk = vec![0u8; RS7_PAYLOAD_LEN];
            chunk[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            for k in 0..12 {
                chunk[8 + k * 2..10 + k * 2].copy_from_slice(&150u16.to_be_bytes());
            }
            mock.push_input_response(chunk);
        }

        // 24 RT chunks (4 keys each), enabled with 0.20 mm press / 0.25 mm release
        for _ in 0..24 {
            let mut chunk = vec![0u8; RS7_PAYLOAD_LEN];
            chunk[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            for k in 0..4 {
                let b = 8 + k * 5;
                chunk[b] = 1;
                chunk[b + 1..b + 3].copy_from_slice(&25u16.to_be_bytes());
                chunk[b + 3..b + 5].copy_from_slice(&20u16.to_be_bytes());
            }
            mock.push_input_response(chunk);
        }

        // 20 SOCD slots: slot 0 = A(2,1) + D(2,3) LastInputPriority; slots 1..20 empty
        for slot in 0..20 {
            let mut s = vec![0u8; RS7_PAYLOAD_LEN];
            s[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            if slot == 0 {
                s[7] = protocol::socd_wire::SOCD_LAST;
                s[13] = 2;
                s[14] = 1;
                s[15] = 2;
                s[16] = 3;
            }
            mock.push_input_response(s);
        }

        // 20 DKS slots: empty
        for _ in 0..20 {
            let mut d = vec![0u8; RS7_PAYLOAD_LEN];
            d[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            mock.push_input_response(d);
        }

        // 4 calibration status chunks (24 keys each): calibrated
        for _ in 0..4 {
            let mut c = vec![0u8; RS7_PAYLOAD_LEN];
            c[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            for k in 0..24 {
                c[8 + k] = 1;
            }
            mock.push_input_response(c);
        }

        // Backlight RGB: Static cyan @ 80%
        let mut rgb = vec![0u8; RS7_PAYLOAD_LEN];
        rgb[0] = protocol::cmd::LIGHTING_GET_VALUE;
        rgb[2] = 2; // Static
        rgb[5] = 0x00;
        rgb[6] = 0xE5;
        rgb[7] = 0xFF;
        rgb[8] = 80;
        mock.push_input_response(rgb);

        let mut dev = AtkRs7Device::open(mock, false, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.12"));
        assert!(dev.is_extreme_mode());
        assert_eq!(dev.polling_rate(), Some(PollingRateHz::Hz8000));
        assert_eq!(
            dev.hall_effect_capabilities().actuation_step,
            TravelHundredthsMm::from_hundredths(1)
        );

        let snap = dev.read_hall_effect_state().unwrap();
        assert_eq!(snap.keys.len(), 96);
        assert_eq!(snap.keys[0].actuation.as_hundredths(), 150);
        assert_eq!(
            snap.keys[0].top_deadzone,
            Some(TravelHundredthsMm::from_hundredths(10))
        );
        assert!(snap.keys[0].rapid_trigger.is_enabled());
        assert_eq!(
            snap.keys[0].rapid_trigger.press_sensitivity.as_hundredths(),
            20
        );
        assert_eq!(
            snap.keys[0]
                .rapid_trigger
                .release_sensitivity
                .as_hundredths(),
            25
        );
        assert_eq!(snap.socd_pairs.len(), 1);
        assert_eq!(snap.socd_pairs[0].mode, SocdMode::LastInputPriority);
        assert_eq!(snap.calibration, CalibrationStatus::Calibrated);

        let light = dev.read_rgb_state().unwrap();
        assert_eq!(light.zones[0].brightness.percent(), 80);
        assert_eq!(
            light.zones[0].effect,
            RgbEffect::Preset {
                id: 2,
                color: RgbColor::new(0x00, 0xE5, 0xFF),
                speed: 0,
                colorful: false,
            }
        );
    }

    #[test]
    fn dry_run_validates_and_suppresses_actuation_rt_and_socd_writes() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, false);

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = AtkRs7Device::open(mock, true, Some(sink.clone())).unwrap();

        // Out-of-step (0.11 mm in normal 0.02 mm step mode) must fail validation before TX.
        assert!(
            dev.set_key_actuation(KeyIndex::new(33), TravelHundredthsMm::from_hundredths(11))
                .is_err()
        );

        // Valid writes must pass validator and be suppressed by dry-run.
        dev.set_key_actuation(KeyIndex::new(33), TravelHundredthsMm::from_hundredths(40))
            .unwrap();
        dev.set_rapid_trigger(
            KeyIndex::new(33),
            RapidTriggerConfig::new(
                true,
                true,
                TravelHundredthsMm::from_hundredths(20),
                TravelHundredthsMm::from_hundredths(20),
            ),
        )
        .unwrap();
        let pair = SocdPair::new(
            protocol::row_col_to_key(2, 1),
            protocol::row_col_to_key(2, 3),
            SocdMode::LastInputPriority,
        )
        .unwrap();
        dev.set_socd_pairs(&[pair]).unwrap();

        // Only the 3 read-only open() discovery packets touched the mock transport.
        assert_eq!(dev.transport().inner().recorded_writes().len(), 3);

        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert_eq!(suppressed, 3);
    }

    #[test]
    fn socd_preserves_sparse_slots_skips_non_socd_and_clears_removed_slots() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, false);

        // Scan 20 slots:
        // - slot 0 holds non-SOCD Rappy Snappy (mode=1) -> must never be overwritten
        // - slot 4 holds SOCD A(3,1) + D(3,3) LastInputPriority -> must be updated in place
        for slot in 0..20u8 {
            let mut s = vec![0u8; RS7_PAYLOAD_LEN];
            s[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            if slot == 0 {
                s[7] = protocol::socd_wire::RS;
            } else if slot == 4 {
                s[7] = protocol::socd_wire::SOCD_LAST;
                s[13] = 3;
                s[14] = 1;
                s[15] = 3;
                s[16] = 3;
            }
            mock.push_input_response(s);
        }

        // Replies for adding W/S (writes slot 4 + slot 1, skipping reserved slot 0)
        for _ in 0..2 {
            let mut ack = vec![0u8; RS7_PAYLOAD_LEN];
            ack[0] = protocol::cmd::SET_KEYBOARD_VALUE;
            mock.push_input_response(ack);
        }
        // Replies for removing W/S (writes slot 4 + clears slot 1 with mode=NONE)
        for _ in 0..2 {
            let mut ack = vec![0u8; RS7_PAYLOAD_LEN];
            ack[0] = protocol::cmd::SET_KEYBOARD_VALUE;
            mock.push_input_response(ack);
        }

        let mut dev = AtkRs7Device::open(mock, false, None).unwrap();
        let ad = SocdPair::new(
            protocol::row_col_to_key(3, 1),
            protocol::row_col_to_key(3, 3),
            SocdMode::Neutral,
        )
        .unwrap();
        let ws = SocdPair::new(
            protocol::row_col_to_key(2, 2),
            protocol::row_col_to_key(3, 2),
            SocdMode::LastInputPriority,
        )
        .unwrap();

        // 1. Add W/S and change A/D mode -> updates slot 4 in place and claims slot 1 (skipping slot 0)
        dev.set_socd_pairs(&[ad, ws]).unwrap();
        // 2. Remove W/S -> keeps slot 4 and clears slot 1 with mode=NONE
        dev.set_socd_pairs(&[ad]).unwrap();

        let writes = dev.transport().inner().recorded_writes();
        // Filter SET_KEYBOARD_VALUE + RS_SOCD frames (byte 0 = Report ID 0x00, byte 1 = cmd 0x03)
        let socd_sets: Vec<&[u8]> = writes
            .iter()
            .map(|w| w.bytes.as_slice())
            .filter(|w| {
                w.len() == RS7_TX_REPORT_LEN
                    && w[1] == protocol::cmd::SET_KEYBOARD_VALUE
                    && w[2] == protocol::sub::CUSTOM_ID
                    && w[3] == protocol::sub::RS_SOCD
            })
            .collect();
        assert_eq!(socd_sets.len(), 4);
        // Write 0: slot 4 updated to Neutral (mode=5)
        assert_eq!(socd_sets[0][7], 4);
        assert_eq!(socd_sets[0][8], protocol::socd_wire::SOCD_BALANCE);
        // Write 1: slot 1 claimed for W/S (slot 0 preserved for RS!)
        assert_eq!(socd_sets[1][7], 1);
        assert_eq!(socd_sets[1][8], protocol::socd_wire::SOCD_LAST);
        // Write 2: slot 4 rewritten with A/D
        assert_eq!(socd_sets[2][7], 4);
        assert_eq!(socd_sets[2][8], protocol::socd_wire::SOCD_BALANCE);
        // Write 3: slot 1 explicitly cleared with NONE (0)
        assert_eq!(socd_sets[3][7], 1);
        assert_eq!(socd_sets[3][8], protocol::socd_wire::NONE);
    }

    #[test]
    fn logo_light_feature_flags_and_macro_codecs_roundtrip() {
        // Logo light Always On (mode 3) @ 75% brightness -> 120 raw out of 160
        let req = req_logo_light_set(
            RgbEffect::Preset {
                id: 3,
                color: RgbColor::new(0x10, 0x80, 0xFF),
                speed: 20,
                colorful: true,
            },
            75,
        );
        let (eff, pct) = decode_logo_light(&req);
        assert_eq!(pct, 75);
        assert_eq!(
            eff,
            RgbEffect::Preset {
                id: 3,
                color: RgbColor::new(0x10, 0x80, 0xFF),
                speed: 20,
                colorful: true,
            }
        );

        // Feature flags: 6KRO inverted on wire (true => byte 0, false => byte 1)
        let f = req_feature_flags_set(0, true, false, true, true);
        assert_eq!(f[7], 0);
        assert_eq!(decode_feature_flags(&f), (0, true, false, true, true));

        // Macro roundtrip with key down + 50ms delay + key up
        let macros = vec![MacroSlot::new(
            0,
            "M1",
            vec![
                agnoperiph_core::MacroStep::new(
                    agnoperiph_core::MacroActionKind::KeyDown,
                    0x04,
                    50,
                ),
                agnoperiph_core::MacroStep::new(agnoperiph_core::MacroActionKind::KeyUp, 0x04, 0),
            ],
        )];
        let encoded = encode_macros(&macros);
        let decoded = decode_macros(&encoded);
        assert_eq!(decoded.len(), usize::from(RS7_MAX_MACRO_SLOTS));
        assert_eq!(decoded[0].steps, macros[0].steps);

        // SOCD and RS per-pair trigger travel + Rapid Trigger wire roundtrip (`withGlobal = 0`)
        let socd = SocdPair::new(
            protocol::row_col_to_key(3, 1),
            protocol::row_col_to_key(3, 3),
            SocdMode::LastInputPriority,
        )
        .unwrap()
        .with_trigger_point(TravelHundredthsMm::from_hundredths(40))
        .with_rapid_trigger(true);
        let socd_frame = req_socd_set(2, Some(&socd)).unwrap();
        assert_eq!(socd_frame[9..11], [0x00, 0x28]); // 0.40 mm BE
        assert_eq!(socd_frame[11], 0); // withGlobal = 0
        assert_eq!(socd_frame[12], 1); // rt = on
        assert_eq!(decode_socd_slot(&socd_frame), Some(socd));

        let rs = RapidSwitchPair::new(
            protocol::row_col_to_key(2, 2),
            protocol::row_col_to_key(3, 2),
        )
        .unwrap()
        .with_trigger_point(TravelHundredthsMm::from_hundredths(80))
        .with_rapid_trigger(true);
        let rs_frame = req_rs_pair_set(5, Some(&rs)).unwrap();
        assert_eq!(rs_frame[9..11], [0x00, 0x50]); // 0.80 mm BE
        assert_eq!(rs_frame[11], 0); // withGlobal = 0
        assert_eq!(rs_frame[12], 1); // rt = on
        assert_eq!(decode_rs_slot(&rs_frame), Some(rs));
    }

    #[test]
    fn atk_device_dispatches_and_reads_live_key_travel() {
        // 1. DuckBread 0xFF60 dispatch + live key travel decoding (0x1C chunks)
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, true);
        for chunk_ix in 0..(RS7_KEY_COUNT / 4) {
            let mut resp = vec![0u8; RS7_PAYLOAD_LEN];
            resp[0] = protocol::cmd::GET_KEYBOARD_VALUE;
            if chunk_ix == 8 {
                // key index 33 = chunk 8, k = 1 -> 1.85 mm (185 = 0x00B9)
                let base = 8 + 5;
                resp[base + 2..base + 4].copy_from_slice(&185u16.to_be_bytes());
            }
            mock.push_input_response(resp);
        }

        let mut dev = AtkDevice::open(mock, false, None).unwrap();
        assert!(matches!(dev, AtkDevice::DuckBread(_)));
        let he = dev.as_hall_effect_mut().unwrap();
        assert!(he.hall_effect_capabilities().has_live_travel());
        let samples = he.read_live_key_travel().unwrap();
        assert_eq!(
            samples,
            vec![KeyTravelSample::new(
                KeyIndex::new(33),
                TravelHundredthsMm::from_hundredths(185),
            )]
        );

        // 2. Unsupported usage page returns validation error
        let bad = HidDeviceDescriptor::new(ATK_VID, 0x9999, 0x0001, 0x0006, 0, "mock://bad");
        assert!(AtkDevice::open(MockHidTransport::new(bad), false, None).is_err());

        // 3. Descriptor covers all three protocol families
        let desc = driver_descriptor();
        assert!(
            desc.rules
                .iter()
                .any(|r| r.usage_page == Some(ATK_RS7_USAGE_PAGE))
        );
        assert!(
            desc.rules
                .iter()
                .any(|r| r.usage_page == Some(ATK_FF01_USAGE_PAGE))
        );
        assert!(
            desc.rules
                .iter()
                .any(|r| r.usage_page == Some(ATK_MOUSE_USAGE_PAGE))
        );
    }

    #[test]
    fn exchange_drains_stale_unsolicited_reports_and_matches_subcmd() {
        // 1. Stale unsolicited reports sitting in the OS HID ring buffer (> old limit of 8)
        //    must be drained before sending a request so the real reply is received.
        let mut stale_mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut stale_mock, false);
        for _ in 0..20 {
            let mut unsolicited = vec![0u8; RS7_PAYLOAD_LEN];
            unsolicited[0] = protocol::cmd::LIGHTING_GET_VALUE;
            unsolicited[1] = protocol::sub::LIGHT_INFO;
            stale_mock.push_stale_input_report(unsolicited);
        }

        // Open cleanly first (drains the 20 stale reports before firmware version query).
        let dev = AtkRs7Device::open(stale_mock, false, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.12"));

        // 2. In-flight unsolicited reports (> 8) plus a mismatched 0x96 subcmd are skipped.
        let mut mock2 = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock2, false);
        for _ in 0..12 {
            let mut noise = vec![0u8; RS7_PAYLOAD_LEN];
            noise[0] = protocol::cmd::LIGHTING_GET_VALUE;
            mock2.push_input_response(noise);
        }
        let mut wrong_subcmd = vec![0u8; RS7_PAYLOAD_LEN];
        wrong_subcmd[0] = protocol::cmd::SET_KEYBOARD_VALUE;
        wrong_subcmd[1] = protocol::sub::CUSTOM_ID;
        wrong_subcmd[2] = protocol::sub::BOX_LIGHT;
        mock2.push_input_response(wrong_subcmd);

        let mut real_ack = vec![0u8; RS7_PAYLOAD_LEN];
        real_ack[0] = protocol::cmd::SET_KEYBOARD_VALUE;
        real_ack[1] = protocol::sub::CUSTOM_ID;
        real_ack[2] = protocol::sub::ONE_TOG_TH;
        mock2.push_input_response(real_ack);

        let mut dev2 = AtkRs7Device::open(mock2, false, None).unwrap();
        dev2.set_key_actuation(KeyIndex::new(33), TravelHundredthsMm::from_hundredths(40))
            .unwrap();
    }

    #[test]
    fn focused_live_key_travel_reads_focus_chunk_plus_one_rotating_scan_chunk() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, true);

        // Call 1: focus_key = 33 (chunk 8) + scan chunk 0.
        let mut focus_resp = vec![0u8; RS7_PAYLOAD_LEN];
        focus_resp[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        focus_resp[8 + 5 + 2..8 + 5 + 4].copy_from_slice(&210u16.to_be_bytes());
        mock.push_input_response(focus_resp);

        let mut scan0_resp = vec![0u8; RS7_PAYLOAD_LEN];
        scan0_resp[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        mock.push_input_response(scan0_resp);

        // Call 2: focus_key = 33 (chunk 8) + scan chunk 1 (discovers key 5 = chunk 1, k = 1).
        let mut focus_resp2 = vec![0u8; RS7_PAYLOAD_LEN];
        focus_resp2[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        mock.push_input_response(focus_resp2);

        let mut scan1_resp = vec![0u8; RS7_PAYLOAD_LEN];
        scan1_resp[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        scan1_resp[8 + 5 + 2..8 + 5 + 4].copy_from_slice(&145u16.to_be_bytes());
        mock.push_input_response(scan1_resp);

        let mut dev = AtkRs7Device::open(mock, false, None).unwrap();
        let first = dev
            .read_live_key_travel_focused(&[KeyIndex::new(33)])
            .unwrap();
        assert_eq!(
            first,
            vec![KeyTravelSample::new(
                KeyIndex::new(33),
                TravelHundredthsMm::from_hundredths(210),
            )]
        );

        let second = dev
            .read_live_key_travel_focused(&[KeyIndex::new(33)])
            .unwrap();
        assert_eq!(
            second,
            vec![KeyTravelSample::new(
                KeyIndex::new(5),
                TravelHundredthsMm::from_hundredths(145),
            )]
        );

        // Verify the two focused calls only sent 4 total live-travel packets (2 per call)
        // with offsets 32, 0, 32, 4 instead of 48 packets for two full-matrix scans.
        let sent = dev.transport().inner().recorded_writes();
        let live_offsets: Vec<u16> = sent
            .iter()
            .filter(|w| {
                w.bytes.len() == RS7_TX_REPORT_LEN
                    && w.bytes[1] == protocol::cmd::GET_KEYBOARD_VALUE
                    && w.bytes[2] == protocol::sub::CUSTOM_ID
                    && w.bytes[3] == protocol::sub::CALIBRATION_LIVE_BUFFER
            })
            .map(|w| u16::from_be_bytes([w.bytes[6], w.bytes[7]]))
            .collect();
        assert_eq!(live_offsets, vec![32, 0, 32, 4]);
    }

    #[test]
    fn focused_live_key_travel_reads_every_focus_chunk_and_skips_them_when_scanning() {
        let mut mock = MockHidTransport::new(rs7_descriptor());
        push_open_replies(&mut mock, true);
        // Stale battery-style frame queued before the request must be drained, not decoded.
        let mut stale = vec![0u8; RS7_PAYLOAD_LEN];
        stale[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        stale[8 + 2..8 + 4].copy_from_slice(&99u16.to_be_bytes());
        mock.push_stale_input_report(stale);

        // Focus keys 49 (chunk 12, k = 1) and 51 (same chunk, k = 3) plus 4 (chunk 1, k = 0).
        let mut chunk12 = vec![0u8; RS7_PAYLOAD_LEN];
        chunk12[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        chunk12[8 + 5 + 2..8 + 5 + 4].copy_from_slice(&200u16.to_be_bytes());
        mock.push_input_response(chunk12);
        let mut chunk1 = vec![0u8; RS7_PAYLOAD_LEN];
        chunk1[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        chunk1[8 + 2..8 + 4].copy_from_slice(&120u16.to_be_bytes());
        mock.push_input_response(chunk1);
        // Scan chunk 0 is not a focus chunk, so it is read; chunk 1 is skipped next time.
        let mut scan0 = vec![0u8; RS7_PAYLOAD_LEN];
        scan0[0] = protocol::cmd::GET_KEYBOARD_VALUE;
        mock.push_input_response(scan0);

        let mut dev = AtkRs7Device::open(mock, false, None).unwrap();
        let samples = dev
            .read_live_key_travel_focused(&[KeyIndex::new(49), KeyIndex::new(51), KeyIndex::new(4)])
            .unwrap();
        assert_eq!(
            samples,
            vec![
                KeyTravelSample::new(KeyIndex::new(49), TravelHundredthsMm::from_hundredths(200)),
                KeyTravelSample::new(KeyIndex::new(4), TravelHundredthsMm::from_hundredths(120)),
            ]
        );

        let sent = dev.transport().inner().recorded_writes();
        let live_offsets: Vec<u16> = sent
            .iter()
            .filter(|w| {
                w.bytes.len() == RS7_TX_REPORT_LEN
                    && w.bytes[1] == protocol::cmd::GET_KEYBOARD_VALUE
                    && w.bytes[2] == protocol::sub::CUSTOM_ID
                    && w.bytes[3] == protocol::sub::CALIBRATION_LIVE_BUFFER
            })
            .map(|w| u16::from_be_bytes([w.bytes[6], w.bytes[7]]))
            .collect();
        assert_eq!(live_offsets, vec![48, 4, 0]);
    }
}
