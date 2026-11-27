//! Keybind remapping: the board per layer, what the selected key sends (with undo, redo and
//! restore), and a searchable list of everything it can be set to.

use agnoperiph_core::{DeviceInfo, KeyCode, KeyIndex};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonCustomVariant, ButtonVariant, ButtonVariants},
    h_flex,
    input::Input,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::{DeviceSnapshot, KeymapState, SettingChange, offers_atk_keycodes};
use crate::design::{KEY_FONT, dp, tokens};
use crate::key_matrix::{self, BOARD_WIDTH, KeyContent, KeyFace, KeyTone};
use crate::keycodes::{
    EXTRA_KEYS, ExtraKey, ExtraKeyGroup, HID_KEYS, KeyGroup, keycode_name, matches_search,
};
use crate::keymap::{KeyLayout, LayoutKey, legend};
use crate::macro_panel::custom_name;
use crate::widgets::{keycap, panel, panel_head, panel_title, segment, segmented};

const GAP: f32 = 16.0;
const MIN_ASIDE_WIDTH: f32 = 220.0;
/// Standard usages in the order they are offered: the keys most often remapped first.
const BASIC_GROUPS: [KeyGroup; 8] = [
    KeyGroup::Editing,
    KeyGroup::Modifiers,
    KeyGroup::Navigation,
    KeyGroup::Letters,
    KeyGroup::Numbers,
    KeyGroup::Symbols,
    KeyGroup::Function,
    KeyGroup::Numpad,
];

/// One remap, kept so it can be undone and redone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeymapEdit {
    layer: u8,
    key: KeyIndex,
    before: KeyCode,
    after: KeyCode,
}

/// A category of the assign list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PickerTab {
    #[default]
    Basic,
    Media,
    Macro,
    Lighting,
}

impl PickerTab {
    const ALL: [Self; 4] = [Self::Basic, Self::Media, Self::Macro, Self::Lighting];

    fn label(self) -> &'static str {
        match self {
            Self::Basic => "Basic",
            Self::Media => "Media",
            Self::Macro => "Macro",
            Self::Lighting => "Lighting",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Basic => "keymap-picker-basic",
            Self::Media => "keymap-picker-media",
            Self::Macro => "keymap-picker-macro",
            Self::Lighting => "keymap-picker-lighting",
        }
    }
}

/// Keybind view state: layer, category, search and local history.
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

/// Layout for a keyboard known only by its keymap, with each key named after what it sends
/// on the base layer (as VIA does). Positions that send nothing keep their `R#C#` name.
pub(crate) fn base_keycode_layout(info: &DeviceInfo, keymap: &KeymapState) -> KeyLayout {
    let caps = &keymap.capabilities;
    let mut layout = KeyLayout::for_keymap(info, caps.rows, caps.cols);
    let Some(base) = keymap.snapshot.layers.first() else {
        return layout;
    };
    for cap in layout.rows.iter_mut().flatten() {
        if let Some(&code) = base.get(usize::from(cap.index.value()))
            && code != KeyCode::TRANSPARENT
            && !(code.class == 0x00 && code.id <= 0x01)
        {
            cap.label = keycode_name(code).into();
        }
    }
    layout
}

/// What `code` makes the key at `cap` do, when that isn't what the keycap says.
fn remapped_to(code: KeyCode, cap: &LayoutKey) -> Option<String> {
    // A momentary-layer code on the Fn key is that key's own job.
    let is_own_layer_key = code.class == 0x52 && cap.label == "Fn";
    if code == KeyCode::TRANSPARENT || is_own_layer_key {
        return None;
    }
    let name = keycode_name(code);
    (legend(&name) != legend(&cap.label)).then_some(name)
}

/// Everything the keybind views read about the current layer.
struct LayerView<'a> {
    names: &'a [String],
    layer: u8,
    codes: Vec<KeyCode>,
    /// What the keyboard holds, to mark edits that are not applied yet.
    device_codes: Vec<KeyCode>,
    /// Whether keycaps carry factory legends. A keyboard known only by its keymap names
    /// keys after what they send, so nothing on its base layer can look remapped.
    is_printed: bool,
}

