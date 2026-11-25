//! Keybind remapping: layer selector, undo / redo history, restore default, and a key
//! picker with standard keyboard keys and extra keys (macros, volume, system groups).

use agnoperiph_core::{KeyCode, KeyIndex};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariant, ButtonVariants},
    h_flex,
    input::Input,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, group, muted_note, section};
use crate::backend::{DeviceSnapshot, KeymapState, SettingChange};
use crate::key_matrix::{self, KeyFace, KeyTone};
use crate::keycodes::{
    EXTRA_KEYS, ExtraKeyGroup, HID_KEYS, KeyGroup, keycode_name, matches_search,
};
use crate::keymap::KeyLayout;

/// One remap, kept so it can be undone and redone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeymapEdit {
    layer: u8,
    key: KeyIndex,
    before: KeyCode,
    after: KeyCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PickerTab {
    #[default]
    Keyboard,
    Extra,
}

/// Keybind view state: layer, picker tab, search and local history.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeymapUi {
    pub(crate) layer: u8,
    pub(crate) picker: PickerTab,
    pub(crate) search: SharedString,
    undo: Vec<KeymapEdit>,
    redo: Vec<KeymapEdit>,
}

impl KeymapUi {
    /// Drops undo / redo history; a re-read keymap may no longer match it.
    pub(crate) fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

/// Mac system keys are intentionally deferred on Windows.
const DEFERRED_MAC_GROUP: (&str, &str) = ("Mac system keys", "keymap-extra-mac");

impl AgnoPeriphApp {
    fn keymap_state(&self) -> Option<&KeymapState> {
        self.selected_device()?.keymap.as_ref()
    }

    fn current_code(&self, layer: u8, key: KeyIndex) -> Option<KeyCode> {
        self.keymap_state()?
            .snapshot
            .layers
            .get(usize::from(layer))?
            .get(usize::from(key.value()))
            .copied()
    }

    fn set_keymap_layer(&mut self, layer: u8, cx: &mut Context<Self>) {
        if self.keymap_ui.layer != layer {
            self.keymap_ui.layer = layer;
            cx.notify();
        }
    }

    fn set_picker_tab(&mut self, tab: PickerTab, cx: &mut Context<Self>) {
        if self.keymap_ui.picker != tab {
            self.keymap_ui.picker = tab;
            cx.notify();
        }
    }

    /// Binds the selected key on the current layer to `code`.
    pub(crate) fn assign_key(&mut self, code: KeyCode, cx: &mut Context<Self>) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        let layer = self.keymap_ui.layer;
        let key = self.selected_key;
        let Some(before) = self.current_code(layer, key) else {
            return;
        };
        if before == code {
            return;
        }
        self.keymap_ui.undo.push(KeymapEdit {
            layer,
            key,
            before,
            after: code,
        });
        self.keymap_ui.redo.clear();
        self.apply_change(SettingChange::KeyBinding { layer, key, code }, cx);
    }

    fn undo_keymap(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        if let Some(edit) = self.keymap_ui.undo.pop() {
            self.keymap_ui.redo.push(edit);
            self.apply_change(
                SettingChange::KeyBinding {
                    layer: edit.layer,
                    key: edit.key,
                    code: edit.before,
                },
                cx,
            );
        }
    }

    fn redo_keymap(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        if let Some(edit) = self.keymap_ui.redo.pop() {
            self.keymap_ui.undo.push(edit);
            self.apply_change(
                SettingChange::KeyBinding {
                    layer: edit.layer,
                    key: edit.key,
                    code: edit.after,
                },
                cx,
            );
        }
    }

    fn reset_keymap(&mut self, cx: &mut Context<Self>) {
        self.keymap_ui.clear_history();
        self.apply_change(SettingChange::ResetKeymap, cx);
    }

    fn request_keymap_reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_dry_run() {
            self.reset_keymap(cx);
            return;
        }
        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            alert
                .confirm()
                .title("Restore the default keymap?")
                .description(
                    "Every remapped key on all layers returns to its factory binding, including keys bound to macros, DKS, Hold/Click and Toggle.",
                )
                .ok_text("Restore default")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let _ = app.update(cx, super::app::AgnoPeriphApp::reset_keymap);
                    true
                })
        });
    }
}

fn keymap_select(
    app: &mut AgnoPeriphApp,
    key: KeyIndex,
    _extend: bool,
    _: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    app.select_key(key, false, cx);
}

