//! Pointer editor, rendered for devices whose capabilities include a pointer.

use agnoperiph_core::{
    DebounceMs, DpiStage, DpiValue, LiftOffDistance, PointerCapabilities, PointerSnapshot,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    h_flex,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::{DeviceSnapshot, PointerChange, PointerState, SettingChange};
use crate::design::{DISPLAY_FONT, KEY_FONT, dp, tabular, tokens};
use crate::draft::{ChangeId, EditKey};
use crate::widgets::{device_setting_row, panel, panel_head, segment, segmented, stepper};

const GAP: f32 = 16.0;
const MIN_COLUMN_WIDTH: f32 = 340.0;

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

    /// Switching stages isn't staged: the mouse changes at once, like its own DPI button.
    fn use_dpi_stage(&mut self, stage_ix: u8, cx: &mut Context<Self>) {
        self.apply_change(
            SettingChange::Pointer(PointerChange::ActiveDpiStage(stage_ix)),
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
    workspace_width: f32,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let Some(pointer) = device.pointer.as_ref() else {
        return panel("pointer-panel", cx)
            .child(panel_head("Sensor"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("The mouse’s sensor settings couldn’t be read."),
            )
            .into_any_element();
    };
    let busy = app.is_busy();
    let saved = app
        .selected_raw()
        .and_then(|d| d.pointer.as_ref())
        .map_or_else(|| pointer.snapshot.clone(), |p| p.snapshot.clone());

    let stages = render_dpi_stages(pointer, &saved, busy, cx);
    let reporting = render_reporting(pointer, &saved, busy, cx);
    let sensor = render_sensor_options(pointer, &saved, busy, cx);
    let columns = if workspace_width >= MIN_COLUMN_WIDTH * 2.0 + GAP {
        // Reporting keeps its column on mice without sensor options.
        let second = match sensor {
            Some(sensor) => sensor.flex_1().into_any_element(),
            None => div().flex_1().into_any_element(),
        };
        h_flex()
            .items_stretch()
            .gap(dp(GAP))
            .child(reporting.flex_1())
            .child(second)
    } else {
        v_flex().gap(dp(GAP)).child(reporting).children(sensor)
    };
    v_flex()
        .id("pointer-panel")
        .gap(dp(GAP))
        .child(stages)
        .child(columns)
        .into_any_element()
}

fn render_dpi_stages(
    pointer: &PointerState,
    saved: &PointerSnapshot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> Stateful<Div> {
    let caps = &pointer.capabilities;
    let stages = &pointer.snapshot.dpi_stages;
    let active = pointer.snapshot.active_dpi_stage;
    let is_compact = stages.len() > 4;

    let tiles: Vec<AnyElement> = stages
        .iter()
        .map(|stage| {
            let is_pending = saved
                .dpi_stages
                .iter()
                .find(|s| s.index == stage.index)
                .is_some_and(|s| s.x != stage.x || s.y != stage.y);
            let tile = StageTile {
                stage: *stage,
                is_active: stage.index == active,
                is_pending,
                is_compact,
                busy,
            };
            render_stage_tile(tile, caps, cx).into_any_element()
        })
        .collect();

    let note = format!(
        "Click a stage to make it active. {} to {} DPI.",
        grouped(caps.dpi_min),
        grouped(caps.dpi_max)
    );
    panel("dpi-stages", cx)
        .child(
            panel_head("DPI stages").child(div().flex_1()).child(
                div()
                    .min_w_0()
                    .text_size(dp(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(note),
            ),
        )
        .child(
            h_flex()
                .items_stretch()
                .flex_wrap()
                .gap(dp(12.))
                .children(tiles),
        )
        .child(dpi_ruler(stages, active, caps, cx))
}

/// What one DPI stage tile shows.
#[derive(Clone, Copy)]
struct StageTile {
    stage: DpiStage,
    is_active: bool,
    is_pending: bool,
    is_compact: bool,
    busy: bool,
}

fn render_stage_tile(
    tile: StageTile,
    caps: &PointerCapabilities,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let StageTile {
        stage,
        is_active,
        is_pending,
        is_compact,
        busy,
    } = tile;
    let stage_ix = stage.index;
    let id = u64::from(stage_ix);
    let number = u16::from(stage_ix) + 1;
    let value = stage.x.value();
    let separate_y = stage.y.filter(|y| *y != stage.x).map(DpiValue::value);
    let quiet = ButtonCustomVariant::new(cx)
        .foreground(cx.theme().foreground)
        .hover(transparent_black())
        .active(transparent_black());
    let theme = cx.theme();
    let colors = tokens(cx);
    // The indicator LED color is device data, so it is shown as it is.
    let led = stage
        .indicator_color
        .map(|c| rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)));

    let name = h_flex()
        .w_full()
        .gap(dp(7.))
        .text_size(dp(12.5))
        .when_some(led, |this, led| {
            this.child(
                div()
                    .size(dp(9.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(led),
            )
        })
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(format!("Stage {number}")),
        )
        .when(is_pending, |this| {
            this.child(div().size(dp(7.)).rounded_full().bg(theme.warning))
        })
        .child(div().flex_1())
        .when(is_active, |this| {
            this.child(
                div()
                    .text_size(dp(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.primary)
                    .child("Active"),
            )
        });
    let reading = h_flex()
        .items_baseline()
        .gap(dp(4.))
        .child(
            div()
                .font_family(DISPLAY_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .font_features(tabular())
                .text_size(dp(if is_compact { 28. } else { 34. }))
                .line_height(relative(1.))
                .child(value.to_string()),
        )
        .when(!is_compact || separate_y.is_some(), |this| {
            this.child(
                div()
                    .font_family(DISPLAY_FONT)
                    .text_size(dp(13.))
                    .text_color(theme.muted_foreground)
                    .child(separate_y.map_or_else(|| "DPI".to_owned(), |y| format!("× {y}"))),
            )
        });
    let step = |button: Button, icon: IconName| {
        button
            .ghost()
            .xsmall()
            .size(dp(24.))
            .bg(colors.panel)
            .text_color(theme.muted_foreground)
            .icon(icon)
    };
    let lower = step(Button::new(("dpi-dec", id)), IconName::Minus);
    let raise = step(Button::new(("dpi-inc", id)), IconName::Plus);
    let radius = theme.radius_lg;
    let (border, fill) = if is_active {
        (theme.primary, colors.accent_soft)
    } else {
        (transparent_black(), colors.raised)
    };

    v_flex()
        .id(("dpi-stage", id))
        .flex_1()
        .min_w(dp(150.))
        .gap(dp(8.))
        .py(dp(10.))
        .pl(dp(12.))
        .pr(dp(10.))
        .rounded(radius)
        .border_2()
        .border_color(border)
        .bg(fill)
        .when(!is_active, |this| this.hover(|this| this.bg(colors.hover)))
        .on_click(cx.listener(move |this, _, _, cx| {
            if !is_active && !busy {
                this.use_dpi_stage(stage_ix, cx);
            }
        }))
        .child(
            Button::new(("dpi-active", id))
                .custom(quiet)
                .w_full()
                .h(dp(20.))
                .px_0()
                .disabled(busy)
                .accessibility_label(if is_active {
                    format!("Stage {number} is active")
                } else {
                    format!("Make stage {number} active")
                })
                .child(name)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if !is_active {
                        this.use_dpi_stage(stage_ix, cx);
                    }
                })),
        )
        .child(
            h_flex()
                .items_end()
                .gap(dp(4.))
                .child(reading)
                .child(div().flex_1())
                // The tile switches stages at once, so its steppers keep their clicks.
                .child(
                    lower
                        .accessibility_label(format!("Lower stage {number} DPI"))
                        .disabled(busy || value <= caps.dpi_min)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.step_dpi(stage_ix, -1, cx);
                        })),
                )
                .child(
                    raise
                        .accessibility_label(format!("Raise stage {number} DPI"))
                        .disabled(busy || value >= caps.dpi_max)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.step_dpi(stage_ix, 1, cx);
                        })),
                ),
        )
        .test_support()
}

