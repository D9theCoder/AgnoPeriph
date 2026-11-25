//! Hall Effect editor, rendered for devices whose capabilities include Hall Effect switches.

use agnoperiph_core::{
    CalibrationStatus, DksBinding, DksTriggerDirection, HallEffectCapabilities, RapidTriggerConfig,
    SocdMode, SocdPair, TravelHundredthsMm,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, Theme,
    button::{Button, ButtonGroup, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    scroll::ScrollableElement,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, field, readout, section};
use crate::backend::{ActuationTarget, DeviceSnapshot, HallEffectState, SettingChange};
use crate::keymap::{KeyCap, RS7_LAYOUT, key_label};

/// Width of one key unit in rem; keycaps and gaps scale with the theme's base font.
const KEY_UNIT_REM: f32 = 3.0;
/// Space between neighbouring keycaps in rem (matches `gap_1`).
const KEY_GAP_REM: f32 = 0.25;

const SOCD_MODES: [(SocdMode, &str); 4] = [
    (SocdMode::LastInputPriority, "Last input"),
    (SocdMode::Neutral, "Neutral"),
    (SocdMode::KeyAPriority, "First key"),
    (SocdMode::KeyBPriority, "Second key"),
];

/// Snaps a travel distance onto `min + k * step` within `min..=max`.
pub fn snap_travel(
    value: i32,
    min: TravelHundredthsMm,
    max: TravelHundredthsMm,
    step: TravelHundredthsMm,
) -> TravelHundredthsMm {
    let min = i32::from(min.as_hundredths());
    let max = i32::from(max.as_hundredths());
    let step = i32::from(step.as_hundredths().max(1));
    let clamped = value.clamp(min, max);
    let mut snapped = min + (clamped - min + step / 2) / step * step;
    while snapped > max {
        snapped -= step;
    }
    TravelHundredthsMm::from_hundredths(u16::try_from(snapped).unwrap_or(0))
}

fn mm(value: TravelHundredthsMm) -> String {
    let raw = value.as_hundredths();
    format!("{}.{:02}", raw / 100, raw % 100)
}

fn socd_element_id(prefix: &str, pair: SocdPair, suffix: &str) -> ElementId {
    ElementId::Name(
        format!(
            "{prefix}-{}-{}-{suffix}",
            pair.key_a.value(),
            pair.key_b.value()
        )
        .into(),
    )
}

impl AgnoPeriphApp {
    fn hall_effect_state(&self) -> Option<&HallEffectState> {
        self.selected_device()?.hall_effect.as_ref()
    }

    /// The selected key's values with unsaved edits applied.
    fn key_values(&self) -> Option<(TravelHundredthsMm, RapidTriggerConfig)> {
        let config = self.hall_effect_state()?.key(self.selected_key)?;
        Some((
            self.key_draft.actuation.unwrap_or(config.actuation),
            self.key_draft.rapid_trigger.unwrap_or(config.rapid_trigger),
        ))
    }

    fn step_actuation(&mut self, direction: i32, cx: &mut Context<Self>) {
        let (Some(he), Some((actuation, _))) = (self.hall_effect_state(), self.key_values()) else {
            return;
        };
        let caps = &he.capabilities;
        let step = i32::from(caps.actuation_step.as_hundredths().max(1));
        let next = snap_travel(
            i32::from(actuation.as_hundredths()) + direction * step,
            caps.actuation_min,
            caps.actuation_max,
            caps.actuation_step,
        );
        self.key_draft.actuation = Some(next);
        cx.notify();
    }

    fn apply_actuation(&mut self, target: ActuationTarget, cx: &mut Context<Self>) {
        if let Some((point, _)) = self.key_values() {
            self.apply_change(SettingChange::Actuation { target, point }, cx);
        }
    }

    fn rapid_trigger_bounds(
        &self,
    ) -> Option<(TravelHundredthsMm, TravelHundredthsMm, TravelHundredthsMm)> {
        let caps = &self.hall_effect_state()?.capabilities;
        Some((
            caps.rapid_trigger_min?,
            caps.rapid_trigger_max?,
            caps.actuation_step,
        ))
    }

    fn set_rapid_trigger_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let (Some((_, config)), Some((min, max, step))) =
            (self.key_values(), self.rapid_trigger_bounds())
        else {
            return;
        };
        let continuous = enabled
            && self
                .hall_effect_state()
                .is_some_and(|he| he.capabilities.supports_continuous_rt);
        // Disabled keys may report 0 mm sensitivities, which the device rejects once enabled.
        let press = snap_travel(
            i32::from(config.press_sensitivity.as_hundredths()),
            min,
            max,
            step,
        );
        let release = snap_travel(
            i32::from(config.release_sensitivity.as_hundredths()),
            min,
            max,
            step,
        );
        self.key_draft.rapid_trigger =
            Some(RapidTriggerConfig::new(enabled, continuous, press, release));
        cx.notify();
    }

    fn step_sensitivity(&mut self, is_press: bool, direction: i32, cx: &mut Context<Self>) {
        let (Some((_, mut config)), Some((min, max, step))) =
            (self.key_values(), self.rapid_trigger_bounds())
        else {
            return;
        };
        let current = if is_press {
            config.press_sensitivity
        } else {
            config.release_sensitivity
        };
        let delta = direction * i32::from(step.as_hundredths().max(1));
        let next = snap_travel(i32::from(current.as_hundredths()) + delta, min, max, step);
        if is_press {
            config.press_sensitivity = next;
        } else {
            config.release_sensitivity = next;
        }
        self.key_draft.rapid_trigger = Some(config);
        cx.notify();
    }

    fn apply_rapid_trigger(&mut self, cx: &mut Context<Self>) {
        if let Some((_, config)) = self.key_values() {
            let key = self.selected_key;
            self.apply_change(SettingChange::RapidTrigger { key, config }, cx);
        }
    }

    fn discard_key_draft(&mut self, cx: &mut Context<Self>) {
        self.key_draft = crate::app::KeyDraft::default();
        cx.notify();
    }

    fn start_socd_pair(&mut self, cx: &mut Context<Self>) {
        self.socd_first = Some(self.selected_key);
        cx.notify();
    }

    fn cancel_socd_pair(&mut self, cx: &mut Context<Self>) {
        self.socd_first = None;
        cx.notify();
    }

    fn add_socd_pair(&mut self, cx: &mut Context<Self>) {
        let (Some(first), Some(he)) = (self.socd_first, self.hall_effect_state()) else {
            return;
        };
        let Ok(pair) = SocdPair::new(first, self.selected_key, SocdMode::LastInputPriority) else {
            return;
        };
        let mut pairs = he.snapshot.socd_pairs.clone();
        pairs.push(pair);
        self.socd_first = None;
        self.apply_change(SettingChange::Socd(pairs), cx);
    }

    fn set_socd_mode(&mut self, target: SocdPair, mode: SocdMode, cx: &mut Context<Self>) {
        let Some(he) = self.hall_effect_state() else {
            return;
        };
        let pairs = he
            .snapshot
            .socd_pairs
            .iter()
            .map(|pair| {
                if pair.key_a == target.key_a && pair.key_b == target.key_b {
                    SocdPair::new(pair.key_a, pair.key_b, mode).unwrap_or(*pair)
                } else {
                    *pair
                }
            })
            .collect();
        self.apply_change(SettingChange::Socd(pairs), cx);
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let Some(he) = device.hall_effect.as_ref() else {
        return section(
            "hall-effect-panel",
            "Switches",
            "The keyboard reports Hall Effect switches, but their settings couldn’t be read.",
            cx.theme(),
        );
    };
    let base = section(
        "hall-effect-panel",
        "Switches",
        "Actuation depth and Rapid Trigger for each key. Select a key on the map to edit it.",
        cx.theme(),
    );

    base.child(render_matrix(app, he, cx)).child(
        h_flex()
            .items_start()
            .gap_8()
            .child(render_key_editor(app, he, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_6()
                    .child(render_calibration(he.snapshot.calibration, cx.theme()))
                    .when(he.capabilities.has_socd(), |this| {
                        this.child(render_socd(app, he, cx))
                    })
                    .when(he.capabilities.has_dks(), |this| {
                        this.child(render_dks(app, he, cx))
                    }),
            ),
    )
}

fn render_matrix(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let rows: Vec<AnyElement> = RS7_LAYOUT
        .iter()
        .map(|row| {
            h_flex()
                .gap_1()
                .children(row.iter().map(|cap| render_keycap(app, he, cap, cx)))
                .into_any_element()
        })
        .collect();
    let theme = cx.theme();
    let rt_count = he
        .snapshot
        .keys
        .iter()
        .filter(|k| k.rapid_trigger.is_enabled())
        .count();

    v_flex()
        .gap_2()
        .child(
            v_flex()
                .id("key-matrix")
                .p_3()
                .gap_1()
                .rounded(theme.radius_lg)
                .bg(theme.group_box)
                .border_1()
                .border_color(theme.border)
                .children(rows)
                .overflow_x_scrollbar(),
        )
        .child(
            h_flex()
                .gap_4()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(div().w_4().h_1().rounded_sm().bg(theme.primary))
                        .child("Bar length shows actuation depth"),
                )
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(div().size_1p5().rounded_full().bg(theme.primary))
                        .child(format!("Rapid Trigger on {rt_count} keys")),
                ),
        )
}

