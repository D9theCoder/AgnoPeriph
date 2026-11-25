//! Triggers & paired keys: per-key actuation and Rapid Trigger with multi-key selection,
//! keyboard-wide dead zones, and the paired / advanced key functions (RS, SOCD, DKS, MT, TGL).

use std::collections::{BTreeMap, BTreeSet};

use agnoperiph_core::{
    DksBinding, DksStageAction, DksTriggerDirection, KeyCode, KeyIndex, ModTapBinding,
    RapidSwitchPair, RapidTriggerConfig, SocdMode, SocdPair, ToggleKeyBinding, TravelHundredthsMm,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, Theme, WindowExt,
    button::{Button, ButtonGroup, ButtonVariant, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    slider::Slider,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    AgnoPeriphApp, DraftScope, JobOrigin, field, group, mm, muted_note, readout, section,
};
use crate::backend::{ActuationTarget, DeviceSnapshot, HallEffectState, SettingChange};
use crate::controls::{SliderRole, sync_slider, to_units};
use crate::key_matrix::{self, KeyFace, KeyTone};
use crate::keycodes::{HID_KEYS, KeyGroup, hid_name};
use crate::keymap::KeyLayout;

/// Depth "Restore default" writes to the selected keys, with Rapid Trigger off.
///
/// The RS7 reports no factory default; 1.20 mm matches the keyboard's shipped tuning
/// observed on the non-WASD keys of the reference unit.
pub const DEFAULT_ACTUATION: TravelHundredthsMm = TravelHundredthsMm::from_hundredths(120);

/// Paired-key slots per function on keyboards with a paired-key table (RS7: 20 each).
/// No capability reports MT/TGL/RS limits, so the driver's documented table size is used.
const PAIRED_SLOT_LIMIT: usize = 20;

/// Key-class `0x7E` ids `5..=24` bind a key to DKS slot `0..=19` (`docs/protocols/atk-rs7.md`).
const DKS_KEY_CLASS: u8 = 0x7E;
const DKS_KEY_ID_BASE: u8 = 5;

const SOCD_MODES: [(SocdMode, &str); 4] = [
    (SocdMode::LastInputPriority, "Last input"),
    (SocdMode::Neutral, "Neutral"),
    (SocdMode::KeyAPriority, "First key"),
    (SocdMode::KeyBPriority, "Second key"),
];

/// Snaps a travel distance onto `min + k * step` within `min..=max`.
pub fn snap_travel(
    value: i32,
    min: TravelHundredthsMm,
    max: TravelHundredthsMm,
    step: TravelHundredthsMm,
) -> TravelHundredthsMm {
    let min = i32::from(min.as_hundredths());
    let max = i32::from(max.as_hundredths());
    let step = i32::from(step.as_hundredths().max(1));
    let clamped = value.clamp(min, max);
    let mut snapped = min + (clamped - min + step / 2) / step * step;
    while snapped > max {
        snapped -= step;
    }
    TravelHundredthsMm::from_hundredths(u16::try_from(snapped).unwrap_or(0))
}

fn t(hundredths: u16) -> TravelHundredthsMm {
    TravelHundredthsMm::from_hundredths(hundredths)
}

/// Paired / advanced key function being configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PairedMode {
    #[default]
    RapidSwitch,
    Socd,
    Dks,
    ModTap,
    Toggle,
}

impl PairedMode {
    const ALL: [Self; 5] = [
        Self::RapidSwitch,
        Self::Socd,
        Self::Dks,
        Self::ModTap,
        Self::Toggle,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::RapidSwitch => "rs",
            Self::Socd => "socd",
            Self::Dks => "dks",
            Self::ModTap => "mt",
            Self::Toggle => "tgl",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::RapidSwitch => "Rapid Switching",
            Self::Socd => "Instant Release",
            Self::Dks => "Dynamic Keystroke",
            Self::ModTap => "Hold / Click",
            Self::Toggle => "Toggle Switch",
        }
    }

    fn badge(self) -> &'static str {
        match self {
            Self::RapidSwitch => "RS",
            Self::Socd => "SOCD",
            Self::Dks => "DKS",
            Self::ModTap => "MT",
            Self::Toggle => "TGL",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::RapidSwitch => "Two keys; the deeper press wins.",
            Self::Socd => "Two opposing keys; choose which one wins.",
            Self::Dks => "Up to four actions along one key’s travel.",
            Self::ModTap => "Tap for one key, hold for another.",
            Self::Toggle => "Tap to latch a key down, tap again to release.",
        }
    }
}

/// Which slot of a pending flow the next key click fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickTarget {
    First,
    Second,
}

/// One DKS stage being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DksStageDraft {
    pub(crate) enabled: bool,
    pub(crate) travel: TravelHundredthsMm,
    pub(crate) hold: bool,
}

/// Draft for the paired / advanced key flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairedKeysUi {
    pub(crate) mode: PairedMode,
    pub(crate) pick: Option<PickTarget>,
    pub(crate) first: Option<KeyIndex>,
    pub(crate) second: Option<KeyIndex>,
    pub(crate) socd_mode: SocdMode,
    pub(crate) dks_stages: [DksStageDraft; 4],
    pub(crate) primary_usage: u8,
    pub(crate) secondary_usage: u8,
    pub(crate) delay_ms: u16,
    /// Why the last key click was refused, shown next to the pick buttons.
    pub(crate) conflict: Option<SharedString>,
}

impl Default for PairedKeysUi {
    fn default() -> Self {
        Self {
            mode: PairedMode::default(),
            pick: None,
            first: None,
            second: None,
            socd_mode: SocdMode::LastInputPriority,
            dks_stages: [
                DksStageDraft {
                    enabled: true,
                    travel: t(100),
                    hold: false,
                },
                DksStageDraft {
                    enabled: false,
                    travel: t(300),
                    hold: false,
                },
                DksStageDraft {
                    enabled: false,
                    travel: t(300),
                    hold: false,
                },
                DksStageDraft {
                    enabled: true,
                    travel: t(100),
                    hold: false,
                },
            ],
            primary_usage: 0x2C,
            secondary_usage: 0xE1,
            delay_ms: 200,
            conflict: None,
        }
    }
}

impl PairedKeysUi {
    /// Fills the pending pick with `key`; returns `false` when no pick is active.
    pub(crate) fn accept_pick(&mut self, key: KeyIndex) -> bool {
        match self.pick.take() {
            Some(PickTarget::First) => {
                self.first = Some(key);
                if matches!(self.mode, PairedMode::RapidSwitch | PairedMode::Socd)
                    && self.second.is_none()
                {
                    self.pick = Some(PickTarget::Second);
                }
                true
            }
            Some(PickTarget::Second) => {
                self.second = Some(key);
                true
            }
            None => false,
        }
    }

    pub(crate) fn cancel_pick(&mut self) {
        self.pick = None;
        self.conflict = None;
    }

    fn reset_flow(&mut self) {
        self.pick = None;
        self.conflict = None;
        self.first = None;
        self.second = None;
    }

    fn is_picked(&self, key: KeyIndex) -> bool {
        self.first == Some(key) || self.second == Some(key)
    }

    fn dks_stage_direction(ix: usize) -> DksTriggerDirection {
        if ix < 2 {
            DksTriggerDirection::Press
        } else {
            DksTriggerDirection::Release
        }
    }
}

