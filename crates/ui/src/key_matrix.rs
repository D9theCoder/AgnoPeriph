//! Physical keyboard renderer shared by the trigger, keybind and calibration views.
//!
//! Each view supplies a per-key [`KeyFace`]; the layout and keycap geometry stay identical
//! everywhere. Keycaps are plain interactive `div`s rather than `Button`s: a full board is
//! ~85 caps, and the component's extra layers made every matrix frame several times more
//! expensive than the caps themselves. Click semantics belong to each view's handler.

use agnoperiph_core::KeyIndex;
use gpui_kit::component::{ActiveTheme, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::icons::{AdvancedIcon, advanced_svg};
use crate::keymap::{KeyLayout, LayoutKey};

/// Width of one key unit in rem; sized so the board sits cleanly beside Paired & Advanced Keys
/// at the default 1240×820 window width without clipping either column.
const KEY_UNIT_REM: f32 = 2.30;
/// Space between neighbouring keycaps in rem (matches `gap_1`).
const KEY_GAP_REM: f32 = 0.25;

/// How a keycap is emphasised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyTone {
    Normal,
    /// Part of the current selection.
    Selected,
    /// Picked as part of a pending paired-key flow.
    Picked,
    /// A positive per-key state, such as calibrated.
    Done,
    /// Managed by RS or SOCD: per-key actuation/RT on the keyboard map is disabled.
    Paired,
    /// Unavailable for the current action, e.g. already bound while picking another function.
    Locked,
}

/// What a keycap shows.
#[derive(Debug, Clone)]
pub(crate) struct KeyFace {
    pub(crate) detail: Option<SharedString>,
    /// Bar fill from 0 to 1 along the keycap's bottom edge.
    pub(crate) fill: Option<f32>,
    /// Small dot in the top corner, e.g. Rapid Trigger on.
    pub(crate) marker: bool,
    /// Compact SVG glyph in the top-right corner showing which advanced function owns this key.
    pub(crate) badge: Option<AdvancedIcon>,
    pub(crate) tone: KeyTone,
}

impl KeyFace {
    pub(crate) fn plain(tone: KeyTone) -> Self {
        Self {
            detail: None,
            fill: None,
            marker: false,
            badge: None,
            tone,
        }
    }
}

type OnSelect = fn(&mut AgnoPeriphApp, KeyIndex, bool, &mut Window, &mut Context<AgnoPeriphApp>);

/// Renders `layout` as clickable keycaps with ids `(id_prefix, key)`.
///
/// `on_select` receives the key and whether Ctrl or Shift was held.
pub(crate) fn render(
    id: &'static str,
    id_prefix: &'static str,
    layout: &KeyLayout,
    face: impl Fn(&LayoutKey) -> KeyFace,
    on_select: OnSelect,
    cx: &mut Context<AgnoPeriphApp>,
) -> Stateful<Div> {
    let mut board = board(id, cx.theme());
    for row in &layout.rows {
        let mut line = h_flex().gap_1();
        for cap in row {
            let key = cap.index;
            let listener = cx.listener(move |this, event: &ClickEvent, window, cx| {
                let modifiers = event.modifiers();
                on_select(this, key, modifiers.shift || modifiers.control, window, cx);
            });
            line = line.child(keycap(
                id_prefix,
                cap,
                &face(cap),
                Some(Box::new(listener)),
                cx.theme(),
            ));
        }
        board = board.child(line);
    }
    board
}

/// Renders `layout` as read-only keycaps with ids `(id_prefix, key)`.
pub(crate) fn render_static(
    id: &'static str,
    id_prefix: &'static str,
    layout: &KeyLayout,
    face: impl Fn(&LayoutKey) -> KeyFace,
    theme: &Theme,
) -> Stateful<Div> {
    let mut board = board(id, theme);
    for row in &layout.rows {
        board = board.child(
            h_flex().gap_1().children(
                row.iter()
                    .map(|cap| keycap(id_prefix, cap, &face(cap), None, theme)),
            ),
        );
    }
    board
}

fn board(id: &'static str, theme: &Theme) -> Stateful<Div> {
    v_flex()
        .id(id)
        .flex_shrink_0()
        .p_2()
        .gap_1()
        .rounded(theme.radius_lg)
        .bg(theme.group_box)
        .border_1()
        .border_color(theme.border)
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

fn keycap(
    id_prefix: &'static str,
    cap: &LayoutKey,
    face: &KeyFace,
    on_click: Option<ClickHandler>,
    theme: &Theme,
) -> AnyElement {
    let width = f32::from(cap.width) / 100.0 * KEY_UNIT_REM - KEY_GAP_REM;
    let gap = f32::from(cap.gap) / 100.0 * KEY_UNIT_REM;
    let (accent, border, background, hover) = match face.tone {
        KeyTone::Normal => (
            theme.primary,
            theme.border,
            theme.background,
            theme.secondary_hover,
        ),
        KeyTone::Selected => (
            theme.primary,
            theme.primary,
            theme.primary.opacity(0.16),
            theme.primary.opacity(0.24),
        ),
        KeyTone::Picked => (
            theme.warning,
            theme.warning,
            theme.warning.opacity(0.18),
            theme.warning.opacity(0.26),
        ),
        KeyTone::Done => (
            theme.success,
            theme.success,
            theme.success.opacity(0.18),
            theme.success.opacity(0.26),
        ),
        KeyTone::Paired | KeyTone::Locked => (
            theme.muted_foreground,
            theme.border,
            theme.muted,
            theme.muted,
        ),
    };
    let label = match &face.detail {
        Some(detail) => format!("{} key, {detail}", cap.label),
        None => format!("{} key", cap.label),
    };
    let is_dimmed = matches!(face.tone, KeyTone::Locked | KeyTone::Paired);
    let label_color = if is_dimmed {
        theme.muted_foreground
    } else {
        theme.foreground
    };

    let top = h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(label_color)
                .truncate()
                .child(cap.label.clone()),
        )
        .when_some(face.badge, |this, icon| {
            let icon_color = if face.tone == KeyTone::Picked {
                theme.warning
            } else {
                theme.muted_foreground
            };
            this.child(advanced_svg(icon, rems(0.70), icon_color))
        })
        .when(face.marker && face.badge.is_none(), |this| {
            this.child(div().flex_shrink_0().size_1p5().rounded_full().bg(accent))
        });

    div()
        .id((id_prefix, u64::from(cap.index.value())))
        .aria_label(label)
        .aria_selected(face.tone == KeyTone::Selected)
        .ml(rems(gap))
        .w(rems(width))
        .h_11()
        .px_0p5()
        .py_0p5()
        .flex()
        .flex_col()
        .justify_between()
        .rounded(theme.radius)
        .border_1()
        .border_color(border)
        .bg(background)
        .when(face.tone == KeyTone::Locked, |this| this.opacity(0.45))
        .when(face.tone == KeyTone::Paired, |this| this.opacity(0.68))
        .when_some(on_click, |this, on_click| {
            this.when(face.tone != KeyTone::Locked, Styled::cursor_pointer)
                .hover(move |style| style.bg(hover))
                .on_click(on_click)
        })
        .child(top)
        .when_some(face.detail.clone(), |this, detail| {
            this.child(
                div()
                    .text_size(rems(0.625))
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
        })
        .when_some(face.fill, |this, fill| {
            this.child(
                div().w_full().h_0p5().rounded_sm().bg(theme.border).child(
                    div()
                        .h_full()
                        .rounded_sm()
                        .w(relative(fill.clamp(0.0, 1.0)))
                        .bg(accent),
                ),
            )
        })
        .test_support()
        .into_any_element()
}
