//! Lighting editor: zone switcher, firmware preset grid, brightness / speed, full-color mode
//! and a color editor (HSV sliders, RGB steppers, hex entry and a curated palette).

use agnoperiph_core::{
    Brightness, RgbColor, RgbDirection, RgbEffect, RgbEffectKind, RgbPresetInfo, RgbZoneId,
    RgbZoneInfo, RgbZoneState,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, Theme,
    button::{Button, ButtonGroup},
    h_flex,
    input::{Input, InputEvent},
    slider::Slider,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, group, muted_note, readout, section};
use crate::backend::{DeviceSnapshot, RgbState, SettingChange};
use crate::controls::{SliderRole, sync_input, sync_slider, to_units};

/// Curated swatches: the spectrum at full saturation plus white and two warm/cool whites.
const PALETTE: [(&str, RgbColor); 12] = [
    ("Red", RgbColor::new(255, 0, 0)),
    ("Orange", RgbColor::new(255, 110, 0)),
    ("Amber", RgbColor::new(255, 180, 0)),
    ("Yellow", RgbColor::new(255, 235, 0)),
    ("Lime", RgbColor::new(130, 255, 0)),
    ("Green", RgbColor::new(0, 255, 60)),
    ("Cyan", RgbColor::new(0, 230, 255)),
    ("Blue", RgbColor::new(0, 70, 255)),
    ("Violet", RgbColor::new(140, 0, 255)),
    ("Magenta", RgbColor::new(255, 0, 200)),
    ("Warm white", RgbColor::new(255, 200, 140)),
    ("White", RgbColor::new(255, 255, 255)),
];

const DEFAULT_SPEED: u8 = 128;

/// Converts RGB to hue (0..360), saturation and value (0..=1).
pub fn rgb_to_hsv(color: RgbColor) -> (f32, f32, f32) {
    let r = f32::from(color.r) / 255.0;
    let g = f32::from(color.g) / 255.0;
    let b = f32::from(color.b) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if (max - r).abs() <= f32::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() <= f32::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (hue, saturation, max)
}

/// Converts hue (degrees), saturation and value (0..=1) to RGB.
#[allow(clippy::many_single_char_names)]
pub fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> RgbColor {
    let h = hue.rem_euclid(360.0) / 60.0;
    let s = saturation.clamp(0.0, 1.0);
    let v = value.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h {
        h if h < 1.0 => (c, x, 0.0),
        h if h < 2.0 => (x, c, 0.0),
        h if h < 3.0 => (0.0, c, x),
        h if h < 4.0 => (0.0, x, c),
        h if h < 5.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let channel = |f: f32| u8::try_from(to_units((f + m) * 255.0).min(255)).unwrap_or(255);
    RgbColor::new(channel(r), channel(g), channel(b))
}

pub fn rgb_to_hex(color: RgbColor) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b)
}

/// Parses `#RRGGBB` or `RRGGBB` (case-insensitive).
pub fn hex_to_rgb(text: &str) -> Option<RgbColor> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let [_, r, g, b] = value.to_be_bytes();
    Some(RgbColor::new(r, g, b))
}

/// Lighting edits that have not been written yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LightingUi {
    pub(crate) zone: RgbZoneId,
    /// HSV while a color slider is dragged, so hue survives zero saturation.
    hsv: Option<(f32, f32, f32)>,
    brightness: Option<u8>,
    speed: Option<u8>,
}

impl Default for LightingUi {
    fn default() -> Self {
        Self {
            zone: RgbZoneId::new(0),
            hsv: None,
            brightness: None,
            speed: None,
        }
    }
}

impl LightingUi {
    pub(crate) fn clear_draft(&mut self) {
        self.hsv = None;
        self.brightness = None;
        self.speed = None;
    }
}

fn effect_label(kind: RgbEffectKind) -> &'static str {
    match kind {
        RgbEffectKind::Off => "Off",
        RgbEffectKind::Static => "Static",
        RgbEffectKind::Breathing => "Breathing",
        RgbEffectKind::SpectrumCycle => "Spectrum",
        RgbEffectKind::Wave => "Wave",
        RgbEffectKind::Reactive => "Reactive",
        RgbEffectKind::Preset => "Preset",
    }
}