fn dks_stage_label(ix: usize) -> &'static str {
    match ix {
        0 => "Press past",
        1 => "Bottom out",
        2 => "Lift from bottom",
        _ => "Release above",
    }
}

impl AgnoPeriphApp {
    pub(crate) fn hall_effect_state(&self) -> Option<&HallEffectState> {
        self.selected_device()?.hall_effect.as_ref()
    }

    /// The primary key's values with unsaved edits applied.
    pub(crate) fn key_values(&self) -> Option<(TravelHundredthsMm, RapidTriggerConfig)> {
        let config = self.hall_effect_state()?.key(self.selected_key)?;
        Some((
            self.key_draft.actuation.unwrap_or(config.actuation),
            self.key_draft.rapid_trigger.unwrap_or(config.rapid_trigger),
        ))
    }

    fn deadzone_values(&self) -> Option<(TravelHundredthsMm, TravelHundredthsMm)> {
        let he = self.hall_effect_state()?;
        let first = he.snapshot.keys.first();
        Some((
            self.deadzone_draft
                .top
                .or_else(|| first.and_then(|k| k.top_deadzone))
                .unwrap_or(t(0)),
            self.deadzone_draft
                .bottom
                .or_else(|| first.and_then(|k| k.bottom_deadzone))
                .unwrap_or(t(0)),
        ))
    }

    fn selection(&self) -> Vec<KeyIndex> {
        self.selected_keys.iter().copied().collect()
    }

    fn set_actuation_draft(&mut self, value: i32, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let caps = &he.capabilities;
        let next = snap_travel(
            value,
            caps.actuation_min,
            caps.actuation_max,
            caps.actuation_step,
        );
        self.key_draft.actuation = Some(next);
        self.request_sync(cx);
        cx.notify();
    }

    fn step_actuation(&mut self, direction: i32, cx: &mut Context<Self>) {
        let (Some(he), Some((actuation, _))) = (self.hall_effect_state(), self.key_values()) else {
            return;
        };
        let step = i32::from(he.capabilities.actuation_step.as_hundredths().max(1));
        self.set_actuation_draft(i32::from(actuation.as_hundredths()) + direction * step, cx);
    }

    fn apply_actuation(&mut self, cx: &mut Context<Self>) {
        let Some((point, _)) = self.key_values() else {
            return;
        };
        let keys = self.selection();
        let change = match keys.as_slice() {
            [] => return,
            [key] => SettingChange::Actuation {
                target: ActuationTarget::Key(*key),
                point,
            },
            _ => SettingChange::ActuationMany { keys, point },
        };
        self.apply_change(change, cx);
    }

    fn apply_global_actuation(&mut self, cx: &mut Context<Self>) {
        if let Some((point, _)) = self.key_values() {
            self.apply_change(
                SettingChange::Actuation {
                    target: ActuationTarget::AllKeys,
                    point,
                },
                cx,
            );
        }
    }

