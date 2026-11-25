//! Brand-agnostic pointer / mouse capability trait and validated models.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::rgb::RgbColor;

/// Validated mouse sensitivity value in dots-per-inch (DPI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DpiValue(u32);

impl DpiValue {
    /// Creates a new non-zero `DpiValue`.
    ///
    /// Device-specific min/max/step checks are enforced via [`PointerCapabilities::validate_dpi`].
    pub fn new(dpi: u32) -> CoreResult<Self> {
        if dpi == 0 {
            return Err(CoreError::OutOfRange {
                field: "dpi",
                actual: "0".to_owned(),
                expected: ">= 1".to_owned(),
            });
        }
        Ok(Self(dpi))
    }

    /// Returns the raw DPI value.
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for DpiValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} DPI", self.0)
    }
}

/// One onboard DPI preset stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DpiStage {
    pub index: u8,
    pub x: DpiValue,
    /// Optional independent Y-axis DPI; `None` means Y tracks X.
    pub y: Option<DpiValue>,
    /// Optional stage indicator LED color.
    pub indicator_color: Option<RgbColor>,
}

impl DpiStage {
    /// Creates a linked X/Y DPI stage.
    pub const fn new(index: u8, dpi: DpiValue) -> Self {
        Self {
            index,
            x: dpi,
            y: None,
            indicator_color: None,
        }
    }

    pub const fn with_y(mut self, y: DpiValue) -> Self {
        self.y = Some(y);
        self
    }

    pub const fn with_indicator_color(mut self, color: RgbColor) -> Self {
        self.indicator_color = Some(color);
        self
    }

    /// Returns the effective Y-axis DPI (falling back to X when linked).
    pub const fn effective_y(&self) -> DpiValue {
        match self.y {
            Some(y) => y,
            None => self.x,
        }
    }
}

/// Supported USB/wireless report polling rates in Hertz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PollingRateHz {
    Hz125,
    Hz250,
    Hz500,
    Hz1000,
    Hz2000,
    Hz4000,
    Hz8000,
}

impl PollingRateHz {
    /// Converts from a numeric Hz rate.
    pub fn from_hz(hz: u16) -> CoreResult<Self> {
        match hz {
            125 => Ok(Self::Hz125),
            250 => Ok(Self::Hz250),
            500 => Ok(Self::Hz500),
            1000 => Ok(Self::Hz1000),
            2000 => Ok(Self::Hz2000),
            4000 => Ok(Self::Hz4000),
            8000 => Ok(Self::Hz8000),
            other => Err(CoreError::OutOfRange {
                field: "polling_rate_hz",
                actual: other.to_string(),
                expected: "one of 125, 250, 500, 1000, 2000, 4000, 8000".to_owned(),
            }),
        }
    }

    /// Returns the numeric rate in Hertz.
    pub const fn as_hz(self) -> u16 {
        match self {
            Self::Hz125 => 125,
            Self::Hz250 => 250,
            Self::Hz500 => 500,
            Self::Hz1000 => 1000,
            Self::Hz2000 => 2000,
            Self::Hz4000 => 4000,
            Self::Hz8000 => 8000,
        }
    }
}

impl fmt::Display for PollingRateHz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.as_hz())
    }
}

/// Sensor lift-off distance (LOD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LiftOffDistance {
    Low,
    Medium,
    High,
    /// Lift-off height in tenths of a millimeter (e.g., `10` = `1.0 mm`, `20` = `2.0 mm`).
    TenthsMm(u8),
}

impl fmt::Display for LiftOffDistance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => f.write_str("Low"),
            Self::Medium => f.write_str("Medium"),
            Self::High => f.write_str("High"),
            Self::TenthsMm(tenths) => write!(f, "{}.{} mm", tenths / 10, tenths % 10),
        }
    }
}

/// Switch click debounce time in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DebounceMs(u8);

impl DebounceMs {
    /// Creates a new debounce value in milliseconds.
    pub const fn new(ms: u8) -> Self {
        Self(ms)
    }

    /// Returns the raw millisecond count.
    pub const fn value(self) -> u8 {
        self.0
    }
}

impl fmt::Display for DebounceMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ms", self.0)
    }
}

