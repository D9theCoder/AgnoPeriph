//! Game Profiles manager: automatic switching status, installed-game scan, manual add,
//! per-game editor, and the cross-brand device preset matrix (plus the Desktop fallback).

use agnoperiph_core::{DeviceId, ProfileSlot};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariant, ButtonVariants},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::Input,
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, group, muted_note};
use crate::backend::DeviceSnapshot;
use crate::controls::{InputRole, sync_input};
use crate::game_profiles::{
    DEFAULT_SOFTWARE_PROFILE_SLOTS, DetectedGame, GameProfile, normalize_exe_name,
};
use crate::keycodes::matches_search;

/// Scan results shown at once; the search narrows the rest.
const MAX_SCAN_RESULTS: usize = 40;

/// What the shared rename dialog renames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenameTarget {
    Preset { device: DeviceId, slot: u8 },
    Game(String),
    Macro(u8),
}

/// Game Profiles view state.
#[derive(Debug, Clone, Default)]
pub(crate) struct GamesUi {
    pub(crate) search: SharedString,
    pub(crate) rename_target: Option<RenameTarget>,
    /// `None` = not scanned yet.
    pub(crate) scanned: Option<Vec<DetectedGame>>,
    pub(crate) is_scanning: bool,
    /// Selected Game Profile id; `None` selects the Desktop / Default fallback.
    pub(crate) selected: Option<String>,
    /// Validation message for the manual add form.
    pub(crate) form_error: Option<SharedString>,
    /// Validation message for the selected game's executable.
    pub(crate) exe_error: Option<SharedString>,
}

/// Opens the shared rename dialog backed by the `rename` input.
pub(crate) fn open_rename(
    app: &mut AgnoPeriphApp,
    target: RenameTarget,
    title: String,
    current: &str,
    hint: &'static str,
    window: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let Some(input) = app.controls.as_ref().map(|c| c.rename.clone()) else {
        return;
    };
    sync_input(&input, current, window, cx);
    app.games_ui.rename_target = Some(target);
    let entity = cx.entity().downgrade();
    window.open_dialog(cx, move |dialog, _, _| {
        let entity = entity.clone();
        let input = input.clone();
        dialog
            .title(title.clone())
            .w(px(420.))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().id("rename-input").child(Input::new(&input)))
                    .child(div().text_xs().child(hint)),
            )
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(
                        DialogClose::new()
                            .child(Button::new("rename-cancel").outline().label("Cancel")),
                    )
                    .child(DialogAction::new().child(
                        Button::new("rename-save").primary().label("Save").on_click(
                            move |_, _, cx| {
                                let name = input.read(cx).value().to_string();
                                let _ = entity.update(cx, |this, cx| {
                                    this.commit_rename(&name, cx);
                                });
                            },
                        ),
                    )),
            )
    });
}

/// Enter in one of the Game Profile inputs.
pub(crate) fn on_enter(
    app: &mut AgnoPeriphApp,
    role: InputRole,
    window: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let Some(controls) = app.controls.as_ref() else {
        return;
    };
    match role {
        InputRole::Rename => {
            let name = controls.rename.read(cx).value().to_string();
            if app.games_ui.rename_target.is_some() {
                app.commit_rename(&name, cx);
                window.close_dialog(cx);
            }
        }
        InputRole::ManualName | InputRole::ManualExe => app.add_manual_game(window, cx),
        InputRole::ProfileExe => app.commit_game_exe(cx),
        _ => {}
    }
}

impl AgnoPeriphApp {
    /// Applies the pending rename from the shared dialog.
    pub(crate) fn commit_rename(&mut self, name: &str, cx: &mut Context<Self>) {
        match self.games_ui.rename_target.take() {
            Some(RenameTarget::Preset { device, slot }) => {
                self.rename_preset(&device, slot, name, cx);
            }
            Some(RenameTarget::Game(id)) => self.rename_game_profile(&id, name, cx),
            Some(RenameTarget::Macro(index)) => self.rename_macro(index, name, cx),
            None => {}
        }
    }

    pub fn set_auto_switch(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.config.auto_switch_enabled == enabled {
            return;
        }
        self.config.auto_switch_enabled = enabled;
        self.applied_targets.clear();
        self.auto_switch_note = None;
        if !enabled {
            self.active_game = None;
        }
        self.save_config();
        cx.notify();
    }

