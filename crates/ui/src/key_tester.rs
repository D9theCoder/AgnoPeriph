//! Hall Effect key tester: shows one key's travel against its actuation point, dead zones
//! and Rapid Trigger band, using live readings from the keyboard. It always reflects
//! unsaved edits, because it reads the same draft-applied values as the editors.
//!
//! Live readings are polled only while the Triggers tab of a keyboard that advertises
//! live travel is showing and the window is open, so an idle app sends no HID traffic.
//! Rendering is isolated in [`KeyTesterView`] so native-refresh-rate animation frames
//! (`window.request_animation_frame`) only re-render this column rather than the whole app.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agnoperiph_core::{
    CoreError, DeviceId, KeyIndex, KeyTravelSample, RapidTriggerConfig, SocdMode,
    TravelHundredthsMm,
};
use gpui_kit::component::{ActiveTheme, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, KeyboardTab, Route, mm};
use crate::backend::{HallEffectState, PeripheralService};
use crate::design::{DISPLAY_FONT, KEY_FONT, dp, glow, tabular, tokens};
use crate::keymap::KeyLayout;
use crate::widgets::{keycap, panel, panel_title};

/// Target live travel read cadence while the tester is showing (~166 Hz to feed 165 Hz+ displays).
pub const LIVE_POLL_INTERVAL: Duration = Duration::from_millis(6);
/// Initial reconnect retry interval after a transient USB HID disconnect (`0x0000048F`).
pub const LIVE_RETRY_INTERVAL: Duration = Duration::from_millis(250);
/// Visible horizontal time span of the scrolling travel trace.
const TRACE_WINDOW: Duration = Duration::from_millis(2400);
/// Keep slightly more than `TRACE_WINDOW` so the sample entering the left edge stays anchored.
const TRACE_RETENTION: Duration = Duration::from_millis(2800);
/// Upper bound on retained timestamped samples.
const MAX_TRACE_SAMPLES: usize = 2048;

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
    let mut state = TriggerState::default();
    for depth in trace {
        state.step(depth, params);
    }
    state.pressed
}

/// Running trigger state of one key, advanced one travel sample at a time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TriggerState {
    pressed: bool,
    engaged: bool,
    extreme: u16,
}

impl TriggerState {
    fn step(&mut self, raw: u16, params: &TriggerParams) -> bool {
        let bottom_limit = params.max_travel.saturating_sub(params.bottom_deadzone);
        let rt = params.rapid_trigger;
        let depth = raw.min(bottom_limit.max(params.actuation));
        if depth <= params.top_deadzone {
            *self = Self {
                extreme: depth,
                ..Self::default()
            };
        } else if !rt.is_enabled() {
            self.pressed = depth >= params.actuation;
        } else if !rt.is_continuous() && depth < params.actuation {
            *self = Self {
                extreme: depth,
                ..Self::default()
            };
        } else if !self.engaged {
            if depth >= params.actuation {
                self.engaged = true;
                self.pressed = true;
                self.extreme = depth;
            }
        } else if self.pressed {
            self.extreme = self.extreme.max(depth);
            if depth + rt.release_sensitivity.as_hundredths() <= self.extreme {
                self.pressed = false;
                self.extreme = depth;
            }
        } else {
            self.extreme = self.extreme.min(depth);
            if depth >= self.extreme + rt.press_sensitivity.as_hundredths() {
                self.pressed = true;
                self.extreme = depth;
            }
        }
        self.pressed
    }
}

/// How a SOCD or Rapid Switching pair settles both keys being pressed at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairRule {
    /// Rapid Switching: the deeper key wins.
    Deeper,
    /// SOCD: the key pressed last wins; the other comes back once it lifts.
    LastInput,
    /// SOCD: both keys cancel out.
    Neutral,
    /// SOCD: the pair's first key always wins.
    KeyA,
    /// SOCD: the pair's second key always wins.
    KeyB,
}

/// A SOCD or Rapid Switching pair as the tester resolves it. Both keys trigger at the pair's
/// own trigger point and Rapid Trigger setting instead of their per-key values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairParams {
    pub key_a: KeyIndex,
    pub key_b: KeyIndex,
    pub rule: PairRule,
    pub a: TriggerParams,
    pub b: TriggerParams,
}

impl PairParams {
    pub fn partner_of(&self, key: KeyIndex) -> Option<KeyIndex> {
        if key == self.key_a {
            Some(self.key_b)
        } else if key == self.key_b {
            Some(self.key_a)
        } else {
            None
        }
    }

    const fn badge(&self) -> &'static str {
        match self.rule {
            PairRule::Deeper => "RS",
            _ => "SOCD",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    A,
    B,
}

/// Running state of a pair, advanced one sample of both keys' travel at a time. It is kept
/// across polls rather than replayed from the trace, so the last-input order survives keys
/// held longer than the trace retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairState {
    params: PairParams,
    a: TriggerState,
    b: TriggerState,
    last_input: Option<Side>,
    deeper: Option<Side>,
    /// Whether key A and key B are reported down after the pair rule.
    output: (bool, bool),
}

