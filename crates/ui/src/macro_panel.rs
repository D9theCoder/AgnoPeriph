//! Onboard macro slots: the slot list with memory use, beside a step editor where each step
//! reads as "Press Shift" and carries its delay. Rename, reorder, clearing, cycle times where
//! the device stores them, and an explicit Save that writes every slot at once.

use std::collections::BTreeMap;

use agnoperiph_core::{MacroActionKind, MacroCapabilities, MacroSlot, MacroStep, MouseButton};
use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::{DeviceSnapshot, MacroState, SettingChange, macro_bytes_used};
use crate::design::{DISPLAY_FONT, dp, tabular, tokens};
use crate::hall_effect_panel::key_chooser;
use crate::widgets::{panel, panel_head, panel_title, segment, segmented, stepper_sized};

const LIST_WIDTH: f32 = 300.0;
const MIN_EDITOR_WIDTH: f32 = 480.0;
const GAP: f32 = 16.0;

/// Delay change per stepper click, in milliseconds.
const DELAY_STEP_MS: u16 = 10;
const MAX_DELAY_MS: u16 = 9_990;
const DEFAULT_DELAY_MS: u16 = 20;
/// Steps a new keyboard macro starts with use the Space key.
const DEFAULT_STEP_USAGE: u8 = 0x2C;
/// Default USB HID Consumer usage (`Play / Pause`) for newly added media steps.
const DEFAULT_MEDIA_USAGE: u16 = 0x00CD;
const MIN_REPEAT: u8 = 1;
const MAX_REPEAT: u8 = u8::MAX;

/// Standard USB HID Consumer Page (`0x0C`) usages offered by the media macro chooser.
const CONSUMER_MEDIA_USAGES: &[(u16, &str)] = &[
    (0x00CD, "Play / Pause"),
    (0x00B5, "Next Track"),
    (0x00B6, "Previous Track"),
    (0x00B7, "Stop"),
    (0x00E9, "Volume Up"),
    (0x00EA, "Volume Down"),
    (0x00E2, "Mute"),
    (0x0183, "Media Player"),
    (0x018A, "Mail"),
    (0x0192, "Calculator"),
    (0x0194, "My Computer"),
    (0x0221, "Search"),
    (0x0223, "Browser Home"),
    (0x0224, "Browser Back"),
    (0x0225, "Browser Forward"),
    (0x0227, "Browser Refresh"),
];

fn media_label(usage: u16) -> String {
    CONSUMER_MEDIA_USAGES
        .iter()
        .find(|(code, _)| *code == usage)
        .map_or_else(
            || format!("Media 0x{usage:04X}"),
            |(_, name)| (*name).to_owned(),
        )
}

/// Macro view state: the selected slot and the unsaved copy of every slot.
#[derive(Debug, Clone, Default)]
pub(crate) struct MacroUi {
    pub(crate) selected: u8,
    draft: Option<Vec<MacroSlot>>,
    /// Stable identities for draft steps, so reordering keeps each row's element state.
    step_ids: BTreeMap<u8, Vec<u64>>,
    next_step_id: u64,
}

impl MacroUi {
    pub(crate) fn clear_draft(&mut self) {
        self.draft = None;
        self.step_ids.clear();
    }

    /// Element identities for `slot`'s steps. Unedited slots fall back to positions,
    /// which are stable until the first edit assigns real ids.
    fn ids_for(&self, slot: &MacroSlot) -> Vec<u64> {
        match self.step_ids.get(&slot.index) {
            Some(ids) if ids.len() == slot.steps.len() => ids.clone(),
            _ => (0..slot.steps.len() as u64)
                .map(|ix| u64::MAX - ix)
                .collect(),
        }
    }

    pub(crate) fn has_draft(&self) -> bool {
        self.draft.is_some()
    }
}

/// The name a slot was given. `M{n}` is what drivers without stored names fill in.
pub(crate) fn custom_name(slot: &MacroSlot) -> Option<&str> {
    let name = slot.name.trim();
    (!name.is_empty() && name != format!("M{}", u16::from(slot.index) + 1)).then_some(name)
}