/// Grid for a keyboard without a physical layout table, with each key named after what it
/// sends on the base layer (as VIA does). Only standard HID usages (`class 0x00`) are named;
/// unused positions and vendor-specific codes keep their `R#C#` name.
fn base_keycode_grid(keymap: &KeymapState) -> KeyLayout {
    let caps = &keymap.capabilities;
    let mut layout = KeyLayout::grid(
        u16::from(caps.rows) * u16::from(caps.cols),
        u16::from(caps.cols),
    );
    let Some(base) = keymap.snapshot.layers.first() else {
        return layout;
    };
    for cap in layout.rows.iter_mut().flatten() {
        if let Some(&code) = base.get(usize::from(cap.index.value()))
            && code.class == 0x00
            && code.id > 0x01
        {
            cap.label = keycode_name(code).into();
        }
    }
    layout
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let base = section(
        "keymap-panel",
        "Keybinds",
        "Select a key on the keyboard, then choose what it sends below.",
        cx.theme(),
    );
    let Some(keymap) = device.keymap.as_ref() else {
        return base.child(muted_note("The keymap couldn’t be read.", cx.theme()));
    };
    let (layout, is_named_by_keycode) = match device.hall_effect.as_ref() {
        Some(he) => (KeyLayout::for_device(&device.info, &he.capabilities), false),
        None => (std::sync::Arc::new(base_keycode_grid(keymap)), true),
    };
    let busy = app.is_busy() || app.is_loading();
    let ui = &app.keymap_ui;
    let layer = ui.layer;
    let layer_codes = keymap
        .snapshot
        .layers
        .get(usize::from(layer))
        .cloned()
        .unwrap_or_default();
    let selected = app.selected_key();
    let current = layer_codes.get(usize::from(selected.value())).copied();

    let layers = keymap
        .capabilities
        .layer_names
        .iter()
        .enumerate()
        .map(|(ix, name)| {
            let ix = u8::try_from(ix).unwrap_or(0);
            Button::new(("keymap-layer", u64::from(ix)))
                .label(name.clone())
                .selected(ix == layer)
                .on_click(cx.listener(move |this, _, _, cx| this.set_keymap_layer(ix, cx)))
        });
    let toolbar = h_flex()
        .gap_2()
        .child(
            ButtonGroup::new("keymap-layers")
                .outline()
                .small()
                .children(layers),
        )
        .child(div().flex_1())
        .child(
            Button::new("keymap-undo")
                .ghost()
                .small()
                .icon(IconName::Undo2)
                .label("Undo")
                .disabled(busy || ui.undo.is_empty())
                .on_click(cx.listener(|this, _, _, cx| this.undo_keymap(cx))),
        )
        .child(
            Button::new("keymap-redo")
                .ghost()
                .small()
                .icon(IconName::Redo2)
                .label("Redo")
                .disabled(busy || ui.redo.is_empty())
                .on_click(cx.listener(|this, _, _, cx| this.redo_keymap(cx))),
        )
        .when(keymap.capabilities.supports_reset, |this| {
            this.child(
                Button::new("keymap-reset")
                    .outline()
                    .small()
                    .label("Restore default…")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.request_keymap_reset(window, cx);
                    })),
            )
        });

    let codes = layer_codes.clone();
    let matrix = key_matrix::render(
        "keymap-matrix",
        "keymap-key",
        &layout,
        |cap| {
            let code = codes.get(usize::from(cap.index.value())).copied();
            let is_default = code.is_none_or(|c| c == KeyCode::TRANSPARENT);
            KeyFace {
                detail: code
                    .filter(|_| !is_default)
                    .map(|c| SharedString::from(keycode_name(c)))
                    .filter(|name| !is_named_by_keycode || *name != cap.label),
                fill: None,
                marker: false,
                badge: None,
                tone: if cap.index == selected {
                    KeyTone::Selected
                } else {
                    KeyTone::Normal
                },
            }
        },
        keymap_select,
        cx,
    );
    let selection_note = format!(
        "{} on {} sends {}",
        layout.label(selected),
        keymap
            .capabilities
            .layer_names
            .get(usize::from(layer))
            .cloned()
            .unwrap_or_default(),
        current.map_or_else(|| "nothing".to_owned(), keycode_name),
    );
    let picker = render_picker(app, current, busy, cx);
    let theme = cx.theme();
    base.child(toolbar)
        .child(matrix)
        .child(muted_note(selection_note, theme))
        .child(picker)
}