impl PairState {
    pub fn new(params: PairParams) -> Self {
        Self {
            params,
            a: TriggerState::default(),
            b: TriggerState::default(),
            last_input: None,
            deeper: None,
            output: (false, false),
        }
    }

    /// Advances both keys by one sample and returns whether A and B are down.
    pub fn step(&mut self, a_depth: u16, b_depth: u16) -> (bool, bool) {
        let (was_a, was_b) = (self.a.pressed, self.b.pressed);
        let a = self.a.step(a_depth, &self.params.a);
        let b = self.b.step(b_depth, &self.params.b);
        let deeper_side = match a_depth.cmp(&b_depth) {
            std::cmp::Ordering::Greater => Some(Side::A),
            std::cmp::Ordering::Less => Some(Side::B),
            std::cmp::Ordering::Equal => None,
        };
        match (a && !was_a, b && !was_b) {
            (true, true) => self.last_input = deeper_side.or(Some(Side::A)),
            (true, false) => self.last_input = Some(Side::A),
            (false, true) => self.last_input = Some(Side::B),
            (false, false) => {}
        }
        self.output = if a && b {
            let winner = match self.params.rule {
                PairRule::Deeper => {
                    self.deeper = deeper_side.or(self.deeper).or(Some(Side::A));
                    self.deeper
                }
                PairRule::LastInput => self.last_input,
                PairRule::Neutral => None,
                PairRule::KeyA => Some(Side::A),
                PairRule::KeyB => Some(Side::B),
            };
            (winner == Some(Side::A), winner == Some(Side::B))
        } else {
            self.deeper = None;
            (a, b)
        };
        self.output
    }

    pub fn is_pressed(&self, key: KeyIndex) -> bool {
        if key == self.params.key_a {
            self.output.0
        } else {
            key == self.params.key_b && self.output.1
        }
    }
}

enum PollAction {
    Read(Arc<dyn PeripheralService>, DeviceId, Vec<KeyIndex>),
    Pause,
    Stop,
}

/// Tester state owned by the app.
#[derive(Default)]
pub(crate) struct TesterUi {
    trace: VecDeque<(Instant, u16)>,
    /// Key the live readings last moved; the tester follows it.
    live_key: Option<KeyIndex>,
    depth: u16,
    /// Pair that owns the tester key, advanced on every reading.
    pair: Option<PairState>,
    last_depth_change: Option<Instant>,
    pub(crate) live_polls: usize,
    live_error: Option<SharedString>,
    consecutive_errors: u32,
    /// Duration of the previous HID read, subtracted from the next sleep to avoid aliasing.
    last_read_elapsed: Duration,
    /// Increments whenever polling starts or stops; replies from an older run are dropped.
    run: u64,
    poll: Option<Task<()>>,
}

impl std::fmt::Debug for TesterUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TesterUi")
            .field("is_polling", &self.poll.is_some())
            .field("consecutive_errors", &self.consecutive_errors)
            .finish_non_exhaustive()
    }
}

impl TesterUi {
    /// Returns whether the starting depth for trigger replay changed as samples expired.
    fn push_trace(&mut self, now: Instant, depth: u16) -> bool {
        let first = self.trace.front().map(|&(_, depth)| depth);
        if self.trace.back().map_or(0, |&(_, depth)| depth) != depth {
            self.last_depth_change = Some(now);
        }
        self.trace.push_back((now, depth));
        while self.trace.len() > MAX_TRACE_SAMPLES {
            self.trace.pop_front();
        }
        // Keep one sample older than TRACE_RETENTION as a left-edge anchor if present.
        while self.trace.len() >= 2
            && now.saturating_duration_since(self.trace[1].0) > TRACE_RETENTION
        {
            self.trace.pop_front();
        }
        first != self.trace.front().map(|&(_, depth)| depth)
    }

    fn trace_is_moving(&self, now: Instant) -> bool {
        self.last_depth_change
            .is_some_and(|last| now.saturating_duration_since(last) <= TRACE_WINDOW)
    }

    pub(crate) fn clear_error(&mut self) {
        self.live_error = None;
        self.consecutive_errors = 0;
        self.last_read_elapsed = Duration::ZERO;
    }

    pub(crate) fn reset(&mut self) {
        self.trace.clear();
        self.live_key = None;
        self.depth = 0;
        self.pair = None;
        self.last_depth_change = None;
        self.clear_error();
    }
}

/// Isolated GPUI view for the Key tester column so native-refresh-rate animation frames
/// (`165 Hz`, `240 Hz`, etc.) only re-render this column instead of the whole workspace.
pub(crate) struct KeyTesterView {
    app: WeakEntity<AgnoPeriphApp>,
    _subscriptions: Vec<Subscription>,
}

impl KeyTesterView {
    pub(crate) fn new(app: &Entity<AgnoPeriphApp>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(app, |_, _, cx| cx.notify()),
            cx.observe_global::<Theme>(|_, cx| cx.notify()),
        ];
        Self {
            app: app.downgrade(),
            _subscriptions: subscriptions,
        }
    }
}

