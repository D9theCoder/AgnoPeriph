//! Root window view: owns device state, selection, dry-run mode, the write queue,
//! persistent configuration and the automatic Game Profile switcher.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use agnoperiph_core::{CoreError, CoreResult, DeviceId, KeyIndex, ProfileSlot, TravelHundredthsMm};
use agnoperiph_transport::{PacketOutcome, PacketTraceEntry};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, Theme, ThemeMode, WindowExt,
    alert::Alert,
    button::{Button, ButtonVariant, ButtonVariants},
    h_flex,
    input::InputEvent,
    spinner::Spinner,
    switch::Switch,
    tab::{Tab, TabBar},
    tag::Tag,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::backend::{ApplyError, DeviceSnapshot, PeripheralService, SettingChange};
use crate::battery_alerts::BatteryAlertTracker;
use crate::controls::{Controls, InputRole, SliderRange, SliderRole};
use crate::draft::{ChangeId, DeviceDraft, DraftHistory};
use crate::game_profiles::{ActiveProcessWatcher, AppConfig, DetectedGame};
use crate::key_tester::TesterUi;
use crate::smooth_scroll::{ScrollIds, SmoothScroll};
use crate::system::{DetachedSystem, SystemIntegration};
use crate::updates::{GithubReleases, ReleaseSource, UpdateUi};
use crate::{
    calibration, game_profiles_view, hall_effect_panel, inspector_drawer, keyboard_config_panel,
    keymap_panel, macro_panel, pointer_panel, profile_bar, rgb_panel, sidebar,
};

/// How often the process watcher looks for configured games.
pub const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Notice shown above the workspace after a failed operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    title: SharedString,
    message: SharedString,
}

impl Banner {
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn message(&self) -> &SharedString {
        &self.message
    }
}

/// Which panel-local edits a write commits, so a successful write clears only those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DraftScope {
    Macros,
    None,
}

impl DraftScope {
    fn of(change: &SettingChange) -> Self {
        match change {
            SettingChange::Macros(_) => Self::Macros,
            _ => Self::None,
        }
    }
}

/// What runs once the pending edits have been applied, from the unsaved-changes guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterApply {
    SelectDevice(DeviceId),
    SwitchProfile { device: DeviceId, slot: u8 },
}

/// Why a queued write exists, which decides what happens after it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobOrigin {
    /// A user edit; clears the matching draft on success.
    User(DraftScope),
    /// Commits the unsaved edits. `steps[i]` names the change written by step `i`.
    ApplyDraft { steps: Vec<ChangeId> },
    /// Activates a profile slot, from the profile bar or the Game Profile switcher.
    ProfileSwitch {
        slot: u8,
        is_software: bool,
        is_automatic: bool,
    },
    /// Starts a calibration run; marks calibration active only once the write succeeds.
    CalibrationStart,
    /// Periodic calibration progress read; never marks the window busy.
    CalibrationPoll,
}

impl JobOrigin {
    fn is_background(&self) -> bool {
        matches!(self, Self::CalibrationPoll)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WriteJob {
    pub(crate) device: DeviceId,
    pub(crate) changes: Vec<SettingChange>,
    pub(crate) origin: JobOrigin,
}

/// Batches per-device automatic preset switches from one game launch/exit into one notification.
#[derive(Debug, Clone)]
pub(crate) struct PendingSwitchNotice {
    game_name: Option<String>,
    applied: Vec<(String, String)>,
}

/// Top-level destination shown in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Device,
    GameProfiles,
}

/// Sub-view of a keyboard's workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardTab {
    Triggers,
    Keybinds,
    Lighting,
    Macros,
    Advanced,
}

impl KeyboardTab {
    pub const ALL: [Self; 5] = [
        Self::Triggers,
        Self::Keybinds,
        Self::Lighting,
        Self::Macros,
        Self::Advanced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Triggers => "Triggers & paired keys",
            Self::Keybinds => "Keybinds",
            Self::Lighting => "Lighting",
            Self::Macros => "Macros",
            Self::Advanced => "Advanced",
        }
    }
}

type GameDiscovery = Arc<dyn Fn() -> Vec<DetectedGame> + Send + Sync>;

/// Construction options for [`AgnoPeriphApp`].
///
/// The default is fully detached from the host: no config file, no registry access,
/// no process polling and an empty game scan. The binary opts in with [`Self::live`].
#[non_exhaustive]
pub struct AppOptions {
    system: Arc<dyn SystemIntegration>,
    config: AppConfig,
    discover_games: GameDiscovery,
    watch_processes: bool,
    show_dev_tools: bool,
    check_updates: bool,
    releases: Arc<dyn ReleaseSource>,
}

impl std::fmt::Debug for AppOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppOptions")
            .field("watch_processes", &self.watch_processes)
            .field("show_dev_tools", &self.show_dev_tools)
            .field("check_updates", &self.check_updates)
            .finish_non_exhaustive()
    }
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            system: Arc::new(DetachedSystem),
            config: AppConfig::default(),
            discover_games: Arc::new(Vec::new),
            watch_processes: false,
            show_dev_tools: true,
            check_updates: false,
            releases: Arc::new(GithubReleases::default()),
        }
    }
}

impl AppOptions {
    /// Options for the real desktop app: config on disk, installed game scan, process watcher.
    pub fn live(system: Arc<dyn SystemIntegration>, config: AppConfig) -> Self {
        Self {
            system,
            config,
            discover_games: Arc::new(crate::game_profiles::discover_installed_games),
            watch_processes: true,
            show_dev_tools: cfg!(debug_assertions),
            check_updates: true,
            releases: Arc::new(GithubReleases::default()),
        }
    }

    pub fn with_dev_tools(mut self, enabled: bool) -> Self {
        self.show_dev_tools = enabled;
        self
    }

    pub fn with_system(mut self, system: Arc<dyn SystemIntegration>) -> Self {
        self.system = system;
        self
    }

