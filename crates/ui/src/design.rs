//! The design layer: bundled fonts, the app's light and dark themes with their accent
//! presets, and the colour roles gpui-kit's theme has no slot for.
//!
//! This is the only place in the UI with raw colour values (LED colours are device data).

use std::borrow::Cow;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode};
use gpui_kit::*;
use serde::{Deserialize, Serialize};

/// Everything that isn't a title, a legend or a big number.
pub(crate) const BODY_FONT: &str = "Figtree";
/// Device names, readouts and numbers.
pub(crate) const DISPLAY_FONT: &str = "Saira";
/// Panel titles. The wider cut and the tracking are part of the font file.
pub(crate) const TITLE_FONT: &str = "Saira Title";
/// Keycap legends and values: the narrow cut.
pub(crate) const KEY_FONT: &str = "Saira Keys";

const FONTS: [&[u8]; 9] = [
    include_bytes!("../assets/fonts/figtree-regular.ttf"),
    include_bytes!("../assets/fonts/figtree-medium.ttf"),
    include_bytes!("../assets/fonts/figtree-semibold.ttf"),
    include_bytes!("../assets/fonts/figtree-bold.ttf"),
    include_bytes!("../assets/fonts/saira-medium.ttf"),
    include_bytes!("../assets/fonts/saira-semibold.ttf"),
    include_bytes!("../assets/fonts/saira-bold.ttf"),
    include_bytes!("../assets/fonts/saira-title.ttf"),
    include_bytes!("../assets/fonts/saira-keys.ttf"),
];

/// A length from the design mockup, which is drawn at the default 16 px base size.
pub(crate) fn dp(at_default_zoom: f32) -> Rems {
    rems(at_default_zoom / 16.0)
}

/// Digits that all take the same width, for values that change in place.
pub(crate) fn tabular() -> FontFeatures {
    static TABULAR: LazyLock<Arc<Vec<(String, u32)>>> =
        LazyLock::new(|| Arc::new(vec![("tnum".to_owned(), 1)]));
    FontFeatures(TABULAR.clone())
}

/// The accent colour used for selections, switches and Apply.
///
/// Orange and red are not offered: they already mean "not applied" and "destructive".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Accent {
    #[default]
    Cyan,
    Violet,
    Green,
    Pink,
    Gold,
}

impl Accent {
    pub const ALL: [Self; 5] = [
        Self::Cyan,
        Self::Violet,
        Self::Green,
        Self::Pink,
        Self::Gold,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cyan => "Cyan",
            Self::Violet => "Violet",
            Self::Green => "Green",
            Self::Pink => "Pink",
            Self::Gold => "Gold",
        }
    }

    /// The accent and the text colour drawn on top of it.
    fn colors(self, mode: ThemeMode) -> (u32, u32) {
        if mode.is_dark() {
            match self {
                Self::Cyan => (0x47b4_ffff, 0x0412_1fff),
                Self::Violet => (0xa98b_ffff, 0x120a_2aff),
                Self::Green => (0x3ddc_84ff, 0x0417_0cff),
                Self::Pink => (0xff6f_b5ff, 0x2a06_17ff),
                Self::Gold => (0xf5c5_42ff, 0x1f16_00ff),
            }
        } else {
            let accent = match self {
                Self::Cyan => 0x0067_b3ff,
                Self::Violet => 0x6a3f_d0ff,
                Self::Green => 0x0e73_42ff,
                Self::Pink => 0xb416_5fff,
                Self::Gold => 0x8a62_00ff,
            };
            (accent, 0xffff_ffff)
        }
    }

    /// This preset's colour in `mode`, for the picker in Settings.
    pub(crate) fn swatch(self, mode: ThemeMode) -> Hsla {
        color(self.colors(mode).0)
    }
}

/// The theme mode a fresh install starts in.
pub(crate) fn default_mode() -> ThemeMode {
    ThemeMode::Dark
}