impl LayerView<'_> {
    fn name(&self, layer: u8) -> &str {
        self.names
            .get(usize::from(layer))
            .map_or("this layer", String::as_str)
    }

    fn code(&self, key: KeyIndex) -> Option<KeyCode> {
        self.codes.get(usize::from(key.value())).copied()
    }

    fn has_remaps_to_show(&self) -> bool {
        self.is_printed || self.layer > 0
    }

    /// What the key at `cap` sends, when that differs from what the keycap says.
    fn remap(&self, cap: &LayoutKey) -> Option<String> {
        self.code(cap.index).and_then(|code| remapped_to(code, cap))
    }
}

/// The code a key's printed legend names, offered as its default on the base layer. Other
/// layers have no known default: which layer a transparent key falls back to depends on the
/// keyboard.
fn default_code(layer: u8, layout: &KeyLayout, key: KeyIndex, is_printed: bool) -> Option<KeyCode> {
    if layer > 0 || !is_printed {
        return None;
    }
    let label = layout.label(key);
    let wanted = legend(&label);
    let mut matches = HID_KEYS.iter().filter(|k| legend(k.name) == wanted);
    let left = matches.next()?;
    // Two-sided modifiers share a legend, so take the side the key sits on.
    let usage = match matches.next() {
        Some(right) if layout.key_centers().get(&key).is_some_and(|x| *x > 0.5) => right.usage,
        _ => left.usage,
    };
    Some(KeyCode::standard(usage))
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    workspace_width: f32,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let Some(keymap) = device.keymap.as_ref() else {
        return panel("keymap-panel", cx)
            .child(panel_head("Keybinds"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("The keymap couldn’t be read."),
            )
            .into_any_element();
    };
    let is_printed = device.hall_effect.is_some();
    let layout = match device.hall_effect.as_ref() {
        Some(he) => KeyLayout::for_device(&device.info, &he.capabilities),
        None => std::sync::Arc::new(base_keycode_layout(&device.info, keymap)),
    };
    let layer = app.keymap_ui.layer;
    let codes_of = |keymap: &KeymapState| {
        keymap
            .snapshot
            .layers
            .get(usize::from(layer))
            .cloned()
            .unwrap_or_default()
    };
    let view = LayerView {
        names: &keymap.capabilities.layer_names,
        layer,
        codes: codes_of(keymap),
        device_codes: app
            .selected_raw()
            .and_then(|d| d.keymap.as_ref())
            .map_or_else(Vec::new, codes_of),
        is_printed,
    };

    let board = board_column(app, &view, &layout, cx);
    let inspector = key_inspector(app, keymap, &view, &layout, cx);
    let default = default_code(layer, &layout, app.selected_key(), view.is_printed);
    let assign = assign_panel(app, &view, &layout, default, cx);
    let top = if workspace_width >= BOARD_WIDTH + GAP + MIN_ASIDE_WIDTH {
        h_flex()
            .items_stretch()
            .gap(dp(GAP))
            .child(board)
            .child(v_flex().flex_1().min_w_0().child(inspector))
    } else {
        v_flex().gap(dp(14.)).child(board).child(inspector)
    };
    v_flex()
        .id("keymap-panel")
        .gap(dp(14.))
        .child(top)
        .child(assign)
        .into_any_element()
}

/// The keyboard on the current layer, with the layer switch under it.
fn board_column(
    app: &AgnoPeriphApp,
    view: &LayerView<'_>,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> Div {
    let selected = app.selected_key();
    let lip = keymap_lip(view, cx);
    let matrix = key_matrix::render(
        "keymap-matrix",
        "keymap-key",
        layout,
        BOARD_WIDTH,
        |cap| {
            let at = usize::from(cap.index.value());
            let code = view.codes.get(at).copied();
            let is_default = code.is_none_or(|c| c == KeyCode::TRANSPARENT);
            let sends = view.remap(cap);
            let content = match &sends {
                Some(name) => KeyContent::Remap(legend(name).to_owned().into()),
                None if view.layer > 0 && is_default => KeyContent::Inherit,
                None => KeyContent::Legend,
            };
            let tone = if cap.index == selected {
                KeyTone::Selected
            } else {
                KeyTone::Normal
            };
            let mut face = KeyFace::new(content, tone);
            face.is_pending = view.device_codes.get(at) != view.codes.get(at);
            face.description = sends.map(|name| format!("sends {name}").into());
            face
        },
        keymap_select,
        lip,
        cx,
    );

    let remapped = layout
        .rows
        .iter()
        .flatten()
        .filter(|cap| view.remap(cap).is_some())
        .count();
    let note = view.has_remaps_to_show().then(|| match remapped {
        0 => "No keys remapped on this layer".to_owned(),
        1 => "1 key remapped on this layer".to_owned(),
        count => format!("{count} keys remapped on this layer"),
    });
    let layers = (view.names.len() > 1).then(|| {
        let mut layers = segmented(cx);
        for (ix, name) in view.names.iter().enumerate() {
            let ix = u8::try_from(ix).unwrap_or(0);
            layers = layers.child(
                segment(
                    Button::new(("keymap-layer", u64::from(ix))),
                    name.clone(),
                    ix == view.layer,
                    cx,
                )
                .h(dp(24.))
                .px(dp(10.))
                .on_click(cx.listener(move |this, _, _, cx| this.set_keymap_layer(ix, cx))),
            );
        }
        layers
    });
    let muted = cx.theme().muted_foreground;
    let bar = h_flex()
        .gap(dp(8.))
        .min_h(dp(34.))
        .mt(dp(4.))
        .text_size(dp(12.5))
        .when_some(layers, |this, layers| {
            this.child(div().text_color(muted).child("Layer"))
                .child(layers)
        })
        .child(div().flex_1())
        .children(note.map(|note| div().text_color(muted).child(note)));
    v_flex().flex_shrink_0().child(matrix).child(bar)
}

fn keymap_lip(view: &LayerView<'_>, cx: &App) -> Vec<AnyElement> {
    let theme = cx.theme();
    let mut lip = Vec::new();
    if !view.has_remaps_to_show() {
        let note = "Keys are named after what they send";
        lip.push(div().child(note).into_any_element());
    }
    if view.has_remaps_to_show() {
        let remapped = div()
            .size(dp(12.))
            .rounded(dp(3.))
            .border_1()
            .border_color(theme.primary)
            .bg(theme.primary.opacity(0.35));
        lip.push(key_matrix::legend_item(remapped, "Remapped"));
    }
    if view.layer > 0 {
        let inherited = div()
            .size(dp(12.))
            .rounded(dp(3.))
            .border_1()
            .border_color(tokens(cx).case_text);
        lip.push(key_matrix::legend_item(inherited, "Not set on this layer"));
    }
    if view.device_codes != view.codes {
        let swatch = div().w(dp(14.)).h(dp(4.)).rounded_full().bg(theme.warning);
        lip.push(key_matrix::legend_item(swatch, "Not applied"));
    }
    lip.push(div().flex_1().into_any_element());
    if view.has_remaps_to_show() {
        let small_legend = if view.is_printed {
            "Small legend shows the original key"
        } else {
            "Small legend shows the base layer key"
        };
        lip.push(div().flex_shrink_0().child(small_legend).into_any_element());
    }
    lip
}

/// The keycap a remapped key now acts as, tinted like its cap on the board.
fn remap_keycap(label: SharedString, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_shrink_0()
        .justify_center()
        .min_w(dp(46.))
        .h(dp(48.))
        .px(dp(10.))
        .rounded(theme.radius_lg)
        .border_2()
        .border_color(theme.primary)
        .bg(tokens(cx).accent_soft)
        .font_family(KEY_FONT)
        .text_size(dp(18.))
        .text_color(theme.primary)
        .whitespace_nowrap()
        .child(label)
}

/// The selected key: what it sends, history, and every remap on the layer.
fn key_inspector(
    app: &AgnoPeriphApp,
    keymap: &KeymapState,
    view: &LayerView<'_>,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy() || app.is_loading();
    let ui = &app.keymap_ui;
    let selected = app.selected_key();
    let label = layout.label(selected);
    let code = view.code(selected);
    let sends = layout
        .rows
        .iter()
        .flatten()
        .find(|cap| cap.index == selected)
        .and_then(|cap| view.remap(cap));
    let is_unset = view.layer > 0 && code == Some(KeyCode::TRANSPARENT);
    let note = match (&sends, code) {
        (Some(name), _) => format!("Sends {name} on {}.", view.name(view.layer)),
        (None, None) => "This key’s binding couldn’t be read.".to_owned(),
        (None, Some(_)) if is_unset => format!("Not set on {}.", view.name(view.layer)),
        (None, Some(code)) if code.class == 0x52 => {
            "Switches to another layer while held.".to_owned()
        }
        _ if !view.has_remaps_to_show() => {
            format!("Sends {} on {}.", legend(&label), view.name(view.layer))
        }
        _ => "Sends its default.".to_owned(),
    };
    let original: SharedString = legend(&label).to_owned().into();
    let current = match &sends {
        Some(name) => remap_keycap(legend(name).to_owned().into(), cx),
        // Echoing the printed legend here would suggest the key still sends it.
        None if is_unset => keycap("–", 46., 18., cx).opacity(0.5),
        None => keycap(original.clone(), 46., 18., cx),
    };

    let history = |id: &'static str, icon: IconName, name: &'static str, is_empty: bool| {
        Button::new(id)
            .ghost()
            .small()
            .icon(icon)
            .tooltip(name)
            .accessibility_label(name)
            .disabled(busy || is_empty)
    };
    let head = h_flex()
        .gap_1()
        .min_h(dp(26.))
        .mb(dp(10.))
        .child(panel_title("Key"))
        .child(div().flex_1())
        .child(
            history(
                "keymap-undo",
                IconName::Undo2,
                "Undo remap",
                ui.undo.is_empty(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.undo_keymap(cx))),
        )
        .child(
            history(
                "keymap-redo",
                IconName::Redo2,
                "Redo remap",
                ui.redo.is_empty(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.redo_keymap(cx))),
        );

    let chips: Vec<AnyElement> = layout
        .rows
        .iter()
        .flatten()
        .filter_map(|cap| view.remap(cap).map(|name| (cap, name)))
        .map(|(cap, name)| remap_chip(cap, &name, cap.index == selected, cx))
        .collect();
    let muted = cx.theme().muted_foreground;
    let line = cx.theme().border;
    panel("keymap-inspector", cx)
        .size_full()
        .child(head)
        .child(
            h_flex()
                .gap(dp(10.))
                .mb(dp(10.))
                .child(keycap(original, 46., 18., cx))
                .child(Icon::new(IconName::ArrowRight).small().text_color(muted))
                .child(current),
        )
        .child(div().text_size(dp(12.5)).text_color(muted).child(note))
        .when(view.has_remaps_to_show(), |this| {
            let empty = if view.layer > 0 {
                "Nothing set on this layer yet."
            } else {
                "Nothing yet. Every key sends its default."
            };
            this.child(div().h_px().my(dp(12.)).bg(line))
                .child(
                    div()
                        .mb(dp(6.))
                        .text_size(dp(12.))
                        .text_color(muted)
                        .child(format!("Remapped on {}", view.name(view.layer))),
                )
                .map(|this| {
                    if chips.is_empty() {
                        this.child(div().text_size(dp(12.5)).text_color(muted).child(empty))
                    } else {
                        this.child(h_flex().flex_wrap().gap(dp(5.)).children(chips))
                    }
                })
        })
        .child(div().flex_1())
        .when(keymap.capabilities.supports_reset, |this| {
            this.child(
                h_flex().mt(dp(12.)).child(
                    Button::new("keymap-reset")
                        .outline()
                        .small()
                        .label("Restore default keymap…")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.request_keymap_reset(window, cx);
                        })),
                ),
            )
        })
        .into_any_element()
}