/// Display name of a macro slot, falling back to `Macro {n}`.
pub(crate) fn slot_name(slot: &MacroSlot) -> String {
    custom_name(slot).map_or_else(
        || format!("Macro {}", u16::from(slot.index) + 1),
        str::to_owned,
    )
}

/// Stable id suffix of a step kind.
fn kind_label(kind: MacroActionKind) -> &'static str {
    match kind {
        MacroActionKind::KeyDown | MacroActionKind::MouseDown | MacroActionKind::MediaDown => {
            "Down"
        }
        MacroActionKind::KeyUp | MacroActionKind::MouseUp | MacroActionKind::MediaUp => "Up",
        MacroActionKind::KeyTap | MacroActionKind::MediaTap => "Tap",
        MacroActionKind::MouseClick => "Click",
        MacroActionKind::PointerMove => "Move",
    }
}

/// What a step does, as the first word of its sentence.
fn kind_verb(kind: MacroActionKind) -> &'static str {
    match kind {
        MacroActionKind::KeyDown | MacroActionKind::MouseDown | MacroActionKind::MediaDown => {
            "Press"
        }
        MacroActionKind::KeyUp | MacroActionKind::MouseUp | MacroActionKind::MediaUp => "Release",
        MacroActionKind::KeyTap | MacroActionKind::MediaTap => "Tap",
        MacroActionKind::MouseClick => "Click",
        MacroActionKind::PointerMove => "Move",
    }
}

fn kind_icon(kind: MacroActionKind) -> Icon {
    match kind_verb(kind) {
        "Press" => Icon::new(LucideIcon::ArrowDown),
        "Release" => Icon::new(LucideIcon::ArrowUp),
        "Move" => Icon::new(LucideIcon::MousePointer2),
        _ => Icon::new(LucideIcon::ArrowDownUp),
    }
}

impl AgnoPeriphApp {
    /// Current slots, preferring the unsaved draft.
    pub fn macro_slots(&self) -> Vec<MacroSlot> {
        if let Some(draft) = &self.macros_ui.draft {
            return draft.clone();
        }
        self.selected_device()
            .and_then(|d| d.macros.as_ref())
            .map_or_default(|m| m.slots.clone())
    }

