//! Brand-agnostic onboard hardware profile slot management.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// Zero-based index identifying an onboard hardware profile slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProfileSlot(u8);

impl ProfileSlot {
    pub const fn new(index: u8) -> Self {
        Self(index)
    }

    /// Zero-based slot index.
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Human-readable one-based slot number.
    pub const fn display_number(self) -> u8 {
        self.0.saturating_add(1)
    }
}

impl fmt::Display for ProfileSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Profile {}", self.display_number())
    }
}

/// Memory execution mode on devices that distinguish onboard vs. host-driven configuration
/// (such as Logitech HID++ 2.0 `OnboardProfiles` feature `0x8100`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProfileMode {
    /// Device autonomously runs settings stored in its onboard memory slot.
    OnboardMemory,
    /// Host software dynamically pushes live settings without persisting to onboard slots.
    HostControlled,
}

impl fmt::Display for ProfileMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OnboardMemory => f.write_str("Onboard Memory"),
            Self::HostControlled => f.write_str("Host Controlled"),
        }
    }
}

/// Metadata describing one onboard profile slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProfileInfo {
    pub slot: ProfileSlot,
    pub label: Option<String>,
    pub enabled: bool,
}

impl ProfileInfo {
    pub fn new(slot: ProfileSlot, enabled: bool) -> Self {
        Self {
            slot,
            label: None,
            enabled,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Hardware capabilities for onboard profile switching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProfileCapabilities {
    pub slot_count: u8,
    pub supports_mode_switch: bool,
    pub supports_profile_labels: bool,
}

impl ProfileCapabilities {
    pub const fn new(
        slot_count: u8,
        supports_mode_switch: bool,
        supports_profile_labels: bool,
    ) -> Self {
        Self {
            slot_count,
            supports_mode_switch,
            supports_profile_labels,
        }
    }

    pub fn validate_slot(&self, slot: ProfileSlot) -> CoreResult<()> {
        if slot.index() >= self.slot_count {
            return Err(CoreError::OutOfRange {
                field: "profile_slot",
                actual: slot.index().to_string(),
                expected: format!("0..{}", self.slot_count),
            });
        }
        Ok(())
    }
}

/// Read-only snapshot of onboard profile state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProfileSnapshot {
    pub active_slot: ProfileSlot,
    pub mode: ProfileMode,
    pub profiles: Vec<ProfileInfo>,
}

impl ProfileSnapshot {
    pub fn new(active_slot: ProfileSlot, mode: ProfileMode, profiles: Vec<ProfileInfo>) -> Self {
        Self {
            active_slot,
            mode,
            profiles,
        }
    }
}

/// Capability trait for peripherals with onboard hardware profile slots.
pub trait ProfileManager: Send + Sync {
    /// Returns slot count and supported profile controls.
    fn profile_capabilities(&self) -> ProfileCapabilities;

    /// Performs a read-only query of the active slot and onboard/host mode.
    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot>;

    /// Switches the active onboard hardware profile slot.
    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()>;

    /// Switches between onboard memory mode and host-controlled mode when supported.
    fn set_profile_mode(&mut self, mode: ProfileMode) -> CoreResult<()> {
        let _ = mode;
        Err(CoreError::UnsupportedCapability("profile_mode_switch"))
    }
}

/// Current schema version for portable JSON device profile exports.
pub const EXPORTED_PROFILE_SCHEMA_VERSION: u32 = 1;

/// Portable, validated JSON profile payload for importing/exporting mouse and keyboard tuning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExportedProfile {
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub vendor: Option<crate::device::VendorKind>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub pointer: Option<crate::pointer::PointerSnapshot>,
    #[serde(default)]
    pub hall_effect: Option<ExportedHallEffectProfile>,
    #[serde(default)]
    pub rgb_zones: Vec<crate::rgb::RgbZoneState>,
    #[serde(default)]
    pub keyboard_config: Option<crate::keyboard::KeyboardConfigSnapshot>,
}

/// Portable Hall Effect keyboard tuning section inside [`ExportedProfile`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ExportedHallEffectProfile {
    pub global_actuation: Option<crate::hall_effect::TravelHundredthsMm>,
    pub top_deadzone: Option<crate::hall_effect::TravelHundredthsMm>,
    pub bottom_deadzone: Option<crate::hall_effect::TravelHundredthsMm>,
    pub keys: Vec<crate::hall_effect::KeyHallEffectConfig>,
    pub socd_pairs: Vec<crate::hall_effect::SocdPair>,
    pub rapid_switch_pairs: Vec<crate::hall_effect::RapidSwitchPair>,
    pub dks_bindings: Vec<crate::hall_effect::DksBinding>,
    pub mod_tap_bindings: Vec<crate::hall_effect::ModTapBinding>,
    pub toggle_bindings: Vec<crate::hall_effect::ToggleKeyBinding>,
}