/// Hardware limits and feature flags for a pointer device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PointerCapabilities {
    pub dpi_min: u32,
    pub dpi_max: u32,
    pub dpi_step: u32,
    pub max_dpi_stages: u8,
    pub supports_xy_independent_dpi: bool,
    pub supported_polling_rates: Vec<PollingRateHz>,
    pub supported_lods: Vec<LiftOffDistance>,
    pub debounce_range_ms: Option<(u8, u8)>,
    pub supports_motion_sync: bool,
    pub supports_angle_snapping: bool,
    pub supports_ripple_control: bool,
}

impl PointerCapabilities {
    /// Creates a new `PointerCapabilities` with required sensor bounds.
    pub fn new(dpi_min: u32, dpi_max: u32, dpi_step: u32, max_dpi_stages: u8) -> Self {
        Self {
            dpi_min,
            dpi_max,
            dpi_step: dpi_step.max(1),
            max_dpi_stages: max_dpi_stages.max(1),
            supports_xy_independent_dpi: false,
            supported_polling_rates: Vec::new(),
            supported_lods: Vec::new(),
            debounce_range_ms: None,
            supports_motion_sync: false,
            supports_angle_snapping: false,
            supports_ripple_control: false,
        }
    }

    pub const fn with_xy_independent_dpi(mut self, enabled: bool) -> Self {
        self.supports_xy_independent_dpi = enabled;
        self
    }

    pub fn with_polling_rates(mut self, rates: Vec<PollingRateHz>) -> Self {
        self.supported_polling_rates = rates;
        self
    }

    pub fn with_lods(mut self, lods: Vec<LiftOffDistance>) -> Self {
        self.supported_lods = lods;
        self
    }

    pub const fn with_debounce_range_ms(mut self, min_ms: u8, max_ms: u8) -> Self {
        self.debounce_range_ms = Some((min_ms, max_ms));
        self
    }

    pub const fn with_motion_sync(mut self, enabled: bool) -> Self {
        self.supports_motion_sync = enabled;
        self
    }

    pub const fn with_angle_snapping(mut self, enabled: bool) -> Self {
        self.supports_angle_snapping = enabled;
        self
    }

    pub const fn with_ripple_control(mut self, enabled: bool) -> Self {
        self.supports_ripple_control = enabled;
        self
    }

    /// Validates a single DPI value against min, max, and step bounds.
    pub fn validate_dpi(&self, dpi: DpiValue) -> CoreResult<()> {
        let v = dpi.value();
        if v < self.dpi_min || v > self.dpi_max {
            return Err(CoreError::OutOfRange {
                field: "dpi",
                actual: v.to_string(),
                expected: format!("{}..={}", self.dpi_min, self.dpi_max),
            });
        }
        if self.dpi_step > 1 && !(v - self.dpi_min).is_multiple_of(self.dpi_step) {
            return Err(CoreError::Validation(format!(
                "DPI {v} does not align with step {} from min {}",
                self.dpi_step, self.dpi_min
            )));
        }
        Ok(())
    }

    /// Validates a complete set of DPI stages and the active stage index.
    pub fn validate_dpi_stages(&self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        if stages.is_empty() || stages.len() > usize::from(self.max_dpi_stages) {
            return Err(CoreError::OutOfRange {
                field: "dpi_stages",
                actual: stages.len().to_string(),
                expected: format!("1..={}", self.max_dpi_stages),
            });
        }
        if usize::from(active_ix) >= stages.len() {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: active_ix.to_string(),
                expected: format!("0..{}", stages.len()),
            });
        }
        for stage in stages {
            self.validate_dpi(stage.x)?;
            if let Some(y) = stage.y {
                if !self.supports_xy_independent_dpi && y != stage.x {
                    return Err(CoreError::UnsupportedCapability("xy_independent_dpi"));
                }
                self.validate_dpi(y)?;
            }
        }
        Ok(())
    }

    /// Validates a polling rate against the device's supported list.
    pub fn validate_polling_rate(&self, rate: PollingRateHz) -> CoreResult<()> {
        if !self.supported_polling_rates.contains(&rate) {
            return Err(CoreError::OutOfRange {
                field: "polling_rate",
                actual: rate.to_string(),
                expected: format!("{:?}", self.supported_polling_rates),
            });
        }
        Ok(())
    }

    /// Validates lift-off distance against supported values.
    pub fn validate_lod(&self, lod: LiftOffDistance) -> CoreResult<()> {
        if self.supported_lods.is_empty() {
            return Err(CoreError::UnsupportedCapability("lift_off_distance"));
        }
        if !self.supported_lods.contains(&lod) {
            return Err(CoreError::OutOfRange {
                field: "lod",
                actual: lod.to_string(),
                expected: format!("{:?}", self.supported_lods),
            });
        }
        Ok(())
    }

    /// Validates debounce milliseconds against the device's supported range.
    pub fn validate_debounce(&self, debounce: DebounceMs) -> CoreResult<()> {
        let Some((min, max)) = self.debounce_range_ms else {
            return Err(CoreError::UnsupportedCapability("debounce_ms"));
        };
        if debounce.value() < min || debounce.value() > max {
            return Err(CoreError::OutOfRange {
                field: "debounce_ms",
                actual: debounce.to_string(),
                expected: format!("{min}..={max} ms"),
            });
        }
        Ok(())
    }
}