    /// Adds a Game Profile and selects it. Returns its id.
    pub fn add_game_profile(
        &mut self,
        name: &str,
        exe: &str,
        exe_path: &str,
        cx: &mut Context<Self>,
    ) -> String {
        let id = self.config.unique_game_profile_id(name);
        let exe_name = std::path::Path::new(exe.trim().trim_matches('"'))
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(exe)
            .to_owned();
        self.config.upsert_game_profile(GameProfile::new(
            id.clone(),
            name.trim(),
            exe_name,
            exe_path,
        ));
        self.games_ui.selected = Some(id.clone());
        self.applied_targets.clear();
        self.save_config();
        cx.notify();
        id
    }

    pub fn rename_game_profile(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.config.rename_game_profile(id, name);
        self.save_config();
        cx.notify();
    }

    pub fn set_game_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        if let Some(profile) = self.config.game_profile_mut(id) {
            profile.enabled = enabled;
            self.applied_targets.clear();
            self.save_config();
            cx.notify();
        }
    }

    pub fn delete_game_profile(&mut self, id: &str, cx: &mut Context<Self>) {
        self.config.remove_game_profile(id);
        if self.games_ui.selected.as_deref() == Some(id) {
            self.games_ui.selected = None;
        }
        if self.active_game.as_deref() == Some(id) {
            self.active_game = None;
        }
        self.applied_targets.clear();
        self.save_config();
        cx.notify();
    }

    /// Sets which preset `device` uses for a game (`None` = the Desktop fallback);
    /// `slot: None` leaves the device unchanged.
    pub fn set_game_target(
        &mut self,
        game: Option<&str>,
        device: &DeviceId,
        slot: Option<u8>,
        cx: &mut Context<Self>,
    ) {
        let targets = match game {
            Some(id) => match self.config.game_profile_mut(id) {
                Some(profile) => &mut profile.device_targets,
                None => return,
            },
            None => &mut self.config.default_targets,
        };
        match slot {
            Some(slot) => targets.insert(device.as_str().to_owned(), slot),
            None => targets.remove(device.as_str()),
        };
        self.applied_targets.remove(device.as_str());
        self.save_config();
        cx.notify();
    }

    fn select_game_profile(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let exe = id
            .as_deref()
            .and_then(|id| self.config.game_profile(id))
            .map_or_default(|p| p.exe_name.clone());
        if let Some(input) = self.controls.as_ref().map(|c| c.profile_exe.clone()) {
            sync_input(&input, &exe, window, cx);
        }
        self.games_ui.selected = id;
        self.games_ui.exe_error = None;
        cx.notify();
    }

    fn commit_game_exe(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(controls)) = (self.games_ui.selected.clone(), self.controls.as_ref())
        else {
            return;
        };
        let exe = controls.profile_exe.read(cx).value().trim().to_owned();
        if !is_exe(&exe) {
            self.games_ui.exe_error = Some("Enter an executable name ending in .exe.".into());
            cx.notify();
            return;
        }
        if let Some(profile) = self.config.game_profile_mut(&id) {
            let leaf = std::path::Path::new(exe.trim_matches('"'))
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&exe)
                .to_owned();
            if leaf != exe {
                profile.exe_path.clone_from(&exe);
            }
            profile.exe_name = leaf;
        }
        self.games_ui.exe_error = None;
        self.applied_targets.clear();
        self.save_config();
        cx.notify();
    }

    fn add_manual_game(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(controls) = self.controls.as_ref() else {
            return;
        };
        let name_input = controls.manual_name.clone();
        let exe_input = controls.manual_exe.clone();
        let name = name_input.read(cx).value().trim().to_owned();
        let exe = exe_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.games_ui.form_error = Some("Enter a name for the game.".into());
            cx.notify();
            return;
        }
        if !is_exe(&exe) {
            self.games_ui.form_error = Some("Enter an executable name ending in .exe.".into());
            cx.notify();
            return;
        }
        if self.has_game_exe(&exe) {
            self.games_ui.form_error = Some(format!("A Game Profile already uses {exe}.").into());
            cx.notify();
            return;
        }
        self.games_ui.form_error = None;
        let id = self.add_game_profile(&name, &exe, &exe, cx);
        sync_input(&name_input, "", window, cx);
        sync_input(&exe_input, "", window, cx);
        self.select_game_profile(Some(id), window, cx);
    }

    fn add_detected_game(
        &mut self,
        game: &DetectedGame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_game_exe(&game.exe_name) {
            return;
        }
        let id = self.add_game_profile(&game.name, &game.exe_name, &game.exe_path, cx);
        self.select_game_profile(Some(id), window, cx);
    }

    fn has_game_exe(&self, exe: &str) -> bool {
        let wanted = normalize_exe_name(exe);
        self.config
            .game_profiles
            .iter()
            .any(|p| p.normalized_exe() == wanted)
    }

    /// Scans installed games off the UI thread.
    pub fn scan_games(&mut self, cx: &mut Context<Self>) {
        if self.games_ui.is_scanning {
            return;
        }
        self.games_ui.is_scanning = true;
        let discover = self.discover_games.clone();
        cx.spawn(async move |this, cx| {
            let games = cx.background_spawn(async move { discover() }).await;
            let _ = this.update(cx, |this, cx| {
                this.games_ui.scanned = Some(games);
                this.games_ui.is_scanning = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_delete_game(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.config.game_profile(&id).map(|p| p.name.clone()) else {
            return;
        };
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let id = id.clone();
            alert
                .confirm()
                .title(format!("Delete “{name}”?"))
                .description("Its preset assignments are removed. Running the game no longer switches your devices.")
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| this.delete_game_profile(&id, cx));
                    true
                })
        });
    }

    fn open_rename_game(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.config.game_profile(&id).map(|p| p.name.clone()) else {
            return;
        };
        open_rename(
            self,
            RenameTarget::Game(id),
            format!("Rename “{name}”"),
            &name,
            "The name is only shown in AgnoPeriph.",
            window,
            cx,
        );
    }

    /// Preset slots offered for `device` with their display names.
    pub(crate) fn preset_slots(&self, device: &DeviceSnapshot) -> Vec<(u8, String)> {
        let id = device.id();
        let count = match &device.profiles {
            Some(profiles) => profiles.capabilities.slot_count,
            None if device.capabilities.has_hall_effect() => self
                .config
                .software_profiles
                .get(id.as_str())
                .map_or(DEFAULT_SOFTWARE_PROFILE_SLOTS, |sw| {
                    u8::try_from(sw.slots.len()).unwrap_or(DEFAULT_SOFTWARE_PROFILE_SLOTS)
                }),
            None => 0,
        };
        (0..count)
            .map(|slot| (slot, self.config.preset_name(id, ProfileSlot::new(slot))))
            .collect()
    }
}

