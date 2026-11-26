//! Unsaved device edits ("dirty state") and their undo history.
//!
//! Every staged edit lives in one [`DeviceDraft`] overlay for the selected device. The panels
//! render the device snapshot with the overlay applied ([`DeviceDraft::overlay`]), the dirty
//! bar lists [`DeviceDraft::pending`], and Apply sends [`DeviceDraft::setting_changes`] as one
//! write job. Nothing reaches the device until the user applies.

use std::collections::BTreeMap;

use agnoperiph_core::{
    Brightness, DebounceMs, DpiStage, KeyIndex, KeyboardConfigSnapshot, LiftOffDistance,
    PollingRateHz, RapidTriggerConfig, RgbEffect, RgbZoneId, TravelHundredthsMm,
};

use crate::app::mm;
use crate::backend::{ActuationTarget, DeviceSnapshot, PointerChange, SettingChange};

/// Staged pointer edits; `None` keeps the device value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PointerDraft {
    pub(crate) dpi_stages: Option<Vec<DpiStage>>,
    pub(crate) polling_rate: Option<PollingRateHz>,
    pub(crate) lod: Option<LiftOffDistance>,
    pub(crate) debounce: Option<DebounceMs>,
    pub(crate) motion_sync: Option<bool>,
    pub(crate) angle_snapping: Option<bool>,
    pub(crate) ripple_control: Option<bool>,
}

/// One staged lighting zone: effect and brightness are written together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZoneDraft {
    pub(crate) effect: RgbEffect,
    pub(crate) brightness: Brightness,
}

/// All unsaved edits for one device. Entries equal to the device value are dropped by
/// [`Self::normalize`], so an empty draft always means "nothing to apply".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeviceDraft {
    pub(crate) pointer: PointerDraft,
    pub(crate) actuation: BTreeMap<KeyIndex, TravelHundredthsMm>,
    pub(crate) rapid_trigger: BTreeMap<KeyIndex, RapidTriggerConfig>,
    pub(crate) deadzones: Option<(TravelHundredthsMm, TravelHundredthsMm)>,
    pub(crate) rgb: BTreeMap<u8, ZoneDraft>,
    pub(crate) keyboard_config: Option<KeyboardConfigSnapshot>,
}

/// One row of the pending-changes list, and the unit a per-item revert removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChangeId {
    DpiStages,
    PollingRate,
    LiftOff,
    Debounce,
    MotionSync,
    AngleSnapping,
    RippleControl,
    Actuation,
    RapidTrigger,
    Deadzones,
    Lighting(u8),
    KeyboardConfig,
}

impl ChangeId {
    /// Stable fragment for element ids, e.g. `revert-actuation`.
    pub fn slug(self) -> String {
        match self {
            Self::DpiStages => "dpi-stages".into(),
            Self::PollingRate => "polling-rate".into(),
            Self::LiftOff => "lift-off".into(),
            Self::Debounce => "debounce".into(),
            Self::MotionSync => "motion-sync".into(),
            Self::AngleSnapping => "angle-snapping".into(),
            Self::RippleControl => "ripple-control".into(),
            Self::Actuation => "actuation".into(),
            Self::RapidTrigger => "rapid-trigger".into(),
            Self::Deadzones => "deadzones".into(),
            Self::Lighting(zone) => format!("lighting-{zone}"),
            Self::KeyboardConfig => "keyboard-config".into(),
        }
    }
}

/// A pending change as the dirty bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChange {
    pub id: ChangeId,
    pub label: String,
    pub scope: Option<String>,
    pub from: String,
    pub to: String,
}

/// What an edit touched, so consecutive slider events coalesce into one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditKey {
    Change(ChangeId),
    /// A bulk edit such as a quick preset, import or "Restore default"; never coalesced.
    Bulk,
}

fn same_or_mixed<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Option<T> {
    let mut values = values;
    let first = values.next()?;
    values.all(|v| v == first).then_some(first)
}

fn rt_label(config: RapidTriggerConfig) -> String {
    if config.is_enabled() {
        format!(
            "{} / {} mm",
            mm(config.press_sensitivity),
            mm(config.release_sensitivity)
        )
    } else {
        "Off".to_owned()
    }
}

