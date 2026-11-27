//! Short transitions that show what an action just changed. They run once per change,
//! never loop, and finish at once when Windows has animation effects turned off.

use std::time::Duration;

use gpui_kit::base::{Easing, Presence, Transition};
use gpui_kit::component::v_flex;
use gpui_kit::*;

use crate::design::dp;

const ENTRANCE: Duration = Duration::from_millis(160);
/// How far content travels while it fades in, in pixels at the default text size.
const RISE: f32 = 6.0;

/// Content that fades in while rising into place. It replays when `key` changes, so the
/// key names what is being shown, not where.
#[derive(IntoElement)]
pub(crate) struct Entrance {
    key: SharedString,
    child: AnyElement,
}

impl Entrance {
    pub(crate) fn new(key: impl Into<SharedString>, child: impl IntoElement) -> Self {
        Self {
            key: key.into(),
            child: child.into_any_element(),
        }
    }
}

impl RenderOnce for Entrance {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let progress = Presence::new(self.key, true)
            .transition(Transition::new(ENTRANCE).easing(Easing::EaseOut))
            .sample(window, cx)
            .progress;
        v_flex()
            .min_w_0()
            .flex_shrink_0()
            .relative()
            .top(dp(RISE * (1.0 - progress)))
            .opacity(progress)
            .child(self.child)
    }
}
