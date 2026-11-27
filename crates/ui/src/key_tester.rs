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
    CoreError, DeviceId, KeyIndex, KeyTravelSample, RapidTriggerConfig, TravelHundredthsMm,
};
use gpui_kit::component::{ActiveTheme, Sizable, Theme, h_flex, tag::Tag, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, KeyboardTab, Route, mm, muted_note};
use crate::backend::{HallEffectState, PeripheralService};
use crate::keymap::KeyLayout;

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

enum PollAction {
    Read(Arc<dyn PeripheralService>, DeviceId, Option<KeyIndex>),
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
        let pressed = if params.rapid_trigger.is_enabled() {
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
            self.app.clone(),
            now,
            app.tester.live_key,
            app.tester.live_error.as_ref(),
            &layout,
            cx.theme(),
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
                    PollAction::Read(service, device, focus_key) => {
                        let requested_device = device.clone();
                        let started = cx.background_executor().now();
                        let reply = cx
                            .background_spawn(async move {
                                service.read_live_key_travel_focused(&device, focus_key)
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
        PollAction::Read(self.service.clone(), device, Some(self.tester_key()))
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

    pub fn tester_key(&self) -> KeyIndex {
        self.tester.live_key.unwrap_or(self.selected_key)
    }

    pub fn tester_depth(&self) -> u16 {
        self.tester.depth
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

/// Returns the tester column for the Triggers deck, or an empty placeholder on keyboards
/// that do not report live travel.
pub(crate) fn render(app: &AgnoPeriphApp, he: &HallEffectState) -> AnyElement {
    if !he.capabilities.has_live_travel() {
        return div().into_any_element();
    }
    app.tester_view.clone().into_any_element()
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_column(
    key: KeyIndex,
    depth: u16,
    pressed: bool,
    params: TriggerParams,
    app: WeakEntity<AgnoPeriphApp>,
    now: Instant,
    live_key: Option<KeyIndex>,
    live_error: Option<&SharedString>,
    layout: &KeyLayout,
    theme: &Theme,
) -> AnyElement {
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
        }));

    let trace_color = theme.primary.opacity(0.5);
    let max_travel = params.max_travel;
    let trace_canvas = canvas(
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

    let trace_view = div()
        .id("tester-trace")
        .relative()
        .w_full()
        .h_6()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .child(trace_canvas)
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
    // One note line: a reconnect/error state wins, then the live prompt, then the trigger summary.
    let note: SharedString = match (live_error, live_key) {
        (Some(err), _) => err.clone(),
        (None, None) => "Press any key on the keyboard.".into(),
        (None, Some(_)) => legend.into(),
    };

    v_flex()
        .id("key-tester")
        .w_40()
        .flex_shrink_0()
        .gap_2()
        .test_support()
        .child(
            h_flex().min_h_6().justify_between().gap_2().child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Key tester"),
            ),
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
        .child(muted_note(note, theme).id("tester-note"))
        .into_any_element()
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

    use super::{TriggerParams, is_pressed};

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

    #[test]
    fn continuous_rapid_trigger_stays_active_above_the_actuation_point() {
        let p = params(RapidTriggerConfig::new(true, true, t(20), t(20)));
        assert!(!is_pressed([150, 60], &p));
        assert!(is_pressed([150, 60, 80], &p));
        assert!(!is_pressed([150, 60, 80, 5], &p));
    }
}
