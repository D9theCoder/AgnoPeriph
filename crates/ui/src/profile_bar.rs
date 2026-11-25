//! Profile selector shown at the top of every device page.
//!
//! Devices with onboard profiles (Lamzu, Logitech) switch slots in hardware. Keyboards with
//! a single onboard slot (ATK RS7) get four host-managed presets whose saved settings are
//! pushed through [`crate::backend::PeripheralService`] when the preset is activated.

use agnoperiph_core::{DeviceId, ProfileSlot};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariant, ButtonVariants},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, mm, muted_note};
use crate::backend::{ActuationTarget, DeviceSnapshot, SettingChange};
use crate::game_profiles::SavedKeyboardPreset;

/// Settings changes that restore `preset`, in a safe order: dead zones, lighting,
/// keyboard config and, last, the flash-rewriting all-keys actuation table.
pub(crate) fn preset_changes(
    preset: &SavedKeyboardPreset,
    include_actuation: bool,
) -> Vec<SettingChange> {
    let mut changes = Vec::new();
    if let (Some(top), Some(bottom)) = (preset.top_deadzone, preset.bottom_deadzone) {
        changes.push(SettingChange::Deadzones { top, bottom });
    }
    for zone in &preset.rgb_zones {
        changes.push(SettingChange::Rgb {
            zone: zone.zone,
            effect: zone.effect,
            brightness: zone.brightness,
        });
    }
    if let Some(config) = preset.keyboard_config {
        changes.push(SettingChange::KeyboardConfig(config));
    }
    if include_actuation && let Some(point) = preset.global_actuation {
        changes.push(SettingChange::Actuation {
            target: ActuationTarget::AllKeys,
            point,
        });
    }
    changes
}

/// Captures the device's current keyboard-wide settings into a preset.
///
/// Actuation is stored only when every key shares one depth: a preset holds a single
/// all-keys value, and restoring it would otherwise flatten per-key tuning.
pub(crate) fn capture_preset(device: &DeviceSnapshot) -> SavedKeyboardPreset {
    let mut preset = SavedKeyboardPreset::default();
    if let Some(he) = &device.hall_effect {
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
    }
    if let Some(rgb) = &device.rgb {
        preset.rgb_zones.clone_from(&rgb.snapshot.zones);
    }
    if let Some(kc) = &device.keyboard_config {
        preset.keyboard_config = Some(kc.snapshot);
    }
    preset
}

fn describe_preset(preset: &SavedKeyboardPreset) -> String {
    if preset.is_empty() {
        return "Nothing saved yet. Switching keeps the keyboard’s current settings.".to_owned();
    }
    let mut parts = Vec::new();
    match preset.global_actuation {
        Some(point) => parts.push(format!("actuation {} mm on all keys", mm(point))),
        None => parts.push("per-key actuation kept as is".to_owned()),
    }
    if let (Some(top), Some(bottom)) = (preset.top_deadzone, preset.bottom_deadzone) {
        parts.push(format!("dead zones {} / {} mm", mm(top), mm(bottom)));
    }
    if !preset.rgb_zones.is_empty() {
        parts.push("lighting".to_owned());
    }
    if preset.keyboard_config.is_some() {
        parts.push("advanced settings".to_owned());
    }
    let mut text = parts.join(", ");
    if let Some(first) = text.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    format!("Saved: {text}.")
}

impl AgnoPeriphApp {
    /// Activates `slot` on the selected device, confirming first when a live switch
    /// would rewrite the onboard actuation table.
    pub(crate) fn request_profile_switch(
        &mut self,
        device: DeviceId,
        slot: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        let rewrites_flash = self.device(&device).is_some_and(|d| d.profiles.is_none())
            && !self.is_dry_run()
            && self
                .config
                .ensure_software_profiles(&device)
                .slots
                .iter()
                .find(|s| s.slot == slot)
                .is_some_and(|s| s.saved.global_actuation.is_some());
        if !rewrites_flash {
            self.switch_profile(&device, slot, cx);
            return;
        }
        let name = self.config.preset_name(&device, ProfileSlot::new(slot));
        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            let device = device.clone();
            alert
                .confirm()
                .title(format!("Switch to “{name}”?"))
                .description(
                    "This preset sets one actuation depth for every key, which erases and rewrites the keyboard’s onboard actuation table.",
                )
                .ok_text("Switch preset")
                .ok_variant(ButtonVariant::Warning)
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let _ = app.update(cx, |this, cx| this.switch_profile(&device, slot, cx));
                    true
                })
        });
    }

    fn switch_profile(&mut self, device: &DeviceId, slot: u8, cx: &mut Context<Self>) {
        if let Some(job) = self.profile_switch_job(device, slot, false) {
            self.enqueue(job, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn save_current_to_preset(&mut self, device: &DeviceId, cx: &mut Context<Self>) {
        let Some(snapshot) = self.device(device) else {
            return;
        };
        let preset = capture_preset(snapshot);
        let profiles = self.config.ensure_software_profiles(device);
        let active = profiles.active_slot;
        if let Some(entry) = profiles.slots.iter_mut().find(|s| s.slot == active) {
            entry.saved = preset;
        }
        self.save_config();
        cx.notify();
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
                    this.request_profile_switch(device.clone(), slot, window, cx);
                }
            }))
    });
    let saved = hardware.is_none().then(|| {
        app.config()
            .software_profiles
            .get(id.as_str())
            .and_then(|sw| sw.slots.iter().find(|s| s.slot == active))
            .map_or_else(
                || describe_preset(&SavedKeyboardPreset::default()),
                |s| describe_preset(&s.saved),
            )
    });
    let rename_device = id.clone();
    let save_device = id.clone();
    let theme = cx.theme();
    let caption = if hardware.is_some() {
        "Onboard profile"
    } else {
        "Preset"
    };

    v_flex()
        .id("profile-bar")
        .gap_2()
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(
                    div()
                        .w_24()
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
                .when(hardware.is_none(), |this| {
                    this.child(
                        Button::new("save-preset")
                            .ghost()
                            .small()
                            .icon(IconName::Check)
                            .label("Save current settings")
                            .disabled(busy)
                            .tooltip("Store the keyboard’s current dead zones, lighting, advanced settings and (when uniform) actuation in this preset")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_current_to_preset(&save_device, cx);
                            })),
                    )
                }),
        )
        .when_some(saved, |this, saved| {
            this.child(h_flex().gap_3().child(div().w_24()).child(muted_note(saved, theme)))
        })
        .into_any_element()
}