fn effect_color(effect: RgbEffect) -> Option<RgbColor> {
    match effect {
        RgbEffect::Static { color }
        | RgbEffect::Breathing { color, .. }
        | RgbEffect::Reactive { color, .. }
        | RgbEffect::Preset { color, .. } => Some(color),
        RgbEffect::Off | RgbEffect::SpectrumCycle { .. } | RgbEffect::Wave { .. } => None,
    }
}

fn effect_speed(effect: RgbEffect) -> u8 {
    match effect {
        RgbEffect::Breathing { speed, .. }
        | RgbEffect::SpectrumCycle { speed }
        | RgbEffect::Wave { speed, .. }
        | RgbEffect::Reactive { speed, .. }
        | RgbEffect::Preset { speed, .. } => speed,
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
        RgbEffectKind::Preset => RgbEffect::Preset {
            id: 2,
            color,
            speed,
            colorful: false,
        },
    }
}

fn with_color(effect: RgbEffect, color: RgbColor) -> RgbEffect {
    match effect {
        RgbEffect::Preset {
            id,
            speed,
            colorful,
            ..
        } => RgbEffect::Preset {
            id,
            color,
            speed,
            colorful,
        },
        other => effect_of_kind(other.kind(), color, effect_speed(other)),
    }
}

fn with_speed(effect: RgbEffect, speed: u8) -> RgbEffect {
    match effect {
        RgbEffect::Preset {
            id,
            color,
            colorful,
            ..
        } => RgbEffect::Preset {
            id,
            color,
            speed,
            colorful,
        },
        other => effect_of_kind(
            other.kind(),
            effect_color(other).unwrap_or(RgbColor::WHITE),
            speed,
        ),
    }
}

/// Device LED color as a GPUI color. The color is the data itself, not a theme choice.
fn swatch_color(color: RgbColor) -> Rgba {
    rgb((u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b))
}

impl AgnoPeriphApp {
    fn rgb_state(&self) -> Option<&RgbState> {
        self.selected_device()?.rgb.as_ref()
    }

    /// The zone being edited, falling back to the first reported zone.
    fn active_zone(&self) -> Option<(RgbZoneInfo, RgbZoneState)> {
        let rgb = self.rgb_state()?;
        let state = rgb
            .snapshot
            .zones
            .iter()
            .find(|z| z.zone == self.lighting.zone)
            .or_else(|| rgb.snapshot.zones.first())?;
        let info = rgb
            .capabilities
            .zones
            .iter()
            .find(|z| z.id == state.zone)
            .cloned()
            .unwrap_or_else(|| {
                RgbZoneInfo::new(state.zone, format!("Zone {}", state.zone.value()))
            });
        Some((info, *state))
    }

    fn preset_info(&self) -> Option<RgbPresetInfo> {
        let (info, state) = self.active_zone()?;
        let id = match state.effect {
            RgbEffect::Preset { id, .. } => id,
            RgbEffect::Off => 0,
            _ => return None,
        };
        info.presets.into_iter().find(|p| p.id == id)
    }

    fn write_zone(&mut self, effect: RgbEffect, brightness: Brightness, cx: &mut Context<Self>) {
        let Some((_, state)) = self.active_zone() else {
            return;
        };
        self.apply_change(
            SettingChange::Rgb {
                zone: state.zone,
                effect,
                brightness,
            },
            cx,
        );
    }

    fn select_zone(&mut self, zone: RgbZoneId, cx: &mut Context<Self>) {
        if self.lighting.zone != zone {
            self.lighting = LightingUi {
                zone,
                ..LightingUi::default()
            };
            self.request_sync(cx);
            cx.notify();
        }
    }

