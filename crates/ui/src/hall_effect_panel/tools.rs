//! The Actuation and Rapid Trigger panels under the board.

use agnoperiph_core::{RapidTriggerConfig, TravelHundredthsMm};
use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::Button,
    h_flex,
    slider::{Slider, SliderState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::t;
use crate::app::{AgnoPeriphApp, mm};
use crate::backend::HallEffectState;
use crate::design::{DISPLAY_FONT, KEY_FONT, dp, tabular, tokens};
use crate::widgets::{MarkedSwitch, block, idle, panel, panel_title, stepper, stepper_sized};

/// The selected keys' values of one setting: the primary key's, and whether all agree.
fn shared<T: PartialEq + Copy>(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    primary: T,
    value: impl Fn(&agnoperiph_core::KeyHallEffectConfig) -> T,
) -> Option<T> {
    app.selected_keys()
        .iter()
        .filter_map(|&key| he.key(key))
        .all(|config| value(config) == primary)
        .then_some(primary)
}

fn head(title: &str) -> Div {
    h_flex()
        .gap_2()
        .min_h(dp(26.))
        .mb(dp(8.))
        .child(panel_title(title))
        .child(div().flex_1())
}

fn select_all(id: &'static str, cx: &mut Context<AgnoPeriphApp>) -> Button {
    Button::new(id)
        .outline()
        .small()
        .label("Select all keys")
        .on_click(cx.listener(|this, _, _, cx| this.select_all_keys(cx)))
}

pub(super) fn actuation_panel(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let has_selection = !app.selected_keys().is_empty();
    let has_deadzones = he
        .snapshot
        .keys
        .iter()
        .any(|k| k.top_deadzone.is_some() || k.bottom_deadzone.is_some());
    let busy = app.is_busy();

    let use_for_all = has_selection.then(|| {
        Button::new("actuation-all-keys")
            .outline()
            .small()
            .label("Use for all keys")
            .tooltip("Stage this actuation point for every key")
            .disabled(busy)
            .on_click(cx.listener(|this, _, _, cx| this.stage_actuation_for_all_keys(cx)))
    });
    let body = if has_selection {
        actuation_controls(app, he, cx)
    } else {
        let button = select_all("actuation-select-all", cx);
        idle(
            LucideIcon::MousePointer2,
            "Select keys on the board to set how far they travel before they register.",
            cx,
        )
        .min_h(dp(if has_deadzones { 120. } else { 178. }))
        .child(button)
        .into_any_element()
    };
    let line = cx.theme().border;
    // Dead zones apply to every key, so they stay reachable without a selection.
    let footer = if has_deadzones {
        Some(deadzones(app, cx))
    } else {
        has_selection.then(|| {
            let caps = &he.capabilities;
            div()
                .text_size(dp(12.5))
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "Moves in {} mm steps, from {} to {} mm.",
                    mm(caps.actuation_step),
                    mm(caps.actuation_min),
                    mm(caps.actuation_max)
                ))
                .into_any_element()
        })
    };

    panel("trigger-travel", cx)
        .size_full()
        .child(head("Actuation").children(use_for_all))
        .child(block(body))
        .child(div().flex_1())
        .when_some(footer, |this, footer| {
            this.child(div().mt(dp(10.)).mb(dp(8.)).h_px().bg(line))
                .child(footer)
        })
        .into_any_element()
}