fn render_picker(
    app: &AgnoPeriphApp,
    current: Option<KeyCode>,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let ui = &app.keymap_ui;
    let query = ui.search.to_string();
    let tab = ui.picker;
    let tabs = ButtonGroup::new("keymap-picker-tabs")
        .outline()
        .small()
        .child(
            Button::new("keymap-picker-keyboard")
                .label("Keyboard keys")
                .selected(tab == PickerTab::Keyboard)
                .on_click(
                    cx.listener(|this, _, _, cx| this.set_picker_tab(PickerTab::Keyboard, cx)),
                ),
        )
        .child(
            Button::new("keymap-picker-extra")
                .label("Extra keys")
                .selected(tab == PickerTab::Extra)
                .on_click(cx.listener(|this, _, _, cx| this.set_picker_tab(PickerTab::Extra, cx))),
        );
    let search = app.controls.as_ref().map(|c| c.key_search.clone());
    let header = h_flex()
        .gap_3()
        .child(tabs)
        .child(div().flex_1())
        .when_some(search, |this, search| {
            this.child(
                div().w_64().child(
                    Input::new(&search)
                        .small()
                        .cleanable(true)
                        .prefix(gpui_kit::component::Icon::new(IconName::Search).xsmall()),
                ),
            )
        });

    let key_button = |id: (&'static str, u64),
                      label: SharedString,
                      code: KeyCode,
                      cx: &mut Context<AgnoPeriphApp>| {
        Button::new(id)
            .outline()
            .small()
            .label(label)
            .selected(current == Some(code))
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| this.assign_key(code, cx)))
            .into_any_element()
    };

    let mut groups: Vec<AnyElement> = Vec::new();
    match tab {
        PickerTab::Keyboard => {
            for group_kind in KeyGroup::ALL {
                let keys: Vec<AnyElement> = HID_KEYS
                    .iter()
                    .filter(|k| k.group == group_kind && matches_search(k.name, &query))
                    .map(|k| {
                        key_button(
                            ("hid-key", u64::from(k.usage)),
                            k.name.into(),
                            KeyCode::standard(k.usage),
                            cx,
                        )
                    })
                    .collect();
                if !keys.is_empty() {
                    groups.push(picker_group(group_kind.label(), keys, cx.theme()));
                }
            }
        }
        PickerTab::Extra => {
            let macro_slots = app
                .selected_device()
                .and_then(|d| d.macros.as_ref())
                .map_or(16, |m| m.capabilities.max_slots);
            let macros: Vec<AnyElement> = (0..macro_slots)
                .filter(|ix| matches_search(&format!("M{} macro", ix + 1), &query))
                .map(|ix| {
                    key_button(
                        ("macro-key", u64::from(ix)),
                        format!("M{}", ix + 1).into(),
                        KeyCode::macro_slot(ix),
                        cx,
                    )
                })
                .collect();
            if !macros.is_empty() {
                groups.push(picker_group("Quick commands (macros)", macros, cx.theme()));
            }
            for extra_group in ExtraKeyGroup::ALL {
                let keys: Vec<AnyElement> = EXTRA_KEYS
                    .iter()
                    .filter(|k| {
                        k.group == extra_group
                            && (matches_search(k.name, &query)
                                || matches_search(k.short_name, &query))
                    })
                    .map(|k| {
                        let id_num = (u64::from(k.class) << 8) | u64::from(k.id);
                        key_button(("extra-key", id_num), k.name.into(), k.code(), cx)
                    })
                    .collect();
                if !keys.is_empty() {
                    groups.push(picker_group(extra_group.label(), keys, cx.theme()));
                }
            }
            let (title, id) = DEFERRED_MAC_GROUP;
            if matches_search(title, &query) {
                let theme = cx.theme();
                groups.push(
                    v_flex()
                        .id(id)
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .child(title),
                        )
                        .child(muted_note("Deferred for a later release.", theme))
                        .into_any_element(),
                );
            }
        }
    }
    let theme = cx.theme();
    group("keymap-picker", "Choose a key", theme)
        .child(header)
        .when(groups.is_empty(), |this| {
            this.child(muted_note("No keys match the search.", theme))
        })
        .children(groups)
}

fn picker_group(
    title: &'static str,
    keys: Vec<AnyElement>,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    v_flex()
        .gap_1p5()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(title),
        )
        .child(h_flex().flex_wrap().gap_1().children(keys))
        .into_any_element()
}
