//! Hall Effect key tester: shows one key's travel against its actuation point, dead zones
//! and Rapid Trigger band, using live readings when the keyboard reports them and a
//! simulated press otherwise. It always reflects unsaved edits, because it reads the same
//! draft-applied values as the editors.
//!
//! Live readings are polled only while Live is on, the Triggers tab of a keyboard that
//! advertises live travel is showing and the window is open, so an idle app sends no
//! HID traffic.

use std::collections::VecDeque;
use std::time::Duration;

use agnoperiph_core::{KeyIndex, KeyTravelSample, RapidTriggerConfig, TravelHundredthsMm};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonGroup},
    h_flex,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, KeyboardTab, Route, mm, muted_note};
use crate::backend::HallEffectState;
use crate::keymap::KeyLayout;

/// Live travel read interval while the tester is showing.
pub const LIVE_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Samples kept for the travel trace.
const TRACE_LEN: usize = 48;
/// Depth change per simulated press step, in hundredths of a millimetre.
const SIM_STEP: u16 = 20;

/// What the tester reads its travel from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TesterSource {
    #[default]
    Simulated,
    Live,
}

/// The values that decide when a key is pressed, all in hundredths of a millimetre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerParams {
    pub actuation: u16,
    pub top_deadzone: u16,
    pub bottom_deadzone: u16,
    pub max_travel: u16,
    pub rapid_trigger: RapidTriggerConfig,
}

/// Replays a travel trace through the trigger logic and returns whether the key is down.
///
/// Without Rapid Trigger the key is down past the actuation point. With it, the key also
/// releases after lifting by the lift sensitivity and presses again after travelling down
/// by the press sensitivity. Standard Rapid Trigger resets above the actuation point;
/// continuous Rapid Trigger stays active until the key is back inside the top dead zone.
pub fn is_pressed(trace: impl IntoIterator<Item = u16>, params: &TriggerParams) -> bool {
    let mut pressed = false;
    let mut engaged = false;
    let mut extreme = 0u16;
    let bottom_limit = params.max_travel.saturating_sub(params.bottom_deadzone);
    let rt = params.rapid_trigger;
    for raw in trace {
        let depth = raw.min(bottom_limit.max(params.actuation));
        if depth <= params.top_deadzone {
            pressed = false;
            engaged = false;
            extreme = depth;
            continue;
        }
        if !rt.is_enabled() {
            pressed = depth >= params.actuation;
            continue;
        }
        if !rt.is_continuous() && depth < params.actuation {
            pressed = false;
            engaged = false;
            extreme = depth;
            continue;
        }
        if !engaged {
            if depth >= params.actuation {
                engaged = true;
                pressed = true;
                extreme = depth;
            }
            continue;
        }
        if pressed {
            extreme = extreme.max(depth);
            if depth + rt.release_sensitivity.as_hundredths() <= extreme {
                pressed = false;
                extreme = depth;
            }
        } else {
            extreme = extreme.min(depth);
            if depth >= extreme + rt.press_sensitivity.as_hundredths() {
                pressed = true;
                extreme = depth;
            }
        }
    }
    pressed
}

/// Tester state owned by the app.
#[derive(Default)]
pub(crate) struct TesterUi {
    pub(crate) source: TesterSource,
    sim_depth: u16,
    trace: VecDeque<u16>,
    /// Key the live readings last moved; the tester follows it.
    live_key: Option<KeyIndex>,
    live_samples: Vec<KeyTravelSample>,
    pub(crate) live_polls: usize,
    live_error: Option<SharedString>,
    /// Increments whenever polling starts or stops; replies from an older run are dropped.
    run: u64,
    is_reading: bool,
    poll: Option<Task<()>>,
}

impl std::fmt::Debug for TesterUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TesterUi")
            .field("source", &self.source)
            .field("is_polling", &self.poll.is_some())
            .finish_non_exhaustive()
    }
}

impl TesterUi {
    fn push_trace(&mut self, depth: u16) {
        if self.trace.len() == TRACE_LEN {
            self.trace.pop_front();
        }
        self.trace.push_back(depth);
    }

    pub(crate) fn reset(&mut self) {
        self.sim_depth = 0;
        self.trace.clear();
        self.live_key = None;
        self.live_samples.clear();
        self.live_error = None;
    }
}

