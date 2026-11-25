//! Pointer editor, rendered for devices whose capabilities include a pointer.

use agnoperiph_core::{
    DebounceMs, DpiStage, DpiValue, LiftOffDistance, PointerCapabilities, PointerSnapshot,
    PollingRateHz,
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

use crate::app::{AgnoPeriphApp, field, readout, section, was_note};
use crate::backend::{DeviceSnapshot, PointerChange, PointerState, SettingChange};
use crate::draft::{ChangeId, EditKey};

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
        let mut stages = pointer.snapshot.dpi_stages.clone();
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
        self.edit_draft(
            EditKey::Change(ChangeId::DpiStages),
            false,
            |draft, _| draft.pointer.dpi_stages = Some(stages),
            cx,
        );
    }

    /// Stages one pointer setting.
    fn stage_pointer(&mut self, change: PointerChange, cx: &mut Context<Self>) {
        let id = match change {
            PointerChange::PollingRate(_) => ChangeId::PollingRate,
            PointerChange::LiftOffDistance(_) => ChangeId::LiftOff,
            PointerChange::Debounce(_) => ChangeId::Debounce,
            PointerChange::MotionSync(_) => ChangeId::MotionSync,
            PointerChange::AngleSnapping(_) => ChangeId::AngleSnapping,
            PointerChange::RippleControl(_) => ChangeId::RippleControl,
            _ => return,
        };
        self.edit_draft(
            EditKey::Change(id),
            false,
            move |draft, _| {
                let p = &mut draft.pointer;
                match change {
                    PointerChange::PollingRate(rate) => p.polling_rate = Some(rate),
                    PointerChange::LiftOffDistance(lod) => p.lod = Some(lod),
                    PointerChange::Debounce(ms) => p.debounce = Some(ms),
                    PointerChange::MotionSync(on) => p.motion_sync = Some(on),
                    PointerChange::AngleSnapping(on) => p.angle_snapping = Some(on),
                    PointerChange::RippleControl(on) => p.ripple_control = Some(on),
                    _ => {}
                }
            },
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
        "DPI stages, polling rate and sensor options stored on the mouse.",
        cx.theme(),
    );
    let busy = app.is_busy();
    let raw = app
        .selected_raw()
        .and_then(|d| d.pointer.as_ref())
        .map(|p| p.snapshot.clone());
    let saved = raw.clone().unwrap_or_else(|| pointer.snapshot.clone());

    base.child(render_dpi_stages(pointer, &saved, busy, cx))
        .child(render_polling(pointer, &saved, busy, cx))
        .when(!pointer.capabilities.supported_lods.is_empty(), |this| {
            this.child(render_lod(pointer, &saved, busy, cx))
        })
        .when_some(
            pointer
                .capabilities
                .debounce_range_ms
                .zip(pointer.snapshot.debounce_ms),
            |this, (range, current)| {
                this.child(render_debounce(range, current, saved.debounce_ms, busy, cx))
            },
        )
        .when_some(
            pointer
                .snapshot
                .motion_sync
                .filter(|_| pointer.capabilities.supports_motion_sync),
            |this, enabled| {
                this.child(render_sensor_switch(
                    SensorSwitch::MotionSync,
                    enabled,
                    saved.motion_sync,
                    busy,
                    cx,
                ))
            },
        )
        .when_some(
            pointer
                .snapshot
                .angle_snapping
                .filter(|_| pointer.capabilities.supports_angle_snapping),
            |this, enabled| {
                this.child(render_sensor_switch(
                    SensorSwitch::AngleSnapping,
                    enabled,
                    saved.angle_snapping,
                    busy,
                    cx,
                ))
            },
        )
        .when_some(
            pointer
                .snapshot
                .ripple_control
                .filter(|_| pointer.capabilities.supports_ripple_control),
            |this, enabled| {
                this.child(render_sensor_switch(
                    SensorSwitch::RippleControl,
                    enabled,
                    saved.ripple_control,
                    busy,
                    cx,
                ))
            },
        )
}

fn render_dpi_stages(
    pointer: &PointerState,
    saved: &PointerSnapshot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let stages: Vec<DpiStage> = pointer.snapshot.dpi_stages.clone();
    let active = pointer.snapshot.active_dpi_stage;
    let caps = &pointer.capabilities;

    let rows: Vec<AnyElement> = stages
        .iter()
        .map(|stage| {
            let was = saved
                .dpi_stages
                .iter()
                .find(|s| s.index == stage.index)
                .filter(|s| s.x != stage.x || s.y != stage.y)
                .map(|s| s.x.value());
            render_stage_row(stage, was, stage.index == active, caps, busy, cx).into_any_element()
        })
        .collect();

    let theme = cx.theme();
    field("DPI stages", theme).items_start().child(
        v_flex().flex_1().gap_2().children(rows).child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{} to {} DPI in steps of {}. Select a stage to make it active.",
                    caps.dpi_min, caps.dpi_max, caps.dpi_step
                )),
        ),
    )
}

