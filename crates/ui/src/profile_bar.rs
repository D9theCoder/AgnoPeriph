//! Profile selector shown at the top of every device page.
//!
//! Devices with onboard profiles (Lamzu, Logitech) switch slots in hardware. Keyboards with
//! a single onboard slot (ATK RS7) get four host-managed presets whose full state (per-key
//! actuation & Rapid Trigger, dead zones, paired/advanced keys, lighting, and keyboard config)
//! is diffed and pushed through [`crate::backend::PeripheralService`] when switched.

use agnoperiph_core::{DeviceId, KeyIndex, ProfileSlot};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::{ActuationTarget, DeviceSnapshot, SettingChange};
use crate::game_profiles::SavedKeyboardPreset;

/// Computes the exact ordered diff that transitions `current` into `preset`.
///
/// Clearing removed paired/advanced keys runs BEFORE adding new ones so switching between
/// profiles that reuse the same key (e.g. SOCD A+D in Profile 1 and RS A+D in Profile 2)
/// never trips exclusive-key validation. Per-key actuation and Rapid Trigger changes use
/// single-key writes for only the differing keys—fast and without erasing flash.
pub(crate) fn preset_changes_for(
    preset: &SavedKeyboardPreset,
    current: &DeviceSnapshot,
) -> Vec<SettingChange> {
    let mut changes = Vec::new();

    if let Some(he) = &current.hall_effect {
        let cur = &he.snapshot;

        // 1. Clear advanced/paired functions that do not belong in the target preset.
        //    Even when `preset` is brand-new/unconfigured, switching into it clears
        //    leftover SOCD / RS / DKS / MT / TGL from the previous profile.
        let mut cleared_socd = false;
        if !cur.socd_pairs.is_empty() && cur.socd_pairs != preset.socd_pairs {
            changes.push(SettingChange::Socd(Vec::new()));
            cleared_socd = true;
        }
        let mut cleared_rs = false;
        if !cur.rapid_switch_pairs.is_empty() && cur.rapid_switch_pairs != preset.rapid_switch_pairs
        {
            changes.push(SettingChange::RapidSwitch(Vec::new()));
            cleared_rs = true;
        }
        for existing in &cur.dks_bindings {
            if !preset.dks_bindings.contains(existing) {
                changes.push(SettingChange::ClearDks { slot: existing.key });
            }
        }
        for existing in &cur.mod_tap_bindings {
            if !preset.mod_tap_bindings.contains(existing) {
                changes.push(SettingChange::ClearModTap {
                    slot: existing.slot,
                    key: existing.key,
                });
            }
        }
        for existing in &cur.toggle_bindings {
            if !preset.toggle_bindings.contains(existing) {
                changes.push(SettingChange::ClearToggleKey {
                    slot: existing.slot,
                    key: existing.key,
                });
            }
        }

        // 2. Dead zones (only when saved and different from current).
        if let (Some(top), Some(bottom)) = (preset.top_deadzone, preset.bottom_deadzone) {
            let cur_top = cur.keys.first().and_then(|k| k.top_deadzone);
            let cur_bottom = cur.keys.first().and_then(|k| k.bottom_deadzone);
            if cur_top != Some(top) || cur_bottom != Some(bottom) {
                changes.push(SettingChange::Deadzones { top, bottom });
            }
        }

        // 3. Per-key actuation and Rapid Trigger diffs (single-key writes, no flash erase).
        if !preset.key_configs.is_empty() {
            for target_key in &preset.key_configs {
                let current_key = cur.keys.iter().find(|k| k.key == target_key.key);
                if current_key.is_none_or(|k| k.actuation != target_key.actuation) {
                    changes.push(SettingChange::Actuation {
                        target: ActuationTarget::Key(target_key.key),
                        point: target_key.actuation,
                    });
                }
                if current_key.is_none_or(|k| k.rapid_trigger != target_key.rapid_trigger) {
                    changes.push(SettingChange::RapidTrigger {
                        key: target_key.key,
                        config: target_key.rapid_trigger,
                    });
                }
            }
        } else if let Some(point) = preset.global_actuation {
            // Backward compatibility for older config.json files that only stored global_actuation.
            let diff_keys: Vec<KeyIndex> = cur
                .keys
                .iter()
                .filter(|k| k.actuation != point)
                .map(|k| k.key)
                .collect();
            if !diff_keys.is_empty() {
                changes.push(SettingChange::ActuationMany {
                    keys: diff_keys,
                    point,
                });
            }
        }

        // 4. Write the target preset's paired / advanced keys (after all clears above).
        if !preset.socd_pairs.is_empty() && (cleared_socd || cur.socd_pairs != preset.socd_pairs) {
            changes.push(SettingChange::Socd(preset.socd_pairs.clone()));
        }
        if !preset.rapid_switch_pairs.is_empty()
            && (cleared_rs || cur.rapid_switch_pairs != preset.rapid_switch_pairs)
        {
            changes.push(SettingChange::RapidSwitch(
                preset.rapid_switch_pairs.clone(),
            ));
        }
        for target in &preset.dks_bindings {
            if !cur.dks_bindings.contains(target) {
                changes.push(SettingChange::Dks(target.clone()));
            }
        }
        for target in &preset.mod_tap_bindings {
            if !cur.mod_tap_bindings.contains(target) {
                changes.push(SettingChange::ModTap(*target));
            }
        }
        for target in &preset.toggle_bindings {
            if !cur.toggle_bindings.contains(target) {
                changes.push(SettingChange::ToggleKey(*target));
            }
        }
    }

    // 5. Lighting zones (only changed zones).
    for zone in &preset.rgb_zones {
        let same = current
            .rgb
            .as_ref()
            .and_then(|rgb| rgb.snapshot.zones.iter().find(|z| z.zone == zone.zone))
            .is_some_and(|cur| cur.effect == zone.effect && cur.brightness == zone.brightness);
        if !same {
            changes.push(SettingChange::Rgb {
                zone: zone.zone,
                effect: zone.effect,
                brightness: zone.brightness,
            });
        }
    }

    // 6. Advanced keyboard config (only if different).
    if let Some(config) = preset.keyboard_config {
        let same = current
            .keyboard_config
            .as_ref()
            .is_some_and(|kc| kc.snapshot == config);
        if !same {
            changes.push(SettingChange::KeyboardConfig(config));
        }
    }

    changes
}

