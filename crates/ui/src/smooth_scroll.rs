//! Eased wheel scrolling for the workspace.
//!
//! Precise (trackpad) deltas already arrive per frame and pass through untouched. Line
//! wheel notches move a target offset instead, and each frame closes a fixed fraction of
//! the remaining distance, scaled by the real frame time, so motion stays smooth at 60 Hz
//! and 165 Hz alike. Only the scroll offset changes, so no layout is recomputed while
//! easing. Reduced motion falls back to GPUI's immediate scrolling.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::*;

/// Fraction of the remaining distance closed per second (exponential approach rate).
const APPROACH_RATE: f32 = 22.0;
/// Pixels per wheel line; GPUI's default line height feels sluggish for long panels.
const LINE_BOOST: f32 = 1.6;

#[derive(Debug, Default)]
struct Easing {
    target: Option<Pixels>,
    last_frame: Option<Instant>,
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
        id: &'static str,
        content: Div,
        background: Hsla,
        _cx: &mut Context<V>,
    ) -> Stateful<Div> {
        let handle = self.handle.clone();
        let easing = self.easing.clone();
        let content = content
            .w_full()
            .on_scroll(move |event: &ScrollEvent, window, cx| {
                if event.delta.precise() || cx.reduce_motion() {
                    easing.borrow_mut().target = None;
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height()).y * LINE_BOOST;
                let max = handle.max_offset().y;
                let start_ticking = {
                    let mut state = easing.borrow_mut();
                    let from = state.target.unwrap_or(handle.offset().y);
                    let target = (from + delta).clamp(-max, px(0.));
                    let was_idle = state.target.is_none();
                    state.target = Some(target);
                    if was_idle {
                        state.last_frame = Some(Instant::now());
                    }
                    was_idle
                };
                cx.stop_propagation();
                if start_ticking {
                    schedule_frame(handle.clone(), easing.clone(), window);
                }
            });

        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .bg(background)
            .child(
                div()
                    .id(SharedString::from(format!("{id}-viewport")))
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
                        .id(SharedString::from(format!("{id}-scrollbar")))
                        .axis(gpui_kit::component::scroll::ScrollbarAxis::Vertical)
                        .viewport_from_layout(),
                ),
            )
    }
}

fn schedule_frame(handle: ScrollHandle, easing: Rc<RefCell<Easing>>, window: &Window) {
    window.on_next_frame(move |window, _| step(&handle, &easing, window));
}

fn step(handle: &ScrollHandle, easing: &Rc<RefCell<Easing>>, window: &mut Window) {
    let now = Instant::now();
    let next_frame = {
        let mut state = easing.borrow_mut();
        let Some(target) = state.target else {
            return;
        };
        let elapsed = state
            .last_frame
            .map_or(1.0 / 120.0, |last| now.duration_since(last).as_secs_f32())
            .min(0.1);
        state.last_frame = Some(now);
        let current = handle.offset();
        let alpha = 1.0 - (-APPROACH_RATE * elapsed).exp();
        let mut next = current.y + (target - current.y) * alpha;
        if (target - next).abs() < px(0.5) {
            next = target;
            state.target = None;
        }
        handle.set_offset(point(current.x, next));
        state.target.is_some()
    };
    window.refresh();
    if next_frame {
        schedule_frame(handle.clone(), easing.clone(), window);
    }
}