    fn edit_macro(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut MacroSlot, &mut Vec<u64>, &mut u64),
    ) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        let mut slots = self.macro_slots();
        let selected = self.macros_ui.selected;
        // Read-only slots hold events the driver cannot write back, so they never enter a draft.
        if let Some(slot) = slots
            .iter_mut()
            .find(|s| s.index == selected && !s.is_readonly)
        {
            let ui = &mut self.macros_ui;
            let ids = ui.step_ids.entry(selected).or_default();
            if ids.len() != slot.steps.len() {
                ids.clear();
                for _ in 0..slot.steps.len() {
                    ids.push(ui.next_step_id);
                    ui.next_step_id += 1;
                }
            }
            f(slot, ids, &mut ui.next_step_id);
            self.macros_ui.draft = Some(slots);
            cx.notify();
        }
    }

    fn select_macro(&mut self, index: u8, cx: &mut Context<Self>) {
        if self.macros_ui.selected != index {
            self.macros_ui.selected = index;
            cx.notify();
        }
    }

    fn add_macro_step(&mut self, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, next| {
            ids.push(*next);
            *next += 1;
            let usage = slot
                .steps
                .iter()
                .rev()
                .find(|s| s.kind.is_keyboard())
                .map_or(DEFAULT_STEP_USAGE, |s| s.hid_keycode);
            slot.steps.push(MacroStep::new(
                MacroActionKind::KeyTap,
                usage,
                DEFAULT_DELAY_MS,
            ));
        });
    }

    fn add_macro_mouse_step(&mut self, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, next| {
            ids.push(*next);
            *next += 1;
            let button = slot
                .steps
                .iter()
                .rev()
                .find_map(|s| s.mouse_button)
                .unwrap_or(MouseButton::Left);
            slot.steps.push(MacroStep::mouse(
                MacroActionKind::MouseClick,
                button,
                DEFAULT_DELAY_MS,
            ));
        });
    }

    fn add_macro_media_step(&mut self, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, next| {
            ids.push(*next);
            *next += 1;
            let usage = slot
                .steps
                .iter()
                .rev()
                .find(|s| s.kind.is_media())
                .map_or(DEFAULT_MEDIA_USAGE, |s| s.consumer_usage);
            slot.steps.push(MacroStep::media(
                MacroActionKind::MediaTap,
                usage,
                DEFAULT_DELAY_MS,
            ));
        });
    }

    fn add_macro_move_step(&mut self, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, next| {
            ids.push(*next);
            *next += 1;
            slot.steps
                .push(MacroStep::pointer_move(0, 0, DEFAULT_DELAY_MS));
        });
    }

    fn remove_macro_step(&mut self, step: usize, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, _| {
            if step < slot.steps.len() {
                slot.steps.remove(step);
                ids.remove(step);
            }
            // An empty slot keeps no trigger, so its cycle count cannot be stored.
            if slot.steps.is_empty() {
                slot.repeat = MIN_REPEAT;
            }
        });
    }

    fn move_macro_step(&mut self, step: usize, up: bool, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, _| {
            let other = if up {
                step.checked_sub(1)
            } else {
                Some(step + 1)
            };
            if let Some(other) = other.filter(|o| *o < slot.steps.len())
                && step < slot.steps.len()
            {
                slot.steps.swap(step, other);
                ids.swap(step, other);
            }
        });
    }

    fn set_step_kind(&mut self, step: usize, kind: MacroActionKind, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, _, _| {
            if let Some(s) = slot.steps.get_mut(step) {
                s.kind = kind;
            }
        });
    }

    fn set_step_key(&mut self, step: usize, usage: u8, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, _, _| {
            if let Some(s) = slot.steps.get_mut(step) {
                s.hid_keycode = usage;
            }
        });
    }

    fn set_step_mouse_button(&mut self, step: usize, button: MouseButton, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, _, _| {
            if let Some(s) = slot.steps.get_mut(step) {
                s.mouse_button = Some(button);
            }
        });
    }

    fn set_step_media(&mut self, step: usize, usage: u16, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, _, _| {
            if let Some(s) = slot.steps.get_mut(step) {
                s.consumer_usage = usage;
            }
        });
    }

    fn step_move_delta(&mut self, step: usize, axis_x: bool, delta: i8, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, _, _| {
            if let Some(s) = slot.steps.get_mut(step) {
                if axis_x {
                    s.move_x = s.move_x.saturating_add(delta);
                } else {
                    s.move_y = s.move_y.saturating_add(delta);
                }
            }
        });
    }

    fn step_delay(&mut self, step: usize, increase: bool, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, _, _| {
            if let Some(s) = slot.steps.get_mut(step) {
                s.delay_ms = if increase {
                    s.delay_ms.saturating_add(DELAY_STEP_MS).min(MAX_DELAY_MS)
                } else {
                    s.delay_ms.saturating_sub(DELAY_STEP_MS)
                };
            }
        });
    }

    fn step_repeat(&mut self, increase: bool, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, _, _| {
            slot.repeat = if increase {
                slot.repeat.saturating_add(1)
            } else {
                slot.repeat.saturating_sub(1)
            }
            .clamp(MIN_REPEAT, MAX_REPEAT);
        });
    }

    fn clear_macro(&mut self, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, _| {
            slot.steps.clear();
            ids.clear();
            slot.repeat = MIN_REPEAT;
        });
    }

    /// Renames the selected macro in the draft; empty restores `Macro {n}`.
    pub(crate) fn rename_macro(&mut self, index: u8, name: &str, cx: &mut Context<Self>) {
        let previous = self.macros_ui.selected;
        self.macros_ui.selected = index;
        let name = name.trim().to_owned();
        self.edit_macro(cx, |slot, _, _| slot.name = name);
        self.macros_ui.selected = previous;
    }

    fn discard_macros(&mut self, cx: &mut Context<Self>) {
        self.macros_ui.clear_draft();
        cx.notify();
    }

    fn save_macros(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.macros_ui.draft.clone() {
            self.apply_change(SettingChange::Macros(draft), cx);
        }
    }

    fn open_rename_macro(&mut self, index: u8, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .macro_slots()
            .iter()
            .find(|s| s.index == index)
            .map_or_default(slot_name);
        crate::game_profiles_view::open_rename(
            self,
            crate::game_profiles_view::RenameTarget::Macro(index),
            format!("Rename “{current}”"),
            &current,
            "Leave empty to use the slot number.",
            window,
            cx,
        );
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    workspace_width: f32,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let Some(state) = device.macros.as_ref() else {
        return panel("macro-panel", cx)
            .child(panel_head("Macros"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Macros couldn’t be read."),
            )
            .into_any_element();
    };
    let slots = app.macro_slots();
    let selected = app.macros_ui.selected;
    let list = slot_list(device, state, &slots, selected, cx);
    let editor = match slots.iter().find(|s| s.index == selected) {
        Some(slot) => render_editor(app, device, slot, state.capabilities, cx),
        None => panel("macro-steps", cx)
            .child("Select a macro slot.")
            .into_any_element(),
    };
    let body = if workspace_width >= LIST_WIDTH + GAP + MIN_EDITOR_WIDTH {
        h_flex()
            .items_start()
            .gap(dp(GAP))
            .child(v_flex().w(dp(LIST_WIDTH)).flex_shrink_0().child(list))
            .child(v_flex().flex_1().min_w_0().child(editor))
    } else {
        v_flex().gap(dp(14.)).child(list).child(editor)
    };
    div().id("macro-panel").child(body).into_any_element()
}

/// Every slot with its name and step count, over the memory it takes.
fn slot_list(
    device: &DeviceSnapshot,
    state: &MacroState,
    slots: &[MacroSlot],
    selected: u8,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let caps = state.capabilities;
    let rows: Vec<AnyElement> = slots
        .iter()
        .map(|slot| slot_row(slot, slot.index == selected, cx))
        .collect();
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let line = theme.border;
    let memory = match macro_bytes_used(&device.info, slots) {
        Some(used) => {
            let total = usize::from(caps.max_buffer_bytes.max(1));
            let used_share = f32::from(u16::try_from(used.min(total)).unwrap_or(u16::MAX))
                / f32::from(caps.max_buffer_bytes.max(1));
            let is_full = used > total;
            let fill = if is_full { theme.danger } else { theme.primary };
            v_flex()
                .gap(dp(6.))
                .child(
                    h_flex()
                        .justify_between()
                        .text_size(dp(12.))
                        .text_color(muted)
                        .child("Memory, all macros")
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .font_family(DISPLAY_FONT)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .font_features(tabular())
                                        .text_color(if is_full {
                                            theme.danger
                                        } else {
                                            theme.foreground
                                        })
                                        .child(used.to_string()),
                                )
                                .child(format!("of {total} bytes")),
                        ),
                )
                .child(
                    div()
                        .h(dp(4.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(theme.muted)
                        .child(div().h_full().w(relative(used_share)).bg(fill)),
                )
        }
        None => v_flex().text_size(dp(12.)).text_color(muted).child(format!(
            "{} bytes of macro memory on the device.",
            caps.max_buffer_bytes
        )),
    };
    panel("macro-slots", cx)
        .child(
            h_flex()
                .min_h(dp(26.))
                .mb(dp(10.))
                .child(panel_title("Macros"))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(dp(12.))
                        .text_color(muted)
                        .child(format!("{} slots", caps.max_slots)),
                ),
        )
        .child(v_flex().gap(dp(1.)).children(rows))
        .child(div().h_px().mt(dp(12.)).mb(dp(12.)).bg(line))
        .child(memory)
        .into_any_element()
}