fn render_keycap(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cap: &KeyCap,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let key = cap.index();
    let config = he.key(key);
    let is_selected = app.selected_key() == key;
    let max = f32::from(he.capabilities.actuation_max.as_hundredths().max(1));
    let depth = config.map_or(0.0, |c| f32::from(c.actuation.as_hundredths()) / max);
    let has_rt = config.is_some_and(|c| c.rapid_trigger.is_enabled());
    let width = f32::from(cap.width) / 100.0 * KEY_UNIT_REM - KEY_GAP_REM;
    let gap = f32::from(cap.gap) / 100.0 * KEY_UNIT_REM;
    let actuation_text = config.map_or_else(|| "–".to_owned(), |c| mm(c.actuation));
    let bar_fill = theme.primary;

    Button::new(("key", u64::from(key.value())))
        .selected(is_selected)
        .accessibility_label(format!("{} key, actuation {actuation_text} mm", cap.label))
        .ml(rems(gap))
        .w(rems(width))
        .h_12()
        .px_1()
        .py_1()
        .child(
            v_flex()
                .size_full()
                .justify_between()
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .child(cap.label),
                        )
                        .when(has_rt, |this| {
                            this.child(div().size_1p5().rounded_full().bg(bar_fill))
                        }),
                )
                .child(
                    v_flex()
                        .w_full()
                        .gap_0p5()
                        .child(
                            div()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .opacity(0.8)
                                .child(actuation_text),
                        )
                        .child(
                            div()
                                .w_full()
                                .h_0p5()
                                .rounded_sm()
                                .bg(theme.border)
                                .child(div().h_full().rounded_sm().w(relative(depth)).bg(bar_fill)),
                        ),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.select_key(key, cx)))
}