fn is_exe(value: &str) -> bool {
    std::path::Path::new(value.trim().trim_matches('"'))
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let header = v_flex()
        .gap_1()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Game Profiles"),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Switch every connected device to the right preset when a game starts, and back when it closes."),
        );
    let status = render_status(app, cx);
    let list = render_list(app, cx);
    let editor = render_editor(app, cx);
    let discovery = render_discovery(app, cx);
    v_flex()
        .id("game-profiles")
        .gap_5()
        .child(header)
        .child(status)
        .child(
            h_flex().items_start().gap_5().child(list).child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_5()
                    .child(editor)
                    .child(discovery),
            ),
        )
}

fn render_status(app: &AgnoPeriphApp, cx: &mut Context<AgnoPeriphApp>) -> impl IntoElement + use<> {
    let enabled = app.config.auto_switch_enabled;
    let playing = app.active_game_name().map(str::to_owned);
    let theme = cx.theme();
    let line = match (&playing, enabled) {
        (_, false) => "Automatic switching is off.".to_owned(),
        (Some(name), true) => format!("Detected {name}. Its presets are active."),
        (None, true) => {
            "No configured game is running. Desktop / Default presets are active.".to_owned()
        }
    };
    let dot = if playing.is_some() && enabled {
        theme.success
    } else {
        theme.muted_foreground
    };
    h_flex()
        .id("auto-switch-status")
        .gap_4()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .child(
            v_flex()
                .flex_1()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().size_2().rounded_full().bg(dot))
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(
                            match (&playing, enabled) {
                                (Some(name), true) => name.clone(),
                                _ => "Desktop / Default".to_owned(),
                            },
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(line),
                )
                .when_some(app.auto_switch_note.clone(), |this, note| {
                    this.child(div().text_xs().text_color(theme.warning).child(note))
                }),
        )
        .child(
            h_flex()
                .gap_2()
                .child(div().text_sm().child("Switch automatically"))
                .child(
                    Switch::new("auto-switch")
                        .checked(enabled)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_auto_switch(*checked, cx);
                        })),
                ),
        )
}