impl Render for KeyTesterView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app_entity) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let app = app_entity.read(cx);
        let Some(device) = app.selected_device() else {
            return div().into_any_element();
        };
        let Some(he) = device.hall_effect.as_ref() else {
            return div().into_any_element();
        };
        if !he.capabilities.has_live_travel() {
            return div().into_any_element();
        }

        let layout = KeyLayout::for_device(&device.info, &he.capabilities);
        let now = cx.background_executor().now();
        let key = app.tester_key();
        let depth = app.tester_depth();
        let params = params_for(he, key);
        let pressed = if let Some(pressed) = app.tester_pair_pressed() {
            pressed
        } else if params.rapid_trigger.is_enabled() {
            is_pressed(app.tester.trace.iter().map(|&(_, d)| d), &params)
        } else {
            is_pressed(
                [app.tester.trace.back().map_or(0, |&(_, depth)| depth)],
                &params,
            )
        };
        if app.is_live_polling()
            && app.tester.live_error.is_none()
            && app.tester.trace_is_moving(now)
            && !cx.reduce_motion()
        {
            // Drive the meter and scrolling waveform at the monitor's native VSync refresh rate
            // (e.g. 165 fps on a 165 Hz display), notifying only this lightweight view.
            window.request_animation_frame();
        }

        render_column(
            key,
            depth,
            pressed,
            params,
            pair_params(he, key),
            self.app.clone(),
            now,
            app.tester.live_key,
            app.tester.live_error.as_ref(),
            &layout,
            cx,
        )
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

    /// Current live tester error/reconnecting status note, if any.
    pub fn tester_error(&self) -> Option<&str> {
        self.tester.live_error.as_deref()
    }

    fn should_poll_live(&self) -> bool {
        self.route() == Route::Device
            && self.keyboard_tab() == KeyboardTab::Triggers
            && self.has_live_travel()
            && self.has_window()
            && !self.is_calibrating()
    }

    /// Starts or stops the live poll so it runs exactly while [`Self::should_poll_live`].
    pub(crate) fn update_live_polling(&mut self, cx: &mut Context<Self>) {
        let wanted = self.should_poll_live();
        if wanted == self.tester.poll.is_some() {
            return;
        }
        self.tester.run += 1;
        self.tester.last_read_elapsed = Duration::ZERO;
        if !wanted {
            self.tester.poll = None;
            return;
        }
        self.tester.clear_error();
        let run = self.tester.run;
        self.tester.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(delay)) = this.read_with(cx, |this, _| this.live_poll_delay(run))
                else {
                    break;
                };
                cx.background_executor().timer(delay).await;
                let Ok(action) = this.update(cx, |this, _| this.prepare_live_read(run)) else {
                    break;
                };
                match action {
                    PollAction::Stop => break,
                    PollAction::Pause => {}
                    PollAction::Read(service, device, focus_keys) => {
                        let requested_device = device.clone();
                        let started = cx.background_executor().now();
                        let reply = cx
                            .background_spawn(async move {
                                service.read_live_key_travel_focused(&device, &focus_keys)
                            })
                            .await;
                        let now = cx.background_executor().now();
                        let elapsed = now.saturating_duration_since(started);
                        let Ok(keep) = this.update(cx, |this, cx| {
                            if this.selected_device_id() != Some(&requested_device) {
                                return run == this.tester.run;
                            }
                            this.finish_live_read(run, reply, now, elapsed, cx)
                        }) else {
                            break;
                        };
                        if !keep {
                            break;
                        }
                    }
                }
            }
        }));
    }

    fn live_poll_delay(&self, run: u64) -> Option<Duration> {
        if run != self.tester.run || !self.should_poll_live() {
            return None;
        }
        if self.tester.consecutive_errors > 0 {
            let factor = 1u32 << self.tester.consecutive_errors.saturating_sub(1).min(2);
            return Some(LIVE_RETRY_INTERVAL.saturating_mul(factor));
        }
        let remaining = LIVE_POLL_INTERVAL.saturating_sub(self.tester.last_read_elapsed);
        Some(remaining.max(Duration::from_millis(1)))
    }

    fn prepare_live_read(&mut self, run: u64) -> PollAction {
        if run != self.tester.run {
            return PollAction::Stop;
        }
        if !self.should_poll_live() {
            self.tester.poll = None;
            self.tester.run += 1;
            return PollAction::Stop;
        }
        // None while a write or refresh holds the device.
        if self.has_pending_writes() || self.is_loading() {
            self.tester.last_read_elapsed = Duration::ZERO;
            return PollAction::Pause;
        }
        let Some(device) = self.selected_device_id().cloned() else {
            self.tester.last_read_elapsed = Duration::ZERO;
            return PollAction::Pause;
        };
        // The partner is read on every poll too, so its absence means it is fully released.
        let key = self.tester_key();
        let mut focus_keys = vec![key];
        focus_keys.extend(
            self.hall_effect_state()
                .and_then(|he| pair_params(he, key))
                .and_then(|pair| pair.partner_of(key)),
        );
        PollAction::Read(self.service.clone(), device, focus_keys)
    }

    fn finish_live_read(
        &mut self,
        run: u64,
        reply: agnoperiph_core::CoreResult<Vec<KeyTravelSample>>,
        now: Instant,
        elapsed: Duration,
        cx: &mut Context<Self>,
    ) -> bool {
        if run != self.tester.run {
            return false;
        }
        self.tester.last_read_elapsed = elapsed;
        let was = (
            self.tester.live_key,
            self.tester.depth,
            self.tester.live_error.clone(),
        );
        let was_pair_output = self.tester.pair.map(|state| state.output);
        let mut trace_changed = false;
        let keep = match reply {
            Ok(samples) => {
                self.tester.live_polls += 1;
                self.tester.consecutive_errors = 0;
                self.tester.live_error = None;
                let deepest = samples.iter().max_by_key(|s| s.travel.as_hundredths());
                if let Some(deepest) = deepest {
                    self.tester.live_key = Some(deepest.key);
                }
                self.tester.depth = deepest.map_or(0, |sample| sample.travel.as_hundredths());
                self.step_pair(&samples);
                let replay_start_changed = self.tester.push_trace(now, self.tester.depth);
                trace_changed = (cx.reduce_motion() && self.tester.trace_is_moving(now))
                    || self.hall_effect_state().is_some_and(|he| {
                        let rt = params_for(he, self.tester_key()).rapid_trigger;
                        rt.is_enabled()
                            && (replay_start_changed
                                || (self.tester.depth > 0
                                    && (rt.press_sensitivity.as_hundredths() == 0
                                        || rt.release_sensitivity.as_hundredths() == 0)))
                    });
                true
            }
            Err(CoreError::UnsupportedCapability(_)) => {
                self.tester.reset();
                self.tester.live_error = Some("Live travel unavailable.".into());
                self.tester.poll = None;
                self.tester.run += 1;
                false
            }
            Err(_) => {
                // Keep polling with gentle backoff so a stale/suspended HID handle or brief
                // USB reset (`0x0000048F`) recovers automatically without a manual refresh.
                self.tester.depth = 0;
                self.tester.consecutive_errors = self.tester.consecutive_errors.saturating_add(1);
                let note: SharedString = if self.tester.consecutive_errors <= 2 {
                    "Reconnecting to keyboard…".into()
                } else {
                    "Keyboard disconnected — waiting to reconnect…".into()
                };
                self.tester.live_error = Some(note);
                true
            }
        };
        // Visible motion schedules its own frames. Identical readings need no extra redraw.
        if trace_changed
            || self.tester.pair.map(|state| state.output) != was_pair_output
            || (
                self.tester.live_key,
                self.tester.depth,
                &self.tester.live_error,
            ) != (was.0, was.1, &was.2)
        {
            self.tester_view.update(cx, |_, cx| cx.notify());
        }
        keep
    }

    /// Advances the pair owning the tester key with this reading, starting it afresh when the
    /// tester moves to another pair or the pair's settings change.
    fn step_pair(&mut self, samples: &[KeyTravelSample]) {
        let params = self
            .hall_effect_state()
            .and_then(|he| pair_params(he, self.tester_key()));
        let Some(params) = params else {
            self.tester.pair = None;
            return;
        };
        let depth_of = |key| {
            samples
                .iter()
                .find(|s| s.key == key)
                .map_or(0, |s| s.travel.as_hundredths())
        };
        let state = match &mut self.tester.pair {
            Some(state) if state.params == params => state,
            slot => slot.insert(PairState::new(params)),
        };
        state.step(depth_of(params.key_a), depth_of(params.key_b));
    }

    /// Whether the tester key is down after its SOCD or Rapid Switching pair rule, when it
    /// belongs to one.
    pub fn tester_pair_pressed(&self) -> Option<bool> {
        let key = self.tester_key();
        self.tester
            .pair
            .filter(|state| state.params.partner_of(key).is_some())
            .map(|state| state.is_pressed(key))
    }

    pub fn tester_key(&self) -> KeyIndex {
        self.tester.live_key.unwrap_or(self.selected_key)
    }

    pub fn tester_depth(&self) -> u16 {
        self.tester.depth
    }
}

