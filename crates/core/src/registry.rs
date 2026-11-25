//! Driver matching rules and central `DeviceRegistry` for capability discovery.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::device::{ConnectionKind, DeviceId, Peripheral, VendorKind};
use crate::error::{CoreError, CoreResult};

/// Transport-agnostic snapshot of USB HID interface attributes used for driver matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HidMatchDescriptor {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage_id: u16,
    pub interface_number: i32,
    pub path: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial_number: Option<String>,
}

impl HidMatchDescriptor {
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
            path: path.into(),
            manufacturer: None,
            product: None,
            serial_number: None,
        }
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
}

/// Declarative USB HID filter rule published by each vendor driver.
///
/// Separates wired USB cables from 2.4 GHz wireless receivers via explicit `pid`
/// and `connection` fields, and avoids opening non-configuration boot HID interfaces
/// by matching vendor-defined `usage_page` / `usage_id` / `interface_number`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DeviceMatchRule {
    pub vid: u16,
    pub pid: Option<u16>,
    pub usage_page: Option<u16>,
    pub usage_id: Option<u16>,
    pub interface_number: Option<i32>,
    pub connection: ConnectionKind,
    pub model_hint: String,
}

impl DeviceMatchRule {
    pub fn new(vid: u16, connection: ConnectionKind, model_hint: impl Into<String>) -> Self {
        Self {
            vid,
            pid: None,
            usage_page: None,
            usage_id: None,
            interface_number: None,
            connection,
            model_hint: model_hint.into(),
        }
    }

    pub const fn with_pid(mut self, pid: u16) -> Self {
        self.pid = Some(pid);
        self
    }

    pub const fn with_usage_page(mut self, usage_page: u16) -> Self {
        self.usage_page = Some(usage_page);
        self
    }

    pub const fn with_usage_id(mut self, usage_id: u16) -> Self {
        self.usage_id = Some(usage_id);
        self
    }

    pub const fn with_interface_number(mut self, interface_number: i32) -> Self {
        self.interface_number = Some(interface_number);
        self
    }

    /// Returns `true` if `descriptor` satisfies every constrained field on this rule.
    pub fn matches(&self, descriptor: &HidMatchDescriptor) -> bool {
        if self.vid != descriptor.vid {
            return false;
        }
        if let Some(pid) = self.pid
            && pid != descriptor.pid
        {
            return false;
        }
        if let Some(usage_page) = self.usage_page
            && usage_page != descriptor.usage_page
        {
            return false;
        }
        if let Some(usage_id) = self.usage_id
            && usage_id != descriptor.usage_id
        {
            return false;
        }
        if let Some(iface) = self.interface_number
            && iface != descriptor.interface_number
        {
            return false;
        }
        true
    }

    /// Specificity score used to prefer exact PID + Usage Page + Interface rules over wildcards.
    pub const fn specificity(&self) -> u8 {
        let mut score = 1; // VID always matches
        if self.pid.is_some() {
            score += 4;
        }
        if self.usage_page.is_some() {
            score += 2;
        }
        if self.usage_id.is_some() {
            score += 1;
        }
        if self.interface_number.is_some() {
            score += 1;
        }
        score
    }
}

/// Metadata registered by a driver crate describing which devices it can claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DriverDescriptor {
    pub driver_id: String,
    pub vendor: VendorKind,
    pub rules: Vec<DeviceMatchRule>,
}

impl DriverDescriptor {
    pub fn new(
        driver_id: impl Into<String>,
        vendor: VendorKind,
        rules: Vec<DeviceMatchRule>,
    ) -> Self {
        Self {
            driver_id: driver_id.into(),
            vendor,
            rules,
        }
    }
}

/// Resolution result associating an enumerated HID interface with its winning driver rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DriverMatch {
    pub driver_id: String,
    pub vendor: VendorKind,
    pub connection: ConnectionKind,
    pub model_hint: String,
    pub specificity: u8,
}

/// Central registry for driver match rules and active brand-agnostic `Peripheral` handles.
#[derive(Default)]
pub struct DeviceRegistry {
    drivers: Vec<DriverDescriptor>,
    devices: BTreeMap<DeviceId, Box<dyn Peripheral>>,
}

impl fmt::Debug for DeviceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let device_ids: Vec<&DeviceId> = self.devices.keys().collect();
        f.debug_struct("DeviceRegistry")
            .field("drivers", &self.drivers)
            .field("device_ids", &device_ids)
            .finish()
    }
}