/// Surface and text roles per mode, as `0xRRGGBBAA`.
struct Palette {
    background: u32,
    chrome: u32,
    sidebar: u32,
    panel: u32,
    raised: u32,
    hover: u32,
    popover: u32,
    line: u32,
    line_strong: u32,
    tick: u32,
    foreground: u32,
    muted: u32,
    faint: u32,
    case: u32,
    case_highlight: u32,
    case_shadow: u32,
    case_text: u32,
    key_skirt: u32,
    key_face: u32,
    key_face_hover: u32,
    key_highlight: u32,
    key_legend: u32,
    key_sub: u32,
    key_track: u32,
    thumb: u32,
    switch_off: u32,
    attention: u32,
    danger: u32,
    accent_soft_alpha: u8,
}

const DARK: Palette = Palette {
    background: 0x1517_1cff,
    chrome: 0x1113_17ff,
    sidebar: 0x191c_21ff,
    panel: 0x1d20_26ff,
    raised: 0x262a_31ff,
    hover: 0x2d31_39ff,
    popover: 0x2327_2eff,
    line: 0x272b_32ff,
    line_strong: 0x383d_46ff,
    tick: 0x3d42_4bff,
    foreground: 0xe8ea_eeff,
    muted: 0x969d_aaff,
    faint: 0x6a71_7dff,
    case: 0x0e10_13ff,
    case_highlight: 0xffff_ff0d,
    case_shadow: 0x0000_00b3,
    case_text: 0x7b82_8eff,
    key_skirt: 0x1b1e_24ff,
    key_face: 0x2a2e_36ff,
    key_face_hover: 0x3237_41ff,
    key_highlight: 0xffff_ff0f,
    key_legend: 0xe2e5_eaff,
    key_sub: 0x8a91_9dff,
    key_track: 0xffff_ff12,
    thumb: 0xeef0_f3ff,
    switch_off: 0x3a3f_48ff,
    attention: 0xff8a_3dff,
    danger: 0xff7a_7aff,
    accent_soft_alpha: 0x21,
};

const LIGHT: Palette = Palette {
    background: 0xecee_f2ff,
    chrome: 0xf7f8_faff,
    sidebar: 0xf4f5_f8ff,
    panel: 0xffff_ffff,
    raised: 0xe1e4_eaff,
    hover: 0xd7db_e2ff,
    popover: 0xffff_ffff,
    line: 0xdde0_e6ff,
    line_strong: 0xc8cd_d5ff,
    tick: 0xb4ba_c4ff,
    foreground: 0x1618_1dff,
    muted: 0x5b62_70ff,
    faint: 0x7c83_90ff,
    case: 0xcfd4_dcff,
    case_highlight: 0xffff_ffb3,
    case_shadow: 0x1e28_3c59,
    case_text: 0x4f56_64ff,
    key_skirt: 0xd6da_e1ff,
    key_face: 0xffff_ffff,
    key_face_hover: 0xf6f7_f9ff,
    key_highlight: 0xffff_ffe6,
    key_legend: 0x1b1e_24ff,
    key_sub: 0x6b72_80ff,
    key_track: 0x0000_0012,
    thumb: 0xffff_ffff,
    switch_off: 0xc5ca_d3ff,
    attention: 0xa84a_00ff,
    danger: 0xcf34_34ff,
    accent_soft_alpha: 0x1c,
};

fn palette(mode: ThemeMode) -> &'static Palette {
    if mode.is_dark() { &DARK } else { &LIGHT }
}

fn color(rgba_hex: u32) -> Hsla {
    rgba(rgba_hex).into()
}

/// Colour roles the gpui-kit theme has no field for, resolved for the active theme.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tokens {
    pub(crate) panel: Hsla,
    pub(crate) raised: Hsla,
    pub(crate) hover: Hsla,
    pub(crate) line_strong: Hsla,
    pub(crate) tick: Hsla,
    pub(crate) faint: Hsla,
    pub(crate) accent_soft: Hsla,
    pub(crate) case: Hsla,
    pub(crate) case_highlight: Hsla,
    pub(crate) case_shadow: Hsla,
    pub(crate) case_text: Hsla,
    pub(crate) key_skirt: Hsla,
    pub(crate) key_face: Hsla,
    pub(crate) key_face_hover: Hsla,
    pub(crate) key_highlight: Hsla,
    pub(crate) key_legend: Hsla,
    pub(crate) key_sub: Hsla,
    pub(crate) key_track: Hsla,
}