impl ExportedProfile {
    /// Creates a new versioned profile export with the given display name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema_version: EXPORTED_PROFILE_SCHEMA_VERSION,
            name: name.into(),
            vendor: None,
            model_name: None,
            pointer: None,
            hall_effect: None,
            rgb_zones: Vec::new(),
            keyboard_config: None,
        }
    }

    pub fn with_device_hint(
        mut self,
        vendor: crate::device::VendorKind,
        model_name: impl Into<String>,
    ) -> Self {
        self.vendor = Some(vendor);
        self.model_name = Some(model_name.into());
        self
    }

    pub fn with_pointer(mut self, pointer: crate::pointer::PointerSnapshot) -> Self {
        self.pointer = Some(pointer);
        self
    }

    pub fn with_hall_effect(mut self, hall_effect: ExportedHallEffectProfile) -> Self {
        self.hall_effect = Some(hall_effect);
        self
    }

    pub fn with_rgb_zones(mut self, zones: Vec<crate::rgb::RgbZoneState>) -> Self {
        self.rgb_zones = zones;
        self
    }

    pub fn with_keyboard_config(mut self, config: crate::keyboard::KeyboardConfigSnapshot) -> Self {
        self.keyboard_config = Some(config);
        self
    }

    /// Validates structural invariants and optional target device capability limits.
    pub fn validate(
        &self,
        pointer_caps: Option<&crate::pointer::PointerCapabilities>,
        hall_effect_caps: Option<&crate::hall_effect::HallEffectCapabilities>,
        rgb_caps: Option<&crate::rgb::RgbCapabilities>,
        kb_caps: Option<&crate::keyboard::KeyboardConfigCapabilities>,
    ) -> CoreResult<()> {
        if self.schema_version != EXPORTED_PROFILE_SCHEMA_VERSION {
            return Err(CoreError::Validation(format!(
                "unsupported profile schema_version {}, expected {EXPORTED_PROFILE_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if self.name.trim().is_empty() {
            return Err(CoreError::Validation(
                "profile name must not be empty".to_owned(),
            ));
        }
        if let Some(pointer) = &self.pointer {
            let Some(caps) = pointer_caps else {
                return Err(CoreError::UnsupportedCapability("pointer"));
            };
            caps.validate_dpi_stages(&pointer.dpi_stages, pointer.active_dpi_stage)?;
            if !caps.supported_polling_rates.is_empty() {
                caps.validate_polling_rate(pointer.polling_rate)?;
            }
            if let Some(lod) = pointer.lod
                && !caps.supported_lods.is_empty()
            {
                caps.validate_lod(lod)?;
            }
            if let Some(debounce) = pointer.debounce_ms
                && caps.debounce_range_ms.is_some()
            {
                caps.validate_debounce(debounce)?;
            }
        }
        if let Some(he) = &self.hall_effect {
            let Some(caps) = hall_effect_caps else {
                return Err(CoreError::UnsupportedCapability("hall_effect"));
            };
            if let Some(point) = he.global_actuation {
                caps.validate_actuation(point)?;
            }
            for key_cfg in &he.keys {
                caps.validate_key(key_cfg.key)?;
                caps.validate_actuation(key_cfg.actuation)?;
                caps.validate_rapid_trigger(&key_cfg.rapid_trigger)?;
            }
            if !he.socd_pairs.is_empty() {
                caps.validate_socd_pairs(&he.socd_pairs)?;
            }
            if !he.rapid_switch_pairs.is_empty() {
                caps.validate_rapid_switch_pairs(&he.rapid_switch_pairs)?;
            }
        }
        if !self.rgb_zones.is_empty()
            && let Some(caps) = rgb_caps
        {
            for zone in &self.rgb_zones {
                if !caps.zones.iter().any(|info| info.id == zone.zone) {
                    return Err(CoreError::OutOfRange {
                        field: "rgb_zone",
                        actual: zone.zone.value().to_string(),
                        expected: "a zone supported by this device".to_owned(),
                    });
                }
                if !caps.supported_effects.contains(&zone.effect.kind()) {
                    return Err(CoreError::UnsupportedCapability("rgb_effect"));
                }
            }
        }
        if let Some(cfg) = &self.keyboard_config
            && let Some(caps) = kb_caps
            && !caps.supported_polling_rates.is_empty()
            && !caps.supported_polling_rates.contains(&cfg.polling_rate)
        {
            return Err(CoreError::OutOfRange {
                field: "polling_rate",
                actual: cfg.polling_rate.to_string(),
                expected: format!("{:?}", caps.supported_polling_rates),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DeviceInfo, DpiStage, DpiValue, HallEffectCapabilities, KeyHallEffectConfig, KeyIndex,
        LiftOffDistance, PointerCapabilities, PointerSnapshot, PollingRateHz, RapidTriggerConfig,
        RgbCapabilities, RgbColor, RgbEffect, RgbEffectKind, RgbZoneId, RgbZoneInfo, RgbZoneState,
        TravelHundredthsMm, VendorKind,
    };

    #[test]
    fn exported_profile_validates_pointer_and_hall_effect_payloads() {
        let pointer_caps = PointerCapabilities::new(100, 26_000, 50, 5)
            .with_polling_rates(vec![PollingRateHz::Hz1000, PollingRateHz::Hz4000])
            .with_lods(vec![LiftOffDistance::Low, LiftOffDistance::High]);
        let pointer_snap = PointerSnapshot::new(
            0,
            vec![
                DpiStage::new(0, DpiValue::new(800).unwrap()),
                DpiStage::new(1, DpiValue::new(1600).unwrap()),
            ],
            PollingRateHz::Hz1000,
        )
        .with_lod(LiftOffDistance::Low);

        let he_caps = HallEffectCapabilities::new(
            96,
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(330),
            TravelHundredthsMm::from_hundredths(10),
        )
        .with_rapid_trigger_range(
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(200),
            true,
        );
        let he_profile = ExportedHallEffectProfile {
            global_actuation: Some(TravelHundredthsMm::from_hundredths(40)),
            keys: vec![KeyHallEffectConfig::new(
                KeyIndex::new(33),
                TravelHundredthsMm::from_hundredths(40),
                RapidTriggerConfig::new(
                    true,
                    true,
                    TravelHundredthsMm::from_hundredths(20),
                    TravelHundredthsMm::from_hundredths(20),
                ),
            )],
            ..Default::default()
        };

        let rgb_caps = RgbCapabilities::new(
            vec![RgbZoneInfo::new(RgbZoneId::new(0), "Primary")],
            vec![RgbEffectKind::Static, RgbEffectKind::Off],
            true,
        );
        let zone = RgbZoneState::new(
            RgbZoneId::new(0),
            RgbEffect::Static {
                color: RgbColor::new(0, 180, 255),
            },
            crate::Brightness::new(80).unwrap(),
        );

        let exported = ExportedProfile::new("FPS Comp")
            .with_device_hint(VendorKind::Atk, "ATK RS7 RGB")
            .with_pointer(pointer_snap)
            .with_hall_effect(he_profile)
            .with_rgb_zones(vec![zone]);

        assert!(
            exported
                .validate(Some(&pointer_caps), Some(&he_caps), Some(&rgb_caps), None)
                .is_ok()
        );

        // Reject unsupported schema version
        let mut bad_ver = exported.clone();
        bad_ver.schema_version = 99;
        assert!(
            bad_ver
                .validate(Some(&pointer_caps), Some(&he_caps), Some(&rgb_caps), None)
                .is_err()
        );

        // Reject empty name
        let mut empty_name = exported.clone();
        empty_name.name = "   ".to_owned();
        assert!(
            empty_name
                .validate(Some(&pointer_caps), Some(&he_caps), Some(&rgb_caps), None)
                .is_err()
        );

        // Reject out-of-range actuation
        let mut bad_he = exported;
        bad_he.hall_effect.as_mut().unwrap().global_actuation =
            Some(TravelHundredthsMm::from_hundredths(400));
        assert!(
            bad_he
                .validate(Some(&pointer_caps), Some(&he_caps), Some(&rgb_caps), None)
                .is_err()
        );

        let _ = DeviceInfo::new(
            crate::DeviceId::new("test"),
            VendorKind::Mchose,
            "MCHOSE A7",
            crate::ConnectionKind::WiredUsb,
            0x3837,
            0x0001,
        );
    }
}
