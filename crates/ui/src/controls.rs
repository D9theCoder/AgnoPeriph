//! Window-bound control state (sliders and text inputs) owned by [`AgnoPeriphApp`].
//!
//! The app entity outlives its window when AgnoPeriph runs in the notification area, so
//! these entities are rebuilt by [`AgnoPeriphApp::attach_window`] for every new window.

use agnoperiph_core::TravelHundredthsMm;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::slider::{SliderEvent, SliderState, SliderValue};
use gpui_kit::*;

use crate::app::AgnoPeriphApp;

/// Which value a slider edits; one subscription per slider routes events by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SliderRole {
    Travel,
    RtPress,
    RtRelease,
    Brightness,
    Speed,
}

/// Which text field emitted an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputRole {
    Hex,
    Rename,
    GameSearch,
    ManualName,
    ManualExe,
    ProfileExe,
    KeySearch,
}

/// Slider ranges, all in the slider's own units (hundredths of a millimetre, percent, …).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SliderRange {
    pub(crate) min: f32,
    pub(crate) max: f32,
    pub(crate) step: f32,
}

impl SliderRange {
    pub(crate) fn new(min: impl Into<f32>, max: impl Into<f32>, step: impl Into<f32>) -> Self {
        Self {
            min: min.into(),
            max: max.into(),
            step: step.into(),
        }
    }

    pub(crate) fn travel(
        min: TravelHundredthsMm,
        max: TravelHundredthsMm,
        step: TravelHundredthsMm,
    ) -> Self {
        Self::new(
            min.as_hundredths(),
            max.as_hundredths(),
            step.as_hundredths().max(1),
        )
    }
}

/// Converts a slider position to a whole unit value, clamped to `0..=u16::MAX`.
///
/// Sliders report `f32`; every wire value is an integer, so this is the single place
/// the float is rounded.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn to_units(value: f32) -> u16 {
    if value.is_nan() {
        return 0;
    }
    value.round().clamp(0.0, f32::from(u16::MAX)) as u16
}

pub(crate) fn single(value: SliderValue) -> f32 {
    match value {
        SliderValue::Single(v) => v,
        SliderValue::Range(_, end) => end,
    }
}

/// Sliders and inputs that need a window to exist.
pub(crate) struct Controls {
    pub(crate) travel: Entity<SliderState>,
    pub(crate) rt_press: Entity<SliderState>,
    pub(crate) rt_release: Entity<SliderState>,
    pub(crate) brightness: Entity<SliderState>,
    pub(crate) speed: Entity<SliderState>,
    pub(crate) hex: Entity<InputState>,
    pub(crate) rename: Entity<InputState>,
    pub(crate) game_search: Entity<InputState>,
    pub(crate) manual_name: Entity<InputState>,
    pub(crate) manual_exe: Entity<InputState>,
    pub(crate) profile_exe: Entity<InputState>,
    pub(crate) key_search: Entity<InputState>,
    /// Ranges the travel sliders were built with; a device with other limits rebuilds them.
    pub(crate) travel_range: SliderRange,
    pub(crate) rt_range: SliderRange,
    pub(crate) speed_max: f32,
}

impl std::fmt::Debug for Controls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controls")
            .field("travel_range", &self.travel_range)
            .finish_non_exhaustive()
    }
}

fn slider(range: SliderRange, value: f32, cx: &mut App) -> Entity<SliderState> {
    cx.new(|_| {
        SliderState::new()
            .min(range.min)
            .max(range.max)
            .step(range.step)
            .default_value(value.clamp(range.min, range.max))
    })
}

