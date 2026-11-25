//! Lighting editor, rendered for devices whose capabilities include RGB lighting.

use agnoperiph_core::{
    Brightness, RgbColor, RgbDirection, RgbEffect, RgbEffectKind, RgbZoneInfo, RgbZoneState,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonGroup},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, field, readout, section};
use crate::backend::{DeviceSnapshot, RgbState, SettingChange};

/// Preset colors offered for effects that take a color.
const PRESETS: [(&str, RgbColor); 8] = [
    ("White", RgbColor::new(255, 255, 255)),
    ("Red", RgbColor::new(255, 0, 0)),
    ("Orange", RgbColor::new(255, 128, 0)),
    ("Yellow", RgbColor::new(255, 220, 0)),
    ("Green", RgbColor::new(0, 255, 0)),
    ("Cyan", RgbColor::new(0, 255, 255)),
    ("Blue", RgbColor::new(0, 0, 255)),
    ("Magenta", RgbColor::new(255, 0, 255)),
];

const DEFAULT_SPEED: u8 = 128;
const BRIGHTNESS_STEP: u8 = 10;

fn effect_label(kind: RgbEffectKind) -> &'static str {
    match kind {
        RgbEffectKind::Off => "Off",
        RgbEffectKind::Static => "Static",
        RgbEffectKind::Breathing => "Breathing",
        RgbEffectKind::SpectrumCycle => "Spectrum",
        RgbEffectKind::Wave => "Wave",
        RgbEffectKind::Reactive => "Reactive",
    }
}

fn effect_color(effect: RgbEffect) -> Option<RgbColor> {
    match effect {
        RgbEffect::Static { color }
        | RgbEffect::Breathing { color, .. }
        | RgbEffect::Reactive { color, .. } => Some(color),
        RgbEffect::Off | RgbEffect::SpectrumCycle { .. } | RgbEffect::Wave { .. } => None,
    }
}

fn effect_speed(effect: RgbEffect) -> u8 {
    match effect {
        RgbEffect::Breathing { speed, .. }
        | RgbEffect::SpectrumCycle { speed }
        | RgbEffect::Wave { speed, .. }
        | RgbEffect::Reactive { speed, .. } => speed,
        RgbEffect::Off | RgbEffect::Static { .. } => DEFAULT_SPEED,
    }
}

/// Builds an effect of `kind`, carrying over the current color and speed where they apply.
pub fn effect_of_kind(kind: RgbEffectKind, color: RgbColor, speed: u8) -> RgbEffect {
    match kind {
        RgbEffectKind::Off => RgbEffect::Off,
        RgbEffectKind::Static => RgbEffect::Static { color },
        RgbEffectKind::Breathing => RgbEffect::Breathing { color, speed },
        RgbEffectKind::SpectrumCycle => RgbEffect::SpectrumCycle { speed },
        RgbEffectKind::Wave => RgbEffect::Wave {
            speed,
            direction: RgbDirection::LeftToRight,
        },
        RgbEffectKind::Reactive => RgbEffect::Reactive { color, speed },
    }
}

fn with_color(effect: RgbEffect, color: RgbColor) -> RgbEffect {
    effect_of_kind(effect.kind(), color, effect_speed(effect))
}

/// Device LED color as a GPUI color. The color is the data itself, not a theme choice.
fn swatch_color(color: RgbColor) -> Rgba {
    rgb((u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b))
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let base = section(
        "rgb-panel",
        "Lighting",
        "Backlight effect, brightness and color.",
        cx.theme(),
    );
    let Some(rgb) = device.rgb.as_ref() else {
        return base.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("The lighting state couldn’t be read."),
        );
    };
    let zones: Vec<AnyElement> = rgb
        .snapshot
        .zones
        .iter()
        .map(|zone| render_zone(app, rgb, *zone, cx).into_any_element())
        .collect();
    base.children(zones)
}