fn on_off(value: bool) -> String {
    if value { "On" } else { "Off" }.to_owned()
}

fn dpi_list(stages: &[DpiStage]) -> String {
    stages
        .iter()
        .map(|s| s.x.value().to_string())
        .collect::<Vec<_>>()
        .join(" / ")
}

impl DeviceDraft {
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Drops entries the device already holds or can't take.
    pub(crate) fn normalize(&mut self, device: &DeviceSnapshot) {
        match device.pointer.as_ref() {
            Some(pointer) => {
                let snap = &pointer.snapshot;
                let p = &mut self.pointer;
                if p.dpi_stages.as_ref() == Some(&snap.dpi_stages) {
                    p.dpi_stages = None;
                }
                if p.polling_rate == Some(snap.polling_rate) {
                    p.polling_rate = None;
                }
                if p.lod.is_some() && p.lod == snap.lod {
                    p.lod = None;
                }
                if p.debounce.is_some() && p.debounce == snap.debounce_ms {
                    p.debounce = None;
                }
                if p.motion_sync.is_some() && p.motion_sync == snap.motion_sync {
                    p.motion_sync = None;
                }
                if p.angle_snapping.is_some() && p.angle_snapping == snap.angle_snapping {
                    p.angle_snapping = None;
                }
                if p.ripple_control.is_some() && p.ripple_control == snap.ripple_control {
                    p.ripple_control = None;
                }
            }
            None => self.pointer = PointerDraft::default(),
        }
        if let Some(he) = device.hall_effect.as_ref() {
            {
                let keys = &he.snapshot.keys;
                self.actuation.retain(|key, point| {
                    keys.iter()
                        .find(|k| k.key == *key)
                        .is_some_and(|k| k.actuation != *point)
                });
                self.rapid_trigger.retain(|key, config| {
                    keys.iter()
                        .find(|k| k.key == *key)
                        .is_some_and(|k| k.rapid_trigger != *config)
                });
                let current = keys
                    .first()
                    .map_or((None, None), |k| (k.top_deadzone, k.bottom_deadzone));
                if let Some((top, bottom)) = self.deadzones
                    && current == (Some(top), Some(bottom))
                {
                    self.deadzones = None;
                }
            }
        } else {
            self.actuation.clear();
            self.rapid_trigger.clear();
            self.deadzones = None;
        }
        match device.rgb.as_ref() {
            Some(rgb) => self.rgb.retain(|zone, draft| {
                rgb.snapshot
                    .zones
                    .iter()
                    .find(|z| z.zone.value() == *zone)
                    .is_some_and(|z| z.effect != draft.effect || z.brightness != draft.brightness)
            }),
            None => self.rgb.clear(),
        }
        if self.keyboard_config.is_some_and(|cfg| {
            device
                .keyboard_config
                .as_ref()
                .is_none_or(|kc| kc.snapshot == cfg)
        }) {
            self.keyboard_config = None;
        }
    }

    /// Returns `device` with every staged edit applied, for rendering the editors.
    pub(crate) fn overlay(&self, device: &DeviceSnapshot) -> DeviceSnapshot {
        let mut out = device.clone();
        if let Some(pointer) = out.pointer.as_mut() {
            let p = &self.pointer;
            let snap = &mut pointer.snapshot;
            if let Some(stages) = &p.dpi_stages {
                snap.dpi_stages.clone_from(stages);
            }
            if let Some(rate) = p.polling_rate {
                snap.polling_rate = rate;
            }
            if p.lod.is_some() {
                snap.lod = p.lod;
            }
            if p.debounce.is_some() {
                snap.debounce_ms = p.debounce;
            }
            if p.motion_sync.is_some() {
                snap.motion_sync = p.motion_sync;
            }
            if p.angle_snapping.is_some() {
                snap.angle_snapping = p.angle_snapping;
            }
            if p.ripple_control.is_some() {
                snap.ripple_control = p.ripple_control;
            }
        }
        if let Some(he) = out.hall_effect.as_mut() {
            for key in &mut he.snapshot.keys {
                if let Some(point) = self.actuation.get(&key.key) {
                    key.actuation = *point;
                }
                if let Some(config) = self.rapid_trigger.get(&key.key) {
                    key.rapid_trigger = *config;
                }
                if let Some((top, bottom)) = self.deadzones {
                    key.top_deadzone = Some(top);
                    key.bottom_deadzone = Some(bottom);
                }
            }
        }
        if let Some(rgb) = out.rgb.as_mut() {
            for zone in &mut rgb.snapshot.zones {
                if let Some(draft) = self.rgb.get(&zone.zone.value()) {
                    zone.effect = draft.effect;
                    zone.brightness = draft.brightness;
                }
            }
        }
        if let (Some(cfg), Some(kc)) = (self.keyboard_config, out.keyboard_config.as_mut()) {
            kc.snapshot = cfg;
        }
        out
    }

