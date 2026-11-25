//! Lighting editor in ATK HUB's three-column deck: zone switch and effect grid, effect
//! settings (brightness, speed), and a color picker (saturation/value pad, hue strip,
//! swatches, HEX or RGB entry, full-color mode).

use agnoperiph_core::{
    Brightness, RgbColor, RgbDirection, RgbEffect, RgbEffectKind, RgbPresetInfo, RgbZoneId,
    RgbZoneInfo, RgbZoneState,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, Theme,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    input::{Input, InputEvent},
    slider::Slider,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, muted_note, section};
use crate::backend::{DeviceSnapshot, RgbState, SettingChange};
use crate::controls::{SliderRole, sync_input, sync_slider, to_units};

/// Swatch column from ATK HUB, read in pairs: the spectrum plus white.
const SWATCHES: [(&str, RgbColor); 12] = [
    ("Red", RgbColor::new(255, 0, 0)),
    ("Blue", RgbColor::new(0, 90, 255)),
    ("Orange", RgbColor::new(255, 110, 0)),
    ("Indigo", RgbColor::new(60, 40, 255)),
    ("Yellow", RgbColor::new(255, 220, 0)),
    ("Purple", RgbColor::new(140, 0, 255)),
    ("Lime", RgbColor::new(130, 255, 0)),
    ("Magenta", RgbColor::new(255, 0, 200)),
    ("Green", RgbColor::new(0, 255, 60)),
    ("Pink", RgbColor::new(255, 90, 140)),
    ("Teal", RgbColor::new(0, 200, 190)),
    ("White", RgbColor::new(255, 255, 255)),
];

/// Effect buttons per row in the effect grid.
const EFFECT_COLUMNS: usize = 3;

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

/// How the color entry row shows the color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ColorFormat {
    #[default]
    Hex,
    Rgb,
}

/// Which part of the color picker a pointer drag is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerPart {
    SaturationValue,
    Hue,
}

/// Lighting edits that have not been written yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LightingUi {
    pub(crate) zone: RgbZoneId,
    /// HSV while the picker is dragged, so hue survives zero saturation.
    hsv: Option<(f32, f32, f32)>,
    brightness: Option<u8>,
    speed: Option<u8>,
    pub(crate) format: ColorFormat,
    pub(crate) drag: Option<PickerPart>,
}

impl Default for LightingUi {
    fn default() -> Self {
        Self {
            zone: RgbZoneId::new(0),
            hsv: None,
            brightness: None,
            speed: None,
            format: ColorFormat::default(),
            drag: None,
        }
    }
}