fn slot_row(slot: &MacroSlot, is_selected: bool, cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let index = slot.index;
    let steps = slot.steps.len();
    let is_empty = steps == 0 && !slot.is_readonly;
    let name: SharedString = if is_empty && custom_name(slot).is_none() {
        "Empty".into()
    } else {
        slot_name(slot).into()
    };
    let count = if slot.is_readonly {
        Some("Read-only".to_owned())
    } else {
        match steps {
            0 => None,
            1 => Some("1 step".to_owned()),
            steps => Some(format!("{steps} steps")),
        }
    };
    let theme = cx.theme();
    let faint = tokens(cx).faint;
    let soft = tokens(cx).accent_soft;
    Button::new(("macro-slot", u64::from(index)))
        .ghost()
        .selected(is_selected)
        .w_full()
        .h(dp(28.))
        .px(dp(8.))
        .when(is_selected, |this| this.bg(soft))
        .accessibility_label(format!("M{}, {name}", index + 1))
        .child(
            h_flex()
                .w_full()
                .gap(dp(10.))
                .text_size(dp(13.))
                .child(
                    div()
                        .w(dp(28.))
                        .flex_shrink_0()
                        .text_left()
                        .font_family(DISPLAY_FONT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(dp(12.))
                        .text_color(if is_selected {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        })
                        .child(format!("M{}", index + 1)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_left()
                        .truncate()
                        .map(|this| {
                            if is_empty {
                                this.text_color(faint)
                            } else {
                                this.font_weight(FontWeight::MEDIUM)
                            }
                        })
                        .child(name),
                )
                .children(
                    count.map(|count| div().text_size(dp(12.)).text_color(faint).child(count)),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.select_macro(index, cx)))
        .into_any_element()
}

/// The selected slot's name, steps, and the actions on them.
fn render_editor(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    slot: &MacroSlot,
    caps: MacroCapabilities,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy() || app.is_loading();
    let index = slot.index;
    let count = slot.steps.len();
    let is_readonly = slot.is_readonly;
    let has_draft = app.macros_ui.has_draft();
    let header = h_flex()
        .gap_2()
        .min_h(dp(30.))
        .mb(dp(10.))
        .child(
            Button::new("rename-macro")
                .ghost()
                .small()
                .ml(dp(-8.))
                .tooltip("Rename this macro")
                .accessibility_label(format!("Rename {}", slot_name(slot)))
                .disabled(busy || is_readonly)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_size(dp(15.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(slot_name(slot)),
                        )
                        .child(
                            Icon::new(LucideIcon::Pencil)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_rename_macro(index, window, cx);
                })),
        )
        .child(div().flex_1())
        .child(
            Button::new("clear-macro")
                .ghost()
                .small()
                .text_color(cx.theme().danger)
                .label("Clear steps")
                .disabled(busy || is_readonly || count == 0)
                .on_click(cx.listener(|this, _, _, cx| this.clear_macro(cx))),
        );

    let ids = app.macros_ui.ids_for(slot);
    let rows: Vec<AnyElement> = slot
        .steps
        .iter()
        .zip(ids)
        .enumerate()
        .map(|(ix, (step, id))| render_step(ix, id, *step, count, busy, cx))
        .collect();
    let repeat = (caps.supports_repeat && !is_readonly).then(|| render_repeat(slot, busy, cx));
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let line = theme.border;
    let note = |text: SharedString| div().text_size(dp(12.5)).text_color(muted).child(text);
    // A keyboard binds the slot on its Keybinds tab; other devices keep their buttons.
    let hint = if device.capabilities.has_keymap() {
        format!("To use it, assign M{} to a key in Keybinds.", index + 1)
    } else {
        "Saving doesn’t reassign buttons.".to_owned()
    };
    let add = |id: &'static str, label: &'static str| {
        Button::new(id)
            .outline()
            .small()
            .icon(IconName::Plus)
            .label(label)
            .disabled(busy || is_readonly)
    };
    let footer = h_flex()
        .flex_wrap()
        .gap_2()
        .child(
            add("add-macro-step", "Key")
                .accessibility_label("Add a key step")
                .on_click(cx.listener(|this, _, _, cx| this.add_macro_step(cx))),
        )
        .when(caps.supports_mouse_buttons, |this| {
            this.child(
                add("add-macro-mouse-step", "Click")
                    .accessibility_label("Add a mouse click step")
                    .on_click(cx.listener(|this, _, _, cx| this.add_macro_mouse_step(cx))),
            )
        })
        .when(caps.supports_media_keys, |this| {
            this.child(
                add("add-macro-media-step", "Media")
                    .accessibility_label("Add a media key step")
                    .on_click(cx.listener(|this, _, _, cx| this.add_macro_media_step(cx))),
            )
        })
        .when(caps.supports_pointer_move, |this| {
            this.child(
                add("add-macro-move-step", "Move")
                    .accessibility_label("Add a pointer move step")
                    .on_click(cx.listener(|this, _, _, cx| this.add_macro_move_step(cx))),
            )
        })
        .child(div().flex_1())
        .child(
            Button::new("discard-macros")
                .ghost()
                .small()
                .label("Discard")
                .disabled(busy || !has_draft)
                .on_click(cx.listener(|this, _, _, cx| this.discard_macros(cx))),
        )
        .child(
            Button::new("save-macros")
                .primary()
                .small()
                .label("Save macros")
                .disabled(busy || !has_draft)
                .on_click(cx.listener(|this, _, _, cx| this.save_macros(cx))),
        );

    panel("macro-steps", cx)
        .child(header)
        .children(repeat)
        .when(is_readonly, |this| {
            this.child(note(
                "This macro uses events AgnoPeriph can’t edit yet. Saving keeps it unchanged on the device."
                    .into(),
            ))
        })
        .when(rows.is_empty() && !is_readonly, |this| {
            this.child(
                v_flex()
                    .items_center()
                    .justify_center()
                    .min_h(dp(150.))
                    .child(note("No steps yet. Add a step to start the sequence.".into())),
            )
        })
        .child(v_flex().gap(dp(2.)).children(rows))
        .child(div().h_px().mt(dp(10.)).mb(dp(12.)).bg(line))
        .child(footer)
        .child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .mt(dp(12.))
                .child(note(hint.into()))
                .child(div().flex_1())
                .when(has_draft, |this| {
                    this.child(note("Unsaved macro changes.".into()))
                }),
        )
        .into_any_element()
}

