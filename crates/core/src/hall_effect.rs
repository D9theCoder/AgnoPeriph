//! Brand-agnostic Hall Effect keyboard capability trait and fixed-point travel models.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// Key travel distance in hundredths of a millimeter (fixed-point).
///
/// Examples: `10` = `0.10 mm`, `15` = `0.15 mm`, `400` = `4.00 mm`.
/// Fixed-point representation avoids floating-point drift when encoding HID reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TravelHundredthsMm(u16);

impl TravelHundredthsMm {
    /// Creates a travel distance from hundredths of a millimeter.
    pub const fn from_hundredths(hundredths: u16) -> Self {
        Self(hundredths)
    }

    /// Returns the raw value in hundredths of a millimeter.
    pub const fn as_hundredths(self) -> u16 {
        self.0
    }
}

impl fmt::Display for TravelHundredthsMm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02} mm", self.0 / 100, self.0 % 100)
    }
}

/// Stable logical/matrix key identifier independent of physical keycap labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KeyIndex(u16);

impl KeyIndex {
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    pub const fn value(self) -> u16 {
        self.0
    }
}

impl fmt::Display for KeyIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({})", self.0)
    }
}

/// Per-key Rapid Trigger configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RapidTriggerConfig {
    pub enabled: bool,
    pub continuous: bool,
    pub press_sensitivity: TravelHundredthsMm,
    pub release_sensitivity: TravelHundredthsMm,
}

impl RapidTriggerConfig {
    /// Creates a Rapid Trigger configuration.
    pub const fn new(
        enabled: bool,
        continuous: bool,
        press_sensitivity: TravelHundredthsMm,
        release_sensitivity: TravelHundredthsMm,
    ) -> Self {
        Self {
            enabled,
            continuous,
            press_sensitivity,
            release_sensitivity,
        }
    }

    /// Creates a disabled Rapid Trigger configuration.
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            continuous: false,
            press_sensitivity: TravelHundredthsMm::from_hundredths(0),
            release_sensitivity: TravelHundredthsMm::from_hundredths(0),
        }
    }

    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub const fn is_continuous(&self) -> bool {
        self.continuous
    }
}

/// Direction in which a Dynamic Keystroke (DKS) stage triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DksTriggerDirection {
    Press,
    Release,
}

/// Single trigger stage inside a Dynamic Keystroke (DKS) binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DksStageAction {
    pub direction: DksTriggerDirection,
    pub trigger_travel: TravelHundredthsMm,
    /// HID usage/keycode emitted by this stage.
    pub hid_keycode: u16,
    /// Whether the key remains held across subsequent travel until release.
    pub hold: bool,
}

impl DksStageAction {
    pub const fn new(
        direction: DksTriggerDirection,
        trigger_travel: TravelHundredthsMm,
        hid_keycode: u16,
        hold: bool,
    ) -> Self {
        Self {
            direction,
            trigger_travel,
            hid_keycode,
            hold,
        }
    }
}

/// 4-stage Dynamic Keystroke (DKS) binding attached to one key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DksBinding {
    pub key: KeyIndex,
    pub stages: [Option<DksStageAction>; 4],
}

impl DksBinding {
    pub const fn new(key: KeyIndex, stages: [Option<DksStageAction>; 4]) -> Self {
        Self { key, stages }
    }
}

/// Simultaneous Opposing Cardinal Directions (SOCD / Snap Tap) resolution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SocdMode {
    LastInputPriority,
    Neutral,
    KeyAPriority,
    KeyBPriority,
}

/// Paired opposing keys for hardware SOCD resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SocdPair {
    pub key_a: KeyIndex,
    pub key_b: KeyIndex,
    pub mode: SocdMode,
}

impl SocdPair {
    /// Creates a validated SOCD pair (keys must be distinct).
    pub fn new(key_a: KeyIndex, key_b: KeyIndex, mode: SocdMode) -> CoreResult<Self> {
        if key_a == key_b {
            return Err(CoreError::Validation(format!(
                "SOCD pair requires two distinct keys, got {key_a}"
            )));
        }
        Ok(Self { key_a, key_b, mode })
    }
}

/// Magnetic sensor calibration state reported by the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalibrationStatus {
    Unknown,
    Calibrated,
    NeedsCalibration,
    InProgress { progress_pct: u8 },
}

