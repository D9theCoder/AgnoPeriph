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
