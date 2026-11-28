//! The paired-keys inspector beside the board: a type select, the configured entries of that
//! type, and the form for adding one.

use agnoperiph_core::{
    DksBinding, DksTriggerDirection, KeyIndex, RapidSwitchPair, SocdMode, SocdPair,
};
use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    popover::Popover,
    radio::Radio,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{
    DKS_KEY_CLASS, DKS_KEY_ID_BASE, PairedEntry, PairedKeysUi, PairedMode, PickTarget,
    dks_stage_label, key_chooser, snap_travel, t,
};
use crate::app::{AgnoPeriphApp, mm};
use crate::backend::HallEffectState;
use crate::design::{dp, tokens};
use crate::icons::advanced_svg;
use crate::keycodes::hid_name;
use crate::keymap::KeyLayout;
use crate::widgets::{MarkedSwitch, block, mini_keycap, panel, panel_title, stepper};

const SOCD_CHOICES: [(SocdMode, &str); 4] = [
    (SocdMode::LastInputPriority, "last"),
    (SocdMode::Neutral, "neutral"),
    (SocdMode::KeyAPriority, "first"),
    (SocdMode::KeyBPriority, "second"),
];

fn socd_label(mode: SocdMode, first: &str, second: &str) -> SharedString {
    match mode {
        SocdMode::LastInputPriority => "Last pressed wins".into(),
        SocdMode::Neutral => "Neither key sends".into(),
        SocdMode::KeyAPriority => format!("{first} always wins").into(),
        SocdMode::KeyBPriority => format!("{second} always wins").into(),
    }
}

fn needs_two(mode: PairedMode) -> bool {
    matches!(mode, PairedMode::RapidSwitch | PairedMode::Socd)
}

fn count_of(he: &HallEffectState, mode: PairedMode) -> usize {
    let snapshot = &he.snapshot;
    match mode {
        PairedMode::RapidSwitch => snapshot.rapid_switch_pairs.len(),
        PairedMode::Socd => snapshot.socd_pairs.len(),
        PairedMode::Dks => snapshot.dks_bindings.len(),
        PairedMode::ModTap => snapshot.mod_tap_bindings.len(),
        PairedMode::Toggle => snapshot.toggle_bindings.len(),
    }
}

/// The panel. With a `height` it matches the board beside it and its body scrolls.
pub(super) fn render(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    occupied: &std::collections::BTreeMap<KeyIndex, &'static str>,
    height: Option<f32>,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let ui = &app.paired;
    let mode = ui.mode;
    let is_adding = ui.pick.is_some() || ui.first.is_some();
    let busy = app.is_busy();

    let title = match (is_adding, needs_two(mode)) {
        (false, _) => "Paired keys",
        (true, true) => "New pair",
        (true, false) => "New binding",
    };
    let muted = cx.theme().muted_foreground;
    let head = h_flex()
        .gap_2()
        .min_h(dp(26.))
        .mb(dp(10.))
        .child(panel_title(title))
        .child(div().flex_1())
        .when(is_adding, |this| {
            this.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(dp(12.))
                    .text_color(muted)
                    .child(mode.title()),
            )
        })
        .when(!is_adding, |this| {
            this.child(
                Button::new("paired-pick-first")
                    .outline()
                    .h(dp(26.))
                    .px(dp(10.))
                    .icon(IconName::Plus)
                    .label("Add")
                    .small()
                    .accessibility_label(format!("Add {}", mode.title()))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.start_pick(PickTarget::First, cx))),
            )
        });
    // While adding, the type is already chosen and the form needs the room.
    let select = (!is_adding).then(|| type_select(app, he, cx));

    let (body, footer) = if is_adding {
        let (body, footer) = add_form(app, layout, occupied, cx);
        (body, Some(footer))
    } else {
        (configured(app, he, layout, cx), None)
    };

    panel("paired-keys", cx)
        .when_some(height, |this, height| this.h(dp(height)))
        .child(head)
        .children(select)
        .child(
            div()
                .id("paired-body")
                .min_h_0()
                .when(height.is_some(), |this| this.flex_1().overflow_y_scroll())
                .child(block(body)),
        )
        .children(footer)
        .into_any_element()
}