    fn request_global_actuation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(he), Some((point, _))) = (self.hall_effect_state(), self.key_values()) else {
            return;
        };
        if self.is_dry_run() {
            self.apply_global_actuation(cx);
            return;
        }
        let key_count = he.capabilities.key_count;
        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            alert
                .confirm()
                .title("Write actuation to all keys?")
                .description(format!(
                    "This erases and rewrites the onboard actuation table for all {key_count} keys to {} mm.",
                    mm(point)
                ))
                .ok_text("Write all keys")
                .ok_variant(ButtonVariant::Warning)
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let _ = app.update(cx, super::app::AgnoPeriphApp::apply_global_actuation);
                    true
                })
        });
    }

    fn rapid_trigger_bounds(
        &self,
    ) -> Option<(TravelHundredthsMm, TravelHundredthsMm, TravelHundredthsMm)> {
        let caps = &self.hall_effect_state()?.capabilities;
        Some((
            caps.rapid_trigger_min?,
            caps.rapid_trigger_max?,
            caps.actuation_step,
        ))
    }

    fn set_rapid_trigger_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let (Some((_, config)), Some((min, max, step))) =
            (self.key_values(), self.rapid_trigger_bounds())
        else {
            return;
        };
        let continuous = enabled
            && self
                .hall_effect_state()
                .is_some_and(|he| he.capabilities.supports_continuous_rt);
        // Disabled keys may report 0 mm sensitivities, which the device rejects once enabled.
        let press = snap_travel(
            i32::from(config.press_sensitivity.as_hundredths()),
            min,
            max,
            step,
        );
        let release = snap_travel(
            i32::from(config.release_sensitivity.as_hundredths()),
            min,
            max,
            step,
        );
        self.key_draft.rapid_trigger =
            Some(RapidTriggerConfig::new(enabled, continuous, press, release));
        self.request_sync(cx);
        cx.notify();
    }

    fn set_sensitivity(&mut self, is_press: bool, value: i32, cx: &mut Context<Self>) {
        let (Some((_, mut config)), Some((min, max, step))) =
            (self.key_values(), self.rapid_trigger_bounds())
        else {
            return;
        };
        let next = snap_travel(value, min, max, step);
        if is_press {
            config.press_sensitivity = next;
        } else {
            config.release_sensitivity = next;
        }
        self.key_draft.rapid_trigger = Some(config);
        self.request_sync(cx);
        cx.notify();
    }

    fn step_sensitivity(&mut self, is_press: bool, direction: i32, cx: &mut Context<Self>) {
        let (Some((_, config)), Some((_, _, step))) =
            (self.key_values(), self.rapid_trigger_bounds())
        else {
            return;
        };
        let current = if is_press {
            config.press_sensitivity
        } else {
            config.release_sensitivity
        };
        let delta = direction * i32::from(step.as_hundredths().max(1));
        self.set_sensitivity(is_press, i32::from(current.as_hundredths()) + delta, cx);
    }

    fn apply_rapid_trigger(&mut self, cx: &mut Context<Self>) {
        let Some((_, config)) = self.key_values() else {
            return;
        };
        let keys = self.selection();
        let change = match keys.as_slice() {
            [] => return,
            [key] => SettingChange::RapidTrigger { key: *key, config },
            _ => SettingChange::RapidTriggerMany { keys, config },
        };
        self.apply_change(change, cx);
    }

    fn discard_key_draft(&mut self, cx: &mut Context<Self>) {
        self.key_draft = crate::app::KeyDraft::default();
        self.request_sync(cx);
        cx.notify();
    }

    fn step_deadzone(&mut self, is_top: bool, direction: i32, cx: &mut Context<Self>) {
        let Some((top, bottom)) = self.deadzone_values() else {
            return;
        };
        let current = if is_top { top } else { bottom };
        self.set_deadzone(is_top, i32::from(current.as_hundredths()) + direction, cx);
    }

    fn set_deadzone(&mut self, is_top: bool, value: i32, cx: &mut Context<Self>) {
        let next = snap_travel(value, t(0), t(100), t(1));
        if is_top {
            self.deadzone_draft.top = Some(next);
        } else {
            self.deadzone_draft.bottom = Some(next);
        }
        self.request_sync(cx);
        cx.notify();
    }

    fn apply_deadzones(&mut self, cx: &mut Context<Self>) {
        if let Some((top, bottom)) = self.deadzone_values() {
            self.apply_change(SettingChange::Deadzones { top, bottom }, cx);
        }
    }

    fn layout_keys(&self) -> Vec<KeyIndex> {
        let Some(device) = self.selected_device() else {
            return Vec::new();
        };
        let Some(he) = device.hall_effect.as_ref() else {
            return Vec::new();
        };
        KeyLayout::for_device(&device.info, &he.capabilities)
            .rows
            .iter()
            .flat_map(|row| row.iter().map(|cap| cap.index))
            .collect()
    }

    fn select_all_keys(&mut self, cx: &mut Context<Self>) {
        self.selected_keys = self.layout_keys().into_iter().collect();
        self.key_draft = crate::app::KeyDraft::default();
        self.request_sync(cx);
        cx.notify();
    }

    fn invert_key_selection(&mut self, cx: &mut Context<Self>) {
        let all: BTreeSet<KeyIndex> = self.layout_keys().into_iter().collect();
        self.selected_keys = all.difference(&self.selected_keys).copied().collect();
        if let Some(first) = self.selected_keys.iter().next() {
            self.selected_key = *first;
        }
        self.key_draft = crate::app::KeyDraft::default();
        self.request_sync(cx);
        cx.notify();
    }

    fn clear_key_selection(&mut self, cx: &mut Context<Self>) {
        self.selected_keys.clear();
        self.key_draft = crate::app::KeyDraft::default();
        cx.notify();
    }

    fn restore_default_keys(&mut self, cx: &mut Context<Self>) {
        let keys = self.selection();
        if keys.is_empty() {
            return;
        }
        self.key_draft = crate::app::KeyDraft::default();
        self.apply_changes(
            vec![
                SettingChange::ActuationMany {
                    keys: keys.clone(),
                    point: DEFAULT_ACTUATION,
                },
                SettingChange::RapidTriggerMany {
                    keys,
                    config: RapidTriggerConfig::disabled(),
                },
            ],
            JobOrigin::User(DraftScope::None),
            cx,
        );
    }

    // ---- Paired keys ------------------------------------------------------------------

    fn set_paired_mode(&mut self, mode: PairedMode, cx: &mut Context<Self>) {
        if self.paired.mode != mode {
            self.paired.mode = mode;
            self.paired.reset_flow();
            cx.notify();
        }
    }

    fn start_pick(&mut self, target: PickTarget, cx: &mut Context<Self>) {
        self.paired.pick = Some(target);
        self.paired.conflict = None;
        cx.notify();
    }

    fn cancel_paired_flow(&mut self, cx: &mut Context<Self>) {
        self.paired.reset_flow();
        cx.notify();
    }

    fn add_paired(&mut self, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let snapshot = he.snapshot.clone();
        let ui = self.paired.clone();
        let Some(first) = ui.first else {
            return;
        };
        if let Some(device) = self.selected_device() {
            let occupied = crate::backend::occupied_advanced_keys(device);
            if [Some(first), ui.second]
                .into_iter()
                .flatten()
                .any(|key| occupied.contains_key(&key))
            {
                return;
            }
        }
        let changes = match ui.mode {
            PairedMode::RapidSwitch => {
                let Some(second) = ui.second else {
                    return;
                };
                let Ok(pair) = RapidSwitchPair::new(first, second) else {
                    return;
                };
                let mut pairs = snapshot.rapid_switch_pairs;
                pairs.push(pair);
                vec![SettingChange::RapidSwitch(pairs)]
            }
            PairedMode::Socd => {
                let Some(second) = ui.second else {
                    return;
                };
                let Ok(pair) = SocdPair::new(first, second, ui.socd_mode) else {
                    return;
                };
                let mut pairs = snapshot.socd_pairs;
                pairs.push(pair);
                vec![SettingChange::Socd(pairs)]
            }
            PairedMode::Dks => {
                let Some(slot) = free_slot(snapshot.dks_bindings.iter().map(|b| b.key.value()))
                else {
                    return;
                };
                let stages = ui.dks_stages.map(|_| None);
                let mut stages: [Option<DksStageAction>; 4] = stages;
                for (ix, stage) in ui.dks_stages.iter().enumerate() {
                    if stage.enabled {
                        stages[ix] = Some(DksStageAction::new(
                            PairedKeysUi::dks_stage_direction(ix),
                            stage.travel,
                            u16::from(ui.primary_usage),
                            stage.hold,
                        ));
                    }
                }
                let Ok(slot_u8) = u8::try_from(slot) else {
                    return;
                };
                vec![
                    SettingChange::Dks(DksBinding::new(KeyIndex::new(slot), stages)),
                    SettingChange::KeyBinding {
                        layer: 0,
                        key: first,
                        code: KeyCode::new(DKS_KEY_CLASS, DKS_KEY_ID_BASE + slot_u8),
                    },
                ]
            }
            PairedMode::ModTap => {
                let Some(slot) =
                    free_slot(snapshot.mod_tap_bindings.iter().map(|b| u16::from(b.slot)))
                else {
                    return;
                };
                let Ok(slot) = u8::try_from(slot) else {
                    return;
                };
                vec![SettingChange::ModTap(ModTapBinding::new(
                    slot,
                    first,
                    (0, ui.primary_usage),
                    (0, ui.secondary_usage),
                    ui.delay_ms,
                ))]
            }
            PairedMode::Toggle => {
                let Some(slot) =
                    free_slot(snapshot.toggle_bindings.iter().map(|b| u16::from(b.slot)))
                else {
                    return;
                };
                let Ok(slot) = u8::try_from(slot) else {
                    return;
                };
                vec![SettingChange::ToggleKey(ToggleKeyBinding::new(
                    slot,
                    first,
                    (0, ui.primary_usage),
                    ui.delay_ms,
                ))]
            }
        };
        self.paired.reset_flow();
        self.apply_changes(changes, JobOrigin::User(DraftScope::None), cx);
    }

    fn remove_paired(&mut self, entry: PairedEntry, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let snapshot = &he.snapshot;
        let change = match entry {
            PairedEntry::RapidSwitch(target) => SettingChange::RapidSwitch(
                snapshot
                    .rapid_switch_pairs
                    .iter()
                    .copied()
                    .filter(|p| *p != target)
                    .collect(),
            ),
            PairedEntry::Socd(target) => SettingChange::Socd(
                snapshot
                    .socd_pairs
                    .iter()
                    .copied()
                    .filter(|p| !(p.key_a == target.key_a && p.key_b == target.key_b))
                    .collect(),
            ),
            PairedEntry::Dks(slot) => SettingChange::ClearDks { slot },
            PairedEntry::ModTap { slot, key } => SettingChange::ClearModTap { slot, key },
            PairedEntry::Toggle { slot, key } => SettingChange::ClearToggleKey { slot, key },
        };
        self.apply_change(change, cx);
    }

    fn set_socd_mode(&mut self, target: SocdPair, mode: SocdMode, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let pairs = he
            .snapshot
            .socd_pairs
            .iter()
            .map(|pair| {
                if pair.key_a == target.key_a && pair.key_b == target.key_b {
                    SocdPair::new(pair.key_a, pair.key_b, mode).unwrap_or(*pair)
                } else {
                    *pair
                }
            })
            .collect();
        self.apply_change(SettingChange::Socd(pairs), cx);
    }

    pub(crate) fn set_paired_usage(&mut self, is_primary: bool, usage: u8, cx: &mut Context<Self>) {
        if is_primary {
            self.paired.primary_usage = usage;
        } else {
            self.paired.secondary_usage = usage;
        }
        cx.notify();
    }
}

