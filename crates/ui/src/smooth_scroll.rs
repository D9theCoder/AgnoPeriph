//! Eased wheel scrolling for the workspace.
//!
//! Precise (trackpad) deltas already arrive per frame and pass through untouched. Line
//! wheel notches move a target offset instead, and each frame closes a fixed fraction of
//! the remaining distance, scaled by the real frame time, so motion stays smooth at 60 Hz
//! and 165 Hz alike. Reduced motion falls back to GPUI's immediate scrolling.
//!
//! Each easing frame only moves the scroll offset and notifies the owning view, the same
//! contract as GPUI's own wheel listener (set the offset, then notify the view that holds
//! the container). gpui-fast counts that pairing as a scroll, which lets it composite a
//! scroll layer instead of rebuilding the view when the container qualifies for one.
//! `window.refresh()` must not be used here: it marks the whole window dirty and forces a
//! full render and layout on every animation frame.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::scroll::{Scrollbar, ScrollbarAxis};
use gpui_kit::*;

/// Fraction of the remaining distance closed per second (exponential approach rate).
const APPROACH_RATE: f32 = 26.0;
/// Pixels per wheel line; GPUI's default line height feels sluggish for long panels.
const LINE_BOOST: f32 = 1.6;
/// Distance under which the animation snaps to its target and stops.
const SNAP_DISTANCE: Pixels = px(0.75);

#[derive(Debug, Default)]
struct Easing {
    target: Option<Pixels>,
    last_frame: Option<Instant>,
}

/// Element ids for one scrolling viewport; static so rendering allocates nothing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollIds {
    pub(crate) root: &'static str,
    pub(crate) viewport: &'static str,
    pub(crate) scrollbar: &'static str,
}

/// Scroll owner for one viewport. The handle survives re-renders, keeping position.
#[derive(Default)]
pub(crate) struct SmoothScroll {
    handle: ScrollHandle,
    easing: Rc<RefCell<Easing>>,
}

impl std::fmt::Debug for SmoothScroll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmoothScroll").finish_non_exhaustive()
    }
}

impl SmoothScroll {
    /// Returns to the top, e.g. when the workspace shows a different object.
    pub(crate) fn reset(&self) {
        self.easing.borrow_mut().target = None;
        self.handle.set_offset(point(px(0.), px(0.)));
    }

    /// Wraps `content` in a vertically scrolling viewport that fills its flex parent.
    pub(crate) fn wrap<V: 'static>(
        &self,
        ids: ScrollIds,
        content: Div,
        background: Hsla,
        cx: &mut Context<V>,
    ) -> Stateful<Div> {
        let handle = self.handle.clone();
        let easing = self.easing.clone();
        let view = cx.entity_id();
        let content = content
            .w_full()
            .on_scroll(move |event: &ScrollEvent, window, cx| {
                if event.delta.precise() || cx.reduce_motion() {
                    easing.borrow_mut().target = None;
                    return;
                }
                let max = handle.max_offset().y;
                if max <= px(0.) {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height()).y * LINE_BOOST;
                let start_ticking = {
                    let mut state = easing.borrow_mut();
                    let current = handle.offset().y;
                    let from = state.target.unwrap_or(current);
                    let target = (from + delta).clamp(-max, px(0.));
                    if state.target.is_none() && target == current {
                        // Already at the edge in this direction; let outer scrollers act.
                        return;
                    }
                    let was_idle = state.target.is_none();
                    state.target = Some(target);
                    if was_idle {
                        state.last_frame = Some(Instant::now());
                    }
                    was_idle
                };
                cx.stop_propagation();
                if start_ticking {
                    schedule_frame(handle.clone(), easing.clone(), view, window);
                }
            });

        div()
            .id(ids.root)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .bg(background)
            .child(
                div()
                    .id(ids.viewport)
                    .size_full()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.handle)
                    .child(content),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::new(&self.handle)
                        .id(ids.scrollbar)
                        .axis(ScrollbarAxis::Vertical)
                        .viewport_from_layout(),
                ),
            )
    }
}

fn schedule_frame(
    handle: ScrollHandle,
    easing: Rc<RefCell<Easing>>,
    view: EntityId,
    window: &Window,
) {
    window.on_next_frame(move |window, cx| step(&handle, &easing, view, window, cx));
}

fn step(
    handle: &ScrollHandle,
    easing: &Rc<RefCell<Easing>>,
    view: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    let now = Instant::now();
    let (moved, next_frame) = {
        let mut state = easing.borrow_mut();
        let Some(target) = state.target else {
            return;
        };
        let elapsed = state
            .last_frame
            .map_or(1.0 / 165.0, |last| now.duration_since(last).as_secs_f32())
            .min(0.05);
        state.last_frame = Some(now);
        // The content may have shrunk since the notch (tab switch, collapsed section).
        let target = target.clamp(-handle.max_offset().y.max(px(0.)), px(0.));
        let current = handle.offset();
        let alpha = 1.0 - (-APPROACH_RATE * elapsed).exp();
        let mut next = current.y + (target - current.y) * alpha;
        if (target - next).abs() < SNAP_DISTANCE {
            next = target;
            state.target = None;
        } else {
            state.target = Some(target);
        }
        let moved = next != current.y;
        if moved {
            handle.set_offset(point(current.x, next));
        }
        (moved, state.target.is_some())
    };
    if moved {
        cx.notify(view);
    }
    if next_frame {
        schedule_frame(handle.clone(), easing.clone(), view, window);
    }
}
