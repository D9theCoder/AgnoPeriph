//! Persistent navigation: connected peripherals, the Game Profiles manager and app preferences.

use agnoperiph_core::{BatteryStatus, ChargingState, DeviceId};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    scroll::ScrollableElement,
    switch::Switch,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, Route};
use crate::backend::DeviceSnapshot;

/// Stable element id for a device row, derived from its domain [`DeviceId`].
pub fn device_element_id(device: &DeviceId) -> ElementId {
    ElementId::Name(format!("device-{device}").into())
}

fn heading(text: &'static str, muted: Hsla) -> Div {
    div()
        .px_4()
        .pt_4()
        .pb_2()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(muted)
        .child(text)
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let route = app.route();
    let selected = app.selected_device_id().cloned();
    let rows: Vec<AnyElement> = app
        .devices()
        .iter()
        .map(|device| {
            let is_selected = route == Route::Device && selected.as_ref() == Some(device.id());
            let preset = app.current_slot(device.id()).map(|slot| {
                app.config()
                    .preset_name(device.id(), agnoperiph_core::ProfileSlot::new(slot))
            });
            render_row(device, is_selected, preset, cx).into_any_element()
        })
        .collect();
    let game_count = app.config().game_profiles.len();
    let active_game = app.active_game_name().map(str::to_owned);
    let close_to_tray = app.config().close_to_tray;
    let start_with_windows = app.config().start_with_windows;
    let theme = cx.theme();
    let muted = theme.muted_foreground;

    let games_status: SharedString = match (&active_game, app.config().auto_switch_enabled) {
        (Some(name), true) => format!("Playing {name}").into(),
        (_, false) => "Auto switch off".into(),
        (None, true) => match game_count {
            0 => "No games yet".into(),
            1 => "1 game".into(),
            n => format!("{n} games").into(),
        },
    };

    v_flex()
        .id("sidebar")
        .w_64()
        .flex_shrink_0()
        .min_h_0()
        .border_r_1()
        .border_color(theme.sidebar_border)
        .bg(theme.sidebar)
        .text_color(theme.sidebar_foreground)
        .child(heading("Devices", muted))
        .child(
            v_flex()
                .id("sidebar-devices")
                .flex_1()
                .min_h_0()
                .px_2()
                .pb_2()
                .gap_1()
                .children(rows)
                .overflow_y_scrollbar(),
        )
        .child(
            v_flex()
                .flex_shrink_0()
                .border_t_1()
                .border_color(theme.sidebar_border)
                .child(heading("Automation", muted))
                .child(
                    div().px_2().child(
                        Button::new("nav-game-profiles")
                            .ghost()
                            .selected(route == Route::GameProfiles)
                            .w_full()
                            .h_auto()
                            .py_2()
                            .px_2()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_3()
                                    .child(Icon::new(IconName::LayoutDashboard).small().text_color(muted))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_0p5()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("Game Profiles"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .truncate()
                                                    .child(games_status),
                                            ),
                                    )
                                    .when(active_game.is_some(), |this| {
                                        this.child(
                                            div()
                                                .size_2()
                                                .rounded_full()
                                                .bg(cx.theme().success),
                                        )
                                    }),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.open_game_profiles(cx))),
                    ),
                )
                .child(
                    v_flex()
                        .px_4()
                        .pt_4()
                        .pb_4()
                        .gap_3()
                        .child(
                            Switch::new("pref-close-to-tray")
                                .small()
                                .label("Close to tray")
                                .checked(close_to_tray)
                                .tooltip("Keep running in the notification area when the window closes, so Game Profiles keep switching")
                                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                    this.set_close_to_tray(*checked, cx);
                                })),
                        )
                        .child(
                            Switch::new("pref-start-with-windows")
                                .small()
                                .label("Start with Windows")
                                .checked(start_with_windows)
                                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                    this.set_start_with_windows(*checked, cx);
                                })),
                        ),
                ),
        )
}

fn render_row(
    device: &DeviceSnapshot,
    is_selected: bool,
    preset: Option<String>,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let info = &device.info;
    let muted = theme.muted_foreground;
    let icon = if device.capabilities.has_hall_effect() {
        IconName::Frame
    } else {
        IconName::Cpu
    };
    let detail = preset.unwrap_or_else(|| {
        info.firmware_version
            .clone()
            .unwrap_or_else(|| "Firmware unknown".to_owned())
    });
    let battery = device
        .capabilities
        .has_battery()
        .then_some(device.battery)
        .flatten();
    let id = device.id().clone();

    Button::new(device_element_id(&id))
        .ghost()
        .selected(is_selected)
        .accessibility_label(info.model_name.clone())
        .w_full()
        .h_auto()
        .py_2()
        .px_2()
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .items_start()
                .child(
                    div()
                        .pt_0p5()
                        .child(Icon::new(icon).small().text_color(muted)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(info.model_name.clone()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Tag::secondary().xsmall().child(info.connection.to_string()))
                                .when_some(battery, |this, battery| {
                                    this.child(battery_tag(battery))
                                }),
                        )
                        .child(div().text_xs().text_color(muted).truncate().child(detail)),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.select_device(id.clone(), cx)))
}

fn battery_tag(battery: BatteryStatus) -> Tag {
    let label = match (battery.percentage, battery.state) {
        (Some(pct), ChargingState::Charging) => format!("{pct}% charging"),
        (Some(pct), _) => format!("{pct}%"),
        (None, state) => state.to_string(),
    };
    let tag = if battery.percentage.is_some_and(|pct| pct <= 15) {
        Tag::warning()
    } else {
        Tag::secondary()
    };
    tag.xsmall().child(label)
}