    pub fn with_config(mut self, config: AppConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_game_discovery(
        mut self,
        discover: impl Fn() -> Vec<DetectedGame> + Send + Sync + 'static,
    ) -> Self {
        self.discover_games = Arc::new(discover);
        self
    }

    pub fn with_process_watcher(mut self, enabled: bool) -> Self {
        self.watch_processes = enabled;
        self
    }

    /// Whether the app checks for a newer release when it starts.
    pub fn with_update_check(mut self, enabled: bool) -> Self {
        self.check_updates = enabled;
        self
    }

    /// Replaces the release source, for tests and previews.
    pub fn with_release_source(mut self, source: Arc<dyn ReleaseSource>) -> Self {
        self.releases = source;
        self
    }
}

/// The AgnoPeriph main window.
///
/// Owns the loaded device snapshots and all view state. Every service call runs on the
/// background executor; replies to superseded refreshes are dropped by revision, and
/// writes run one job at a time from a queue so automatic switches never drop.
pub struct AgnoPeriphApp {
    pub(crate) service: Arc<dyn PeripheralService>,
    pub(crate) system: Arc<dyn SystemIntegration>,
    pub(crate) discover_games: GameDiscovery,
    devices: Vec<DeviceSnapshot>,
    pub(crate) selected: Option<DeviceId>,
    route: Route,
    /// Incremented by every refresh; a reply carrying an older revision is stale.
    revision: u64,
    has_loaded: bool,
    is_loading: bool,
    queue: VecDeque<WriteJob>,
    /// Origin of the write in flight. Only one runs at a time and it is never cancelled,
    /// because a superseded write has still reached the device.
    in_flight: Option<JobOrigin>,
    dry_run: bool,
    show_dev_tools: bool,
    is_inspector_open: bool,
    reconnect_attempts: u8,
    banner: Option<Banner>,
    pub(crate) releases: Arc<dyn ReleaseSource>,
    pub(crate) updates: UpdateUi,
    packets: Vec<PacketTraceEntry>,
    formatted_packets: Arc<Vec<inspector_drawer::FormattedPacket>>,
    suppressed_packets: usize,
    /// Highest packet sequence number explicitly cleared by the user; any older frames
    /// arriving from an in-flight refresh or write are dropped.
    cleared_through_sequence: u64,
    pub(crate) selected_key: KeyIndex,
    pub(crate) selected_keys: BTreeSet<KeyIndex>,
    /// Unsaved edits for `draft_device`, shown in the dirty bar until applied or discarded.
    pub(crate) draft: DeviceDraft,
    pub(crate) draft_device: Option<DeviceId>,
    pub(crate) history: DraftHistory,
    /// Selected device with the draft applied; `None` when nothing is staged.
    pub(crate) effective: Option<DeviceSnapshot>,
    /// Last discarded draft, restorable from the Discard notification.
    pub(crate) discarded: Option<(DeviceId, DeviceDraft)>,
    /// Why the last Apply stopped early; the dirty bar offers Retry.
    pub(crate) apply_failure: Option<SharedString>,
    pub(crate) after_apply: Option<AfterApply>,
    pub(crate) is_changes_open: bool,
    pub(crate) is_shortcuts_open: bool,
    pub(crate) tester: TesterUi,
    pub(crate) battery_alerts: BatteryAlertTracker,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) keyboard_tab: KeyboardTab,
    pub(crate) paired: hall_effect_panel::PairedKeysUi,
    pub(crate) keymap_ui: keymap_panel::KeymapUi,
    pub(crate) lighting: rgb_panel::LightingUi,
    pub(crate) macros_ui: macro_panel::MacroUi,
    pub(crate) games_ui: game_profiles_view::GamesUi,
    pub(crate) config: AppConfig,
    pub(crate) active_game: Option<String>,
    /// Profile slot last confirmed active per device by the Game Profile switcher.
    pub(crate) applied_targets: BTreeMap<String, u8>,
    /// Devices with an automatic switch queued or in flight.
    pending_targets: BTreeSet<String>,
    /// Coalesced Windows notification waiting for all queued switches of one game change to settle.
    pub(crate) pending_switch_notice: Option<PendingSwitchNotice>,
    pub(crate) auto_switch_note: Option<SharedString>,
    pub(crate) calibration: calibration::CalibrationUi,
    pub(crate) controls: Option<Controls>,
    pub(crate) window_handle: Option<AnyWindowHandle>,
    pub(crate) smooth_scroll: SmoothScroll,
    pending_save: Arc<Mutex<Option<AppConfig>>>,
    save_lock: Arc<Mutex<()>>,
    watcher: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    control_subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for AgnoPeriphApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgnoPeriphApp")
            .field("devices", &self.devices.len())
            .field("selected", &self.selected)
            .field("dry_run", &self.dry_run)
            .field("in_flight", &self.in_flight)
            .finish_non_exhaustive()
    }
}

impl AgnoPeriphApp {
    /// Creates the view with detached options. Call [`Self::attach_window`] before rendering.
    pub fn new(service: Arc<dyn PeripheralService>, cx: &mut Context<Self>) -> Self {
        Self::with_options(service, AppOptions::default(), cx)
    }

    /// Creates the view and starts the initial device scan.
    pub fn with_options(
        service: Arc<dyn PeripheralService>,
        options: AppOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let dry_run = service.is_dry_run();
        let check_updates = options.check_updates;
        crate::shortcuts::bind_keys(cx);
        let theme_subscription = cx.observe_global::<Theme>(|_, cx| cx.notify());
        let mut app = Self {
            service,
            system: options.system,
            discover_games: options.discover_games,
            devices: Vec::new(),
            selected: None,
            route: Route::Device,
            revision: 0,
            has_loaded: false,
            is_loading: false,
            queue: VecDeque::new(),
            in_flight: None,
            dry_run,
            show_dev_tools: options.show_dev_tools,
            is_inspector_open: options.show_dev_tools,
            reconnect_attempts: 0,
            banner: None,
            releases: options.releases,
            updates: UpdateUi::default(),
            packets: Vec::new(),
            formatted_packets: Arc::new(Vec::new()),
            suppressed_packets: 0,
            cleared_through_sequence: 0,
            selected_key: KeyIndex::new(0),
            selected_keys: BTreeSet::new(),
            draft: DeviceDraft::default(),
            draft_device: None,
            history: DraftHistory::default(),
            effective: None,
            discarded: None,
            apply_failure: None,
            after_apply: None,
            is_changes_open: false,
            is_shortcuts_open: false,
            tester: TesterUi::default(),
            battery_alerts: BatteryAlertTracker::default(),
            focus_handle: cx.focus_handle(),
            keyboard_tab: KeyboardTab::Triggers,
            paired: hall_effect_panel::PairedKeysUi::default(),
            keymap_ui: keymap_panel::KeymapUi::default(),
            lighting: rgb_panel::LightingUi::default(),
            macros_ui: macro_panel::MacroUi::default(),
            games_ui: game_profiles_view::GamesUi::default(),
            config: options.config,
            active_game: None,
            applied_targets: BTreeMap::new(),
            pending_targets: BTreeSet::new(),
            pending_switch_notice: None,
            auto_switch_note: None,
            calibration: calibration::CalibrationUi::default(),
            controls: None,
            window_handle: None,
            smooth_scroll: SmoothScroll::default(),
            pending_save: Arc::new(Mutex::new(None)),
            save_lock: Arc::new(Mutex::new(())),
            watcher: None,
            _subscriptions: vec![theme_subscription],
            control_subscriptions: Vec::new(),
        };
        if options.watch_processes {
            app.watcher = Some(Self::spawn_process_watcher(cx));
        }
        app.refresh(cx);
        if check_updates {
            app.check_for_updates(false, cx);
        }
        app
    }

    /// Creates the app entity for `window` in one step, as `open_window` expects.
    pub fn build(
        service: Arc<dyn PeripheralService>,
        options: AppOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let app = cx.new(|cx| Self::with_options(service, options, cx));
        app.update(cx, |app, cx| app.attach_window(window, cx));
        app
    }