/// The drop-down that picks which kind of paired or advanced key the panel shows.
fn type_select(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let mode = app.paired.mode;
    let caps = &he.capabilities;
    let entries: Vec<(PairedMode, usize)> = PairedMode::ALL
        .into_iter()
        .filter(|m| match m {
            PairedMode::Socd => caps.has_socd(),
            PairedMode::Dks => caps.has_dks(),
            _ => true,
        })
        .map(|m| (m, count_of(he, m)))
        .collect();
    let used = count_of(he, mode);
    let theme = cx.theme();
    let muted = theme.muted_foreground;

    let trigger = Button::new("paired-type")
        .outline()
        .w_full()
        .h(dp(32.))
        .px(dp(10.))
        .accessibility_label(format!("Paired key type: {}", mode.title()))
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .text_size(dp(13.))
                .child(advanced_svg(mode.icon(), dp(15.), theme.primary))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(mode.title()),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(dp(12.))
                        .text_color(muted)
                        .child(if used == 0 {
                            "None".to_owned()
                        } else {
                            used.to_string()
                        }),
                )
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(dp(14.))
                        .text_color(muted),
                ),
        );

    let entity = cx.entity().downgrade();
    let on_open = entity.clone();
    Popover::new("paired-type-menu")
        .anchor(Anchor::TopLeft)
        .p_1()
        .open(app.paired.is_menu_open)
        .on_open_change(move |open, _, cx| {
            let open = *open;
            let _ = on_open.update(cx, |this, cx| {
                this.paired.is_menu_open = open;
                cx.notify();
            });
        })
        .trigger(trigger)
        .content(move |_, _, cx| {
            let theme = cx.theme();
            let (muted, primary) = (theme.muted_foreground, theme.primary);
            v_flex()
                .id("paired-type-list")
                .w(dp(224.))
                .gap(dp(1.))
                .children(entries.iter().map(|&(entry, count)| {
                    let entity = entity.clone();
                    let is_selected = entry == mode;
                    Button::new(SharedString::from(format!("paired-mode-{}", entry.id())))
                        .ghost()
                        .selected(is_selected)
                        .w_full()
                        .h_auto()
                        .px(dp(8.))
                        .py(dp(5.))
                        .accessibility_label(entry.title())
                        .child(
                            h_flex()
                                .w_full()
                                .items_start()
                                .gap_2()
                                .child(advanced_svg(
                                    entry.icon(),
                                    dp(16.),
                                    if is_selected { primary } else { muted },
                                ))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .items_start()
                                        .child(
                                            div()
                                                .text_size(dp(13.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(entry.title()),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .whitespace_normal()
                                                .line_height(relative(1.3))
                                                .text_size(dp(11.5))
                                                .text_color(muted)
                                                .child(entry.summary()),
                                        ),
                                )
                                .when(count > 0, |this| {
                                    this.child(
                                        div()
                                            .text_size(dp(12.))
                                            .text_color(muted)
                                            .child(count.to_string()),
                                    )
                                }),
                        )
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| this.set_paired_mode(entry, cx));
                        })
                }))
        })
        .into_any_element()
}

// ---- Configured entries ---------------------------------------------------------------

