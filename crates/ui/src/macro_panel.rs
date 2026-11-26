//! Onboard macro slots: rename, step list editing (Down / Up / Tap, key, delay),
//! reordering, clearing, cycle times where the device stores them, and an explicit
//! Save that writes every slot at once.

use std::collections::BTreeMap;

use agnoperiph_core::{MacroActionKind, MacroCapabilities, MacroSlot, MacroStep, MouseButton};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    tag::Tag,
    v_flex,
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

/// Display name of a macro slot, falling back to `Macro {n}`.
pub(crate) fn slot_name(slot: &MacroSlot) -> String {
    if slot.name.trim().is_empty() {
        format!("Macro {}", slot.index + 1)
    } else {
        slot.name.clone()
    }
}

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
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let Some(state) = device.macros.as_ref() else {
        return section(
            "macro-panel",
            "Macros",
            "Record key sequences into the device’s onboard macro slots.",
            cx.theme(),
        )
        .child(muted_note("Macros couldn’t be read.", cx.theme()));
    };
    let caps = state.capabilities;
    let has_advanced =
        caps.supports_mouse_buttons || caps.supports_media_keys || caps.supports_pointer_move;
    let description = if device.capabilities.has_keymap() {
        format!(
            "Record key sequences into the device’s {} macro slots, then bind them on the Keybinds tab.",
            caps.max_slots
        )
    } else if has_advanced {
        format!(
            "Record key, mouse-button, media, and pointer-move sequences into the device’s {} macro slots. Saving doesn’t reassign buttons.",
            caps.max_slots
        )
    } else {
        format!(
            "Record key sequences into the device’s {} macro slots. Saving doesn’t reassign buttons.",
            caps.max_slots
        )
    };
    let base = section("macro-panel", "Macros", description, cx.theme());
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
            let summary = if slot.is_readonly {
                "Read-only".to_owned()
            } else if steps == 0 {
                "Empty".to_owned()
            } else {
                format!("{steps} steps")
            };
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
                                .child(summary),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.select_macro(index, cx)))
        }));

    let editor = match current {
        Some(slot) => render_editor(app, &slot, caps, busy, cx).into_any_element(),
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
            "{} slots, {} bytes of macro memory on the device.",
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
    caps: MacroCapabilities,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let index = slot.index;
    let count = slot.steps.len();
    let is_readonly = slot.is_readonly;
    let has_advanced =
        caps.supports_mouse_buttons || caps.supports_media_keys || caps.supports_pointer_move;
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
                .disabled(busy || is_readonly)
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
    group("macro-steps", "Steps", theme)
        .child(header)
        .children(repeat)
        .when(is_readonly, |this| {
            this.child(muted_note(
                "This macro uses events AgnoPeriph can’t edit yet. Saving keeps it unchanged on the device.",
                theme,
            ))
        })
        .when(rows.is_empty() && !is_readonly, |this| {
            this.child(muted_note(
                "No steps yet. Add a step to start the sequence.",
                theme,
            ))
        })
        .child(v_flex().gap_1().children(rows))
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("add-macro-step")
                        .outline()
                        .small()
                        .icon(IconName::Plus)
                        .label(if has_advanced { "Add key" } else { "Add step" })
                        .disabled(busy || is_readonly)
                        .on_click(cx.listener(|this, _, _, cx| this.add_macro_step(cx))),
                )
                .when(caps.supports_mouse_buttons, |this| {
                    this.child(
                        Button::new("add-macro-mouse-step")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add click")
                            .disabled(busy || is_readonly)
                            .on_click(cx.listener(|this, _, _, cx| this.add_macro_mouse_step(cx))),
                    )
                })
                .when(caps.supports_media_keys, |this| {
                    this.child(
                        Button::new("add-macro-media-step")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add media")
                            .disabled(busy || is_readonly)
                            .on_click(cx.listener(|this, _, _, cx| this.add_macro_media_step(cx))),
                    )
                })
                .when(caps.supports_pointer_move, |this| {
                    this.child(
                        Button::new("add-macro-move-step")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add move")
                            .disabled(busy || is_readonly)
                            .on_click(cx.listener(|this, _, _, cx| this.add_macro_move_step(cx))),
                    )
                }),
        )
}

