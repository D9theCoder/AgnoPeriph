//! Lighting editor: the board lit as the zone looks, the lamp settings beside it, then the
//! effect list and a color picker (saturation/value pad, hue strip, swatches, HEX or RGB
//! entry, full-color mode).

use agnoperiph_core::{
    Brightness, RgbColor, RgbDirection, RgbEffect, RgbEffectKind, RgbPresetInfo, RgbZoneId,
    RgbZoneInfo, RgbZoneState,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    h_flex,
    input::{Input, InputEvent},
    slider::{Slider, SliderState},
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::{DeviceSnapshot, RgbState};
use crate::controls::{SliderRole, sync_input, sync_slider, to_units};
use crate::design::{DISPLAY_FONT, KEY_FONT, dp, mix, tabular, tokens};
use crate::draft::{ChangeId, EditKey, ZoneDraft};
use crate::icons::effect_icon_path;
use crate::key_matrix::{self, BOARD_WIDTH, KeyContent, KeyFace, KeyTone};
use crate::keymap::KeyLayout;
use crate::widgets::{panel, panel_head, panel_title, segment, segmented};

/// Preset colors, three to a row. LED colors are data, not theme.
mod mouse_art;

use mouse_art::{Light, MousePart, ZoneArt};

const SWATCHES: [(&str, RgbColor); 12] = [
    ("Red", RgbColor::new(255, 0, 0)),
    ("Orange", RgbColor::new(255, 110, 0)),
    ("Yellow", RgbColor::new(255, 220, 0)),
    ("Lime", RgbColor::new(130, 255, 0)),
    ("Green", RgbColor::new(0, 255, 60)),
    ("Teal", RgbColor::new(0, 200, 190)),
    ("Blue", RgbColor::new(0, 90, 255)),
    ("Indigo", RgbColor::new(60, 40, 255)),
    ("Purple", RgbColor::new(140, 0, 255)),
    ("Magenta", RgbColor::new(255, 0, 200)),
    ("Pink", RgbColor::new(255, 90, 140)),
    ("White", RgbColor::new(255, 255, 255)),
];

/// Effects per row in the effect list.
const EFFECT_COLUMNS: usize = 3;
const DEFAULT_SPEED: u8 = 128;
const LAMP_WIDTH: f32 = 256.0;
const MIN_LAMP_WIDTH: f32 = 220.0;
const COLOR_WIDTH: f32 = 352.0;
const PAD_SIZE: (f32, f32) = (206.0, 122.0);
const GAP: f32 = 16.0;
const MIN_FIGURE_PANEL_WIDTH: f32 = 360.0;

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

pub(crate) fn effect_label(kind: RgbEffectKind) -> &'static str {
    match kind {
        RgbEffectKind::Off => "Off",
        RgbEffectKind::Static => "Single color",
        RgbEffectKind::Breathing => "Breathing",
        RgbEffectKind::SpectrumCycle => "Spectrum cycle",
        RgbEffectKind::Wave => "Wave",
        RgbEffectKind::Reactive => "Reactive",
        RgbEffectKind::Preset => "Preset",
    }
}

/// A vendor effect's name as shown: plain words in sentence case, the same on every brand.
pub(crate) fn effect_name(name: &str) -> SharedString {
    match name {
        "Close" => "Off".into(),
        "Single Color Constant" | "Solid Color" => "Single color".into(),
        name => sentence_case(name),
    }
}