fn actuation_controls(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy();
    let caps = &he.capabilities;
    let primary = app.key_values().map_or(caps.actuation_min, |(a, _)| a);
    let agreed = shared(app, he, primary, |k| k.actuation);
    let slider = app.controls.as_ref().map(|c| c.travel.clone());

    let step = |id: &'static str, icon: IconName, label: &'static str| {
        Button::new(id)
            .icon(icon)
            .small()
            .size(dp(30.))
            .accessibility_label(label)
            .disabled(busy)
    };
    let decrease = step("actuation-dec", IconName::Minus, "Decrease actuation")
        .on_click(cx.listener(|this, _, _, cx| this.step_actuation(-1, cx)));
    let increase = step("actuation-inc", IconName::Plus, "Increase actuation")
        .on_click(cx.listener(|this, _, _, cx| this.step_actuation(1, cx)));

    let muted = cx.theme().muted_foreground;
    let value = h_flex()
        .min_w(dp(122.))
        .justify_center()
        .items_baseline()
        .gap(dp(4.))
        .child(
            div()
                .id("actuation-value")
                .font_family(DISPLAY_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .font_features(tabular())
                .text_size(dp(34.))
                .line_height(relative(1.))
                .child(agreed.map_or_else(|| "Mixed".to_owned(), mm)),
        )
        .when(agreed.is_some(), |this| {
            this.child(
                div()
                    .font_family(DISPLAY_FONT)
                    .text_size(dp(14.))
                    .text_color(muted)
                    .child("mm"),
            )
        });

    v_flex()
        .child(
            h_flex()
                .justify_center()
                .gap(dp(12.))
                .mb(dp(10.))
                .child(decrease)
                .child(value)
                .child(increase),
        )
        .when_some(slider, |this, slider| {
            this.child(
                Slider::new(&slider)
                    .disabled(busy)
                    .accessibility_label("Actuation point"),
            )
        })
        .child(ruler(caps.actuation_min, caps.actuation_max, cx))
        .into_any_element()
}

/// Millimetre ticks under the actuation slider.
fn ruler(min: TravelHundredthsMm, max: TravelHundredthsMm, cx: &App) -> Div {
    let (low, high) = (
        min.as_hundredths(),
        max.as_hundredths().max(min.as_hundredths() + 1),
    );
    let at = |value: u16| f32::from(value.saturating_sub(low)) / f32::from(high - low);
    let tokens = tokens(cx);
    let ticks = (low.div_ceil(10)..=high / 10).map(|tenth| {
        let is_major = tenth % 5 == 0;
        div()
            .absolute()
            .top_0()
            .left(relative(at(tenth * 10)))
            .w_px()
            .h(dp(if is_major { 7. } else { 4. }))
            .bg(tokens.tick)
    });
    let mut marks = vec![low];
    marks.extend(
        (1..=high / 100)
            .map(|mm| mm * 100)
            .filter(|&v| v > low && v < high),
    );
    marks.push(high);
    let labels = marks.into_iter().map(|value| {
        div()
            .absolute()
            .top(dp(9.))
            .left(relative(at(value)))
            .ml(dp(-16.))
            .w(dp(32.))
            .text_center()
            .font_family(KEY_FONT)
            .text_size(dp(10.))
            .line_height(relative(1.))
            .text_color(tokens.faint)
            .child(mm(TravelHundredthsMm::from_hundredths(value)))
    });
    div().mx(dp(8.)).mt(dp(3.)).h(dp(20.)).child(
        div()
            .relative()
            .size_full()
            .children(ticks)
            .children(labels),
    )
}

/// Keyboard-wide dead zones, available with or without a selection.
fn deadzones(app: &AgnoPeriphApp, cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let busy = app.is_busy();
    let (top, bottom) = app.deadzone_values().unwrap_or((t(0), t(0)));
    let zone = |name: &'static str, id: &'static str, value, is_top: bool| {
        let button = |suffix: &str, label: String, direction: i32| {
            Button::new(SharedString::from(format!("{id}-{suffix}")))
                .accessibility_label(label)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.step_deadzone(is_top, direction, cx);
                }))
        };
        let decrease = button("dec", format!("Decrease {name} dead zone"), -1);
        let increase = button("inc", format!("Increase {name} dead zone"), 1);
        h_flex()
            .gap(dp(6.))
            .child(div().text_size(dp(12.5)).child(name))
            .child(stepper_sized(decrease, mm(value), "", increase, 30., cx))
    };
    let top = zone("Top", "deadzone-top", top, true);
    let bottom = zone("Bottom", "deadzone-bottom", bottom, false);
    v_flex()
        .gap(dp(6.))
        .child(
            div()
                .text_size(dp(12.))
                .text_color(cx.theme().muted_foreground)
                .child("Dead zones for all keys (mm)"),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_x(dp(8.))
                .gap_y(dp(6.))
                .child(top)
                .child(bottom),
        )
        .into_any_element()
}