/// Lowest free slot below the paired-key limit.
fn free_slot(used: impl Iterator<Item = u16>) -> Option<u16> {
    let used: BTreeSet<u16> = used.collect();
    (0..u16::try_from(PAIRED_SLOT_LIMIT).unwrap_or(20)).find(|slot| !used.contains(slot))
}

/// One configured paired / advanced key entry, for removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairedEntry {
    RapidSwitch(RapidSwitchPair),
    Socd(SocdPair),
    Dks(KeyIndex),
    ModTap { slot: u8, key: KeyIndex },
    Toggle { slot: u8, key: KeyIndex },
}

pub(crate) fn sync_controls(
    app: &mut AgnoPeriphApp,
    window: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let Some(controls) = app.controls.as_ref() else {
        return;
    };
    let (travel, rt_press, rt_release, dz_top, dz_bottom) = (
        controls.travel.clone(),
        controls.rt_press.clone(),
        controls.rt_release.clone(),
        controls.deadzone_top.clone(),
        controls.deadzone_bottom.clone(),
    );
    if let Some((actuation, rt)) = app.key_values() {
        sync_slider(&travel, f32::from(actuation.as_hundredths()), window, cx);
        sync_slider(
            &rt_press,
            f32::from(rt.press_sensitivity.as_hundredths()),
            window,
            cx,
        );
        sync_slider(
            &rt_release,
            f32::from(rt.release_sensitivity.as_hundredths()),
            window,
            cx,
        );
    }
    if let Some((top, bottom)) = app.deadzone_values() {
        sync_slider(&dz_top, f32::from(top.as_hundredths()), window, cx);
        sync_slider(&dz_bottom, f32::from(bottom.as_hundredths()), window, cx);
    }
}

pub(crate) fn on_slider(
    app: &mut AgnoPeriphApp,
    role: SliderRole,
    value: f32,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let units = i32::from(to_units(value));
    match role {
        SliderRole::Travel => app.set_actuation_draft(units, cx),
        SliderRole::RtPress => app.set_sensitivity(true, units, cx),
        SliderRole::RtRelease => app.set_sensitivity(false, units, cx),
        SliderRole::DeadzoneTop => app.set_deadzone(true, units, cx),
        SliderRole::DeadzoneBottom => app.set_deadzone(false, units, cx),
        _ => {}
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let Some(he) = device.hall_effect.as_ref() else {
        return section(
            "hall-effect-panel",
            "Triggers",
            "The keyboard reports Hall Effect switches, but their settings couldn’t be read.",
            cx.theme(),
        );
    };
    let layout = KeyLayout::for_device(&device.info, &he.capabilities);
    let occupied = crate::backend::occupied_advanced_keys(device);
    let has_paired = he.capabilities.has_socd() || he.capabilities.has_dks();

    v_flex()
        .id("hall-effect-panel")
        .gap_4()
        .child(
            h_flex()
                .items_start()
                .gap_4()
                .child(render_matrix(app, he, &layout, &occupied, cx))
                .child(render_selection_rail(app, &layout, cx)),
        )
        .child(render_deck(app, he, &layout, cx))
        .when(has_paired, |this| {
            this.child(render_paired(app, he, &layout, &occupied, cx))
        })
}

/// Selection commands beside the matrix, as in ATK HUB: what is selected, then the
/// bulk selection tools, then the one command that writes.
fn render_selection_rail(
    app: &AgnoPeriphApp,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let busy = app.is_busy();
    let count = app.selected_keys().len();
    let summary: SharedString = match count {
        0 => "No keys selected".into(),
        1 => format!("{} selected", layout.label(app.selected_key())).into(),
        n => format!("{n} keys selected").into(),
    };
    let theme = cx.theme();
    v_flex()
        .id("selection-rail")
        .flex_1()
        .min_w_32()
        .max_w_48()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(summary),
        )
        .child(muted_note("Click keys to select or deselect them.", theme))
        .child(div().h_2())
        .child(
            Button::new("select-all-keys")
                .ghost()
                .small()
                .w_full()
                .justify_start()
                .label("Select all")
                .on_click(cx.listener(|this, _, _, cx| this.select_all_keys(cx))),
        )
        .child(
            Button::new("invert-keys")
                .ghost()
                .small()
                .w_full()
                .justify_start()
                .label("Invert")
                .on_click(cx.listener(|this, _, _, cx| this.invert_key_selection(cx))),
        )
        .child(
            Button::new("clear-keys")
                .ghost()
                .small()
                .w_full()
                .justify_start()
                .label("Clear selection")
                .disabled(count == 0)
                .on_click(cx.listener(|this, _, _, cx| this.clear_key_selection(cx))),
        )
        .child(div().my_2().h_px().w_full().bg(theme.border))
        .child(
            Button::new("restore-default-keys")
                .outline()
                .small()
                .w_full()
                .label("Restore default")
                .tooltip(format!(
                    "Set the selected keys to {} mm with Rapid Trigger off",
                    mm(DEFAULT_ACTUATION)
                ))
                .disabled(busy || count == 0)
                .on_click(cx.listener(|this, _, _, cx| this.restore_default_keys(cx))),
        )
}

fn trigger_select(
    app: &mut AgnoPeriphApp,
    key: KeyIndex,
    _extend: bool,
    _: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    app.toggle_key(key, cx);
}

fn render_matrix(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    occupied: &BTreeMap<KeyIndex, &'static str>,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let max = f32::from(he.capabilities.actuation_max.as_hundredths().max(1));
    let selected = app.selected_keys();
    let paired = &app.paired;
    let picking = paired.pick.is_some();
    let keys = &he.snapshot.keys;
    let rt_count = keys.iter().filter(|k| k.rapid_trigger.is_enabled()).count();
    let matrix = key_matrix::render(
        "key-matrix",
        "key",
        layout,
        |cap| {
            let config = keys.iter().find(|k| k.key == cap.index);
            let owner = occupied.get(&cap.index).copied();
            let tone = if paired.is_picked(cap.index) {
                KeyTone::Picked
            } else if picking && owner.is_some() {
                KeyTone::Locked
            } else if !picking && selected.contains(&cap.index) {
                KeyTone::Selected
            } else {
                KeyTone::Normal
            };
            KeyFace {
                detail: Some(config.map_or_else(|| "–".into(), |c| mm(c.actuation).into())),
                fill: Some(config.map_or(0.0, |c| f32::from(c.actuation.as_hundredths()) / max)),
                marker: config.is_some_and(|c| c.rapid_trigger.is_enabled()),
                badge: owner,
                tone,
            }
        },
        trigger_select,
        cx,
    );
    let theme = cx.theme();
    v_flex().flex_shrink_0().gap_1p5().child(matrix).child(
        h_flex()
            .gap_4()
            .text_xs()
            .text_color(theme.muted_foreground)
            .when(!picking, |this| {
                this.child(
                    h_flex()
                        .gap_1p5()
                        .child(div().w_4().h_1().rounded_sm().bg(theme.primary))
                        .child("Bar shows actuation depth"),
                )
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(div().size_1p5().rounded_full().bg(theme.primary))
                        .child(format!("Rapid Trigger on {rt_count} keys")),
                )
            })
            .when(picking, |this| {
                this.child(
                    div()
                        .id("paired-pick-hint")
                        .text_color(theme.warning)
                        .child("Click a key to pick it. Dimmed keys already belong to another function.")
                        .test_support(),
                )
            }),
    )
}