impl AgnoPeriphApp {
    /// Whether the selected keyboard can report live key travel.
    pub fn has_live_travel(&self) -> bool {
        self.hall_effect_state()
            .is_some_and(|he| he.capabilities.has_live_travel())
    }

    /// Whether the key tester is reading live travel right now.
    pub fn is_live_polling(&self) -> bool {
        self.tester.poll.is_some()
    }

    /// Completed live travel reads since the app started.
    pub fn live_travel_polls(&self) -> usize {
        self.tester.live_polls
    }

    pub fn tester_source(&self) -> TesterSource {
        self.tester.source
    }

    fn should_poll_live(&self) -> bool {
        self.tester.source == TesterSource::Live
            && self.route() == Route::Device
            && self.keyboard_tab() == KeyboardTab::Triggers
            && self.has_live_travel()
            && self.has_window()
            && !self.is_calibrating()
    }

    pub fn set_tester_source(&mut self, source: TesterSource, cx: &mut Context<Self>) {
        if source == TesterSource::Live && !self.has_live_travel() {
            return;
        }
        if self.tester.source != source {
            self.tester.source = source;
            self.tester.reset();
        }
        self.update_live_polling(cx);
        cx.notify();
    }

    /// Starts or stops the live poll so it runs exactly while [`Self::should_poll_live`].
    pub(crate) fn update_live_polling(&mut self, cx: &mut Context<Self>) {
        let wanted = self.should_poll_live();
        if wanted == self.tester.poll.is_some() {
            return;
        }
        self.tester.run += 1;
        self.tester.is_reading = false;
        if !wanted {
            self.tester.poll = None;
            return;
        }
        let run = self.tester.run;
        self.tester.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LIVE_POLL_INTERVAL).await;
                let Ok(keep) = this.update(cx, |this, cx| this.poll_live_once(run, cx)) else {
                    break;
                };
                if !keep {
                    break;
                }
            }
        }));
    }

    /// Queues one live read; returns whether to keep polling.
    fn poll_live_once(&mut self, run: u64, cx: &mut Context<Self>) -> bool {
        if run != self.tester.run {
            return false;
        }
        if !self.should_poll_live() {
            self.tester.poll = None;
            self.tester.run += 1;
            return false;
        }
        // One read at a time, and none while a write or refresh holds the device.
        if self.tester.is_reading || self.has_pending_writes() || self.is_loading() {
            return true;
        }
        let Some(device) = self.selected_device_id().cloned() else {
            return true;
        };
        self.tester.is_reading = true;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_spawn(async move { service.read_live_key_travel(&device) })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_live_read(run, reply, cx));
        })
        .detach();
        true
    }

    fn finish_live_read(
        &mut self,
        run: u64,
        reply: agnoperiph_core::CoreResult<Vec<KeyTravelSample>>,
        cx: &mut Context<Self>,
    ) {
        if run != self.tester.run {
            return;
        }
        self.tester.is_reading = false;
        match reply {
            Ok(samples) => {
                self.tester.live_polls += 1;
                if let Some(deepest) = samples.iter().max_by_key(|s| s.travel.as_hundredths()) {
                    self.tester.live_key = Some(deepest.key);
                }
                let depth = self.tester.live_key.map_or(0, |key| {
                    samples
                        .iter()
                        .find(|s| s.key == key)
                        .map_or(0, |s| s.travel.as_hundredths())
                });
                self.tester.live_samples = samples;
                self.tester.push_trace(depth);
            }
            Err(err) => {
                // Fall back to the simulated tester instead of retrying a failing read.
                self.tester.source = TesterSource::Simulated;
                self.tester.reset();
                self.tester.live_error =
                    Some(format!("Live travel stopped: {err}. Showing the simulation.").into());
                self.tester.poll = None;
                self.tester.run += 1;
            }
        }
        cx.notify();
    }

    fn step_simulated_depth(&mut self, direction: i32, cx: &mut Context<Self>) {
        let Some(max) = self
            .hall_effect_state()
            .map(|he| he.capabilities.actuation_max.as_hundredths())
        else {
            return;
        };
        let next = (i32::from(self.tester.sim_depth) + direction * i32::from(SIM_STEP))
            .clamp(0, i32::from(max));
        self.tester.sim_depth = u16::try_from(next).unwrap_or(0);
        let depth = self.tester.sim_depth;
        self.tester.push_trace(depth);
        cx.notify();
    }

    pub fn tester_key(&self) -> KeyIndex {
        match self.tester.source {
            TesterSource::Live => self.tester.live_key.unwrap_or(self.selected_key),
            TesterSource::Simulated => self.selected_key,
        }
    }

    pub fn tester_depth(&self) -> u16 {
        match self.tester.source {
            TesterSource::Simulated => self.tester.sim_depth,
            TesterSource::Live => {
                let key = self.tester_key();
                self.tester
                    .live_samples
                    .iter()
                    .find(|s| s.key == key)
                    .map_or(0, |s| s.travel.as_hundredths())
            }
        }
    }
}