fn render_list(app: &AgnoPeriphApp, cx: &mut Context<AgnoPeriphApp>) -> impl IntoElement + use<> {
    let selected = app.games_ui.selected.clone();
    let active = app.active_game().map(str::to_owned);
    let mut items: Vec<AnyElement> = Vec::new();
    items.push(
        Button::new("game-profile-default")
            .ghost()
            .small()
            .w_full()
            .justify_start()
            .icon(IconName::LayoutDashboard)
            .label("Desktop / Default")
            .selected(selected.is_none())
            .on_click(cx.listener(|this, _, window, cx| {
                this.select_game_profile(None, window, cx);
            }))
            .into_any_element(),
    );
    for profile in &app.config.game_profiles {
        let id = profile.id.clone();
        let is_running = active.as_deref() == Some(id.as_str());
        let muted = cx.theme().muted_foreground;
        let success = cx.theme().success;
        items.push(
            Button::new(SharedString::from(format!("game-profile-{id}")))
                .ghost()
                .small()
                .w_full()
                .justify_start()
                .selected(selected.as_deref() == Some(id.as_str()))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(div().flex_1().truncate().child(profile.name.clone()))
                        .when(is_running, |this| {
                            this.child(div().size_2().rounded_full().bg(success))
                        })
                        .when(!profile.enabled, |this| {
                            this.child(div().text_xs().text_color(muted).child("Off"))
                        }),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_game_profile(Some(id.clone()), window, cx);
                }))
                .into_any_element(),
        );
    }
    let theme = cx.theme();
    v_flex()
        .id("game-profile-list")
        .w_56()
        .flex_none()
        .gap_0p5()
        .child(
            div()
                .px_2()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(format!("Profiles ({})", app.config.game_profiles.len())),
        )
        .children(items)
        .when(app.config.game_profiles.is_empty(), |this| {
            this.child(div().px_2().pt_2().child(muted_note(
                "No games yet. Scan installed games or add one by its executable.",
                theme,
            )))
        })
}

fn render_editor(app: &AgnoPeriphApp, cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let selected = app.games_ui.selected.clone();
    let profile = selected
        .as_deref()
        .and_then(|id| app.config.game_profile(id))
        .cloned();
    let matrix = render_matrix(app, profile.as_ref(), cx);
    let Some(profile) = profile else {
        let theme = cx.theme();
        return group("game-editor-default", "Desktop / Default", theme)
            .child(muted_note(
                "Presets restored when no configured game is running. Devices set to “Don’t change” keep whatever preset they have.",
                theme,
            ))
            .child(matrix)
            .into_any_element();
    };
    let id = profile.id.clone();
    let exe_input = app.controls.as_ref().map(|c| c.profile_exe.clone());
    let theme = cx.theme();
    let title = h_flex()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(profile.name.clone()),
        )
        .child(div().flex_1())
        .child(
            Button::new("rename-game")
                .ghost()
                .small()
                .label("Rename…")
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| this.open_rename_game(id.clone(), window, cx)
                })),
        )
        .child(
            Button::new("delete-game")
                .ghost()
                .small()
                .icon(IconName::Delete)
                .label("Delete…")
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| this.request_delete_game(id.clone(), window, cx)
                })),
        );
    let enabled_row = h_flex()
        .gap_4()
        .child(
            div()
                .w_40()
                .flex_shrink_0()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Switch for this game"),
        )
        .child(
            Switch::new("game-enabled")
                .checked(profile.enabled)
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, checked: &bool, _, cx| this.set_game_enabled(&id, *checked, cx)
                })),
        );
    let exe_row = h_flex()
        .gap_4()
        .child(
            div()
                .w_40()
                .flex_shrink_0()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Executable"),
        )
        .when_some(exe_input, |this, input| {
            this.child(div().flex_1().max_w_96().child(Input::new(&input).small()))
        })
        .child(
            Button::new("save-game-exe")
                .outline()
                .small()
                .label("Save")
                .on_click(cx.listener(|this, _, _, cx| this.commit_game_exe(cx))),
        );
    let theme = cx.theme();
    group(
        SharedString::from(format!("game-editor-{id}")),
        "Game",
        theme,
    )
    .child(title)
    .child(enabled_row)
    .child(exe_row)
    .when(
        !profile.exe_path.is_empty() && profile.exe_path != profile.exe_name,
        |this| this.child(muted_note(profile.exe_path.clone(), theme)),
    )
    .when_some(app.games_ui.exe_error.clone(), |this, err| {
        this.child(div().text_xs().text_color(theme.danger).child(err))
    })
    .child(matrix)
    .into_any_element()
}

