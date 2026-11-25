//! What the notification-area icon shows: per-device battery and profile slots, and the
//! active Game Profile. `main.rs` maps this model onto the native menu.

use agnoperiph_core::{ChargingState, ConnectionKind, DeviceId};
use gpui_kit::{Context, Window};

use crate::app::AgnoPeriphApp;

/// Windows truncates tray tooltips at 127 characters.
pub const TOOLTIP_LIMIT: usize = 127;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayDevice {
    pub id: DeviceId,
    pub name: String,
    /// Short power state, e.g. `15%`, `15% charging` or `Wired`.
    pub power: Option<String>,
    pub slots: Vec<(u8, String)>,
    pub active_slot: Option<u8>,
}

impl TrayDevice {
    /// Menu label for the device's submenu.
    pub fn label(&self) -> String {
        match &self.power {
            Some(power) => format!("{}  ({power})", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayModel {
    pub devices: Vec<TrayDevice>,
    pub status: String,
}

impl TrayModel {
    /// Tooltip with one line per device, capped to the shell's limit.
    pub fn tooltip(&self) -> String {
        let mut text = "AgnoPeriph".to_owned();
        for device in &self.devices {
            if let Some(power) = &device.power {
                let line = format!("\n{}: {power}", device.name);
                if text.chars().count() + line.chars().count() > TOOLTIP_LIMIT {
                    break;
                }
                text.push_str(&line);
            }
        }
        text
    }

    /// Changes only when the menu needs rebuilding (devices or slots), not on label updates.
    pub fn structure_key(&self) -> String {
        self.devices
            .iter()
            .map(|d| format!("{}:{}", d.id, d.slots.len()))
            .collect::<Vec<_>>()
            .join("|")
    }
}

impl AgnoPeriphApp {
    /// What the notification-area icon should show right now.
    pub fn tray_model(&self) -> TrayModel {
        let devices = self
            .devices()
            .iter()
            .map(|device| TrayDevice {
                id: device.id().clone(),
                name: device.info.model_name.clone(),
                power: power_label(
                    device.info.connection,
                    device
                        .capabilities
                        .has_battery()
                        .then_some(device.battery)
                        .flatten(),
                ),
                slots: self.preset_slots(device),
                active_slot: self.current_slot(device.id()),
            })
            .collect();
        let status = match self.active_game_name() {
            Some(name) => format!("Game Profile: {name}"),
            None => "Game Profile: Desktop / Default".to_owned(),
        };
        TrayModel { devices, status }
    }

    /// A profile picked from the tray menu. The window must be open, because unsaved
    /// edits on that device are confirmed in the window first.
    pub fn request_tray_profile_switch(
        &mut self,
        device: &DeviceId,
        slot: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.current_slot(device) == Some(slot) {
            return;
        }
        if self.is_dirty() && self.draft_device.as_ref() == Some(device) {
            self.select_device(device.clone(), cx);
        }
        self.request_profile_switch(device, slot, window, cx);
    }
}

/// Short power text for a device.
pub fn power_label(
    connection: ConnectionKind,
    battery: Option<agnoperiph_core::BatteryStatus>,
) -> Option<String> {
    match (battery.and_then(|b| b.percentage), battery.map(|b| b.state)) {
        (Some(pct), Some(ChargingState::Charging)) => Some(format!("{pct}% charging")),
        (Some(pct), _) => Some(format!("{pct}%")),
        _ if connection == ConnectionKind::WiredUsb => Some("Wired".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::BatteryStatus;

    fn device(name: &str, power: Option<&str>, slots: usize) -> TrayDevice {
        TrayDevice {
            id: DeviceId::new(name),
            name: name.to_owned(),
            power: power.map(str::to_owned),
            slots: (0..slots)
                .map(|s| (u8::try_from(s).unwrap(), format!("Profile {}", s + 1)))
                .collect(),
            active_slot: Some(0),
        }
    }

    #[test]
    fn tooltip_lists_power_and_stays_under_the_limit() {
        let model = TrayModel {
            devices: vec![
                device("Lamzu Atlantis", Some("15%"), 4),
                device("ATK RS7", Some("Wired"), 4),
            ],
            status: String::new(),
        };
        assert_eq!(
            model.tooltip(),
            "AgnoPeriph\nLamzu Atlantis: 15%\nATK RS7: Wired"
        );
        let many = TrayModel {
            devices: (0..20)
                .map(|i| device(&format!("A long device name {i}"), Some("99%"), 1))
                .collect(),
            status: String::new(),
        };
        assert!(many.tooltip().chars().count() <= TOOLTIP_LIMIT);
    }

    #[test]
    fn structure_key_ignores_labels() {
        let a = TrayModel {
            devices: vec![device("Mouse", Some("15%"), 4)],
            status: "x".into(),
        };
        let mut b = a.clone();
        b.devices[0].power = Some("14%".into());
        b.devices[0].active_slot = Some(2);
        assert_eq!(a.structure_key(), b.structure_key());
        b.devices[0].slots.pop();
        assert_ne!(a.structure_key(), b.structure_key());
    }

    #[test]
    fn power_label_prefers_battery_then_wired() {
        let battery = BatteryStatus::new(Some(15), None, ChargingState::Charging, true);
        assert_eq!(
            power_label(ConnectionKind::WirelessDongle24Ghz, Some(battery)).as_deref(),
            Some("15% charging")
        );
        assert_eq!(
            power_label(ConnectionKind::WiredUsb, None).as_deref(),
            Some("Wired")
        );
        assert_eq!(power_label(ConnectionKind::Bluetooth, None), None);
    }
}
