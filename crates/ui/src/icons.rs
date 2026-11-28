//! Embedded SVG icon assets for AgnoPeriph, composing custom advanced-key SVGs
//! on top of `gpui_kit::assets::Assets`.

use std::borrow::Cow;

use gpui_kit::*;

/// One of the five advanced / paired key SVG glyphs provided in `assets/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdvancedIcon {
    Rs,
    Socd,
    Dks,
    Mt,
    Tgl,
}

impl AdvancedIcon {
    /// Parses an owner tag (`"RS"`, `"SOCD"`, `"DKS"`, `"MT"`, `"TGL"`) into its icon.
    pub fn from_owner(owner: &str) -> Option<Self> {
        match owner {
            "RS" => Some(Self::Rs),
            "SOCD" => Some(Self::Socd),
            "DKS" => Some(Self::Dks),
            "MT" => Some(Self::Mt),
            "TGL" => Some(Self::Tgl),
            _ => None,
        }
    }

    pub const fn asset_path(self) -> &'static str {
        match self {
            Self::Rs => "icons/advanced/rs.svg",
            Self::Socd => "icons/advanced/socd.svg",
            Self::Dks => "icons/advanced/dks.svg",
            Self::Mt => "icons/advanced/mt.svg",
            Self::Tgl => "icons/advanced/tgl.svg",
        }
    }
}

/// Renders one of the custom advanced-key SVGs at `icon_size` tinted with `color`.
pub fn advanced_svg(icon: AdvancedIcon, icon_size: Rems, color: Hsla) -> Svg {
    svg()
        .path(icon.asset_path())
        .size(icon_size)
        .flex_shrink_0()
        .text_color(color)
}

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Pencil,
        Keyboard,
        Download,
        Upload,
        SlidersHorizontal,
        Ellipsis,
        FileText,
        Plug,
        Unplug,
        Wifi,
        Bluetooth,
        Mouse,
        Gamepad2,
        LocateFixed,
        MousePointer2,
        ArrowDown,
        ArrowUp,
        ArrowDownUp
    ]
);

macro_rules! effect_icons {
    ($($slug:literal),* $(,)?) => {
        [$((
            concat!("icons/effects/", $slug, ".svg"),
            include_bytes!(concat!("../assets/icons/effects/", $slug, ".svg")).as_slice(),
        )),*]
    };
}

/// Line icons for lighting effects, named after the effect.
const EFFECT_ICONS: [(&str, &[u8]); 23] = effect_icons!(
    "christmas",
    "colorful-swirls",
    "crucifix",
    "customization",
    "digital-rain",
    "double-vortex",
    "fingertip-rainbow",
    "fluttering-ribbons",
    "focus",
    "laser",
    "left-right-rainbow",
    "monochrome-breath",
    "off",
    "pinwheel",
    "reactive",
    "single-color",
    "single-light-up",
    "spectral-cycle",
    "spectrum-breathing",
    "spectrum-float",
    "up-down-rainbow",
    "vortex",
    "wave",
);

/// The icon for a lighting effect shown as `name`.
pub fn effect_icon_path(name: &str) -> &'static str {
    let slug = name.to_lowercase().replace(' ', "-");
    let slug = match slug.as_str() {
        "breathing" => "monochrome-breath",
        "spectrum-cycle" | "cycle-all" => "spectral-cycle",
        "gradient-up-down" | "cycle-up-down" => "up-down-rainbow",
        "gradient-left-right" | "cycle-left-right" => "left-right-rainbow",
        "band-sat" => "fluttering-ribbons",
        "rainbow-moving-chevron" | "streamer" => "wave",
        "always-on" => "single-color",
        other => other,
    };
    let find = |slug: &str| {
        EFFECT_ICONS
            .iter()
            .map(|(path, _)| *path)
            .find(|path| path.strip_suffix(".svg").and_then(|p| p.rsplit('/').next()) == Some(slug))
    };
    find(slug)
        .or_else(|| find("customization"))
        .unwrap_or_default()
}

macro_rules! mouse_layers {
    ($($name:literal),* $(,)?) => {
        [$((
            concat!("icons/mouse/", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/mouse/", $name, ".svg")).as_slice(),
        )),*]
    };
}

/// The layers of the mouse drawing on the Lighting tab.
const MOUSE_LAYERS: [(&str, &[u8]); 6] =
    mouse_layers!("body", "rim", "glow", "rim-rainbow", "glow-rainbow", "seam");

/// Asset source combining AgnoPeriph's embedded custom SVGs with GPUI Kit's Lucide icons.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

const CUSTOM_ICONS: [(&str, &[u8]); 5] = [
    (
        AdvancedIcon::Rs.asset_path(),
        include_bytes!("../assets/icons/rs.svg"),
    ),
    (
        AdvancedIcon::Socd.asset_path(),
        include_bytes!("../assets/icons/socd.svg"),
    ),
    (
        AdvancedIcon::Dks.asset_path(),
        include_bytes!("../assets/icons/dks.svg"),
    ),
    (
        AdvancedIcon::Mt.asset_path(),
        include_bytes!("../assets/icons/mt.svg"),
    ),
    (
        AdvancedIcon::Tgl.asset_path(),
        include_bytes!("../assets/icons/tgl.svg"),
    ),
];

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = CUSTOM_ICONS
            .iter()
            .chain(&EFFECT_ICONS)
            .chain(&MOUSE_LAYERS)
            .find(|(p, _)| *p == path)
        {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = gpui_kit::assets::Assets.list(path)?;
        out.extend(ExtraIcons.list(path)?);
        for (p, _) in CUSTOM_ICONS
            .into_iter()
            .chain(EFFECT_ICONS)
            .chain(MOUSE_LAYERS)
        {
            if p.starts_with(path) {
                out.push(p.into());
            }
        }
        Ok(out)
    }
}
