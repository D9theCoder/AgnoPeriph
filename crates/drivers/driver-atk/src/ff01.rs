//! ATK68 / ATK75 (`0xFF01` GTech) Hall Effect keyboard driver.
//!
//! All framing, command IDs, and model descriptors are verified in
//! `docs/protocols/atk-ff01.md` (from `bsdrop/atk68`).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    Brightness, CalibrationStatus, Capabilities, ConnectionKind, CoreError, CoreResult, DeviceId,
    DeviceInfo, DeviceMatchRule, HallEffectCapabilities, HallEffectKeyboard, HallEffectSnapshot,
    KeyHallEffectConfig, KeyIndex, KeyboardConfigCapabilities, KeyboardConfigController,
    KeyboardConfigSnapshot, Peripheral, PollingRateHz, ProfileCapabilities, ProfileInfo,
    ProfileManager, ProfileMode, ProfileSlot, ProfileSnapshot, RapidTriggerConfig, RgbCapabilities,
    RgbColor, RgbController, RgbDirection, RgbEffect, RgbEffectKind, RgbPresetInfo, RgbSnapshot,
    RgbZoneId, RgbZoneInfo, RgbZoneState, TravelHundredthsMm, VendorKind,
};
use agnoperiph_transport::{
    FixedReportValidator, GuardedTransport, HidTransport, PacketSink, ReportKind, TransportError,
};

/// Vendor HID Usage Page for GTech `0xFF01` keyboards (ATK68 / ATK75 / EDGE / Hex80).
pub const ATK_FF01_USAGE_PAGE: u16 = 0xFF01;
pub const ATK_FF01_REPORT_ID: u8 = 0x00;
pub const ATK_FF01_PAYLOAD_LEN: usize = 64;
pub const ATK_FF01_TX_LEN: usize = 65;

const READ_TIMEOUT: Duration = Duration::from_millis(800);
const MAX_READ_ATTEMPTS: usize = 16;
const FF01_PROFILE_COUNT: u8 = 4;
const FF01_BRIGHTNESS_MAX: u8 = 8;
const FF01_SPEED_MAX: u8 = 4;

/// Command IDs for the `0xFF01` GTech keyboard protocol.
pub mod op {
    pub const GET_KEYBOARD_INFO: u8 = 0x12;
    pub const SAVE_STORAGE: u8 = 0x21;
    pub const RESET_STORAGE: u8 = 0x22; // BLOCKED by safety validator
    pub const GET_REPORT_RATE: u8 = 0x23;
    pub const SET_REPORT_RATE: u8 = 0x24;
    pub const GET_DEVICE_PROFILE: u8 = 0x2B;
    pub const SET_DEVICE_PROFILE: u8 = 0x2C;
    pub const SET_ALL_ACTUATION: u8 = 0x42;
    pub const GET_ALL_ACTUATION: u8 = 0x43;
    pub const SET_LIGHT: u8 = 0x44;
    pub const GET_LIGHT: u8 = 0x45;
    pub const DATA_REPORTED: u8 = 0x48; // Async unsolicited frame
    pub const GET_ONE_FAST_TRIGGER: u8 = 0x54;
    pub const SET_ONE_FAST_TRIGGER: u8 = 0x55;
    pub const BOOTLOADER_JUMP: u8 = 0xAA; // BLOCKED by safety validator
}

/// Declarative specification for one ATK `0xFF01` Hall Effect keyboard model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ff01BoardSpec {
    pub vid: u16,
    pub pid: u16,
    pub model_name: &'static str,
    pub rows: u8,
    pub cols: u8,
    pub step_hundredths: u16,
    pub max_travel_hundredths: u16,
}

