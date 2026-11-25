//! Connected peripheral list.

use agnoperiph_core::{BatteryStatus, ChargingState, DeviceId};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    scroll::ScrollableElement,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::DeviceSnapshot;

/// Stable element id for a device row, derived from its domain [`DeviceId`].
pub fn device_element_id(device: &DeviceId) -> ElementId {
    ElementId::Name(format!("device-{device}").into())
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let selected = app.selected_device_id().cloned();
    let rows: Vec<AnyElement> = app
        .devices()
        .iter()
        .map(|device| {
            let is_selected = selected.as_ref() == Some(device.id());
            render_row(device, is_selected, cx).into_any_element()
        })
        .collect();
    let theme = cx.theme();

    v_flex()
        .id("sidebar")
        .w_64()
        .flex_shrink_0()
        .min_h_0()
        .border_r_1()
        .border_color(theme.sidebar_border)
        .bg(theme.sidebar)
        .text_color(theme.sidebar_foreground)
        .child(
            div()
                .px_4()
                .pt_4()
                .pb_2()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child("Devices"),
        )
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
}

fn render_row(
    device: &DeviceSnapshot,
    is_selected: bool,
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
    let firmware = info
        .firmware_version
        .clone()
        .unwrap_or_else(|| "Firmware unknown".to_owned());
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
                        .child(div().text_xs().text_color(muted).truncate().child(firmware)),
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