/// Vertical travel gauge: where the primary key actuates, its Rapid Trigger band and
/// the keyboard's dead zones, read top (rest) to bottom (bottomed out).
fn render_gauge(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let max = f32::from(he.capabilities.actuation_max.as_hundredths().max(1));
    let (actuation, rt) = app.key_values().unwrap_or((
        he.capabilities.actuation_min,
        RapidTriggerConfig::disabled(),
    ));
    let (top, bottom) = app.deadzone_values().unwrap_or((t(0), t(0)));
    let frac = |v: TravelHundredthsMm| (f32::from(v.as_hundredths()) / max).clamp(0.0, 1.0);
    let depth = frac(actuation);
    let band = rt.is_enabled().then(|| {
        let start = (f32::from(actuation.as_hundredths())
            - f32::from(rt.release_sensitivity.as_hundredths()))
            / max;
        let end = (f32::from(actuation.as_hundredths())
            + f32::from(rt.press_sensitivity.as_hundredths()))
            / max;
        (start.clamp(0.0, 1.0), end.clamp(0.0, 1.0))
    });

    v_flex()
        .id("travel-gauge")
        .w_24()
        .flex_shrink_0()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Trigger demo"),
        )
        .child(
            h_flex()
                .items_stretch()
                .gap_2()
                .h_40()
                .child(
                    v_flex()
                        .justify_between()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child("0.00")
                        .child(mm(he.capabilities.actuation_max)),
                )
                .child(
                    div()
                        .relative()
                        .w_5()
                        .h_full()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(relative(frac(top)))
                                .bg(theme.border),
                        )
                        .child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(relative(frac(bottom)))
                                .bg(theme.border),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(relative(depth))
                                .rounded(theme.radius)
                                .bg(theme.primary.opacity(0.35)),
                        )
                        .when_some(band, |this, (start, end)| {
                            this.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top(relative(start))
                                    .h(relative((end - start).max(0.01)))
                                    .bg(theme.warning.opacity(0.45)),
                            )
                        })
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(relative(depth))
                                .h_0p5()
                                .bg(theme.primary),
                        ),
                ),
        )
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .font_family(theme.mono_font_family.clone())
                .child(format!("{} mm", mm(actuation))),
        )
        .child(muted_note(
            if rt.is_enabled() {
                format!(
                    "RT ↓{} ↑{}",
                    mm(rt.press_sensitivity),
                    mm(rt.release_sensitivity)
                )
            } else {
                "Rapid Trigger off".to_owned()
            },
            theme,
        ))
}

fn stepper<F>(
    prefix: &'static str,
    value: TravelHundredthsMm,
    disabled: bool,
    on_step: F,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<F>
where
    F: Fn(&mut AgnoPeriphApp, i32, &mut Context<AgnoPeriphApp>) + Copy + 'static,
{
    h_flex()
        .flex_shrink_0()
        .gap_1()
        .child(
            Button::new(SharedString::from(format!("{prefix}-dec")))
                .outline()
                .xsmall()
                .label("−")
                .accessibility_label(format!("Decrease {prefix}"))
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| on_step(this, -1, cx))),
        )
        .child(readout(format!("{} mm", mm(value)), cx.theme()))
        .child(
            Button::new(SharedString::from(format!("{prefix}-inc")))
                .outline()
                .xsmall()
                .label("+")
                .accessibility_label(format!("Increase {prefix}"))
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| on_step(this, 1, cx))),
        )
}

fn slider_row(
    label: &'static str,
    slider: Option<Entity<gpui_kit::component::slider::SliderState>>,
    control: impl IntoElement,
    disabled: bool,
    theme: &Theme,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .min_w_0()
        .child(
            h_flex()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(control),
        )
        .when_some(slider, |this, slider| {
            this.child(
                Slider::new(&slider)
                    .disabled(disabled)
                    .accessibility_label(label),
            )
        })
}

/// Column title with room for a trailing control, shared by the deck's columns.
fn deck_title(title: impl Into<SharedString>, theme: &Theme) -> Div {
    h_flex().gap_2().justify_between().min_h_6().child(
        div()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.foreground)
            .child(title.into()),
    )
}

/// The bottom deck from ATK HUB: gauge, travel + dead zones, Rapid Trigger.
/// One surface with hairline dividers instead of nested cards.
fn render_deck(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let gauge = render_gauge(app, he, cx);
    let travel = render_travel_column(app, he, layout, cx);
    let rapid = he
        .capabilities
        .has_rapid_trigger()
        .then(|| render_rapid_trigger_column(app, cx));
    let theme = cx.theme();
    let divider = || div().w_px().flex_shrink_0().bg(theme.border);
    h_flex()
        .id("trigger-deck")
        .items_stretch()
        .gap_4()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .child(gauge)
        .child(divider())
        .child(travel)
        .when_some(rapid, |this, rapid| this.child(divider()).child(rapid))
}

fn render_travel_column(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let count = app.selected_keys().len();
    let busy = app.is_busy();
    let locked = busy || count == 0;
    let (actuation, _) = app.key_values().unwrap_or((
        he.capabilities.actuation_min,
        RapidTriggerConfig::disabled(),
    ));
    let caps = &he.capabilities;
    let range_note = format!(
        "{}–{} mm, {} mm steps",
        mm(caps.actuation_min),
        mm(caps.actuation_max),
        mm(caps.actuation_step)
    );
    let controls = app.controls.as_ref();
    let travel_slider = controls.map(|c| c.travel.clone());
    let top_slider = controls.map(|c| c.deadzone_top.clone());
    let bottom_slider = controls.map(|c| c.deadzone_bottom.clone());
    let (top, bottom) = app.deadzone_values().unwrap_or((t(0), t(0)));
    let has_deadzone_draft = app.deadzone_draft != crate::app::DeadzoneDraft::default();
    let target: SharedString = match count {
        0 => "Trigger travel".into(),
        1 => format!("Trigger travel: {}", layout.label(app.selected_key())).into(),
        n => format!("Trigger travel: {n} keys").into(),
    };
    let apply_label = match count {
        0 | 1 => "Apply to key".to_owned(),
        n => format!("Apply to {n} keys"),
    };

    let actuation_stepper = stepper(
        "actuation",
        actuation,
        locked,
        AgnoPeriphApp::step_actuation,
        cx,
    );
    let top_stepper = stepper(
        "deadzone-top",
        top,
        busy,
        |this, direction, cx| this.step_deadzone(true, direction, cx),
        cx,
    );
    let bottom_stepper = stepper(
        "deadzone-bottom",
        bottom,
        busy,
        |this, direction, cx| this.step_deadzone(false, direction, cx),
        cx,
    );
    let theme = cx.theme();

    v_flex()
        .id("trigger-travel")
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(deck_title(target, theme))
        .child(slider_row(
            "Actuation point",
            travel_slider,
            actuation_stepper,
            locked,
            theme,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(muted_note(range_note, theme)),
                )
                .child(
                    Button::new("apply-actuation-all")
                        .ghost()
                        .small()
                        .label("Apply to all keys")
                        .disabled(locked)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.request_global_actuation(window, cx);
                        })),
                )
                .child(
                    Button::new("apply-actuation-key")
                        .primary()
                        .small()
                        .label(apply_label)
                        .loading(busy)
                        .disabled(count == 0 || app.key_draft.actuation.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.apply_actuation(cx))),
                ),
        )
        .child(div().h_px().my_1().w_full().bg(theme.border))
        .child(
            deck_title("Dead zones, all keys", theme).child(
                Button::new("apply-deadzones")
                    .outline()
                    .small()
                    .label("Apply dead zones")
                    .loading(busy)
                    .disabled(!has_deadzone_draft)
                    .on_click(cx.listener(|this, _, _, cx| this.apply_deadzones(cx))),
            ),
        )
        .child(
            h_flex()
                .items_start()
                .gap_4()
                .child(div().flex_1().min_w_0().child(slider_row(
                    "Top",
                    top_slider,
                    top_stepper,
                    busy,
                    theme,
                )))
                .child(div().flex_1().min_w_0().child(slider_row(
                    "Bottom",
                    bottom_slider,
                    bottom_stepper,
                    busy,
                    theme,
                ))),
        )
}