/// The values that decide when `key` is pressed, taking a SOCD or Rapid Switching pair's
/// own trigger point and Rapid Trigger setting over the key's.
fn params_for(he: &HallEffectState, key: KeyIndex) -> TriggerParams {
    pair_params(he, key).map_or_else(
        || key_params(he, key),
        |pair| if key == pair.key_a { pair.a } else { pair.b },
    )
}

/// The SOCD or Rapid Switching pair that owns `key`, if any.
pub(crate) fn pair_params(he: &HallEffectState, key: KeyIndex) -> Option<PairParams> {
    let (key_a, key_b, rule, trigger_point, rapid_trigger) = he
        .snapshot
        .socd_pairs
        .iter()
        .find(|p| p.key_a == key || p.key_b == key)
        .map(|p| {
            let rule = match p.mode {
                SocdMode::LastInputPriority => PairRule::LastInput,
                SocdMode::Neutral => PairRule::Neutral,
                SocdMode::KeyAPriority => PairRule::KeyA,
                SocdMode::KeyBPriority => PairRule::KeyB,
            };
            (p.key_a, p.key_b, rule, p.trigger_point, p.rapid_trigger)
        })
        .or_else(|| {
            he.snapshot
                .rapid_switch_pairs
                .iter()
                .find(|p| p.key_a == key || p.key_b == key)
                .map(|p| {
                    (
                        p.key_a,
                        p.key_b,
                        PairRule::Deeper,
                        p.trigger_point,
                        p.rapid_trigger,
                    )
                })
        })?;
    let caps = &he.capabilities;
    // The pair stores only an on/off Rapid Trigger flag, so it borrows the key's sensitivities,
    // raised to the smallest valid step because keys with Rapid Trigger off may hold 0 mm.
    let min_sensitivity = caps
        .rapid_trigger_min
        .unwrap_or(caps.actuation_step)
        .as_hundredths()
        .max(1);
    let member = |key: KeyIndex| {
        let mut params = key_params(he, key);
        params.actuation = trigger_point.as_hundredths();
        params.rapid_trigger = if rapid_trigger {
            let own = params.rapid_trigger;
            let sensitivity = |value: TravelHundredthsMm| {
                TravelHundredthsMm::from_hundredths(value.as_hundredths().max(min_sensitivity))
            };
            RapidTriggerConfig::new(
                true,
                false,
                sensitivity(own.press_sensitivity),
                sensitivity(own.release_sensitivity),
            )
        } else {
            RapidTriggerConfig::disabled()
        };
        params
    };
    Some(PairParams {
        key_a,
        key_b,
        rule,
        a: member(key_a),
        b: member(key_b),
    })
}