fn params_for(he: &HallEffectState, key: KeyIndex) -> TriggerParams {
    let caps = &he.capabilities;
    let config = he.key(key).or_else(|| he.snapshot.keys.first());
    let first = he.snapshot.keys.first();
    TriggerParams {
        actuation: config
            .map_or(caps.actuation_min, |c| c.actuation)
            .as_hundredths(),
        top_deadzone: first
            .and_then(|k| k.top_deadzone)
            .map_or(0, TravelHundredthsMm::as_hundredths),
        bottom_deadzone: first
            .and_then(|k| k.bottom_deadzone)
            .map_or(0, TravelHundredthsMm::as_hundredths),
        max_travel: caps.actuation_max.as_hundredths().max(1),
        rapid_trigger: config.map_or(RapidTriggerConfig::disabled(), |c| c.rapid_trigger),
    }
}

fn hundredths(value: u16) -> String {
    mm(TravelHundredthsMm::from_hundredths(value))
}

/// The tester column of the Triggers deck. `he` already carries unsaved edits.
pub(crate) fn render(
    app: &AgnoPeriphApp,
    he: &HallEffectState,
    layout: &KeyLayout,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let has_live = he.capabilities.has_live_travel();
    let source = app.tester.source;
    let key = app.tester_key();
    let depth = app.tester_depth();
    let params = params_for(he, key);
    let trace: Vec<u16> = app.tester.trace.iter().copied().collect();
    let pressed = is_pressed(trace.iter().copied(), &params);
    let max = f32::from(params.max_travel);
    let frac = |v: u16| (f32::from(v) / max).clamp(0.0, 1.0);
    let rt = params.rapid_trigger;
    let band = rt.is_enabled().then(|| {
        let start = params
            .actuation
            .saturating_sub(rt.release_sensitivity.as_hundredths());
        let end = params.actuation + rt.press_sensitivity.as_hundredths();
        (frac(start), frac(end))
    });
    let live_error = app.tester.live_error.clone();
    let theme = cx.theme();

    let source_toggle = ButtonGroup::new("tester-source")
        .outline()
        .xsmall()
        .child(
            Button::new("tester-sim")
                .label("Simulated")
                .selected(source == TesterSource::Simulated)
                .tooltip("Step through the values shown. Nothing is sent")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_tester_source(TesterSource::Simulated, cx);
                })),
        )
        .child(
            Button::new("tester-live")
                .label("Live")
                .selected(source == TesterSource::Live)
                .disabled(!has_live)
                .tooltip(if has_live {
                    "Read key travel from the keyboard while this tab is open"
                } else {
                    "This keyboard doesn’t report live key travel"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_tester_source(TesterSource::Live, cx);
                })),
        );

    let bar = div()
        .id("tester-meter")
        .relative()
        .w_6()
        .h_full()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(relative(frac(params.top_deadzone)))
                .bg(theme.border),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(relative(frac(params.bottom_deadzone)))
                .bg(theme.border),
        )
        .when_some(band, |this, (start, end)| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(relative(start))
                    .h(relative((end - start).max(0.01)))
                    .bg(theme.warning.opacity(0.3)),
            )
        })
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(relative(frac(depth)))
                .bg(theme.primary.opacity(if pressed { 0.7 } else { 0.35 })),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(relative(frac(params.actuation)))
                .h_0p5()
                .bg(theme.foreground),
        );

    let readout = v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(layout.label(key)),
        )
        .child(
            div()
                .id("tester-depth")
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .child(format!("{} mm", hundredths(depth))),
        )
        .child(div().id("tester-state").child(if pressed {
            Tag::primary().xsmall().child("Pressed")
        } else {
            Tag::secondary().xsmall().child("Released")
        }))
        .when(source == TesterSource::Simulated, |this| {
            this.child(
                h_flex()
                    .mt_auto()
                    .gap_1()
                    .child(
                        Button::new("tester-lift")
                            .outline()
                            .xsmall()
                            .label("Lift")
                            .disabled(depth == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step_simulated_depth(-1, cx);
                            })),
                    )
                    .child(
                        Button::new("tester-press")
                            .outline()
                            .xsmall()
                            .label("Press")
                            .disabled(depth >= params.max_travel)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step_simulated_depth(1, cx);
                            })),
                    ),
            )
        });

    let trace_view = h_flex()
        .id("tester-trace")
        .relative()
        .w_full()
        .h_6()
        .items_start()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .children((0..TRACE_LEN).map(|ix| {
            let offset = TRACE_LEN - trace.len();
            let value = ix.checked_sub(offset).and_then(|i| trace.get(i)).copied();
            div()
                .flex_1()
                .h(relative(value.map_or(0.0, frac)))
                .bg(theme.primary.opacity(0.5))
        }))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(relative(frac(params.actuation)))
                .h_px()
                .bg(theme.muted_foreground),
        );

    let legend = if rt.is_enabled() {
        format!(
            "Actuates at {} mm · RT {}/{}",
            hundredths(params.actuation),
            mm(rt.press_sensitivity),
            mm(rt.release_sensitivity)
        )
    } else {
        format!("Actuates at {} mm · RT off", hundredths(params.actuation))
    };
    // One note line: a read error wins, then the live prompt, then the trigger summary.
    let note: SharedString = match (&live_error, source, app.tester.live_key) {
        (Some(err), _, _) => err.clone(),
        (None, TesterSource::Live, None) => "Press any key on the keyboard.".into(),
        _ => legend.into(),
    };

    v_flex()
        .id("key-tester")
        .w_40()
        .flex_shrink_0()
        .gap_2()
        .child(
            h_flex()
                .min_h_6()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Key tester"),
                )
                .child(source_toggle),
        )
        .child(
            h_flex()
                .h_24()
                .items_stretch()
                .gap_2()
                .child(
                    v_flex()
                        .justify_between()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child("0.00")
                        .child(hundredths(params.max_travel)),
                )
                .child(bar)
                .child(readout),
        )
        .child(trace_view)
        .child(
            muted_note(note, theme)
                .id("tester-note")
                .whitespace_nowrap()
                .truncate(),
        )
}

