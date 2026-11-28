//! The physical keyboard board shared by the trigger, keybind, lighting and calibration views.
//!
//! Each view supplies a per-key [`KeyFace`]; geometry and keycap chrome stay identical
//! everywhere. Keycaps are plain interactive `div`s rather than `Button`s: a full board is
//! ~85 caps, and the component's extra layers made every matrix frame several times more
//! expensive than the caps themselves. Click semantics belong to each view's handler.

use std::time::Duration;

use agnoperiph_core::{DeviceId, KeyIndex};
use gpui_kit::base::{Transition, transition};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::design::{KEY_FONT, Tokens, dp, glow, legend_on, mix, tabular, tokens};
use crate::icons::{AdvancedIcon, advanced_svg};
use crate::keymap::{KeyLayout, LayoutKey, legend};

/// Pitch of one key unit. Sizes here are pixels at the default text size.
const UNIT: f32 = 40.0;
const KEY_HEIGHT: f32 = 37.0;
/// Horizontal space between neighbouring keycaps.
const KEY_SPACING: f32 = 3.0;
const CASE_PADDING: f32 = 8.0;
const CASE_TOP_PADDING: f32 = 10.0;
const LIP_HEIGHT: f32 = 30.0;
/// How far a selected keycap sinks, taken from the lip under its face.
const PRESS_TRAVEL: f32 = 2.5;
/// A key goes down faster than it comes back, as a real switch does.
const PRESS_IN: Duration = Duration::from_millis(90);
const PRESS_OUT: Duration = Duration::from_millis(140);
/// Width of the board column: a 16u board plus the case padding.
pub(crate) const BOARD_WIDTH: f32 = 16.0 * UNIT + 2.0 * CASE_PADDING;

/// Group name that lets a keycap's face react to hovering anywhere on the cap.
const HOVER_GROUP: &str = "keycap";

/// How a keycap is emphasised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyTone {
    Normal,
    /// Part of the current selection, or picked for a paired key.
    Selected,
    /// A positive per-key state, such as calibrated.
    Done,
    /// Unavailable for the current action, e.g. already bound while picking another function.
    Locked,
}

/// What is printed on a keycap, which depends on the view looking at the board.
#[derive(Debug, Clone)]
pub(crate) enum KeyContent {
    Legend,
    /// Actuation depth along the bottom edge.
    Trigger {
        fill: f32,
        /// Shown only when the key differs from the board's common actuation.
        value: Option<SharedString>,
        is_rapid: bool,
    },
    /// Sends something else on this layer; the original name moves below.
    Remap(SharedString),
    /// Falls through to the base layer.
    Inherit,
    /// Shows the colour its LED is set to.
    Lit(Hsla),
}

/// What a keycap shows.
#[derive(Debug, Clone)]
pub(crate) struct KeyFace {
    pub(crate) content: KeyContent,
    pub(crate) tone: KeyTone,
    /// Differs from the value on the device.
    pub(crate) is_pending: bool,
    /// Which advanced function owns this key.
    pub(crate) badge: Option<AdvancedIcon>,
    /// Dashed ring, used to point out the keys of a hovered pair.
    pub(crate) is_outlined: bool,
    /// Spoken after the key name.
    pub(crate) description: Option<SharedString>,
}

impl KeyFace {
    pub(crate) fn new(content: KeyContent, tone: KeyTone) -> Self {
        Self {
            content,
            tone,
            is_pending: false,
            badge: None,
            is_outlined: false,
            description: None,
        }
    }
}

type OnSelect = fn(&mut AgnoPeriphApp, KeyIndex, bool, &mut Window, &mut Context<AgnoPeriphApp>);

struct Placed<'a> {
    cap: &'a LayoutKey,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// Positions in default-size pixels, with the plate's size. Boards wider than 16 units
