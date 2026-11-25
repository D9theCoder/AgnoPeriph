//! Core peripheral identity, connection profiles, capabilities, and the root `Peripheral` trait.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::battery::BatteryMonitor;
use crate::error::CoreResult;
use crate::hall_effect::HallEffectKeyboard;
use crate::keyboard::{KeyboardConfigController, KeymapController, MacroController};
use crate::pointer::PointerDevice;
use crate::profile::ProfileManager;
use crate::rgb::RgbController;

/// Stable domain identifier for a peripheral instance.
///
/// Suitable for persisting UI selection and deriving stable GPUI `ElementId`s.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(String);

impl DeviceId {
    /// Creates a new `DeviceId` from a non-empty stable string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Logical peripheral vendor family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VendorKind {
    Logitech,
    Lamzu,
    Atk,
    Unknown(u16),
}

impl VendorKind {
    /// Identifies well-known USB Vendor IDs without guessing ambiguous OEM VIDs.
    ///
    /// Note: OEM-shared VIDs (e.g., CompX/Compx-derived receivers) should be matched
    /// with both VID and PID inside each vendor driver's match table.
    pub fn from_vid(vid: u16) -> Self {
        match vid {
            0x046D => Self::Logitech,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for VendorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Logitech => f.write_str("Logitech"),
            Self::Lamzu => f.write_str("Lamzu"),
            Self::Atk => f.write_str("ATK"),
            Self::Unknown(vid) => write!(f, "Unknown (0x{vid:04X})"),
        }
    }
}

/// Physical link topology for a peripheral interface.
///
/// Wired USB cables and 2.4 GHz wireless dongles are modeled as distinct profiles
/// because they frequently expose different PIDs, report prefixes, and battery paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConnectionKind {
    WiredUsb,
    WirelessDongle24Ghz,
    Bluetooth,
}

impl fmt::Display for ConnectionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WiredUsb => f.write_str("Wired USB"),
            Self::WirelessDongle24Ghz => f.write_str("2.4 GHz Dongle"),
            Self::Bluetooth => f.write_str("Bluetooth"),
        }
    }
}

/// Wire-level HID profile describing a specific wired or wireless interface endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TransportProfile {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage_id: u16,
    pub interface_number: i32,
    pub connection: ConnectionKind,
    pub report_id: u8,
    pub report_size: usize,
}

impl TransportProfile {
    /// Creates a new `TransportProfile`.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        vid: u16,
        pid: u16,
        usage_page: u16,
        usage_id: u16,
        interface_number: i32,
        connection: ConnectionKind,
        report_id: u8,
        report_size: usize,
    ) -> Self {
        Self {
            vid,
            pid,
            usage_page,
            usage_id,
            interface_number,
            connection,
            report_id,
            report_size,
        }
    }
}

/// High-level capabilities advertised by a `Peripheral`.
///
/// Both `agnoperiph-cli` and `crates/ui` inspect this struct (and the corresponding
/// `as_*` trait accessors) to render controls dynamically without vendor branching.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Capabilities {
    pub pointer: bool,
    pub hall_effect: bool,
    pub rgb: bool,
    pub battery: bool,
    pub profiles: bool,
    pub keymap: bool,
    pub macros: bool,
    pub keyboard_config: bool,
}

impl Capabilities {
    /// Creates an empty capability set.
    pub const fn empty() -> Self {
        Self {
            pointer: false,
            hall_effect: false,
            rgb: false,
            battery: false,
            profiles: false,
            keymap: false,
            macros: false,
            keyboard_config: false,
        }
    }

    pub const fn with_pointer(mut self, enabled: bool) -> Self {
        self.pointer = enabled;
        self
    }

    pub const fn with_hall_effect(mut self, enabled: bool) -> Self {
        self.hall_effect = enabled;
        self
    }

    pub const fn with_rgb(mut self, enabled: bool) -> Self {
        self.rgb = enabled;
        self
    }