/// Immutable snapshot of a pointer device's current configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PointerSnapshot {
    pub active_dpi_stage: u8,
    pub dpi_stages: Vec<DpiStage>,
    pub polling_rate: PollingRateHz,
    pub lod: Option<LiftOffDistance>,
    pub debounce_ms: Option<DebounceMs>,
    pub motion_sync: Option<bool>,
    pub angle_snapping: Option<bool>,
    pub ripple_control: Option<bool>,
}

impl PointerSnapshot {
    /// Creates a minimal snapshot with active DPI stages and polling rate.
    pub fn new(
        active_dpi_stage: u8,
        dpi_stages: Vec<DpiStage>,
        polling_rate: PollingRateHz,
    ) -> Self {
        Self {
            active_dpi_stage,
            dpi_stages,
            polling_rate,
            lod: None,
            debounce_ms: None,
            motion_sync: None,
            angle_snapping: None,
            ripple_control: None,
        }
    }

    pub const fn with_lod(mut self, lod: LiftOffDistance) -> Self {
        self.lod = Some(lod);
        self
    }

    pub const fn with_debounce_ms(mut self, debounce: DebounceMs) -> Self {
        self.debounce_ms = Some(debounce);
        self
    }

    pub const fn with_motion_sync(mut self, enabled: bool) -> Self {
        self.motion_sync = Some(enabled);
        self
    }

    pub const fn with_angle_snapping(mut self, enabled: bool) -> Self {
        self.angle_snapping = Some(enabled);
        self
    }

    pub const fn with_ripple_control(mut self, enabled: bool) -> Self {
        self.ripple_control = Some(enabled);
        self
    }
}

/// Capability trait for gaming mice and pointer peripherals.
pub trait PointerDevice: Send + Sync {
    /// Returns sensor bounds and supported toggles for capability-driven UI/validation.
    fn pointer_capabilities(&self) -> PointerCapabilities;

    /// Performs a read-only query of current pointer configuration.
    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot>;

    /// Updates onboard DPI stages and selects `active_ix`.
    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()>;

    /// Selects an existing DPI stage by zero-based index.
    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()>;

    /// Configures the report polling rate.
    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()>;

    /// Configures sensor lift-off distance.
    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        let _ = lod;
        Err(CoreError::UnsupportedCapability("lift_off_distance"))
    }

    /// Configures switch click debounce latency in milliseconds.
    fn set_debounce_ms(&mut self, debounce: DebounceMs) -> CoreResult<()> {
        let _ = debounce;
        Err(CoreError::UnsupportedCapability("debounce_ms"))
    }

    /// Toggles sensor motion sync.
    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        let _ = enabled;
        Err(CoreError::UnsupportedCapability("motion_sync"))
    }

    /// Toggles sensor angle snapping.
    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        let _ = enabled;
        Err(CoreError::UnsupportedCapability("angle_snapping"))
    }

    /// Toggles sensor ripple control.
    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        let _ = enabled;
        Err(CoreError::UnsupportedCapability("ripple_control"))
    }
}