/// scale down to fit `board_width`.
fn place(layout: &KeyLayout, board_width: f32) -> (Vec<Placed<'_>>, f32, f32) {
    let units = f32::from(layout.width().max(100)) / 100.0;
    let unit = UNIT.min((board_width - 2.0 * CASE_PADDING) / units);
    let scale = unit / UNIT;
    let row_gap = f32::from(layout.row_gap) / 100.0 * unit;
    let mut keys = Vec::new();
    let mut y = 0.0;
    for (row_ix, row) in layout.rows.iter().enumerate() {
        if row_ix == 1 {
            y += row_gap;
        }
        let mut x = 0.0;
        for cap in row {
            x += f32::from(cap.gap) / 100.0 * unit;
            let width = f32::from(cap.width) / 100.0 * unit;
            keys.push(Placed {
                cap,
                x,
                y,
                width: width - KEY_SPACING,
                height: KEY_HEIGHT * scale,
            });
            x += width;
        }
        y += unit;
    }
    let height = (y - (UNIT - KEY_HEIGHT) * scale).max(0.0);
    (keys, units * unit - KEY_SPACING, height)
}

/// Height of the case around `layout`, in pixels at the default text size.
pub(crate) fn height(layout: &KeyLayout, board_width: f32) -> f32 {
    let (_, _, plate) = place(layout, board_width);
    CASE_TOP_PADDING + plate + LIP_HEIGHT
}

/// A whole board as one component, which gives its keys a window to animate in without a
/// layer per keycap.
#[derive(IntoElement)]
pub(crate) struct KeyBoard {
    id: &'static str,
    id_prefix: &'static str,
    /// Scopes the press motion to one board of one device, so switching either never plays
    /// it. `None` for read-only boards.
    press_scope: Option<SharedString>,
    layout: KeyLayout,
    board_width: f32,
    faces: Vec<KeyFace>,
    handlers: Vec<ClickHandler>,
    lip: Vec<AnyElement>,
}

impl RenderOnce for KeyBoard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (keys, width, height) = place(&self.layout, self.board_width);
        let presses: Vec<f32> = keys
            .iter()
            .zip(&self.faces)
            .map(|(placed, face)| {
                let is_down = face.tone == KeyTone::Selected;
                let target = if is_down { 1.0 } else { 0.0 };
                let Some(scope) = &self.press_scope else {
                    return target;
                };
                let duration = if is_down { PRESS_IN } else { PRESS_OUT };
                transition(
                    ElementId::NamedInteger(scope.clone(), u64::from(placed.cap.index.value())),
                    target,
                    Transition::new(duration),
                    window,
                    cx,
                )
            })
            .collect();
        let colors = tokens(cx);
        let mut handlers = self.handlers.into_iter();
        let caps: Vec<AnyElement> = keys
            .iter()
            .zip(&self.faces)
            .zip(&presses)
            .map(|((placed, face), &press)| {
                keycap(
                    self.id_prefix,
                    placed,
                    face,
                    press,
                    handlers.next(),
                    &colors,
                    cx,
                )
            })
            .collect();
        board(
            self.id,
            self.board_width,
            &keys,
            &self.faces,
            &presses,
            caps,
            (width, height),
            self.lip,
            cx,
        )
    }
}

/// Renders `layout` as clickable keycaps with ids `(id_prefix, key)` over a legend lip.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    id: &'static str,
    id_prefix: &'static str,
    device: Option<&DeviceId>,
    layout: &KeyLayout,
    board_width: f32,
    face: impl Fn(&LayoutKey) -> KeyFace,
    on_select: OnSelect,
    lip: Vec<AnyElement>,
    cx: &mut Context<AgnoPeriphApp>,
) -> KeyBoard {
    let handler = |key: KeyIndex, cx: &mut Context<AgnoPeriphApp>| -> ClickHandler {
        Box::new(cx.listener(move |this, event: &ClickEvent, window, cx| {
            let modifiers = event.modifiers();
            on_select(this, key, modifiers.shift || modifiers.control, window, cx);
        }))
    };
    let device = device.map_or_else(String::new, DeviceId::to_string);
    KeyBoard {
        id,
        id_prefix,
        press_scope: Some(format!("{id}-press-{device}").into()),
        board_width,
        faces: layout.rows.iter().flatten().map(face).collect(),
        handlers: layout
            .rows
            .iter()
            .flatten()
            .map(|cap| handler(cap.index, cx))
            .collect(),
        layout: layout.clone(),
        lip,
    }
}