/// Complete Hall Effect tuning for a single key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeyHallEffectConfig {
    pub key: KeyIndex,
    pub actuation: TravelHundredthsMm,
    pub rapid_trigger: RapidTriggerConfig,
    pub top_deadzone: Option<TravelHundredthsMm>,
    pub bottom_deadzone: Option<TravelHundredthsMm>,
}

impl KeyHallEffectConfig {
    pub const fn new(
        key: KeyIndex,
        actuation: TravelHundredthsMm,
        rapid_trigger: RapidTriggerConfig,
    ) -> Self {
        Self {
            key,
            actuation,
            rapid_trigger,
            top_deadzone: None,
            bottom_deadzone: None,
        }
    }

    pub const fn with_deadzones(
        mut self,
        top: Option<TravelHundredthsMm>,
        bottom: Option<TravelHundredthsMm>,
    ) -> Self {
        self.top_deadzone = top;
        self.bottom_deadzone = bottom;
        self
    }
}

/// Hardware limits and supported Hall Effect features for a magnetic keyboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HallEffectCapabilities {
    pub key_count: u16,
    pub actuation_min: TravelHundredthsMm,
    pub actuation_max: TravelHundredthsMm,
    pub actuation_step: TravelHundredthsMm,
    pub rapid_trigger_min: Option<TravelHundredthsMm>,
    pub rapid_trigger_max: Option<TravelHundredthsMm>,
    pub supports_continuous_rt: bool,
    pub max_dks_bindings: u8,
    pub max_socd_pairs: u8,
    pub supports_calibration_status: bool,
}

impl HallEffectCapabilities {
    /// Creates a new `HallEffectCapabilities` descriptor.
    pub const fn new(
        key_count: u16,
        actuation_min: TravelHundredthsMm,
        actuation_max: TravelHundredthsMm,
        actuation_step: TravelHundredthsMm,
    ) -> Self {
        Self {
            key_count,
            actuation_min,
            actuation_max,
            actuation_step,
            rapid_trigger_min: None,
            rapid_trigger_max: None,
            supports_continuous_rt: false,
            max_dks_bindings: 0,
            max_socd_pairs: 0,
            supports_calibration_status: false,
        }
    }

    pub const fn with_rapid_trigger_range(
        mut self,
        min: TravelHundredthsMm,
        max: TravelHundredthsMm,
        supports_continuous: bool,
    ) -> Self {
        self.rapid_trigger_min = Some(min);
        self.rapid_trigger_max = Some(max);
        self.supports_continuous_rt = supports_continuous;
        self
    }

    pub const fn with_max_dks_bindings(mut self, max: u8) -> Self {
        self.max_dks_bindings = max;
        self
    }

    pub const fn with_max_socd_pairs(mut self, max: u8) -> Self {
        self.max_socd_pairs = max;
        self
    }

    pub const fn with_calibration_status(mut self, supported: bool) -> Self {
        self.supports_calibration_status = supported;
        self
    }

    pub const fn has_rapid_trigger(&self) -> bool {
        self.rapid_trigger_min.is_some() && self.rapid_trigger_max.is_some()
    }

    pub const fn has_dks(&self) -> bool {
        self.max_dks_bindings > 0
    }

    pub const fn has_socd(&self) -> bool {
        self.max_socd_pairs > 0
    }

    /// Validates a key index against `key_count`.
    pub fn validate_key(&self, key: KeyIndex) -> CoreResult<()> {
        if key.value() >= self.key_count {
            return Err(CoreError::OutOfRange {
                field: "key_index",
                actual: key.value().to_string(),
                expected: format!("0..{}", self.key_count),
            });
        }
        Ok(())
    }

    /// Validates an actuation point against min, max, and step.
    pub fn validate_actuation(&self, point: TravelHundredthsMm) -> CoreResult<()> {
        let min = self.actuation_min.as_hundredths();
        let max = self.actuation_max.as_hundredths();
        let step = self.actuation_step.as_hundredths().max(1);
        let value = point.as_hundredths();

        if value < min || value > max {
            return Err(CoreError::OutOfRange {
                field: "actuation_point",
                actual: point.to_string(),
                expected: format!("{}..={}", self.actuation_min, self.actuation_max),
            });
        }
        if step > 1 && !(value - min).is_multiple_of(step) {
            return Err(CoreError::Validation(format!(
                "actuation {point} does not align with step {} from min {}",
                self.actuation_step, self.actuation_min
            )));
        }
        Ok(())
    }