/// Where each stage sits on a doubling scale from 100 DPI to the sensor's maximum.
fn dpi_ruler(stages: &[DpiStage], active: u8, caps: &PointerCapabilities, cx: &App) -> Div {
    let theme = cx.theme();
    let colors = tokens(cx);
    let values = stages.iter().map(|s| s.x.value());
    let low = values
        .clone()
        .chain([caps.dpi_min.max(100)])
        .min()
        .unwrap_or(100)
        .max(1);
    let high = values
        .chain([caps.dpi_max])
        .max()
        .unwrap_or(low)
        .max(low + 1);
    let log = |dpi: u32| f32::from(u16::try_from(dpi).unwrap_or(u16::MAX)).ln();
    let at = move |dpi: u32| ((log(dpi) - log(low)) / (log(high) - log(low))).clamp(0.0, 1.0);

    let marks: Vec<u32> = std::iter::successors(Some(100_u32), |mark| mark.checked_mul(2))
        .take_while(|&mark| mark <= high)
        .filter(|&mark| mark >= low)
        .collect();
    let ticks = marks.iter().map(|&mark| {
        div()
            .absolute()
            .top(dp(18.))
            .left(relative(at(mark)))
            .w_px()
            .h(dp(10.))
            .bg(colors.tick)
    });
    let labels = marks.iter().map(|&mark| {
        div()
            .absolute()
            .top(dp(32.))
            .left(relative(at(mark)))
            .ml(dp(-20.))
            .w(dp(40.))
            .text_center()
            .font_family(KEY_FONT)
            .text_size(dp(10.5))
            .line_height(relative(1.))
            .text_color(colors.faint)
            .child(short_dpi(mark))
    });
    // The active stage is drawn last so it stays on top of a stage sharing its place.
    let mut ordered: Vec<&DpiStage> = stages.iter().collect();
    ordered.sort_by_key(|stage| stage.index == active);
    let pins = ordered.into_iter().map(|stage| {
        let (fill, ink) = if stage.index == active {
            (theme.primary, theme.primary_foreground)
        } else {
            (colors.line_strong, theme.foreground)
        };
        v_flex()
            .absolute()
            .top_0()
            .left(relative(at(stage.x.value())))
            .ml(dp(-12.))
            .w(dp(24.))
            .items_center()
            .child(
                h_flex()
                    .justify_center()
                    .min_w(dp(18.))
                    .h(dp(16.))
                    .px(dp(5.))
                    .rounded(dp(4.))
                    .bg(fill)
                    .font_family(KEY_FONT)
                    .text_size(dp(10.))
                    .text_color(ink)
                    .child((u16::from(stage.index) + 1).to_string()),
            )
            .child(div().w(dp(1.5)).h(dp(7.)).bg(fill))
    });

    div().mt(dp(18.)).mx(dp(10.)).child(
        div()
            .relative()
            .h(dp(46.))
            .child(
                div()
                    .absolute()
                    .top(dp(22.))
                    .left_0()
                    .right_0()
                    .h(dp(2.))
                    .rounded_full()
                    .bg(colors.raised),
            )
            .children(ticks)
            .children(labels)
            .children(pins),
    )
}