/// Replay count stepper; empty slots carry no trigger on the device, so it waits for a step.
fn render_repeat(
    slot: &MacroSlot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let id = u64::from(slot.index);
    let repeat = slot.repeat;
    let disabled = busy || slot.steps.is_empty();
    let theme = cx.theme();
    h_flex()
        .id(("macro-repeat", id))
        .gap_3()
        .child(div().text_sm().child("Cycle times"))
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(("macro-repeat-dec", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Minus)
                        .accessibility_label("Fewer cycles")
                        .disabled(disabled || repeat <= MIN_REPEAT)
                        .on_click(cx.listener(|this, _, _, cx| this.step_repeat(false, cx))),
                )
                .child(
                    div()
                        .w_10()
                        .text_sm()
                        .text_center()
                        .font_family(theme.mono_font_family.clone())
                        .when(disabled, |this| this.text_color(theme.muted_foreground))
                        .child(repeat.to_string()),
                )
                .child(
                    Button::new(("macro-repeat-inc", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .accessibility_label("More cycles")
                        .disabled(disabled || repeat == MAX_REPEAT)
                        .on_click(cx.listener(|this, _, _, cx| this.step_repeat(true, cx))),
                ),
        )
        .when(slot.steps.is_empty(), |this| {
            this.child(muted_note("Add a step first.", theme))
        })
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
    let theme = cx.theme();
    h_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            Button::new(dec_id)
                .ghost()
                .xsmall()
                .icon(IconName::Minus)
                .accessibility_label(dec_label)
                .tooltip("Hold Shift for −10")
                .disabled(busy || value == i8::MIN)
                .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                    let step = if ev.modifiers().shift { -10 } else { -1 };
                    this.step_move_delta(ix, axis_x, step, cx);
                })),
        )
        .child(
            div()
                .w_10()
                .text_sm()
                .text_center()
                .font_family(theme.mono_font_family.clone())
                .child(format!("{value:+}")),
        )
        .child(
            Button::new(inc_id)
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .accessibility_label(inc_label)
                .tooltip("Hold Shift for +10")
                .disabled(busy || value == i8::MAX)
                .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                    let step = if ev.modifiers().shift { 10 } else { 1 };
                    this.step_move_delta(ix, axis_x, step, cx);
                })),
        )
        .into_any_element()
}

fn render_step(
    ix: usize,
    id: u64,
    step: MacroStep,
    count: usize,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let action_controls: AnyElement = if step.kind.is_keyboard() {
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
        h_flex().gap_3().child(kinds).child(key).into_any_element()
    } else if step.kind.is_mouse_button() {
        let kinds = ButtonGroup::new(("macro-step-kind", id))
            .outline()
            .xsmall()
            .children(
                [
                    MacroActionKind::MouseDown,
                    MacroActionKind::MouseUp,
                    MacroActionKind::MouseClick,
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
        let button = step.mouse_button.unwrap_or(MouseButton::Left);
        let chooser = mouse_button_chooser(
            ("macro-step-mouse", id),
            button,
            busy,
            move |this, next, cx| this.set_step_mouse_button(ix, next, cx),
            cx,
        );
        h_flex()
            .gap_3()
            .child(kinds)
            .child(chooser)
            .into_any_element()
    } else if step.kind.is_media() {
        let kinds = ButtonGroup::new(("macro-step-kind", id))
            .outline()
            .xsmall()
            .children(
                [
                    MacroActionKind::MediaDown,
                    MacroActionKind::MediaUp,
                    MacroActionKind::MediaTap,
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
        let chooser = media_chooser(
            ("macro-step-media", id),
            step.consumer_usage,
            busy,
            move |this, next, cx| this.set_step_media(ix, next, cx),
            cx,
        );
        h_flex()
            .gap_3()
            .child(kinds)
            .child(chooser)
            .into_any_element()
    } else {
        h_flex()
            .gap_3()
            .child(Tag::secondary().small().child("Move XY"))
            .child(render_move_axis(ix, id, true, step.move_x, busy, cx))
            .child(render_move_axis(ix, id, false, step.move_y, busy, cx))
            .into_any_element()
    };
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
        .child(action_controls)
        .child(delay)
        .child(div().flex_1())
        .child(order)
        .into_any_element()
}