impl Ff01BoardSpec {
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

/// Verified ATK68 / ATK75 / EDGE / Hex80 model table from `docs/protocols/atk-ff01.md`.
pub const FF01_BOARDS: &[Ff01BoardSpec] = &[
    Ff01BoardSpec {
        vid: 0x1A81,
        pid: 0x207E,
        model_name: "ATK68",
        rows: 5,
        cols: 15,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x1A81,
        pid: 0x2075,
        model_name: "ATK75 G",
        rows: 6,
        cols: 16,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x1A81,
        pid: 0x2082,
        model_name: "ATK75 L",
        rows: 6,
        cols: 16,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x1A81,
        pid: 0x2083,
        model_name: "ATK68 L",
        rows: 5,
        cols: 15,
        step_hundredths: 2,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x1005,
        model_name: "ATK68 Air",
        rows: 5,
        cols: 15,
        step_hundredths: 2,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x2029,
        model_name: "ATK68 V2",
        rows: 5,
        cols: 15,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x1259,
        model_name: "ATK68 V4",
        rows: 5,
        cols: 15,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x21B8,
        model_name: "ATK68 V4",
        rows: 5,
        cols: 15,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x21C6,
        model_name: "ATK68 RX",
        rows: 5,
        cols: 15,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x2169,
        model_name: "ATK EDGE75",
        rows: 6,
        cols: 16,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x21BC,
        model_name: "ATK EDGE 63HE Ultimate",
        rows: 5,
        cols: 15,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x2188,
        model_name: "ATK75 V2",
        rows: 6,
        cols: 16,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x21C9,
        model_name: "ATK RS7 Turbo",
        rows: 6,
        cols: 16,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
    Ff01BoardSpec {
        vid: 0x373B,
        pid: 0x1177,
        model_name: "ATK x QK Hex80",
        rows: 6,
        cols: 16,
        step_hundredths: 10,
        max_travel_hundredths: 400,
    },
];

/// Finds a verified `0xFF01` board specification by `(vid, pid)`.
pub fn find_ff01_board(vid: u16, pid: u16) -> Option<&'static Ff01BoardSpec> {
    FF01_BOARDS.iter().find(|b| b.vid == vid && b.pid == pid)
}

/// Builds declarative `DeviceMatchRule`s for all `0xFF01` ATK68/ATK75 keyboards.
pub fn ff01_match_rules() -> Vec<DeviceMatchRule> {
    FF01_BOARDS
        .iter()
        .map(|b| {
            DeviceMatchRule::new(b.vid, ConnectionKind::WiredUsb, b.model_name)
                .with_pid(b.pid)
                .with_usage_page(ATK_FF01_USAGE_PAGE)
        })
        .collect()
}

/// Safety validator blocking `ResetStorage (0x22)` and bootloader jump (`0xAA`).
pub fn verify_ff01_safety(_kind: ReportKind, report: &[u8]) -> Result<(), String> {
    if report.len() != ATK_FF01_TX_LEN {
        return Err(format!(
            "expected {ATK_FF01_TX_LEN}-byte Report 0x00 frame, got {} bytes",
            report.len()
        ));
    }
    if report[0] != ATK_FF01_REPORT_ID {
        return Err(format!("disallowed Report ID 0x{:02X}", report[0]));
    }
    if matches!(report[1], op::RESET_STORAGE | op::BOOTLOADER_JUMP) {
        return Err(format!(
            "blocked dangerous ATK 0xFF01 command 0x{:02X}",
            report[1]
        ));
    }
    Ok(())
}

/// Builds the strict `FixedReportValidator` for `0xFF01` GTech keyboard traffic.
pub fn atk_ff01_validator() -> FixedReportValidator {
    FixedReportValidator::new(vec![ATK_FF01_TX_LEN], vec![ATK_FF01_REPORT_ID])
        .with_checksum(verify_ff01_safety)
}

/// Encodes a 65-byte TX output report `[0x00, cmd, body...]`.
pub fn encode_ff01_tx(cmd: u8, body: &[u8]) -> CoreResult<[u8; ATK_FF01_TX_LEN]> {
    if body.len() + 2 > ATK_FF01_PAYLOAD_LEN {
        return Err(CoreError::OutOfRange {
            field: "ff01_body_len",
            actual: body.len().to_string(),
            expected: format!("0..={}", ATK_FF01_PAYLOAD_LEN - 2),
        });
    }
    let mut tx = [0u8; ATK_FF01_TX_LEN];
    tx[0] = ATK_FF01_REPORT_ID;
    tx[1] = cmd;
    tx[2..2 + body.len()].copy_from_slice(body);
    Ok(tx)
}

/// Normalizes an incoming 64- or 65-byte input report and extracts validated reply data.
pub fn unwrap_ff01_rx(raw: &[u8], expected_cmd: u8) -> CoreResult<Option<Vec<u8>>> {
    let payload = if raw.len() >= ATK_FF01_TX_LEN && raw[0] == ATK_FF01_REPORT_ID {
        &raw[1..ATK_FF01_TX_LEN]
    } else if raw.len() >= 3 {
        raw
    } else {
        return Ok(None);
    };
    if payload[0] == op::DATA_REPORTED || payload[0] != expected_cmd {
        return Ok(None);
    }
    if payload[2] != 0 {
        return Err(CoreError::Protocol(format!(
            "ATK 0xFF01 command 0x{expected_cmd:02X} returned error 0x{:02X}",
            payload[2]
        )));
    }
    let declared = usize::from(payload[1]);
    if declared > ATK_FF01_PAYLOAD_LEN - 3 || payload.len() < declared + 3 {
        return Err(CoreError::Protocol(format!(
            "truncated ATK 0xFF01 reply for 0x{expected_cmd:02X}"
        )));
    }
    Ok(Some(payload[3..3 + declared].to_vec()))
}

const FF01_REPORT_RATES: [PollingRateHz; 7] = [
    PollingRateHz::Hz125,
    PollingRateHz::Hz250,
    PollingRateHz::Hz500,
    PollingRateHz::Hz1000,
    PollingRateHz::Hz2000,
    PollingRateHz::Hz4000,
    PollingRateHz::Hz8000,
];

fn rate_from_index(ix: u8) -> Option<PollingRateHz> {
    FF01_REPORT_RATES.get(usize::from(ix)).copied()
}

fn index_from_rate(rate: PollingRateHz) -> Option<u8> {
    FF01_REPORT_RATES
        .iter()
        .position(|&r| r == rate)
        .and_then(|ix| u8::try_from(ix).ok())
}

fn ff01_light_presets() -> Vec<RgbPresetInfo> {
    [
        (0, "Spectrum Cycle", false, true, false),
        (1, "Static", true, false, false),
        (2, "Wave Left-to-Right", false, true, false),
        (3, "Breathing", true, true, true),
        (4, "Snake", true, true, true),
        (5, "Reactive", true, true, true),
        (6, "Rotate", false, true, false),
        (7, "Wave to Centre", false, true, false),
        (8, "Fountain", true, true, true),
        (9, "Laser", true, true, true),
        (10, "Custom", true, false, false),
    ]
    .into_iter()
    .map(|(id, name, color, speed, colorful)| RgbPresetInfo::new(id, name, color, speed, colorful))
    .collect()
}

/// Active ATK68 / ATK75 (`0xFF01`) Hall Effect keyboard backed by [`GuardedTransport`].
pub struct AtkFf01Keyboard<T: HidTransport> {
    transport: GuardedTransport<T>,
    info: DeviceInfo,
    spec: Ff01BoardSpec,
    capabilities: Capabilities,
    he_caps: HallEffectCapabilities,
}

impl<T: HidTransport> fmt::Debug for AtkFf01Keyboard<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtkFf01Keyboard")
            .field("info", &self.info)
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> AtkFf01Keyboard<T> {
    /// Connects to an ATK68 / ATK75 `0xFF01` vendor interface and runs read-only info discovery.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        let spec = find_ff01_board(desc.vid, desc.pid)
            .copied()
            .ok_or_else(|| {
                CoreError::Validation(format!(
                    "unsupported ATK 0xFF01 board 0x{:04X}:0x{:04X}",
                    desc.vid, desc.pid
                ))
            })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(atk_ff01_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "atk-ff01:{:04X}:{:04X}:{}",
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
            ConnectionKind::WiredUsb,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let step = TravelHundredthsMm::from_hundredths(spec.step_hundredths);
        let max_travel = TravelHundredthsMm::from_hundredths(spec.max_travel_hundredths);
        let he_caps = HallEffectCapabilities::new(spec.key_count(), step, max_travel, step)
            .with_rapid_trigger_range(step, max_travel, false);

        let capabilities = Capabilities::empty()
            .with_hall_effect(true)
            .with_rgb(true)
            .with_profiles(true)
            .with_keyboard_config(true);

        let mut dev = Self {
            transport: guarded,
            info,
            spec,
            capabilities,
            he_caps,
        };
        dev.discover_readonly()?;
        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn exchange(&mut self, cmd: u8, body: &[u8], mutating: bool) -> CoreResult<Vec<u8>> {
        let tx = encode_ff01_tx(cmd, body)?;
        if !mutating || !self.transport.is_dry_run() {
            let _ = self.transport.drain_input_reports();
        }
        if mutating {
            self.transport.send_output_report(&tx)?;
            if self.transport.is_dry_run() {
                return Ok(Vec::new());
            }
        } else {
            self.transport.send_query_output_report(&tx)?;
        }

        let mut rx = [0u8; ATK_FF01_TX_LEN];
        for _ in 0..MAX_READ_ATTEMPTS {
            let n = match self.transport.read_input_report(&mut rx, READ_TIMEOUT) {
                Ok(len) => len,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for ATK 0xFF01 reply to 0x{cmd:02X}"
                    )));
                }
                Err(err) => return Err(err.into()),
            };
            if let Some(data) = unwrap_ff01_rx(&rx[..n], cmd)? {
                return Ok(data);
            }
        }
        Err(CoreError::Protocol(format!(
            "no matching ATK 0xFF01 reply for 0x{cmd:02X}"
        )))
    }

    fn discover_readonly(&mut self) -> CoreResult<()> {
        let info = self.exchange(op::GET_KEYBOARD_INFO, &[0, 0], false)?;
        if info.len() >= 8 {
            self.info.firmware_version = Some(format!("v{}.{:02x}", info[6], info[7]));
        }
        Ok(())
    }

    fn raw_to_travel(&self, raw: u8) -> TravelHundredthsMm {
        TravelHundredthsMm::from_hundredths(
            u16::from(raw).saturating_mul(self.spec.step_hundredths),
        )
    }

    fn travel_to_raw(&self, travel: TravelHundredthsMm) -> u8 {
        let step = self.spec.step_hundredths.max(1);
        let max_raw = (self.spec.max_travel_hundredths / step).min(255);
        let raw = (travel.as_hundredths() / step).clamp(1, max_raw);
        u8::try_from(raw).unwrap_or(1)
    }

    /// Commits current RAM settings to onboard flash (`0x21`).
    pub fn save_storage(&mut self) -> CoreResult<()> {
        self.exchange(op::SAVE_STORAGE, &[0, 0], true)?;
        Ok(())
    }
}