    /// Validates a Rapid Trigger configuration against hardware limits.
    pub fn validate_rapid_trigger(&self, config: &RapidTriggerConfig) -> CoreResult<()> {
        if !config.is_enabled() {
            return Ok(());
        }
        let (Some(min), Some(max)) = (self.rapid_trigger_min, self.rapid_trigger_max) else {
            return Err(CoreError::UnsupportedCapability("rapid_trigger"));
        };
        if config.is_continuous() && !self.supports_continuous_rt {
            return Err(CoreError::UnsupportedCapability("continuous_rapid_trigger"));
        }
        for (field, value) in [
            ("rapid_trigger.press_sensitivity", config.press_sensitivity),
            (
                "rapid_trigger.release_sensitivity",
                config.release_sensitivity,
            ),
        ] {
            if value < min || value > max {
                return Err(CoreError::OutOfRange {
                    field,
                    actual: value.to_string(),
                    expected: format!("{min}..={max}"),
                });
            }
        }
        Ok(())
    }

    /// Validates a slice of SOCD pairs against key bounds and maximum slot count.
    pub fn validate_socd_pairs(&self, pairs: &[SocdPair]) -> CoreResult<()> {
        if !self.has_socd() && !pairs.is_empty() {
            return Err(CoreError::UnsupportedCapability("socd"));
        }
        if pairs.len() > usize::from(self.max_socd_pairs) {
            return Err(CoreError::OutOfRange {
                field: "socd_pairs",
                actual: pairs.len().to_string(),
                expected: format!("0..={}", self.max_socd_pairs),
            });
        }
        for pair in pairs {
            self.validate_key(pair.key_a)?;
            self.validate_key(pair.key_b)?;
            if pair.key_a == pair.key_b {
                return Err(CoreError::Validation(format!(
                    "SOCD pair cannot reference the same key {}",
                    pair.key_a
                )));
            }
        }
        Ok(())
    }
}

/// Immutable read-only snapshot of a Hall Effect keyboard's active tuning state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HallEffectSnapshot {
    pub keys: Vec<KeyHallEffectConfig>,
    pub dks_bindings: Vec<DksBinding>,
    pub socd_pairs: Vec<SocdPair>,
    pub calibration: CalibrationStatus,
}

impl HallEffectSnapshot {
    pub fn new(
        keys: Vec<KeyHallEffectConfig>,
        dks_bindings: Vec<DksBinding>,
        socd_pairs: Vec<SocdPair>,
        calibration: CalibrationStatus,
    ) -> Self {
        Self {
            keys,
            dks_bindings,
            socd_pairs,
            calibration,
        }
    }
}

/// Capability trait for magnetic / Hall Effect keyboards (e.g., ATK RS7).
pub trait HallEffectKeyboard: Send + Sync {
    /// Returns supported travel bounds, Rapid Trigger, DKS, SOCD, and calibration limits.
    fn hall_effect_capabilities(&self) -> HallEffectCapabilities;

    /// Performs a read-only query of the keyboard's Hall Effect configuration.
    fn read_hall_effect_state(&mut self) -> CoreResult<HallEffectSnapshot>;

    /// Reads the current magnetic sensor calibration state.
    fn read_calibration_status(&mut self) -> CoreResult<CalibrationStatus>;

    /// Sets actuation travel distance for a single key.
    fn set_key_actuation(&mut self, key: KeyIndex, point: TravelHundredthsMm) -> CoreResult<()>;

    /// Sets uniform actuation travel distance across all keys.
    fn set_global_actuation(&mut self, point: TravelHundredthsMm) -> CoreResult<()>;

    /// Configures per-key Rapid Trigger.
    fn set_rapid_trigger(&mut self, key: KeyIndex, config: RapidTriggerConfig) -> CoreResult<()>;

    /// Configures a 4-stage Dynamic Keystroke (DKS) binding for a key.
    fn set_dks_binding(&mut self, binding: DksBinding) -> CoreResult<()> {
        let _ = binding;
        Err(CoreError::UnsupportedCapability("dks"))
    }

    /// Removes any Dynamic Keystroke (DKS) binding from a key.
    fn clear_dks_binding(&mut self, key: KeyIndex) -> CoreResult<()> {
        let _ = key;
        Err(CoreError::UnsupportedCapability("dks"))
    }

    /// Writes the active hardware SOCD (Snap Tap) key pairs.
    fn set_socd_pairs(&mut self, pairs: &[SocdPair]) -> CoreResult<()> {
        let _ = pairs;
        Err(CoreError::UnsupportedCapability("socd"))
    }
}