fn render_key_editor(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let key = app.selected_key();
    let label = key_label(key);
    let Some((actuation, rapid_trigger)) = app.key_values() else {
        return v_flex()
            .flex_1()
            .min_w_0()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(format!("The {label} key has no Hall Effect data."));
    };
    let busy = app.is_busy();
    let has_draft = app.key_draft != crate::app::KeyDraft::default();
    let caps = &he.capabilities;

    v_flex()
        .flex_1()
        .min_w_0()
        .gap_4()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("{label} key")),
        )
        .child(render_actuation(
            actuation,
            app.key_draft.actuation.is_some(),
            caps,
            busy,
            cx,
        ))
        .when(caps.has_rapid_trigger(), |this| {
            this.child(render_rapid_trigger(
                rapid_trigger,
                app.key_draft.rapid_trigger.is_some(),
                busy,
                cx,
            ))
        })
        .when(has_draft, |this| {
            this.child(
                h_flex().child(
                    Button::new("discard-key-draft")
                        .ghost()
                        .small()
                        .label("Discard edits")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.discard_key_draft(cx))),
                ),
            )
        })
}

fn stepper<F>(
    prefix: &'static str,
    value: TravelHundredthsMm,
    disabled: bool,
    on_step: F,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<F>
where
    F: Fn(&mut AgnoPeriphApp, i32, &mut Context<AgnoPeriphApp>) + Copy + 'static,
{
    h_flex()
        .gap_2()
        .child(
            Button::new(SharedString::from(format!("{prefix}-dec")))
                .outline()
                .xsmall()
                .label("−")
                .accessibility_label(format!("Decrease {prefix}"))
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| on_step(this, -1, cx))),
        )
        .child(readout(format!("{} mm", mm(value)), cx.theme()))
        .child(
            Button::new(SharedString::from(format!("{prefix}-inc")))
                .outline()
                .xsmall()
                .label("+")
                .accessibility_label(format!("Increase {prefix}"))
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| on_step(this, 1, cx))),
        )
}