impl LightingUi {
    pub(crate) fn clear_draft(&mut self) {
        self.hsv = None;
        self.brightness = None;
        self.speed = None;
        self.drag = None;
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
                format: self.lighting.format,
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

    fn set_color_format(&mut self, format: ColorFormat, cx: &mut Context<Self>) {
        if self.lighting.format != format {
            self.lighting.format = format;
            self.request_sync(cx);
            cx.notify();
        }
    }

    /// Moves the draft color to the pointer position `(x, y)`, each `0..=1`, on `part`.
    fn pick_hsv(&mut self, part: PickerPart, across: f32, down: f32, cx: &mut Context<Self>) {
        let Some((_, state)) = self.active_zone() else {
            return;
        };
        let current = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
        let (mut hue, mut saturation, mut value) =
            self.lighting.hsv.unwrap_or_else(|| rgb_to_hsv(current));
        match part {
            PickerPart::SaturationValue => {
                saturation = across;
                value = 1.0 - down;
            }
            PickerPart::Hue => {
                hue = (across * 360.0).min(359.0);
                // Picking a hue on a grey or black color would show no change.
                if saturation < 0.05 || value < 0.05 {
                    saturation = 1.0;
                    value = 1.0;
                }
            }
        }
        self.lighting.hsv = Some((hue, saturation, value));
        cx.notify();
    }

    /// Writes the dragged color once, when the pointer is released.
    fn commit_hsv(&mut self, cx: &mut Context<Self>) {
        let Some((h, s, v)) = self.lighting.hsv else {
            return;
        };
        self.set_zone_color(hsv_to_rgb(h, s, v), cx);
        self.lighting.hsv = Some((h, s, v));
        self.request_sync(cx);
        cx.notify();
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
    let shown = app
        .lighting
        .hsv
        .map_or(color, |(h, s, v)| hsv_to_rgb(h, s, v));
    let (brightness_slider, speed_slider, hex) = (
        controls.brightness.clone(),
        controls.speed.clone(),
        controls.hex.clone(),
    );
    sync_slider(&brightness_slider, f32::from(brightness), window, cx);
    sync_slider(&speed_slider, f32::from(speed), window, cx);
    let is_typing = hex.read(cx).focus_handle(cx).is_focused(window);
    if !is_typing {
        sync_input(&hex, &rgb_to_hex(shown), window, cx);
    }
}

/// What the zone's current effect lets the user change.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct ZoneSupport {
    color: bool,
    speed: bool,
    colorful: bool,
    is_colorful: bool,
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let (Some(rgb), Some((info, state))) = (device.rgb.as_ref(), app.active_zone()) else {
        return section(
            "rgb-panel",
            "Lighting",
            "The lighting state couldn’t be read.",
            cx.theme(),
        );
    };
    let busy = app.is_busy();
    let preset = app.preset_info();
    let support = ZoneSupport {
        color: preset.as_ref().map_or_else(
            || effect_color(state.effect).is_some(),
            |p| p.supports_color,
        ),
        speed: preset.as_ref().map_or_else(
            || !matches!(state.effect, RgbEffect::Off | RgbEffect::Static { .. }),
            |p| p.supports_speed,
        ),
        colorful: preset.as_ref().is_some_and(|p| p.supports_colorful),
        is_colorful: matches!(state.effect, RgbEffect::Preset { colorful: true, .. }),
    };

    let effects = render_effects(app, &info, state, rgb, busy, cx);
    let settings = render_lamp_settings(app, &info, state, support, busy, cx);
    let color = render_color(app, state, support, busy, cx);
    let theme = cx.theme();
    let divider = || div().w_px().flex_shrink_0().bg(theme.border);

    h_flex()
        .id("rgb-panel")
        .items_stretch()
        .gap_5()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .child(effects)
        .child(divider())
        .child(settings)
        .child(divider())
        .child(color)
}

fn column_title(title: impl Into<SharedString>, theme: &Theme) -> Div {
    h_flex().gap_2().justify_between().min_h_7().child(
        div()
            .text_base()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.foreground)
            .child(title.into()),
    )
}

/// Left column: zone switch and the effect grid, three equal columns like ATK HUB.
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

    let effect_button = |id: SharedString,
                         name: SharedString,
                         is_current: bool,
                         next: RgbEffect,
                         cx: &mut Context<AgnoPeriphApp>| {
        let button = Button::new(id)
            .small()
            .flex_1()
            .min_w_0()
            .selected(is_current)
            .disabled(busy)
            .tooltip(name.clone())
            .child(div().min_w_0().truncate().child(name))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !is_current {
                    this.write_zone(next, brightness, cx);
                }
            }));
        if is_current {
            button.primary()
        } else {
            button.outline()
        }
        .into_any_element()
    };

    let buttons: Vec<AnyElement> = if info.presets.is_empty() {
        rgb.capabilities
            .supported_effects
            .iter()
            .map(|&kind| {
                effect_button(
                    format!("rgb-{zone}-effect-{kind:?}").into(),
                    effect_label(kind).into(),
                    state.effect.kind() == kind,
                    effect_of_kind(kind, current_color, current_speed),
                    cx,
                )
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
                effect_button(
                    format!("rgb-{zone}-preset-{id}").into(),
                    preset.name.clone().into(),
                    is_current,
                    next,
                    cx,
                )
            })
            .collect()
    };

    let zone_buttons: Vec<Button> = rgb
        .snapshot
        .zones
        .iter()
        .map(|zone_state| {
            let id = zone_state.zone;
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
        .collect();
    let has_zones = zone_buttons.len() > 1;

    let mut grid = v_flex().gap_1p5();
    let mut buttons = buttons.into_iter().peekable();
    while buttons.peek().is_some() {
        let mut row = h_flex().gap_1p5();
        for _ in 0..EFFECT_COLUMNS {
            row = match buttons.next() {
                Some(button) => row.child(button),
                // Keeps the last row's buttons the same width as the rows above.
                None => row.child(div().flex_1().min_w_0()),
            };
        }
        grid = grid.child(row);
    }

    let _ = app;
    let theme = cx.theme();
    v_flex()
        .id(("rgb-effects", u64::from(zone)))
        .flex_basis(relative(0.38))
        .flex_grow(1.)
        .min_w_0()
        .gap_3()
        .child(if has_zones {
            h_flex().min_h_7().child(
                ButtonGroup::new("rgb-zones")
                    .outline()
                    .small()
                    .children(zone_buttons),
            )
        } else {
            column_title(info.name.clone(), theme)
        })
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Lighting effect mode"),
        )
        .child(grid)
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
                .child(div().text_sm().child(label))
                .child(
                    div()
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(if disabled {
                            theme.muted_foreground
                        } else {
                            theme.foreground
                        })
                        .child(value),
                ),
        )
        .when_some(slider, |this, slider| {
            this.child(
                Slider::new(&slider)
                    .disabled(disabled)
                    .accessibility_label(label),
            )
        })
}