pub(crate) fn tokens(cx: &App) -> Tokens {
    let theme = cx.theme();
    let palette = palette(theme.mode);
    Tokens {
        panel: color(palette.panel),
        raised: color(palette.raised),
        hover: color(palette.hover),
        line_strong: color(palette.line_strong),
        tick: color(palette.tick),
        faint: color(palette.faint),
        accent_soft: theme
            .primary
            .opacity(f32::from(palette.accent_soft_alpha) / 255.0),
        case: color(palette.case),
        case_highlight: color(palette.case_highlight),
        case_shadow: color(palette.case_shadow),
        case_text: color(palette.case_text),
        key_skirt: color(palette.key_skirt),
        key_face: color(palette.key_face),
        key_face_hover: color(palette.key_face_hover),
        key_highlight: color(palette.key_highlight),
        key_legend: color(palette.key_legend),
        key_sub: color(palette.key_sub),
        key_track: color(palette.key_track),
    }
}

/// `share` of `top` over the rest of `bottom`, mixed in sRGB like CSS `color-mix`.
pub(crate) fn mix(top: Hsla, bottom: Hsla, share: f32) -> Hsla {
    let top = top.to_rgb();
    let bottom = bottom.to_rgb();
    let share = share.clamp(0.0, 1.0);
    let lerp = |a: f32, b: f32| a * share + b * (1.0 - share);
    Rgba {
        r: lerp(top.r, bottom.r),
        g: lerp(top.g, bottom.g),
        b: lerp(top.b, bottom.b),
        a: lerp(top.a, bottom.a),
    }
    .into()
}

/// A legible legend colour on top of a lit keycap of `fill`.
pub(crate) fn legend_on(fill: Hsla) -> Hsla {
    let rgb = fill.to_rgb();
    let linear = |c: f32| {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b);
    if luminance > 0.32 {
        color(0x0b0d_10ff)
    } else {
        color(0xffff_ffff)
    }
}

/// A soft coloured halo, used for the actuation line, lit keys and the mouse underglow.
pub(crate) fn glow(color: Hsla, radius: f32) -> BoxShadow {
    // Shadow geometry is raster-space, so it takes pixels rather than the rem scale.
    BoxShadow::new(px(0.), px(0.), color).blur_radius(px(radius))
}

#[derive(Debug)]
struct FontsLoaded;

impl Global for FontsLoaded {}

/// Registers the bundled fonts once per process.
pub(crate) fn init_fonts(cx: &mut App) {
    if cx.has_global::<FontsLoaded>() {
        return;
    }
    let fonts = FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("failed to load the bundled fonts: {err}");
    }
    cx.set_global(FontsLoaded);
}