    fn set_zone_color(&mut self, color: RgbColor, cx: &mut Context<Self>) {
        let Some((_, state)) = self.active_zone() else {
            return;
        };
        self.lighting.hsv = None;
        if effect_color(state.effect) == Some(color) {
            return;
        }
        self.write_zone(with_color(state.effect, color), state.brightness, cx);
    }

    fn step_channel(&mut self, channel: usize, delta: i16, cx: &mut Context<Self>) {
        let Some(current) = self.active_zone().and_then(|(_, s)| effect_color(s.effect)) else {
            return;
        };
        let mut rgb = [current.r, current.g, current.b];
        let next = (i16::from(rgb[channel]) + delta).clamp(0, 255);
        rgb[channel] = u8::try_from(next).unwrap_or(rgb[channel]);
        self.set_zone_color(RgbColor::new(rgb[0], rgb[1], rgb[2]), cx);
    }

    fn set_colorful(&mut self, colorful: bool, cx: &mut Context<Self>) {
        let Some((_, state)) = self.active_zone() else {
            return;
        };
        if let RgbEffect::Preset {
            id, color, speed, ..
        } = state.effect
        {
            self.write_zone(
                RgbEffect::Preset {
                    id,
                    color,
                    speed,
                    colorful,
                },
                state.brightness,
                cx,
            );
        }
    }
}

pub(crate) fn on_slider(
    app: &mut AgnoPeriphApp,
    role: SliderRole,
    value: f32,
    released: bool,
    _window: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let Some((_, state)) = app.active_zone() else {
        return;
    };
    let units = to_units(value);
    match role {
        SliderRole::Brightness => {
            let pct = u8::try_from(units.min(100)).unwrap_or(100);
            app.lighting.brightness = Some(pct);
            if released {
                app.lighting.brightness = None;
                if pct != state.brightness.percent()
                    && let Ok(brightness) = Brightness::new(pct)
                {
                    app.write_zone(state.effect, brightness, cx);
                }
            }
        }
        SliderRole::Speed => {
            let speed = u8::try_from(units.min(255)).unwrap_or(255);
            app.lighting.speed = Some(speed);
            if released {
                app.lighting.speed = None;
                if speed != effect_speed(state.effect) {
                    app.write_zone(with_speed(state.effect, speed), state.brightness, cx);
                }
            }
        }
        SliderRole::Hue | SliderRole::Saturation | SliderRole::Value => {
            let current = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
            let (mut h, mut s, mut v) = app.lighting.hsv.unwrap_or_else(|| rgb_to_hsv(current));
            match role {
                SliderRole::Hue => h = value,
                SliderRole::Saturation => s = value / 100.0,
                _ => v = value / 100.0,
            }
            app.lighting.hsv = Some((h, s, v));
            if released {
                let color = hsv_to_rgb(h, s, v);
                app.set_zone_color(color, cx);
                app.lighting.hsv = Some((h, s, v));
            }
        }
        _ => {}
    }
    cx.notify();
}

pub(crate) fn on_hex_input(
    app: &mut AgnoPeriphApp,
    event: &InputEvent,
    value: &str,
    _window: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur)
        && let Some(color) = hex_to_rgb(value)
    {
        app.set_zone_color(color, cx);
    }
}