#[cfg(test)]
mod tests {
    use agnoperiph_core::{RapidTriggerConfig, TravelHundredthsMm};

    use super::{TriggerParams, is_pressed};

    fn params(rt: RapidTriggerConfig) -> TriggerParams {
        TriggerParams {
            actuation: 100,
            top_deadzone: 10,
            bottom_deadzone: 0,
            max_travel: 400,
            rapid_trigger: rt,
        }
    }

    fn t(v: u16) -> TravelHundredthsMm {
        TravelHundredthsMm::from_hundredths(v)
    }

    #[test]
    fn plain_trigger_follows_the_actuation_point() {
        let p = params(RapidTriggerConfig::disabled());
        assert!(!is_pressed([50, 90], &p));
        assert!(is_pressed([50, 120], &p));
        assert!(!is_pressed([120, 300, 99], &p));
    }

    #[test]
    fn rapid_trigger_releases_on_lift_and_repeats_on_press() {
        let p = params(RapidTriggerConfig::new(true, false, t(20), t(30)));
        assert!(is_pressed([150], &p));
        assert!(is_pressed([150, 200, 175], &p));
        assert!(!is_pressed([150, 200, 170], &p));
        assert!(!is_pressed([150, 200, 170, 185], &p));
        assert!(is_pressed([150, 200, 170, 190], &p));
        // Standard Rapid Trigger resets above the actuation point.
        assert!(!is_pressed([150, 90], &p));
    }

    #[test]
    fn continuous_rapid_trigger_stays_active_above_the_actuation_point() {
        let p = params(RapidTriggerConfig::new(true, true, t(20), t(20)));
        assert!(!is_pressed([150, 60], &p));
        assert!(is_pressed([150, 60, 80], &p));
        assert!(!is_pressed([150, 60, 80, 5], &p));
    }
}