fn render_matrix(
    app: &AgnoPeriphApp,
    profile: Option<&GameProfile>,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let targets = profile.map_or_else(
        || app.config.default_targets.clone(),
        |p| p.device_targets.clone(),
    );
    let game = profile.map(|p| p.id.clone());
    let mut rows: Vec<AnyElement> = Vec::new();
    for device in app.devices() {
        let id = device.id().clone();
        let slots = app.preset_slots(device);
        let current = targets.get(id.as_str()).copied();
        let row_id = SharedString::from(format!("target-{id}"));
        let theme = cx.theme();
        let name = v_flex()
            .w_48()
            .flex_none()
            .gap_0p5()
            .child(
                div()
                    .text_sm()
                    .truncate()
                    .child(device.info.model_name.clone()),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                if device.profiles.is_some() {
                    "Onboard profiles"
                } else if slots.is_empty() {
                    "No presets"
                } else {
                    "AgnoPeriph presets"
                },
            ));
        if slots.is_empty() {
            rows.push(
                h_flex()
                    .id(row_id)
                    .gap_4()
                    .child(name)
                    .child(muted_note("This device has no presets to switch.", theme))
                    .into_any_element(),
            );
            continue;
        }
        let mut buttons: Vec<Button> = Vec::new();
        {
            let game = game.clone();
            let id = id.clone();
            buttons.push(
                Button::new(SharedString::from(format!("target-{id}-none")))
                    .label("Don’t change")
                    .selected(current.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_game_target(game.as_deref(), &id, None, cx);
                    })),
            );
        }
        for (slot, slot_name) in &slots {
            let slot = *slot;
            let game = game.clone();
            let id = id.clone();
            buttons.push(
                Button::new(SharedString::from(format!("target-{id}-{slot}")))
                    .label(slot_name.clone())
                    .selected(current == Some(slot))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_game_target(game.as_deref(), &id, Some(slot), cx);
                    })),
            );
        }
        let rename = rename_menu(&id, &slots, cx);
        rows.push(
            h_flex()
                .id(row_id)
                .gap_4()
                .child(name)
                .child(
                    ButtonGroup::new(SharedString::from(format!("targets-{id}")))
                        .outline()
                        .small()
                        .children(buttons),
                )
                .child(div().flex_1())
                .child(rename)
                .into_any_element(),
        );
    }
    let theme = cx.theme();
    v_flex()
        .id("device-preset-matrix")
        .gap_3()
        .pt_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child("Device presets"),
        )
        .when(rows.is_empty(), |this| {
            this.child(muted_note("No devices connected.", theme))
        })
        .children(rows)
        .into_any_element()
}

fn rename_menu(
    device: &DeviceId,
    slots: &[(u8, String)],
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let entity = cx.entity().downgrade();
    let device = device.clone();
    let slots = slots.to_vec();
    Button::new(SharedString::from(format!("rename-presets-{device}")))
        .ghost()
        .small()
        .label("Rename preset")
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            for (slot, name) in &slots {
                let entity = entity.clone();
                let device = device.clone();
                let slot = *slot;
                menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                    move |_, window, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.open_rename_preset(device.clone(), slot, window, cx);
                        });
                    },
                ));
            }
            menu
        })
}