    /// Binds the app to a (new) window: rebuilds window-bound controls and syncs them.
    ///
    /// The app outlives its window when it runs in the notification area, so this is
    /// called again whenever the window is reopened.
    pub fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_handle = Some(window.window_handle());
        self.rebuild_controls(window, cx);
        self.sync_controls(window, cx);
        self.focus_handle.focus(window, cx);
        self.update_live_polling(cx);
        cx.notify();
    }

    /// Called when the window closes but the app keeps running in the notification area.
    pub fn detach_window(&mut self, cx: &mut Context<Self>) {
        self.window_handle = None;
        self.is_changes_open = false;
        self.is_shortcuts_open = false;
        self.update_live_polling(cx);
    }

    pub(crate) fn has_window(&self) -> bool {
        self.window_handle.is_some()
    }

    fn device_ranges(&self) -> (SliderRange, SliderRange, f32) {
        let (travel, rt) = self
            .selected_device()
            .and_then(|d| d.hall_effect.as_ref())
            .map_or(
                (
                    SliderRange::new(10u16, 340u16, 2u16),
                    SliderRange::new(2u16, 340u16, 2u16),
                ),
                |he| {
                    let caps = &he.capabilities;
                    let travel = SliderRange::travel(
                        caps.actuation_min,
                        caps.actuation_max,
                        caps.actuation_step,
                    );
                    let rt = match (caps.rapid_trigger_min, caps.rapid_trigger_max) {
                        (Some(min), Some(max)) => {
                            SliderRange::travel(min, max, caps.actuation_step)
                        }
                        _ => travel,
                    };
                    (travel, rt)
                },
            );
        let speed = f32::from(
            self.selected_device()
                .and_then(|d| d.rgb.as_ref())
                .and_then(|rgb| {
                    rgb.capabilities
                        .zones
                        .iter()
                        .find(|z| z.id == self.lighting.zone)
                        .map(|z| z.max_speed)
                })
                .unwrap_or(255),
        );
        (travel, rt, speed)
    }

    fn rebuild_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (travel, rt, speed) = self.device_ranges();
        let (controls, subscriptions) = Controls::new(travel, rt, speed, window, cx);
        self.controls = Some(controls);
        self.control_subscriptions = subscriptions;
    }

    /// Pushes the current model values into sliders and inputs.
    pub(crate) fn sync_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (travel, rt, speed) = self.device_ranges();
        let needs_rebuild = self.controls.as_ref().is_some_and(|c| {
            c.travel_range != travel
                || c.rt_range != rt
                || (c.speed_max - speed).abs() > f32::EPSILON
        });
        if needs_rebuild {
            self.rebuild_controls(window, cx);
        }
        hall_effect_panel::sync_controls(self, window, cx);
        rgb_panel::sync_controls(self, window, cx);
    }

    /// Schedules [`Self::sync_controls`] once the current update finishes.
    pub(crate) fn request_sync(&self, cx: &mut Context<Self>) {
        let Some(handle) = self.window_handle else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.sync_controls(window, cx));
            });
        })
        .detach();
    }

    pub fn devices(&self) -> &[DeviceSnapshot] {
        &self.devices
    }

    pub fn device(&self, id: &DeviceId) -> Option<&DeviceSnapshot> {
        self.devices.iter().find(|device| device.id() == id)
    }

    pub fn selected_device_id(&self) -> Option<&DeviceId> {
        self.selected.as_ref()
    }

    /// The selected device as the editors show it: device values with unsaved edits applied.
    pub fn selected_device(&self) -> Option<&DeviceSnapshot> {
        let id = self.selected.as_ref()?;
        match &self.effective {
            Some(effective) if effective.id() == id => Some(effective),
            _ => self.device(id),
        }
    }

    /// The selected device's values as last read from (or written to) the device.
    pub(crate) fn selected_raw(&self) -> Option<&DeviceSnapshot> {
        self.device(self.selected.as_ref()?)
    }

    pub fn route(&self) -> Route {
        self.route
    }

    pub fn keyboard_tab(&self) -> KeyboardTab {
        self.keyboard_tab
    }

    /// Whether a calibration run is active on the selected keyboard.
    pub fn is_calibrating(&self) -> bool {
        self.calibration.is_active
    }

    /// Calibration progress reads completed since the current run started.
    pub fn calibration_polls(&self) -> usize {
        self.calibration.polls
    }

    pub fn selected_key(&self) -> KeyIndex {
        self.selected_key
    }

    pub fn selected_keys(&self) -> &BTreeSet<KeyIndex> {
        &self.selected_keys
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Id of the Game Profile whose executable is running, if auto switching matched one.
    pub fn active_game(&self) -> Option<&str> {
        self.active_game.as_deref()
    }

    /// Display name of the active Game Profile, or `None` on the desktop default.
    pub fn active_game_name(&self) -> Option<&str> {
        let id = self.active_game.as_deref()?;
        self.config.game_profile(id).map(|p| p.name.as_str())
    }

    /// Explains a part of a Game Profile switch that wasn't written automatically.
    pub fn auto_switch_note(&self) -> Option<&SharedString> {
        self.auto_switch_note.as_ref()
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn is_loading(&self) -> bool {
        self.is_loading
    }

    /// `true` while a user-visible write is in flight; background polls don't count.
    pub fn is_busy(&self) -> bool {
        self.in_flight.as_ref().is_some_and(|o| !o.is_background())
    }

    pub fn has_pending_writes(&self) -> bool {
        self.in_flight.is_some() || !self.queue.is_empty()
    }

    pub fn is_inspector_open(&self) -> bool {
        self.is_inspector_open
    }

    pub fn banner(&self) -> Option<&Banner> {
        self.banner.as_ref()
    }

    pub fn packets(&self) -> &[PacketTraceEntry] {
        &self.packets
    }

    pub(crate) fn formatted_packets(&self) -> &Arc<Vec<inspector_drawer::FormattedPacket>> {
        &self.formatted_packets
    }

    pub(crate) fn suppressed_packets(&self) -> usize {
        self.suppressed_packets
    }

    /// Re-enumerates devices. Previous data stays visible until the reply lands.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        self.run_refresh(cx, move || {
            let devices = service.refresh();
            (devices, service.packet_log(), service.is_dry_run())
        });
    }

    /// Switches dry-run mode. Device handles reopen in the new mode before any further write.
    pub fn set_dry_run(&mut self, dry_run: bool, cx: &mut Context<Self>) {
        if self.has_pending_writes() || dry_run == self.dry_run {
            return;
        }
        let service = self.service.clone();
        self.run_refresh(cx, move || {
            service.set_dry_run(dry_run);
            let devices = service.refresh();
            (devices, service.packet_log(), service.is_dry_run())
        });
    }

    fn run_refresh(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> (CoreResult<Vec<DeviceSnapshot>>, Vec<PacketTraceEntry>, bool)
        + Send
        + 'static,
    ) {
        self.revision += 1;
        let revision = self.revision;
        self.is_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = cx.background_spawn(async move { work() }).await;
            let _ = this.update(cx, |this, cx| this.finish_refresh(revision, reply, cx));
        })
        .detach();
    }

    fn set_packets(&mut self, packets: Vec<PacketTraceEntry>) {
        self.packets = packets
            .into_iter()
            .filter(|entry| entry.sequence > self.cleared_through_sequence)
            .collect();
        self.suppressed_packets = self
            .packets
            .iter()
            .filter(|p| p.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        self.formatted_packets = Arc::new(inspector_drawer::format_packets(&self.packets));
    }

    /// Clears panel-local edits (paired-key flow, lighting drag, macros). The unsaved
    /// device draft is separate: see [`Self::reset_draft`].
    fn clear_drafts(&mut self) {
        self.paired = hall_effect_panel::PairedKeysUi::default();
        self.lighting.clear_draft();
        self.macros_ui.clear_draft();
    }

    fn finish_refresh(
        &mut self,
        revision: u64,
        (devices, packets, dry_run): (CoreResult<Vec<DeviceSnapshot>>, Vec<PacketTraceEntry>, bool),
        cx: &mut Context<Self>,
    ) {
        if revision != self.revision {
            return;
        }
        self.is_loading = false;
        self.has_loaded = true;
        self.dry_run = dry_run;
        self.set_packets(packets);
        self.clear_drafts();
        self.keymap_ui.clear_history();
        self.apply_failure = None;
        self.after_apply = None;
        // Fresh handles hold whatever the device stores; the switcher re-checks every target.
        self.applied_targets.clear();
        match devices {
            Ok(devices) => {
                let previous = self.selected.clone();
                let needs_reconnect = devices.iter().any(|d| {
                    d.warnings
                        .iter()
                        .any(|w| crate::backend::is_disconnect_message(&w.message))
                });
                self.devices = devices;
                self.banner = None;
                let still_present = self
                    .selected
                    .as_ref()
                    .is_some_and(|id| self.devices.iter().any(|d| d.id() == id));
                if !still_present {
                    self.selected = self.devices.first().map(|d| d.id().clone());
                }
                if self.selected != previous {
                    self.reset_key_selection();
                    self.keymap_ui = keymap_panel::KeymapUi::default();
                }
                for device in &self.devices {
                    if device.profiles.is_none() && device.capabilities.has_hall_effect() {
                        self.config.ensure_software_profiles(device.id());
                    }
                }
                // Unsaved edits survive a refresh while their device is still connected.
                if self.draft_device.as_ref() != self.selected.as_ref() {
                    self.reset_draft();
                }
                self.renormalize_draft();
                self.observe_batteries(cx);
                if needs_reconnect {
                    self.schedule_auto_reconnect(cx);
                } else {
                    self.reconnect_attempts = 0;
                }
            }
            Err(err) => {
                self.devices.clear();
                self.selected = None;
                self.reset_draft();
                self.reset_key_selection();
                self.banner = Some(Banner {
                    title: "Couldn’t scan for devices".into(),
                    message: format!(
                        "{err}. Close vendor software that may hold the device, then refresh."
                    )
                    .into(),
                });
            }
        }
        self.update_live_polling(cx);
        self.request_sync(cx);
        self.pump_queue(cx);
        cx.notify();
    }

    /// Clears the Triggers selection. `selected_key` keeps pointing at a real key so the
    /// editors can show values, but nothing is applied until a key is clicked.
    fn reset_key_selection(&mut self) {
        self.selected_key = KeyIndex::new(0);
        self.selected_keys = BTreeSet::new();
    }

    /// Validates and applies one change to the selected device.
    ///
    /// User edits are ignored while a write or refresh is running; their controls are
    /// disabled in that state, so nothing the user sees is silently dropped.
    pub(crate) fn apply_change(&mut self, change: SettingChange, cx: &mut Context<Self>) {
        let scope = DraftScope::of(&change);
        self.apply_changes(vec![change], JobOrigin::User(scope), cx);
    }

    /// Applies several changes to the selected device as one job, stopping at the first failure.
    pub(crate) fn apply_changes(
        &mut self,
        changes: Vec<SettingChange>,
        origin: JobOrigin,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.is_loading {
            return;
        }
        let Some(device) = self.selected.clone() else {
            return;
        };
        self.enqueue(
            WriteJob {
                device,
                changes,
                origin,
            },
            cx,
        );
    }

    /// Queues a write job. Jobs run strictly one after another.
    pub(crate) fn enqueue(&mut self, job: WriteJob, cx: &mut Context<Self>) {
        self.queue.push_back(job);
        self.pump_queue(cx);
        cx.notify();
    }

    fn pump_queue(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_some() || self.is_loading {
            return;
        }
        let Some(job) = self.queue.pop_front() else {
            return;
        };
        self.in_flight = Some(job.origin.clone());
        let service = self.service.clone();
        let WriteJob {
            device,
            changes,
            origin,
        } = job;
        cx.spawn(async move |this, cx| {
            let device_for_reply = device.clone();
            let reply = cx
                .background_spawn(async move {
                    let total = changes.len();
                    let mut last = None;
                    let mut failure = None;
                    for (ix, change) in changes.into_iter().enumerate() {
                        match service.apply(&device, change) {
                            Ok(snapshot) => last = Some(snapshot),
                            Err(err) => {
                                failure = Some((ix, total, err));
                                break;
                            }
                        }
                    }
                    (last, failure, service.packet_log())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_job(&device_for_reply, &origin, reply, cx);
            });
        })
        .detach();
    }

    #[allow(clippy::type_complexity)]
    fn finish_job(
        &mut self,
        device: &DeviceId,
        origin: &JobOrigin,
        (last, failure, packets): (
            Option<DeviceSnapshot>,
            Option<(usize, usize, ApplyError)>,
            Vec<PacketTraceEntry>,
        ),
        cx: &mut Context<Self>,
    ) {
        self.in_flight = None;
        self.set_packets(packets);
        if let Some(snapshot) = last {
            let needs_reconnect = snapshot
                .warnings
                .iter()
                .any(|w| crate::backend::is_disconnect_message(&w.message));
            self.store_snapshot(snapshot);
            self.observe_batteries(cx);
            if needs_reconnect {
                self.schedule_auto_reconnect(cx);
            } else {
                self.reconnect_attempts = 0;
            }
        }
        match failure {
            None => {
                if !origin.is_background() {
                    self.banner = None;
                }
                self.finish_origin(device, origin, cx);
            }
            Some((step, total, failure)) => {
                if let JobOrigin::ApplyDraft { steps } = origin {
                    self.finish_partial_apply(device, steps, step, &failure.error);
                }
                self.report_failure(device, origin, step, total, failure, cx);
            }
        }
        self.renormalize_draft();
        if let JobOrigin::ProfileSwitch {
            is_automatic: true, ..
        } = origin
        {
            self.pending_targets.remove(device.as_str());
            self.maybe_flush_switch_notification(cx);
        }
        self.request_sync(cx);
        self.pump_queue(cx);
        cx.notify();
    }

    fn schedule_auto_reconnect(&mut self, cx: &mut Context<Self>) {
        if self.reconnect_attempts >= 4 || self.is_loading {
            return;
        }
        self.reconnect_attempts += 1;
        let expected_rev = self.revision;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.revision == expected_rev && !this.has_pending_writes() && !this.is_loading {
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    fn store_snapshot(&mut self, snapshot: DeviceSnapshot) {
        if let Some(slot) = self.devices.iter_mut().find(|d| d.id() == snapshot.id()) {
            *slot = snapshot;
        }
        self.sync_effective();
    }

    fn finish_origin(&mut self, device: &DeviceId, origin: &JobOrigin, cx: &mut Context<Self>) {
        match origin {
            JobOrigin::User(scope) => {
                match scope {
                    DraftScope::Macros => self.macros_ui.clear_draft(),
                    DraftScope::None => {}
                }
                // Keep the active software preset synchronized with every confirmed user edit.
                self.snapshot_active_software_preset(device);
            }
            JobOrigin::ApplyDraft { .. } => {
                if self.draft_device.as_ref() == Some(device) {
                    self.draft = DeviceDraft::default();
                    self.history.clear();
                    self.sync_effective();
                }
                self.apply_failure = None;
                self.is_changes_open = false;
                self.snapshot_active_software_preset(device);
                if let Some(next) = self.after_apply.take() {
                    self.run_after_apply(next, cx);
                }
            }
            JobOrigin::ProfileSwitch {
                slot,
                is_software,
                is_automatic,
            } => {
                if *is_software {
                    self.config.ensure_software_profiles(device).active_slot = *slot;
                    self.save_config();
                }
                if *is_automatic {
                    self.applied_targets
                        .insert(device.as_str().to_owned(), *slot);
                    self.record_auto_switch(device, *slot);
                }
            }
            JobOrigin::CalibrationStart => {
                self.calibration.is_starting = false;
                self.calibration.is_active = true;
                self.calibration.polls = 0;
            }
            JobOrigin::CalibrationPoll => self.calibration.polls += 1,
        }
    }

    fn report_failure(
        &mut self,
        device: &DeviceId,
        origin: &JobOrigin,
        step: usize,
        total: usize,
        failure: ApplyError,
        cx: &mut Context<Self>,
    ) {
        self.paired.cancel_pick();
        if matches!(origin, JobOrigin::CalibrationStart) {
            self.calibration.is_starting = false;
            self.calibration.is_active = false;
        }
        let refreshed = failure.refreshed_snapshot.is_some();
        let disconnect_failed = crate::backend::is_disconnect_message(&failure.error.to_string())
            || failure.refreshed_snapshot.as_ref().is_some_and(|s| {
                s.warnings
                    .iter()
                    .any(|w| crate::backend::is_disconnect_message(&w.message))
            });
        if let Some(snapshot) = failure.refreshed_snapshot {
            self.store_snapshot(*snapshot);
        }
        if disconnect_failed && !self.dry_run {
            self.schedule_auto_reconnect(cx);
        }
        let device_name = self
            .device(device)
            .map_or_else(|| device.to_string(), |d| d.info.model_name.clone());
        let step_note = if total > 1 {
            format!(
                " Step {} of {total} failed; earlier steps were applied.",
                step + 1
            )
        } else {
            String::new()
        };
        let message = if failure.rejected_before_send
            && matches!(failure.error, CoreError::UnsupportedCapability(_))
        {
            format!(
                "{device_name} doesn’t accept this change from AgnoPeriph ({}). Nothing was written.{step_note}",
                failure.error
            )
        } else if failure.rejected_before_send {
            format!(
                "{}. Nothing was written for this step. Adjust the value, or refresh if the device was disconnected.{step_note}",
                failure.error
            )
        } else if self.dry_run {
            format!(
                "{}. Dry run is enabled, so no write reached the device.{step_note}",
                failure.error
            )
        } else if refreshed {
            format!(
                "{}. Some frames may have already reached the device; the view was refreshed with the device’s current state.{step_note}",
                failure.error
            )
        } else {
            format!(
                "{}. Some frames may have already reached the device. Refresh to verify its current state.{step_note}",
                failure.error
            )
        };
        let title = match origin {
            JobOrigin::ProfileSwitch {
                is_automatic: true, ..
            } => format!("Couldn’t switch {device_name} automatically"),
            JobOrigin::ProfileSwitch { .. } => format!("Couldn’t switch {device_name}’s profile"),
            _ => "Couldn’t apply the change".to_owned(),
        };
        if let JobOrigin::ProfileSwitch {
            slot,
            is_automatic: true,
            ..
        } = origin
        {
            // Remember the failed target so the switcher doesn't retry it every poll.
            self.applied_targets
                .insert(device.as_str().to_owned(), *slot);
        }
        self.banner = Some(Banner {
            title: title.into(),
            message: message.into(),
        });
    }

    /// Shows `device`. Unsaved edits on the previous device are dropped; use
    /// [`Self::request_select_device`] to ask first.
    pub fn select_device(&mut self, device: DeviceId, cx: &mut Context<Self>) {
        if self.route == Route::Device && self.selected.as_ref() == Some(&device) {
            return;
        }
        let changed = self.selected.as_ref() != Some(&device);
        self.route = Route::Device;
        if changed {
            self.selected = Some(device);
            self.reset_key_selection();
            self.clear_drafts();
            self.reset_draft();
            self.keymap_ui = keymap_panel::KeymapUi::default();
            self.lighting = rgb_panel::LightingUi::default();
            self.macros_ui = macro_panel::MacroUi::default();
            self.tester.reset();
        }
        self.smooth_scroll.reset();
        self.update_live_polling(cx);
        self.request_sync(cx);
        cx.notify();
    }

    /// Shows `device`, asking first when the current device has unsaved edits.
    pub fn request_select_device(
        &mut self,
        device: DeviceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_ref() != Some(&device) && self.is_dirty() {
            let name = self
                .device(&device)
                .map_or_else(|| device.to_string(), |d| d.info.model_name.clone());
            crate::dirty_bar::open_guard(self, AfterApply::SelectDevice(device), &name, window, cx);
            return;
        }
        self.select_device(device, cx);
    }

    pub fn open_game_profiles(&mut self, cx: &mut Context<Self>) {
        if self.route != Route::GameProfiles {
            self.route = Route::GameProfiles;
            self.is_changes_open = false;
            self.smooth_scroll.reset();
            self.update_live_polling(cx);
            cx.notify();
        }
    }

    pub fn set_keyboard_tab(&mut self, tab: KeyboardTab, cx: &mut Context<Self>) {
        if self.keyboard_tab != tab {
            self.keyboard_tab = tab;
            self.paired.cancel_pick();
            self.smooth_scroll.reset();
            self.update_live_polling(cx);
            self.request_sync(cx);
            cx.notify();
        }
    }

    /// Keyboard tabs the selected device offers, in order.
    pub fn keyboard_tabs(&self) -> Vec<KeyboardTab> {
        let Some(device) = self.selected_device() else {
            return Vec::new();
        };
        let caps = device.capabilities;
        let is_keyboard = caps.has_hall_effect()
            || caps.has_keymap()
            || caps.has_macros()
            || caps.has_keyboard_config();
        if !is_keyboard {
            return Vec::new();
        }
        KeyboardTab::ALL
            .into_iter()
            .filter(|tab| match tab {
                KeyboardTab::Triggers => caps.has_hall_effect(),
                KeyboardTab::Keybinds => caps.has_keymap(),
                KeyboardTab::Lighting => caps.has_rgb(),
                KeyboardTab::Macros => caps.has_macros(),
                KeyboardTab::Advanced => caps.has_keyboard_config(),
            })
            .collect()
    }

    /// Moves to the next (`forward`) or previous keyboard tab, wrapping around.
    pub fn cycle_keyboard_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let tabs = self.keyboard_tabs();
        if tabs.is_empty() || self.route != Route::Device {
            return;
        }
        let current = tabs
            .iter()
            .position(|t| *t == self.keyboard_tab)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % tabs.len()
        } else {
            (current + tabs.len() - 1) % tabs.len()
        };
        self.set_keyboard_tab(tabs[next], cx);
    }

    /// Feeds `key` to a pending paired-key pick. Returns `false` when no pick is armed.
    ///
    /// A key already bound to an advanced function (RS, SOCD, DKS, MT, TGL) is refused
    /// with an inline note and the pick stays armed, so the user can choose another key.
    fn route_pick(&mut self, key: KeyIndex, cx: &mut Context<Self>) -> bool {
        if self.paired.pick.is_none() {
            return false;
        }
        let owner = self.selected_device().and_then(|device| {
            crate::backend::occupied_advanced_keys(device)
                .get(&key)
                .copied()
        });
        if let Some(owner) = owner {
            let label = self
                .selected_device()
                .and_then(|d| {
                    d.hall_effect
                        .as_ref()
                        .map(|he| crate::keymap::KeyLayout::for_device(&d.info, &he.capabilities))
                })
                .map_or_else(|| format!("Key {}", key.value()).into(), |l| l.label(key));
            self.paired.conflict =
                Some(format!("{label} is already used by {owner}. Remove it there first.").into());
        } else {
            self.paired.conflict = None;
            self.paired.accept_pick(key);
        }
        cx.notify();
        true
    }

    /// Triggers matrix click: adds an unselected key, removes a selected one.
    ///
    /// Keys owned by RS or SOCD are managed by their pair's dedicated travel and Rapid Trigger
    /// settings, so clicking them outside a pick focuses that mode instead of selecting them
    /// for regular per-key actuation edits.
    pub fn toggle_key(&mut self, key: KeyIndex, cx: &mut Context<Self>) {
        if self.route_pick(key, cx) {
            return;
        }
        if let Some(pair_mode) = self.pair_managed_mode(key) {
            self.paired.mode = pair_mode;
            self.selected_keys.remove(&key);
            cx.notify();
            return;
        }
        if self.selected_keys.remove(&key) {
            if self.selected_key == key
                && let Some(&next) = self.selected_keys.iter().next_back()
            {
                self.selected_key = next;
            }
        } else {
            self.selected_keys.insert(key);
            self.selected_key = key;
        }
        self.history.end_group();
        self.request_sync(cx);
        cx.notify();
    }

    /// Returns the paired mode (`Socd` or `RapidSwitch`) owning `key`, if any.
    pub(crate) fn pair_managed_mode(&self, key: KeyIndex) -> Option<hall_effect_panel::PairedMode> {
        let he = self.hall_effect_state()?;
        if he
            .snapshot
            .socd_pairs
            .iter()
            .any(|p| p.key_a == key || p.key_b == key)
        {
            return Some(hall_effect_panel::PairedMode::Socd);
        }
        if he
            .snapshot
            .rapid_switch_pairs
            .iter()
            .any(|p| p.key_a == key || p.key_b == key)
        {
            return Some(hall_effect_panel::PairedMode::RapidSwitch);
        }
        None
    }

    /// Selects `key` alone, or toggles it in a multi-key selection when `extend` is set.
    pub fn select_key(&mut self, key: KeyIndex, extend: bool, cx: &mut Context<Self>) {
        if self.route_pick(key, cx) {
            return;
        }
        if extend {
            if self.selected_keys.contains(&key) && self.selected_keys.len() > 1 {
                self.selected_keys.remove(&key);
                if self.selected_key == key {
                    self.selected_key = *self.selected_keys.iter().next().unwrap_or(&key);
                }
            } else {
                self.selected_keys.insert(key);
                self.selected_key = key;
            }
        } else {
            if self.selected_key == key && self.selected_keys.len() == 1 {
                return;
            }
            self.selected_key = key;
            self.selected_keys = BTreeSet::from([key]);
        }
        self.history.end_group();
        self.request_sync(cx);
        cx.notify();
    }

    pub fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.is_inspector_open = !self.is_inspector_open;
        cx.notify();
    }

    pub fn clear_packets(&mut self, cx: &mut Context<Self>) {
        if let Some(last) = self.packets.last() {
            self.cleared_through_sequence = self.cleared_through_sequence.max(last.sequence);
        }
        self.packets.clear();
        self.suppressed_packets = 0;
        self.formatted_packets = Arc::new(Vec::new());
        // Clearing only touches the sink's internal log buffer, never the HID state mutex.
        self.service.clear_packet_log();
        cx.notify();
    }

    /// Persists the configuration off the UI thread. The newest snapshot always wins.
    pub(crate) fn save_config(&self) {
        let pending = self.pending_save.clone();
        let lock = self.save_lock.clone();
        let system = self.system.clone();
        *crate::backend_lock(&pending) = Some(self.config.clone());
        std::thread::spawn(move || {
            let _guard = crate::backend_lock(&lock);
            let next = crate::backend_lock(&pending).take();
            if let Some(config) = next {
                let _ = system.save_config(&config);
            }
        });
    }

    pub fn set_close_to_tray(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.config.close_to_tray == enabled {
            return;
        }
        self.config.close_to_tray = enabled;
        if self.config.start_with_windows {
            let _ = self.system.set_start_with_windows(true, enabled);
        }
        self.save_config();
        cx.notify();
    }

    pub fn set_start_with_windows(&mut self, enabled: bool, cx: &mut Context<Self>) {
        match self
            .system
            .set_start_with_windows(enabled, self.config.close_to_tray)
        {
            Ok(()) => {
                self.config.start_with_windows = enabled;
                self.save_config();
            }
            Err(err) => {
                self.banner = Some(Banner {
                    title: "Couldn’t change Start with Windows".into(),
                    message: format!("{err}.").into(),
                });
            }
        }
        cx.notify();
    }

    // ---- Game Profile switcher -------------------------------------------------------

    fn spawn_process_watcher(cx: &mut Context<Self>) -> Task<()> {
        let watcher = Arc::new(Mutex::new(ActiveProcessWatcher::new()));
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROCESS_POLL_INTERVAL).await;
                let watcher = watcher.clone();
                let exes = cx
                    .background_spawn(async move { crate::backend_lock(&watcher).running_exes() })
                    .await;
                if this
                    .update(cx, |this, cx| this.handle_running_exes(&exes, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Matches running executables against Game Profiles and queues profile switches.
    ///
    /// The process watcher calls this every [`PROCESS_POLL_INTERVAL`]; tests call it directly.
    pub fn handle_running_exes(&mut self, running: &HashSet<String>, cx: &mut Context<Self>) {
        let matched = self
            .config
            .match_running_game(running)
            .map(|p| p.id.clone());
        let previous_game = self.active_game.clone();
        let mut changed = false;
        if matched != previous_game {
            self.active_game = matched;
            self.applied_targets.clear();
            self.auto_switch_note = None;
            self.pending_switch_notice = if self.config.auto_switch_enabled
                && self.config.notify_on_game_switch
                && (self.active_game.is_some() || previous_game.is_some())
            {
                let game_name = self
                    .active_game
                    .as_deref()
                    .and_then(|id| self.config.game_profile(id))
                    .map(|p| p.name.clone());
                Some(PendingSwitchNotice {
                    game_name,
                    applied: Vec::new(),
                })
            } else {
                None
            };
            changed = true;
        }
        if !self.config.auto_switch_enabled || self.is_loading {
            if changed {
                cx.notify();
            }
            return;
        }
        let targets = match self.active_game.as_deref() {
            Some(id) => self
                .config
                .game_profile(id)
                .map_or_default(|p| p.device_targets.clone()),
            None => self.config.default_targets.clone(),
        };
        let device_ids: Vec<DeviceId> = self
            .devices
            .iter()
            .map(|d| d.id().clone())
            // A device with unsaved edits switches once they are applied or discarded.
            .filter(|id| !(self.is_dirty() && self.draft_device.as_ref() == Some(id)))
            .collect();
        for device in device_ids {
            let key = device.as_str().to_owned();
            let Some(&slot) = targets.get(&key) else {
                continue;
            };
            if self.applied_targets.get(&key) == Some(&slot) || self.pending_targets.contains(&key)
            {
                continue;
            }
            if self.current_slot(&device) == Some(slot) {
                self.applied_targets.insert(key, slot);
                continue;
            }
            if let Some(job) = self.profile_switch_job(&device, slot, true) {
                self.pending_targets.insert(key);
                self.queue.push_back(job);
                changed = true;
            } else {
                self.record_auto_switch(&device, slot);
                self.applied_targets.insert(key, slot);
            }
        }
        self.maybe_flush_switch_notification(cx);
        if changed {
            self.pump_queue(cx);
            cx.notify();
        }
    }

    fn record_auto_switch(&mut self, device: &DeviceId, slot: u8) {
        if self.pending_switch_notice.is_none() {
            return;
        }
        let device_name = self
            .device(device)
            .map_or_else(|| device.to_string(), |d| d.info.model_name.clone());
        let preset_name = self.config.preset_name(device, ProfileSlot::new(slot));
        if let Some(notice) = self.pending_switch_notice.as_mut() {
            notice.applied.push((device_name, preset_name));
        }
    }

    fn maybe_flush_switch_notification(&mut self, cx: &mut Context<Self>) {
        if !self.pending_targets.is_empty() {
            return;
        }
        let Some(notice) = self.pending_switch_notice.take() else {
            return;
        };
        if !self.config.notify_on_game_switch || notice.applied.is_empty() {
            return;
        }
        let (title, body) =
            crate::system::format_switch_notification(notice.game_name.as_deref(), &notice.applied);
        let system = self.system.clone();
        cx.background_spawn(async move {
            let _ = system.send_notification(&title, &body);
        })
        .detach();
    }

    /// The device's active profile slot: onboard for hardware profiles, host-managed otherwise.
    pub fn current_slot(&self, device: &DeviceId) -> Option<u8> {
        let snapshot = self.device(device)?;
        if let Some(profiles) = &snapshot.profiles {
            return Some(profiles.snapshot.active_slot.index());
        }
        self.config
            .software_profiles
            .get(device.as_str())
            .map(|sw| sw.active_slot)
    }

    /// Builds the job that activates `slot` on `device`, or `None` when nothing needs writing.
    ///
    /// For software-profile keyboards (e.g. ATK RS7), automatically saves the currently active
    /// slot's live state before switching away and computes a full diff (including clearing any
    /// SOCD/RS/DKS/MT/TGL bindings not present in the target preset).
    pub(crate) fn profile_switch_job(
        &mut self,
        device: &DeviceId,
        slot: u8,
        is_automatic: bool,
    ) -> Option<WriteJob> {
        let snapshot = self.device(device)?.clone();
        if snapshot.profiles.is_some() {
            return Some(WriteJob {
                device: device.clone(),
                changes: vec![SettingChange::Profile(ProfileSlot::new(slot))],
                origin: JobOrigin::ProfileSwitch {
                    slot,
                    is_software: false,
                    is_automatic,
                },
            });
        }
        let current_active = self.config.ensure_software_profiles(device).active_slot;
        if current_active != slot {
            self.snapshot_active_software_preset(device);
        }
        let preset = self
            .config
            .ensure_software_profiles(device)
            .slots
            .iter()
            .find(|s| s.slot == slot)
            .map_or_default(|s| s.saved.clone());
        let changes = profile_bar::preset_changes_for(&preset, &snapshot);
        if changes.is_empty() {
            self.config.ensure_software_profiles(device).active_slot = slot;
            self.save_config();
            return None;
        }
        Some(WriteJob {
            device: device.clone(),
            changes,
            origin: JobOrigin::ProfileSwitch {
                slot,
                is_software: true,
                is_automatic,
            },
        })
    }

    // ---- Control events --------------------------------------------------------------

    pub(crate) fn on_slider(
        &mut self,
        role: SliderRole,
        value: f32,
        released: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match role {
            SliderRole::Travel
            | SliderRole::PairTravel
            | SliderRole::RtPress
            | SliderRole::RtRelease
            | SliderRole::DeadzoneTop
            | SliderRole::DeadzoneBottom => {
                hall_effect_panel::on_slider(self, role, value, released, cx);
            }
            SliderRole::Brightness | SliderRole::Speed => {
                rgb_panel::on_slider(self, role, value, released, window, cx);
            }
        }
    }

    pub(crate) fn on_input(
        &mut self,
        role: InputRole,
        event: &InputEvent,
        value: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match role {
            InputRole::Hex => rgb_panel::on_hex_input(self, event, &value, window, cx),
            InputRole::KeySearch => {
                if matches!(event, InputEvent::Change) {
                    self.keymap_ui.search = value;
                    cx.notify();
                }
            }
            InputRole::GameSearch => {
                if matches!(event, InputEvent::Change) {
                    self.games_ui.search = value;
                    cx.notify();
                }
            }
            InputRole::Rename
            | InputRole::ManualName
            | InputRole::ManualExe
            | InputRole::ProfileExe => {
                if let InputEvent::PressEnter { .. } = event {
                    game_profiles_view::on_enter(self, role, window, cx);
                }
            }
        }
    }

    // ---- Header and chrome -----------------------------------------------------------

    fn request_dry_run_change(
        &mut self,
        dry_run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_pending_writes() || self.is_loading || dry_run == self.dry_run {
            return;
        }
        if dry_run {
            self.set_dry_run(true, cx);
            return;
        }
        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            alert
                .confirm()
                .title("Enable live device writes?")
                .description(
                    "Changes will be sent directly to connected peripherals. Fully exit each vendor’s own software, including system tray icons, before continuing.",
                )
                .ok_text("Enable live writes")
                .ok_variant(ButtonVariant::Warning)
                .cancel_text("Keep dry run")
                .on_ok(move |_, _, cx| {
                    let _ = app.update(cx, |this, cx| this.set_dry_run(false, cx));
                    true
                })
        });
    }

    fn toggle_theme(window: &mut Window, cx: &mut Context<Self>) {
        let next = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(next, Some(window), cx);
    }

    fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banner = None;
        cx.notify();
    }

    pub(crate) fn show_banner(
        &mut self,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
    ) {
        self.banner = Some(Banner {
            title: title.into(),
            message: message.into(),
        });
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shortcuts = crate::dirty_bar::shortcuts_button(self, cx);
        let theme = cx.theme();
        let is_dark = theme.mode.is_dark();
        let write_state = if self.dry_run {
            Tag::secondary().small().child("Writes held back")
        } else {
            Tag::warning().small().child("Writes go to devices")
        };
        let locked = self.has_pending_writes() || self.is_loading;

        h_flex()
            .id("header")
            .flex_shrink_0()
            .h_12()
            .px_4()
            .gap_3()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(logo_mark(theme))
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("AgnoPeriph"),
                    )
                    .child(
                        div()
                            .ml_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(device_count_label(self.devices.len())),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when(self.show_dev_tools, |this| {
                        this.child(
                            Switch::new("dry-run")
                                .label("Dry run")
                                .checked(self.dry_run)
                                .disabled(locked)
                                .tooltip("Validate and log writes without sending them to devices")
                                .on_change(cx.listener(|this, checked: &bool, window, cx| {
                                    this.request_dry_run_change(*checked, window, cx);
                                })),
                        )
                        .child(write_state)
                        .child(div().w_px().h_5().bg(theme.border))
                    })
                    .child(
                        Button::new("refresh")
                            .outline()
                            .small()
                            .icon(IconName::RefreshCw)
                            .label("Refresh")
                            .loading(self.is_loading)
                            .disabled(self.has_pending_writes())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(shortcuts)
                    .child(
                        Button::new("theme-toggle")
                            .ghost()
                            .small()
                            .icon(if is_dark {
                                IconName::Sun
                            } else {
                                IconName::Moon
                            })
                            .tooltip(if is_dark {
                                "Use light appearance"
                            } else {
                                "Use dark appearance"
                            })
                            .on_click(cx.listener(|_, _, window, cx| {
                                Self::toggle_theme(window, cx);
                            })),
                    )
                    .when(self.show_dev_tools, |this| {
                        this.child(
                            Button::new("inspector-toggle")
                                .ghost()
                                .small()
                                .icon(IconName::PanelBottom)
                                .selected(self.is_inspector_open)
                                .tooltip("HID packet inspector")
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_inspector(cx))),
                        )
                    }),
            )
    }

    fn render_notices(&self, cx: &mut Context<Self>) -> Div {
        let update = crate::updates::render_notice(self, cx);
        v_flex()
            .gap_3()
            .when_some(update, gpui_kit::ParentElement::child)
            .when_some(self.banner.clone(), |this, banner| {
                this.child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            Alert::error("error-banner", banner.message)
                                .title(banner.title)
                                .flex_1(),
                        )
                        .child(
                            Button::new("dismiss-banner")
                                .ghost()
                                .small()
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| this.dismiss_banner(cx))),
                        ),
                )
            })
    }

    fn render_workspace(
        &self,
        workspace_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let background = cx.theme().background;
        let notices = self.render_notices(cx);
        let body = match self.route {
            Route::GameProfiles => {
                game_profiles_view::render(self, workspace_width, cx).into_any_element()
            }
            Route::Device => match self.selected_device() {
                Some(device) => self
                    .render_device(device, workspace_width, cx)
                    .into_any_element(),
                None if !self.has_loaded => loading_state(cx.theme()).into_any_element(),
                None => self.render_empty_state(cx).into_any_element(),
            },
        };
        let content = v_flex()
            .px_5()
            .pt_4()
            .pb_6()
            .gap_4()
            .when(
                self.banner.is_some() || self.update_available().is_some(),
                |this| this.child(notices),
            )
            .child(body);
        self.smooth_scroll.wrap(
            ScrollIds {
                root: "workspace",
                viewport: "workspace-viewport",
                scrollbar: "workspace-scrollbar",
            },
            content,
            background,
            cx,
        )
    }

    fn render_device(
        &self,
        device: &DeviceSnapshot,
        workspace_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let info = &device.info;
        let mut facts: Vec<SharedString> = vec![
            info.vendor.to_string().into(),
            info.connection.to_string().into(),
        ];
        if let Some(firmware) = &info.firmware_version {
            facts.push(format!("Firmware {firmware}").into());
        }
        // USB ids help when debugging a driver; release builds leave them out.
        let vid_pid = self
            .show_dev_tools
            .then(|| format!("{:04X}:{:04X}", info.vid, info.pid));
        let caps = device.capabilities;
        let is_keyboard = caps.has_hall_effect()
            || caps.has_keymap()
            || caps.has_macros()
            || caps.has_keyboard_config();

        let header = h_flex()
            .gap_4()
            .justify_between()
            .child(
                h_flex()
                    .gap_x_4()
                    .gap_y_1()
                    .flex_wrap()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(info.model_name.clone()),
                            )
                            .when(device.is_preview, |this| {
                                this.child(
                                    Tag::info()
                                        .small()
                                        .child("Dry-run preview, device unchanged"),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .children(facts)
                            .when_some(vid_pid, |this, vid_pid| {
                                this.child(
                                    div()
                                        .id("device-vid-pid")
                                        .font_family(theme.mono_font_family.clone())
                                        .child(vid_pid)
                                        .tooltip(|window, cx| {
                                            Tooltip::new("USB vendor and product ID")
                                                .build(window, cx)
                                        })
                                        .test_support(),
                                )
                            }),
                    ),
            )
            .when(
                device
                    .hall_effect
                    .as_ref()
                    .is_some_and(|he| he.capabilities.supports_calibration_status),
                |this| {
                    this.child(
                        Button::new("open-calibration")
                            .outline()
                            .small()
                            .icon(IconName::Frame)
                            .label("Calibrate…")
                            .disabled(self.is_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                calibration::open(this, window, cx);
                            })),
                    )
                },
            );

        let mut body = v_flex()
            .gap_4()
            .child(header)
            .child(profile_bar::render(self, device, cx))
            .children(device.warnings.iter().map(|warning| {
                let id = ElementId::Name(
                    format!("device-warning-{}-{}", device.id(), warning.capability).into(),
                );
                Alert::warning(id, warning.message.clone())
                    .title(format!("{} partly loaded", warning.capability))
            }));

        if caps.has_pointer() {
            body = body.child(pointer_panel::render(self, device, cx));
        }
        if is_keyboard {
            body = body.child(self.render_keyboard(device, workspace_width, cx));
        } else if caps.has_rgb() {
            body = body.child(rgb_panel::render(self, device, cx));
        }
        body
    }

    fn render_keyboard(
        &self,
        device: &DeviceSnapshot,
        workspace_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tabs = self.keyboard_tabs();
        let current = if tabs.contains(&self.keyboard_tab) {
            self.keyboard_tab
        } else {
            tabs.first().copied().unwrap_or(KeyboardTab::Triggers)
        };
        let selected_ix = tabs.iter().position(|t| *t == current).unwrap_or(0);
        let tab_list = tabs.clone();
        let bar = TabBar::new("keyboard-tabs")
            .underline()
            .selected_index(selected_ix)
            .children(tabs.iter().map(|tab| Tab::new().label(tab.label())))
            .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                if let Some(tab) = tab_list.get(*ix) {
                    this.set_keyboard_tab(*tab, cx);
                }
            }));
        let panel = match current {
            KeyboardTab::Triggers => {
                hall_effect_panel::render(self, device, workspace_width, cx).into_any_element()
            }
            KeyboardTab::Keybinds => keymap_panel::render(self, device, cx).into_any_element(),
            KeyboardTab::Lighting => rgb_panel::render(self, device, cx).into_any_element(),
            KeyboardTab::Macros => macro_panel::render(self, device, cx).into_any_element(),
            KeyboardTab::Advanced => {
                keyboard_config_panel::render(self, device, cx).into_any_element()
            }
        };
        v_flex().gap_4().child(bar).child(panel)
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        v_flex()
            .id("empty-state")
            .flex_1()
            .items_center()
            .justify_center()
            .py_16()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("No supported peripherals found"),
            )
            .child(
                div()
                    .max_w_96()
                    .text_sm()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(
                        "Connect a supported mouse or keyboard. Fully exit each vendor’s own software, including tray icons, then refresh.",
                    ),
            )
            .child(
                Button::new("empty-refresh")
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label("Refresh")
                    .loading(self.is_loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }
}