fn render_zone(
    app: &AgnoPeriphApp,
    rgb_state: &RgbState,
    zone: RgbZoneState,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let busy = app.is_busy();
    let zone_id = zone.zone;
    let effect = zone.effect;
    let brightness = zone.brightness;
    let color = effect_color(effect);
    let zone_name = rgb_state
        .capabilities
        .zones
        .iter()
        .find(|info| info.id == zone_id)
        .map_or_else(
            || format!("Zone {}", zone_id.value()),
            |info: &RgbZoneInfo| info.name.clone(),
        );
    let prefix = format!("rgb-{}", zone_id.value());

    let apply = move |effect: RgbEffect, brightness: Brightness| SettingChange::Rgb {
        zone: zone_id,
        effect,
        brightness,
    };

    let effect_buttons = rgb_state
        .capabilities
        .supported_effects
        .iter()
        .map(|&kind| {
            let next = effect_of_kind(kind, color.unwrap_or(RgbColor::WHITE), effect_speed(effect));
            Button::new(SharedString::from(format!("{prefix}-effect-{kind:?}")))
                .label(effect_label(kind))
                .selected(effect.kind() == kind)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if next.kind() != effect.kind() {
                        this.apply_change(apply(next, brightness), cx);
                    }
                }))
        });

    let step_brightness = move |up: bool| {
        let pct = brightness.percent();
        let next = if up {
            pct.saturating_add(BRIGHTNESS_STEP).min(100)
        } else {
            pct.saturating_sub(BRIGHTNESS_STEP)
        };
        Brightness::new(next).unwrap_or(brightness)
    };
    let darker = step_brightness(false);
    let brighter = step_brightness(true);

    let theme = cx.theme();
    v_flex()
        .gap_4()
        .when(rgb_state.snapshot.zones.len() > 1, |this| {
            this.child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(zone_name),
            )
        })
        .child(
            field("Effect", theme).child(
                ButtonGroup::new(SharedString::from(format!("{prefix}-effects")))
                    .outline()
                    .small()
                    .children(effect_buttons),
            ),
        )
        .child(
            field("Brightness", cx.theme()).child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("{prefix}-brightness-dec")))
                            .outline()
                            .xsmall()
                            .label("−")
                            .accessibility_label("Dimmer")
                            .disabled(busy || brightness.percent() == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply_change(apply(effect, darker), cx);
                            })),
                    )
                    .child(readout(format!("{}%", brightness.percent()), cx.theme()))
                    .child(
                        Button::new(SharedString::from(format!("{prefix}-brightness-inc")))
                            .outline()
                            .xsmall()
                            .label("+")
                            .accessibility_label("Brighter")
                            .disabled(busy || brightness.percent() >= 100)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply_change(apply(effect, brighter), cx);
                            })),
                    ),
            ),
        )
        .when_some(color, |this, current| {
            this.child(render_colors(
                &prefix, effect, brightness, current, busy, apply, cx,
            ))
        })
}

fn render_colors<F>(
    prefix: &str,
    effect: RgbEffect,
    brightness: Brightness,
    current: RgbColor,
    busy: bool,
    apply: F,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<F>
where
    F: Fn(RgbEffect, Brightness) -> SettingChange + Copy + 'static,
{
    let swatches: Vec<AnyElement> = PRESETS
        .iter()
        .map(|&(name, color)| {
            let next = with_color(effect, color);
            Button::new(SharedString::from(format!("{prefix}-color-{name}")))
                .outline()
                .small()
                .compact()
                .selected(color == current)
                .disabled(busy)
                .accessibility_label(format!("{name} {color}"))
                .tooltip(format!("{name} {color}"))
                .child(
                    div()
                        .size_4()
                        .rounded_sm()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(swatch_color(color)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if color != current {
                        this.apply_change(apply(next, brightness), cx);
                    }
                }))
                .into_any_element()
        })
        .collect();
    let theme = cx.theme();
    field("Color", theme).child(
        h_flex()
            .gap_3()
            .child(h_flex().gap_1().children(swatches))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .size_5()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme.border)
                            .bg(swatch_color(current)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(current.to_string()),
                    ),
            ),
    )
}