    /// Removes one pending change.
    pub(crate) fn revert(&mut self, id: ChangeId) {
        let p = &mut self.pointer;
        match id {
            ChangeId::DpiStages => p.dpi_stages = None,
            ChangeId::PollingRate => p.polling_rate = None,
            ChangeId::LiftOff => p.lod = None,
            ChangeId::Debounce => p.debounce = None,
            ChangeId::MotionSync => p.motion_sync = None,
            ChangeId::AngleSnapping => p.angle_snapping = None,
            ChangeId::RippleControl => p.ripple_control = None,
            ChangeId::Actuation => self.actuation.clear(),
            ChangeId::RapidTrigger => self.rapid_trigger.clear(),
            ChangeId::Deadzones => self.deadzones = None,
            ChangeId::Lighting(zone) => {
                self.rgb.remove(&zone);
            }
            ChangeId::KeyboardConfig => self.keyboard_config = None,
        }
    }

    /// The pending changes in display order. `key_label` names a key for the scope text.
    pub fn pending(
        &self,
        device: &DeviceSnapshot,
        key_label: &dyn Fn(KeyIndex) -> String,
    ) -> Vec<PendingChange> {
        let mut out = Vec::new();
        let mut push = |id, label: &str, scope: Option<String>, from: String, to: String| {
            out.push(PendingChange {
                id,
                label: label.to_owned(),
                scope,
                from,
                to,
            });
        };
        if let Some(pointer) = device.pointer.as_ref() {
            let snap = &pointer.snapshot;
            let p = &self.pointer;
            if let Some(stages) = &p.dpi_stages {
                push(
                    ChangeId::DpiStages,
                    "DPI stages",
                    None,
                    dpi_list(&snap.dpi_stages),
                    dpi_list(stages),
                );
            }
            if let Some(rate) = p.polling_rate {
                push(
                    ChangeId::PollingRate,
                    "Polling rate",
                    None,
                    snap.polling_rate.to_string(),
                    rate.to_string(),
                );
            }
            if let Some(lod) = p.lod {
                push(
                    ChangeId::LiftOff,
                    "Lift-off distance",
                    None,
                    snap.lod.map_or_else(|| "–".into(), |l| l.to_string()),
                    lod.to_string(),
                );
            }
            if let Some(debounce) = p.debounce {
                push(
                    ChangeId::Debounce,
                    "Click debounce",
                    None,
                    snap.debounce_ms
                        .map_or_else(|| "–".into(), |d| d.to_string()),
                    debounce.to_string(),
                );
            }
            for (id, label, staged, current) in [
                (
                    ChangeId::MotionSync,
                    "Motion sync",
                    p.motion_sync,
                    snap.motion_sync,
                ),
                (
                    ChangeId::AngleSnapping,
                    "Angle snapping",
                    p.angle_snapping,
                    snap.angle_snapping,
                ),
                (
                    ChangeId::RippleControl,
                    "Ripple control",
                    p.ripple_control,
                    snap.ripple_control,
                ),
            ] {
                if let Some(value) = staged {
                    push(
                        id,
                        label,
                        None,
                        on_off(current.unwrap_or(!value)),
                        on_off(value),
                    );
                }
            }
        }
        if let Some(he) = device.hall_effect.as_ref() {
            let keys = &he.snapshot.keys;
            let current = |key: &KeyIndex| keys.iter().find(|k| k.key == *key);
            if !self.actuation.is_empty() {
                let from = same_or_mixed(
                    self.actuation
                        .keys()
                        .filter_map(|k| current(k).map(|c| c.actuation)),
                )
                .map_or_else(|| "mixed".to_owned(), |v| format!("{} mm", mm(v)));
                let to = same_or_mixed(self.actuation.values().copied())
                    .map_or_else(|| "mixed".to_owned(), |v| format!("{} mm", mm(v)));
                push(
                    ChangeId::Actuation,
                    "Actuation point",
                    Some(key_scope(self.actuation.keys(), keys.len(), key_label)),
                    from,
                    to,
                );
            }
            if !self.rapid_trigger.is_empty() {
                let from = same_or_mixed(
                    self.rapid_trigger
                        .keys()
                        .filter_map(|k| current(k).map(|c| c.rapid_trigger)),
                )
                .map_or_else(|| "mixed".to_owned(), rt_label);
                let to = same_or_mixed(self.rapid_trigger.values().copied())
                    .map_or_else(|| "mixed".to_owned(), rt_label);
                push(
                    ChangeId::RapidTrigger,
                    "Rapid Trigger",
                    Some(key_scope(self.rapid_trigger.keys(), keys.len(), key_label)),
                    from,
                    to,
                );
            }
            if let Some((top, bottom)) = self.deadzones {
                let first = keys.first();
                let zero = TravelHundredthsMm::from_hundredths(0);
                let cur_top = first.and_then(|k| k.top_deadzone).unwrap_or(zero);
                let cur_bottom = first.and_then(|k| k.bottom_deadzone).unwrap_or(zero);
                push(
                    ChangeId::Deadzones,
                    "Dead zones",
                    Some("All keys".to_owned()),
                    format!("{} / {} mm", mm(cur_top), mm(cur_bottom)),
                    format!("{} / {} mm", mm(top), mm(bottom)),
                );
            }
        }
        if let Some(rgb) = device.rgb.as_ref() {
            for (zone, draft) in &self.rgb {
                let Some(state) = rgb.snapshot.zones.iter().find(|z| z.zone.value() == *zone)
                else {
                    continue;
                };
                let name = rgb
                    .capabilities
                    .zones
                    .iter()
                    .find(|z| z.id == state.zone)
                    .map_or_else(|| format!("Zone {zone}"), |z| z.name.clone());
                let describe = |effect: RgbEffect, brightness: Brightness| {
                    format!(
                        "{}, {}%",
                        crate::rgb_panel::effect_label(effect.kind()),
                        brightness.percent()
                    )
                };
                push(
                    ChangeId::Lighting(*zone),
                    "Lighting",
                    Some(name),
                    describe(state.effect, state.brightness),
                    describe(draft.effect, draft.brightness),
                );
            }
        }
        if let (Some(cfg), Some(kc)) = (self.keyboard_config, device.keyboard_config.as_ref()) {
            let changed = config_differences(&kc.snapshot, &cfg);
            push(
                ChangeId::KeyboardConfig,
                "Keyboard settings",
                None,
                changed
                    .iter()
                    .map(|(name, from, _)| format!("{name} {from}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                changed
                    .iter()
                    .map(|(name, _, to)| format!("{name} {to}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        out
    }

    /// The writes that commit this draft, each tagged with the change it belongs to.
    ///
    /// Per-key edits are grouped by value. A group that covers every key becomes one
    /// keyboard-wide write.
    pub(crate) fn setting_changes(
        &self,
        device: &DeviceSnapshot,
    ) -> Vec<(ChangeId, SettingChange)> {
        let mut out = Vec::new();
        let p = &self.pointer;
        if let Some(pointer) = device.pointer.as_ref() {
            if let Some(stages) = &p.dpi_stages {
                out.push((
                    ChangeId::DpiStages,
                    SettingChange::Pointer(PointerChange::DpiStages {
                        stages: stages.clone(),
                        active: pointer.snapshot.active_dpi_stage,
                    }),
                ));
            }
            if let Some(rate) = p.polling_rate {
                out.push((
                    ChangeId::PollingRate,
                    SettingChange::Pointer(PointerChange::PollingRate(rate)),
                ));
            }
            if let Some(lod) = p.lod {
                out.push((
                    ChangeId::LiftOff,
                    SettingChange::Pointer(PointerChange::LiftOffDistance(lod)),
                ));
            }
            if let Some(debounce) = p.debounce {
                out.push((
                    ChangeId::Debounce,
                    SettingChange::Pointer(PointerChange::Debounce(debounce)),
                ));
            }
            if let Some(on) = p.motion_sync {
                out.push((
                    ChangeId::MotionSync,
                    SettingChange::Pointer(PointerChange::MotionSync(on)),
                ));
            }
            if let Some(on) = p.angle_snapping {
                out.push((
                    ChangeId::AngleSnapping,
                    SettingChange::Pointer(PointerChange::AngleSnapping(on)),
                ));
            }
            if let Some(on) = p.ripple_control {
                out.push((
                    ChangeId::RippleControl,
                    SettingChange::Pointer(PointerChange::RippleControl(on)),
                ));
            }
        }
        if let Some(he) = device.hall_effect.as_ref() {
            let total = he.snapshot.keys.len();
            if let Some((top, bottom)) = self.deadzones {
                out.push((
                    ChangeId::Deadzones,
                    SettingChange::Deadzones { top, bottom },
                ));
            }
            let mut by_point: BTreeMap<TravelHundredthsMm, Vec<KeyIndex>> = BTreeMap::new();
            for (key, point) in &self.actuation {
                by_point.entry(*point).or_default().push(*key);
            }
            for (point, keys) in by_point {
                let change = match keys.as_slice() {
                    // Scope follows the edited keys, never the resulting table: a single
                    // key that happens to make the board uniform is still one key.
                    [key] if total > 1 => SettingChange::Actuation {
                        target: ActuationTarget::Key(*key),
                        point,
                    },
                    _ if keys.len() == total => SettingChange::Actuation {
                        target: ActuationTarget::AllKeys,
                        point,
                    },
                    _ => SettingChange::ActuationMany { keys, point },
                };
                out.push((ChangeId::Actuation, change));
            }
            let mut by_config: Vec<(RapidTriggerConfig, Vec<KeyIndex>)> = Vec::new();
            for (key, config) in &self.rapid_trigger {
                match by_config.iter_mut().find(|(c, _)| c == config) {
                    Some((_, keys)) => keys.push(*key),
                    None => by_config.push((*config, vec![*key])),
                }
            }
            for (config, keys) in by_config {
                let change = match keys.as_slice() {
                    [key] => SettingChange::RapidTrigger { key: *key, config },
                    _ => SettingChange::RapidTriggerMany { keys, config },
                };
                out.push((ChangeId::RapidTrigger, change));
            }
        }
        for (zone, draft) in &self.rgb {
            out.push((
                ChangeId::Lighting(*zone),
                SettingChange::Rgb {
                    zone: RgbZoneId::new(*zone),
                    effect: draft.effect,
                    brightness: draft.brightness,
                },
            ));
        }
        if let Some(cfg) = self.keyboard_config {
            out.push((ChangeId::KeyboardConfig, SettingChange::KeyboardConfig(cfg)));
        }
        out
    }
}

/// `true` when the writes rewrite every key's actuation, which erases the onboard table.
pub(crate) fn rewrites_all_keys(changes: &[(ChangeId, SettingChange)]) -> bool {
    changes.iter().any(|(_, change)| {
        matches!(
            change,
            SettingChange::Actuation {
                target: ActuationTarget::AllKeys,
                ..
            }
        )
    })
}

fn key_scope<'a>(
    keys: impl Iterator<Item = &'a KeyIndex>,
    total: usize,
    key_label: &dyn Fn(KeyIndex) -> String,
) -> String {
    let keys: Vec<KeyIndex> = keys.copied().collect();
    if keys.len() == total && total > 1 {
        return "All keys".to_owned();
    }
    if keys.len() <= 6 {
        return keys
            .iter()
            .map(|k| key_label(*k))
            .collect::<Vec<_>>()
            .join(" ");
    }
    format!("{} keys", keys.len())
}

fn config_differences(
    from: &KeyboardConfigSnapshot,
    to: &KeyboardConfigSnapshot,
) -> Vec<(&'static str, String, String)> {
    let mut out = Vec::new();
    for (name, a, b) in [
        ("Mac mode", from.mac_mode, to.mac_mode),
        ("WASD swap", from.exchange_wasd, to.exchange_wasd),
        ("Win lock", from.win_lock, to.win_lock),
        ("6KRO", from.six_key_rollover, to.six_key_rollover),
        (
            "Anti-accidental",
            from.anti_accidental_touch,
            to.anti_accidental_touch,
        ),
        ("Extreme mode", from.extreme_mode, to.extreme_mode),
        ("Fn inversion", from.fn_inversion, to.fn_inversion),
        ("Game mode", from.game_mode, to.game_mode),
    ] {
        if a != b {
            out.push((name, on_off(a).to_lowercase(), on_off(b).to_lowercase()));
        }
    }
    if from.polling_rate != to.polling_rate {
        out.push((
            "Polling",
            from.polling_rate.to_string(),
            to.polling_rate.to_string(),
        ));
    }
    out
}

/// Undo stack of whole-draft states. Edits with the same [`EditKey`] inside one open group
/// (a slider drag) share one step.
#[derive(Debug, Clone, Default)]
pub(crate) struct DraftHistory {
    undo: Vec<DeviceDraft>,
    open_group: Option<EditKey>,
}

impl DraftHistory {
    /// Records `before` as an undo step unless the edit continues the open group.
    pub(crate) fn record(&mut self, before: &DeviceDraft, key: EditKey, coalesce: bool) {
        let continues = coalesce && key != EditKey::Bulk && self.open_group == Some(key);
        if !continues {
            self.undo.push(before.clone());
        }
        self.open_group = (coalesce && key != EditKey::Bulk).then_some(key);
    }

    pub(crate) fn end_group(&mut self) {
        self.open_group = None;
    }

    pub(crate) fn pop(&mut self) -> Option<DeviceDraft> {
        self.open_group = None;
        self.undo.pop()
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.undo.clear();
        self.open_group = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MockPeripheralService;
    use crate::backend::{MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, PeripheralService};
    use agnoperiph_core::DeviceId;

    fn device(id: &str) -> DeviceSnapshot {
        MockPeripheralService::new()
            .refresh()
            .unwrap()
            .into_iter()
            .find(|d| d.id() == &DeviceId::new(id))
            .unwrap()
    }

    fn t(v: u16) -> TravelHundredthsMm {
        TravelHundredthsMm::from_hundredths(v)
    }

    #[test]
    fn normalize_drops_values_the_device_already_holds() {
        let kb = device(MOCK_KEYBOARD_ID);
        let he = kb.hall_effect.as_ref().unwrap();
        let first = he.snapshot.keys[0];
        let mut draft = DeviceDraft::default();
        draft.actuation.insert(first.key, first.actuation);
        draft.rapid_trigger.insert(first.key, first.rapid_trigger);
        draft.normalize(&kb);
        assert!(draft.is_empty());

        draft.actuation.insert(first.key, t(80));
        draft.normalize(&kb);
        assert_eq!(draft.actuation.len(), 1);
    }

    #[test]
    fn overlay_and_pending_show_staged_values() {
        let kb = device(MOCK_KEYBOARD_ID);
        let keys: Vec<KeyIndex> = kb.hall_effect.as_ref().unwrap().snapshot.keys[..2]
            .iter()
            .map(|k| k.key)
            .collect();
        let mut draft = DeviceDraft::default();
        for key in &keys {
            draft.actuation.insert(*key, t(80));
        }
        draft.deadzones = Some((t(10), t(20)));
        let shown = draft.overlay(&kb);
        let he = shown.hall_effect.as_ref().unwrap();
        assert_eq!(he.key(keys[0]).unwrap().actuation, t(80));
        assert_eq!(he.snapshot.keys[5].top_deadzone, Some(t(10)));

        let pending = draft.pending(&kb, &|k| format!("K{}", k.value()));
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, ChangeId::Actuation);
        assert_eq!(pending[0].to, "0.80 mm");
        assert_eq!(pending[1].id, ChangeId::Deadzones);

        draft.revert(ChangeId::Actuation);
        assert!(draft.actuation.is_empty());
    }

    #[test]
    fn setting_changes_group_keys_and_use_one_write_for_all_keys() {
        let kb = device(MOCK_KEYBOARD_ID);
        let all: Vec<KeyIndex> = kb
            .hall_effect
            .as_ref()
            .unwrap()
            .snapshot
            .keys
            .iter()
            .map(|k| k.key)
            .collect();
        let mut draft = DeviceDraft::default();
        draft.actuation.insert(all[0], t(80));
        draft.actuation.insert(all[1], t(80));
        draft.actuation.insert(all[2], t(150));
        let changes = draft.setting_changes(&kb);
        assert_eq!(changes.len(), 2);
        assert!(matches!(
            changes[0].1,
            SettingChange::ActuationMany { ref keys, .. } if keys.len() == 2
        ));
        assert!(matches!(
            changes[1].1,
            SettingChange::Actuation {
                target: ActuationTarget::Key(_),
                ..
            }
        ));
        assert!(!rewrites_all_keys(&changes));

        let mut everything = DeviceDraft::default();
        for key in &all {
            everything.actuation.insert(*key, t(90));
        }
        assert!(rewrites_all_keys(&everything.setting_changes(&kb)));
    }

    #[test]
    fn last_odd_key_edit_stays_scoped_to_that_key() {
        let mut kb = device(MOCK_KEYBOARD_ID);
        let he = kb.hall_effect.as_mut().unwrap();
        for config in &mut he.snapshot.keys {
            config.actuation = t(120);
        }
        he.snapshot.keys[0].actuation = t(80);
        let key = he.snapshot.keys[0].key;

        let mut draft = DeviceDraft::default();
        draft.actuation.insert(key, t(120));
        let changes = draft.setting_changes(&kb);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            changes[0].1,
            SettingChange::Actuation {
                target: ActuationTarget::Key(k),
                point,
            } if k == key && point == t(120)
        ));
        assert!(!rewrites_all_keys(&changes));
    }

    #[test]
    fn pointer_edits_become_pointer_writes() {
        let mouse = device(MOCK_MOUSE_ID);
        let pointer = mouse.pointer.as_ref().unwrap();
        let rate = *pointer
            .capabilities
            .supported_polling_rates
            .iter()
            .find(|r| **r != pointer.snapshot.polling_rate)
            .unwrap();
        let mut draft = DeviceDraft::default();
        draft.pointer.polling_rate = Some(rate);
        draft.normalize(&mouse);
        let changes = draft.setting_changes(&mouse);
        assert_eq!(
            changes,
            vec![(
                ChangeId::PollingRate,
                SettingChange::Pointer(PointerChange::PollingRate(rate))
            )]
        );
        assert_eq!(
            draft.overlay(&mouse).pointer.unwrap().snapshot.polling_rate,
            rate
        );
    }

    #[test]
    fn history_coalesces_one_drag_into_one_step() {
        let mut history = DraftHistory::default();
        let a = DeviceDraft::default();
        let key = EditKey::Change(ChangeId::Actuation);
        history.record(&a, key, true);
        history.record(&a, key, true);
        history.record(&a, key, true);
        history.end_group();
        history.record(&a, key, false);
        history.record(&a, EditKey::Bulk, true);
        assert!(history.pop().is_some());
        assert!(history.pop().is_some());
        assert!(history.pop().is_some());
        assert!(history.pop().is_none());
        assert!(!history.can_undo());
    }
}