/// Captures the device's complete current state into a software preset.
pub(crate) fn capture_preset(device: &DeviceSnapshot) -> SavedKeyboardPreset {
    let mut preset = SavedKeyboardPreset::default();
    if let Some(he) = &device.hall_effect {
        preset.has_hall_effect = true;
        preset.key_configs.clone_from(&he.snapshot.keys);
        let mut depths = he.snapshot.keys.iter().map(|k| k.actuation);
        if let Some(first) = depths.next()
            && depths.all(|d| d == first)
        {
            preset.global_actuation = Some(first);
        }
        if let Some(key) = he.snapshot.keys.first() {
            preset.top_deadzone = key.top_deadzone;
            preset.bottom_deadzone = key.bottom_deadzone;
        }
        preset.socd_pairs.clone_from(&he.snapshot.socd_pairs);
        preset
            .rapid_switch_pairs
            .clone_from(&he.snapshot.rapid_switch_pairs);
        preset.dks_bindings.clone_from(&he.snapshot.dks_bindings);
        preset
            .mod_tap_bindings
            .clone_from(&he.snapshot.mod_tap_bindings);
        preset
            .toggle_bindings
            .clone_from(&he.snapshot.toggle_bindings);
    }
    if let Some(rgb) = &device.rgb {
        preset.rgb_zones.clone_from(&rgb.snapshot.zones);
    }
    if let Some(kc) = &device.keyboard_config {
        preset.keyboard_config = Some(kc.snapshot);
    }
    preset
}

fn describe_preset_short(preset: &SavedKeyboardPreset) -> String {
    if preset.is_empty() {
        return "Clean preset — edits auto-save".to_owned();
    }
    let mut parts = Vec::new();
    let pairs = preset.socd_pairs.len()
        + preset.rapid_switch_pairs.len()
        + preset.dks_bindings.len()
        + preset.mod_tap_bindings.len()
        + preset.toggle_bindings.len();
    if pairs > 0 {
        parts.push(if pairs == 1 {
            "1 adv key".to_owned()
        } else {
            format!("{pairs} adv keys")
        });
    }
    let rt_keys = preset
        .key_configs
        .iter()
        .filter(|k| k.rapid_trigger.is_enabled())
        .count();
    if rt_keys > 0 {
        parts.push(format!("RT ×{rt_keys}"));
    }
    if !preset.rgb_zones.is_empty() {
        parts.push("RGB".to_owned());
    }
    if parts.is_empty() {
        "Saved · auto-saves edits".to_owned()
    } else {
        format!("Saved: {}", parts.join(" · "))
    }
}

