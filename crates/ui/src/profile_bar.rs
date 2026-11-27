//! Profile selector shown at the top of every device page.
//!
//! Devices with onboard profiles (Lamzu, Logitech) switch slots in hardware. Keyboards with
//! a single onboard slot (ATK RS7) get four host-managed presets whose full state (per-key
//! actuation & Rapid Trigger, dead zones, paired/advanced keys, lighting, and keyboard config)
//! is diffed and pushed through [`crate::backend::PeripheralService`] when switched.

use std::path::{Path, PathBuf};

use agnoperiph_core::{DeviceId, ExportedProfile, KeyIndex, ProfileSlot};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable, WindowExt,
    alert::Alert,
    button::{Button, ButtonVariants},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    notification::Notification,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AfterApply, AgnoPeriphApp};
use crate::backend::{ActuationTarget, DeviceSnapshot, SettingChange};
use crate::design::dp;
use crate::draft::EditKey;
use crate::game_profiles::SavedKeyboardPreset;
use crate::profile_io::{ImportPreview, export_file_name, export_json, stage_import};
use crate::quick_presets::QuickPreset;
use crate::widgets::{segment, segmented};

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

impl AgnoPeriphApp {
    /// Activates `slot` on `device`, asking first when that device has unsaved edits.
    pub fn request_profile_switch(
        &mut self,
        device: &DeviceId,
        slot: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        if self.has_unsaved(device) {
            // The guard lists the edits, so their device has to be on screen.
            self.select_device(device.clone(), cx);
            let target = self.config.preset_name(device, ProfileSlot::new(slot));
            crate::dirty_bar::open_guard(
                self,
                AfterApply::SwitchProfile {
                    device: device.clone(),
                    slot,
                },
                &target,
                window,
                cx,
            );
            return;
        }
        self.switch_profile(device, slot, cx);
    }

    /// Stages a built-in quick preset on the selected device as one undo step.
    pub fn load_quick_preset(&mut self, preset: QuickPreset, cx: &mut Context<Self>) {
        self.edit_draft(
            EditKey::Bulk,
            false,
            move |draft, raw| preset.stage(raw, draft),
            cx,
        );
    }

    /// Stages every supported section of an imported profile.
    pub fn import_profile(&mut self, profile: ExportedProfile, cx: &mut Context<Self>) {
        self.edit_draft(
            EditKey::Bulk,
            false,
            move |draft, raw| stage_import(&profile, raw, draft),
            cx,
        );
    }

    /// The selected device's values (with unsaved edits) as profile JSON.
    pub fn export_profile_json(&self) -> Option<(String, String)> {
        let device = self.selected_device()?;
        let preset = self
            .current_slot(device.id())
            .map(|slot| self.config.preset_name(device.id(), ProfileSlot::new(slot)));
        let name = preset
            .clone()
            .unwrap_or_else(|| device.info.model_name.clone());
        Some((
            export_file_name(device, preset.as_deref()),
            export_json(device, &name),
        ))
    }