fn remap_chip(
    cap: &LayoutKey,
    sends: &str,
    is_selected: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let key = cap.index;
    let theme = cx.theme();
    let original = legend(&cap.label).to_owned();
    let now = legend(sends).to_owned();
    Button::new(("keymap-remapped", u64::from(key.value())))
        .ghost()
        .selected(is_selected)
        .h(dp(24.))
        .px(dp(8.))
        .bg(theme.muted)
        .border_1()
        .border_color(if is_selected {
            theme.primary
        } else {
            theme.transparent
        })
        .accessibility_label(format!("{original} sends {now}"))
        .child(
            h_flex()
                .gap(dp(5.))
                .text_size(dp(12.))
                .child(div().font_family(KEY_FONT).child(original))
                .child(
                    Icon::new(IconName::ArrowRight)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(now),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.select_key(key, false, cx)))
        .into_any_element()
}

/// Something the selected key can be set to.
struct Choice {
    id: (&'static str, u64),
    label: SharedString,
    /// The longer name, searched along with the label.
    name: SharedString,
    code: KeyCode,
}

/// Keycap-sized name of a standard key; the right-hand modifiers are told apart.
fn basic_label(name: &str) -> SharedString {
    match name {
        "Backspace" => "Bksp".into(),
        "Print Screen" => "PrtSc".into(),
        "Scroll Lock" => "ScrLk".into(),
        name => match name.strip_prefix("Right ") {
            Some(modifier) => format!("R {modifier}").into(),
            None => name
                .strip_prefix("Left ")
                .unwrap_or(legend(name))
                .to_owned()
                .into(),
        },
    }
}

fn extra_choice(key: &ExtraKey) -> Choice {
    Choice {
        id: ("extra-key", (u64::from(key.class) << 8) | u64::from(key.id)),
        label: key.name.into(),
        name: key.short_name.into(),
        code: key.code(),
    }
}

fn choices(app: &AgnoPeriphApp, tab: PickerTab) -> Vec<Choice> {
    let has_vendor_codes = app
        .selected_device()
        .is_some_and(|d| offers_atk_keycodes(&d.info));
    let extras = |groups: [ExtraKeyGroup; 2]| {
        EXTRA_KEYS
            .iter()
            .filter(move |key| groups.contains(&key.group))
            // Class 0x7E codes are the vendor's own; other firmware reads them differently.
            .filter(move |key| has_vendor_codes || key.class != 0x7E)
            .map(extra_choice)
            .collect()
    };
    match tab {
        PickerTab::Basic => BASIC_GROUPS
            .iter()
            .flat_map(|group| HID_KEYS.iter().filter(move |key| key.group == *group))
            .map(|key| Choice {
                id: ("hid-key", u64::from(key.usage)),
                label: basic_label(key.name),
                name: key.name.into(),
                code: KeyCode::standard(key.usage),
            })
            .collect(),
        PickerTab::Media => extras([ExtraKeyGroup::Media, ExtraKeyGroup::Windows]),
        PickerTab::Lighting => extras([ExtraKeyGroup::Backlight, ExtraKeyGroup::LogoLight]),
        PickerTab::Macro => {
            let macros = app.selected_device().and_then(|d| d.macros.as_ref());
            let slots = macros.map_or(16, |m| m.capabilities.max_slots);
            (0..slots)
                .map(|ix| {
                    let name = macros
                        .and_then(|m| m.slots.iter().find(|slot| slot.index == ix))
                        .and_then(custom_name);
                    let number = ix + 1;
                    Choice {
                        id: ("macro-key", u64::from(ix)),
                        label: name.map_or_else(
                            || format!("M{number}").into(),
                            |name| format!("M{number} · {name}").into(),
                        ),
                        name: "macro".into(),
                        code: KeyCode::macro_slot(ix),
                    }
                })
                .collect()
        }
    }
}

/// Search, categories and the keys to choose from, with a per-key restore.
fn assign_panel(
    app: &AgnoPeriphApp,
    view: &LayerView<'_>,
    layout: &KeyLayout,
    default: Option<KeyCode>,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy() || app.is_loading();
    let selected = app.selected_key();
    let current = view.code(selected);
    let ui = &app.keymap_ui;
    let query = ui.search.trim().to_owned();
    let tab = ui.picker;
    let shown: Vec<Choice> = if query.is_empty() {
        choices(app, tab)
    } else {
        PickerTab::ALL
            .into_iter()
            .flat_map(|tab| choices(app, tab))
            .filter(|c| matches_search(&c.label, &query) || matches_search(&c.name, &query))
            .collect()
    };

    let can_restore = default.is_some_and(|code| current.is_some_and(|now| now != code));
    let muted = cx.theme().muted_foreground;
    let head = h_flex()
        .gap_2()
        .min_h(dp(26.))
        .mb(dp(10.))
        .child(panel_title("Assign"))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(dp(12.5))
                .text_color(muted)
                .child(format!(
                    "to {} on {}",
                    legend(&layout.label(selected)),
                    view.name(view.layer)
                )),
        )
        .child(div().flex_1())
        .when(view.layer == 0, |this| {
            this.child(
                Button::new("keymap-restore-key")
                    .outline()
                    .small()
                    .label("Restore default")
                    .disabled(busy || !can_restore)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(code) = default {
                            this.assign_key(code, cx);
                        }
                    })),
            )
        });

    let search = app.controls.as_ref().map(|c| c.key_search.clone());
    let categories = query.is_empty().then(|| {
        let mut categories = segmented(cx);
        for choice in PickerTab::ALL {
            categories = categories.child(
                segment(Button::new(choice.id()), choice.label(), choice == tab, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_picker_tab(choice, cx))),
            );
        }
        categories
    });
    let filters = h_flex()
        .gap(dp(10.))
        .mb(dp(10.))
        .when_some(search, |this, search| {
            this.child(
                div().flex_1().min_w_0().child(
                    Input::new(&search)
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).xsmall()),
                ),
            )
        })
        .children(categories);

    let is_empty = shown.is_empty();
    let chips: Vec<AnyElement> = shown
        .into_iter()
        .map(|choice| {
            let is_current = current == Some(choice.code);
            key_chip(choice, is_current, busy, cx)
        })
        .collect();
    panel("keymap-picker", cx)
        .child(head)
        .child(filters)
        .child(h_flex().flex_wrap().gap(dp(6.)).children(chips))
        .when(is_empty, |this| {
            this.child(
                div()
                    .text_size(dp(12.5))
                    .text_color(muted)
                    .child("Nothing matches. Try a key name such as F13 or Vol."),
            )
        })
        .into_any_element()
}