/// Middle column: brightness and speed, always in the same place so the layout never
/// jumps between effects; unsupported controls are disabled with a reason.
fn render_lamp_settings(
    app: &AgnoPeriphApp,
    info: &RgbZoneInfo,
    state: RgbZoneState,
    support: ZoneSupport,
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
    let speed_pct = speed_percent(speed, info.max_speed);
    let is_off = matches!(state.effect, RgbEffect::Off);
    let note = if is_off {
        Some("Choose an effect to turn this zone on.")
    } else if !support.speed {
        Some("This effect runs at a fixed speed.")
    } else {
        None
    };
    let theme = cx.theme();
    v_flex()
        .id("rgb-adjust")
        .flex_basis(relative(0.24))
        .flex_grow(1.)
        .min_w_48()
        .gap_4()
        .child(column_title("Lamp effect settings", theme))
        .child(labelled_slider(
            "Brightness",
            format!("{brightness}%"),
            controls.map(|c| c.brightness.clone()),
            busy || is_off,
            theme,
        ))
        .child(labelled_slider(
            "Speed",
            format!("{speed_pct}%"),
            controls.map(|c| c.speed.clone()),
            busy || !support.speed,
            theme,
        ))
        .when_some(note, |this, note| this.child(muted_note(note, theme)))
}

/// Speed as a 0–100 % readout of the zone's own range. Firmware defaults can sit above a
/// zone's maximum, so the result is clamped.
fn speed_percent(speed: u8, max_speed: u8) -> u16 {
    let max = u32::from(max_speed.max(1));
    let pct = (u32::from(speed) * 100 + max / 2) / max;
    u16::try_from(pct.min(100)).unwrap_or(100)
}

