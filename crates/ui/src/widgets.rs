//! Shared pieces of the app's look: panels, segmented controls, steppers, chips and
//! setting rows. Interactive ones restyle gpui-kit's [`Button`], so focus, keyboard
//! activation and disabled behaviour stay the component's own.

use agnoperiph_core::{BatteryStatus, ConnectionKind};
use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::{
    ActiveTheme as _, Disableable, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::design::{DISPLAY_FONT, KEY_FONT, dp, tabular, tokens};

/// A column that keeps its natural height, so a scrolling parent never squeezes it.
pub(crate) fn block(child: impl IntoElement) -> Div {
    v_flex().min_w_0().flex_shrink_0().child(child)
}

/// A borderless card on the panel surface.
pub(crate) fn panel(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    v_flex()
        .id(id)
        .min_w_0()
        .pt(dp(14.))
        .px(dp(16.))
        .pb(dp(16.))
        .rounded(cx.theme().radius_lg)
        .bg(tokens(cx).panel)
}

pub(crate) fn panel_title(title: &str) -> Div {
    div()
        .flex_shrink_0()
        .font_family(DISPLAY_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(dp(16.))
        .whitespace_nowrap()
        .child(SharedString::from(title.to_owned()))
}

/// The title line of a panel; add notes and actions as further children.
pub(crate) fn panel_head(title: &str) -> Div {
    head_row().child(panel_title(title))
}

/// [`panel_head`] without its title, for a title that needs its own styling.
pub(crate) fn head_row() -> Div {
    h_flex().gap_2().min_h(dp(26.)).mb(dp(10.))
}

/// Heading of a top-level view or a device.
pub(crate) fn page_title(title: impl Into<SharedString>) -> Div {
    div()
        .font_family(DISPLAY_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(dp(26.))
        .line_height(relative(1.15))
        .whitespace_nowrap()
        .child(title.into())
}

/// Container of mutually exclusive [`segment`]s.
pub(crate) fn segmented(cx: &App) -> Div {
    h_flex()
        .flex_shrink_0()
        .p(dp(2.))
        .gap(dp(2.))
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
}

/// Styles `button` as one choice of a [`segmented`] control.
pub(crate) fn segment(
    button: Button,
    label: impl Into<SharedString>,
    is_selected: bool,
    cx: &App,
) -> Button {
    let theme = cx.theme();
    let label = label.into();
    button
        .ghost()
        .selected(is_selected)
        .accessibility_label(label.clone())
        .h(dp(26.))
        .px(dp(12.))
        .map(|this| {
            if is_selected {
                this.bg(theme.primary).text_color(theme.primary_foreground)
            } else {
                this.text_color(theme.muted_foreground)
            }
        })
        .child(
            div()
                .text_size(dp(13.))
                .font_weight(if is_selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .whitespace_nowrap()
                .child(label),
        )
}

/// A keycap naming one key, `size` high.
pub(crate) fn keycap(label: impl Into<SharedString>, size: f32, text_size: f32, cx: &App) -> Div {
    let colors = tokens(cx);
    let skirt = (size * 0.11).round();
    div()
        .flex_shrink_0()
        .rounded(dp(size * 0.2))
        .border_1()
        .border_color(cx.theme().border)
        .bg(colors.key_skirt)
        .pb(dp(skirt))
        .child(
            h_flex()
                .justify_center()
                .min_w(dp(size))
                .h(dp(size - skirt))
                .px(dp(size * 0.2))
                .rounded(dp(size * 0.2))
                .bg(colors.key_face)
                .font_family(KEY_FONT)
                .text_size(dp(text_size))
                .line_height(relative(1.))
                .text_color(colors.key_legend)
                .whitespace_nowrap()
                .child(label.into()),
        )
}

pub(crate) fn mini_keycap(label: impl Into<SharedString>, cx: &App) -> Div {
    keycap(label, 28., 13., cx)
}

/// A "− value unit +" control. `decrease` and `increase` bring their own ids and handlers.
pub(crate) fn stepper(
    decrease: Button,
    value: impl Into<SharedString>,
    unit: &'static str,
    increase: Button,
    cx: &App,
) -> Div {
    stepper_sized(decrease, value, unit, increase, 60., cx)
}

/// A [`stepper`] whose value takes at least `value_width`, for tight rows.
pub(crate) fn stepper_sized(
    decrease: Button,
    value: impl Into<SharedString>,
    unit: &'static str,
    increase: Button,
    value_width: f32,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_shrink_0()
        .h(dp(28.))
        .p(dp(2.))
        .gap(dp(2.))
        .rounded(theme.radius)
        .bg(theme.muted)
        .child(step_button(decrease, IconName::Minus, cx))
        .child(
            h_flex()
                .min_w(dp(value_width))
                .justify_center()
                .items_baseline()
                .gap(dp(2.))
                .child(
                    div()
                        .font_family(DISPLAY_FONT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_features(tabular())
                        .text_size(dp(13.))
                        .child(value.into()),
                )
                .when(!unit.is_empty(), |this| {
                    this.child(
                        div()
                            .text_size(dp(11.5))
                            .text_color(theme.muted_foreground)
                            .child(unit),
                    )
                }),
        )
        .child(step_button(increase, IconName::Plus, cx))
}

fn step_button(button: Button, icon: IconName, cx: &App) -> Button {
    button
        .ghost()
        .xsmall()
        .size(dp(24.))
        .text_color(cx.theme().muted_foreground)
        .icon(icon)
}

/// A read-only fact next to a device name.
pub(crate) fn chip(cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_shrink_0()
        .gap(dp(5.))
        .h(dp(22.))
        .px(dp(9.))
        .rounded_full()
        .bg(theme.muted)
        .text_color(theme.muted_foreground)
        .text_size(dp(12.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
}

/// A named setting with its control on the right, separated from the row above.
pub(crate) fn setting_row(
    name: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    is_first: bool,
    cx: &App,
) -> Div {
    device_setting_row(name, description, control, is_first, false, cx)
}

/// A [`setting_row`] for a setting stored on a device, dotted while it isn't applied.
pub(crate) fn device_setting_row(
    name: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    is_first: bool,
    is_pending: bool,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    h_flex()
        .gap_4()
        .py(dp(12.))
        .map(|this| {
            if is_first {
                this.pt(dp(2.))
            } else {
                this.border_t_1().border_color(theme.border)
            }
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(dp(1.))
                .child(
                    h_flex()
                        .gap(dp(7.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(name.into()))
                        .when(is_pending, |this| {
                            this.child(div().size(dp(7.)).rounded_full().bg(theme.warning))
                        }),
                )
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .text_size(dp(12.5))
                            .text_color(theme.muted_foreground)
                            .child(description),
                    )
                }),
        )
        .child(control)
}

/// Centred placeholder for a panel that has nothing to show yet.
pub(crate) fn idle(icon: impl Into<Icon>, message: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .items_center()
        .justify_center()
        .gap(dp(10.))
        .min_h(dp(178.))
        .px(dp(12.))
        .text_center()
        .text_color(theme.muted_foreground)
        .child(icon.into().size(dp(26.)))
        .child(div().max_w(dp(250.)).child(message.into()))
}

pub(crate) fn connection_icon(connection: ConnectionKind) -> Icon {
    match connection {
        ConnectionKind::WiredUsb => Icon::new(LucideIcon::Plug),
        ConnectionKind::WirelessDongle24Ghz => Icon::new(LucideIcon::Wifi),
        ConnectionKind::Bluetooth => Icon::new(LucideIcon::Bluetooth),
    }
}

/// A battery drawn to its charge level, so the level reads without the number.
pub(crate) fn battery_glyph(percentage: u8, color: Hsla) -> Div {
    let fill = f32::from(percentage.min(100)) / 100.0;
    h_flex()
        .flex_shrink_0()
        .gap(dp(1.))
        .child(
            div()
                .w(dp(15.))
                .h(dp(9.))
                .p(dp(1.))
                .rounded(dp(2.5))
                .border_1()
                .border_color(color)
                .child(div().h_full().w(relative(fill)).rounded(dp(1.)).bg(color)),
        )
        .child(div().w(dp(1.5)).h(dp(3.5)).rounded(dp(1.)).bg(color))
}

/// `"78%"`, or the charging state when the device reports no level.
pub(crate) fn battery_label(battery: BatteryStatus) -> SharedString {
    match (battery.percentage, battery.state) {
        (Some(pct), agnoperiph_core::ChargingState::Charging) => format!("{pct}% charging").into(),
        (Some(pct), _) => format!("{pct}%").into(),
        (None, state) => state.to_string().into(),
    }
}

/// A [`Switch`] with a check or a cross in the half of the track its thumb leaves free.
#[derive(IntoElement)]
pub(crate) struct MarkedSwitch {
    id: ElementId,
    switch: Switch,
    is_checked: bool,
    is_disabled: bool,
}

impl MarkedSwitch {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            switch: Switch::new(id.clone()),
            id,
            is_checked: false,
            is_disabled: false,
        }
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.is_checked = checked;
        self.switch = self.switch.checked(checked);
        self
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.switch = self.switch.label(label.into());
        self
    }

    pub(crate) fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.switch = self.switch.accessibility_label(label);
        self
    }

    pub(crate) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.switch = self.switch.tooltip(tooltip);
        self
    }

    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.switch = self.switch.on_change(handler);
        self
    }
}

impl Disableable for MarkedSwitch {
    fn disabled(mut self, disabled: bool) -> Self {
        self.is_disabled = disabled;
        self.switch = self.switch.disabled(disabled);
        self
    }
}

impl RenderOnce for MarkedSwitch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The kit draws its medium track in fixed pixels (36 × 20, a 16 px thumb that
        // travels 16 px), so the marks laid over it use pixels too.
        let travel = px(16.);
        // Same spring as the thumb, so a mark fades in as the thumb moves off its side.
        let position = gpui_kit::base::spring(
            (self.id, "mark"),
            if self.is_checked { travel } else { px(0.) },
            cx.theme().motion_tokens().spring_move,
            window,
            cx,
        );
        let progress = (position / travel).clamp(0.0, 1.0);
        let strength = if self.is_disabled { 0.5 } else { 1.0 };
        let mark = |icon: IconName, color: Hsla, opacity: f32| {
            div()
                .absolute()
                .top(px(4.))
                .opacity(opacity * strength)
                .child(Icon::new(icon).with_size(px(12.)).text_color(color))
        };
        let theme = cx.theme();

        div().relative().child(self.switch).child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .w(px(36.))
                .h(px(20.))
                .child(mark(IconName::Check, theme.primary_foreground, progress).left(px(4.)))
                .child(mark(IconName::Close, theme.muted_foreground, 1.0 - progress).right(px(4.))),
        )
    }
}