pub(crate) fn sync_controls(
    app: &mut AgnoPeriphApp,
    window: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let (Some(controls), Some((_, state))) = (app.controls.as_ref(), app.active_zone()) else {
        return;
    };
    let brightness = app
        .lighting
        .brightness
        .unwrap_or_else(|| state.brightness.percent());
    let speed = app
        .lighting
        .speed
        .unwrap_or_else(|| effect_speed(state.effect));
    let color = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
    let (hue, saturation, value) = app.lighting.hsv.unwrap_or_else(|| rgb_to_hsv(color));
    let shown = app
        .lighting
        .hsv
        .map_or(color, |(h, s, v)| hsv_to_rgb(h, s, v));
    let (brightness_slider, speed_slider, hue_slider, saturation_slider, value_slider, hex) = (
        controls.brightness.clone(),
        controls.speed.clone(),
        controls.hue.clone(),
        controls.saturation.clone(),
        controls.value.clone(),
        controls.hex.clone(),
    );
    sync_slider(&brightness_slider, f32::from(brightness), window, cx);
    sync_slider(&speed_slider, f32::from(speed), window, cx);
    sync_slider(&hue_slider, hue, window, cx);
    sync_slider(&saturation_slider, saturation * 100.0, window, cx);
    sync_slider(&value_slider, value * 100.0, window, cx);
    let is_typing = hex.read(cx).focus_handle(cx).is_focused(window);
    if !is_typing {
        sync_input(&hex, &rgb_to_hex(shown), window, cx);
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let base = section(
        "rgb-panel",
        "Lighting",
        "Effects, brightness and color for each lighting zone.",
        cx.theme(),
    );
    let (Some(rgb), Some((info, state))) = (device.rgb.as_ref(), app.active_zone()) else {
        return base.child(muted_note(
            "The lighting state couldn’t be read.",
            cx.theme(),
        ));
    };
    let busy = app.is_busy();
    let zone_buttons = rgb
        .snapshot
        .zones
        .iter()
        .map(|zone| {
            let id = zone.zone;
            let name = rgb
                .capabilities
                .zones
                .iter()
                .find(|z| z.id == id)
                .map_or_else(|| format!("Zone {}", id.value()), |z| z.name.clone());
            Button::new(("rgb-zone", u64::from(id.value())))
                .label(name)
                .selected(id == state.zone)
                .on_click(cx.listener(move |this, _, _, cx| this.select_zone(id, cx)))
        })
        .collect::<Vec<_>>();
    let preset = app.preset_info();
    let supports_color = preset.as_ref().map_or_else(
        || effect_color(state.effect).is_some(),
        |p| p.supports_color,
    );
    let supports_speed = preset.as_ref().map_or_else(
        || !matches!(state.effect, RgbEffect::Off | RgbEffect::Static { .. }),
        |p| p.supports_speed,
    );
    let supports_colorful = preset.as_ref().is_some_and(|p| p.supports_colorful);
    let colorful = matches!(state.effect, RgbEffect::Preset { colorful: true, .. });

    let effects = render_effects(app, &info, state, rgb, busy, cx);
    let adjust = render_adjustments(
        app,
        state,
        supports_speed,
        supports_colorful,
        colorful,
        busy,
        cx,
    );
    let color = (supports_color && !colorful).then(|| render_color(app, state, busy, cx));

    base.when(rgb.snapshot.zones.len() > 1, |this| {
        this.child(
            ButtonGroup::new("rgb-zones")
                .outline()
                .small()
                .children(zone_buttons),
        )
    })
    .child(effects)
    .child(
        h_flex()
            .items_start()
            .gap_6()
            .child(adjust)
            .when_some(color, gpui_kit::ParentElement::child),
    )
}

fn render_effects(
    app: &AgnoPeriphApp,
    info: &RgbZoneInfo,
    state: RgbZoneState,
    rgb: &RgbState,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let zone = state.zone.value();
    let current_color = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
    let current_speed = effect_speed(state.effect);
    let current_colorful = matches!(state.effect, RgbEffect::Preset { colorful: true, .. });
    let brightness = state.brightness;
    let buttons: Vec<AnyElement> = if info.presets.is_empty() {
        rgb.capabilities
            .supported_effects
            .iter()
            .map(|&kind| {
                let next = effect_of_kind(kind, current_color, current_speed);
                let is_current = state.effect.kind() == kind;
                Button::new(SharedString::from(format!("rgb-{zone}-effect-{kind:?}")))
                    .outline()
                    .small()
                    .label(effect_label(kind))
                    .selected(is_current)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !is_current {
                            this.write_zone(next, brightness, cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect()
    } else {
        info.presets
            .iter()
            .map(|preset| {
                let id = preset.id;
                let is_current = match state.effect {
                    RgbEffect::Preset { id: current, .. } => current == id,
                    RgbEffect::Off => id == 0,
                    _ => false,
                };
                let next = if id == 0 {
                    RgbEffect::Off
                } else {
                    RgbEffect::Preset {
                        id,
                        color: current_color,
                        speed: current_speed,
                        colorful: current_colorful && preset.supports_colorful,
                    }
                };
                Button::new(SharedString::from(format!("rgb-{zone}-preset-{id}")))
                    .outline()
                    .small()
                    .label(preset.name.clone())
                    .selected(is_current)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !is_current {
                            this.write_zone(next, brightness, cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect()
    };
    let _ = app;
    let theme = cx.theme();
    group(
        ("rgb-effects", u64::from(zone)),
        format!("{} effects", info.name),
        theme,
    )
    .child(h_flex().flex_wrap().gap_2().children(buttons))
}

fn labelled_slider(
    label: &'static str,
    value: String,
    slider: Option<Entity<gpui_kit::component::slider::SliderState>>,
    disabled: bool,
    theme: &Theme,
) -> impl IntoElement {
    v_flex()
        .gap_1p5()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(readout(value, theme)),
        )
        .when_some(slider, |this, slider| {
            this.child(
                Slider::new(&slider)
                    .disabled(disabled)
                    .accessibility_label(label),
            )
        })
}

#[allow(clippy::fn_params_excessive_bools)]
fn render_adjustments(
    app: &AgnoPeriphApp,
    state: RgbZoneState,
    supports_speed: bool,
    supports_colorful: bool,
    colorful: bool,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let controls = app.controls.as_ref();
    let brightness = app
        .lighting
        .brightness
        .unwrap_or_else(|| state.brightness.percent());
    let speed = app
        .lighting
        .speed
        .unwrap_or_else(|| effect_speed(state.effect));
    let is_off = matches!(state.effect, RgbEffect::Off);
    let theme = cx.theme();
    group("rgb-adjust", "Adjust", theme)
        .w_80()
        .flex_shrink_0()
        .child(labelled_slider(
            "Brightness",
            format!("{brightness}%"),
            controls.map(|c| c.brightness.clone()),
            busy || is_off,
            theme,
        ))
        .when(supports_speed, |this| {
            this.child(labelled_slider(
                "Speed",
                speed.to_string(),
                controls.map(|c| c.speed.clone()),
                busy,
                theme,
            ))
        })
        .when(supports_colorful, |this| {
            this.child(
                Switch::new("rgb-colorful")
                    .label("Full color")
                    .checked(colorful)
                    .disabled(busy)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_colorful(*checked, cx);
                    })),
            )
        })
        .when(is_off, |this| {
            this.child(muted_note(
                "Choose an effect to turn this zone on.",
                cx.theme(),
            ))
        })
}

fn render_color(
    app: &AgnoPeriphApp,
    state: RgbZoneState,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let current = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
    let shown = app
        .lighting
        .hsv
        .map_or(current, |(h, s, v)| hsv_to_rgb(h, s, v));
    let (h, s, v) = app.lighting.hsv.unwrap_or_else(|| rgb_to_hsv(current));
    let controls = app.controls.as_ref();
    let hue = controls.map(|c| c.hue.clone());
    let saturation = controls.map(|c| c.saturation.clone());
    let value = controls.map(|c| c.value.clone());
    let hex = controls.map(|c| c.hex.clone());

    let swatches: Vec<AnyElement> = PALETTE
        .iter()
        .map(|&(name, color)| {
            let theme = cx.theme();
            Button::new(SharedString::from(format!("rgb-swatch-{name}")))
                .outline()
                .small()
                .compact()
                .selected(color == current)
                .disabled(busy)
                .accessibility_label(format!("{name} {}", rgb_to_hex(color)))
                .tooltip(name)
                .child(
                    div()
                        .size_5()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(swatch_color(color)),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_zone_color(color, cx)))
                .into_any_element()
        })
        .collect();

    let channels: Vec<AnyElement> = [("R", 0usize, shown.r), ("G", 1, shown.g), ("B", 2, shown.b)]
        .into_iter()
        .map(|(label, channel, level)| {
            let theme = cx.theme();
            h_flex()
                .gap_1()
                .child(
                    div()
                        .w_4()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    Button::new(("rgb-channel-dec", channel))
                        .outline()
                        .xsmall()
                        .label("−")
                        .accessibility_label(format!("Decrease {label}"))
                        .disabled(busy || level == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.step_channel(channel, -5, cx);
                        })),
                )
                .child(
                    div()
                        .w_10()
                        .text_sm()
                        .text_center()
                        .font_family(theme.mono_font_family.clone())
                        .child(level.to_string()),
                )
                .child(
                    Button::new(("rgb-channel-inc", channel))
                        .outline()
                        .xsmall()
                        .label("+")
                        .accessibility_label(format!("Increase {label}"))
                        .disabled(busy || level == 255)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.step_channel(channel, 5, cx);
                        })),
                )
                .into_any_element()
        })
        .collect();

    let theme = cx.theme();
    // The hue track is the LED spectrum itself; its stops are data, not theme colors.
    let spectrum = h_flex()
        .h_1p5()
        .w_full()
        .rounded(theme.radius)
        .overflow_hidden()
        .children((0..12).map(|step| {
            div().flex_1().h_full().bg(swatch_color(hsv_to_rgb(
                f32::from(u8::try_from(step).unwrap_or(0)) * 30.0,
                1.0,
                1.0,
            )))
        }));
    group("rgb-color", "Color", theme)
        .flex_1()
        .min_w_0()
        .child(
            h_flex()
                .gap_4()
                .items_start()
                .child(
                    div()
                        .id("rgb-preview")
                        .size_16()
                        .flex_shrink_0()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(theme.border)
                        .bg(swatch_color(shown)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(labelled_slider(
                                    "Hue",
                                    format!("{}°", to_units(h)),
                                    hue,
                                    busy,
                                    theme,
                                ))
                                .child(spectrum),
                        )
                        .child(labelled_slider(
                            "Saturation",
                            format!("{}%", to_units(s * 100.0)),
                            saturation,
                            busy,
                            theme,
                        ))
                        .child(labelled_slider(
                            "Brightness of color",
                            format!("{}%", to_units(v * 100.0)),
                            value,
                            busy,
                            theme,
                        )),
                ),
        )
        .child(
            h_flex()
                .gap_4()
                .flex_wrap()
                .child(h_flex().gap_3().children(channels))
                .when_some(hex, |this, hex| {
                    this.child(div().w_32().child(Input::new(&hex).small().disabled(busy)))
                }),
        )
        .child(h_flex().flex_wrap().gap_1().children(swatches))
}

