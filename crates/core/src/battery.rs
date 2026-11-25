//! Brand-agnostic battery telemetry trait and status model.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CoreResult;

/// Battery charging status reported by a wireless peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChargingState {
    Discharging,
    Charging,
    Full,
    Error,
    Unknown,
}

impl fmt::Display for ChargingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discharging => f.write_str("Discharging"),
            Self::Charging => f.write_str("Charging"),
            Self::Full => f.write_str("Full"),
            Self::Error => f.write_str("Error"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// Read-only battery telemetry snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BatteryStatus {
    /// Charge level in percent (`0..=100`), if reported.
    pub percentage: Option<u8>,
    /// Battery pack voltage in millivolts (e.g., Logitech `BatteryVoltage` `0x1001`), if reported.
    pub voltage_mv: Option<u16>,
    pub state: ChargingState,
    /// `false` when a wireless dongle is plugged in but the peripheral is asleep/off.
    pub wireless_connected: bool,
}

impl BatteryStatus {
    pub const fn new(
        percentage: Option<u8>,
        voltage_mv: Option<u16>,
        state: ChargingState,
        wireless_connected: bool,
    ) -> Self {
        Self {
            percentage,
            voltage_mv,
            state,
            wireless_connected,
        }
    }

    pub const fn is_wireless_connected(&self) -> bool {
        self.wireless_connected
    }
}

/// Capability trait for peripherals that expose battery telemetry.
pub trait BatteryMonitor: Send + Sync {
    /// Performs a read-only battery status query.
    fn read_battery(&mut self) -> CoreResult<BatteryStatus>;
}
