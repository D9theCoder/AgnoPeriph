//! Pointer editor, rendered for devices whose capabilities include a pointer.

use agnoperiph_core::{
    DebounceMs, DpiStage, DpiValue, LiftOffDistance, PointerCapabilities, PollingRateHz,
    ProfileSlot,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, field, readout, section};
use crate::backend::{DeviceSnapshot, PointerChange, PointerState, SettingChange};

/// Snaps a DPI value onto the device grid `dpi_min + k * dpi_step` inside its range.
pub fn snap_dpi(value: i64, caps: &PointerCapabilities) -> u32 {
    let min = i64::from(caps.dpi_min);
    let max = i64::from(caps.dpi_max);
    let step = i64::from(caps.dpi_step.max(1));
    let clamped = value.clamp(min, max);
    let steps = (clamped - min + step / 2) / step;
    let mut snapped = min + steps * step;
    while snapped > max {
        snapped -= step;
    }
    u32::try_from(snapped).unwrap_or(caps.dpi_min)
}

/// DPI change per stepper click: the device step, coarsened to at least 100 DPI.
fn dpi_increment(caps: &PointerCapabilities) -> i64 {
    let step = i64::from(caps.dpi_step.max(1));
    step * (100 / step).max(1)
}

impl AgnoPeriphApp {
    fn pointer_state(&self) -> Option<&PointerState> {
        self.selected_device()?.pointer.as_ref()
    }

    pub(crate) fn step_dpi(&mut self, stage_ix: u8, direction: i64, cx: &mut Context<Self>) {
        let Some(pointer) = self.pointer_state() else {
            return;
        };
        let caps = pointer.capabilities.clone();
        let mut stages = self
            .dpi_draft
            .clone()
            .unwrap_or_else(|| pointer.snapshot.dpi_stages.clone());
        let Some(stage) = stages.iter_mut().find(|s| s.index == stage_ix) else {
            return;
        };
        let next = snap_dpi(
            i64::from(stage.x.value()) + direction * dpi_increment(&caps),
            &caps,
        );
        let Ok(next) = DpiValue::new(next) else {
            return;
        };
        stage.x = next;
        // A linked Y axis follows X; an independent Y keeps its own value.
        if stage.y.is_some_and(|y| y.value() == stage.x.value())
            || !caps.supports_xy_independent_dpi
        {
            stage.y = None;
        }
        self.dpi_draft = Some(stages);
        cx.notify();
    }

    pub(crate) fn discard_dpi_draft(&mut self, cx: &mut Context<Self>) {
        self.dpi_draft = None;
        cx.notify();
    }