/// Replay count stepper; empty slots carry no trigger on the device, so it waits for a step.
fn render_repeat(slot: &MacroSlot, busy: bool, cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let id = u64::from(slot.index);
    let repeat = slot.repeat;
    let disabled = busy || slot.steps.is_empty();
    let decrease = Button::new(("macro-repeat-dec", id))
        .accessibility_label("Fewer cycles")
        .disabled(disabled || repeat <= MIN_REPEAT)
        .on_click(cx.listener(|this, _, _, cx| this.step_repeat(false, cx)));
    let increase = Button::new(("macro-repeat-inc", id))
        .accessibility_label("More cycles")
        .disabled(disabled || repeat == MAX_REPEAT)
        .on_click(cx.listener(|this, _, _, cx| this.step_repeat(true, cx)));
    let muted = cx.theme().muted_foreground;
    h_flex()
        .id(("macro-repeat", id))
        .gap_3()
        .mb(dp(8.))
        .child("Cycle times")
        .child(stepper_sized(
            decrease,
            repeat.to_string(),
            "",
            increase,
            28.,
            cx,
        ))
        .when(slot.steps.is_empty(), |this| {
            this.child(
                div()
                    .text_size(dp(12.5))
                    .text_color(muted)
                    .child("Add a step first."),
            )
        })
        .into_any_element()
}