fn render_rapid_trigger_column(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let count = app.selected_keys().len();
    let busy = app.is_busy();
    let locked = busy || count == 0;
    let rapid_trigger = app
        .key_values()
        .map_or(RapidTriggerConfig::disabled(), |(_, rt)| rt);
    let rt_enabled = rapid_trigger.is_enabled();
    let has_draft = app.key_draft != crate::app::KeyDraft::default();
    let controls = app.controls.as_ref();
    let press_slider = controls.map(|c| c.rt_press.clone());
    let release_slider = controls.map(|c| c.rt_release.clone());
    let press = stepper(
        "press-sensitivity",
        rapid_trigger.press_sensitivity,
        locked || !rt_enabled,
        |this, direction, cx| this.step_sensitivity(true, direction, cx),
        cx,
    );
    let release = stepper(
        "release-sensitivity",
        rapid_trigger.release_sensitivity,
        locked || !rt_enabled,
        |this, direction, cx| this.step_sensitivity(false, direction, cx),
        cx,
    );
    let apply_label = match count {
        0 | 1 => "Apply Rapid Trigger".to_owned(),
        n => format!("Apply to {n} keys"),
    };
    let theme = cx.theme();
    v_flex()
        .id("rapid-trigger-group")
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(
            deck_title("Rapid Trigger", theme).child(
                Switch::new("rapid-trigger")
                    .checked(rt_enabled)
                    .disabled(locked)
                    .tooltip("Re-trigger whenever the key changes direction")
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_rapid_trigger_enabled(*checked, cx);
                    })),
            ),
        )
        .child(slider_row(
            "Press sensitivity",
            press_slider,
            press,
            locked || !rt_enabled,
            theme,
        ))
        .child(slider_row(
            "Lift sensitivity",
            release_slider,
            release,
            locked || !rt_enabled,
            theme,
        ))
        .child(
            h_flex()
                .gap_2()
                .justify_end()
                .when(has_draft, |this| {
                    this.child(
                        Button::new("discard-key-draft")
                            .ghost()
                            .small()
                            .label("Discard edits")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.discard_key_draft(cx))),
                    )
                })
                .child(
                    Button::new("apply-rapid-trigger")
                        .primary()
                        .small()
                        .label(apply_label)
                        .loading(busy)
                        .disabled(count == 0 || app.key_draft.rapid_trigger.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.apply_rapid_trigger(cx))),
                ),
        )
}

/// Button that opens a grouped HID key menu and passes the chosen usage to `on_pick`.
pub(crate) fn key_chooser(
    id: impl Into<ElementId>,
    usage: u8,
    disabled: bool,
    on_pick: impl Fn(&mut AgnoPeriphApp, u8, &mut Context<AgnoPeriphApp>) + 'static,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let app = cx.entity().downgrade();
    let on_pick = std::rc::Rc::new(on_pick);
    Button::new(id)
        .outline()
        .small()
        .label(hid_name(u16::from(usage)))
        .dropdown_caret(true)
        .disabled(disabled)
        .dropdown_menu(move |mut menu, window, cx| {
            menu = menu.scrollable(true).max_h(px(360.));
            for group in KeyGroup::ALL {
                let app = app.clone();
                let on_pick = on_pick.clone();
                menu = menu.submenu(group.label(), window, cx, move |mut sub, _, _| {
                    for key in HID_KEYS.iter().filter(|k| k.group == group) {
                        let app = app.clone();
                        let on_pick = on_pick.clone();
                        let code = key.usage;
                        sub = sub.item(
                            PopupMenuItem::new(key.name)
                                .checked(code == usage)
                                .on_click(move |_, _, cx| {
                                    let _ = app.update(cx, |this, cx| on_pick(this, code, cx));
                                }),
                        );
                    }
                    sub
                });
            }
            menu
        })
        .into_any_element()
}

fn render_paired(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    occupied: &BTreeMap<KeyIndex, &'static str>,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let mode = app.paired.mode;
    let snapshot = &he.snapshot;
    let counts = [
        (PairedMode::RapidSwitch, snapshot.rapid_switch_pairs.len()),
        (PairedMode::Socd, snapshot.socd_pairs.len()),
        (PairedMode::Dks, snapshot.dks_bindings.len()),
        (PairedMode::ModTap, snapshot.mod_tap_bindings.len()),
        (PairedMode::Toggle, snapshot.toggle_bindings.len()),
    ];
    let caps = &he.capabilities;
    let pills: Vec<AnyElement> = PairedMode::ALL
        .iter()
        .filter(|m| match m {
            PairedMode::Socd => caps.has_socd(),
            PairedMode::Dks => caps.has_dks(),
            _ => true,
        })
        .map(|&m| {
            let used = counts.iter().find(|(c, _)| *c == m).map_or(0, |(_, n)| *n);
            let theme = cx.theme();
            Button::new(SharedString::from(format!("paired-mode-{}", m.id())))
                .outline()
                .small()
                .selected(m == mode)
                .flex_1()
                .min_w_0()
                .tooltip(m.summary())
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .font_family(theme.mono_font_family.clone())
                                .child(m.badge()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(m.title()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{used}/{PAIRED_SLOT_LIMIT}")),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_paired_mode(m, cx)))
                .into_any_element()
        })
        .collect();

    let flow = render_flow(app, layout, occupied, cx);
    let list = render_configured(app, he, layout, cx);
    let theme = cx.theme();
    v_flex()
        .id("paired-keys")
        .gap_3()
        .pt_4()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Paired & advanced keys"),
                )
                .child(muted_note(
                    "Each key can belong to one function at a time.",
                    theme,
                )),
        )
        .child(h_flex().gap_2().children(pills))
        .child(h_flex().items_start().gap_4().child(flow).child(list))
}

#[allow(clippy::too_many_arguments)]
fn pick_button(
    id: &'static str,
    label: &'static str,
    picked: Option<KeyIndex>,
    is_waiting: bool,
    target: PickTarget,
    layout: &KeyLayout,
    disabled: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let text: SharedString = match (is_waiting, picked) {
        (true, _) => "Click a key…".into(),
        (false, Some(key)) => layout.label(key),
        (false, None) => "Pick key".into(),
    };
    let theme = cx.theme();
    field(label, theme).child(
        Button::new(id)
            .outline()
            .small()
            .selected(is_waiting)
            .label(text)
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, _, cx| this.start_pick(target, cx))),
    )
}