    pub const fn with_battery(mut self, enabled: bool) -> Self {
        self.battery = enabled;
        self
    }

    pub const fn with_profiles(mut self, enabled: bool) -> Self {
        self.profiles = enabled;
        self
    }

    pub const fn with_keymap(mut self, enabled: bool) -> Self {
        self.keymap = enabled;
        self
    }

    pub const fn with_macros(mut self, enabled: bool) -> Self {
        self.macros = enabled;
        self
    }

    pub const fn with_keyboard_config(mut self, enabled: bool) -> Self {
        self.keyboard_config = enabled;
        self
    }

    pub const fn has_pointer(&self) -> bool {
        self.pointer
    }

    pub const fn has_hall_effect(&self) -> bool {
        self.hall_effect
    }

    pub const fn has_rgb(&self) -> bool {
        self.rgb
    }

    pub const fn has_battery(&self) -> bool {
        self.battery
    }

    pub const fn has_profiles(&self) -> bool {
        self.profiles
    }

    pub const fn has_keymap(&self) -> bool {
        self.keymap
    }

    pub const fn has_macros(&self) -> bool {
        self.macros
    }

    pub const fn has_keyboard_config(&self) -> bool {
        self.keyboard_config
    }
}

/// Static and semi-static identity metadata for a connected peripheral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub vendor: VendorKind,
    pub model_name: String,
    pub firmware_version: Option<String>,
    pub serial_number: Option<String>,
    pub connection: ConnectionKind,
    pub vid: u16,
    pub pid: u16,
}

impl DeviceInfo {
    /// Creates a new `DeviceInfo` with required identity fields.
    pub fn new(
        id: DeviceId,
        vendor: VendorKind,
        model_name: impl Into<String>,
        connection: ConnectionKind,
        vid: u16,
        pid: u16,
    ) -> Self {
        Self {
            id,
            vendor,
            model_name: model_name.into(),
            firmware_version: None,
            serial_number: None,
            connection,
            vid,
            pid,
        }
    }

    pub fn with_firmware_version(mut self, version: impl Into<String>) -> Self {
        self.firmware_version = Some(version.into());
        self
    }

    pub fn with_serial_number(mut self, serial: impl Into<String>) -> Self {
        self.serial_number = Some(serial.into());
        self
    }
}

/// Root brand-agnostic abstraction implemented by every peripheral driver.
pub trait Peripheral: Send + Sync {
    /// Returns the device's static metadata and connection kind.
    fn info(&self) -> &DeviceInfo;

    /// Returns the capability bitset supported by this device.
    fn capabilities(&self) -> Capabilities;

    /// Performs a read-only refresh of cached state from the physical device.
    fn refresh(&mut self) -> CoreResult<()>;

    fn as_pointer(&self) -> Option<&dyn PointerDevice> {
        None
    }

    fn as_pointer_mut(&mut self) -> Option<&mut dyn PointerDevice> {
        None
    }

    fn as_hall_effect(&self) -> Option<&dyn HallEffectKeyboard> {
        None
    }

    fn as_hall_effect_mut(&mut self) -> Option<&mut dyn HallEffectKeyboard> {
        None
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        None
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        None
    }

    fn as_battery(&self) -> Option<&dyn BatteryMonitor> {
        None
    }

    fn as_battery_mut(&mut self) -> Option<&mut dyn BatteryMonitor> {
        None
    }

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        None
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        None
    }

    fn as_keymap(&self) -> Option<&dyn KeymapController> {
        None
    }

    fn as_keymap_mut(&mut self) -> Option<&mut dyn KeymapController> {
        None
    }

    fn as_macros(&self) -> Option<&dyn MacroController> {
        None
    }

    fn as_macros_mut(&mut self) -> Option<&mut dyn MacroController> {
        None
    }

    fn as_keyboard_config(&self) -> Option<&dyn KeyboardConfigController> {
        None
    }

    fn as_keyboard_config_mut(&mut self) -> Option<&mut dyn KeyboardConfigController> {
        None
    }
}