impl<T: HidTransport> Peripheral for AtkFf01Keyboard<T> {
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

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        Some(self)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        Some(self)
    }

    fn as_keyboard_config(&self) -> Option<&dyn KeyboardConfigController> {
        Some(self)
    }

    fn as_keyboard_config_mut(&mut self) -> Option<&mut dyn KeyboardConfigController> {
        Some(self)
    }
}

impl<T: HidTransport> HallEffectKeyboard for AtkFf01Keyboard<T> {
    fn hall_effect_capabilities(&self) -> HallEffectCapabilities {
        self.he_caps.clone()
    }

    fn read_hall_effect_state(&mut self) -> CoreResult<HallEffectSnapshot> {
        let data = self.exchange(op::GET_ALL_ACTUATION, &[0, 0], false)?;
        if data.len() < 3 {
            return Err(CoreError::Protocol(format!(
                "short ATK 0xFF01 actuation reply ({} bytes)",
                data.len()
            )));
        }
        let point = self.raw_to_travel(data[0]);
        let rt_press = self.raw_to_travel(data[1]);
        let rt_release = self.raw_to_travel(data[2]);
        let rt_enabled = data[1] > 0 || data[2] > 0;
        let rt = RapidTriggerConfig::new(rt_enabled, false, rt_press, rt_release);

        let keys = (0..self.spec.key_count())
            .map(|ix| KeyHallEffectConfig::new(KeyIndex::new(ix), point, rt))
            .collect();
        Ok(HallEffectSnapshot::new(
            keys,
            Vec::new(),
            Vec::new(),
            CalibrationStatus::Unknown,
        ))
    }

