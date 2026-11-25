//! Onboard macros M1..M16: rename, step list editing (Down / Up / Tap, key, delay),
//! reordering, clearing, and an explicit Save that writes every slot at once.

use std::collections::BTreeMap;

use agnoperiph_core::{MacroActionKind, MacroSlot, MacroStep};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, group, muted_note, section};
use crate::backend::{DeviceSnapshot, SettingChange};
use crate::hall_effect_panel::key_chooser;

/// Delay change per stepper click, in milliseconds.
const DELAY_STEP_MS: u16 = 10;
const MAX_DELAY_MS: u16 = 9_990;
const DEFAULT_DELAY_MS: u16 = 20;
/// Steps a new macro starts with use the Space key.
const DEFAULT_STEP_USAGE: u8 = 0x2C;

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

/// Display name of a macro slot, falling back to `M{n}`.
pub(crate) fn slot_name(slot: &MacroSlot) -> String {
    if slot.name.trim().is_empty() {
        format!("M{}", slot.index + 1)
    } else {
        slot.name.clone()
    }
}

fn kind_label(kind: MacroActionKind) -> &'static str {
    match kind {
        MacroActionKind::KeyDown => "Down",
        MacroActionKind::KeyUp => "Up",
        MacroActionKind::KeyTap => "Tap",
    }
}

impl AgnoPeriphApp {
    /// Current slots, preferring the unsaved draft.
    pub(crate) fn macro_slots(&self) -> Vec<MacroSlot> {
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
        if let Some(slot) = slots.iter_mut().find(|s| s.index == selected) {
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
                .last()
                .map_or(DEFAULT_STEP_USAGE, |s| s.hid_keycode);
            slot.steps.push(MacroStep::new(
                MacroActionKind::KeyTap,
                usage,
                DEFAULT_DELAY_MS,
            ));
        });
    }

    fn remove_macro_step(&mut self, step: usize, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, _| {
            if step < slot.steps.len() {
                slot.steps.remove(step);
                ids.remove(step);
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

    fn clear_macro(&mut self, cx: &mut Context<Self>) {
        self.edit_macro(cx, |slot, ids, _| {
            slot.steps.clear();
            ids.clear();
        });
    }

    /// Renames the selected macro in the draft; empty restores `M{n}`.
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
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let base = section(
        "macro-panel",
        "Macros",
        "Record key sequences into the keyboard’s 16 macro slots, then bind them on the Keybinds tab.",
        cx.theme(),
    );
    let Some(state) = device.macros.as_ref() else {
        return base.child(muted_note("Macros couldn’t be read.", cx.theme()));
    };
    let busy = app.is_busy() || app.is_loading();
    let slots = app.macro_slots();
    let selected = app.macros_ui.selected;
    let has_draft = app.macros_ui.has_draft();
    let current = slots.iter().find(|s| s.index == selected).cloned();

    let list = v_flex()
        .id("macro-slots")
        .w_48()
        .flex_none()
        .gap_0p5()
        .children(slots.iter().map(|slot| {
            let index = slot.index;
            let steps = slot.steps.len();
            Button::new(("macro-slot", u64::from(index)))
                .ghost()
                .small()
                .w_full()
                .justify_start()
                .selected(index == selected)
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .gap_2()
                        .child(div().truncate().child(slot_name(slot)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if steps == 0 {
                                    "Empty".to_owned()
                                } else {
                                    format!("{steps} steps")
                                }),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.select_macro(index, cx)))
        }));

    let editor = match current {
        Some(slot) => render_editor(app, &slot, busy, cx).into_any_element(),
        None => muted_note("Select a macro slot.", cx.theme()).into_any_element(),
    };

    let footer = h_flex()
        .gap_2()
        .justify_end()
        .when(has_draft, |this| {
            this.child(muted_note("Unsaved macro changes.", cx.theme()))
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

    let limit = muted_note(
        format!(
            "{} slots, {} bytes of macro memory on the keyboard.",
            state.capabilities.max_slots, state.capabilities.max_buffer_bytes
        ),
        cx.theme(),
    );
    base.child(
        h_flex()
            .items_start()
            .gap_4()
            .child(list)
            .child(v_flex().flex_1().min_w_0().gap_3().child(editor)),
    )
    .child(limit)
    .child(footer)
}

fn render_editor(
    app: &AgnoPeriphApp,
    slot: &MacroSlot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let index = slot.index;
    let count = slot.steps.len();
    let header = h_flex()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(slot_name(slot)),
        )
        .child(div().flex_1())
        .child(
            Button::new("rename-macro")
                .ghost()
                .small()
                .label("Rename…")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_rename_macro(index, window, cx);
                })),
        )
        .child(
            Button::new("clear-macro")
                .ghost()
                .small()
                .icon(IconName::Delete)
                .label("Clear steps")
                .disabled(busy || count == 0)
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
    let theme = cx.theme();
    group("macro-steps", "Steps", theme)
        .child(header)
        .when(rows.is_empty(), |this| {
            this.child(muted_note(
                "No steps yet. Add a step to start the sequence.",
                theme,
            ))
        })
        .child(v_flex().gap_1().children(rows))
        .child(
            h_flex().child(
                Button::new("add-macro-step")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label("Add step")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.add_macro_step(cx))),
            ),
        )
}