fn mouse_button_chooser(
    id: impl Into<ElementId>,
    current: MouseButton,
    disabled: bool,
    on_pick: impl Fn(&mut AgnoPeriphApp, MouseButton, &mut Context<AgnoPeriphApp>) + 'static,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let app = cx.entity().downgrade();
    let on_pick = std::rc::Rc::new(on_pick);
    Button::new(id)
        .outline()
        .small()
        .label(current.label())
        .dropdown_caret(true)
        .disabled(disabled)
        .dropdown_menu(move |mut menu, _, _| {
            for button in MouseButton::ALL {
                let app = app.clone();
                let on_pick = on_pick.clone();
                menu = menu.item(
                    PopupMenuItem::new(button.label())
                        .checked(button == current)
                        .on_click(move |_, _, cx| {
                            let _ = app.update(cx, |this, cx| on_pick(this, button, cx));
                        }),
                );
            }
            menu
        })
        .into_any_element()
}

fn media_chooser(
    id: impl Into<ElementId>,
    current: u16,
    disabled: bool,
    on_pick: impl Fn(&mut AgnoPeriphApp, u16, &mut Context<AgnoPeriphApp>) + 'static,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let app = cx.entity().downgrade();
    let on_pick = std::rc::Rc::new(on_pick);
    Button::new(id)
        .outline()
        .small()
        .label(media_label(current))
        .dropdown_caret(true)
        .disabled(disabled)
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.scrollable(true).max_h(px(320.));
            for &(usage, name) in CONSUMER_MEDIA_USAGES {
                let app = app.clone();
                let on_pick = on_pick.clone();
                menu = menu.item(PopupMenuItem::new(name).checked(usage == current).on_click(
                    move |_, _, cx| {
                        let _ = app.update(cx, |this, cx| on_pick(this, usage, cx));
                    },
                ));
            }
            menu
        })
        .into_any_element()
}