/// `800` stays as is; thousands shorten to `1.6K`.
fn short_dpi(dpi: u32) -> String {
    match (dpi / 1000, (dpi % 1000) / 100) {
        (0, _) => dpi.to_string(),
        (thousands, 0) => format!("{thousands}K"),
        (thousands, tenths) => format!("{thousands}.{tenths}K"),
    }
}

/// `26000` as `26,000`.
fn grouped(value: u32) -> String {
    if value < 1000 {
        value.to_string()
    } else {
        format!("{},{:03}", grouped(value / 1000), value % 1000)
    }
}

fn render_reporting(
    pointer: &PointerState,
    saved: &PointerSnapshot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> Stateful<Div> {
    let mut group = panel("pointer-reporting", cx).child(panel_head("Reporting"));

    let current = pointer.snapshot.polling_rate;
    let mut rates = pointer.capabilities.supported_polling_rates.clone();
    rates.sort_by_key(|rate| rate.as_hz());
    let mut choices = segmented(cx).id("polling-rates");
    for rate in rates {
        let label = if rate.as_hz() >= 1000 {
            format!("{}K", rate.as_hz() / 1000)
        } else {
            rate.as_hz().to_string()
        };
        choices = choices.child(
            segment(
                Button::new(("polling-rate", u64::from(rate.as_hz()))),
                label,
                rate == current,
                cx,
            )
            .px(dp(9.))
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                if rate != current {
                    this.stage_pointer(PointerChange::PollingRate(rate), cx);
                }
            })),
        );
    }
    group = group.child(device_setting_row(
        "Polling rate",
        Some("Reports per second, in Hz.".into()),
        choices,
        true,
        saved.polling_rate != current,
        cx,
    ));

    if !pointer.capabilities.supported_lods.is_empty() {
        let current = pointer.snapshot.lod;
        let mut choices = segmented(cx).id("lift-off");
        for &lod in &pointer.capabilities.supported_lods {
            let key = match lod {
                LiftOffDistance::Low => 1,
                LiftOffDistance::Medium => 3,
                LiftOffDistance::High => 2,
                LiftOffDistance::TenthsMm(tenths) => 100 + u64::from(tenths),
            };
            choices = choices.child(
                segment(
                    Button::new(("lod", key)),
                    lod.to_string(),
                    Some(lod) == current,
                    cx,
                )
                .px(dp(9.))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if Some(lod) != current {
                        this.stage_pointer(PointerChange::LiftOffDistance(lod), cx);
                    }
                })),
            );
        }
        group = group.child(device_setting_row(
            "Lift-off distance",
            Some("How high you can lift before tracking stops.".into()),
            choices,
            false,
            saved.lod != current,
            cx,
        ));
    }

    if let Some(((min, max), current)) = pointer
        .capabilities
        .debounce_range_ms
        .zip(pointer.snapshot.debounce_ms)
    {
        let value = current.value();
        let set = move |next: u8| {
            move |this: &mut AgnoPeriphApp,
                  _: &ClickEvent,
                  _: &mut Window,
                  cx: &mut Context<AgnoPeriphApp>| {
                this.stage_pointer(PointerChange::Debounce(DebounceMs::new(next)), cx);
            }
        };
        let shorter = Button::new("debounce-dec")
            .accessibility_label("Shorter debounce")
            .disabled(busy || value <= min)
            .on_click(cx.listener(set(value.saturating_sub(1))));
        let longer = Button::new("debounce-inc")
            .accessibility_label("Longer debounce")
            .disabled(busy || value >= max)
            .on_click(cx.listener(set(value.saturating_add(1))));
        let control = stepper(shorter, value.to_string(), "ms", longer, cx);
        group = group.child(device_setting_row(
            "Click debounce",
            Some("Raise it if a button double-clicks.".into()),
            control,
            false,
            saved.debounce_ms.is_some_and(|s| s != current),
            cx,
        ));
    }
    group
}