#[cfg(test)]
mod tests {
    use agnoperiph_core::RgbColor;

    use super::{hex_to_rgb, hsv_to_rgb, rgb_to_hex, rgb_to_hsv};

    #[test]
    fn hsv_round_trips_primary_and_mixed_colors() {
        for color in [
            RgbColor::new(255, 0, 0),
            RgbColor::new(0, 255, 0),
            RgbColor::new(0, 0, 255),
            RgbColor::new(255, 110, 0),
            RgbColor::new(12, 34, 56),
            RgbColor::WHITE,
            RgbColor::BLACK,
        ] {
            let (h, s, v) = rgb_to_hsv(color);
            assert_eq!(hsv_to_rgb(h, s, v), color, "{color}");
        }
    }

    #[test]
    fn hex_parses_with_or_without_hash() {
        assert_eq!(hex_to_rgb("#FF8000"), Some(RgbColor::new(255, 128, 0)));
        assert_eq!(hex_to_rgb("00ff7f"), Some(RgbColor::new(0, 255, 127)));
        assert_eq!(hex_to_rgb("#12345"), None);
        assert_eq!(hex_to_rgb("#GG0000"), None);
        assert_eq!(rgb_to_hex(RgbColor::new(1, 171, 255)), "#01ABFF");
    }
}