fn configured(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let mode = app.paired.mode;
    let snapshot = &he.snapshot;
    let rows: Vec<AnyElement> = match mode {
        PairedMode::RapidSwitch => snapshot
            .rapid_switch_pairs
            .iter()
            .map(|&pair| rs_entry(app, layout, pair, cx))
            .collect(),
        PairedMode::Socd => snapshot
            .socd_pairs
            .iter()
            .map(|&pair| socd_entry(app, layout, pair, cx))
            .collect(),
        PairedMode::Dks => snapshot
            .dks_bindings
            .iter()
            .map(|binding| dks_entry(app, layout, binding, cx))
            .collect(),
        PairedMode::ModTap => snapshot
            .mod_tap_bindings
            .iter()
            .map(|binding| {
                let remove = remove_button(
                    app,
                    SharedString::from(format!("mt-remove-{}", binding.slot)),
                    PairedEntry::ModTap {
                        slot: binding.slot,
                        key: binding.key,
                    },
                    cx,
                );
                let head = entry_head(layout, binding.key, None, remove, cx);
                let detail = format!(
                    "Tap sends {}, hold sends {} after {} ms.",
                    hid_name(u16::from(binding.tap_keycode.1)),
                    hid_name(u16::from(binding.hold_keycode.1)),
                    binding.hold_delay_ms
                );
                v_flex()
                    .child(head)
                    .child(note(detail, cx))
                    .into_any_element()
            })
            .collect(),
        PairedMode::Toggle => snapshot
            .toggle_bindings
            .iter()
            .map(|binding| {
                let remove = remove_button(
                    app,
                    SharedString::from(format!("tgl-remove-{}", binding.slot)),
                    PairedEntry::Toggle {
                        slot: binding.slot,
                        key: binding.key,
                    },
                    cx,
                );
                let head = entry_head(layout, binding.key, None, remove, cx);
                let detail = format!(
                    "Latches {} when tapped within {} ms.",
                    hid_name(u16::from(binding.keycode.1)),
                    binding.delay_ms
                );
                v_flex()
                    .child(head)
                    .child(note(detail, cx))
                    .into_any_element()
            })
            .collect(),
    };
    let is_empty = rows.is_empty();
    let line = cx.theme().border;
    v_flex()
        .id("paired-configured")
        .pt(dp(2.))
        .when(is_empty, |this| {
            this.child(note(mode.summary(), cx)).child(note(
                "None set up yet. Choose Add, then pick the keys on the board.",
                cx,
            ))
        })
        .children(rows.into_iter().enumerate().map(|(ix, row)| {
            div()
                .when(ix > 0, |this| {
                    this.mt(dp(8.)).pt(dp(4.)).border_t_1().border_color(line)
                })
                .child(row)
        }))
        .test_support()
        .into_any_element()
}

fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .pt(dp(8.))
        .text_size(dp(12.5))
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn field_label(text: &'static str, cx: &App) -> Div {
    div()
        .mb(dp(4.))
        .text_size(dp(12.))
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// A label with its control at the right edge.
fn control_row(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    h_flex()
        .gap_2()
        .min_h(dp(30.))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(control)
}

/// The keys an entry owns, with its remove action. Hovering outlines them on the board.
fn entry_head(
    layout: &KeyLayout,
    first: KeyIndex,
    second: Option<KeyIndex>,
    remove: Button,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let keys = (first, second);
    let hover = tokens(cx).raised;
    let faint = tokens(cx).faint;
    h_flex()
        .id(SharedString::from(format!(
            "paired-entry-{}",
            first.value()
        )))
        .gap(dp(6.))
        .mx(dp(-6.))
        .px(dp(6.))
        .py(dp(4.))
        .rounded(cx.theme().radius)
        .hover(move |style| style.bg(hover))
        .on_hover(cx.listener(move |this, is_hovered: &bool, _, cx| {
            if *is_hovered {
                this.paired.hovered = Some(keys);
            } else if this.paired.hovered == Some(keys) {
                this.paired.hovered = None;
            }
            cx.notify();
        }))
        .child(mini_keycap(layout.label(first), cx))
        .when_some(second, |this, second| {
            this.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(faint)
                    .child("+"),
            )
            .child(mini_keycap(layout.label(second), cx))
        })
        .child(div().flex_1())
        .child(remove)
        .into_any_element()
}

fn remove_button(
    app: &AgnoPeriphApp,
    id: SharedString,
    entry: PairedEntry,
    cx: &mut Context<AgnoPeriphApp>,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .label("Remove")
        .text_color(cx.theme().danger)
        .disabled(app.is_busy())
        .on_click(cx.listener(move |this, _, _, cx| this.remove_paired(entry, cx)))
}

fn pair_element_id(prefix: &str, key_a: KeyIndex, key_b: KeyIndex, suffix: &str) -> SharedString {
    format!("{prefix}-{}-{}-{suffix}", key_a.value(), key_b.value()).into()
}