/// Embedded AgnoPeriph brand mark (`64×64` PNG), compiled into the binary so standalone
/// executables render the header icon quickly regardless of working directory.
static LOGO_IMAGE: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../assets/logo_64.png").to_vec(),
    ))
});

/// The header's brand mark.
fn logo_mark(theme: &Theme) -> AnyElement {
    img(LOGO_IMAGE.clone())
        .id("app-logo")
        .size_6()
        .rounded(theme.radius)
        .into_any_element()
}

fn device_count_label(count: usize) -> String {
    match count {
        0 => "No devices".to_owned(),
        1 => "1 device".to_owned(),
        n => format!("{n} devices"),
    }
}

fn loading_state(theme: &Theme) -> impl IntoElement + use<> {
    h_flex()
        .py_16()
        .gap_2()
        .justify_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(Spinner::new().small())
        .child("Looking for peripherals…")
}

impl Render for AgnoPeriphApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;
        if self.controls.is_none() {
            return v_flex()
                .id("app-root")
                .size_full()
                .bg(background)
                .text_color(foreground);
        }
        // Available inner width of the workspace pane (viewport minus 240px sidebar and 40px padding).
        let viewport_w: f32 = window.viewport_size().width.into();
        let workspace_width = (viewport_w - 240.0 - 40.0).max(320.0);
        let header = self.render_header(cx);
        let sidebar = sidebar::render(self, cx);
        let workspace = self.render_workspace(workspace_width, cx);
        let dirty_bar = (self.route == Route::Device && self.has_dirty_bar())
            .then(|| crate::dirty_bar::render(self, cx));
        let root = v_flex()
            .id("app-root")
            .key_context(crate::shortcuts::CONTEXT)
            .track_focus(&self.focus_handle);
        crate::dirty_bar::bind_actions(root, cx)
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(header)
            .child(
                h_flex()
                    .items_stretch()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(workspace)
                            .children(dirty_bar),
                    ),
            )
            .when(self.show_dev_tools && self.is_inspector_open, |this| {
                this.child(inspector_drawer::render(self, cx))
            })
    }
}