fn render_move_axis(
    ix: usize,
    id: u64,
    axis_x: bool,
    value: i8,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let (label, dec_id, inc_id, dec_label, inc_label) = if axis_x {
        (
            "X",
            ("macro-move-x-dec", id),
            ("macro-move-x-inc", id),
            "Decrease X delta",
            "Increase X delta",
        )
    } else {
        (
            "Y",
            ("macro-move-y-dec", id),
            ("macro-move-y-inc", id),
            "Decrease Y delta",
            "Increase Y delta",
        )
    };
    let step_by = |sign: i8| {
        move |this: &mut AgnoPeriphApp,
              ev: &ClickEvent,
              _: &mut Window,
              cx: &mut Context<AgnoPeriphApp>| {
            let step = if ev.modifiers().shift { 10 } else { 1 };
            this.step_move_delta(ix, axis_x, sign * step, cx);
        }
    };
    let decrease = Button::new(dec_id)
        .accessibility_label(dec_label)
        .tooltip("Hold Shift for −10")
        .disabled(busy || value == i8::MIN)
        .on_click(cx.listener(step_by(-1)));
    let increase = Button::new(inc_id)
        .accessibility_label(inc_label)
        .tooltip("Hold Shift for +10")
        .disabled(busy || value == i8::MAX)
        .on_click(cx.listener(step_by(1)));
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap_1()
        .child(div().text_size(dp(12.)).text_color(muted).child(label))
        .child(stepper_sized(
            decrease,
            format!("{value:+}"),
            "",
            increase,
            24.,
            cx,
        ))
        .into_any_element()
}

/// The Press / Release / Tap choice of a step.
fn kind_choice(
    ix: usize,
    id: u64,
    current: MacroActionKind,
    kinds: [MacroActionKind; 3],
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let mut choices = segmented(cx);
    for kind in kinds {
        let button = Button::new(SharedString::from(format!(
            "macro-step-{id}-{}",
            kind_label(kind).to_lowercase()
        )));
        choices = choices.child(
            segment(button, kind_verb(kind), current == kind, cx)
                .h(dp(24.))
                .px(dp(7.))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.set_step_kind(ix, kind, cx))),
        );
    }
    choices.into_any_element()
}