fn key_params(he: &HallEffectState, key: KeyIndex) -> TriggerParams {
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

/// Returns the tester column for the Triggers deck, or an empty placeholder on keyboards
/// that do not report live travel.
pub(crate) fn render(app: &AgnoPeriphApp, he: &HallEffectState) -> AnyElement {
    if !he.capabilities.has_live_travel() {
        return div().into_any_element();
    }
    app.tester_view.clone().into_any_element()
}

/// Height of the travel gauge's track, in pixels at the default text size.
const GAUGE_TRACK: f32 = 136.0;
const GAUGE_TOP: f32 = 8.0;

#[allow(clippy::too_many_arguments)]
fn render_column(
    key: KeyIndex,
    depth: u16,
    pressed: bool,
    params: TriggerParams,
    pair: Option<PairParams>,
    app: WeakEntity<AgnoPeriphApp>,
    now: Instant,
    live_key: Option<KeyIndex>,
    live_error: Option<&SharedString>,
    layout: &KeyLayout,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let mut summary = format!("Actuates at {} mm.", hundredths(params.actuation));
    if params.rapid_trigger.is_enabled() {
        summary.push_str(" Rapid Trigger is on.");
    }
    if let Some((pair, partner)) = pair.and_then(|p| Some((p, p.partner_of(key)?))) {
        summary = format!("{} with {}. {summary}", pair.badge(), layout.label(partner));
    }
    let (note, note_color): (SharedString, Hsla) = match live_error {
        Some(error) => (error.clone(), theme.danger),
        None => (summary.into(), theme.muted_foreground),
    };

    panel("key-tester", cx)
        .flex_1()
        .test_support()
        .child(
            h_flex()
                .gap_2()
                .min_h(dp(26.))
                .mb(dp(10.))
                .child(panel_title("Key tester"))
                .child(div().flex_1())
                .when(live_key.is_none(), |this| {
                    this.child(
                        div()
                            .text_size(dp(12.))
                            .text_color(theme.muted_foreground)
                            .child("Press any key"),
                    )
                }),
        )
        .child(
            h_flex()
                .items_start()
                .gap(dp(14.))
                .child(gauge(depth, pressed, params, cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .items_start()
                        .gap(dp(6.))
                        .child(keycap(layout.label(key), 44., 18., cx))
                        .child(depth_readout(depth, cx))
                        .child(div().id("tester-state").child(state_pill(pressed, cx)))
                        .child(
                            div()
                                .id("tester-note")
                                .text_size(dp(12.))
                                .text_color(note_color)
                                .child(note),
                        ),
                ),
        )
        .child(div().flex_1())
        .child(trace(params, app, now, cx))
        .into_any_element()
}

/// The key's travel from top to bottom, with its dead zones and actuation point.
fn gauge(depth: u16, pressed: bool, params: TriggerParams, cx: &App) -> Div {
    let theme = cx.theme();
    let colors = tokens(cx);
    let max = f32::from(params.max_travel.max(1));
    let y = |value: u16| (f32::from(value) / max).clamp(0.0, 1.0) * GAUGE_TRACK;
    let ticks = (0..=params.max_travel / 10).map(|tenth| {
        let is_major = tenth % 5 == 0;
        div()
            .absolute()
            .top(dp(GAUGE_TOP + y(tenth * 10)))
            .left(dp(if is_major { 22. } else { 27. }))
            .w(dp(if is_major { 10. } else { 5. }))
            .h(px(1.))
            .bg(colors.tick)
    });
    let labels = (0..=params.max_travel / 50).map(|half| {
        let value = half * 50;
        div()
            .absolute()
            .top(dp(GAUGE_TOP + y(value) - 6.))
            .left_0()
            .w(dp(18.))
            .text_right()
            .font_family(KEY_FONT)
            .text_size(dp(9.5))
            .line_height(dp(12.))
            .text_color(colors.faint)
            .child(format!("{}.{}", value / 100, value % 100 / 10))
    });
    let dead_zone = |height: f32| {
        div()
            .absolute()
            .left_0()
            .right_0()
            .h(dp(height))
            .bg(pattern_slash(colors.tick, 1.4, 4.0))
    };
    let rt = params.rapid_trigger;
    let band = rt.is_enabled().then(|| {
        let start = y(params
            .actuation
            .saturating_sub(rt.release_sensitivity.as_hundredths()));
        let end = y(params.actuation + rt.press_sensitivity.as_hundredths());
        (start, (end - start).max(1.0))
    });
    let track = div()
        .id("tester-meter")
        .absolute()
        .top(dp(GAUGE_TOP))
        .left(dp(38.))
        .w(dp(22.))
        .h(dp(GAUGE_TRACK))
        .rounded(dp(5.))
        .overflow_hidden()
        .bg(colors.raised)
        .child(dead_zone(y(params.top_deadzone)).top_0())
        .child(dead_zone(y(params.bottom_deadzone)).bottom_0())
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(dp(y(depth)))
                .bg(theme.primary.opacity(if pressed { 0.45 } else { 0.28 })),
        )
        .when(depth > 0, |this| {
            this.child(
                div()
                    .absolute()
                    .top(dp((y(depth) - 1.5).max(0.0)))
                    .left_0()
                    .right_0()
                    .h(dp(1.5))
                    .bg(theme.foreground),
            )
        })
        // Where Rapid Trigger releases and presses again around the actuation point.
        .when_some(band, |this, (start, height)| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .top(dp(start))
                    .w(dp(3.))
                    .h(dp(height))
                    .bg(theme.primary),
            )
        });
    let line = GAUGE_TOP + y(params.actuation);
    div()
        .relative()
        .flex_shrink_0()
        .w(dp(96.))
        .h(dp(GAUGE_TRACK + 2.0 * GAUGE_TOP))
        .children(ticks)
        .children(labels)
        .child(track)
        .child(
            div()
                .absolute()
                .top(dp(line - 1.1))
                .left(dp(31.))
                .w(dp(36.))
                .h(dp(2.2))
                .rounded_full()
                .bg(theme.primary)
                .shadow(vec![glow(theme.primary, 6.)]),
        )
        .child(
            div()
                .absolute()
                .top(dp(line - 7.))
                .left(dp(70.))
                .font_family(KEY_FONT)
                .font_weight(FontWeight::BOLD)
                .text_size(dp(10.5))
                .line_height(dp(14.))
                .text_color(theme.primary)
                .child(hundredths(params.actuation)),
        )
}

