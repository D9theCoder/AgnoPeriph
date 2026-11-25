//! Low-battery notifications for wireless devices.
//!
//! Alerts fire once below 20 % and once more below 5 %. A level re-arms only after the
//! battery climbs back above the threshold plus a 2 % margin, or the device starts
//! charging, so readings that bounce around a threshold never repeat an alert.

use std::collections::BTreeMap;

use agnoperiph_core::{BatteryStatus, ChargingState, ConnectionKind};

/// Alert thresholds in percent, highest first.
pub const THRESHOLDS: [u8; 2] = [20, 5];
/// Recovery a reading needs above a threshold before that level can alert again.
pub const MARGIN: u8 = 2;

/// One alert to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryAlert {
    pub threshold: u8,
    pub percent: u8,
}

impl BatteryAlert {
    /// Toast title and body for `device_name`.
    pub fn message(&self, device_name: &str) -> (String, String) {
        let title = format!("{device_name} battery at {}%", self.percent);
        let body = if self.threshold > THRESHOLDS[THRESHOLDS.len() - 1] {
            format!(
                "Charge it soon. The next alert comes at {}%.",
                THRESHOLDS[THRESHOLDS.len() - 1]
            )
        } else {
            "Charge it now to keep using it wirelessly.".to_owned()
        };
        (title, body)
    }
}

/// Per-device alert memory, keyed by device id.
#[derive(Debug, Clone, Default)]
pub struct BatteryAlertTracker {
    /// Lowest threshold already alerted for each device.
    alerted: BTreeMap<String, u8>,
}

impl BatteryAlertTracker {
    /// Feeds one battery reading and returns the alert to show, if any.
    ///
    /// Wired devices, charging or full batteries, sleeping wireless devices and readings
    /// without a percentage never alert. With `enabled` off nothing alerts, but charging
    /// still re-arms the levels.
    pub fn observe(
        &mut self,
        device: &str,
        connection: ConnectionKind,
        battery: Option<BatteryStatus>,
        enabled: bool,
    ) -> Option<BatteryAlert> {
        let battery = battery?;
        if matches!(battery.state, ChargingState::Charging | ChargingState::Full) {
            self.alerted.remove(device);
            return None;
        }
        if connection == ConnectionKind::WiredUsb || !battery.wireless_connected {
            return None;
        }
        let percent = battery.percentage?;
        if let Some(&lowest) = self.alerted.get(device) {
            // Re-arm every level the battery has recovered past.
            let rearmed = THRESHOLDS
                .iter()
                .copied()
                .filter(|&t| t >= lowest && percent > t.saturating_add(MARGIN))
                .max();
            if let Some(level) = rearmed {
                match THRESHOLDS.iter().copied().find(|&t| t > level) {
                    Some(higher) if percent <= higher.saturating_add(MARGIN) => {
                        self.alerted.insert(device.to_owned(), higher);
                    }
                    _ => {
                        self.alerted.remove(device);
                    }
                }
            }
        }
        if !enabled {
            return None;
        }
        let already = self.alerted.get(device).copied();
        let level = THRESHOLDS.iter().copied().filter(|&t| percent < t).min()?;
        if already.is_some_and(|a| a <= level) {
            return None;
        }
        self.alerted.insert(device.to_owned(), level);
        Some(BatteryAlert {
            threshold: level,
            percent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(pct: u8, state: ChargingState) -> BatteryStatus {
        BatteryStatus::new(Some(pct), None, state, true)
    }

    fn observe(tracker: &mut BatteryAlertTracker, pct: u8) -> Option<u8> {
        tracker
            .observe(
                "mouse",
                ConnectionKind::WirelessDongle24Ghz,
                Some(reading(pct, ChargingState::Discharging)),
                true,
            )
            .map(|a| a.threshold)
    }

    #[test]
    fn alerts_once_per_level_with_hysteresis() {
        let mut tracker = BatteryAlertTracker::default();
        assert_eq!(observe(&mut tracker, 40), None);
        assert_eq!(observe(&mut tracker, 19), Some(20));
        assert_eq!(observe(&mut tracker, 18), None);
        // Bouncing inside the margin never repeats.
        assert_eq!(observe(&mut tracker, 21), None);
        assert_eq!(observe(&mut tracker, 19), None);
        assert_eq!(observe(&mut tracker, 4), Some(5));
        assert_eq!(observe(&mut tracker, 6), None);
        assert_eq!(observe(&mut tracker, 4), None);
        // Recovering past 5 + margin re-arms only the 5 % level.
        assert_eq!(observe(&mut tracker, 8), None);
        assert_eq!(observe(&mut tracker, 4), Some(5));
        // Recovering past 20 + margin re-arms everything.
        assert_eq!(observe(&mut tracker, 23), None);
        assert_eq!(observe(&mut tracker, 19), Some(20));
    }

    #[test]
    fn first_reading_below_five_alerts_once() {
        let mut tracker = BatteryAlertTracker::default();
        assert_eq!(observe(&mut tracker, 3), Some(5));
        assert_eq!(observe(&mut tracker, 2), None);
        assert_eq!(observe(&mut tracker, 12), None);
    }

    #[test]
    fn charging_rearms_and_wired_or_disabled_never_alert() {
        let mut tracker = BatteryAlertTracker::default();
        assert_eq!(observe(&mut tracker, 15), Some(20));
        let charging = tracker.observe(
            "mouse",
            ConnectionKind::WirelessDongle24Ghz,
            Some(reading(15, ChargingState::Charging)),
            true,
        );
        assert!(charging.is_none());
        assert_eq!(observe(&mut tracker, 15), Some(20));

        let mut tracker = BatteryAlertTracker::default();
        let wired = tracker.observe(
            "mouse",
            ConnectionKind::WiredUsb,
            Some(reading(3, ChargingState::Discharging)),
            true,
        );
        assert!(wired.is_none());
        let disabled = tracker.observe(
            "mouse",
            ConnectionKind::WirelessDongle24Ghz,
            Some(reading(3, ChargingState::Discharging)),
            false,
        );
        assert!(disabled.is_none());
        let asleep = tracker.observe(
            "mouse",
            ConnectionKind::WirelessDongle24Ghz,
            Some(BatteryStatus::new(
                Some(3),
                None,
                ChargingState::Discharging,
                false,
            )),
            true,
        );
        assert!(asleep.is_none());
        assert_eq!(observe(&mut tracker, 3), Some(5));
    }

    #[test]
    fn message_names_device_and_next_level() {
        let (title, body) = BatteryAlert {
            threshold: 20,
            percent: 15,
        }
        .message("Lamzu Atlantis");
        assert_eq!(title, "Lamzu Atlantis battery at 15%");
        assert_eq!(body, "Charge it soon. The next alert comes at 5%.");
    }
}