fn render_actuation(
    actuation: TravelHundredthsMm,
    is_edited: bool,
    caps: &HallEffectCapabilities,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let hint = format!(
        "{} to {} mm in steps of {} mm",
        mm(caps.actuation_min),
        mm(caps.actuation_max),
        mm(caps.actuation_step)
    );
    let control = stepper(
        "actuation",
        actuation,
        busy,
        AgnoPeriphApp::step_actuation,
        cx,
    );
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .child(field("Actuation point", theme).child(control))
        .child(
            field("", theme).child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(hint),
            ),
        )
        .child(
            field("", cx.theme()).child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("apply-actuation-key")
                            .primary()
                            .small()
                            .label("Apply to key")
                            .loading(busy)
                            .disabled(!is_edited)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let key = this.selected_key();
                                this.apply_actuation(ActuationTarget::Key(key), cx);
                            })),
                    )
                    .child(
                        Button::new("apply-actuation-all")
                            .outline()
                            .small()
                            .label("Apply to all keys")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.apply_actuation(ActuationTarget::AllKeys, cx);
                            })),
                    ),
            ),
        )
}

fn render_rapid_trigger(
    config: RapidTriggerConfig,
    is_edited: bool,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let enabled = config.is_enabled();
    let press = stepper(
        "press-sensitivity",
        config.press_sensitivity,
        busy || !enabled,
        |this, direction, cx| this.step_sensitivity(true, direction, cx),
        cx,
    );
    let release = stepper(
        "release-sensitivity",
        config.release_sensitivity,
        busy || !enabled,
        |this, direction, cx| this.step_sensitivity(false, direction, cx),
        cx,
    );
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .child(
            field("Rapid Trigger", theme).child(
                Checkbox::new("rapid-trigger")
                    .label("Enabled")
                    .checked(enabled)
                    .disabled(busy)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_rapid_trigger_enabled(*checked, cx);
                    })),
            ),
        )
        .child(field("Press sensitivity", cx.theme()).child(press))
        .child(field("Release sensitivity", cx.theme()).child(release))
        .child(
            field("", cx.theme()).child(
                Button::new("apply-rapid-trigger")
                    .primary()
                    .small()
                    .label("Apply Rapid Trigger")
                    .loading(busy)
                    .disabled(!is_edited)
                    .on_click(cx.listener(|this, _, _, cx| this.apply_rapid_trigger(cx))),
            ),
        )
}

fn render_calibration(status: CalibrationStatus, theme: &Theme) -> impl IntoElement + use<> {
    let tag = match status {
        CalibrationStatus::Calibrated => Tag::success().small().child("Calibrated"),
        CalibrationStatus::NeedsCalibration => Tag::warning().small().child("Needs calibration"),
        CalibrationStatus::InProgress { progress_pct } => Tag::info()
            .small()
            .child(format!("Calibrating, {progress_pct}%")),
        CalibrationStatus::Unknown => Tag::secondary().small().child("Unknown"),
    };
    v_flex()
        .gap_2()
        .child(field("Calibration", theme).child(tag))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Shown for reference. Calibration runs in the manufacturer’s utility."),
        )
}