    fn read_calibration_status(&mut self) -> CoreResult<CalibrationStatus> {
        Ok(CalibrationStatus::Unknown)
    }

    fn set_key_actuation(&mut self, key: KeyIndex, point: TravelHundredthsMm) -> CoreResult<()> {
        self.he_caps.validate_key(key)?;
        self.set_global_actuation(point)
    }

    fn set_global_actuation(&mut self, point: TravelHundredthsMm) -> CoreResult<()> {
        self.he_caps.validate_actuation(point)?;
        let (down, up) = if self.transport.is_dry_run() {
            (0, 0)
        } else {
            let cur = self.exchange(op::GET_ALL_ACTUATION, &[0, 0], false)?;
            (
                cur.get(1).copied().unwrap_or(0),
                cur.get(2).copied().unwrap_or(0),
            )
        };
        let raw_point = self.travel_to_raw(point);
        self.exchange(op::SET_ALL_ACTUATION, &[3, raw_point, down, up], true)?;
        Ok(())
    }

    fn set_rapid_trigger(&mut self, key: KeyIndex, config: RapidTriggerConfig) -> CoreResult<()> {
        self.he_caps.validate_key(key)?;
        self.he_caps.validate_rapid_trigger(&config)?;
        let (row, col) = self.spec.key_to_row_col(key)?;
        let raw_val = if config.is_enabled() {
            self.travel_to_raw(config.press_sensitivity)
        } else {
            0
        };
        self.exchange(op::SET_ONE_FAST_TRIGGER, &[3, row, col, raw_val], true)?;
        Ok(())
    }
}