fn depth_readout(depth: u16, cx: &App) -> Div {
    h_flex()
        .items_baseline()
        .gap(dp(4.))
        .child(
            div()
                .id("tester-depth")
                .font_family(DISPLAY_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .font_features(tabular())
                .text_size(dp(34.))
                .line_height(relative(1.))
                .child(hundredths(depth)),
        )
        .child(
            div()
                .font_family(DISPLAY_FONT)
                .text_size(dp(14.))
                .text_color(cx.theme().muted_foreground)
                .child("mm"),
        )
}

fn state_pill(pressed: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let (fill, text) = if pressed {
        (tokens(cx).accent_soft, theme.primary)
    } else {
        (tokens(cx).raised, theme.muted_foreground)
    };
    h_flex()
        .h(dp(22.))
        .px(dp(9.))
        .rounded_full()
        .bg(fill)
        .text_size(dp(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(text)
        .child(if pressed { "Pressed" } else { "Released" })
}

/// The recent travel of the key as a scrolling waveform.
fn trace(
    params: TriggerParams,
    app: WeakEntity<AgnoPeriphApp>,
    now: Instant,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let trace_color = theme.primary.opacity(0.5);
    let max_travel = params.max_travel;
    let actuation = f32::from(params.actuation) / f32::from(max_travel.max(1));
    let waveform = canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            if let Some(app) = app.upgrade() {
                paint_trace(
                    bounds,
                    &app.read(cx).tester.trace,
                    now,
                    max_travel,
                    trace_color,
                    window,
                );
            }
        },
    )
    .size_full();
    div()
        .id("tester-trace")
        .relative()
        .flex_shrink_0()
        .w_full()
        .mt(dp(8.))
        .h(dp(24.))
        .rounded(theme.radius)
        .overflow_hidden()
        .bg(tokens(cx).raised)
        .child(waveform)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(relative(actuation.clamp(0.0, 1.0)))
                .h_px()
                .bg(theme.muted_foreground),
        )
}