fn socd_entry(
    app: &AgnoPeriphApp,
    layout: &KeyLayout,
    pair: SocdPair,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy();
    let (a, b) = (pair.key_a, pair.key_b);
    let remove = remove_button(
        app,
        pair_element_id("socd-remove", a, b, "x"),
        PairedEntry::Socd(pair),
        cx,
    );
    let head = entry_head(layout, a, Some(b), remove, cx);
    let (first, second) = (layout.label(a), layout.label(b));
    let modes = v_flex()
        .id(pair_element_id("socd-modes", a, b, "group"))
        .gap(dp(5.))
        .children(SOCD_CHOICES.map(|(mode, suffix)| {
            Radio::new(pair_element_id("socd-mode", a, b, suffix))
                .label(socd_label(mode, &first, &second))
                .checked(pair.mode == mode)
                .disabled(busy)
                .on_change(cx.listener(move |this, _: &bool, _, cx| {
                    if pair.mode != mode {
                        this.set_socd_mode(pair, mode, cx);
                    }
                }))
        }));
    let travel = stepper(
        Button::new(pair_element_id("socd-travel-dec", a, b, "m"))
            .accessibility_label("Decrease pair actuation")
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| this.step_socd_travel(pair, -1, cx))),
        mm(pair.trigger_point),
        "mm",
        Button::new(pair_element_id("socd-travel-inc", a, b, "p"))
            .accessibility_label("Increase pair actuation")
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| this.step_socd_travel(pair, 1, cx))),
        cx,
    );
    let rapid = MarkedSwitch::new(pair_element_id("socd-rt", a, b, "sw"))
        .accessibility_label(format!("Rapid Trigger for {first} and {second}"))
        .checked(pair.rapid_trigger)
        .disabled(busy)
        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
            this.set_socd_rt(pair, *checked, cx);
        }));
    let line = cx.theme().border;
    v_flex()
        .child(head)
        .child(field_label("When both are held", cx))
        .child(modes)
        .child(div().mt(dp(6.)).h_px().bg(line))
        .child(control_row("Actuation", travel))
        .child(control_row("Rapid Trigger", rapid))
        .into_any_element()
}

fn rs_entry(
    app: &AgnoPeriphApp,
    layout: &KeyLayout,
    pair: RapidSwitchPair,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy();
    let (a, b) = (pair.key_a, pair.key_b);
    let remove = remove_button(
        app,
        pair_element_id("rs-remove", a, b, "x"),
        PairedEntry::RapidSwitch(pair),
        cx,
    );
    let head = entry_head(layout, a, Some(b), remove, cx);
    let travel = stepper(
        Button::new(pair_element_id("rs-travel-dec", a, b, "m"))
            .accessibility_label("Decrease pair actuation")
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| this.step_rs_travel(pair, -1, cx))),
        mm(pair.trigger_point),
        "mm",
        Button::new(pair_element_id("rs-travel-inc", a, b, "p"))
            .accessibility_label("Increase pair actuation")
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| this.step_rs_travel(pair, 1, cx))),
        cx,
    );
    let rapid = MarkedSwitch::new(pair_element_id("rs-rt", a, b, "sw"))
        .accessibility_label(format!(
            "Rapid Trigger for {} and {}",
            layout.label(a),
            layout.label(b)
        ))
        .checked(pair.rapid_trigger)
        .disabled(busy)
        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
            this.set_rs_rt(pair, *checked, cx);
        }));
    v_flex()
        .child(head)
        .child(control_row("Actuation", travel))
        .child(control_row("Rapid Trigger", rapid))
        .into_any_element()
}