fn render_stage_row(
    stage: &DpiStage,
    was: Option<u32>,
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
                .tooltip(if is_active {
                    "The mouse is using this stage"
                } else {
                    "Switch the mouse to this stage now"
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !is_active {
                        this.apply_change(
                            SettingChange::Pointer(PointerChange::ActiveDpiStage(stage_ix)),
                            cx,
                        );
                    }
                })),
        )
        .when_some(was, |this, was| {
            this.child(was_note(was.to_string(), cx.theme()).id(("dpi-was", id)))
        })
}

fn render_polling(
    pointer: &PointerState,
    saved: &PointerSnapshot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let current = pointer.snapshot.polling_rate;
    let was = (saved.polling_rate != current).then(|| polling_label(saved.polling_rate));
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
                        this.stage_pointer(PointerChange::PollingRate(rate), cx);
                    }
                }))
        });
    let group = ButtonGroup::new("polling-rates")
        .outline()
        .small()
        .children(buttons);
    let theme = cx.theme();
    field("Polling rate", theme)
        .child(group)
        .when_some(was, |this, was| {
            this.child(was_note(was, theme).id("polling-was"))
        })
}

fn polling_label(rate: PollingRateHz) -> String {
    match rate.as_hz() {
        hz if hz >= 1000 => format!("{}K Hz", hz / 1000),
        hz => format!("{hz} Hz"),
    }
}

fn render_lod(
    pointer: &PointerState,
    saved: &PointerSnapshot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let current = pointer.snapshot.lod;
    let was =
        (saved.lod != current).then(|| saved.lod.map_or_else(|| "–".to_owned(), |l| l.to_string()));
    let buttons = pointer.capabilities.supported_lods.iter().map(|&lod| {
        let key = match lod {
            LiftOffDistance::Low => 1,
            LiftOffDistance::Medium => 3,
            LiftOffDistance::High => 2,
            LiftOffDistance::TenthsMm(tenths) => 100 + u64::from(tenths),
        };
        Button::new(("lod", key))
            .label(lod.to_string())
            .selected(Some(lod) == current)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                if Some(lod) != current {
                    this.stage_pointer(PointerChange::LiftOffDistance(lod), cx);
                }
            }))
    });
    let group = ButtonGroup::new("lift-off")
        .outline()
        .small()
        .children(buttons);
    let theme = cx.theme();
    field("Lift-off distance", theme)
        .child(group)
        .when_some(was, |this, was| {
            this.child(was_note(was, theme).id("lod-was"))
        })
}

fn render_debounce(
    (min, max): (u8, u8),
    current: DebounceMs,
    saved: Option<DebounceMs>,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let value = current.value();
    let was = saved.filter(|s| *s != current);
    let set = move |next: u8| {
        move |this: &mut AgnoPeriphApp,
              _: &ClickEvent,
              _: &mut Window,
              cx: &mut Context<AgnoPeriphApp>| {
            this.stage_pointer(PointerChange::Debounce(DebounceMs::new(next)), cx);
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
            )
            .when_some(was, |this, was| {
                this.child(was_note(was.to_string(), cx.theme()).id("debounce-was"))
            }),
    )
}

/// On/off sensor features that share one switch row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SensorSwitch {
    MotionSync,
    AngleSnapping,
    RippleControl,
}

impl SensorSwitch {
    const fn label(self) -> &'static str {
        match self {
            Self::MotionSync => "Motion sync",
            Self::AngleSnapping => "Angle snapping",
            Self::RippleControl => "Ripple control",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::MotionSync => "motion-sync",
            Self::AngleSnapping => "angle-snapping",
            Self::RippleControl => "ripple-control",
        }
    }

    const fn change(self, enabled: bool) -> PointerChange {
        match self {
            Self::MotionSync => PointerChange::MotionSync(enabled),
            Self::AngleSnapping => PointerChange::AngleSnapping(enabled),
            Self::RippleControl => PointerChange::RippleControl(enabled),
        }
    }
}

fn render_sensor_switch(
    kind: SensorSwitch,
    enabled: bool,
    saved: Option<bool>,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let was = saved
        .filter(|s| *s != enabled)
        .map(|s| if s { "on" } else { "off" });
    let theme = cx.theme();
    field(kind.label(), theme)
        .child(
            Switch::new(kind.id())
                .checked(enabled)
                .disabled(busy)
                .accessibility_label(kind.label())
                .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                    this.stage_pointer(kind.change(*checked), cx);
                })),
        )
        .when_some(was, |this, was| this.child(was_note(was, theme)))
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