/// Right column: full-color switch, saturation/value pad with hue strip, swatches, and
/// a HEX / RGB entry row.
#[allow(clippy::too_many_lines)]
fn render_color(
    app: &AgnoPeriphApp,
    state: RgbZoneState,
    support: ZoneSupport,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let current = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
    let (h, s, v) = app.lighting.hsv.unwrap_or_else(|| rgb_to_hsv(current));
    let shown = app
        .lighting
        .hsv
        .map_or(current, |(h, s, v)| hsv_to_rgb(h, s, v));
    let is_enabled = support.color && !support.is_colorful && !busy;
    let hex = app.controls.as_ref().map(|c| c.hex.clone());
    let format = app.lighting.format;
    let entity = cx.entity().downgrade();

    let swatches: Vec<AnyElement> = SWATCHES
        .iter()
        .map(|&(name, color)| {
            let theme = cx.theme();
            Button::new(SharedString::from(format!("rgb-swatch-{name}")))
                .outline()
                .xsmall()
                .compact()
                .selected(color == current)
                .disabled(!is_enabled)
                .accessibility_label(format!("{name} {}", rgb_to_hex(color)))
                .tooltip(name)
                .child(
                    div()
                        .size_4()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(swatch_color(color)),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_zone_color(color, cx)))
                .into_any_element()
        })
        .collect();
    let mut swatch_grid = v_flex().flex_shrink_0().gap_1();
    let mut swatches = swatches.into_iter();
    while let Some(first) = swatches.next() {
        swatch_grid = swatch_grid.child(h_flex().gap_1().child(first).children(swatches.next()));
    }

    let format_toggle = ButtonGroup::new("rgb-format")
        .outline()
        .xsmall()
        .child(
            Button::new("rgb-format-hex")
                .label("HEX")
                .selected(format == ColorFormat::Hex)
                .on_click(
                    cx.listener(|this, _, _, cx| this.set_color_format(ColorFormat::Hex, cx)),
                ),
        )
        .child(
            Button::new("rgb-format-rgb")
                .label("RGB")
                .selected(format == ColorFormat::Rgb)
                .on_click(
                    cx.listener(|this, _, _, cx| this.set_color_format(ColorFormat::Rgb, cx)),
                ),
        );

    let entry: AnyElement = match format {
        ColorFormat::Hex => h_flex()
            .gap_2()
            .flex_1()
            .min_w_0()
            .when_some(hex, |this, hex| {
                this.child(
                    div()
                        .w_32()
                        .child(Input::new(&hex).small().disabled(!is_enabled)),
                )
            })
            .into_any_element(),
        ColorFormat::Rgb => h_flex()
            .gap_3()
            .flex_1()
            .min_w_0()
            .children(
                [("R", 0usize, shown.r), ("G", 1, shown.g), ("B", 2, shown.b)]
                    .into_iter()
                    .map(|(label, channel, level)| {
                        channel_stepper(label, channel, level, is_enabled, cx)
                    }),
            )
            .into_any_element(),
    };

    let theme = cx.theme();
    let thumb = |theme: &Theme| {
        div()
            .absolute()
            .size_3()
            .rounded_full()
            .border_2()
            .border_color(theme.background)
            .shadow_sm()
    };
    let pure_hue = swatch_color(hsv_to_rgb(h, 1.0, 1.0));
    let white = gpui_kit::white();
    let black = gpui_kit::black();
    // The pad and strip draw LED color space itself: their colors are data, not theme.
    let pad = div()
        .id("rgb-sv-pad")
        .relative()
        .w_full()
        .h_32()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(pure_hue)
        .when(is_enabled, Styled::cursor_crosshair)
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(theme.radius)
                .bg(linear_gradient(
                    90.,
                    linear_color_stop(white, 0.),
                    linear_color_stop(white.opacity(0.), 1.),
                )),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(theme.radius)
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(black.opacity(0.), 0.),
                    linear_color_stop(black, 1.),
                )),
        )
        .child(
            thumb(theme)
                .left(relative(s.clamp(0.0, 1.0)))
                .top(relative((1.0 - v).clamp(0.0, 1.0)))
                .ml(rems(-0.375))
                .mt(rems(-0.375))
                .bg(swatch_color(shown)),
        )
        .child(picker_surface(
            PickerPart::SaturationValue,
            is_enabled,
            entity.clone(),
        ))
        .test_support();

    let strip = div()
        .id("rgb-hue-strip")
        .relative()
        .w_full()
        .h_3()
        .rounded_full()
        .when(is_enabled, Styled::cursor_crosshair)
        .child(
            h_flex()
                .size_full()
                .rounded_full()
                .overflow_hidden()
                .children((0..6u8).map(|segment| {
                    let from = f32::from(segment) * 60.0;
                    div().flex_1().h_full().bg(linear_gradient(
                        90.,
                        linear_color_stop(swatch_color(hsv_to_rgb(from, 1.0, 1.0)), 0.),
                        linear_color_stop(swatch_color(hsv_to_rgb(from + 60.0, 1.0, 1.0)), 1.),
                    ))
                })),
        )
        .child(
            thumb(theme)
                .top_0()
                .left(relative((h / 360.0).clamp(0.0, 1.0)))
                .ml(rems(-0.375))
                .bg(pure_hue),
        )
        .child(picker_surface(PickerPart::Hue, is_enabled, entity))
        .test_support();

    let reason = if support.is_colorful {
        Some("Full color mode cycles the spectrum, so a single color isn’t used.")
    } else if !support.color {
        Some("This effect sets its own colors.")
    } else {
        None
    };

    v_flex()
        .id("rgb-color")
        .flex_basis(relative(0.38))
        .flex_grow(1.)
        .min_w_72()
        .gap_3()
        .child(
            column_title("Color settings", theme).child(
                Switch::new("rgb-colorful")
                    .label("Full color mode")
                    .checked(support.is_colorful)
                    .disabled(busy || !support.colorful)
                    .tooltip(if support.colorful {
                        "Cycle through every color instead of one"
                    } else {
                        "This effect has no full color mode"
                    })
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_colorful(*checked, cx);
                    })),
            ),
        )
        .child(
            h_flex()
                .items_start()
                .gap_4()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2p5()
                        .when(!is_enabled, |this| this.opacity(0.45))
                        .child(pad)
                        .child(strip),
                )
                .child(swatch_grid),
        )
        // Toggle and preview on one line, the entry below: R/G/B steppers need the
        // column's full width at the default window size.
        .child(
            h_flex()
                .gap_3()
                .justify_between()
                .child(format_toggle)
                .child(
                    div()
                        .id("rgb-preview")
                        .flex_shrink_0()
                        .size_6()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(swatch_color(shown))
                        .test_support(),
                ),
        )
        .child(entry)
        .when_some(reason, |this, reason| this.child(muted_note(reason, theme)))
}