fn render_discovery(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let ui = &app.games_ui;
    let controls = app.controls.as_ref();
    let scan = Button::new("scan-games")
        .outline()
        .small()
        .icon(IconName::Search)
        .label(if ui.is_scanning {
            "Scanning…"
        } else if ui.scanned.is_some() {
            "Scan again"
        } else {
            "Scan installed games"
        })
        .loading(ui.is_scanning)
        .disabled(ui.is_scanning)
        .on_click(cx.listener(|this, _, _, cx| this.scan_games(cx)));

    let mut results: Vec<AnyElement> = Vec::new();
    let mut hidden = 0usize;
    if let Some(scanned) = &ui.scanned {
        let query = ui.search.to_string();
        let matching: Vec<&DetectedGame> = scanned
            .iter()
            .filter(|g| matches_search(&g.name, &query) || matches_search(&g.exe_name, &query))
            .collect();
        hidden = matching.len().saturating_sub(MAX_SCAN_RESULTS);
        for game in matching.into_iter().take(MAX_SCAN_RESULTS) {
            let added = app.has_game_exe(&game.exe_name);
            let game = game.clone();
            let theme = cx.theme();
            let key = normalize_exe_name(&game.exe_name);
            results.push(
                h_flex()
                    .id(SharedString::from(format!("detected-{key}")))
                    .gap_3()
                    .py_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(game.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{} · {}", game.source, game.exe_name)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("add-detected-{key}")))
                            .outline()
                            .xsmall()
                            .label(if added { "Added" } else { "Add" })
                            .disabled(added)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_detected_game(&game, window, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
    }

    let theme = cx.theme();
    let scanned_count = ui.scanned.as_ref().map(Vec::len);
    let search = controls.map(|c| c.game_search.clone());
    let picker = v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_3()
                .child(scan)
                .child(div().flex_1())
                .when_some(search.filter(|_| scanned_count.is_some()), |this, search| {
                    this.child(
                        div().w_64().child(
                            Input::new(&search)
                                .small()
                                .cleanable(true)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search).xsmall()),
                        ),
                    )
                }),
        )
        .map(|this| match scanned_count {
            None => this.child(muted_note(
                "Looks through Steam, Epic Games, Riot Games and installed programs. Nothing leaves this PC.",
                theme,
            )),
            Some(0) => this.child(muted_note(
                "No installed games were found. Add a game by its executable below.",
                theme,
            )),
            Some(_) if results.is_empty() => {
                this.child(muted_note("No games match the search.", theme))
            }
            Some(_) => this
                .child(
                    v_flex()
                        .id("detected-games")
                        .max_h_80()
                        .overflow_y_scroll()
                        // The list scrolls itself; keep the eased workspace scroll from moving too.
                        .on_scroll(|_, _, cx| cx.stop_propagation())
                        .children(results),
                )
                .when(hidden > 0, |this| {
                    this.child(muted_note(
                        format!("{hidden} more. Refine the search to see them."),
                        theme,
                    ))
                }),
        });

    let manual = controls.map(|c| (c.manual_name.clone(), c.manual_exe.clone()));
    let form = h_flex()
        .gap_2()
        .when_some(manual, |this, (name, exe)| {
            this.child(
                div()
                    .id("manual-game-name")
                    .w_56()
                    .child(Input::new(&name).small()),
            )
            .child(
                div()
                    .id("manual-game-exe")
                    .flex_1()
                    .child(Input::new(&exe).small()),
            )
        })
        .child(
            Button::new("add-manual-game")
                .primary()
                .small()
                .icon(IconName::Plus)
                .label("Add game")
                .on_click(cx.listener(|this, _, window, cx| this.add_manual_game(window, cx))),
        );
    group("game-discovery", "Add a game", theme)
        .child(picker)
        .child(
            v_flex()
                .gap_2()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child("Add by executable"),
                )
                .child(form)
                .when_some(app.games_ui.form_error.clone(), |this, err| {
                    this.child(div().text_xs().text_color(theme.danger).child(err))
                }),
        )
}
