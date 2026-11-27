//! Triggers & paired keys: per-key actuation and Rapid Trigger with multi-key selection,
//! keyboard-wide dead zones, and the paired / advanced key functions (RS, SOCD, DKS, MT, TGL).

use std::collections::{BTreeMap, BTreeSet};

mod inspector;
mod tools;

use agnoperiph_core::{
    DEFAULT_PAIR_TRIGGER_POINT, DksBinding, DksStageAction, DksTriggerDirection, KeyCode,
    KeyHallEffectConfig, KeyIndex, ModTapBinding, RapidSwitchPair, RapidTriggerConfig, SocdMode,
    SocdPair, ToggleKeyBinding, TravelHundredthsMm,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, DraftScope, JobOrigin, mm};
use crate::backend::{DeviceSnapshot, HallEffectState, SettingChange};
use crate::controls::{SliderRole, sync_slider, to_units};
use crate::design::{dp, tokens};
use crate::draft::{ChangeId, EditKey};
use crate::icons::{AdvancedIcon, advanced_svg};
use crate::key_matrix::{self, BOARD_WIDTH, KeyContent, KeyFace, KeyTone};
use crate::keycodes::{HID_KEYS, KeyGroup, hid_name};
use crate::keymap::KeyLayout;
use crate::widgets::{panel, panel_head};

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
            Self::RapidSwitch => "Rapid switching",
            Self::Socd => "SOCD",
            Self::Dks => "Dynamic keystroke",
            Self::ModTap => "Mod-tap",
            Self::Toggle => "Toggle",
        }
    }

    /// The type's name for an owner tag from `occupied_advanced_keys`.
    pub(crate) fn title_of_owner(owner: &str) -> &str {
        Self::ALL
            .into_iter()
            .find(|mode| mode.badge() == owner)
            .map_or(owner, |mode| mode.title())
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

    const fn icon(self) -> AdvancedIcon {
        match self {
            Self::RapidSwitch => AdvancedIcon::Rs,
            Self::Socd => AdvancedIcon::Socd,
            Self::Dks => AdvancedIcon::Dks,
            Self::ModTap => AdvancedIcon::Mt,
            Self::Toggle => AdvancedIcon::Tgl,
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
    /// Per-pair trigger actuation depth for new RS / SOCD pairs.
    pub(crate) pair_trigger_point: TravelHundredthsMm,
    /// Per-pair Rapid Trigger toggle for new RS / SOCD pairs.
    pub(crate) pair_rapid_trigger: bool,
    pub(crate) dks_stages: [DksStageDraft; 4],
    pub(crate) primary_usage: u8,
    pub(crate) secondary_usage: u8,
    pub(crate) delay_ms: u16,
    /// Why the last key click was refused, shown next to the pick buttons.
    pub(crate) conflict: Option<SharedString>,
    /// Whether the type drop-down is showing.
    pub(crate) is_menu_open: bool,
    /// Keys of the configured entry under the pointer, outlined on the board.
    pub(crate) hovered: Option<(KeyIndex, Option<KeyIndex>)>,
}

impl Default for PairedKeysUi {
    fn default() -> Self {
        Self {
            mode: PairedMode::Socd,
            pick: None,
            first: None,
            second: None,
            socd_mode: SocdMode::LastInputPriority,
            pair_trigger_point: DEFAULT_PAIR_TRIGGER_POINT,
            pair_rapid_trigger: false,
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
            is_menu_open: false,
            hovered: None,
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
        self.hovered = None;
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
        Some((config.actuation, config.rapid_trigger))
    }

    pub(crate) fn deadzone_values(&self) -> Option<(TravelHundredthsMm, TravelHundredthsMm)> {
        Some(deadzones_of(self.hall_effect_state()?))
    }

    fn selection(&self) -> Vec<KeyIndex> {
        self.selected_keys.iter().copied().collect()
    }

    fn set_actuation_draft(&mut self, value: i32, coalesce: bool, cx: &mut Context<Self>) {
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
        let keys = self.selection();
        if keys.is_empty() {
            return;
        }
        self.edit_draft(
            EditKey::Change(ChangeId::Actuation),
            coalesce,
            move |draft, _| {
                for key in keys {
                    draft.actuation.insert(key, next);
                }
            },
            cx,
        );
    }

    fn step_actuation(&mut self, direction: i32, cx: &mut Context<Self>) {
        let (Some(he), Some((actuation, _))) = (self.hall_effect_state(), self.key_values()) else {
            return;
        };
        let step = i32::from(he.capabilities.actuation_step.as_hundredths().max(1));
        self.set_actuation_draft(
            i32::from(actuation.as_hundredths()) + direction * step,
            false,
            cx,
        );
    }

    /// Stages the primary key's actuation for every key with its own trigger travel.
    fn stage_actuation_for_all_keys(&mut self, cx: &mut Context<Self>) {
        let Some((point, _)) = self.key_values() else {
            return;
        };
        let keys = self.selectable_layout_keys();
        self.edit_draft(
            EditKey::Bulk,
            false,
            move |draft, _| {
                for key in keys {
                    draft.actuation.insert(key, point);
                }
            },
            cx,
        );
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

    /// Stages `config` for every selected key.
    fn stage_rapid_trigger(
        &mut self,
        config: RapidTriggerConfig,
        coalesce: bool,
        cx: &mut Context<Self>,
    ) {
        let keys = self.selection();
        if keys.is_empty() {
            return;
        }
        self.edit_draft(
            EditKey::Change(ChangeId::RapidTrigger),
            coalesce,
            move |draft, _| {
                for key in keys {
                    draft.rapid_trigger.insert(key, config);
                }
            },
            cx,
        );
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
        self.stage_rapid_trigger(
            RapidTriggerConfig::new(enabled, continuous, press, release),
            false,
            cx,
        );
    }

    fn set_sensitivity(
        &mut self,
        is_press: bool,
        value: i32,
        coalesce: bool,
        cx: &mut Context<Self>,
    ) {
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
        self.stage_rapid_trigger(config, coalesce, cx);
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
        self.set_sensitivity(
            is_press,
            i32::from(current.as_hundredths()) + delta,
            false,
            cx,
        );
    }

    fn step_deadzone(&mut self, is_top: bool, direction: i32, cx: &mut Context<Self>) {
        let Some((top, bottom)) = self.deadzone_values() else {
            return;
        };
        let current = if is_top { top } else { bottom };
        self.set_deadzone(is_top, i32::from(current.as_hundredths()) + direction, cx);
    }

    fn set_deadzone(&mut self, is_top: bool, value: i32, cx: &mut Context<Self>) {
        let Some((top, bottom)) = self.deadzone_values() else {
            return;
        };
        let next = snap_travel(value, t(0), t(100), t(1));
        let pair = if is_top { (next, bottom) } else { (top, next) };
        self.edit_draft(
            EditKey::Change(ChangeId::Deadzones),
            false,
            move |draft, _| draft.deadzones = Some(pair),
            cx,
        );
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

    fn selectable_layout_keys(&self) -> Vec<KeyIndex> {
        self.layout_keys()
            .into_iter()
            .filter(|&key| self.pair_managed_mode(key).is_none())
            .collect()
    }

    fn select_all_keys(&mut self, cx: &mut Context<Self>) {
        self.selected_keys = self.selectable_layout_keys().into_iter().collect();
        if let Some(&first) = self.selected_keys.iter().next() {
            self.selected_key = first;
        }
        self.end_edit_group();
        self.request_sync(cx);
        cx.notify();
    }

    fn invert_key_selection(&mut self, cx: &mut Context<Self>) {
        let all: BTreeSet<KeyIndex> = self.selectable_layout_keys().into_iter().collect();
        self.selected_keys = all.difference(&self.selected_keys).copied().collect();
        if let Some(first) = self.selected_keys.iter().next() {
            self.selected_key = *first;
        }
        self.end_edit_group();
        self.request_sync(cx);
        cx.notify();
    }

    fn clear_key_selection(&mut self, cx: &mut Context<Self>) {
        self.selected_keys.clear();
        self.end_edit_group();
        cx.notify();
    }

    /// Stages the default actuation with Rapid Trigger off for the selected keys.
    fn restore_default_keys(&mut self, cx: &mut Context<Self>) {
        let keys = self.selection();
        if keys.is_empty() {
            return;
        }
        let has_rt = self
            .hall_effect_state()
            .is_some_and(|he| he.capabilities.has_rapid_trigger());
        self.edit_draft(
            EditKey::Bulk,
            false,
            move |draft, raw| {
                let he = raw.hall_effect.as_ref();
                for key in keys {
                    draft.actuation.insert(key, DEFAULT_ACTUATION);
                    let current = he.and_then(|he| he.key(key)).map(|k| k.rapid_trigger);
                    // A key that already has Rapid Trigger off keeps its stored encoding.
                    if has_rt && current.is_some_and(|c| c.is_enabled()) {
                        draft
                            .rapid_trigger
                            .insert(key, RapidTriggerConfig::disabled());
                    } else {
                        draft.rapid_trigger.remove(&key);
                    }
                }
            },
            cx,
        );
    }

    // ---- Paired keys ------------------------------------------------------------------

    pub(crate) fn set_paired_mode(&mut self, mode: PairedMode, cx: &mut Context<Self>) {
        self.paired.is_menu_open = false;
        if self.paired.mode != mode {
            self.paired.mode = mode;
            self.paired.reset_flow();
        }
        cx.notify();
    }

    fn start_pick(&mut self, target: PickTarget, cx: &mut Context<Self>) {
        self.paired.hovered = None;
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
                let pair = pair
                    .with_trigger_point(ui.pair_trigger_point)
                    .with_rapid_trigger(ui.pair_rapid_trigger);
                self.selected_keys.remove(&first);
                self.selected_keys.remove(&second);
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
                let pair = pair
                    .with_trigger_point(ui.pair_trigger_point)
                    .with_rapid_trigger(ui.pair_rapid_trigger);
                self.selected_keys.remove(&first);
                self.selected_keys.remove(&second);
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
        self.paired.hovered = None;
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

    fn set_pair_trigger_point(&mut self, value: i32, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let caps = &he.capabilities;
        self.paired.pair_trigger_point = snap_travel(
            value,
            caps.actuation_min,
            caps.actuation_max,
            caps.actuation_step,
        );
        self.request_sync(cx);
        cx.notify();
    }

    fn step_pair_trigger_point(&mut self, direction: i32, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let step = i32::from(he.capabilities.actuation_step.as_hundredths().max(1));
        self.set_pair_trigger_point(
            i32::from(self.paired.pair_trigger_point.as_hundredths()) + direction * step,
            cx,
        );
    }

    fn update_socd_pair(
        &mut self,
        target: SocdPair,
        mutate: impl FnOnce(SocdPair) -> SocdPair,
        cx: &mut Context<Self>,
    ) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let mut mutate = Some(mutate);
        let pairs = he
            .snapshot
            .socd_pairs
            .iter()
            .map(|&pair| {
                if pair.key_a == target.key_a && pair.key_b == target.key_b {
                    mutate.take().map_or(pair, |f| f(pair))
                } else {
                    pair
                }
            })
            .collect();
        self.apply_change(SettingChange::Socd(pairs), cx);
    }

    fn set_socd_mode(&mut self, target: SocdPair, mode: SocdMode, cx: &mut Context<Self>) {
        self.update_socd_pair(
            target,
            |pair| {
                SocdPair::new(pair.key_a, pair.key_b, mode).map_or(pair, |p| {
                    p.with_trigger_point(pair.trigger_point)
                        .with_rapid_trigger(pair.rapid_trigger)
                })
            },
            cx,
        );
    }

    fn step_socd_travel(&mut self, target: SocdPair, direction: i32, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let caps = &he.capabilities;
        let step = i32::from(caps.actuation_step.as_hundredths().max(1));
        let next = snap_travel(
            i32::from(target.trigger_point.as_hundredths()) + direction * step,
            caps.actuation_min,
            caps.actuation_max,
            caps.actuation_step,
        );
        if next != target.trigger_point {
            self.update_socd_pair(target, |p| p.with_trigger_point(next), cx);
        }
    }

    fn set_socd_rt(&mut self, target: SocdPair, enabled: bool, cx: &mut Context<Self>) {
        if target.rapid_trigger != enabled {
            self.update_socd_pair(target, |p| p.with_rapid_trigger(enabled), cx);
        }
    }

    fn update_rs_pair(
        &mut self,
        target: RapidSwitchPair,
        mutate: impl FnOnce(RapidSwitchPair) -> RapidSwitchPair,
        cx: &mut Context<Self>,
    ) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let mut mutate = Some(mutate);
        let pairs = he
            .snapshot
            .rapid_switch_pairs
            .iter()
            .map(|&pair| {
                if pair.key_a == target.key_a && pair.key_b == target.key_b {
                    mutate.take().map_or(pair, |f| f(pair))
                } else {
                    pair
                }
            })
            .collect();
        self.apply_change(SettingChange::RapidSwitch(pairs), cx);
    }

    fn step_rs_travel(&mut self, target: RapidSwitchPair, direction: i32, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let caps = &he.capabilities;
        let step = i32::from(caps.actuation_step.as_hundredths().max(1));
        let next = snap_travel(
            i32::from(target.trigger_point.as_hundredths()) + direction * step,
            caps.actuation_min,
            caps.actuation_max,
            caps.actuation_step,
        );
        if next != target.trigger_point {
            self.update_rs_pair(target, |p| p.with_trigger_point(next), cx);
        }
    }

    fn set_rs_rt(&mut self, target: RapidSwitchPair, enabled: bool, cx: &mut Context<Self>) {
        if target.rapid_trigger != enabled {
            self.update_rs_pair(target, |p| p.with_rapid_trigger(enabled), cx);
        }
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
    let key_values = app.key_values();
    let Some(controls) = app.controls.as_mut() else {
        return;
    };
    let (travel, rt_press, rt_release) = (
        controls.travel.clone(),
        controls.rt_press.clone(),
        controls.rt_release.clone(),
    );
    if let Some((actuation, rt)) = key_values {
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
}

/// Slider drags stage one value per event and form one undo step until released.
pub(crate) fn on_slider(
    app: &mut AgnoPeriphApp,
    role: SliderRole,
    value: f32,
    released: bool,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let units = i32::from(to_units(value));
    match role {
        SliderRole::Travel => app.set_actuation_draft(units, true, cx),
        SliderRole::RtPress => app.set_sensitivity(true, units, true, cx),
        SliderRole::RtRelease => app.set_sensitivity(false, units, true, cx),
        _ => {}
    }
    if released {
        app.end_edit_group();
    }
}

fn deadzones_of(he: &HallEffectState) -> (TravelHundredthsMm, TravelHundredthsMm) {
    let first = he.snapshot.keys.first();
    (
        first.and_then(|k| k.top_deadzone).unwrap_or(t(0)),
        first.and_then(|k| k.bottom_deadzone).unwrap_or(t(0)),
    )
}

/// Narrowest panel that still sits beside the board.
const MIN_INSPECTOR_WIDTH: f32 = 220.0;
const BOARD_GAP: f32 = 16.0;
const SELECTION_BAR_HEIGHT: f32 = 38.0;
/// Narrowest tool panel before the row below the board wraps.
const MIN_TOOL_WIDTH: f32 = 260.0;

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    workspace_width: f32,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let Some(he) = device.hall_effect.as_ref() else {
        return panel("hall-effect-panel", cx)
            .child(panel_head("Triggers"))
            .child(div().text_color(cx.theme().muted_foreground).child(
                "The keyboard reports Hall Effect switches, but their settings couldn’t be read.",
            ))
            .into_any_element();
    };
    let layout = KeyLayout::for_device(&device.info, &he.capabilities);
    let occupied = crate::backend::occupied_advanced_keys(device);
    let caps = &he.capabilities;
    let has_paired = caps.has_socd() || caps.has_dks();
    let has_tester = caps.has_live_travel();
    let side_by_side = (has_paired || has_tester)
        && workspace_width >= BOARD_WIDTH + BOARD_GAP + MIN_INSPECTOR_WIDTH;
    let board_width = BOARD_WIDTH.min(workspace_width);

    let board_column = v_flex()
        .flex_shrink_0()
        .w(dp(board_width))
        .gap(dp(4.))
        .child(render_matrix(app, he, &layout, &occupied, board_width, cx))
        .child(render_selection_bar(app, &layout, cx));

    // Beside the board the panel matches its height. Without paired keys that spot
    // goes to the key tester.
    let height =
        side_by_side.then(|| key_matrix::height(&layout, board_width) + SELECTION_BAR_HEIGHT);
    let aside = if has_paired {
        Some(inspector::render(app, he, &layout, &occupied, height, cx))
    } else {
        has_tester.then(|| {
            v_flex()
                .when_some(height, |this, height| this.h(dp(height)))
                .child(crate::key_tester::render(app, he))
                .into_any_element()
        })
    }
    .map(|aside| v_flex().flex_1().min_w_0().child(aside).into_any_element());
    let (beside, below) = if side_by_side {
        (aside, None)
    } else {
        (None, aside)
    };

    let tool = |child: AnyElement| v_flex().flex_1().min_w(dp(MIN_TOOL_WIDTH)).child(child);
    let tools = h_flex()
        .id("trigger-deck")
        .flex_wrap()
        .items_stretch()
        .gap(dp(BOARD_GAP))
        .when(has_paired && has_tester, |this| {
            this.child(tool(crate::key_tester::render(app, he)))
        })
        .child(tool(tools::actuation_panel(app, he, cx)))
        .when(caps.has_rapid_trigger(), |this| {
            this.child(tool(tools::rapid_trigger_panel(app, he, cx)))
        });

    v_flex()
        .id("hall-effect-panel")
        .gap(dp(14.))
        .child(
            h_flex()
                .items_stretch()
                .gap(dp(BOARD_GAP))
                .when(beside.is_none(), Styled::justify_center)
                .child(board_column)
                .children(beside),
        )
        .child(tools)
        .children(below)
        .into_any_element()
}

/// What is selected, with the selection tools, directly under the board.
fn render_selection_bar(
    app: &AgnoPeriphApp,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    /// Names listed before the rest is elided.
    const NAMED: usize = 16;
    let busy = app.is_busy();
    let count = app.selected_keys().len();
    let title: SharedString = match count {
        0 => "No keys selected".into(),
        1 => "1 key selected".into(),
        n => format!("{n} keys selected").into(),
    };
    let names: SharedString = if count == 0 {
        "Click keys to select them".into()
    } else {
        let mut names = app
            .selected_keys()
            .iter()
            .take(NAMED)
            .map(|&key| layout.label(key).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if count > NAMED {
            names.push_str(", …");
        }
        names.into()
    };
    let theme = cx.theme();
    h_flex()
        .id("selection-rail")
        .w_full()
        .h(dp(34.))
        .gap(dp(6.))
        .child(
            div()
                .flex_shrink_0()
                .mr(dp(4.))
                .font_weight(FontWeight::SEMIBOLD)
                .when(count > 0, |this| this.text_color(theme.primary))
                .child(title),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(names),
        )
        .child(
            Button::new("select-all-keys")
                .ghost()
                .small()
                .label("Select all")
                .on_click(cx.listener(|this, _, _, cx| this.select_all_keys(cx))),
        )
        .child(
            Button::new("invert-keys")
                .ghost()
                .small()
                .label("Invert")
                .on_click(cx.listener(|this, _, _, cx| this.invert_key_selection(cx))),
        )
        .child(
            Button::new("clear-keys")
                .ghost()
                .small()
                .label("Clear")
                .disabled(count == 0)
                .on_click(cx.listener(|this, _, _, cx| this.clear_key_selection(cx))),
        )
        .child(
            Button::new("restore-default-keys")
                .outline()
                .small()
                .label("Restore default")
                .tooltip(format!(
                    "Set the selected keys to {} mm with Rapid Trigger off",
                    mm(DEFAULT_ACTUATION)
                ))
                .disabled(busy || count == 0)
                .on_click(cx.listener(|this, _, _, cx| this.restore_default_keys(cx))),
        )
        .into_any_element()
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

/// The actuation most keys share, which the board therefore leaves unmarked.
fn common_actuation(keys: &[KeyHallEffectConfig]) -> TravelHundredthsMm {
    let mut counts: BTreeMap<u16, usize> = BTreeMap::new();
    for key in keys {
        *counts.entry(key.actuation.as_hundredths()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(value, count)| (count, std::cmp::Reverse(value)))
        .map_or(DEFAULT_ACTUATION, |(value, _)| t(value))
}

/// The pair settings that drive `key` instead of its own actuation: RS or SOCD.
fn pair_trigger(he: &HallEffectState, key: KeyIndex) -> Option<(TravelHundredthsMm, bool)> {
    he.snapshot
        .socd_pairs
        .iter()
        .find(|p| p.key_a == key || p.key_b == key)
        .map(|p| (p.trigger_point, p.rapid_trigger))
        .or_else(|| {
            he.snapshot
                .rapid_switch_pairs
                .iter()
                .find(|p| p.key_a == key || p.key_b == key)
                .map(|p| (p.trigger_point, p.rapid_trigger))
        })
}

/// Actuation and Rapid Trigger as the key behaves, wherever the settings come from.
fn key_behaviour(he: &HallEffectState, key: KeyIndex) -> Option<(TravelHundredthsMm, bool)> {
    pair_trigger(he, key).or_else(|| {
        he.key(key)
            .map(|k| (k.actuation, k.rapid_trigger.is_enabled()))
    })
}

fn render_matrix(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    occupied: &BTreeMap<KeyIndex, &'static str>,
    board_width: f32,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let max = f32::from(he.capabilities.actuation_max.as_hundredths().max(1));
    let selected = app.selected_keys();
    let paired = &app.paired;
    let picking = paired.pick.is_some();
    let base = common_actuation(&he.snapshot.keys);
    let device_he = app.selected_raw().and_then(|d| d.hall_effect.as_ref());
    let is_pending = |key: KeyIndex| {
        device_he.is_some_and(|device| {
            device.key(key) != he.key(key) || pair_trigger(device, key) != pair_trigger(he, key)
        })
    };
    let has_pending = he.snapshot.keys.iter().any(|k| is_pending(k.key));
    let lip = matrix_lip(he, base, picking, has_pending, cx);
    let matrix = key_matrix::render(
        "key-matrix",
        "key",
        layout,
        board_width,
        |cap| {
            let owner = occupied.get(&cap.index).copied();
            let tone = if paired.is_picked(cap.index) {
                KeyTone::Selected
            } else if picking && owner.is_some() {
                KeyTone::Locked
            } else if !picking && selected.contains(&cap.index) {
                KeyTone::Selected
            } else {
                KeyTone::Normal
            };
            let (actuation, is_rapid) = key_behaviour(he, cap.index).unwrap_or((base, false));
            let description = format!(
                "{} millimetres{}{}",
                mm(actuation),
                if is_rapid { ", Rapid Trigger on" } else { "" },
                owner.map_or_else(String::new, |o| format!(", {o}")),
            );
            let mut face = KeyFace::new(
                KeyContent::Trigger {
                    fill: f32::from(actuation.as_hundredths()) / max,
                    value: (actuation != base).then(|| mm(actuation).into()),
                    is_rapid,
                },
                tone,
            );
            face.badge = owner.and_then(AdvancedIcon::from_owner);
            face.is_outlined = paired
                .hovered
                .is_some_and(|(a, b)| a == cap.index || b == Some(cap.index));
            face.is_pending = is_pending(cap.index);
            face.description = Some(description.into());
            face
        },
        trigger_select,
        lip,
        cx,
    );
    v_flex().flex_shrink_0().child(matrix)
}

/// What the marks on the board mean, or how to pick keys while a pick is armed.
fn matrix_lip(
    he: &HallEffectState,
    base: TravelHundredthsMm,
    picking: bool,
    has_pending: bool,
    cx: &App,
) -> Vec<AnyElement> {
    let theme = cx.theme();
    if picking {
        return vec![
            div()
                .id("paired-pick-hint")
                .text_color(theme.primary)
                .child("Click a key to pick it. Dimmed keys already belong to another function.")
                .test_support()
                .into_any_element(),
        ];
    }
    let bar = div()
        .w(dp(14.))
        .h(dp(2.))
        .rounded_full()
        .bg(tokens(cx).case_text);
    let dot = div().size(dp(5.)).rounded_full().bg(theme.primary);
    let mut lip = vec![
        key_matrix::legend_item(bar, "Actuation"),
        key_matrix::legend_item(dot, "Rapid Trigger"),
    ];
    if he.capabilities.has_socd() || he.capabilities.has_dks() {
        let icons = h_flex().gap(dp(4.)).children(
            [AdvancedIcon::Rs, AdvancedIcon::Socd, AdvancedIcon::Dks]
                .map(|icon| advanced_svg(icon, dp(12.), tokens(cx).case_text)),
        );
        lip.push(key_matrix::legend_item(icons, "Paired"));
    }
    if has_pending {
        let swatch = div().w(dp(14.)).h(dp(4.)).rounded_full().bg(theme.warning);
        lip.push(key_matrix::legend_item(swatch, "Not applied"));
    }
    lip.push(div().flex_1().into_any_element());
    lip.push(
        div()
            .flex_shrink_0()
            .child(format!("Unmarked keys use {} mm", mm(base)))
            .into_any_element(),
    );
    lip
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
