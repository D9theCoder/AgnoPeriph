//! Built-in quick presets. Loading one stages its values as unsaved edits on the selected
//! device, so the user reviews them in the dirty bar and applies or discards them.
//!
//! Every value is snapped onto the device's own limits; presets the device can't express
//! are not offered.

use std::collections::BTreeSet;

use agnoperiph_core::{DpiValue, KeyIndex, PollingRateHz, RapidTriggerConfig, TravelHundredthsMm};

use crate::backend::DeviceSnapshot;
use crate::draft::DeviceDraft;
use crate::hall_effect_panel::snap_travel;
use crate::pointer_panel::snap_dpi;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickPreset {
    FpsLowActuation,
    Typing,
    RapidTrigger,
    LowSensFps,
    Productivity,
}

impl QuickPreset {
    pub const ALL: [Self; 5] = [
        Self::FpsLowActuation,
        Self::Typing,
        Self::RapidTrigger,
        Self::LowSensFps,
        Self::Productivity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::FpsLowActuation => "FPS / Low Actuation",
            Self::Typing => "Typing",
            Self::RapidTrigger => "Rapid Trigger",
            Self::LowSensFps => "Low-Sens FPS",
            Self::Productivity => "Productivity",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::FpsLowActuation => "0.80 mm actuation, Rapid Trigger off",
            Self::Typing => "2.00 mm actuation, Rapid Trigger off",
            Self::RapidTrigger => "1.00 mm actuation, Rapid Trigger 0.20 mm",
            Self::LowSensFps => "400 / 800 / 1600 DPI, 1000 Hz, lowest lift-off",
            Self::Productivity => "800 / 1600 / 3200 DPI, 500 Hz",
        }
    }

    /// Stable fragment for element ids.
    pub fn slug(self) -> &'static str {
        match self {
            Self::FpsLowActuation => "fps",
            Self::Typing => "typing",
            Self::RapidTrigger => "rapid-trigger",
            Self::LowSensFps => "low-sens-fps",
            Self::Productivity => "productivity",
        }
    }

    /// Presets `device` can express, from its capabilities.
    pub fn available(device: &DeviceSnapshot) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|preset| match preset {
                Self::FpsLowActuation | Self::Typing => device.hall_effect.is_some(),
                Self::RapidTrigger => device
                    .hall_effect
                    .as_ref()
                    .is_some_and(|he| he.capabilities.has_rapid_trigger()),
                Self::LowSensFps | Self::Productivity => device.pointer.is_some(),
            })
            .collect()
    }

    /// Stages the preset onto `draft` for `device` (the raw device values).
    pub(crate) fn stage(self, device: &DeviceSnapshot, draft: &mut DeviceDraft) {
        match self {
            Self::FpsLowActuation => stage_keys(device, draft, 80, None),
            Self::Typing => stage_keys(device, draft, 200, None),
            Self::RapidTrigger => stage_keys(device, draft, 100, Some(20)),
            Self::LowSensFps => stage_pointer(device, draft, &[400, 800, 1600], 1000, true),
            Self::Productivity => stage_pointer(device, draft, &[800, 1600, 3200], 500, false),
        }
    }
}

/// Keys whose travel belongs to a SOCD or Rapid Switch pair instead of per-key settings.
pub(crate) fn pair_managed_keys(device: &DeviceSnapshot) -> BTreeSet<KeyIndex> {
    device.hall_effect.as_ref().map_or_default(|he| {
        he.snapshot
            .socd_pairs
            .iter()
            .flat_map(|p| [p.key_a, p.key_b])
            .chain(
                he.snapshot
                    .rapid_switch_pairs
                    .iter()
                    .flat_map(|p| [p.key_a, p.key_b]),
            )
            .collect()
    })
}

