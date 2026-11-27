//! A mouse seen from above, lit in the colors of the lighting zones it reports.

use agnoperiph_core::RgbZoneId;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::design::{dp, legend_on, tokens};

/// Layout units per drawing unit. The layers' view box starts at (-10, -10).
const SCALE: f32 = 216.0 / 220.0;
const SIZE: (f32, f32) = (216.0, 306.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MousePart {
    /// The strip around the shell.
    Body,
    Wheel,
    Logo,
}

impl MousePart {
    /// The part a zone lights, by the names drivers give their zones. `None` for zones
    /// the drawing has no place for.
    pub(super) fn of_zone(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "mouse" | "primary" => Some(Self::Body),
            "scroll wheel" => Some(Self::Wheel),
            "logo" => Some(Self::Logo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Light {
    Off,
    Solid(Hsla),
    /// An effect that moves through colors, drawn as one still frame.
    Cycling,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ZoneArt {
    pub(super) part: MousePart,
    pub(super) zone: RgbZoneId,
    pub(super) light: Light,
    pub(super) is_selected: bool,
}

/// Where a point of the artwork lands in the drawing.
fn at(unit: f32) -> Rems {
    dp((unit + 10.0) * SCALE)
}

fn layer(path: &'static str, color: Hsla) -> Svg {
    svg()
        .path(path)
        .absolute()
        .inset_0()
        .size_full()
        .text_color(color)
}

fn full_color_layer(path: &'static str) -> Img {
    img(path).absolute().inset_0().size_full()
}

/// Draws the mouse. Clicking a lit part picks its zone; the zone switch above it does the
/// same from the keyboard.
pub(super) fn figure(zones: &[ZoneArt], cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let find = |part| zones.iter().find(|zone| zone.part == part).copied();
    let (body, wheel, logo) = (
        find(MousePart::Body),
        find(MousePart::Wheel),
        find(MousePart::Logo),
    );
    let colors = tokens(cx);
    let primary = cx.theme().primary;
    let body_light = body.map_or(Light::Off, |zone| zone.light);
    let outline = if body.is_some_and(|zone| zone.is_selected) {
        primary
    } else {
        colors.line_strong
    };

    let mut art = div()
        .id("mouse-art")
        .relative()
        .flex_shrink_0()
        .w(dp(SIZE.0))
        .h(dp(SIZE.1));
    art = match body_light {
        Light::Off => art,
        Light::Solid(color) => art.child(layer("icons/mouse/glow.svg", color.opacity(0.75))),
        Light::Cycling => art.child(full_color_layer("icons/mouse/glow-rainbow.svg").opacity(0.75)),
    };
    art = art
        .child(layer("icons/mouse/body.svg", colors.key_face))
        .child(layer("icons/mouse/rim.svg", outline));
    art = match body_light {
        Light::Off => art,
        Light::Solid(color) => art.child(layer("icons/mouse/rim.svg", color)),
        Light::Cycling => art.child(full_color_layer("icons/mouse/rim-rainbow.svg")),
    };
    art = art.child(layer("icons/mouse/seam.svg", colors.line_strong));
    if let Some(zone) = body {
        let id = zone.zone;
        art = art
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.select_zone(id, cx)));
    }

    let wheel_rect = (91.0, 36.0, 18.0, 42.0);
    let logo_rect = (83.0, 195.0, 34.0, 34.0);
    let logo_ink = match logo.map(|zone| zone.light) {
        Some(Light::Solid(color)) => legend_on(color),
        _ => cx.theme().muted_foreground,
    };
    art.child(part_spot(wheel, wheel_rect, "mouse-wheel", cx).test_support())
        .when(logo.is_some(), |this| {
            this.child(
                part_spot(logo, logo_rect, "mouse-logo", cx)
                    .child(
                        Icon::new(IconName::ChevronUp)
                            .size(dp(18.))
                            .text_color(logo_ink),
                    )
                    .test_support(),
            )
        })
        .test_support()
        .into_any_element()
}

/// The scroll wheel or the logo; `rect` is `(x, y, width, height)` in drawing units.
fn part_spot(
    zone: Option<ZoneArt>,
    rect: (f32, f32, f32, f32),
    id: &'static str,
    cx: &mut Context<AgnoPeriphApp>,
) -> Stateful<Div> {
    let (x, y, width, height) = rect;
    let colors = tokens(cx);
    let primary = cx.theme().primary;
    let light = zone.map_or(Light::Off, |zone| zone.light);
    let is_selected = zone.is_some_and(|zone| zone.is_selected);
    let spot = div()
        .flex()
        .items_center()
        .justify_center()
        .id(id)
        .absolute()
        .left(at(x))
        .top(at(y))
        .w(dp(width * SCALE))
        .h(dp(height * SCALE))
        .rounded_full()
        .map(|this| match light {
            Light::Off => this.bg(colors.raised),
            Light::Solid(color) => this.bg(color),
            Light::Cycling => this.bg(linear_gradient(
                135.0,
                linear_color_stop(hsla(0.0, 0.85, 0.6, 1.0), 0.0),
                linear_color_stop(hsla(0.55, 0.85, 0.6, 1.0), 1.0),
            )),
        })
        .map(|this| {
            if is_selected {
                this.border_2().border_color(primary)
            } else {
                this.border_1().border_color(colors.line_strong)
            }
        });
    match zone {
        Some(zone) => {
            let id = zone.zone;
            spot.cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    // The body behind would otherwise take the click for its own zone.
                    cx.stop_propagation();
                    this.select_zone(id, cx);
                }))
        }
        None => spot,
    }
}