impl DeviceRegistry {
    /// Creates an empty `DeviceRegistry`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a driver's declarative match descriptor.
    pub fn register_driver(&mut self, descriptor: DriverDescriptor) {
        self.drivers.push(descriptor);
    }

    /// Returns all registered driver descriptors.
    pub fn drivers(&self) -> &[DriverDescriptor] {
        &self.drivers
    }

    /// Finds the highest-specificity driver rule matching a given HID interface descriptor.
    pub fn match_interface(&self, descriptor: &HidMatchDescriptor) -> Option<DriverMatch> {
        let mut best: Option<DriverMatch> = None;
        for driver in &self.drivers {
            for rule in &driver.rules {
                if rule.matches(descriptor) {
                    let specificity = rule.specificity();
                    let replace = best
                        .as_ref()
                        .is_none_or(|current| specificity > current.specificity);
                    if replace {
                        best = Some(DriverMatch {
                            driver_id: driver.driver_id.clone(),
                            vendor: driver.vendor,
                            connection: rule.connection,
                            model_hint: rule.model_hint.clone(),
                            specificity,
                        });
                    }
                }
            }
        }
        best
    }

    /// Inserts or replaces an opened peripheral instance keyed by its stable `DeviceId`.
    pub fn insert_device(&mut self, device: Box<dyn Peripheral>) -> DeviceId {
        let id = device.info().id.clone();
        self.devices.insert(id.clone(), device);
        id
    }

    /// Removes a peripheral from the registry.
    pub fn remove_device(&mut self, id: &DeviceId) -> Option<Box<dyn Peripheral>> {
        self.devices.remove(id)
    }

    /// Looks up a peripheral by `DeviceId`.
    pub fn get(&self, id: &DeviceId) -> CoreResult<&dyn Peripheral> {
        self.devices
            .get(id)
            .map(AsRef::as_ref)
            .ok_or_else(|| CoreError::DeviceNotFound(id.to_string()))
    }

    /// Looks up a mutable peripheral by `DeviceId`.
    pub fn get_mut(&mut self, id: &DeviceId) -> CoreResult<&mut (dyn Peripheral + 'static)> {
        self.devices
            .get_mut(id)
            .map(AsMut::as_mut)
            .ok_or_else(|| CoreError::DeviceNotFound(id.to_string()))
    }