fn stage_keys(
    device: &DeviceSnapshot,
    draft: &mut DeviceDraft,
    actuation: i32,
    rapid_trigger: Option<i32>,
) {
    let Some(he) = device.hall_effect.as_ref() else {
        return;
    };
    let caps = &he.capabilities;
    let point = snap_travel(
        actuation,
        caps.actuation_min,
        caps.actuation_max,
        caps.actuation_step,
    );
    let rt = match (
        rapid_trigger,
        caps.rapid_trigger_min,
        caps.rapid_trigger_max,
    ) {
        (Some(sensitivity), Some(min), Some(max)) => {
            let value = snap_travel(sensitivity, min, max, caps.actuation_step);
            RapidTriggerConfig::new(true, caps.supports_continuous_rt, value, value)
        }
        _ => RapidTriggerConfig::disabled(),
    };
    let skip = pair_managed_keys(device);
    for key in he.snapshot.keys.iter().filter(|k| !skip.contains(&k.key)) {
        draft.actuation.insert(key.key, point);
        if caps.has_rapid_trigger() {
            // Keep the device's own disabled encoding when Rapid Trigger stays off.
            let next = if rt.is_enabled() || key.rapid_trigger.is_enabled() {
                rt
            } else {
                key.rapid_trigger
            };
            draft.rapid_trigger.insert(key.key, next);
        }
    }
}

fn stage_pointer(
    device: &DeviceSnapshot,
    draft: &mut DeviceDraft,
    dpi: &[u32],
    polling_hz: u16,
    lowest_lod: bool,
) {
    let Some(pointer) = device.pointer.as_ref() else {
        return;
    };
    let caps = &pointer.capabilities;
    let mut stages = pointer.snapshot.dpi_stages.clone();
    for (stage, value) in stages.iter_mut().zip(dpi) {
        if let Ok(next) = DpiValue::new(snap_dpi(i64::from(*value), caps)) {
            stage.x = next;
            stage.y = None;
        }
    }
    draft.pointer.dpi_stages = Some(stages);
    draft.pointer.polling_rate = closest_rate(&caps.supported_polling_rates, polling_hz);
    draft.pointer.lod = if lowest_lod {
        caps.supported_lods.first().copied()
    } else {
        None
    };
}

/// The supported rate closest to `hz`, preferring the lower one on a tie.
fn closest_rate(rates: &[PollingRateHz], hz: u16) -> Option<PollingRateHz> {
    rates
        .iter()
        .copied()
        .min_by_key(|r| (r.as_hz().abs_diff(hz), r.as_hz()))
}

/// Convenience for tests and callers that want the staged actuation value.
pub fn staged_actuation(
    device: &DeviceSnapshot,
    preset: QuickPreset,
) -> Option<TravelHundredthsMm> {
    let mut draft = DeviceDraft::default();
    preset.stage(device, &mut draft);
    draft.actuation.values().next().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, MockPeripheralService, PeripheralService,
    };
    use agnoperiph_core::DeviceId;

    fn device(id: &str) -> DeviceSnapshot {
        MockPeripheralService::new()
            .refresh()
            .unwrap()
            .into_iter()
            .find(|d| d.id() == &DeviceId::new(id))
            .unwrap()
    }

    #[test]
    fn presets_follow_capabilities() {
        let kb = device(MOCK_KEYBOARD_ID);
        let mouse = device(MOCK_MOUSE_ID);
        assert!(QuickPreset::available(&kb).contains(&QuickPreset::RapidTrigger));
        assert!(!QuickPreset::available(&kb).contains(&QuickPreset::LowSensFps));
        assert_eq!(
            QuickPreset::available(&mouse),
            vec![QuickPreset::LowSensFps, QuickPreset::Productivity]
        );
    }

    #[test]
    fn keyboard_presets_skip_paired_keys_and_snap_to_limits() {
        let kb = device(MOCK_KEYBOARD_ID);
        let mut draft = DeviceDraft::default();
        QuickPreset::RapidTrigger.stage(&kb, &mut draft);
        let paired = pair_managed_keys(&kb);
        assert!(!paired.is_empty());
        assert!(draft.actuation.keys().all(|k| !paired.contains(k)));
        assert!(
            draft
                .rapid_trigger
                .values()
                .all(RapidTriggerConfig::is_enabled)
        );
        assert_eq!(
            staged_actuation(&kb, QuickPreset::Typing),
            Some(TravelHundredthsMm::from_hundredths(200))
        );
    }

    #[test]
    fn mouse_presets_stage_dpi_and_polling() {
        let mouse = device(MOCK_MOUSE_ID);
        let mut draft = DeviceDraft::default();
        QuickPreset::LowSensFps.stage(&mouse, &mut draft);
        let stages = draft.pointer.dpi_stages.as_ref().unwrap();
        assert_eq!(stages[0].x.value(), 400);
        assert_eq!(stages[1].x.value(), 800);
        assert!(draft.pointer.polling_rate.is_some());
    }
}