pub(super) fn rapid_trigger_panel(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let count = app.selected_keys().len();
    let busy = app.is_busy();
    let primary = app
        .key_values()
        .map_or(RapidTriggerConfig::disabled(), |(_, rt)| rt);
    let on_count = app
        .selected_keys()
        .iter()
        .filter_map(|&key| he.key(key))
        .filter(|config| config.rapid_trigger.is_enabled())
        .count();
    let is_on = count > 0 && on_count == count;

    let partial = (on_count > 0 && !is_on).then(|| {
        div()
            .text_size(dp(12.))
            .text_color(cx.theme().muted_foreground)
            .child(format!("On for {on_count} of {count}"))
    });
    let switch = (count > 0).then(|| {
        MarkedSwitch::new("rapid-trigger")
            .accessibility_label("Rapid Trigger")
            .checked(is_on)
            .disabled(busy)
            .tooltip("Re-trigger whenever the key changes direction")
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.set_rapid_trigger_enabled(*checked, cx);
            }))
    });
    let body = if count == 0 {
        let button = select_all("rapid-select-all", cx);
        idle(
            LucideIcon::MousePointer2,
            "Select keys on the board to set up Rapid Trigger for them.",
            cx,
        )
        .child(button)
        .into_any_element()
    } else {
        sensitivities(app, he, primary, !is_on, cx)
    };

    panel("rapid-trigger-group", cx)
        .size_full()
        .child(head("Rapid Trigger").children(partial).children(switch))
        .child(block(body))
        .into_any_element()
}

fn sensitivities(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    primary: RapidTriggerConfig,
    is_off: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let locked = app.is_busy() || is_off;
    let controls = app.controls.as_ref();
    let press = shared(app, he, primary.press_sensitivity, |k| {
        k.rapid_trigger.press_sensitivity
    });
    let lift = shared(app, he, primary.release_sensitivity, |k| {
        k.rapid_trigger.release_sensitivity
    });
    let press_row = sensitivity_row(
        "Press sensitivity",
        "press-sensitivity",
        press,
        controls.map(|c| c.rt_press.clone()),
        locked,
        true,
        cx,
    );
    let lift_row = sensitivity_row(
        "Lift sensitivity",
        "release-sensitivity",
        lift,
        controls.map(|c| c.rt_release.clone()),
        locked,
        false,
        cx,
    );
    let caps = &he.capabilities;
    let ends = caps.rapid_trigger_min.zip(caps.rapid_trigger_max);
    let theme = cx.theme();
    v_flex()
        .child(
            div()
                .mb(dp(6.))
                .text_size(dp(12.5))
                .text_color(theme.muted_foreground)
                .child("Fires and resets whenever the key changes direction, instead of at one fixed point."),
        )
        .child(
            v_flex()
                .when(is_off, |this| this.opacity(0.45))
                .child(press_row)
                .child(lift_row)
                .when_some(ends, |this, (min, max)| {
                    this.child(
                        h_flex()
                            .justify_between()
                            .font_family(KEY_FONT)
                            .text_size(dp(10.))
                            .text_color(tokens(cx).faint)
                            .child(mm(min))
                            .child(format!("{} mm", mm(max))),
                    )
                }),
        )
        .into_any_element()
}

fn sensitivity_row(
    label: &'static str,
    id: &'static str,
    value: Option<TravelHundredthsMm>,
    slider: Option<Entity<SliderState>>,
    locked: bool,
    is_press: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let button = |suffix: &str, verb: &str, direction: i32| {
        Button::new(SharedString::from(format!("{id}-{suffix}")))
            .accessibility_label(format!("{verb} {}", label.to_lowercase()))
            .disabled(locked)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.step_sensitivity(is_press, direction, cx);
            }))
    };
    let decrease = button("dec", "Decrease", -1);
    let increase = button("inc", "Increase", 1);
    let unit = if value.is_some() { "mm" } else { "" };
    let control = stepper(
        decrease,
        value.map_or_else(|| "Mixed".to_owned(), mm),
        unit,
        increase,
        cx,
    );
    v_flex()
        .gap(dp(4.))
        .mt(dp(4.))
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(label))
                .child(control),
        )
        .when_some(slider, |this, slider| {
            this.child(
                Slider::new(&slider)
                    .disabled(locked)
                    .accessibility_label(label),
            )
        })
        .into_any_element()
}