impl<T: HidTransport> RgbController for AtkFf01Keyboard<T> {
    fn rgb_capabilities(&self) -> RgbCapabilities {
        RgbCapabilities::new(
            vec![
                RgbZoneInfo::new(RgbZoneId::new(0), "Backlight")
                    .with_presets(ff01_light_presets())
                    .with_max_speed(FF01_SPEED_MAX),
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
            false,
        )
    }

    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot> {
        let d = self.exchange(op::GET_LIGHT, &[0, 0], false)?;
        if d.len() < 8 {
            return Err(CoreError::Protocol(format!(
                "short ATK 0xFF01 lighting reply ({} bytes)",
                d.len()
            )));
        }
        let enabled = d[0] != 0;
        let mode = d[1];
        let bright_pct = u8::try_from(
            (u16::from(d[2].min(FF01_BRIGHTNESS_MAX)) * 100 + u16::from(FF01_BRIGHTNESS_MAX) / 2)
                / u16::from(FF01_BRIGHTNESS_MAX),
        )
        .unwrap_or(100);
        let speed = d[3].min(FF01_SPEED_MAX);
        let color = RgbColor::new(d[4], d[5], d[6]);
        let colorful = d[7] != 0;
        let effect = if enabled {
            RgbEffect::Preset {
                id: mode,
                color,
                speed,
                colorful,
            }
        } else {
            RgbEffect::Off
        };
        Ok(RgbSnapshot::new(vec![RgbZoneState::new(
            RgbZoneId::new(0),
            effect,
            Brightness::new(bright_pct)?,
        )]))
    }

    fn set_zone_effect(
        &mut self,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()> {
        if zone.value() != 0 {
            return Err(CoreError::OutOfRange {
                field: "rgb_zone",
                actual: zone.value().to_string(),
                expected: "0 (Backlight)".to_owned(),
            });
        }
        let raw_bright = u8::try_from(
            (u16::from(brightness.percent()) * u16::from(FF01_BRIGHTNESS_MAX) + 50) / 100,
        )
        .unwrap_or(FF01_BRIGHTNESS_MAX)
        .min(FF01_BRIGHTNESS_MAX);
        let (enabled, mode, speed, color, colorful) = match effect {
            RgbEffect::Off => (0u8, 1u8, 2u8, RgbColor::BLACK, 0u8),
            RgbEffect::Static { color } => (1, 1, 0, color, 0),
            RgbEffect::Breathing { color, speed } => (1, 3, speed.min(FF01_SPEED_MAX), color, 0),
            RgbEffect::SpectrumCycle { speed } => {
                (1, 0, speed.min(FF01_SPEED_MAX), RgbColor::WHITE, 1)
            }
            RgbEffect::Wave {
                speed,
                direction:
                    RgbDirection::LeftToRight
                    | RgbDirection::RightToLeft
                    | RgbDirection::TopToBottom
                    | RgbDirection::BottomToTop,
            } => (1, 2, speed.min(FF01_SPEED_MAX), RgbColor::WHITE, 1),
            RgbEffect::Reactive { color, speed } => (1, 5, speed.min(FF01_SPEED_MAX), color, 0),
            RgbEffect::Preset {
                id,
                color,
                speed,
                colorful,
            } => (
                1,
                id.min(10),
                speed.min(FF01_SPEED_MAX),
                color,
                u8::from(colorful),
            ),
        };
        self.exchange(
            op::SET_LIGHT,
            &[
                8, enabled, mode, raw_bright, speed, color.r, color.g, color.b, 0, colorful,
            ],
            true,
        )?;
        Ok(())
    }
}

impl<T: HidTransport> ProfileManager for AtkFf01Keyboard<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        ProfileCapabilities::new(FF01_PROFILE_COUNT, false, false)
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let d = self.exchange(op::GET_DEVICE_PROFILE, &[0, 0], false)?;
        let active = d.first().copied().unwrap_or(0).min(FF01_PROFILE_COUNT - 1);
        let profiles = (0..FF01_PROFILE_COUNT)
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
        self.exchange(op::SET_DEVICE_PROFILE, &[1, slot.index()], true)?;
        Ok(())
    }
}

