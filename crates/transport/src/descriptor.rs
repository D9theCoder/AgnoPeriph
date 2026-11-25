//! USB HID interface metadata normalized from OS enumeration.

use std::fmt;

use agnoperiph_core::HidMatchDescriptor;
use serde::{Deserialize, Serialize};

/// Physical bus reported by the OS HID stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HidBusType {
    Usb,
    Bluetooth,
    I2c,
    Spi,
    Unknown,
}

impl fmt::Display for HidBusType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usb => f.write_str("USB"),
            Self::Bluetooth => f.write_str("Bluetooth"),
            Self::I2c => f.write_str("I2C"),
            Self::Spi => f.write_str("SPI"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// Complete description of one enumerated Windows HID interface endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HidDeviceDescriptor {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage_id: u16,
    pub interface_number: i32,
    pub release_number: u16,
    pub bus_type: HidBusType,
    pub path: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial_number: Option<String>,
}

impl HidDeviceDescriptor {
    /// Creates a new `HidDeviceDescriptor` with required USB HID coordinates.
    pub fn new(
        vid: u16,
        pid: u16,
        usage_page: u16,
        usage_id: u16,
        interface_number: i32,
        path: impl Into<String>,
    ) -> Self {
        Self {
            vid,
            pid,
            usage_page,
            usage_id,
            interface_number,
            release_number: 0,
            bus_type: HidBusType::Usb,
            path: path.into(),
            manufacturer: None,
            product: None,
            serial_number: None,
        }
    }

    pub const fn with_release_number(mut self, release_number: u16) -> Self {
        self.release_number = release_number;
        self
    }

    pub const fn with_bus_type(mut self, bus_type: HidBusType) -> Self {
        self.bus_type = bus_type;
        self
    }

    pub fn with_strings(
        mut self,
        manufacturer: Option<String>,
        product: Option<String>,
        serial_number: Option<String>,
    ) -> Self {
        self.manufacturer = manufacturer;
        self.product = product;
        self.serial_number = serial_number;
        self
    }

    /// Returns `true` when the interface uses a vendor-defined HID Usage Page (`>= 0xFF00`).
    pub const fn is_vendor_usage_page(&self) -> bool {
        self.usage_page >= 0xFF00
    }

    /// Returns a concise human-readable identifier (`VID:PID UP:USAGE`).
    pub fn short_id(&self) -> String {
        format!(
            "{:04X}:{:04X} UP=0x{:04X} U=0x{:04X} IF={}",
            self.vid, self.pid, self.usage_page, self.usage_id, self.interface_number
        )
    }

    /// Converts into the transport-agnostic `HidMatchDescriptor` consumed by `DeviceRegistry`.
    pub fn to_match_descriptor(&self) -> HidMatchDescriptor {
        HidMatchDescriptor::new(
            self.vid,
            self.pid,
            self.usage_page,
            self.usage_id,
            self.interface_number,
            self.path.clone(),
        )
        .with_strings(
            self.manufacturer.clone(),
            self.product.clone(),
            self.serial_number.clone(),
        )
    }
}