/// Paints the continuous time-mapped travel waveform as a single GPU filled path.
fn paint_trace(
    bounds: Bounds<Pixels>,
    samples: &VecDeque<(Instant, u16)>,
    now: Instant,
    max_travel: u16,
    color: Hsla,
    window: &mut Window,
) {
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    if width <= 0.0 || height <= 0.0 || samples.is_empty() {
        return;
    }

    let window_secs = TRACE_WINDOW.as_secs_f32();
    let max = f32::from(max_travel.max(1));
    let depth_y = |d: u16| (f32::from(d) / max).clamp(0.0, 1.0) * height;

    let start = samples
        .partition_point(|&(time, _)| now.saturating_duration_since(time) >= TRACE_WINDOW)
        .saturating_sub(1);
    if !samples.iter().skip(start).any(|&(_, depth)| depth > 0) {
        return;
    }
    let sample_x = |time| {
        let age = now.saturating_duration_since(time).as_secs_f32();
        (1.0 - age / window_secs).clamp(0.0, 1.0) * width
    };
    let (first_time, first_depth) = samples[start];
    let x_start = sample_x(first_time).min((width - 1.5).max(0.0));
    let left = bounds.left();
    let top = bounds.top();

    let mut builder = PathBuilder::fill();
    builder.move_to(point(left + px(x_start), top));
    builder.line_to(point(left + px(x_start), top + px(depth_y(first_depth))));
    let mut previous = first_depth;
    let mut remaining = samples.iter().skip(start + 1).peekable();
    while let Some(&(time, depth)) = remaining.next() {
        // Interior points on a flat segment do not change the path.
        if depth != previous || remaining.peek().is_some_and(|&&(_, next)| next != depth) {
            builder.line_to(point(left + px(sample_x(time)), top + px(depth_y(depth))));
        }
        previous = depth;
    }
    builder.line_to(point(left + px(width), top + px(depth_y(previous))));
    builder.line_to(point(left + px(width), top));
    builder.close();

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use agnoperiph_core::{RapidTriggerConfig, TravelHundredthsMm};
    use gpui_kit::AppContext;

    use super::{KeyIndex, PairParams, PairRule, PairState, TriggerParams, is_pressed};

    #[gpui_kit::test]
    fn steady_travel_samples_do_not_invalidate_the_view(cx: &mut gpui_kit::TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        use crate::backend::MockPeripheralService;

        use super::{
            AgnoPeriphApp, Arc, Duration, KeyIndex, KeyTravelSample, LIVE_POLL_INTERVAL,
            TRACE_WINDOW,
        };

        cx.update(gpui_kit::init);
        let app = cx.update(|cx| {
            cx.new(|cx| AgnoPeriphApp::new(Arc::new(MockPeripheralService::new()), cx))
        });
        cx.run_until_parked();
        let invalidations = Rc::new(Cell::new(0));
        let _observe = cx.update(|cx| {
            let tester = app.read(cx).tester_view.clone();
            let count = invalidations.clone();
            cx.observe(&tester, move |_, _| count.set(count.get() + 1))
        });
        let start = cx.executor().now();
        let apply_sample = |cx: &mut gpui_kit::TestAppContext, offset, depth| {
            cx.update(|cx| {
                app.update(cx, |app, cx| {
                    let sample = KeyTravelSample::new(
                        KeyIndex::new(35),
                        TravelHundredthsMm::from_hundredths(depth),
                    );
                    app.finish_live_read(0, Ok(vec![sample]), start + offset, Duration::ZERO, cx);
                });
            });
        };
        apply_sample(cx, Duration::ZERO, 0);
        apply_sample(cx, LIVE_POLL_INTERVAL, 0);
        assert_eq!(invalidations.get(), 1);
        apply_sample(cx, LIVE_POLL_INTERVAL * 2, 150);
        assert_eq!(invalidations.get(), 2);
        let steady_at = TRACE_WINDOW + LIVE_POLL_INTERVAL * 3;
        apply_sample(cx, steady_at, 150);
        cx.update(|cx| {
            let tester = &app.read(cx).tester;
            assert!(!tester.trace_is_moving(start + steady_at));
            assert_eq!(tester.live_polls, 4);
        });
        assert_eq!(invalidations.get(), 2);
    }

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

    fn pair(rule: PairRule) -> PairState {
        let p = params(RapidTriggerConfig::disabled());
        PairState::new(PairParams {
            key_a: KeyIndex::new(49),
            key_b: KeyIndex::new(51),
            rule,
            a: p,
            b: p,
        })
    }

    #[test]
    fn last_input_socd_lets_the_newer_key_win_and_restores_the_held_one() {
        let mut s = pair(PairRule::LastInput);
        assert_eq!(s.step(200, 0), (true, false));
        assert_eq!(s.step(200, 150), (false, true));
        // A going deeper does not take it back; only a fresh press does.
        assert_eq!(s.step(300, 150), (false, true));
        assert_eq!(s.step(300, 0), (true, false));
        assert_eq!(s.step(50, 0), (false, false));
        assert_eq!(s.step(150, 0), (true, false));
    }

    #[test]
    fn neutral_and_priority_socd_resolve_both_keys_down() {
        let mut neutral = pair(PairRule::Neutral);
        assert_eq!(neutral.step(200, 0), (true, false));
        assert_eq!(neutral.step(200, 150), (false, false));
        assert_eq!(neutral.step(0, 150), (false, true));

        let mut key_a = pair(PairRule::KeyA);
        key_a.step(0, 200);
        assert_eq!(key_a.step(150, 200), (true, false));

        let mut key_b = pair(PairRule::KeyB);
        key_b.step(200, 0);
        assert_eq!(key_b.step(200, 150), (false, true));
    }

    #[test]
    fn rapid_switching_lets_the_deeper_key_win_and_keeps_it_on_a_tie() {
        let mut s = pair(PairRule::Deeper);
        assert_eq!(s.step(200, 150), (true, false));
        assert_eq!(s.step(200, 250), (false, true));
        assert_eq!(s.step(250, 250), (false, true));
        assert_eq!(s.step(260, 250), (true, false));
        assert_eq!(s.step(260, 0), (true, false));
    }

    #[test]
    fn pair_trigger_point_and_rapid_trigger_apply_per_key() {
        let mut p = params(RapidTriggerConfig::new(true, false, t(20), t(20)));
        p.actuation = 150;
        let mut s = PairState::new(PairParams {
            key_a: KeyIndex::new(49),
            key_b: KeyIndex::new(51),
            rule: PairRule::LastInput,
            a: p,
            b: p,
        });
        assert_eq!(s.step(120, 0), (false, false));
        assert_eq!(s.step(200, 0), (true, false));
        assert_eq!(s.step(175, 0), (false, false));
        assert_eq!(s.step(195, 0), (true, false));
    }

    #[gpui_kit::test]
    fn tester_keeps_the_socd_last_input_order_beyond_the_trace(cx: &mut gpui_kit::TestAppContext) {
        use crate::backend::{MOCK_KEYBOARD_ID, MockPeripheralService};

        use super::{
            AgnoPeriphApp, Arc, DeviceId, Duration, KeyTravelSample, LIVE_POLL_INTERVAL,
            TRACE_RETENTION,
        };

        // The mock RS7 pairs A (49) and D (51) for SOCD last-input priority at 1.50 mm.
        const KEY_A: u16 = 49;
        const KEY_D: u16 = 51;
        cx.update(gpui_kit::init);
        let app = cx.update(|cx| {
            cx.new(|cx| AgnoPeriphApp::new(Arc::new(MockPeripheralService::new()), cx))
        });
        cx.run_until_parked();
        cx.update(|cx| {
            app.update(cx, |app, cx| {
                app.select_device(DeviceId::new(MOCK_KEYBOARD_ID), cx);
            });
        });
        cx.run_until_parked();
        let start = cx.executor().now();
        let read = |cx: &mut gpui_kit::TestAppContext, offset, keys: &[(u16, u16)]| {
            let samples = keys
                .iter()
                .map(|&(key, depth)| KeyTravelSample::new(KeyIndex::new(key), t(depth)))
                .collect();
            cx.update(|cx| {
                app.update(cx, |app, cx| {
                    app.finish_live_read(0, Ok(samples), start + offset, Duration::ZERO, cx);
                    (app.tester_key(), app.tester_pair_pressed())
                })
            })
        };
        assert_eq!(
            read(cx, Duration::ZERO, &[(KEY_A, 250)]),
            (KeyIndex::new(KEY_A), Some(true))
        );
        assert_eq!(
            read(cx, LIVE_POLL_INTERVAL, &[(KEY_A, 250), (KEY_D, 180)]),
            (KeyIndex::new(KEY_A), Some(false))
        );
        // D overtaking A moves the tester to D, which still holds the last input.
        assert_eq!(
            read(cx, LIVE_POLL_INTERVAL * 2, &[(KEY_A, 250), (KEY_D, 300)]),
            (KeyIndex::new(KEY_D), Some(true))
        );
        assert_eq!(
            read(cx, LIVE_POLL_INTERVAL * 3, &[(KEY_A, 250), (KEY_D, 200)]),
            (KeyIndex::new(KEY_A), Some(false))
        );
        let held = TRACE_RETENTION + Duration::from_secs(1);
        assert_eq!(
            read(cx, held, &[(KEY_A, 250), (KEY_D, 200)]),
            (KeyIndex::new(KEY_A), Some(false))
        );
        assert_eq!(
            read(cx, held + LIVE_POLL_INTERVAL, &[(KEY_A, 250)]),
            (KeyIndex::new(KEY_A), Some(true))
        );
    }

    #[test]
    fn continuous_rapid_trigger_stays_active_above_the_actuation_point() {
        let p = params(RapidTriggerConfig::new(true, true, t(20), t(20)));
        assert!(!is_pressed([150, 60], &p));
        assert!(is_pressed([150, 60, 80], &p));
        assert!(!is_pressed([150, 60, 80, 5], &p));
    }
}