    pub(crate) fn apply_dpi_draft(&mut self, cx: &mut Context<Self>) {
        let (Some(stages), Some(pointer)) = (self.dpi_draft.clone(), self.pointer_state()) else {
            return;
        };
        let active = pointer.snapshot.active_dpi_stage;
        self.apply_change(
            SettingChange::Pointer(PointerChange::DpiStages { stages, active }),
            cx,
        );
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let Some(pointer) = device.pointer.as_ref() else {
        return section(
            "pointer-panel",
            "Sensor",
            "The device reports a pointer, but its settings couldn’t be read.",
            cx.theme(),
        );
    };
    let base = section(
        "pointer-panel",
        "Sensor",
        "DPI stages, polling rate and lift-off distance stored on the mouse.",
        cx.theme(),
    );
    let busy = app.is_busy();

    base.child(render_dpi_stages(app, pointer, busy, cx))
        .child(render_polling(pointer, busy, cx))
        .when(!pointer.capabilities.supported_lods.is_empty(), |this| {
            this.child(render_lod(pointer, busy, cx))
        })
        .when_some(
            pointer
                .capabilities
                .debounce_range_ms
                .zip(pointer.snapshot.debounce_ms),
            |this, (range, current)| this.child(render_debounce(range, current, busy, cx)),
        )
        .when_some(
            pointer
                .snapshot
                .motion_sync
                .filter(|_| pointer.capabilities.supports_motion_sync),
            |this, enabled| this.child(render_motion_sync(enabled, busy, cx)),
        )
        .when(device.capabilities.has_profiles(), |this| {
            this.when_some(device.profiles.as_ref(), |this, profiles| {
                this.child(render_profiles(
                    profiles.snapshot.active_slot,
                    profiles.capabilities.slot_count,
                    busy,
                    cx,
                ))
            })
        })
}

fn render_dpi_stages(
    app: &AgnoPeriphApp,
    pointer: &PointerState,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let stages: Vec<DpiStage> = app
        .dpi_draft
        .clone()
        .unwrap_or_else(|| pointer.snapshot.dpi_stages.clone());
    let has_draft = app
        .dpi_draft
        .as_ref()
        .is_some_and(|draft| *draft != pointer.snapshot.dpi_stages);
    let active = pointer.snapshot.active_dpi_stage;
    let caps = &pointer.capabilities;

    let rows: Vec<AnyElement> = stages
        .iter()
        .map(|stage| {
            render_stage_row(stage, stage.index == active, caps, busy, cx).into_any_element()
        })
        .collect();

    let theme = cx.theme();
    field("DPI stages", theme).items_start().child(
        v_flex()
            .flex_1()
            .gap_2()
            .children(rows)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} to {} DPI in steps of {}. Select a stage to make it active.",
                        caps.dpi_min, caps.dpi_max, caps.dpi_step
                    )),
            )
            .when(has_draft, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("dpi-apply")
                                .primary()
                                .small()
                                .label("Apply DPI stages")
                                .loading(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.apply_dpi_draft(cx);
                                })),
                        )
                        .child(
                            Button::new("dpi-discard")
                                .ghost()
                                .small()
                                .label("Discard")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.discard_dpi_draft(cx);
                                })),
                        ),
                )
            }),
    )
}

fn render_stage_row(
    stage: &DpiStage,
    is_active: bool,
    caps: &PointerCapabilities,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let stage_ix = stage.index;
    let id = u64::from(stage_ix);
    let value = stage.x.value();
    // The indicator LED color is device data, so it is the one raw color shown here.
    let swatch = stage
        .indicator_color
        .map(|c| rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)));
    let dpi_label = match stage.y {
        Some(y) if y != stage.x => format!("{value} × {}", y.value()),
        _ => format!("{value}"),
    };

    h_flex()
        .gap_3()
        .child(
            div()
                .size_3()
                .flex_shrink_0()
                .rounded_full()
                .border_1()
                .border_color(theme.border)
                .when_some(swatch, Styled::bg),
        )
        .child(
            div()
                .w_16()
                .text_sm()
                .child(format!("Stage {}", u16::from(stage_ix) + 1)),
        )
        .child(
            Button::new(("dpi-dec", id))
                .outline()
                .xsmall()
                .label("−")
                .accessibility_label(format!("Lower stage {} DPI", u16::from(stage_ix) + 1))
                .disabled(busy || value <= caps.dpi_min)
                .on_click(cx.listener(move |this, _, _, cx| this.step_dpi(stage_ix, -1, cx))),
        )
        .child(readout(dpi_label, theme))
        .child(
            Button::new(("dpi-inc", id))
                .outline()
                .xsmall()
                .label("+")
                .accessibility_label(format!("Raise stage {} DPI", u16::from(stage_ix) + 1))
                .disabled(busy || value >= caps.dpi_max)
                .on_click(cx.listener(move |this, _, _, cx| this.step_dpi(stage_ix, 1, cx))),
        )
        .child(
            Button::new(("dpi-active", id))
                .ghost()
                .xsmall()
                .label(if is_active { "Active" } else { "Use" })
                .selected(is_active)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !is_active {
                        this.apply_change(
                            SettingChange::Pointer(PointerChange::ActiveDpiStage(stage_ix)),
                            cx,
                        );
                    }
                })),
        )
}

fn render_polling(
    pointer: &PointerState,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let current = pointer.snapshot.polling_rate;
    let buttons = pointer
        .capabilities
        .supported_polling_rates
        .iter()
        .map(|&rate| {
            Button::new(("polling-rate", u64::from(rate.as_hz())))
                .label(polling_label(rate))
                .selected(rate == current)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if rate != current {
                        this.apply_change(
                            SettingChange::Pointer(PointerChange::PollingRate(rate)),
                            cx,
                        );
                    }
                }))
        });
    let group = ButtonGroup::new("polling-rates")
        .outline()
        .small()
        .children(buttons);
    field("Polling rate", cx.theme()).child(group)
}