/// Section scaffold shared by the capability panels: title, one-line purpose, content.
pub(crate) fn section(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    v_flex().id(id).gap_4().child(
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(description),
            ),
    )
}

/// A titled group inside a panel: a quiet surface with a hairline boundary.
pub(crate) fn group(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    v_flex()
        .id(id)
        .gap_3()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
}

/// One labelled control row. Labels share a fixed lane so controls form one spine.
pub(crate) fn field(label: impl Into<SharedString>, theme: &Theme) -> Div {
    h_flex().gap_4().min_h_8().child(
        div()
            .w_40()
            .flex_shrink_0()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(label.into()),
    )
}

/// Monospace readout for a numeric value next to its stepper.
pub(crate) fn readout(value: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .min_w_20()
        .text_sm()
        .text_center()
        .font_family(theme.mono_font_family.clone())
        .child(value.into())
}

pub(crate) fn muted_note(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(text.into())
}

/// Formats hundredths of a millimetre as `0.00`.
pub(crate) fn mm(value: TravelHundredthsMm) -> String {
    let raw = value.as_hundredths();
    format!("{}.{:02}", raw / 100, raw % 100)
}

/// Inline note under an edited value showing what the device holds now.
pub(crate) fn was_note(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_xs()
        .italic()
        .text_color(theme.muted_foreground)
        .child(format!("was {}", text.into()))
}
