//! Brand-agnostic RGB lighting capability trait and validated models.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// 24-bit sRGB color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl RgbColor {
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

impl fmt::Display for RgbColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

/// Validated lighting brightness percentage (`0..=100`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Brightness(u8);

impl Brightness {
    pub fn new(percent: u8) -> CoreResult<Self> {
        if percent > 100 {
            return Err(CoreError::OutOfRange {
                field: "brightness",
                actual: percent.to_string(),
                expected: "0..=100".to_owned(),
            });
        }
        Ok(Self(percent))
    }

    pub const fn percent(self) -> u8 {
        self.0
    }
}

/// Direction for animated wave/marquee lighting effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RgbDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// Discriminant listing which effect kinds a device supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RgbEffectKind {
    Off,
    Static,
    Breathing,
    SpectrumCycle,
    Wave,
    Reactive,
    /// Vendor/firmware-defined lighting preset identified by numeric ID.
    Preset,
}

/// Metadata describing one firmware-defined lighting preset exposed by a zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RgbPresetInfo {
    pub id: u8,
    pub name: String,
    pub supports_color: bool,
    pub supports_speed: bool,
    pub supports_colorful: bool,
}

impl RgbPresetInfo {
    pub fn new(
        id: u8,
        name: impl Into<String>,
        supports_color: bool,
        supports_speed: bool,
        supports_colorful: bool,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            supports_color,
            supports_speed,
            supports_colorful,
        }
    }
}

/// Configured RGB lighting effect parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RgbEffect {
    Off,
    Static {
        color: RgbColor,
    },
    Breathing {
        color: RgbColor,
        speed: u8,
    },
    SpectrumCycle {
        speed: u8,
    },
    Wave {
        speed: u8,
        direction: RgbDirection,
    },
    Reactive {
        color: RgbColor,
        speed: u8,
    },
    /// Firmware preset effect by ID with color, speed (`0..=255`), and full-color toggle.
    Preset {
        id: u8,
        color: RgbColor,
        speed: u8,
        colorful: bool,
    },
}

impl RgbEffect {
    pub const fn kind(&self) -> RgbEffectKind {
        match self {
            Self::Off => RgbEffectKind::Off,
            Self::Static { .. } => RgbEffectKind::Static,
            Self::Breathing { .. } => RgbEffectKind::Breathing,
            Self::SpectrumCycle { .. } => RgbEffectKind::SpectrumCycle,
            Self::Wave { .. } => RgbEffectKind::Wave,
            Self::Reactive { .. } => RgbEffectKind::Reactive,
            Self::Preset { .. } => RgbEffectKind::Preset,
        }
    }
}

/// Identifier of an addressable RGB zone on a peripheral (e.g., Logo, Scroll Wheel, Underglow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RgbZoneId(u8);

impl RgbZoneId {
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    pub const fn value(self) -> u8 {
        self.0
    }
}

/// Metadata describing one lighting zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RgbZoneInfo {
    pub id: RgbZoneId,
    pub name: String,
    /// Optional zone-specific firmware lighting presets (e.g. Back Lamp 21 modes, Logo Light 5 modes).
    pub presets: Vec<RgbPresetInfo>,
    /// Maximum raw speed value supported by this zone (`255` default, `40` for ATK Logo Light).
    pub max_speed: u8,
}

impl RgbZoneInfo {
    pub fn new(id: RgbZoneId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            presets: Vec::new(),
            max_speed: 255,
        }
    }

    pub fn with_presets(mut self, presets: Vec<RgbPresetInfo>) -> Self {
        self.presets = presets;
        self
    }

    pub const fn with_max_speed(mut self, max_speed: u8) -> Self {
        self.max_speed = max_speed;
        self
    }
}

/// Active lighting state of one RGB zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RgbZoneState {
    pub zone: RgbZoneId,
    pub effect: RgbEffect,
    pub brightness: Brightness,
}

impl RgbZoneState {
    pub const fn new(zone: RgbZoneId, effect: RgbEffect, brightness: Brightness) -> Self {
        Self {
            zone,
            effect,
            brightness,
        }
    }
}

/// Lighting capabilities exposed by a peripheral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RgbCapabilities {
    pub zones: Vec<RgbZoneInfo>,
    pub supported_effects: Vec<RgbEffectKind>,
    pub supports_per_key_rgb: bool,
}

impl RgbCapabilities {
    pub fn new(
        zones: Vec<RgbZoneInfo>,
        supported_effects: Vec<RgbEffectKind>,
        supports_per_key_rgb: bool,
    ) -> Self {
        Self {
            zones,
            supported_effects,
            supports_per_key_rgb,
        }
    }
}

/// Immutable snapshot of all RGB zones on a peripheral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RgbSnapshot {
    pub zones: Vec<RgbZoneState>,
}

impl RgbSnapshot {
    pub fn new(zones: Vec<RgbZoneState>) -> Self {
        Self { zones }
    }
}

/// Capability trait for peripherals with configurable RGB lighting.
pub trait RgbController: Send + Sync {
    fn rgb_capabilities(&self) -> RgbCapabilities;
    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot>;
    fn set_zone_effect(
        &mut self,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()>;
}