impl Controls {
    pub(crate) fn new(
        travel_range: SliderRange,
        rt_range: SliderRange,
        speed_max: f32,
        window: &mut Window,
        cx: &mut Context<AgnoPeriphApp>,
    ) -> (Self, Vec<Subscription>) {
        let percent = SliderRange::new(0u16, 100u16, 1u16);
        let controls = Self {
            travel: slider(travel_range, travel_range.min, cx),
            rt_press: slider(rt_range, rt_range.min, cx),
            rt_release: slider(rt_range, rt_range.min, cx),
            brightness: slider(percent, 100.0, cx),
            speed: slider(
                SliderRange::new(0.0_f32, speed_max.max(1.0), 1.0_f32),
                0.0,
                cx,
            ),
            hex: cx.new(|cx| InputState::new(window, cx).placeholder("#RRGGBB")),
            rename: cx.new(|cx| InputState::new(window, cx).placeholder("Name")),
            game_search: cx.new(|cx| InputState::new(window, cx).placeholder("Search games…")),
            manual_name: cx.new(|cx| InputState::new(window, cx).placeholder("Game name")),
            manual_exe: cx
                .new(|cx| InputState::new(window, cx).placeholder("VALORANT-Win64-Shipping.exe")),
            profile_exe: cx.new(|cx| InputState::new(window, cx).placeholder("game.exe")),
            key_search: cx
                .new(|cx| InputState::new(window, cx).placeholder("Search keys, media and macros")),
            travel_range,
            rt_range,
            speed_max,
        };
        let subscriptions = controls.subscribe(window, cx);
        (controls, subscriptions)
    }

    fn subscribe(&self, window: &mut Window, cx: &mut Context<AgnoPeriphApp>) -> Vec<Subscription> {
        let sliders = [
            (&self.travel, SliderRole::Travel),
            (&self.rt_press, SliderRole::RtPress),
            (&self.rt_release, SliderRole::RtRelease),
            (&self.brightness, SliderRole::Brightness),
            (&self.speed, SliderRole::Speed),
        ];
        let inputs = [
            (&self.hex, InputRole::Hex),
            (&self.rename, InputRole::Rename),
            (&self.game_search, InputRole::GameSearch),
            (&self.manual_name, InputRole::ManualName),
            (&self.manual_exe, InputRole::ManualExe),
            (&self.profile_exe, InputRole::ProfileExe),
            (&self.key_search, InputRole::KeySearch),
        ];
        let mut subscriptions = Vec::with_capacity(sliders.len() + inputs.len());
        for (state, role) in sliders {
            subscriptions.push(cx.subscribe_in(
                state,
                window,
                move |this, _, event: &SliderEvent, window, cx| {
                    let (value, released) = match event {
                        SliderEvent::Change(value) => (single(*value), false),
                        SliderEvent::Release(value) => (single(*value), true),
                    };
                    this.on_slider(role, value, released, window, cx);
                },
            ));
        }
        for (state, role) in inputs {
            subscriptions.push(cx.subscribe_in(
                state,
                window,
                move |this, state, event: &InputEvent, window, cx| {
                    let value = state.read(cx).value();
                    this.on_input(role, event, value, window, cx);
                },
            ));
        }
        subscriptions
    }
}

/// Sets `state` to `value` only when it differs, so syncing never fights a drag.
pub(crate) fn sync_slider(
    state: &Entity<SliderState>,
    value: f32,
    window: &mut Window,
    cx: &mut App,
) {
    state.update(cx, |state, cx| {
        let clamped = value.clamp(state.min_value(), state.max_value());
        if (single(state.value()) - clamped).abs() > f32::EPSILON {
            state.set_value(clamped, window, cx);
        }
    });
}

/// Replaces the text of `state` only when it differs.
pub(crate) fn sync_input(
    state: &Entity<InputState>,
    value: &str,
    window: &mut Window,
    cx: &mut App,
) {
    state.update(cx, |state, cx| {
        if state.value().as_ref() != value {
            state.set_value(value.to_owned(), window, cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::to_units;

    #[test]
    fn slider_positions_round_onto_whole_units() {
        assert_eq!(to_units(120.4), 120);
        assert_eq!(to_units(120.6), 121);
        assert_eq!(to_units(-4.0), 0);
        assert_eq!(to_units(f32::NAN), 0);
        assert_eq!(to_units(1e9), u16::MAX);
    }
}