impl<T: HidTransport> KeyboardConfigController for AtkFf01Keyboard<T> {
    fn keyboard_config_capabilities(&self) -> KeyboardConfigCapabilities {
        KeyboardConfigCapabilities::new(
            false,
            false,
            false,
            false,
            false,
            false,
            FF01_REPORT_RATES.to_vec(),
        )
    }

    fn read_keyboard_config(&mut self) -> CoreResult<KeyboardConfigSnapshot> {
        let d = self.exchange(op::GET_REPORT_RATE, &[0, 0], false)?;
        let rate = d
            .first()
            .copied()
            .and_then(rate_from_index)
            .unwrap_or(PollingRateHz::Hz1000);
        Ok(KeyboardConfigSnapshot::new(
            false, false, false, false, false, false, rate,
        ))
    }

    fn set_keyboard_config(&mut self, config: &KeyboardConfigSnapshot) -> CoreResult<()> {
        let ix = index_from_rate(config.polling_rate).ok_or_else(|| CoreError::OutOfRange {
            field: "polling_rate",
            actual: config.polling_rate.to_string(),
            expected: "125..=8000 Hz".to_owned(),
        })?;
        self.exchange(op::SET_REPORT_RATE, &[1, ix], true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_transport::{HidDeviceDescriptor, MockHidTransport, PacketValidator};

    fn make_reply(cmd: u8, result_code: u8, data: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; ATK_FF01_PAYLOAD_LEN];
        frame[0] = cmd;
        frame[1] = u8::try_from(data.len()).unwrap_or(0);
        frame[2] = result_code;
        frame[3..3 + data.len()].copy_from_slice(data);
        frame
    }

    #[test]
    fn validator_blocks_reset_storage_and_bootloader_jump() {
        let v = atk_ff01_validator();
        let ok = encode_ff01_tx(op::GET_ALL_ACTUATION, &[0, 0]).unwrap();
        assert!(v.validate_outgoing(ReportKind::Output, &ok).is_ok());

        let reset = encode_ff01_tx(op::RESET_STORAGE, &[0, 0]).unwrap();
        assert!(v.validate_outgoing(ReportKind::Output, &reset).is_err());

        let boot = encode_ff01_tx(op::BOOTLOADER_JUMP, &[0, 0]).unwrap();
        assert!(v.validate_outgoing(ReportKind::Output, &boot).is_err());
    }

    #[test]
    fn reads_and_writes_atk68_info_actuation_rgb_profile_and_polling_rate() {
        let desc = HidDeviceDescriptor::new(
            0x1A81,
            0x207E,
            ATK_FF01_USAGE_PAGE,
            0x0001,
            2,
            "mock://atk/atk68",
        );
        let mut mock = MockHidTransport::new(desc);
        // Unsolicited async 0x48 report should be skipped cleanly before GET_KEYBOARD_INFO reply.
        mock.push_input_response(make_reply(op::DATA_REPORTED, 0, &[1, 2, 3]));
        mock.push_input_response(make_reply(
            op::GET_KEYBOARD_INFO,
            0,
            &[0x1A, 0x81, 0x20, 0x7E, 0, 0, 1, 0x25],
        ));
        // GET_ALL_ACTUATION: point=10 (1.00 mm), down=2 (0.20 mm), up=2 (0.20 mm)
        mock.push_input_response(make_reply(op::GET_ALL_ACTUATION, 0, &[10, 2, 2]));
        // GET_LIGHT: enabled=1, mode=1 (Static), brightness=6 (75%), speed=2, RGB(255, 64, 0), colorful=0
        mock.push_input_response(make_reply(op::GET_LIGHT, 0, &[1, 1, 6, 2, 255, 64, 0, 0]));
        // GET_DEVICE_PROFILE: slot 1
        mock.push_input_response(make_reply(op::GET_DEVICE_PROFILE, 0, &[1]));
        // GET_REPORT_RATE: index 3 = 1000 Hz
        mock.push_input_response(make_reply(op::GET_REPORT_RATE, 0, &[3]));
        // SET_ALL_ACTUATION read-before-write + ack
        mock.push_input_response(make_reply(op::GET_ALL_ACTUATION, 0, &[10, 2, 2]));
        mock.push_input_response(make_reply(op::SET_ALL_ACTUATION, 0, &[]));
        // SET_ONE_FAST_TRIGGER ack
        mock.push_input_response(make_reply(op::SET_ONE_FAST_TRIGGER, 0, &[]));

        let mut kb = AtkFf01Keyboard::open(mock, false, None).unwrap();
        assert_eq!(kb.info().model_name, "ATK68");
        assert_eq!(kb.info().firmware_version.as_deref(), Some("v1.25"));

        let he = kb.read_hall_effect_state().unwrap();
        assert_eq!(he.keys.len(), 75);
        assert_eq!(
            he.keys[0].actuation,
            TravelHundredthsMm::from_hundredths(100)
        );
        assert!(he.keys[0].rapid_trigger.is_enabled());
        assert_eq!(
            he.keys[0].rapid_trigger.press_sensitivity,
            TravelHundredthsMm::from_hundredths(20)
        );

        let rgb = kb.read_rgb_state().unwrap();
        assert_eq!(rgb.zones[0].brightness.percent(), 75);
        assert_eq!(kb.read_profile_state().unwrap().active_slot.index(), 1);
        assert_eq!(
            kb.read_keyboard_config().unwrap().polling_rate,
            PollingRateHz::Hz1000
        );

        kb.set_global_actuation(TravelHundredthsMm::from_hundredths(40))
            .unwrap();
        kb.set_rapid_trigger(
            KeyIndex::new(16),
            RapidTriggerConfig::new(
                true,
                false,
                TravelHundredthsMm::from_hundredths(20),
                TravelHundredthsMm::from_hundredths(20),
            ),
        )
        .unwrap();

        let writes = kb.transport().inner().recorded_writes();
        let last_rt = &writes[writes.len() - 1].bytes;
        assert_eq!(
            &last_rt[0..6],
            &[0x00, op::SET_ONE_FAST_TRIGGER, 3, 1, 1, 2]
        );
    }
}