impl AgnoPeriphApp {
    /// Activates `slot` on `device`.
    pub(crate) fn request_profile_switch(
        &mut self,
        device: &DeviceId,
        slot: u8,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        self.switch_profile(device, slot, cx);
    }

    fn switch_profile(&mut self, device: &DeviceId, slot: u8, cx: &mut Context<Self>) {
        if let Some(job) = self.profile_switch_job(device, slot, false) {
            self.enqueue(job, cx);
        } else {
            cx.notify();
        }
    }

    /// Saves the device's current live state into its currently active software preset slot.
    pub(crate) fn snapshot_active_software_preset(&mut self, device: &DeviceId) {
        let Some(snapshot) = self.device(device) else {
            return;
        };
        if snapshot.profiles.is_some()
            || !snapshot.capabilities.has_hall_effect()
            || snapshot
                .warnings
                .iter()
                .any(|w| crate::backend::is_disconnect_message(&w.message))
        {
            return;
        }
        let preset = capture_preset(snapshot);
        let profiles = self.config.ensure_software_profiles(device);
        let active = profiles.active_slot;
        if let Some(entry) = profiles.slots.iter_mut().find(|s| s.slot == active) {
            entry.saved = preset;
        }
        self.save_config();
    }

    pub(crate) fn rename_preset(
        &mut self,
        device: &DeviceId,
        slot: u8,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        if self.device(device).is_some_and(|d| d.profiles.is_none()) {
            self.config.ensure_software_profiles(device);
        }
        self.config
            .set_preset_name(device, ProfileSlot::new(slot), name);
        self.save_config();
        cx.notify();
    }

    /// Opens the rename dialog for `slot` of `device`.
    pub(crate) fn open_rename_preset(
        &mut self,
        device: DeviceId,
        slot: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.config.preset_name(&device, ProfileSlot::new(slot));
        crate::game_profiles_view::open_rename(
            self,
            crate::game_profiles_view::RenameTarget::Preset { device, slot },
            format!("Rename “{current}”"),
            &current,
            "Leave empty to use the default name.",
            window,
            cx,
        );
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let id = device.id().clone();
    let busy = app.is_busy() || app.is_loading();
    let hardware = device.profiles.as_ref();
    let is_keyboard = device.capabilities.has_hall_effect();
    if hardware.is_none() && !is_keyboard {
        return div().into_any_element();
    }
    let (slots, active, prefix): (Vec<u8>, u8, &'static str) = if let Some(profiles) = hardware {
        (
            (0..profiles.capabilities.slot_count).collect(),
            profiles.snapshot.active_slot.index(),
            "profile",
        )
    } else {
        let sw = app.config().software_profiles.get(id.as_str());
        (
            sw.map_or_else(
                || (0..4).collect(),
                |sw| sw.slots.iter().map(|s| s.slot).collect(),
            ),
            sw.map_or(0, |sw| sw.active_slot),
            "preset",
        )
    };
    let buttons = slots.iter().map(|&slot| {
        let label = app.config().preset_name(&id, ProfileSlot::new(slot));
        let device = id.clone();
        Button::new((prefix, u64::from(slot)))
            .label(label.clone())
            .accessibility_label(label)
            .selected(slot == active)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, window, cx| {
                if slot != active {
                    this.request_profile_switch(&device, slot, window, cx);
                }
            }))
    });
    let saved = hardware.is_none().then(|| {
        app.config()
            .software_profiles
            .get(id.as_str())
            .and_then(|sw| sw.slots.iter().find(|s| s.slot == active))
            .map_or_else(
                || describe_preset_short(&SavedKeyboardPreset::default()),
                |s| describe_preset_short(&s.saved),
            )
    });
    let rename_device = id.clone();
    let theme = cx.theme();
    let caption = if hardware.is_some() {
        "Onboard profile"
    } else {
        "Preset"
    };

    h_flex()
        .id("profile-bar")
        .gap_2()
        .flex_wrap()
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(caption),
        )
        .child(
            ButtonGroup::new(SharedString::from(format!("{prefix}-slots")))
                .outline()
                .small()
                .children(buttons),
        )
        .child(
            Button::new("rename-preset")
                .ghost()
                .small()
                .label("Rename…")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_rename_preset(rename_device.clone(), active, window, cx);
                })),
        )
        .when_some(saved, |this, saved| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(saved),
            )
        })
        .into_any_element()
}