#[allow(clippy::too_many_lines)]
fn render_flow(
    app: &AgnoPeriphApp,
    layout: &KeyLayout,
    occupied: &BTreeMap<KeyIndex, &'static str>,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let ui = app.paired.clone();
    let busy = app.is_busy();
    let mode = ui.mode;
    let needs_two = matches!(mode, PairedMode::RapidSwitch | PairedMode::Socd);
    // Picks are refused for taken keys, but a refresh can take a key after it was picked.
    let taken = [ui.first, ui.second.filter(|_| needs_two)]
        .into_iter()
        .flatten()
        .find_map(|key| occupied.get(&key).map(|owner| (key, *owner)));
    let conflict: Option<SharedString> = ui.conflict.clone().or_else(|| {
        taken.map(|(key, owner)| {
            format!(
                "{} is already used by {owner}. Remove it there first.",
                layout.label(key)
            )
            .into()
        })
    });
    let ready = taken.is_none()
        && ui.first.is_some()
        && (!needs_two || ui.second.is_some_and(|second| Some(second) != ui.first));
    let first_label = if needs_two { "First key" } else { "Key" };
    let mut body = v_flex().gap_2().child(pick_button(
        "paired-pick-first",
        first_label,
        ui.first,
        ui.pick == Some(PickTarget::First),
        PickTarget::First,
        layout,
        busy,
        cx,
    ));
    if needs_two {
        body = body.child(pick_button(
            "paired-pick-second",
            "Second key",
            ui.second,
            ui.pick == Some(PickTarget::Second),
            PickTarget::Second,
            layout,
            busy,
            cx,
        ));
    }
    match mode {
        PairedMode::Socd => {
            let buttons = SOCD_MODES.iter().map(|&(socd, label)| {
                Button::new(SharedString::from(format!("paired-socd-{label}")))
                    .label(label)
                    .selected(ui.socd_mode == socd)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.paired.socd_mode = socd;
                        cx.notify();
                    }))
            });
            body = body.child(
                field("Winner", cx.theme()).child(
                    ButtonGroup::new("paired-socd-modes")
                        .outline()
                        .xsmall()
                        .children(buttons),
                ),
            );
        }
        PairedMode::Dks => {
            body = body.child(field("Sends", cx.theme()).child(key_chooser(
                "paired-key-primary",
                ui.primary_usage,
                busy,
                |this, code, cx| this.set_paired_usage(true, code, cx),
                cx,
            )));
            for (ix, stage) in ui.dks_stages.iter().enumerate() {
                let stage = *stage;
                let travel = stepper(
                    match ix {
                        0 => "dks-stage-0",
                        1 => "dks-stage-1",
                        2 => "dks-stage-2",
                        _ => "dks-stage-3",
                    },
                    stage.travel,
                    busy || !stage.enabled,
                    move |this, direction, cx| {
                        let next = snap_travel(
                            i32::from(this.paired.dks_stages[ix].travel.as_hundredths())
                                + direction * 10,
                            t(10),
                            t(340),
                            t(10),
                        );
                        this.paired.dks_stages[ix].travel = next;
                        cx.notify();
                    },
                    cx,
                );
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(
                            Checkbox::new(("dks-stage-enabled", ix))
                                .label(dks_stage_label(ix))
                                .checked(stage.enabled)
                                .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.paired.dks_stages[ix].enabled = *checked;
                                    cx.notify();
                                }))
                                .w_40(),
                        )
                        .child(travel)
                        .child(
                            Checkbox::new(("dks-stage-hold", ix))
                                .label("Hold")
                                .checked(stage.hold)
                                .disabled(!stage.enabled)
                                .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.paired.dks_stages[ix].hold = *checked;
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        PairedMode::ModTap | PairedMode::Toggle => {
            let primary_label = if mode == PairedMode::ModTap {
                "Tap sends"
            } else {
                "Latches"
            };
            body = body.child(field(primary_label, cx.theme()).child(key_chooser(
                "paired-key-primary",
                ui.primary_usage,
                busy,
                |this, code, cx| this.set_paired_usage(true, code, cx),
                cx,
            )));
            if mode == PairedMode::ModTap {
                body = body.child(field("Hold sends", cx.theme()).child(key_chooser(
                    "paired-key-secondary",
                    ui.secondary_usage,
                    busy,
                    |this, code, cx| this.set_paired_usage(false, code, cx),
                    cx,
                )));
            }
            let delay = ui.delay_ms;
            body = body.child(
                field(
                    if mode == PairedMode::ModTap {
                        "Hold after"
                    } else {
                        "Tap within"
                    },
                    cx.theme(),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("paired-delay-dec")
                                .outline()
                                .xsmall()
                                .label("−")
                                .accessibility_label("Shorter delay")
                                .disabled(delay <= 50)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.paired.delay_ms =
                                        this.paired.delay_ms.saturating_sub(10).max(50);
                                    cx.notify();
                                })),
                        )
                        .child(readout(format!("{delay} ms"), cx.theme()))
                        .child(
                            Button::new("paired-delay-inc")
                                .outline()
                                .xsmall()
                                .label("+")
                                .accessibility_label("Longer delay")
                                .disabled(delay >= 1000)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.paired.delay_ms = (this.paired.delay_ms + 10).min(1000);
                                    cx.notify();
                                })),
                        ),
                ),
            );
        }
        PairedMode::RapidSwitch => {}
    }
    let theme = cx.theme();
    group("paired-flow", format!("New {}", mode.title()), theme)
        .flex_1()
        .min_w_0()
        .child(muted_note(mode.summary(), theme))
        .child(body)
        .when_some(conflict, |this, conflict| {
            this.child(
                div()
                    .id("paired-conflict")
                    .text_sm()
                    .text_color(theme.danger)
                    .child(conflict)
                    .test_support(),
            )
        })
        .when(mode == PairedMode::Dks, |this| {
            this.child(muted_note(
                "The key is rebound to the DKS slot on the Win Base layer.",
                cx.theme(),
            ))
        })
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("paired-add")
                        .primary()
                        .small()
                        .label(format!("Add {}", mode.badge()))
                        .loading(busy)
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| this.add_paired(cx))),
                )
                .child(
                    Button::new("paired-cancel")
                        .ghost()
                        .small()
                        .label("Reset")
                        .disabled(ui.first.is_none() && ui.second.is_none() && ui.pick.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_paired_flow(cx))),
                ),
        )
}

fn socd_element_id(prefix: &str, pair: SocdPair, suffix: &str) -> ElementId {
    ElementId::Name(
        format!(
            "{prefix}-{}-{}-{suffix}",
            pair.key_a.value(),
            pair.key_b.value()
        )
        .into(),
    )
}

fn describe_dks(binding: &DksBinding) -> String {
    let stages: Vec<String> = binding
        .stages
        .iter()
        .flatten()
        .map(|stage| {
            let direction = match stage.direction {
                DksTriggerDirection::Press => "↓",
                DksTriggerDirection::Release => "↑",
            };
            let hold = if stage.hold { " hold" } else { "" };
            format!(
                "{direction}{} mm {}{hold}",
                mm(stage.trigger_travel),
                hid_name(stage.hid_keycode)
            )
        })
        .collect();
    stages.join(", ")
}