/// Renders `layout` as read-only keycaps with ids `(id_prefix, key)`.
pub(crate) fn render_static(
    id: &'static str,
    id_prefix: &'static str,
    layout: &KeyLayout,
    face: impl Fn(&LayoutKey) -> KeyFace,
    lip: Vec<AnyElement>,
) -> KeyBoard {
    KeyBoard {
        id,
        id_prefix,
        press_scope: None,
        board_width: BOARD_WIDTH,
        faces: layout.rows.iter().flatten().map(face).collect(),
        handlers: Vec::new(),
        layout: layout.clone(),
        lip,
    }
}

#[allow(clippy::too_many_arguments)]
fn board(
    id: &'static str,
    board_width: f32,
    keys: &[Placed<'_>],
    faces: &[KeyFace],
    presses: &[f32],
    caps: Vec<AnyElement>,
    (width, height): (f32, f32),
    lip: Vec<AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    let colors = tokens(cx);
    // Badges and rings overhang their cap, so they go in a layer above every cap;
    // drawn with their own key, the next one would paint over them.
    let overlays: Vec<AnyElement> = keys
        .iter()
        .zip(faces)
        .zip(presses)
        .flat_map(|((placed, face), &press)| {
            let ring = face
                .is_outlined
                .then(|| outline_ring(placed, cx).into_any_element());
            let badge = face
                .badge
                .map(|icon| type_badge(placed, icon, press, &colors, cx).into_any_element());
            ring.into_iter().chain(badge)
        })
        .collect();

    v_flex()
        .id(id)
        .flex_shrink_0()
        .w(dp(board_width))
        .pt(dp(CASE_TOP_PADDING))
        .px(dp(CASE_PADDING))
        .rounded(dp(14.))
        .bg(colors.case)
        .border_t_1()
        .border_color(colors.case_highlight)
        .shadow(vec![
            BoxShadow::new(px(0.), px(16.), colors.case_shadow)
                .blur_radius(px(30.))
                .spread_radius(px(-18.)),
        ])
        .child(
            div()
                .relative()
                .mx_auto()
                .w(dp(width))
                .h(dp(height))
                .children(caps)
                .children(overlays),
        )
        .child(
            h_flex()
                .h(dp(LIP_HEIGHT))
                .px(dp(6.))
                .gap(dp(12.))
                .text_size(dp(11.5))
                .text_color(colors.case_text)
                .whitespace_nowrap()
                .overflow_hidden()
                .children(lip),
        )
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// `from` and `to` themselves at the ends, so a settled cap is exactly its colour.
fn blend(from: Hsla, to: Hsla, share: f32) -> Hsla {
    if share <= 0.0 {
        from
    } else if share >= 1.0 {
        to
    } else {
        mix(to, from, share)
    }
}

/// Darker towards the top as the cap sinks, like a dished cap seen from above.
fn face_fill(surface: Hsla, press: f32, colors: &Tokens) -> Background {
    if press <= 0.0 {
        return surface.into();
    }
    linear_gradient(
        180.,
        linear_color_stop(mix(colors.key_well, surface, 0.22 * press), 0.),
        linear_color_stop(surface, 1.),
    )
}

fn keycap(
    id_prefix: &'static str,
    placed: &Placed<'_>,
    face: &KeyFace,
    press: f32,
    on_click: Option<ClickHandler>,
    colors: &Tokens,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let cap = placed.cap;
    let is_selected = face.tone == KeyTone::Selected;

    let (rest_skirt, rest_surface, rest_hover) = match (&face.content, face.tone) {
        (_, KeyTone::Done) => (
            mix(theme.success, colors.key_skirt, 0.55),
            mix(theme.success, colors.key_face, 0.24),
            None,
        ),
        (KeyContent::Lit(color), _) => (mix(*color, colors.key_skirt, 0.45), *color, None),
        (KeyContent::Remap(_), _) => (
            colors.key_skirt,
            mix(theme.primary, colors.key_face, 0.16),
            None,
        ),
        _ => (
            colors.key_skirt,
            colors.key_face,
            Some(colors.key_face_hover),
        ),
    };
    let (down_skirt, down_surface) = match &face.content {
        KeyContent::Lit(_) => (rest_skirt, rest_surface),
        content => {
            let share = if matches!(content, KeyContent::Remap(_)) {
                0.40
            } else {
                0.34
            };
            (
                mix(theme.primary, colors.key_skirt, 0.38),
                mix(theme.primary, colors.key_face, share),
            )
        }
    };
    let mut skirt = blend(rest_skirt, down_skirt, press);
    let mut surface = blend(rest_surface, down_surface, press);
    // Hovering follows the cap down; a fully pressed one looks the same either way.
    let hover = rest_hover
        .filter(|_| press < 1.0)
        .map(|hover| blend(hover, down_surface, press));
    if face.is_pending {
        skirt = theme.warning;
    }
    if face.tone == KeyTone::Locked {
        surface = colors.key_face;
    }

    // A pressed cap sits below the plate: shaded under the recess's top edge, with light
    // from underneath catching its lower edge.
    let mut inner_shadows = Vec::new();
    if press < 1.0 {
        let highlight = colors.key_highlight.opacity(1.0 - press);
        inner_shadows.push(BoxShadow::new(px(0.), px(1.), highlight).inset());
    }
    if press > 0.0 {
        inner_shadows.push(
            BoxShadow::new(px(0.), px(3.), colors.key_press_shade.opacity(press))
                .blur_radius(px(5.))
                .inset(),
        );
        inner_shadows
            .push(BoxShadow::new(px(0.), px(-1.), theme.primary.opacity(0.75 * press)).inset());
    }
    let name = SharedString::from(legend(&cap.label).to_owned());
    let spoken = match &face.description {
        Some(description) => format!("{} key, {description}", cap.label),
        None => format!("{} key", cap.label),
    };
    let radius = if cap.is_knob {
        dp(placed.height)
    } else {
        dp(5.)
    };

    let top = div()
        .relative()
        .size_full()
        .overflow_hidden()
        .rounded(radius)
        .bg(face_fill(surface, press, colors))
        .shadow(inner_shadows)
        .when_some(hover.filter(|_| on_click.is_some()), |this, hover| {
            let fill = face_fill(hover, press, colors);
            this.group_hover(HOVER_GROUP, move |style| style.bg(fill))
        })
        .map(|this| {
            if cap.is_knob {
                // Too round for a second line, so it shows only what the lens is about.
                let (text, color) = match &face.content {
                    KeyContent::Remap(sends) => (sends.clone(), theme.primary),
                    KeyContent::Inherit => (name.clone(), colors.key_legend.opacity(0.32)),
                    KeyContent::Lit(color) => (name.clone(), legend_on(*color)),
                    _ => (name.clone(), colors.key_legend),
                };
                this.flex()
                    .items_center()
                    .justify_center()
                    .child(key_text(text, 9.5, color))
            } else {
                key_content(this, name.clone(), placed.width, face, colors, cx)
            }
        });
    // Lets tests measure how far the face has travelled. The id costs a little every
    // frame, so release builds go without.
    #[cfg(debug_assertions)]
    let top = top.id("cap-face").test_support();

    let width = if cap.is_knob {
        placed.height
    } else {
        placed.width
    };
    let radius_outer = if cap.is_knob {
        dp(placed.height)
    } else {
        dp(6.)
    };
    let body = div()
        .size_full()
        .pt(dp(1.))
        .px(dp(2.))
        .pb(dp(4. - PRESS_TRAVEL * press))
        .rounded(radius_outer)
        .bg(skirt)
        .child(top);
    div()
        .id((id_prefix, u64::from(cap.index.value())))
        .aria_label(spoken)
        .aria_selected(is_selected)
        .absolute()
        .left(dp(placed.x + (placed.width - width) / 2.0))
        .top(dp(placed.y))
        .w(dp(width))
        .h(dp(placed.height))
        .rounded(radius_outer)
        .when(press > 0.0, |this| {
            this.pt(dp(PRESS_TRAVEL * press))
                .bg(colors.key_well.opacity(press))
                .shadow(vec![
                    // Light leaking from under the pressed cap.
                    BoxShadow::new(px(0.), px(2.), theme.primary.opacity(0.55 * press))
                        .blur_radius(px(7.))
                        .spread_radius(px(-2.)),
                ])
        })
        .group(HOVER_GROUP)
        .when(face.tone == KeyTone::Locked, |this| this.opacity(0.45))
        .map(|this| match &face.content {
            KeyContent::Lit(color) => this.shadow(vec![glow(*color, 14.0).spread_radius(px(-5.))]),
            _ => this,
        })
        .when_some(on_click, |this, on_click| {
            this.when(face.tone != KeyTone::Locked, Styled::cursor_pointer)
                .on_click(on_click)
        })
        .child(body)
        .test_support()
        .into_any_element()
}

fn key_text(text: impl Into<SharedString>, size: f32, color: Hsla) -> Div {
    div()
        .font_family(KEY_FONT)
        .text_size(dp(size))
        .line_height(relative(1.0))
        .whitespace_nowrap()
        .text_color(color)
        .child(text.into())
}

fn key_content(
    face_box: Div,
    name: SharedString,
    width: f32,
    face: &KeyFace,
    colors: &Tokens,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let legend_slot = |text: SharedString, color: Hsla| {
        key_text(text, 11.0, color)
            .absolute()
            .left(dp(5.))
            .top(dp(4.))
            .max_w(dp(width - 8.0))
            .overflow_hidden()
    };
    match &face.content {
        KeyContent::Legend => face_box.child(legend_slot(name, colors.key_legend)),
        KeyContent::Inherit => face_box.child(legend_slot(name, colors.key_legend.opacity(0.32))),
        KeyContent::Lit(color) => face_box.child(legend_slot(name, legend_on(*color))),
        KeyContent::Remap(sends) => face_box
            .child(legend_slot(sends.clone(), theme.primary))
            .child(
                key_text(name, 9.0, colors.key_sub)
                    .absolute()
                    .left(dp(5.))
                    .bottom(dp(5.)),
            ),
        KeyContent::Trigger {
            fill,
            value,
            is_rapid,
        } => {
            let differs = value.is_some();
            face_box
                .child(legend_slot(name, colors.key_legend))
                .when_some(value.clone(), |this, value| {
                    this.child(
                        key_text(value, 10.5, theme.foreground)
                            .font_features(tabular())
                            .absolute()
                            .left(dp(5.))
                            .bottom(dp(7.)),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .left(dp(5.))
                        .right(dp(if *is_rapid { 12.0 } else { 5.0 }))
                        .bottom(dp(3.))
                        .h(dp(2.))
                        .rounded_full()
                        .bg(colors.key_track)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .w(relative(fill.clamp(0.0, 1.0)))
                                .bg(if differs {
                                    theme.foreground.opacity(0.85)
                                } else {
                                    colors.key_sub.opacity(0.45)
                                }),
                        ),
                )
                .when(*is_rapid, |this| {
                    this.child(
                        div()
                            .absolute()
                            .right(dp(4.))
                            .bottom(dp(1.5))
                            .size(dp(5.))
                            .rounded_full()
                            .bg(theme.primary),
                    )
                })
        }
    }
}

/// The circle on a cap's top-right corner naming the function that owns the key.
fn type_badge(
    placed: &Placed<'_>,
    icon: AdvancedIcon,
    press: f32,
    colors: &Tokens,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let background = blend(colors.key_face, theme.primary, press);
    let foreground = blend(colors.key_legend, theme.primary_foreground, press);
    let travel = PRESS_TRAVEL * press;
    div()
        .absolute()
        .left(dp(placed.x + placed.width - 13.0))
        .top(dp(placed.y - 6.0 + travel))
        .size(dp(19.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(background)
        .shadow(vec![
            BoxShadow::new(px(0.), px(0.), colors.case).spread_radius(px(1.5)),
        ])
        .child(advanced_svg(icon, dp(13.), foreground))
}

fn outline_ring(placed: &Placed<'_>, cx: &App) -> Div {
    div()
        .absolute()
        .left(dp(placed.x - 3.5))
        .top(dp(placed.y - 3.5))
        .w(dp(placed.width + 7.0))
        .h(dp(placed.height + 7.0))
        .rounded(dp(8.))
        .border(px(1.5))
        .border_dashed()
        .border_color(cx.theme().primary)
}

/// A lip legend entry: a swatch followed by what it means.
pub(crate) fn legend_item(swatch: impl IntoElement, label: impl Into<SharedString>) -> AnyElement {
    h_flex()
        .flex_shrink_0()
        .gap(dp(6.))
        .child(swatch)
        .child(label.into())
        .into_any_element()
}
