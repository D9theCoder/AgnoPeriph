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
        if let Some((_, bytes)) = CUSTOM_ICONS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = gpui_kit::assets::Assets.list(path)?;
        for (p, _) in CUSTOM_ICONS {
            if p.starts_with(path) {
                out.push(p.into());
            }
        }
        Ok(out)
    }
}