fn render_step(
    ix: usize,
    id: u64,
    step: MacroStep,
    count: usize,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let action: Vec<AnyElement> = if step.kind.is_keyboard() {
        let kinds = [
            MacroActionKind::KeyDown,
            MacroActionKind::KeyUp,
            MacroActionKind::KeyTap,
        ];
        vec![
            kind_choice(ix, id, step.kind, kinds, busy, cx),
            key_chooser(
                ("macro-step-key", id),
                step.hid_keycode,
                busy,
                move |this, usage, cx| this.set_step_key(ix, usage, cx),
                cx,
            ),
        ]
    } else if step.kind.is_mouse_button() {
        let kinds = [
            MacroActionKind::MouseDown,
            MacroActionKind::MouseUp,
            MacroActionKind::MouseClick,
        ];
        vec![
            kind_choice(ix, id, step.kind, kinds, busy, cx),
            mouse_button_chooser(
                ("macro-step-mouse", id),
                step.mouse_button.unwrap_or(MouseButton::Left),
                busy,
                move |this, next, cx| this.set_step_mouse_button(ix, next, cx),
                cx,
            ),
        ]
    } else if step.kind.is_media() {
        let kinds = [
            MacroActionKind::MediaDown,
            MacroActionKind::MediaUp,
            MacroActionKind::MediaTap,
        ];
        vec![
            kind_choice(ix, id, step.kind, kinds, busy, cx),
            media_chooser(
                ("macro-step-media", id),
                step.consumer_usage,
                busy,
                move |this, next, cx| this.set_step_media(ix, next, cx),
                cx,
            ),
        ]
    } else {
        vec![
            div().child(kind_verb(step.kind)).into_any_element(),
            render_move_axis(ix, id, true, step.move_x, busy, cx),
            render_move_axis(ix, id, false, step.move_y, busy, cx),
        ]
    };

    let shorter = Button::new(("macro-delay-dec", id))
        .accessibility_label("Shorter delay")
        .disabled(busy || step.delay_ms == 0)
        .on_click(cx.listener(move |this, _, _, cx| this.step_delay(ix, false, cx)));
    let longer = Button::new(("macro-delay-inc", id))
        .accessibility_label("Longer delay")
        .disabled(busy || step.delay_ms >= MAX_DELAY_MS)
        .on_click(cx.listener(move |this, _, _, cx| this.step_delay(ix, true, cx)));
    let delay = stepper_sized(shorter, step.delay_ms.to_string(), "ms", longer, 38., cx);

    let order = |button: Button, icon: LucideIcon, name: &'static str| {
        button
            .ghost()
            .xsmall()
            .icon(Icon::new(icon))
            .tooltip(name)
            .accessibility_label(name)
    };
    let order = h_flex()
        .flex_shrink_0()
        .child(
            order(
                Button::new(("macro-step-up", id)),
                LucideIcon::ArrowUp,
                "Move step up",
            )
            .disabled(busy || ix == 0)
            .on_click(cx.listener(move |this, _, _, cx| this.move_macro_step(ix, true, cx))),
        )
        .child(
            order(
                Button::new(("macro-step-down", id)),
                LucideIcon::ArrowDown,
                "Move step down",
            )
            .disabled(busy || ix + 1 >= count)
            .on_click(cx.listener(move |this, _, _, cx| this.move_macro_step(ix, false, cx))),
        )
        .child(
            Button::new(("macro-step-remove", id))
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip("Remove step")
                .accessibility_label("Remove step")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.remove_macro_step(ix, cx))),
        );

    let theme = cx.theme();
    h_flex()
        .id(("macro-step", id))
        .gap(dp(8.))
        .min_h(dp(34.))
        .px(dp(4.))
        .rounded(theme.radius)
        .hover(|this| this.bg(theme.muted.opacity(0.5)))
        .child(
            div()
                .w(dp(18.))
                .flex_shrink_0()
                .text_right()
                .font_family(DISPLAY_FONT)
                .text_size(dp(11.5))
                .text_color(theme.muted_foreground)
                .child(format!("{}", ix + 1)),
        )
        .child(
            h_flex()
                .flex_shrink_0()
                .justify_center()
                .size(dp(22.))
                .rounded(theme.radius)
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .child(kind_icon(step.kind).xsmall()),
        )
        .children(action)
        .child(div().flex_1())
        .child(delay)
        .child(order)
        .into_any_element()
}