fn dks_entry(
    app: &AgnoPeriphApp,
    layout: &KeyLayout,
    binding: &DksBinding,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let slot = binding.key;
    let bound_key = app
        .selected_device()
        .and_then(|d| d.keymap.as_ref())
        .and_then(|km| km.snapshot.layers.first())
        .and_then(|layer| {
            let code_id = DKS_KEY_ID_BASE + u8::try_from(slot.value()).ok()?;
            let at = layer
                .iter()
                .position(|c| c.class == DKS_KEY_CLASS && c.id == code_id)?;
            Some(KeyIndex::new(u16::try_from(at).ok()?))
        });
    let remove = remove_button(
        app,
        SharedString::from(format!("dks-remove-{}", slot.value())),
        PairedEntry::Dks(slot),
        cx,
    );
    let head = match bound_key {
        Some(key) => entry_head(layout, key, None, remove, cx),
        None => h_flex()
            .gap_2()
            .py(dp(6.))
            .child(div().flex_1().child(format!("Slot {}", slot.value() + 1)))
            .child(remove)
            .into_any_element(),
    };
    let theme = cx.theme();
    let (muted, line) = (theme.muted_foreground, theme.border);
    let stages = binding.stages.iter().flatten().map(|stage| {
        let direction = match stage.direction {
            DksTriggerDirection::Press => LucideIcon::ArrowDownToLine,
            DksTriggerDirection::Release => LucideIcon::ArrowUpFromLine,
        };
        let sends = hid_name(stage.hid_keycode);
        h_flex()
            .gap_2()
            .min_h(dp(24.))
            .text_size(dp(12.5))
            .child(Icon::new(direction).size(dp(13.)).text_color(muted))
            .child(
                div()
                    .flex_1()
                    .text_color(muted)
                    .child(format!("{} mm", mm(stage.trigger_travel))),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if stage.hold {
                        format!("Hold {sends}")
                    } else {
                        sends.clone()
                    }),
            )
    });
    v_flex()
        .child(head)
        .child(field_label("Actions along the key’s travel", cx))
        .child(
            v_flex()
                .border_t_1()
                .border_color(line)
                .pt(dp(4.))
                .children(stages),
        )
        .into_any_element()
}

// ---- Adding ---------------------------------------------------------------------------

/// The form for a new entry: the keys picked on the board, then the type's options.
/// Returns the scrolling body and the footer that stays in view.
fn add_form(
    app: &AgnoPeriphApp,
    layout: &KeyLayout,
    occupied: &std::collections::BTreeMap<KeyIndex, &'static str>,
    cx: &mut Context<AgnoPeriphApp>,
) -> (AnyElement, AnyElement) {
    let ui = app.paired.clone();
    let busy = app.is_busy();
    let mode = ui.mode;
    let two = needs_two(mode);
    // Picks are refused for taken keys, but a refresh can take a key after it was picked.
    let taken = [ui.first, ui.second.filter(|_| two)]
        .into_iter()
        .flatten()
        .find_map(|key| occupied.get(&key).map(|owner| (key, *owner)));
    let conflict: Option<SharedString> = ui.conflict.clone().or_else(|| {
        taken.map(|(key, owner)| {
            format!(
                "{} is already used by {}. Remove it there first.",
                layout.label(key),
                PairedMode::title_of_owner(owner)
            )
            .into()
        })
    });
    let ready = taken.is_none()
        && ui.first.is_some()
        && (!two || ui.second.is_some_and(|second| Some(second) != ui.first));

    let keys = h_flex()
        .gap(dp(6.))
        .py(dp(6.))
        .child(pick_slot(
            "paired-draft-first",
            if two { "Key 1" } else { "Key" },
            ui.first,
            ui.pick == Some(PickTarget::First),
            PickTarget::First,
            layout,
            busy,
            cx,
        ))
        .when(two, |this| {
            this.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens(cx).faint)
                    .child("+"),
            )
            .child(pick_slot(
                "paired-draft-second",
                "Key 2",
                ui.second,
                ui.pick == Some(PickTarget::Second),
                PickTarget::Second,
                layout,
                busy,
                cx,
            ))
        });

    let danger = cx.theme().danger;
    let mut body = v_flex()
        .id("paired-flow")
        .child(keys)
        .when_some(conflict, |this, conflict| {
            this.child(
                div()
                    .id("paired-conflict")
                    .pb(dp(6.))
                    .text_size(dp(12.5))
                    .text_color(danger)
                    .child(conflict)
                    .test_support(),
            )
        });
    body = match mode {
        PairedMode::Socd | PairedMode::RapidSwitch => {
            body.child(pair_options(&ui, layout, busy, cx))
        }
        PairedMode::Dks => body.child(dks_options(&ui, busy, cx)),
        PairedMode::ModTap | PairedMode::Toggle => body.child(hold_options(&ui, busy, cx)),
    };

    let footer = h_flex()
        .flex_shrink_0()
        .gap_2()
        .pt(dp(10.))
        .child(
            Button::new("paired-add")
                .primary()
                .small()
                .label(if two { "Add pair" } else { "Add binding" })
                .loading(busy)
                .disabled(!ready)
                .on_click(cx.listener(|this, _, _, cx| this.add_paired(cx))),
        )
        .child(
            Button::new("paired-cancel")
                .ghost()
                .small()
                .label("Cancel")
                .on_click(cx.listener(|this, _, _, cx| this.cancel_paired_flow(cx))),
        );
    (body.into_any_element(), footer.into_any_element())
}

