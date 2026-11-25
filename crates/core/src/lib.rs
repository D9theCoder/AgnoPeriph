//! Brand-agnostic hardware abstraction traits, strongly typed validation models,
//! and capability discovery for AgnoPeriph.
//!
//! This crate is pure Rust with zero OS, `hidapi`, or UI dependencies.

#![forbid(unsafe_code)]

pub mod battery;
pub mod device;
pub mod error;
pub mod hall_effect;
pub mod pointer;
pub mod profile;
pub mod registry;
pub mod rgb;

pub use battery::{BatteryMonitor, BatteryStatus, ChargingState};
pub use device::{
    Capabilities, ConnectionKind, DeviceId, DeviceInfo, Peripheral, TransportProfile, VendorKind,
};
pub use error::{CoreError, CoreResult};
pub use hall_effect::{
    CalibrationStatus, DksBinding, DksStageAction, DksTriggerDirection, HallEffectCapabilities,
    HallEffectKeyboard, HallEffectSnapshot, KeyHallEffectConfig, KeyIndex, RapidTriggerConfig,
    SocdMode, SocdPair, TravelHundredthsMm,
};
pub use pointer::{
    DebounceMs, DpiStage, DpiValue, LiftOffDistance, PointerCapabilities, PointerDevice,
    PointerSnapshot, PollingRateHz,
};
pub use profile::{
    ProfileCapabilities, ProfileInfo, ProfileManager, ProfileMode, ProfileSlot, ProfileSnapshot,
};
pub use registry::{
    DeviceMatchRule, DeviceRegistry, DriverDescriptor, DriverMatch, HidMatchDescriptor,
};
pub use rgb::{
    Brightness, RgbCapabilities, RgbColor, RgbController, RgbDirection, RgbEffect, RgbEffectKind,
    RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState,
};