/// On/off sensor features that share one switch row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SensorSwitch {
    MotionSync,
    AngleSnapping,
    RippleControl,
}

impl SensorSwitch {
    const ALL: [Self; 3] = [Self::MotionSync, Self::AngleSnapping, Self::RippleControl];

    const fn label(self) -> &'static str {
        match self {
            Self::MotionSync => "Motion sync",
            Self::AngleSnapping => "Angle snapping",
            Self::RippleControl => "Ripple control",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::MotionSync => "Times sensor reads to each report. Smoother, with a little delay.",
            Self::AngleSnapping => "Straightens movement that is almost horizontal or vertical.",
            Self::RippleControl => "Smooths jitter at high DPI, with a little delay.",
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

    /// The setting's value, when this mouse has it.
    fn value(self, caps: &PointerCapabilities, snapshot: &PointerSnapshot) -> Option<bool> {
        let (is_supported, value) = match self {
            Self::MotionSync => (caps.supports_motion_sync, snapshot.motion_sync),
            Self::AngleSnapping => (caps.supports_angle_snapping, snapshot.angle_snapping),
            Self::RippleControl => (caps.supports_ripple_control, snapshot.ripple_control),
        };
        value.filter(|_| is_supported)
    }
}

fn render_sensor_options(
    pointer: &PointerState,
    saved: &PointerSnapshot,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> Option<Stateful<Div>> {
    let supported: Vec<(SensorSwitch, bool)> = SensorSwitch::ALL
        .into_iter()
        .filter_map(|kind| {
            kind.value(&pointer.capabilities, &pointer.snapshot)
                .map(|enabled| (kind, enabled))
        })
        .collect();
    if supported.is_empty() {
        return None;
    }
    let mut group = panel("pointer-sensor", cx).child(panel_head("Sensor"));
    for (ix, (kind, enabled)) in supported.into_iter().enumerate() {
        let switch = Switch::new(kind.id())
            .checked(enabled)
            .disabled(busy)
            .accessibility_label(kind.label())
            .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                this.stage_pointer(kind.change(*checked), cx);
            }));
        let is_pending = kind
            .value(&pointer.capabilities, saved)
            .is_some_and(|stored| stored != enabled);
        group = group.child(device_setting_row(
            kind.label(),
            Some(kind.detail().into()),
            switch,
            ix == 0,
            is_pending,
            cx,
        ));
    }
    Some(group)
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