/// A key of the entry being added. Clicking it picks that key again.
#[allow(clippy::too_many_arguments)]
fn pick_slot(
    id: &'static str,
    placeholder: &'static str,
    picked: Option<KeyIndex>,
    is_waiting: bool,
    target: PickTarget,
    layout: &KeyLayout,
    disabled: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let label: SharedString = match (is_waiting, picked) {
        (true, _) => "Click a key".into(),
        (false, Some(key)) => layout.label(key),
        (false, None) => placeholder.into(),
    };
    Button::new(id)
        .outline()
        .h(dp(30.))
        .min_w(dp(46.))
        .px(dp(10.))
        .selected(is_waiting)
        .label(label)
        .small()
        .when(is_waiting, |this| this.border_color(cx.theme().primary))
        .disabled(disabled)
        .on_click(cx.listener(move |this, _, _, cx| this.start_pick(target, cx)))
        .into_any_element()
}

fn pair_options(
    ui: &PairedKeysUi,
    layout: &KeyLayout,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let name = |key: Option<KeyIndex>, fallback: &'static str| {
        key.map_or_else(|| fallback.into(), |key| layout.label(key))
    };
    let (first, second) = (name(ui.first, "Key 1"), name(ui.second, "Key 2"));
    let modes = (ui.mode == PairedMode::Socd).then(|| {
        v_flex()
            .child(field_label("When both are held", cx))
            .child(
                v_flex()
                    .id("paired-socd-modes")
                    .gap(dp(7.))
                    .children(SOCD_CHOICES.map(|(mode, suffix)| {
                        Radio::new(SharedString::from(format!("paired-socd-{suffix}")))
                            .label(socd_label(mode, &first, &second))
                            .checked(ui.socd_mode == mode)
                            .disabled(busy)
                            .on_change(cx.listener(move |this, _: &bool, _, cx| {
                                this.paired.socd_mode = mode;
                                cx.notify();
                            }))
                    })),
            )
            .child(div().mt(dp(8.)).mb(dp(2.)).h_px().bg(cx.theme().border))
    });
    let travel = stepper(
        Button::new("paired-travel-dec")
            .accessibility_label("Decrease pair actuation")
            .disabled(busy)
            .on_click(cx.listener(|this, _, _, cx| this.step_pair_trigger_point(-1, cx))),
        mm(ui.pair_trigger_point),
        "mm",
        Button::new("paired-travel-inc")
            .accessibility_label("Increase pair actuation")
            .disabled(busy)
            .on_click(cx.listener(|this, _, _, cx| this.step_pair_trigger_point(1, cx))),
        cx,
    );
    let rapid = MarkedSwitch::new("paired-rt")
        .accessibility_label("Rapid Trigger for the new pair")
        .checked(ui.pair_rapid_trigger)
        .disabled(busy)
        .on_change(cx.listener(|this, checked: &bool, _, cx| {
            this.paired.pair_rapid_trigger = *checked;
            cx.notify();
        }));
    v_flex()
        .children(modes)
        .child(control_row("Actuation", travel))
        .child(control_row("Rapid Trigger", rapid))
        .into_any_element()
}