fn render_socd(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let busy = app.is_busy();
    let pairs = he.snapshot.socd_pairs.clone();
    let is_full = pairs.len() >= usize::from(he.capabilities.max_socd_pairs);
    let selected = app.selected_key();

    let rows: Vec<AnyElement> = pairs
        .iter()
        .map(|&pair| {
            let modes = SOCD_MODES.iter().map(|&(mode, label)| {
                Button::new(socd_element_id("socd-mode", pair, label))
                    .label(label)
                    .selected(pair.mode == mode)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if pair.mode != mode {
                            this.set_socd_mode(pair, mode, cx);
                        }
                    }))
            });
            v_flex()
                .gap_1p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!(
                                    "{} and {}",
                                    key_label(pair.key_a),
                                    key_label(pair.key_b)
                                )),
                        )
                        .child(
                            Button::new(socd_element_id("socd-remove", pair, "x"))
                                .ghost()
                                .xsmall()
                                .label("Remove")
                                .disabled(true)
                                .tooltip("The keyboard driver can’t clear a pair slot yet"),
                        ),
                )
                .child(
                    ButtonGroup::new(socd_element_id("socd-modes", pair, "group"))
                        .outline()
                        .xsmall()
                        .children(modes),
                )
                .into_any_element()
        })
        .collect();

    let add_controls = match app.socd_first {
        None => h_flex().child(
            Button::new("socd-start")
                .outline()
                .small()
                .label(format!("Pair {}…", key_label(selected)))
                .disabled(busy || is_full)
                .on_click(cx.listener(|this, _, _, cx| this.start_socd_pair(cx))),
        ),
        Some(first) => h_flex()
            .gap_2()
            .child(
                Button::new("socd-add")
                    .primary()
                    .small()
                    .label(format!(
                        "Pair {} with {}",
                        key_label(first),
                        key_label(selected)
                    ))
                    .disabled(busy || first == selected)
                    .on_click(cx.listener(|this, _, _, cx| this.add_socd_pair(cx))),
            )
            .child(
                Button::new("socd-cancel")
                    .ghost()
                    .small()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_socd_pair(cx))),
            ),
    };

    let theme = cx.theme();
    v_flex()
        .gap_3()
        .child(subheading("SOCD pairs", theme))
        .when(pairs.is_empty(), |this| {
            this.child(muted_note(
                "No pairs. Opposing keys act independently.",
                theme,
            ))
        })
        .children(rows)
        .when(app.socd_first.is_some(), |this| {
            this.child(muted_note("Select the second key on the map.", theme))
        })
        .child(add_controls)
}

fn describe_dks(binding: &DksBinding) -> String {
    let stages: Vec<String> = binding
        .stages
        .iter()
        .flatten()
        .map(|stage| {
            let direction = match stage.direction {
                DksTriggerDirection::Press => "Press",
                DksTriggerDirection::Release => "Release",
            };
            let hold = if stage.hold { ", hold" } else { "" };
            format!(
                "{direction} {} mm sends 0x{:02X}{hold}",
                mm(stage.trigger_travel),
                stage.hid_keycode
            )
        })
        .collect();
    stages.join("; ")
}

fn render_dks(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let busy = app.is_busy();
    let rows: Vec<AnyElement> = he
        .snapshot
        .dks_bindings
        .iter()
        .map(|binding| {
            let slot = binding.key;
            h_flex()
                .gap_3()
                .items_start()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("Slot {}", slot.value() + 1)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(describe_dks(binding)),
                        ),
                )
                .child(
                    Button::new(("dks-clear", u64::from(slot.value())))
                        .outline()
                        .xsmall()
                        .label("Clear")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_change(SettingChange::ClearDks { slot }, cx);
                        })),
                )
                .into_any_element()
        })
        .collect();
    let theme = cx.theme();
    let used = he.snapshot.dks_bindings.len();
    v_flex()
        .gap_3()
        .child(subheading(
            format!(
                "Dynamic keystrokes ({used} of {} slots)",
                he.capabilities.max_dks_bindings
            ),
            theme,
        ))
        .when(rows.is_empty(), |this| {
            this.child(muted_note("No dynamic keystroke slots are in use.", theme))
        })
        .children(rows)
}

fn subheading(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child(text.into())
}

fn muted_note(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use agnoperiph_core::TravelHundredthsMm;

    use super::snap_travel;

    fn t(v: u16) -> TravelHundredthsMm {
        TravelHundredthsMm::from_hundredths(v)
    }

    #[test]
    fn travel_snaps_onto_step_grid_inside_range() {
        assert_eq!(snap_travel(121, t(10), t(340), t(2)), t(122));
        assert_eq!(snap_travel(0, t(10), t(340), t(2)), t(10));
        assert_eq!(snap_travel(999, t(10), t(340), t(2)), t(340));
        assert_eq!(snap_travel(15, t(2), t(340), t(2)), t(16));
    }
}