/// A key to assign, drawn as a small keycap.
fn key_chip(
    choice: Choice,
    is_current: bool,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let Choice {
        id, label, code, ..
    } = choice;
    let colors = tokens(cx);
    let theme = cx.theme();
    let (fill, hover, ink, skirt) = if is_current {
        (
            theme.primary,
            theme.primary,
            theme.primary_foreground,
            theme.primary,
        )
    } else {
        (
            colors.key_face,
            colors.key_face_hover,
            colors.key_legend,
            colors.key_skirt,
        )
    };
    let radius = theme.radius;
    let edge = theme.border;
    div()
        .flex_shrink_0()
        .rounded(radius)
        .border_1()
        .border_color(edge)
        .bg(skirt)
        .pb(dp(3.))
        .child(
            Button::new(id)
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(fill)
                        .foreground(ink)
                        .hover(hover)
                        .active(hover),
                )
                .selected(is_current)
                .h(dp(25.))
                .min_w(dp(36.))
                .px(dp(10.))
                .bg(fill)
                .text_color(ink)
                .accessibility_label(label.clone())
                .disabled(busy)
                .child(div().font_family(KEY_FONT).text_size(dp(12.)).child(label))
                .on_click(cx.listener(move |this, _, _, cx| this.assign_key(code, cx))),
        )
        .into_any_element()
}