fn dks_options(ui: &PairedKeysUi, busy: bool, cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let sends = key_chooser(
        "paired-key-primary",
        ui.primary_usage,
        busy,
        |this, code, cx| this.set_paired_usage(true, code, cx),
        cx,
    );
    let mut body = v_flex().child(control_row("Sends", sends));
    for (ix, stage) in ui.dks_stages.into_iter().enumerate() {
        let step =
            move |this: &mut AgnoPeriphApp, direction: i32, cx: &mut Context<AgnoPeriphApp>| {
                let next = snap_travel(
                    i32::from(this.paired.dks_stages[ix].travel.as_hundredths()) + direction * 10,
                    t(10),
                    t(340),
                    t(10),
                );
                this.paired.dks_stages[ix].travel = next;
                cx.notify();
            };
        let locked = busy || !stage.enabled;
        let travel = stepper(
            Button::new(SharedString::from(format!("dks-stage-{ix}-dec")))
                .accessibility_label(format!("Decrease {} depth", dks_stage_label(ix)))
                .disabled(locked)
                .on_click(cx.listener(move |this, _, _, cx| step(this, -1, cx))),
            mm(stage.travel),
            "mm",
            Button::new(SharedString::from(format!("dks-stage-{ix}-inc")))
                .accessibility_label(format!("Increase {} depth", dks_stage_label(ix)))
                .disabled(locked)
                .on_click(cx.listener(move |this, _, _, cx| step(this, 1, cx))),
            cx,
        );
        body = body.child(
            v_flex()
                .gap(dp(4.))
                .py(dp(6.))
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    Checkbox::new(("dks-stage-enabled", ix))
                        .label(dks_stage_label(ix))
                        .checked(stage.enabled)
                        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                            this.paired.dks_stages[ix].enabled = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    h_flex().justify_between().child(travel).child(
                        Checkbox::new(("dks-stage-hold", ix))
                            .label("Hold")
                            .checked(stage.hold)
                            .disabled(!stage.enabled)
                            .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                this.paired.dks_stages[ix].hold = *checked;
                                cx.notify();
                            })),
                    ),
                ),
        );
    }
    body.child(note("Rebinds the key to the DKS slot on Win Base.", cx))
        .into_any_element()
}

fn hold_options(ui: &PairedKeysUi, busy: bool, cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let is_mod_tap = ui.mode == PairedMode::ModTap;
    let primary = key_chooser(
        "paired-key-primary",
        ui.primary_usage,
        busy,
        |this, code, cx| this.set_paired_usage(true, code, cx),
        cx,
    );
    let secondary = is_mod_tap.then(|| {
        key_chooser(
            "paired-key-secondary",
            ui.secondary_usage,
            busy,
            |this, code, cx| this.set_paired_usage(false, code, cx),
            cx,
        )
    });
    let delay = ui.delay_ms;
    let delay_stepper = stepper(
        Button::new("paired-delay-dec")
            .accessibility_label("Shorter delay")
            .disabled(busy || delay <= 50)
            .on_click(cx.listener(|this, _, _, cx| {
                this.paired.delay_ms = this.paired.delay_ms.saturating_sub(10).max(50);
                cx.notify();
            })),
        delay.to_string(),
        "ms",
        Button::new("paired-delay-inc")
            .accessibility_label("Longer delay")
            .disabled(busy || delay >= 1000)
            .on_click(cx.listener(|this, _, _, cx| {
                this.paired.delay_ms = (this.paired.delay_ms + 10).min(1000);
                cx.notify();
            })),
        cx,
    );
    v_flex()
        .child(control_row(
            if is_mod_tap { "Tap sends" } else { "Latches" },
            primary,
        ))
        .when_some(secondary, |this, secondary| {
            this.child(control_row("Hold sends", secondary))
        })
        .child(control_row(
            if is_mod_tap {
                "Hold after"
            } else {
                "Tap within"
            },
            delay_stepper,
        ))
        .into_any_element()
}