    /// Asks where to save the export, then writes it off the UI thread.
    pub fn begin_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((file_name, json)) = self.export_profile_json() else {
            return;
        };
        let directory = default_profile_dir();
        let chosen = cx.prompt_for_new_path(&directory, Some(&file_name));
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let target = path.clone();
            let written = cx
                .background_spawn(async move { std::fs::write(&target, json) })
                .await;
            let note = match written {
                Ok(()) => Notification::success(format!("Exported to {}", display_name(&path))),
                Err(err) => Notification::error(format!("Couldn’t export the profile: {err}.")),
            };
            let _ = cx.update_window(handle, |_, window, cx| window.push_notification(note, cx));
        })
        .detach();
    }

    /// Asks for a profile file, then reads it off the UI thread and opens the preview.
    pub fn begin_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let source = path.clone();
            let read = cx
                .background_spawn(async move { std::fs::read_to_string(&source) })
                .await;
            let name = display_name(&path);
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| match read {
                    Ok(text) => this.open_import_preview(&name, &text, window, cx),
                    Err(err) => window.push_notification(
                        Notification::error(format!("Couldn’t read {name}: {err}.")),
                        cx,
                    ),
                });
            });
        })
        .detach();
    }

    /// Validates `text` against the selected device and shows what an import would load.
    pub fn open_import_preview(
        &mut self,
        file_name: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(raw) = self.selected_raw() else {
            return;
        };
        let preview = ImportPreview::parse(file_name, text, raw);
        let model = raw.info.model_name.clone();
        let entity = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            render_import_dialog(dialog, &preview, &model, &entity, cx)
        });
    }

    pub(crate) fn switch_profile(&mut self, device: &DeviceId, slot: u8, cx: &mut Context<Self>) {
        if let Some(job) = self.profile_switch_job(device, slot, false) {
            self.enqueue(job, cx);
        } else {
            cx.emit(crate::tray_model::TrayChanged);
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
        if let Some(entry) = profiles.slots.iter_mut().find(|s| s.slot == active)
            && entry.saved != preset
        {
            entry.saved = preset;
            self.save_config();
        }
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
        cx.emit(crate::tray_model::TrayChanged);
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

/// Quick presets and the profile file menu, shown for every device that has settings.
fn render_profile_tools(
    device: &DeviceSnapshot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let presets = QuickPreset::available(device);
    let entity = cx.entity().downgrade();
    let presets_menu = (!presets.is_empty()).then(|| {
        let entity = entity.clone();
        Button::new("quick-presets")
            .outline()
            .h(dp(30.))
            .icon(Icon::new(IconName::SlidersHorizontal))
            .label("Quick presets")
            .dropdown_caret(true)
            .disabled(busy)
            .dropdown_menu(move |mut menu, _, cx| {
                let muted = cx.theme().muted_foreground;
                for preset in &presets {
                    let preset = *preset;
                    let entity = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            v_flex().gap_0p5().child(preset.label()).child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(preset.description()),
                            )
                        })
                        .on_click(move |_, _, cx| {
                            let _ =
                                entity.update(cx, |this, cx| this.load_quick_preset(preset, cx));
                        }),
                    );
                }
                menu.separator()
                    .label("Loads as unsaved changes. Nothing is sent until you apply.")
            })
    });
    let file_entity = entity.clone();
    let file_menu = Button::new("profile-file-menu")
        .outline()
        .size(dp(30.))
        .icon(Icon::new(IconName::Ellipsis))
        .accessibility_label("Profile file actions")
        .tooltip("Import or export a profile file")
        .disabled(busy)
        .dropdown_menu(move |menu, _, _| {
            let import = file_entity.clone();
            let export = file_entity.clone();
            let diag = file_entity.clone();
            menu.item(
                PopupMenuItem::new("Import from JSON…")
                    .icon(Icon::new(IconName::Download))
                    .on_click(move |_, window, cx| {
                        let _ = import.update(cx, |this, cx| this.begin_import(window, cx));
                    }),
            )
            .item(
                PopupMenuItem::new("Export to JSON…")
                    .icon(Icon::new(IconName::Upload))
                    .on_click(move |_, window, cx| {
                        let _ = export.update(cx, |this, cx| this.begin_export(window, cx));
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new("Copy diagnostics")
                    .icon(Icon::new(IconName::Copy))
                    .on_click(move |_, window, cx| {
                        let _ = diag.update(cx, |this, cx| this.copy_diagnostics(window, cx));
                    }),
            )
        });
    h_flex()
        .id("profile-tools")
        .gap_2()
        .children(presets_menu)
        .child(file_menu)
}

/// Whether the device has onboard or app-managed slots to switch between.
fn has_slots(device: &DeviceSnapshot) -> bool {
    device.profiles.is_some() || device.capabilities.has_hall_effect()
}

/// Quick presets and file actions for the device header, when there is no slot row.
pub(crate) fn render_header_tools(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> Option<AnyElement> {
    let busy = app.is_busy() || app.is_loading();
    (!has_slots(device)).then(|| render_profile_tools(device, busy, cx).into_any_element())
}

/// The slot row under the device header.
pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> Option<AnyElement> {
    if !has_slots(device) {
        return None;
    }
    let id = device.id().clone();
    let busy = app.is_busy() || app.is_loading();
    let hardware = device.profiles.as_ref();
    let tools = render_profile_tools(device, busy, cx);
    let (slots, active, prefix): (Vec<(u8, bool)>, u8, &'static str) =
        if let Some(profiles) = hardware {
            let entries = (0..profiles.capabilities.slot_count)
                .map(|slot| {
                    let enabled = profiles
                        .snapshot
                        .profiles
                        .iter()
                        .find(|info| info.slot.index() == slot)
                        .is_none_or(agnoperiph_core::ProfileInfo::is_enabled);
                    (slot, enabled)
                })
                .collect();
            (entries, profiles.snapshot.active_slot.index(), "profile")
        } else {
            let sw = app.config().software_profiles.get(id.as_str());
            (
                sw.map_or_else(
                    || (0..4).map(|slot| (slot, true)).collect(),
                    |sw| sw.slots.iter().map(|s| (s.slot, true)).collect(),
                ),
                sw.map_or(0, |sw| sw.active_slot),
                "preset",
            )
        };
    let mut segments = segmented(cx).id(SharedString::from(format!("{prefix}-slots")));
    for (slot, enabled) in slots {
        let label = app.config().preset_name(&id, ProfileSlot::new(slot));
        let device = id.clone();
        let button = segment(
            Button::new((prefix, u64::from(slot))),
            label,
            slot == active,
            cx,
        )
        .disabled(busy || !enabled)
        .on_click(cx.listener(move |this, _, window, cx| {
            if enabled && slot != active {
                this.request_profile_switch(&device, slot, window, cx);
            }
        }));
        segments = segments.child(if enabled {
            button
        } else {
            button.opacity(0.4).tooltip("Disabled in onboard memory")
        });
    }
    let rename_device = id.clone();
    let caption = if hardware.is_some() {
        "Onboard profile"
    } else {
        "Preset"
    };

    h_flex()
        .id("profile-bar")
        .gap(dp(10.))
        .flex_wrap()
        .child(
            div()
                .text_size(dp(13.))
                .text_color(cx.theme().muted_foreground)
                .child(caption),
        )
        .child(segments)
        .child(
            Button::new("rename-preset")
                .ghost()
                .h(dp(30.))
                .icon(Icon::new(IconName::Pencil))
                .label("Rename…")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_rename_preset(rename_device.clone(), active, window, cx);
                })),
        )
        .child(div().flex_1())
        .child(tools)
        .into_any_element()
        .into()
}