fn sentence_case(name: &str) -> SharedString {
    name.split(' ')
        .enumerate()
        .map(|(ix, word)| {
            let is_acronym = word.len() > 1 && !word.chars().any(char::is_lowercase);
            if is_acronym {
                return word.to_owned();
            }
            let mut chars = word.chars();
            match chars.next() {
                Some(first) if ix == 0 => {
                    format!("{first}{}", chars.as_str().to_lowercase())
                }
                _ => word.to_lowercase(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .into()
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

    /// Stages the zone's effect and brightness as one unsaved edit.
    fn write_zone(&mut self, effect: RgbEffect, brightness: Brightness, cx: &mut Context<Self>) {
        let Some((_, state)) = self.active_zone() else {
            return;
        };
        let zone = state.zone.value();
        self.edit_draft(
            EditKey::Change(ChangeId::Lighting(zone)),
            false,
            move |draft, _| {
                draft.rgb.insert(zone, ZoneDraft { effect, brightness });
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
    /// Whether any preset on the device has a full color mode, so the switch is worth showing.
    offers_colorful: bool,
}

fn zone_support(app: &AgnoPeriphApp, rgb: &RgbState, state: RgbZoneState) -> ZoneSupport {
    let preset = app.preset_info();
    ZoneSupport {
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
        offers_colorful: rgb
            .capabilities
            .zones
            .iter()
            .flat_map(|zone| &zone.presets)
            .any(|p| p.supports_colorful),
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    workspace_width: f32,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let (Some(rgb), Some((info, state))) = (device.rgb.as_ref(), app.active_zone()) else {
        return panel("rgb-panel", cx)
            .child(panel_head("Lighting"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("The lighting state couldn’t be read."),
            )
            .into_any_element();
    };
    let support = zone_support(app, rgb, state);
    let layout = board_layout(device);
    let figure = layout
        .is_none()
        .then(|| mouse_zones(device, rgb, state, cx))
        .flatten();
    let zones = (rgb.snapshot.zones.len() > 1).then(|| zone_switch(rgb, state, cx));
    // Beside a board the zone switch lives in the lamp; a drawing carries its own.
    let (lamp_title, lamp_zones, head_zones) = if figure.is_some() {
        (sentence_case(&info.name), None, zones)
    } else if layout.is_some() && zones.is_some() {
        ("Lamp".into(), zones, None)
    } else {
        (sentence_case(&info.name), zones, None)
    };
    let lamp = lamp_panel(app, &lamp_title, lamp_zones, &info, state, support, cx);
    let effects = effect_panel(app, rgb, &info, state, cx);
    let color = color_panel(app, state, support, cx);

    // A keyboard's board or a mouse's drawing takes the lamp beside it; otherwise the
    // lamp joins the row below.
    let picture = match (layout, figure) {
        (Some(layout), _) => Some((lit_board(app, rgb, &layout, state, cx), BOARD_WIDTH)),
        (None, Some(arts)) => Some((
            zones_panel(app, rgb, &arts, head_zones, cx),
            MIN_FIGURE_PANEL_WIDTH,
        )),
        (None, None) => None,
    };
    let (top, lamp) = match picture {
        Some((picture, width)) => {
            let top = if workspace_width >= width + GAP + MIN_LAMP_WIDTH {
                let side = if width == BOARD_WIDTH {
                    v_flex().flex_1().min_w_0()
                } else {
                    v_flex().w(dp(LAMP_WIDTH)).flex_shrink_0()
                };
                h_flex()
                    .items_stretch()
                    .gap(dp(GAP))
                    .child(picture)
                    .child(side.child(lamp))
            } else {
                v_flex().gap(dp(14.)).child(picture).child(lamp)
            };
            (Some(top), None)
        }
        None => (None, Some(lamp)),
    };

    v_flex()
        .id("rgb-panel")
        .gap(dp(14.))
        .children(top)
        .child(
            h_flex()
                .flex_wrap()
                .items_stretch()
                .gap(dp(GAP))
                .when_some(lamp, |this, lamp| {
                    this.child(v_flex().w(dp(LAMP_WIDTH)).child(lamp))
                })
                .child(v_flex().flex_1().min_w(dp(360.)).child(effects))
                .child(v_flex().w(dp(COLOR_WIDTH)).child(color)),
        )
        .into_any_element()
}

fn board_layout(device: &DeviceSnapshot) -> Option<std::sync::Arc<KeyLayout>> {
    if let Some(he) = device.hall_effect.as_ref() {
        return Some(KeyLayout::for_device(&device.info, &he.capabilities));
    }
    device.keymap.as_ref().map(|keymap| {
        std::sync::Arc::new(crate::keymap_panel::base_keycode_layout(
            &device.info,
            keymap,
        ))
    })
}

/// What the keys can be drawn as, given what the keyboard reports.
#[derive(Debug, Clone, Copy)]
enum Frame {
    Off,
    Solid(RgbColor),
    /// An effect that moves through colors, drawn as one still frame.
    Rainbow,
    /// Per-key colors are stored on the keyboard and aren't read.
    PerKey,
}

fn frame_of(effect: RgbEffect, preset_name: Option<&str>) -> Frame {
    let name = preset_name.map_or_else(String::new, str::to_lowercase);
    if matches!(effect, RgbEffect::Off) || name == "off" || name == "close" {
        return Frame::Off;
    }
    if name == "customization" {
        return Frame::PerKey;
    }
    let cycles = [
        "rainbow",
        "spectr",
        "colorful",
        "christmas",
        "cycle",
        "gradient",
    ]
    .iter()
    .any(|word| name.contains(word));
    let is_colorful = matches!(effect, RgbEffect::Preset { colorful: true, .. });
    match effect_color(effect) {
        Some(color) if !cycles && !is_colorful => Frame::Solid(color),
        _ => Frame::Rainbow,
    }
}

/// The keyboard drawn in its first zone's colors; further zones are not keys.
fn lit_board(
    app: &AgnoPeriphApp,
    rgb: &RgbState,
    layout: &KeyLayout,
    editing: RgbZoneState,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let state = rgb.snapshot.zones.first().copied().unwrap_or(editing);
    let info = rgb.capabilities.zones.iter().find(|z| z.id == state.zone);
    let preset_name = info.and_then(|info| match state.effect {
        RgbEffect::Preset { id, .. } => info.presets.iter().find(|p| p.id == id),
        _ => None,
    });
    let frame = frame_of(state.effect, preset_name.map(|p| p.name.as_str()));
    let strength = 0.35 + 0.65 * f32::from(state.brightness.percent()) / 100.0;
    let positions = layout.key_centers();
    let case = tokens(cx).case;

    let mut note = match frame {
        Frame::Off => "Lighting is off.".to_owned(),
        Frame::Solid(_) => "Keys show the effect’s color.".to_owned(),
        Frame::Rainbow => "Keys show a still frame of the effect.".to_owned(),
        Frame::PerKey => "Per-key colors stored on the keyboard aren’t shown yet.".to_owned(),
    };
    if editing.zone != state.zone
        && let Some(info) = info
    {
        note = format!("The board shows the {}. {note}", info.name.to_lowercase());
    }
    let mut lip = vec![div().child(note).into_any_element()];
    if has_pending_zones(app, rgb) {
        lip.push(div().flex_1().into_any_element());
        lip.push(not_applied_legend(cx));
    }

    let board = key_matrix::render_static(
        "rgb-board",
        "rgb-key",
        layout,
        |cap| {
            let color = match frame {
                Frame::Solid(color) => Some(color),
                Frame::Rainbow => {
                    let across = positions.get(&cap.index).copied().unwrap_or_default();
                    Some(hsv_to_rgb(across * 300.0, 0.8, 0.85))
                }
                Frame::Off | Frame::PerKey => None,
            };
            let content = color.map_or(KeyContent::Legend, |color| {
                KeyContent::Lit(mix(swatch_color(color).into(), case, strength))
            });
            KeyFace::new(content, KeyTone::Normal)
        },
        lip,
        cx,
    );
    v_flex().flex_shrink_0().child(board).into_any_element()
}

/// One segment per lighting zone.
fn zone_switch(rgb: &RgbState, state: RgbZoneState, cx: &mut Context<AgnoPeriphApp>) -> Div {
    let mut zones = segmented(cx);
    for zone_state in &rgb.snapshot.zones {
        let id = zone_state.zone;
        let name = rgb
            .capabilities
            .zones
            .iter()
            .find(|z| z.id == id)
            .map_or_else(
                || format!("Zone {}", id.value()).into(),
                |z| sentence_case(&z.name),
            );
        zones = zones.child(
            segment(
                Button::new(("rgb-zone", u64::from(id.value()))),
                name,
                id == state.zone,
                cx,
            )
            .flex_auto()
            .px(dp(9.))
            .on_click(cx.listener(move |this, _, _, cx| this.select_zone(id, cx))),
        );
    }
    zones
}

/// Whether any zone differs from what the device holds.
fn has_pending_zones(app: &AgnoPeriphApp, rgb: &RgbState) -> bool {
    app.selected_raw()
        .and_then(|d| d.rgb.as_ref())
        .is_some_and(|device| {
            device
                .snapshot
                .zones
                .iter()
                .any(|zone| !rgb.snapshot.zones.contains(zone))
        })
}

fn not_applied_legend(cx: &App) -> AnyElement {
    let swatch = div()
        .w(dp(14.))
        .h(dp(4.))
        .rounded_full()
        .bg(cx.theme().warning);
    key_matrix::legend_item(swatch, "Not applied").into_any_element()
}

/// What to draw for each zone of a mouse. `None` unless every zone has its own place on
/// the drawing.
fn mouse_zones(
    device: &DeviceSnapshot,
    rgb: &RgbState,
    editing: RgbZoneState,
    cx: &App,
) -> Option<Vec<ZoneArt>> {
    device.pointer.as_ref()?;
    let case = tokens(cx).case;
    let mut arts: Vec<ZoneArt> = Vec::new();
    for state in &rgb.snapshot.zones {
        let info = rgb.capabilities.zones.iter().find(|z| z.id == state.zone)?;
        let part = MousePart::of_zone(&info.name)?;
        if arts.iter().any(|art| art.part == part) {
            return None;
        }
        let preset_name = match state.effect {
            RgbEffect::Preset { id, .. } => info.presets.iter().find(|p| p.id == id),
            _ => None,
        };
        let strength = 0.35 + 0.65 * f32::from(state.brightness.percent()) / 100.0;
        let light = match frame_of(state.effect, preset_name.map(|p| p.name.as_str())) {
            Frame::Off | Frame::PerKey => Light::Off,
            Frame::Solid(color) => Light::Solid(mix(swatch_color(color).into(), case, strength)),
            Frame::Rainbow => Light::Cycling,
        };
        arts.push(ZoneArt {
            part,
            zone: state.zone,
            light,
            is_selected: state.zone == editing.zone,
        });
    }
    (!arts.is_empty()).then_some(arts)
}

/// The mouse drawing with its zone switch.
fn zones_panel(
    app: &AgnoPeriphApp,
    rgb: &RgbState,
    arts: &[ZoneArt],
    switch: Option<Div>,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let mut footer = Vec::new();
    if arts.iter().any(|art| art.light == Light::Cycling) {
        footer.push(
            div()
                .child("Moving effects are drawn as a still frame.")
                .into_any_element(),
        );
    }
    if has_pending_zones(app, rgb) {
        footer.push(div().flex_1().into_any_element());
        footer.push(not_applied_legend(cx));
    }
    let figure = mouse_art::figure(arts, cx);
    let muted = cx.theme().muted_foreground;
    panel("rgb-zones", cx)
        .flex_1()
        .child(panel_head("Zones").child(div().flex_1()).children(switch))
        .child(h_flex().justify_center().child(figure))
        .when(!footer.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap(dp(14.))
                    .text_size(dp(12.))
                    .text_color(muted)
                    .children(footer),
            )
        })
        .into_any_element()
}

/// Zone switch, brightness and speed. Unsupported controls are disabled with a reason.
fn lamp_panel(
    app: &AgnoPeriphApp,
    title: &str,
    zones: Option<Div>,
    info: &RgbZoneInfo,
    state: RgbZoneState,
    support: ZoneSupport,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy();
    let zones = zones.map(|zones| v_flex().mb(dp(4.)).child(zones));

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
    let brightness_row = labelled_slider(
        "Brightness",
        Some(format!("{brightness}%")),
        controls.map(|c| c.brightness.clone()),
        busy || is_off,
        cx,
    );
    let speed_row = labelled_slider(
        "Speed",
        support.speed.then(|| format!("{speed_pct}%")),
        controls.map(|c| c.speed.clone()),
        busy || !support.speed,
        cx,
    );
    let muted = cx.theme().muted_foreground;
    panel("rgb-adjust", cx)
        .size_full()
        .child(
            h_flex()
                .min_h(dp(26.))
                .mb(dp(10.))
                .child(panel_title(title)),
        )
        .children(zones)
        .child(brightness_row)
        .child(speed_row)
        .when_some(note, |this, note| {
            this.child(
                div()
                    .mt(dp(8.))
                    .text_size(dp(12.))
                    .text_color(muted)
                    .child(note),
            )
        })
        .into_any_element()
}

fn labelled_slider(
    label: &'static str,
    value: Option<String>,
    slider: Option<Entity<SliderState>>,
    disabled: bool,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    v_flex()
        .gap(dp(8.))
        .mt(dp(12.))
        .child(
            h_flex()
                .justify_between()
                .child(label)
                .when_some(value, |this, value| {
                    this.child(
                        div()
                            .font_family(DISPLAY_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_features(tabular())
                            .when(disabled, |this| this.text_color(theme.muted_foreground))
                            .child(value),
                    )
                }),
        )
        .when_some(slider, |this, slider| {
            this.child(
                Slider::new(&slider)
                    .disabled(disabled)
                    .accessibility_label(label),
            )
        })
}

/// One selectable effect: its id, name, whether it is showing, and what choosing it writes.
struct EffectChoice {
    id: SharedString,
    name: SharedString,
    is_current: bool,
    next: RgbEffect,
}

fn effect_choices(rgb: &RgbState, info: &RgbZoneInfo, state: RgbZoneState) -> Vec<EffectChoice> {
    let zone = state.zone.value();
    let color = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
    let speed = effect_speed(state.effect);
    let is_colorful = matches!(state.effect, RgbEffect::Preset { colorful: true, .. });
    if info.presets.is_empty() {
        return rgb
            .capabilities
            .supported_effects
            .iter()
            .map(|&kind| EffectChoice {
                id: format!("rgb-{zone}-effect-{kind:?}").into(),
                name: effect_label(kind).into(),
                is_current: state.effect.kind() == kind,
                next: effect_of_kind(kind, color, speed),
            })
            .collect();
    }
    info.presets
        .iter()
        .map(|preset| {
            let id = preset.id;
            EffectChoice {
                id: format!("rgb-{zone}-preset-{id}").into(),
                name: effect_name(&preset.name),
                is_current: match state.effect {
                    RgbEffect::Preset { id: current, .. } => current == id,
                    RgbEffect::Off => id == 0,
                    _ => false,
                },
                next: if id == 0 {
                    RgbEffect::Off
                } else {
                    RgbEffect::Preset {
                        id,
                        color,
                        speed,
                        colorful: is_colorful && preset.supports_colorful,
                    }
                },
            }
        })
        .collect()
}

/// The effect list, three to a row with each name in full.
fn effect_panel(
    app: &AgnoPeriphApp,
    rgb: &RgbState,
    info: &RgbZoneInfo,
    state: RgbZoneState,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy();
    let brightness = state.brightness;
    let mut grid = v_flex().gap(dp(2.));
    let mut choices = effect_choices(rgb, info, state).into_iter().peekable();
    while choices.peek().is_some() {
        let mut row = h_flex().items_stretch().gap(dp(8.));
        for _ in 0..EFFECT_COLUMNS {
            row = match choices.next() {
                Some(choice) => row.child(effect_button(choice, brightness, busy, cx)),
                // Keeps the last row's columns in line with the rows above.
                None => row.child(div().flex_1().min_w_0()),
            };
        }
        grid = grid.child(row);
    }
    panel(("rgb-effects", u64::from(state.zone.value())), cx)
        .size_full()
        .child(panel_head("Effect"))
        .child(grid)
        .into_any_element()
}

fn effect_button(
    choice: EffectChoice,
    brightness: Brightness,
    busy: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let EffectChoice {
        id,
        name,
        is_current,
        next,
    } = choice;
    let theme = cx.theme();
    let icon_color = if is_current {
        theme.primary
    } else {
        theme.muted_foreground
    };
    let text_color = if is_current {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    let fill = tokens(cx).accent_soft;
    Button::new(id)
        .ghost()
        .selected(is_current)
        .flex_1()
        .min_w_0()
        .h_auto()
        .min_h(dp(28.))
        .px(dp(8.))
        .py(dp(4.))
        .when(is_current, |this| this.bg(fill))
        .accessibility_label(name.clone())
        .disabled(busy)
        .child(
            h_flex()
                .w_full()
                .gap(dp(8.))
                .child(
                    svg()
                        .path(effect_icon_path(&name))
                        .size(dp(16.))
                        .flex_shrink_0()
                        .text_color(icon_color),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .text_left()
                        .text_size(dp(13.))
                        .line_height(relative(1.25))
                        .text_color(text_color)
                        .when(is_current, |this| this.font_weight(FontWeight::SEMIBOLD))
                        .child(name),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if !is_current {
                this.write_zone(next, brightness, cx);
            }
        }))
        .into_any_element()
}

/// Speed as a 0–100 % readout of the zone's own range. Firmware defaults can sit above a
/// zone's maximum, so the result is clamped.
fn speed_percent(speed: u8, max_speed: u8) -> u16 {
    let max = u32::from(max_speed.max(1));
    let pct = (u32::from(speed) * 100 + max / 2) / max;
    u16::try_from(pct.min(100)).unwrap_or(100)
}

/// Saturation/value pad with hue strip and swatches, then a HEX or RGB entry row and the
/// full color mode switch.
fn color_panel(
    app: &AgnoPeriphApp,
    state: RgbZoneState,
    support: ZoneSupport,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let busy = app.is_busy();
    let current = effect_color(state.effect).unwrap_or(RgbColor::WHITE);
    let shown = app
        .lighting
        .hsv
        .map_or(current, |(h, s, v)| hsv_to_rgb(h, s, v));
    let is_enabled = support.color && !support.is_colorful && !busy;
    let hint = if support.is_colorful {
        "Full color mode cycles the spectrum."
    } else if !support.color {
        "This effect picks its own colors."
    } else {
        "Used for the whole zone"
    };

    let colorful = support.offers_colorful.then(|| {
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
            }))
    });
    let has_switch = colorful.is_some();

    let pad = sv_pad(app, shown, is_enabled, cx);
    let strip = hue_strip(app, is_enabled, cx);
    let swatches = swatch_grid(current, is_enabled, cx);
    let entry = color_entry(app, shown, is_enabled, cx);

    let muted = cx.theme().muted_foreground;
    let hint = div().text_size(dp(12.)).text_color(muted).child(hint);
    let (head_note, foot_note) = if has_switch {
        (None, Some(hint.mt(dp(8.))))
    } else {
        (Some(hint), None)
    };

    panel("rgb-color", cx)
        .size_full()
        .child(
            h_flex()
                .gap_2()
                .min_h(dp(26.))
                .mb(dp(10.))
                .child(panel_title("Color"))
                .child(div().flex_1())
                .children(head_note)
                .children(colorful),
        )
        .child(
            h_flex()
                .items_start()
                .gap(dp(14.))
                .when(!is_enabled, |this| this.opacity(0.4))
                .child(v_flex().gap(dp(10.)).child(pad).child(strip))
                .child(swatches),
        )
        .child(entry)
        .children(foot_note)
        .into_any_element()
}

fn picker_thumb(cx: &App) -> Div {
    div()
        .absolute()
        .size(dp(12.))
        .ml(dp(-6.))
        .mt(dp(-6.))
        .rounded_full()
        .border_2()
        .border_color(cx.theme().background)
        .shadow_sm()
}

// The pad and strip draw LED color space itself: their colors are data, not theme.
fn sv_pad(
    app: &AgnoPeriphApp,
    shown: RgbColor,
    is_enabled: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let (h, s, v) = app.lighting.hsv.unwrap_or_else(|| rgb_to_hsv(shown));
    let radius = cx.theme().radius;
    let white = gpui_kit::white();
    let black = gpui_kit::black();
    div()
        .id("rgb-sv-pad")
        .relative()
        .w(dp(PAD_SIZE.0))
        .h(dp(PAD_SIZE.1))
        .rounded(radius)
        .bg(swatch_color(hsv_to_rgb(h, 1.0, 1.0)))
        .when(is_enabled, Styled::cursor_crosshair)
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(radius)
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
                .rounded(radius)
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(black.opacity(0.), 0.),
                    linear_color_stop(black, 1.),
                )),
        )
        .child(
            picker_thumb(cx)
                .left(relative(s.clamp(0.0, 1.0)))
                .top(relative((1.0 - v).clamp(0.0, 1.0)))
                .bg(swatch_color(shown)),
        )
        .child(picker_surface(
            PickerPart::SaturationValue,
            is_enabled,
            cx.entity().downgrade(),
        ))
        .test_support()
        .into_any_element()
}

fn hue_strip(app: &AgnoPeriphApp, is_enabled: bool, cx: &mut Context<AgnoPeriphApp>) -> AnyElement {
    let current = app
        .active_zone()
        .and_then(|(_, state)| effect_color(state.effect))
        .unwrap_or(RgbColor::WHITE);
    let (h, _, _) = app.lighting.hsv.unwrap_or_else(|| rgb_to_hsv(current));
    div()
        .id("rgb-hue-strip")
        .relative()
        .w(dp(PAD_SIZE.0))
        .h(dp(10.))
        .rounded_full()
        .when(is_enabled, Styled::cursor_crosshair)
        .child(
            h_flex()
                .size_full()
                .rounded_full()
                .overflow_hidden()
                .children((0..6u8).map(|part| {
                    let from = f32::from(part) * 60.0;
                    div().flex_1().h_full().bg(linear_gradient(
                        90.,
                        linear_color_stop(swatch_color(hsv_to_rgb(from, 1.0, 1.0)), 0.),
                        linear_color_stop(swatch_color(hsv_to_rgb(from + 60.0, 1.0, 1.0)), 1.),
                    ))
                })),
        )
        .child(
            picker_thumb(cx)
                .top(relative(0.5))
                .left(relative((h / 360.0).clamp(0.0, 1.0)))
                .bg(swatch_color(hsv_to_rgb(h, 1.0, 1.0))),
        )
        .child(picker_surface(
            PickerPart::Hue,
            is_enabled,
            cx.entity().downgrade(),
        ))
        .test_support()
        .into_any_element()
}

fn swatch_grid(current: RgbColor, is_enabled: bool, cx: &mut Context<AgnoPeriphApp>) -> Div {
    let theme = cx.theme();
    let (ring, edge) = (theme.foreground, theme.border);
    let (radius, ring_radius) = (theme.radius, theme.radius_lg);
    let clear = theme.transparent;
    let mut grid = v_flex().flex_shrink_0().gap(dp(3.));
    for row in SWATCHES.chunks(3) {
        let mut line = h_flex().gap(dp(3.));
        for &(name, color) in row {
            let fill: Hsla = swatch_color(color).into();
            let is_current = color == current;
            let button = Button::new(SharedString::from(format!("rgb-swatch-{name}")))
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(fill)
                        .hover(fill)
                        .active(fill),
                )
                .selected(is_current)
                .disabled(!is_enabled)
                .size(dp(24.))
                .rounded(radius)
                .border_1()
                .border_color(edge)
                .bg(fill)
                .accessibility_label(format!("{name} {}", rgb_to_hex(color)))
                .tooltip(name)
                .on_click(cx.listener(move |this, _, _, cx| this.set_zone_color(color, cx)));
            line = line.child(
                div()
                    .p(dp(1.))
                    .rounded(ring_radius)
                    .border_2()
                    .border_color(if is_current { ring } else { clear })
                    .child(button),
            );
        }
        grid = grid.child(line);
    }
    grid
}

/// Preview, the HEX field or R/G/B steppers, and the switch between the two.
fn color_entry(
    app: &AgnoPeriphApp,
    shown: RgbColor,
    is_enabled: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> Div {
    let format = app.lighting.format;
    let mut formats = segmented(cx);
    for (id, label, kind) in [
        ("rgb-format-hex", "HEX", ColorFormat::Hex),
        ("rgb-format-rgb", "RGB", ColorFormat::Rgb),
    ] {
        formats = formats.child(
            segment(Button::new(id), label, format == kind, cx)
                .px(dp(10.))
                .on_click(cx.listener(move |this, _, _, cx| this.set_color_format(kind, cx))),
        );
    }
    let hex = app
        .controls
        .as_ref()
        .map(|c| c.hex.clone())
        .filter(|_| format == ColorFormat::Hex);
    let channels = (format == ColorFormat::Rgb).then(|| {
        h_flex().justify_between().children(
            [("R", 0usize, shown.r), ("G", 1, shown.g), ("B", 2, shown.b)]
                .into_iter()
                .map(|(label, channel, level)| {
                    channel_stepper(label, channel, level, is_enabled, cx)
                }),
        )
    });
    let edge = cx.theme().border;
    v_flex()
        .gap(dp(8.))
        .mt(dp(12.))
        .child(
            h_flex()
                .gap(dp(8.))
                .child(
                    div()
                        .id("rgb-preview")
                        .flex_shrink_0()
                        .size(dp(28.))
                        .rounded(dp(7.))
                        .border_1()
                        .border_color(edge)
                        .bg(swatch_color(shown))
                        .when(!is_enabled, |this| this.opacity(0.4))
                        .test_support(),
                )
                .when_some(hex, |this, hex| {
                    this.child(
                        div()
                            .w(dp(110.))
                            .font_family(KEY_FONT)
                            .child(Input::new(&hex).small().disabled(!is_enabled)),
                    )
                })
                .child(div().flex_1())
                .child(formats),
        )
        .children(channels)
}

fn channel_stepper(
    label: &'static str,
    channel: usize,
    level: u8,
    is_enabled: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let decrease = Button::new(("rgb-channel-dec", channel))
        .accessibility_label(format!("Decrease {label}"))
        .disabled(!is_enabled || level == 0)
        .on_click(cx.listener(move |this, _, _, cx| this.step_channel(channel, -5, cx)));
    let increase = Button::new(("rgb-channel-inc", channel))
        .accessibility_label(format!("Increase {label}"))
        .disabled(!is_enabled || level == 255)
        .on_click(cx.listener(move |this, _, _, cx| this.step_channel(channel, 5, cx)));
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap(dp(3.))
        .child(div().text_size(dp(11.5)).text_color(muted).child(label))
        .child(crate::widgets::stepper_sized(
            decrease,
            level.to_string(),
            "",
            increase,
            24.,
            cx,
        ))
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
