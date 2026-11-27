//! Persistent navigation: connected peripherals, Game profiles and Settings.

use agnoperiph_core::{BatteryStatus, ConnectionKind, DeviceId};

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable,
    button::{Button, ButtonVariants},
    h_flex,
    scroll::ScrollableElement,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, Route};
use crate::backend::DeviceSnapshot;
use crate::design::{dp, tokens};
use crate::widgets::{battery_glyph, battery_label, connection_icon};

/// Below this charge the sidebar marks a battery as low, matching the first alert.
const LOW_BATTERY_PERCENT: u8 = 20;

/// Stable element id for a device row, derived from its domain [`DeviceId`].
pub fn device_element_id(device: &DeviceId) -> ElementId {
    ElementId::Name(format!("device-{device}").into())
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
            let unsaved = app.unsaved_count(device);
            render_row(device, is_selected, unsaved, cx).into_any_element()
        })
        .collect();
    let games_status: SharedString =
        match (app.active_game_name(), app.config().auto_switch_enabled) {
            (Some(name), true) => format!("Playing {name}").into(),
            (_, false) => "Auto switch off".into(),
            (None, true) => match app.config().game_profiles.len() {
                0 => "None yet".into(),
                1 => "1 game".into(),
                n => format!("{n} games").into(),
            },
        };
    let games = nav_row(
        "nav-game-profiles",
        Icon::new(LucideIcon::Gamepad2),
        "Game profiles",
        Some(games_status),
        route == Route::GameProfiles,
        cx,
    )
    .on_click(cx.listener(|this, _, _, cx| this.open_game_profiles(cx)));
    let settings = nav_row(
        "nav-settings",
        Icon::new(IconName::Settings),
        "Settings",
        None,
        route == Route::Settings,
        cx,
    )
    .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx)));
    let theme = cx.theme();

    v_flex()
        .id("sidebar")
        .w(dp(264.))
        .flex_shrink_0()
        .min_h_0()
        .pt(dp(14.))
        .px(dp(10.))
        .pb(dp(10.))
        .border_r_1()
        .border_color(theme.sidebar_border)
        .bg(theme.sidebar)
        .text_color(theme.sidebar_foreground)
        .child(
            div()
                .px(dp(10.))
                .pb(dp(8.))
                .text_size(dp(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child("Devices"),
        )
        .child(
            v_flex()
                .id("sidebar-devices")
                .flex_1()
                .min_h_0()
                .gap(dp(2.))
                .children(rows)
                .overflow_y_scrollbar(),
        )
        .child(
            v_flex()
                .flex_shrink_0()
                .pt(dp(8.))
                .gap(dp(2.))
                .border_t_1()
                .border_color(theme.sidebar_border)
                .child(games)
                .child(settings),
        )
}

fn nav_row(
    id: &'static str,
    icon: Icon,
    label: &'static str,
    meta: Option<SharedString>,
    is_selected: bool,
    cx: &App,
) -> Button {
    let theme = cx.theme();
    let faint = tokens(cx).faint;
    Button::new(id)
        .ghost()
        .selected(is_selected)
        .accessibility_label(label)
        .w_full()
        .h(dp(34.))
        .px(dp(10.))
        .map(|this| {
            if is_selected {
                this.bg(theme.muted).text_color(theme.foreground)
            } else {
                this.text_color(theme.muted_foreground)
            }
        })
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap(dp(10.))
                .text_size(dp(13.5))
                .child(icon.size(dp(16.)))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(if is_selected {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .child(label),
                )
                .child(div().flex_1())
                .when_some(meta, |this, meta| {
                    this.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(dp(12.))
                            .text_color(faint)
                            .child(meta),
                    )
                }),
        )
}

fn render_row(
    device: &DeviceSnapshot,
    is_selected: bool,
    unsaved: usize,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let panel = tokens(cx).panel;
    let theme = cx.theme();
    let info = &device.info;
    let muted = theme.muted_foreground;
    let caps = device.capabilities;
    let icon = if caps.has_hall_effect() || caps.has_keymap() || caps.has_keyboard_config() {
        LucideIcon::Keyboard
    } else {
        LucideIcon::Mouse
    };
    let battery = caps.has_battery().then_some(device.battery).flatten();
    let id = device.id().clone();

    Button::new(device_element_id(&id))
        .ghost()
        .selected(is_selected)
        .accessibility_label(info.model_name.clone())
        .w_full()
        .h_auto()
        .py(dp(8.))
        .px(dp(10.))
        .when(is_selected, |this| this.bg(theme.muted))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap(dp(10.))
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .size(dp(32.))
                        .justify_center()
                        .rounded(theme.radius)
                        .bg(panel)
                        .text_color(if is_selected { theme.primary } else { muted })
                        .child(Icon::new(icon).size(dp(18.))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(dp(1.))
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap(dp(6.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(dp(13.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(info.model_name.clone()),
                                )
                                .when(unsaved > 0, |this| {
                                    this.child(unsaved_dot(&id, unsaved, cx))
                                }),
                        )
                        .child(
                            h_flex()
                                .gap(dp(5.))
                                .text_size(dp(12.))
                                .text_color(muted)
                                .child(connection_icon(info.connection).size(dp(13.)))
                                .child(short_connection(info.connection))
                                .when_some(battery, |this, battery| {
                                    this.child(battery_note(battery, cx))
                                }),
                        ),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_device(id.clone(), cx);
        }))
}

fn unsaved_dot(device: &DeviceId, count: usize, cx: &App) -> impl IntoElement + use<> {
    let note: SharedString = match count {
        1 => "1 unsaved change".into(),
        n => format!("{n} unsaved changes").into(),
    };
    div()
        .id(ElementId::Name(format!("device-unsaved-{device}").into()))
        .flex_shrink_0()
        .size(dp(7.))
        .rounded_full()
        .bg(cx.theme().warning)
        .tooltip(move |window, cx| Tooltip::new(note.clone()).build(window, cx))
}

fn short_connection(connection: ConnectionKind) -> &'static str {
    match connection {
        ConnectionKind::WiredUsb => "Wired",
        ConnectionKind::WirelessDongle24Ghz => "2.4 GHz",
        ConnectionKind::Bluetooth => "Bluetooth",
    }
}

fn battery_note(battery: BatteryStatus, cx: &App) -> Div {
    let theme = cx.theme();
    let is_low = battery
        .percentage
        .is_some_and(|pct| pct < LOW_BATTERY_PERCENT);
    let color = if is_low {
        theme.warning
    } else {
        theme.muted_foreground
    };
    h_flex()
        .ml(dp(8.))
        .gap(dp(4.))
        .text_color(color)
        .when_some(battery.percentage, |this, pct| {
            this.child(battery_glyph(pct, color))
        })
        .child(battery_label(battery))
}