fn default_profile_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|home| PathBuf::from(home).join("Documents"))
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn render_import_dialog(
    dialog: gpui_kit::component::dialog::Dialog,
    preview: &ImportPreview,
    model: &str,
    entity: &WeakEntity<AgnoPeriphApp>,
    cx: &mut App,
) -> gpui_kit::component::dialog::Dialog {
    let theme = cx.theme();
    let can_import = preview.can_import();
    let title = if can_import {
        format!("Import “{}”", preview.file_name)
    } else {
        format!("Can’t import “{}”", preview.file_name)
    };
    let row = |label: &'static str, value: AnyElement| {
        h_flex()
            .gap_4()
            .items_start()
            .text_sm()
            .child(
                div()
                    .w_24()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(value))
    };
    let made_for = preview.made_for.clone().map(|name| {
        h_flex()
            .gap_2()
            .child(name)
            .child(if preview.matches_model {
                Tag::secondary().xsmall().child("Matches")
            } else {
                Tag::warning().xsmall().child(format!("Not {model}"))
            })
            .into_any_element()
    });
    let sections = v_flex()
        .gap_1()
        .children(preview.sections.iter().map(|section| {
            h_flex()
                .id(SharedString::from(format!(
                    "import-section-{}",
                    section.name.to_lowercase().replace(' ', "-")
                )))
                .gap_2()
                .items_start()
                .child(
                    v_flex().flex_1().min_w_0().child(section.name).child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(section.detail.clone()),
                    ),
                )
                .child(if section.loads {
                    Tag::secondary().xsmall().child("Loads")
                } else {
                    Tag::secondary().outline().xsmall().child("Skipped")
                })
        }));
    let profile = preview.profile.clone();
    let profile_name = profile.as_ref().map(|p| p.name.clone());

    let body = v_flex()
        .id(if can_import {
            "import-preview"
        } else {
            "import-error"
        })
        .gap_3()
        .when_some(preview.error.clone(), |this, error| {
            this.child(Alert::error("import-error-alert", error))
        })
        .when_some(profile_name, |this, name| {
            this.child(row("Profile", div().child(name).into_any_element()))
        })
        .when_some(made_for, |this, made_for| {
            this.child(row("Made for", made_for))
        })
        .when(!preview.sections.is_empty(), |this| {
            this.child(row("Contains", sections.into_any_element()))
        })
        .when(can_import, |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Values load as unsaved changes. Nothing is sent until you apply."),
            )
        });

    let footer = DialogFooter::new()
        .gap_2()
        .child(DialogClose::new().child(Button::new("import-cancel").outline().label("Cancel")))
        .map(|footer| {
            if can_import {
                let entity = entity.clone();
                footer.child(
                    DialogAction::new().child(
                        Button::new("import-confirm")
                            .primary()
                            .label("Import")
                            .on_click(move |_, _, cx| {
                                if let Some(profile) = profile.clone() {
                                    let _ = entity
                                        .update(cx, |this, cx| this.import_profile(profile, cx));
                                }
                            }),
                    ),
                )
            } else {
                let entity = entity.clone();
                footer.child(
                    DialogAction::new().child(
                        Button::new("import-choose-another")
                            .outline()
                            .icon(Icon::new(IconName::FileText))
                            .label("Choose another file…")
                            .on_click(move |_, window, cx| {
                                let _ = entity.update(cx, |this, cx| this.begin_import(window, cx));
                            }),
                    ),
                )
            }
        });

    dialog.title(title).w(px(480.)).child(body).footer(footer)
}