#[allow(clippy::too_many_lines)]
fn render_configured(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let busy = app.is_busy();
    let snapshot = &he.snapshot;
    let mut rows: Vec<AnyElement> = Vec::new();
    let remove = |id: ElementId, entry: PairedEntry, cx: &mut Context<AgnoPeriphApp>| {
        Button::new(id)
            .ghost()
            .xsmall()
            .label("Remove")
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| this.remove_paired(entry, cx)))
    };
    let row = |badge: &'static str,
               title: String,
               detail: Option<String>,
               action: AnyElement,
               theme: &Theme| {
        h_flex()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted_foreground)
                                    .child(badge),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(title),
                            ),
                    )
                    .when_some(detail, |this, detail| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(detail),
                        )
                    }),
            )
            .child(action)
            .into_any_element()
    };
    for mode in PairedMode::ALL {
        let badge = mode.badge();
        match mode {
            PairedMode::RapidSwitch => {
                for pair in &snapshot.rapid_switch_pairs {
                    let id = ElementId::Name(
                        format!("rs-remove-{}-{}", pair.key_a.value(), pair.key_b.value()).into(),
                    );
                    let action = remove(id, PairedEntry::RapidSwitch(*pair), cx).into_any_element();
                    rows.push(row(
                        badge,
                        format!(
                            "{} + {}",
                            layout.label(pair.key_a),
                            layout.label(pair.key_b)
                        ),
                        None,
                        action,
                        cx.theme(),
                    ));
                }
            }
            PairedMode::Socd => {
                for &pair in &snapshot.socd_pairs {
                    let modes = SOCD_MODES
                        .iter()
                        .map(|&(socd, label)| {
                            Button::new(socd_element_id("socd-mode", pair, label))
                                .label(label)
                                .selected(pair.mode == socd)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if pair.mode != socd {
                                        this.set_socd_mode(pair, socd, cx);
                                    }
                                }))
                        })
                        .collect::<Vec<_>>();
                    // Two lines: the pair and its Remove on top, the winner choice below,
                    // so the mode buttons never crowd the key names.
                    let action = remove(
                        socd_element_id("socd-remove", pair, "x"),
                        PairedEntry::Socd(pair),
                        cx,
                    )
                    .into_any_element();
                    let head = row(
                        badge,
                        format!(
                            "{} + {}",
                            layout.label(pair.key_a),
                            layout.label(pair.key_b)
                        ),
                        None,
                        action,
                        cx.theme(),
                    );
                    rows.push(
                        v_flex()
                            .gap_1()
                            .child(head)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(muted_note("Winner", cx.theme()))
                                    .child(
                                        ButtonGroup::new(socd_element_id(
                                            "socd-modes",
                                            pair,
                                            "group",
                                        ))
                                        .outline()
                                        .xsmall()
                                        .children(modes),
                                    ),
                            )
                            .into_any_element(),
                    );
                }
            }
            PairedMode::Dks => {
                let keymap = app
                    .selected_device()
                    .and_then(|d| d.keymap.as_ref())
                    .and_then(|km| km.snapshot.layers.first().cloned());
                for binding in &snapshot.dks_bindings {
                    let slot = binding.key;
                    let bound_key = keymap.as_ref().and_then(|layer| {
                        let code_id = DKS_KEY_ID_BASE + u8::try_from(slot.value()).ok()?;
                        let ix = layer
                            .iter()
                            .position(|c| c.class == DKS_KEY_CLASS && c.id == code_id)?;
                        Some(layout.label(KeyIndex::new(u16::try_from(ix).ok()?)))
                    });
                    let title = match bound_key {
                        Some(key) => format!("Slot {} on {key}", slot.value() + 1),
                        None => format!("Slot {}", slot.value() + 1),
                    };
                    let action = remove(
                        ElementId::Name(format!("dks-remove-{}", slot.value()).into()),
                        PairedEntry::Dks(slot),
                        cx,
                    )
                    .into_any_element();
                    rows.push(row(
                        badge,
                        title,
                        Some(describe_dks(binding)),
                        action,
                        cx.theme(),
                    ));
                }
            }
            PairedMode::ModTap => {
                for binding in &snapshot.mod_tap_bindings {
                    let action = remove(
                        ElementId::Name(format!("mt-remove-{}", binding.slot).into()),
                        PairedEntry::ModTap {
                            slot: binding.slot,
                            key: binding.key,
                        },
                        cx,
                    )
                    .into_any_element();
                    rows.push(row(
                        badge,
                        layout.label(binding.key).to_string(),
                        Some(format!(
                            "Tap {}, hold {} after {} ms",
                            hid_name(u16::from(binding.tap_keycode.1)),
                            hid_name(u16::from(binding.hold_keycode.1)),
                            binding.hold_delay_ms
                        )),
                        action,
                        cx.theme(),
                    ));
                }
            }
            PairedMode::Toggle => {
                for binding in &snapshot.toggle_bindings {
                    let action = remove(
                        ElementId::Name(format!("tgl-remove-{}", binding.slot).into()),
                        PairedEntry::Toggle {
                            slot: binding.slot,
                            key: binding.key,
                        },
                        cx,
                    )
                    .into_any_element();
                    rows.push(row(
                        badge,
                        layout.label(binding.key).to_string(),
                        Some(format!(
                            "Latches {} (tap within {} ms)",
                            hid_name(u16::from(binding.keycode.1)),
                            binding.delay_ms
                        )),
                        action,
                        cx.theme(),
                    ));
                }
            }
        }
    }
    let used = rows.len();
    let theme = cx.theme();
    group(
        "paired-configured",
        format!("Advanced keys configured ({used})"),
        theme,
    )
    .flex_1()
    .min_w_0()
    .child(muted_note(
        format!("Each type holds up to {PAIRED_SLOT_LIMIT} entries."),
        theme,
    ))
    .when(rows.is_empty(), |this| {
        this.child(muted_note("Nothing configured yet.", theme))
    })
    .children(rows)
    .test_support()
}

#[cfg(test)]
mod tests {
    use agnoperiph_core::{KeyIndex, TravelHundredthsMm};

    use super::{PairedKeysUi, PairedMode, PickTarget, free_slot, snap_travel};

    fn t(v: u16) -> TravelHundredthsMm {
        TravelHundredthsMm::from_hundredths(v)
    }

    #[test]
    fn travel_snaps_onto_step_grid_inside_range() {
        assert_eq!(snap_travel(121, t(10), t(340), t(2)), t(122));
        assert_eq!(snap_travel(0, t(10), t(340), t(2)), t(10));
        assert_eq!(snap_travel(999, t(10), t(340), t(2)), t(340));
        assert_eq!(snap_travel(15, t(2), t(340), t(2)), t(16));
    }

    #[test]
    fn pair_picks_advance_from_first_to_second_key() {
        let mut ui = PairedKeysUi {
            mode: PairedMode::Socd,
            ..PairedKeysUi::default()
        };
        assert!(!ui.accept_pick(KeyIndex::new(1)));
        ui.pick = Some(PickTarget::First);
        assert!(ui.accept_pick(KeyIndex::new(49)));
        assert_eq!(ui.pick, Some(PickTarget::Second));
        assert!(ui.accept_pick(KeyIndex::new(51)));
        assert_eq!(
            (ui.first, ui.second),
            (Some(KeyIndex::new(49)), Some(KeyIndex::new(51)))
        );
        assert_eq!(ui.pick, None);
    }

    #[test]
    fn free_slot_finds_the_lowest_gap() {
        assert_eq!(free_slot([0u16, 1, 3].into_iter()), Some(2));
        assert_eq!(free_slot(0u16..20), None);
    }
}
