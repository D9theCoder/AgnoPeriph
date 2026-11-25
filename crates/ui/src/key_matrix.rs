//! Physical keyboard renderer shared by the trigger, keybind and calibration views.
//!
//! Each view supplies a per-key [`KeyFace`]; the layout, keycap geometry and click
//! semantics (plain click selects, Ctrl/Shift-click extends) stay identical everywhere.

use agnoperiph_core::KeyIndex;
use gpui_kit::component::{ActiveTheme, Selectable, Theme, button::Button, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::keymap::{KeyLayout, LayoutKey};

/// Width of one key unit in rem; keycaps and gaps scale with the theme's base font.
const KEY_UNIT_REM: f32 = 3.0;
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
}

/// What a keycap shows.
#[derive(Debug, Clone)]
pub(crate) struct KeyFace {
    pub(crate) detail: Option<SharedString>,
    /// Bar fill from 0 to 1 along the keycap's bottom edge.
    pub(crate) fill: Option<f32>,
    /// Small dot in the top corner, e.g. Rapid Trigger on.
    pub(crate) marker: bool,
    pub(crate) tone: KeyTone,
}

impl KeyFace {
    pub(crate) fn plain(tone: KeyTone) -> Self {
        Self {
            detail: None,
            fill: None,
            marker: false,
            tone,
        }
    }
}

type OnSelect = fn(&mut AgnoPeriphApp, KeyIndex, bool, &mut Window, &mut Context<AgnoPeriphApp>);

/// Renders `layout` as clickable keycaps with ids `(id_prefix, key)`.
///
/// `on_select` receives the key and whether Ctrl or Shift extended the selection.
pub(crate) fn render(
    id: &'static str,
    id_prefix: &'static str,
    layout: &KeyLayout,
    face: impl Fn(&LayoutKey) -> KeyFace,
    on_select: OnSelect,
    cx: &mut Context<AgnoPeriphApp>,
) -> Stateful<Div> {
    let rows: Vec<AnyElement> = layout
        .rows
        .iter()
        .map(|row| {
            h_flex()
                .gap_1()
                .children(row.iter().map(|cap| {
                    let key = cap.index;
                    let listener = cx.listener(move |this, event: &ClickEvent, window, cx| {
                        let modifiers = event.modifiers();
                        on_select(this, key, modifiers.shift || modifiers.control, window, cx);
                    });
                    keycap(
                        id_prefix,
                        cap,
                        &face(cap),
                        Some(Box::new(listener)),
                        cx.theme(),
                    )
                }))
                .into_any_element()
        })
        .collect();
    board(id, rows, cx.theme())
}

/// Renders `layout` as read-only keycaps with ids `(id_prefix, key)`.
pub(crate) fn render_static(
    id: &'static str,
    id_prefix: &'static str,
    layout: &KeyLayout,
    face: impl Fn(&LayoutKey) -> KeyFace,
    theme: &Theme,
) -> Stateful<Div> {
    let rows: Vec<AnyElement> = layout
        .rows
        .iter()
        .map(|row| {
            h_flex()
                .gap_1()
                .children(
                    row.iter()
                        .map(|cap| keycap(id_prefix, cap, &face(cap), None, theme)),
                )
                .into_any_element()
        })
        .collect();
    board(id, rows, theme)
}

fn board(id: &'static str, rows: Vec<AnyElement>, theme: &Theme) -> Stateful<Div> {
    v_flex()
        .id(id)
        .p_3()
        .gap_1()
        .rounded(theme.radius_lg)
        .bg(theme.group_box)
        .border_1()
        .border_color(theme.border)
        .children(rows)
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

fn keycap(
    id_prefix: &'static str,
    cap: &LayoutKey,
    face: &KeyFace,
    on_click: Option<ClickHandler>,
    theme: &Theme,
) -> AnyElement {
    let key = cap.index;
    let width = f32::from(cap.width) / 100.0 * KEY_UNIT_REM - KEY_GAP_REM;
    let gap = f32::from(cap.gap) / 100.0 * KEY_UNIT_REM;
    let accent = match face.tone {
        KeyTone::Done => theme.success,
        KeyTone::Picked => theme.warning,
        KeyTone::Normal | KeyTone::Selected => theme.primary,
    };
    let label = match &face.detail {
        Some(detail) => format!("{} key, {detail}", cap.label),
        None => format!("{} key", cap.label),
    };
    let content = v_flex()
        .size_full()
        .justify_between()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(cap.label.clone()),
                )
                .when(face.marker, |this| {
                    this.child(div().size_1p5().rounded_full().bg(accent))
                }),
        )
        .child(
            v_flex()
                .w_full()
                .gap_0p5()
                .when_some(face.detail.clone(), |this, detail| {
                    this.child(
                        div()
                            .text_xs()
                            .truncate()
                            .font_family(theme.mono_font_family.clone())
                            .opacity(0.8)
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
                }),
        );

    let tinted = matches!(face.tone, KeyTone::Picked | KeyTone::Done);
    let Some(on_click) = on_click else {
        return div()
            .id((id_prefix, u64::from(key.value())))
            .ml(rems(gap))
            .w(rems(width))
            .h_12()
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(if tinted { accent } else { theme.border })
            .bg(if tinted {
                accent.opacity(0.18)
            } else {
                theme.background
            })
            .child(content)
            .into_any_element();
    };
    Button::new((id_prefix, u64::from(key.value())))
        .selected(face.tone == KeyTone::Selected)
        .accessibility_label(label)
        .ml(rems(gap))
        .w(rems(width))
        .h_12()
        .px_1()
        .py_1()
        .when(tinted, |this| {
            this.border_color(accent).bg(accent.opacity(0.18))
        })
        .child(content)
        .on_click(on_click)
        .into_any_element()
}