fn render_step(
    ix: usize,
    id: u64,
    step: MacroStep,
    count: usize,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let kinds = ButtonGroup::new(("macro-step-kind", id))
        .outline()
        .xsmall()
        .children(
            [
                MacroActionKind::KeyDown,
                MacroActionKind::KeyUp,
                MacroActionKind::KeyTap,
            ]
            .map(|kind| {
                Button::new(SharedString::from(format!(
                    "macro-step-{id}-{}",
                    kind_label(kind).to_lowercase()
                )))
                .label(kind_label(kind))
                .selected(step.kind == kind)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.set_step_kind(ix, kind, cx)))
            }),
        );
    let key = key_chooser(
        ("macro-step-key", id),
        step.hid_keycode,
        busy,
        move |this, usage, cx| this.set_step_key(ix, usage, cx),
        cx,
    );
    let theme = cx.theme();
    let ordinal = div()
        .w_6()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(format!("{}", ix + 1));
    let delay = h_flex()
        .gap_1()
        .child(
            Button::new(("macro-delay-dec", id))
                .ghost()
                .xsmall()
                .icon(IconName::Minus)
                .accessibility_label("Shorter delay")
                .disabled(busy || step.delay_ms == 0)
                .on_click(cx.listener(move |this, _, _, cx| this.step_delay(ix, false, cx))),
        )
        .child(
            div()
                .w_16()
                .text_sm()
                .text_center()
                .font_family(cx.theme().mono_font_family.clone())
                .child(format!("{} ms", step.delay_ms)),
        )
        .child(
            Button::new(("macro-delay-inc", id))
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .accessibility_label("Longer delay")
                .disabled(busy || step.delay_ms >= MAX_DELAY_MS)
                .on_click(cx.listener(move |this, _, _, cx| this.step_delay(ix, true, cx))),
        );
    let order = h_flex()
        .gap_0p5()
        .child(
            Button::new(("macro-step-up", id))
                .ghost()
                .xsmall()
                .label("Up")
                .accessibility_label("Move step up")
                .disabled(busy || ix == 0)
                .on_click(cx.listener(move |this, _, _, cx| this.move_macro_step(ix, true, cx))),
        )
        .child(
            Button::new(("macro-step-down", id))
                .ghost()
                .xsmall()
                .label("Down")
                .accessibility_label("Move step down")
                .disabled(busy || ix + 1 >= count)
                .on_click(cx.listener(move |this, _, _, cx| this.move_macro_step(ix, false, cx))),
        )
        .child(
            Button::new(("macro-step-remove", id))
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .accessibility_label("Remove step")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.remove_macro_step(ix, cx))),
        );
    h_flex()
        .id(("macro-step", id))
        .gap_3()
        .py_1()
        .child(ordinal)
        .child(kinds)
        .child(key)
        .child(delay)
        .child(div().flex_1())
        .child(order)
        .into_any_element()
}