fn theme_config(mode: ThemeMode, accent: Accent) -> ThemeConfig {
    let palette = palette(mode);
    let (primary, primary_foreground) = accent.colors(mode);
    let white = color(0xffff_ffff);
    let hex = |value: u32| Some(SharedString::from(format!("#{value:08x}")));
    let mixed = |value: Hsla| {
        let rgb = value.to_rgb();
        let byte = |c: f32| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let byte = (c.clamp(0.0, 1.0) * 255.0).round() as u32;
            byte
        };
        hex(byte(rgb.r) << 24 | byte(rgb.g) << 16 | byte(rgb.b) << 8 | byte(rgb.a))
    };

    let mut config = ThemeConfig::default();
    config.name = if mode.is_dark() {
        "AgnoPeriph Dark".into()
    } else {
        "AgnoPeriph Light".into()
    };
    config.mode = mode;
    config.font_family = Some(BODY_FONT.into());
    config.radius = Some(7);
    config.radius_lg = Some(12);
    config.shadow = Some(false);

    let colors = &mut config.colors;
    colors.background = hex(palette.background);
    colors.foreground = hex(palette.foreground);
    colors.border = hex(palette.line);
    colors.input = hex(palette.line_strong);
    colors.ring = hex(primary);
    colors.caret = hex(palette.foreground);
    colors.muted = hex(palette.raised);
    colors.muted_foreground = hex(palette.muted);
    colors.primary = hex(primary);
    colors.primary_foreground = hex(primary_foreground);
    colors.primary_hover = mixed(mix(white, color(primary), 0.08));
    colors.primary_active = mixed(mix(white, color(primary), 0.14));
    colors.secondary = hex(palette.raised);
    colors.secondary_foreground = hex(palette.foreground);
    colors.secondary_hover = hex(palette.hover);
    colors.secondary_active = hex(palette.hover);
    colors.accent = hex(palette.hover);
    colors.accent_foreground = hex(palette.foreground);
    colors.popover = hex(palette.popover);
    colors.popover_foreground = hex(palette.foreground);
    colors.group_box = hex(palette.panel);
    colors.group_box_foreground = hex(palette.foreground);
    colors.sidebar = hex(palette.sidebar);
    colors.sidebar_foreground = hex(palette.foreground);
    colors.sidebar_border = hex(palette.line);
    colors.sidebar_accent = hex(palette.raised);
    colors.sidebar_accent_foreground = hex(palette.foreground);
    colors.title_bar = hex(palette.chrome);
    colors.title_bar_border = hex(palette.line);
    colors.switch = hex(palette.switch_off);
    colors.switch_thumb = hex(0xffff_ffff);
    colors.slider_bar = hex(primary);
    colors.slider_thumb = hex(palette.thumb);
    colors.tab_foreground = hex(palette.muted);
    colors.tab_active_foreground = hex(palette.foreground);
    colors.danger = hex(palette.danger);
    colors.danger_foreground = hex(palette.background);
    colors.warning = hex(palette.attention);
    colors.warning_foreground = hex(palette.background);
    colors.scrollbar_thumb = hex(palette.line_strong);
    colors.scrollbar_thumb_hover = hex(palette.tick);
    config
}

/// Registers the app's light and dark themes in `accent` and switches to `mode`.
///
/// The configs are registered rather than patched onto the live theme, so
/// [`Theme::change`] (the title bar toggle, tests) keeps the palette.
pub(crate) fn apply(mode: ThemeMode, accent: Accent, cx: &mut App) {
    init_fonts(cx);
    let theme = Theme::global_mut(cx);
    theme.light_theme = Rc::new(theme_config(ThemeMode::Light, accent));
    theme.dark_theme = Rc::new(theme_config(ThemeMode::Dark, accent));
    Theme::change(mode, None, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn mix_keeps_the_endpoints() {
        let black = color(0x0000_00ff);
        let white = color(0xffff_ffff);
        assert_eq!(mix(white, black, 1.0).to_rgb(), white.to_rgb());
        assert_eq!(mix(white, black, 0.0).to_rgb(), black.to_rgb());
    }

    #[::core::prelude::v1::test]
    fn legends_contrast_with_the_lit_key() {
        assert_eq!(legend_on(color(0xffff_ffff)), color(0x0b0d_10ff));
        assert_eq!(legend_on(color(0x4c5b_ffff)), color(0xffff_ffff));
    }

    fn luminance(value: u32) -> f32 {
        let rgb = color(value).to_rgb();
        let linear = |c: f32| {
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
    }

    #[::core::prelude::v1::test]
    fn light_accents_are_readable_as_text() {
        for accent in Accent::ALL {
            let (value, _) = accent.colors(ThemeMode::Light);
            let ratio = (luminance(LIGHT.background) + 0.05) / (luminance(value) + 0.05);
            assert!(ratio >= 4.5, "{} contrast {ratio:.2}", accent.label());
        }
    }
}