    /// Iterates over all active peripherals in deterministic `DeviceId` order.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Peripheral> {
        self.devices.values().map(AsRef::as_ref)
    }

    /// Returns the count of currently open peripherals.
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Returns `true` if no peripherals are currently open.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::{BatteryStatus, ChargingState};
    use crate::hall_effect::{
        HallEffectCapabilities, KeyIndex, RapidTriggerConfig, SocdMode, SocdPair,
        TravelHundredthsMm,
    };
    use crate::pointer::{
        DebounceMs, DpiStage, DpiValue, LiftOffDistance, PointerCapabilities, PollingRateHz,
    };
    use crate::profile::{ProfileCapabilities, ProfileSlot};
    use crate::rgb::Brightness;

    #[test]
    fn pointer_capability_validation_enforces_bounds_and_step() {
        let caps = PointerCapabilities::new(100, 26_000, 50, 5)
            .with_polling_rates(vec![PollingRateHz::Hz1000, PollingRateHz::Hz4000])
            .with_lods(vec![
                LiftOffDistance::TenthsMm(10),
                LiftOffDistance::TenthsMm(20),
            ])
            .with_debounce_range_ms(0, 20);

        assert!(caps.validate_dpi(DpiValue::new(800).unwrap()).is_ok());
        assert!(caps.validate_dpi(DpiValue::new(825).unwrap()).is_err());
        assert!(caps.validate_dpi(DpiValue::new(50).unwrap()).is_err());
        assert!(caps.validate_dpi(DpiValue::new(30_000).unwrap()).is_err());

        let stages = vec![
            DpiStage::new(0, DpiValue::new(800).unwrap()),
            DpiStage::new(1, DpiValue::new(1600).unwrap()),
        ];
        assert!(caps.validate_dpi_stages(&stages, 1).is_ok());
        assert!(caps.validate_dpi_stages(&stages, 2).is_err());

        let xy_stage = vec![
            DpiStage::new(0, DpiValue::new(800).unwrap()).with_y(DpiValue::new(1200).unwrap()),
        ];
        assert!(caps.validate_dpi_stages(&xy_stage, 0).is_err());

        assert!(caps.validate_polling_rate(PollingRateHz::Hz4000).is_ok());
        assert!(caps.validate_polling_rate(PollingRateHz::Hz8000).is_err());
        assert!(caps.validate_lod(LiftOffDistance::TenthsMm(10)).is_ok());
        assert!(caps.validate_lod(LiftOffDistance::High).is_err());
        assert!(caps.validate_debounce(DebounceMs::new(4)).is_ok());
        assert!(caps.validate_debounce(DebounceMs::new(25)).is_err());
    }

    #[test]
    fn hall_effect_validation_uses_fixed_point_hundredths_mm() {
        let caps = HallEffectCapabilities::new(
            82,
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(400),
            TravelHundredthsMm::from_hundredths(5),
        )
        .with_rapid_trigger_range(
            TravelHundredthsMm::from_hundredths(10),
            TravelHundredthsMm::from_hundredths(200),
            true,
        )
        .with_max_socd_pairs(4);

        let point = TravelHundredthsMm::from_hundredths(35);
        assert_eq!(point.to_string(), "0.35 mm");
        assert!(caps.validate_actuation(point).is_ok());
        assert!(
            caps.validate_actuation(TravelHundredthsMm::from_hundredths(12))
                .is_err()
        );
        assert!(
            caps.validate_actuation(TravelHundredthsMm::from_hundredths(5))
                .is_err()
        );

        let rt = RapidTriggerConfig::new(
            true,
            true,
            TravelHundredthsMm::from_hundredths(15),
            TravelHundredthsMm::from_hundredths(20),
        );
        assert!(caps.validate_rapid_trigger(&rt).is_ok());

        let bad_rt = RapidTriggerConfig::new(
            true,
            false,
            TravelHundredthsMm::from_hundredths(5),
            TravelHundredthsMm::from_hundredths(20),
        );
        assert!(caps.validate_rapid_trigger(&bad_rt).is_err());

        assert!(SocdPair::new(KeyIndex::new(10), KeyIndex::new(10), SocdMode::Neutral).is_err());
        let valid_pair = SocdPair::new(
            KeyIndex::new(30),
            KeyIndex::new(32),
            SocdMode::LastInputPriority,
        )
        .unwrap();
        assert!(caps.validate_socd_pairs(&[valid_pair]).is_ok());
        assert!(caps.validate_key(KeyIndex::new(82)).is_err());
    }

    #[test]
    fn profile_and_rgb_and_battery_validators_work() {
        let profile_caps = ProfileCapabilities::new(3, true, false);
        assert!(profile_caps.validate_slot(ProfileSlot::new(0)).is_ok());
        assert!(profile_caps.validate_slot(ProfileSlot::new(2)).is_ok());
        assert!(profile_caps.validate_slot(ProfileSlot::new(3)).is_err());
        assert_eq!(ProfileSlot::new(0).display_number(), 1);

        assert!(Brightness::new(100).is_ok());
        assert!(Brightness::new(101).is_err());

        let status = BatteryStatus::new(Some(87), Some(4050), ChargingState::Discharging, true);
        assert!(status.is_wireless_connected());
    }

    #[test]
    fn registry_prefers_most_specific_wired_or_dongle_match() {
        let mut registry = DeviceRegistry::new();
        registry.register_driver(DriverDescriptor::new(
            "logitech-hidpp",
            VendorKind::Logitech,
            vec![
                DeviceMatchRule::new(0x046D, ConnectionKind::WiredUsb, "Logitech Generic")
                    .with_usage_page(0xFF00),
                DeviceMatchRule::new(
                    0x046D,
                    ConnectionKind::WirelessDongle24Ghz,
                    "Logitech Lightspeed Receiver",
                )
                .with_pid(0xC547)
                .with_usage_page(0xFF00)
                .with_usage_id(0x0002),
            ],
        ));

        let dongle_iface = HidMatchDescriptor::new(0x046D, 0xC547, 0xFF00, 0x0002, 2, "hid#dongle");
        let boot_mouse = HidMatchDescriptor::new(0x046D, 0xC547, 0x0001, 0x0002, 0, "hid#boot");

        let matched = registry.match_interface(&dongle_iface).unwrap();
        assert_eq!(matched.driver_id, "logitech-hidpp");
        assert_eq!(matched.connection, ConnectionKind::WirelessDongle24Ghz);
        assert_eq!(matched.model_hint, "Logitech Lightspeed Receiver");

        // Standard mouse boot interface (Usage Page 0x0001) must not be claimed.
        assert!(registry.match_interface(&boot_mouse).is_none());
    }
}