fn polling_label(rate: PollingRateHz) -> String {
    match rate.as_hz() {
        hz if hz >= 1000 => format!("{}K Hz", hz / 1000),
        hz => format!("{hz} Hz"),
    }
}

fn render_lod(
    pointer: &PointerState,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let current = pointer.snapshot.lod;
    let buttons = pointer.capabilities.supported_lods.iter().map(|&lod| {
        let key = match lod {
            LiftOffDistance::Low => 1,
            LiftOffDistance::High => 2,
            LiftOffDistance::TenthsMm(tenths) => 100 + u64::from(tenths),
        };
        Button::new(("lod", key))
            .label(lod.to_string())
            .selected(Some(lod) == current)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                if Some(lod) != current {
                    this.apply_change(
                        SettingChange::Pointer(PointerChange::LiftOffDistance(lod)),
                        cx,
                    );
                }
            }))
    });
    let group = ButtonGroup::new("lift-off")
        .outline()
        .small()
        .children(buttons);
    field("Lift-off distance", cx.theme()).child(group)
}

fn render_debounce(
    (min, max): (u8, u8),
    current: DebounceMs,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let value = current.value();
    let set = move |next: u8| {
        move |this: &mut AgnoPeriphApp,
              _: &ClickEvent,
              _: &mut Window,
              cx: &mut Context<AgnoPeriphApp>| {
            this.apply_change(
                SettingChange::Pointer(PointerChange::Debounce(DebounceMs::new(next))),
                cx,
            );
        }
    };
    let theme = cx.theme();
    field("Click debounce", theme).child(
        h_flex()
            .gap_2()
            .child(
                Button::new("debounce-dec")
                    .outline()
                    .xsmall()
                    .label("−")
                    .accessibility_label("Shorter debounce")
                    .disabled(busy || value <= min)
                    .on_click(cx.listener(set(value.saturating_sub(1)))),
            )
            .child(readout(current.to_string(), cx.theme()))
            .child(
                Button::new("debounce-inc")
                    .outline()
                    .xsmall()
                    .label("+")
                    .accessibility_label("Longer debounce")
                    .disabled(busy || value >= max)
                    .on_click(cx.listener(set(value.saturating_add(1)))),
            ),
    )
}

fn render_motion_sync(
    enabled: bool,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    field("Motion sync", cx.theme()).child(
        Switch::new("motion-sync")
            .checked(enabled)
            .disabled(busy)
            .accessibility_label("Motion sync")
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.apply_change(
                    SettingChange::Pointer(PointerChange::MotionSync(*checked)),
                    cx,
                );
            })),
    )
}

fn render_profiles(
    active: ProfileSlot,
    slot_count: u8,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let buttons = (0..slot_count).map(ProfileSlot::new).map(|slot| {
        Button::new(("profile", u64::from(slot.index())))
            .label(format!("{}", slot.display_number()))
            .accessibility_label(slot.to_string())
            .selected(slot == active)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                if slot != active {
                    this.apply_change(SettingChange::Profile(slot), cx);
                }
            }))
    });
    let group = ButtonGroup::new("profiles")
        .outline()
        .small()
        .children(buttons);
    field("Onboard profile", cx.theme()).child(group)
}

#[cfg(test)]
mod tests {
    use agnoperiph_core::PointerCapabilities;

    use super::{dpi_increment, snap_dpi};

    #[test]
    fn dpi_snaps_onto_device_grid() {
        let caps = PointerCapabilities::new(50, 26_000, 50, 8);
        assert_eq!(snap_dpi(820, &caps), 800);
        assert_eq!(snap_dpi(830, &caps), 850);
        assert_eq!(snap_dpi(10, &caps), 50);
        assert_eq!(snap_dpi(40_000, &caps), 26_000);
        assert_eq!(dpi_increment(&caps), 100);

        let odd = PointerCapabilities::new(100, 1_050, 200, 4);
        assert_eq!(snap_dpi(2_000, &odd), 900);
    }
}