fn channel_stepper(
    label: &'static str,
    channel: usize,
    level: u8,
    is_enabled: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .gap_0p5()
        .child(
            div()
                .w_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            Button::new(("rgb-channel-dec", channel))
                .ghost()
                .xsmall()
                .label("−")
                .accessibility_label(format!("Decrease {label}"))
                .disabled(!is_enabled || level == 0)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.step_channel(channel, -5, cx);
                })),
        )
        .child(
            div()
                .w_8()
                .text_sm()
                .text_center()
                .font_family(cx.theme().mono_font_family.clone())
                .child(level.to_string()),
        )
        .child(
            Button::new(("rgb-channel-inc", channel))
                .ghost()
                .xsmall()
                .label("+")
                .accessibility_label(format!("Increase {label}"))
                .disabled(!is_enabled || level == 255)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.step_channel(channel, 5, cx);
                })),
        )
        .into_any_element()
}

/// Pointer surface over a picker part. Window-level listeners keep a drag alive when the
/// pointer leaves the part; a press only starts one on this part's own unoccluded hitbox.
fn picker_surface(
    part: PickerPart,
    is_enabled: bool,
    app: WeakEntity<AgnoPeriphApp>,
) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let fraction = move |position: Point<Pixels>| {
                let x = ((position.x - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
                let y = ((position.y - bounds.top()) / bounds.size.height).clamp(0.0, 1.0);
                (x, y)
            };
            if is_enabled {
                let app_down = app.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble
                        && event.button == MouseButton::Left
                        && hitbox.is_hovered(window)
                    {
                        let (x, y) = fraction(event.position);
                        let _ = app_down.update(cx, |this, cx| {
                            this.lighting.drag = Some(part);
                            this.pick_hsv(part, x, y, cx);
                        });
                    }
                });
            }
            let app_move = app.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let is_pressed = event.pressed_button == Some(MouseButton::Left);
                let (x, y) = fraction(event.position);
                let _ = app_move.update(cx, |this, cx| {
                    if this.lighting.drag != Some(part) {
                        return;
                    }
                    if is_pressed {
                        this.pick_hsv(part, x, y, cx);
                    } else {
                        // The release happened where this window couldn't see it.
                        this.lighting.drag = None;
                        this.commit_hsv(cx);
                    }
                });
            });
            let app_up = app.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = app_up.update(cx, |this, cx| {
                    if this.lighting.drag == Some(part) {
                        this.lighting.drag = None;
                        this.commit_hsv(cx);
                    }
                });
            });
        },
    )
    .absolute()
    .inset_0()
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
