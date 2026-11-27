//! UI integration tests: the production `AgnoPeriphApp` in a headless window, driven
//! by real pointer events against deterministic peripheral services.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agnoperiph::backend::{
    ApplyError, DeviceSnapshot, MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, MOCK_PULSAR_ID, MOCK_RAZER_ID,
    MOCK_VIA_ID, MOCK_WOOTING_ID, MockPeripheralService, PeripheralService, PointerChange,
    SettingChange,
};
use agnoperiph::draft::ChangeId;
use agnoperiph::game_profiles::{DetectedGame, GameSource};
use agnoperiph::key_tester::{LIVE_POLL_INTERVAL, LIVE_RETRY_INTERVAL};
use agnoperiph::quick_presets::QuickPreset;
use agnoperiph::{AgnoPeriphApp, AppOptions, KeyboardTab, Route, device_element_id};
use agnoperiph_core::{
    BatteryStatus, ChargingState, CoreError, CoreResult, DeviceId, KeyCode, KeyIndex,
    KeyTravelSample, LiftOffDistance, MacroActionKind, ModTapBinding, PollingRateHz, ProfileSlot,
    RapidSwitchPair, RgbColor, RgbEffect, ToggleKeyBinding, TravelHundredthsMm,
};
use agnoperiph_transport::{PacketOutcome, PacketTraceEntry};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Bounds, ElementId, Entity, Point, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};

/// W on the RS7 matrix (row 2, column 2): owned by the fixture's Rapid Switch pair (W + S).
const KEY_W: u16 = 2 * 16 + 2;
/// S on the RS7 matrix (row 3, column 2): owned by the fixture's Rapid Switch pair (W + S).
const KEY_S: u16 = 3 * 16 + 2;
/// Q on the RS7 matrix (row 2, column 1): regular key, Rapid Trigger off, 1.20 mm actuation.
const KEY_Q: u16 = 2 * 16 + 1;
/// E on the RS7 matrix (row 2, column 3): regular key, Rapid Trigger off, 1.20 mm actuation.
const KEY_E: u16 = 2 * 16 + 3;

fn open_app(
    cx: &mut TestAppContext,
    service: impl PeripheralService + 'static,
) -> (AnyWindowHandle, Entity<AgnoPeriphApp>) {
    open_app_with(cx, service, AppOptions::default())
}

fn open_app_with(
    cx: &mut TestAppContext,
    service: impl PeripheralService + 'static,
    options: AppOptions,
) -> (AnyWindowHandle, Entity<AgnoPeriphApp>) {
    cx.update(gpui_kit::init);
    let service: Arc<dyn PeripheralService> = Arc::new(service);
    let opened = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                // Tall enough that every editor control is on screen without scrolling.
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1440.), px(3200.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| AgnoPeriphApp::build(service, options, window, cx),
        )
        .expect("open test window")
    });
    settle(cx, opened.0);
    opened
}

/// Lets queued background service calls finish, then renders a fresh frame.
fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(handle, |_, window, cx| window.click(id, cx))
        .unwrap();
    settle(cx, handle);
}

/// Sends the staged edits with the dirty bar's Apply button.
fn apply(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    click(cx, handle, "dirty-apply");
}

fn exists(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    cx.update_window(handle, |_, window, _| window.try_find(id).is_some())
        .unwrap()
}

fn key_id(key: u16) -> (&'static str, u64) {
    ("key", u64::from(key))
}

fn keyboard() -> DeviceId {
    DeviceId::new(MOCK_KEYBOARD_ID)
}

fn mouse() -> DeviceId {
    DeviceId::new(MOCK_MOUSE_ID)
}

fn travel(hundredths: u16) -> TravelHundredthsMm {
    TravelHundredthsMm::from_hundredths(hundredths)
}

#[gpui_kit::test]
fn loads_devices_and_switches_capability_editors(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());

    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.devices().len(), 2);
        assert_eq!(app.selected_device_id(), Some(&mouse()));
        assert!(!app.is_loading());
    });
    assert!(exists(cx, handle, device_element_id(&mouse())));
    assert!(exists(cx, handle, device_element_id(&keyboard())));
    // The mouse exposes pointer controls only.
    assert!(exists(cx, handle, ("polling-rate", 1000u64)));
    assert!(exists(cx, handle, ("profile", 0u64)));
    assert!(!exists(cx, handle, key_id(KEY_W)));

    click(cx, handle, device_element_id(&keyboard()));

    cx.update(|cx| assert_eq!(app.read(cx).selected_device_id(), Some(&keyboard())));
    assert!(exists(cx, handle, key_id(KEY_W)));
    assert!(exists(cx, handle, ("preset", 0u64)));
    assert!(!exists(cx, handle, ("polling-rate", 1000u64)));

    click(cx, handle, device_element_id(&mouse()));
    assert!(exists(cx, handle, ("polling-rate", 1000u64)));
    assert!(!exists(cx, handle, key_id(KEY_W)));
}

#[gpui_kit::test]
fn dry_run_is_default_and_writes_are_traced(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());

    cx.update(|cx| assert!(app.read(cx).is_dry_run()));
    let checked = cx
        .update_window(handle, |_, window, _| window.find("dry-run").checked())
        .unwrap();
    assert_eq!(checked, Some(true));

    click(cx, handle, ("polling-rate", 500u64));
    apply(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        let mouse = app.selected_device().unwrap();
        assert!(mouse.is_preview);
        let pointer = mouse.pointer.as_ref().unwrap();
        assert_eq!(pointer.snapshot.polling_rate, PollingRateHz::Hz500);
        let last = app.packets().last().unwrap();
        assert_eq!(last.outcome, PacketOutcome::DryRunSuppressed);
    });

    // Turning dry run off opens a confirmation AlertDialog; cancelling keeps dry run active.
    click(cx, handle, "dry-run");
    assert!(exists(cx, handle, "cancel"));
    click(cx, handle, "cancel");
    cx.update(|cx| assert!(app.read(cx).is_dry_run()));

    // Confirming the dialog reopens devices in live mode and discards the preview.
    click(cx, handle, "dry-run");
    click(cx, handle, "ok");
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(!app.is_dry_run());
        let mouse = app.selected_device().unwrap();
        assert!(!mouse.is_preview);
        let pointer = mouse.pointer.as_ref().unwrap();
        assert_eq!(pointer.snapshot.polling_rate, PollingRateHz::Hz1000);
    });
    // The persistent yellow live-writes warning banner was removed by user request.
    assert!(!exists(cx, handle, "live-writes"));

    click(cx, handle, ("polling-rate", 500u64));
    apply(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        let last = app.packets().last().unwrap();
        assert_eq!(last.outcome, PacketOutcome::Transmitted);
        let pointer = app.selected_device().unwrap().pointer.as_ref().unwrap();
        assert_eq!(pointer.snapshot.polling_rate, PollingRateHz::Hz500);
    });
}

#[gpui_kit::test]
fn edits_actuation_and_rapid_trigger_from_the_key_matrix(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));

    click(cx, handle, key_id(KEY_E));
    cx.update(|cx| assert_eq!(app.read(cx).selected_key(), KeyIndex::new(KEY_E)));

    click(cx, handle, "actuation-inc");
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        let he = app.selected_device().unwrap().hall_effect.as_ref().unwrap();
        assert_eq!(he.key(KeyIndex::new(KEY_E)).unwrap().actuation, travel(124));
        assert_eq!(he.key(KeyIndex::new(KEY_Q)).unwrap().actuation, travel(120));
        assert_eq!(
            app.packets().last().unwrap().outcome,
            PacketOutcome::DryRunSuppressed
        );
    });

    // Enabling Rapid Trigger on a key that reports 0 mm clamps to the device minimum.
    // Clear first: clicks accumulate, and this covers the single-key write.
    click(cx, handle, "clear-keys");
    click(cx, handle, key_id(KEY_Q));
    click(cx, handle, "rapid-trigger");
    click(cx, handle, "press-sensitivity-inc");
    apply(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.banner(), None);
        let he = app.selected_device().unwrap().hall_effect.as_ref().unwrap();
        let rt = he.key(KeyIndex::new(KEY_Q)).unwrap().rapid_trigger;
        assert!(rt.is_enabled());
        assert_eq!(rt.press_sensitivity, travel(4));
        assert_eq!(rt.release_sensitivity, travel(2));
    });

    // Removing an SOCD pair works via slot-aware clearing.
    let pair_remove = "socd-remove-49-51-x";
    let disabled = cx
        .update_window(handle, |_, window, _| window.find(pair_remove).disabled())
        .unwrap();
    assert_ne!(disabled, Some(true));
    click(cx, handle, pair_remove);
    cx.update(|cx| {
        let app = app.read(cx);
        let he = app.selected_device().unwrap().hall_effect.as_ref().unwrap();
        assert_eq!(he.snapshot.socd_pairs, []);
    });

    // In live mode, "Use for all keys" stages Q's actuation for every key with its own travel;
    // keys owned by the Rapid Switch pair keep the pair's travel.
    click(cx, handle, "dry-run");
    click(cx, handle, "ok");
    // Clicks accumulate, so start a fresh selection before picking Q alone.
    click(cx, handle, "clear-keys");
    click(cx, handle, key_id(KEY_Q));
    click(cx, handle, "actuation-inc");
    click(cx, handle, "actuation-all-keys");
    assert_eq!(key_actuation(cx, &app, KEY_E), 120, "staged, not written");
    apply(cx, handle);
    assert!(
        !exists(cx, handle, "ok"),
        "a partial table needs no flash warning"
    );
    assert_eq!(key_actuation(cx, &app, KEY_E), 122);
    assert_eq!(key_actuation(cx, &app, KEY_Q), 122);
    assert_ne!(key_actuation(cx, &app, KEY_W), 122);
}

#[gpui_kit::test]
fn packet_inspector_toggles_and_clears(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());

    cx.update(|cx| {
        let app = app.read(cx);
        assert!(app.is_inspector_open());
        assert_eq!(app.packets().len(), 4, "a query and a reply per device");
    });
    assert!(exists(cx, handle, ("packet", 1u64)));

    click(cx, handle, "inspector-clear");
    cx.update(|cx| assert_eq!(app.read(cx).packets(), []));
    assert!(!exists(cx, handle, ("packet", 1u64)));

    // The service log is cleared too: the next write is the only frame.
    click(cx, handle, ("profile", 2u64));
    cx.update(|cx| {
        let packets = app.read(cx).packets();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].outcome, PacketOutcome::DryRunSuppressed);
    });

    click(cx, handle, "inspector-toggle");
    cx.update(|cx| assert!(!app.read(cx).is_inspector_open()));
    assert!(!exists(cx, handle, "inspector-clear"));

    click(cx, handle, "inspector-toggle");
    assert!(exists(cx, handle, "inspector-clear"));
}

#[gpui_kit::test]
fn shows_empty_state_without_devices(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::empty());

    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.devices(), []);
        assert_eq!(app.selected_device_id(), None);
    });
    assert!(exists(cx, handle, "empty-refresh"));
    click(cx, handle, "empty-refresh");
    cx.update(|cx| assert!(!app.read(cx).is_loading()));
}

#[gpui_kit::test]
fn dpi_draft_survives_unrelated_writes_and_keeps_axes_linked(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());

    click(cx, handle, ("dpi-inc", 1u64));
    assert!(exists(cx, handle, "dirty-apply"));

    // Staging a polling rate keeps the pending DPI edit; both go out with one Apply.
    click(cx, handle, ("polling-rate", 500u64));
    cx.update(|cx| assert_eq!(app.read(cx).pending_changes().len(), 2));

    apply(cx, handle);
    assert!(!exists(cx, handle, "dirty-apply"));
    cx.update(|cx| {
        let app = app.read(cx);
        let pointer = app.selected_device().unwrap().pointer.as_ref().unwrap();
        let stage = pointer.snapshot.dpi_stages[1];
        assert_eq!(stage.x.value(), 900);
        assert_eq!(stage.y, None, "a linked stage keeps Y following X");
        assert_eq!(pointer.snapshot.polling_rate, PollingRateHz::Hz500);
    });
}

/// Service wrapper that simulates a failed live write after frames have already been sent,
/// returning a refreshed live snapshot reflecting the hardware's partial update.
struct FailingLiveWriteService {
    inner: MockPeripheralService,
    fail_live_write: Mutex<bool>,
}

impl PeripheralService for FailingLiveWriteService {
    fn is_dry_run(&self) -> bool {
        self.inner.is_dry_run()
    }

    fn set_dry_run(&self, dry_run: bool) {
        self.inner.set_dry_run(dry_run);
    }

    fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
        self.inner.refresh()
    }

    fn apply(
        &self,
        device: &DeviceId,
        change: SettingChange,
    ) -> Result<DeviceSnapshot, ApplyError> {
        let should_fail = !self.inner.is_dry_run() && *self.fail_live_write.lock().unwrap();
        if should_fail {
            // Apply to inner so we get a realistic post-failure re-read showing what landed.
            let partial = self.inner.apply(device, change)?;
            return Err(ApplyError::failed_after_send(
                CoreError::NotConnected("timed out waiting for chunk reply".to_owned()),
                Some(partial),
            ));
        }
        self.inner.apply(device, change)
    }

    fn packet_log(&self) -> Vec<PacketTraceEntry> {
        self.inner.packet_log()
    }

    fn clear_packet_log(&self) {
        self.inner.clear_packet_log();
    }
}

#[gpui_kit::test]
fn failed_live_write_refreshes_view_and_does_not_claim_nothing_was_written(
    cx: &mut TestAppContext,
) {
    let service = FailingLiveWriteService {
        inner: MockPeripheralService::new(),
        fail_live_write: Mutex::new(true),
    };
    let (handle, app) = open_app(cx, service);

    // Enable live mode via the confirmation dialog.
    click(cx, handle, "dry-run");
    click(cx, handle, "ok");

    // Trigger a live polling-rate write that fails after transmitting frames.
    click(cx, handle, ("polling-rate", 500u64));
    apply(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        let banner = app.banner().expect("failed live write must show a banner");
        assert!(
            !banner.message().contains("Nothing was written"),
            "banner must not claim nothing was written after a live failure: {}",
            banner.message()
        );
        assert!(banner.message().contains("refreshed"));
        // The device view reflects the re-read live snapshot.
        let pointer = app.selected_device().unwrap().pointer.as_ref().unwrap();
        assert_eq!(pointer.snapshot.polling_rate, PollingRateHz::Hz500);
    });
}

fn open_tab(cx: &mut TestAppContext, handle: AnyWindowHandle, tab: KeyboardTab) {
    let index = KeyboardTab::ALL
        .iter()
        .position(|t| *t == tab)
        .expect("tab exists");
    cx.update_window(handle, |_, window, cx| {
        window.within("keyboard-tabs").click(index, cx);
    })
    .unwrap();
    settle(cx, handle);
}

/// Focuses the text input wrapped by `id`, replaces its text and renders.
fn type_into(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str, text: &str) {
    cx.update_window(handle, |_, window, cx| {
        window.within(id).click("input-state", cx);
        window.press("ctrl-a", cx);
        window.press("backspace", cx);
        window.input(text, cx);
    })
    .unwrap();
    settle(cx, handle);
}

fn keyboard_snapshot(cx: &mut TestAppContext, app: &Entity<AgnoPeriphApp>) -> DeviceSnapshot {
    cx.update(|cx| app.read(cx).device(&keyboard()).unwrap().clone())
}

fn zone_effect(snapshot: &DeviceSnapshot, zone: usize) -> RgbEffect {
    snapshot.rgb.as_ref().unwrap().snapshot.zones[zone].effect
}

#[gpui_kit::test]
fn keyboard_tabs_switch_workspaces(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));

    let expected = [
        (KeyboardTab::Triggers, ElementId::from(key_id(KEY_W))),
        (
            KeyboardTab::Keybinds,
            ("keymap-key", u64::from(KEY_W)).into(),
        ),
        (KeyboardTab::Lighting, "rgb-0-preset-0".into()),
        (KeyboardTab::Macros, "save-macros".into()),
        (KeyboardTab::Advanced, "config-win-lock".into()),
    ];
    for (tab, marker) in expected {
        open_tab(cx, handle, tab);
        cx.update(|cx| assert_eq!(app.read(cx).keyboard_tab(), tab));
        assert!(
            exists(cx, handle, marker.clone()),
            "{tab:?} shows {marker:?}"
        );
    }
}

#[gpui_kit::test]
fn lighting_presets_apply_per_zone(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Lighting);

    // The Back Lamp lists every preset the keyboard reports.
    let snapshot = keyboard_snapshot(cx, &app);
    let zones = &snapshot.rgb.as_ref().unwrap().capabilities.zones;
    assert_eq!(zones[0].presets.len(), 21);
    assert_eq!(zones[1].presets.len(), 5);
    for preset in &zones[0].presets {
        let id = format!("rgb-0-preset-{}", preset.id);
        assert!(exists(cx, handle, id.clone()), "{id}");
    }
    click(cx, handle, "rgb-0-preset-5");
    apply(cx, handle);
    let snapshot = keyboard_snapshot(cx, &app);
    assert!(matches!(
        zone_effect(&snapshot, 0),
        RgbEffect::Preset { id: 5, .. }
    ));

    // The Logo zone has its own, shorter preset list.
    click(cx, handle, ("rgb-zone", 1u64));
    assert!(exists(cx, handle, "rgb-1-preset-4"));
    assert!(!exists(cx, handle, "rgb-1-preset-5"));
    click(cx, handle, "rgb-1-preset-1");
    apply(cx, handle);
    let snapshot = keyboard_snapshot(cx, &app);
    assert!(matches!(
        zone_effect(&snapshot, 1),
        RgbEffect::Preset { id: 1, .. }
    ));
    assert!(matches!(
        zone_effect(&snapshot, 0),
        RgbEffect::Preset { id: 5, .. }
    ));
}

#[gpui_kit::test]
fn triggers_select_many_keys_and_restore_defaults(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));

    cx.update(|cx| assert!(app.read(cx).selected_keys().is_empty()));

    // Plain clicks accumulate, as in ATK HUB; no modifier is needed.
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, key_id(KEY_Q));
    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(
            app.selected_keys().clone(),
            [KeyIndex::new(KEY_Q), KeyIndex::new(KEY_E)].into()
        );
        assert_eq!(app.selected_key(), KeyIndex::new(KEY_Q));
    });

    // Clicking a selected key removes it; the editor follows a remaining key.
    click(cx, handle, key_id(KEY_Q));
    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.selected_keys().clone(), [KeyIndex::new(KEY_E)].into());
        assert_eq!(app.selected_key(), KeyIndex::new(KEY_E));
    });
    click(cx, handle, key_id(KEY_E));
    cx.update(|cx| assert!(app.read(cx).selected_keys().is_empty()));
    assert!(is_disabled(cx, handle, "actuation-all-keys"));
    assert!(is_disabled(cx, handle, "restore-default-keys"));

    click(cx, handle, key_id(KEY_E));
    click(cx, handle, key_id(KEY_Q));
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    assert_eq!(key_actuation(cx, &app, KEY_E), 122);

    click(cx, handle, "restore-default-keys");
    apply(cx, handle);
    let snapshot = keyboard_snapshot(cx, &app);
    let he = snapshot.hall_effect.as_ref().unwrap();
    for key in [KEY_E, KEY_Q] {
        let config = he.key(KeyIndex::new(key)).unwrap();
        assert_eq!(config.actuation, travel(120));
        assert!(!config.rapid_trigger.is_enabled());
    }

    click(cx, handle, "select-all-keys");
    let total = cx.update(|cx| app.read(cx).selected_keys().len());
    assert!(total > 60, "selected {total} keys");
    click(cx, handle, "clear-keys");
    cx.update(|cx| assert!(app.read(cx).selected_keys().is_empty()));
}

#[gpui_kit::test]
fn keybinds_remap_undo_and_redo(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Keybinds);

    let code_of = |snapshot: &DeviceSnapshot| {
        snapshot.keymap.as_ref().unwrap().snapshot.layers[0][usize::from(KEY_Q)]
    };
    let original = code_of(&keyboard_snapshot(cx, &app));

    click(cx, handle, ("keymap-key", u64::from(KEY_Q)));
    click(cx, handle, ("hid-key", 0x1Au64));
    assert_eq!(
        code_of(&keyboard_snapshot(cx, &app)),
        KeyCode::standard(0x1A)
    );

    click(cx, handle, "keymap-undo");
    assert_eq!(code_of(&keyboard_snapshot(cx, &app)), original);
    click(cx, handle, "keymap-redo");
    assert_eq!(
        code_of(&keyboard_snapshot(cx, &app)),
        KeyCode::standard(0x1A)
    );

    // Macros are offered as extra keys; vendor groups without verified codes are listed only.
    click(cx, handle, "keymap-picker-extra");
    click(cx, handle, ("macro-key", 2u64));
    assert_eq!(
        code_of(&keyboard_snapshot(cx, &app)),
        KeyCode::macro_slot(2)
    );
}

#[gpui_kit::test]
fn macros_edit_steps_and_save(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Macros);

    let steps =
        |snapshot: &DeviceSnapshot| snapshot.macros.as_ref().unwrap().slots[1].steps.clone();
    click(cx, handle, ("macro-slot", 1u64));
    click(cx, handle, "add-macro-step");
    click(cx, handle, "add-macro-step");
    click(cx, handle, ("macro-delay-inc", 1u64));
    click(cx, handle, "macro-step-0-down");
    // Nothing is written until Save.
    assert_eq!(steps(&keyboard_snapshot(cx, &app)), []);

    click(cx, handle, "save-macros");
    let saved = steps(&keyboard_snapshot(cx, &app));
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].kind, MacroActionKind::KeyDown);
    assert_eq!(saved[1].delay_ms, 30);
}

/// Stack the heaviest screens must fit in, half of the 1 MiB Windows main thread that
/// renders the window. Unoptimized debug builds once needed more than 1 MiB for the macro
/// editor, so the app overflowed its stack at launch on a mouse with a stored macro.
const RENDER_STACK_BUDGET: usize = 512 * 1024;
const STACK_PROBE_ENV: &str = "AGNO_STACK_PROBE";

#[test]
fn heaviest_screens_render_within_the_main_thread_stack() {
    if std::env::var_os(STACK_PROBE_ENV).is_some() {
        return;
    }
    let screens = [
        "mouse_macro_cycle_times_stage_discard_and_save",
        "macros_edit_steps_and_save",
        "keyboard_tabs_switch_workspaces",
        "via_keyboard_without_hall_effect_lands_on_its_own_tabs",
        "triggers_and_lighting_fit_the_default_window",
        "loads_devices_and_switches_capability_editors",
    ];
    // libtest threads honour RUST_MIN_STACK, so the same tests rerun in a child process
    // with the smaller stack; an overflow aborts that process instead of this one.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(screens)
        .args(["--exact", "--test-threads=1"])
        .env("RUST_MIN_STACK", RENDER_STACK_BUDGET.to_string())
        .env(STACK_PROBE_ENV, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "screens overflowed a {RENDER_STACK_BUDGET}-byte stack or failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(&format!("{} passed", screens.len())),
        "not every screen ran:\n{stdout}"
    );
}

#[gpui_kit::test]
fn mouse_macro_cycle_times_stage_discard_and_save(cx: &mut TestAppContext) {
    use agnoperiph_driver_lamzu::atlantis::{self, Command as LamzuCommand};
    use agnoperiph_driver_lamzu::crystall;

    let (handle, app) = open_app(cx, MockPeripheralService::new());
    // The mouse offers only the Macros tab under its pointer settings.
    cx.update(|cx| assert_eq!(app.read(cx).keyboard_tabs(), [KeyboardTab::Macros]));
    assert!(exists(cx, handle, "save-macros"));

    let repeat = |cx: &mut TestAppContext| {
        device_snapshot(cx, &app, &mouse()).macros.unwrap().slots[3].repeat
    };
    let draft_repeat =
        |cx: &mut TestAppContext| cx.update(|cx| app.read(cx).macro_slots()[3].repeat);
    click(cx, handle, ("macro-slot", 3u64));
    assert!(exists(cx, handle, ("macro-repeat-inc", 3u64)));
    assert_eq!(repeat(cx), 3);

    click(cx, handle, ("macro-repeat-inc", 3u64));
    assert_eq!(draft_repeat(cx), 4);
    // The device keeps its value until Save.
    assert_eq!(repeat(cx), 3);
    click(cx, handle, "discard-macros");
    assert_eq!(draft_repeat(cx), 3);

    click(cx, handle, ("macro-repeat-inc", 3u64));
    cx.update(|cx| app.update(cx, AgnoPeriphApp::clear_packets));
    click(cx, handle, "save-macros");
    assert_eq!(repeat(cx), 4);
    let binding = atlantis::encode_request(
        LamzuCommand::WriteFlash,
        crystall::key_slot_address(3).unwrap(),
        &atlantis::seal_field(&crystall::macro_binding(3, 4)),
    )
    .unwrap()
    .to_vec();
    cx.update(|cx| {
        let app = app.read(cx);
        let writes: Vec<_> = app.packets().iter().map(|p| p.bytes.clone()).collect();
        assert_eq!(writes, [binding]);
    });

    // An empty slot cannot store a cycle count, so its stepper waits for a step.
    click(cx, handle, ("macro-slot", 6u64));
    click(cx, handle, ("macro-repeat-inc", 6u64));
    cx.update(|cx| assert_eq!(app.read(cx).macro_slots()[6].repeat, 1));
}

#[gpui_kit::test]
fn mouse_macro_edits_clicks_media_and_pointer_move(cx: &mut TestAppContext) {
    use agnoperiph_core::MouseButton;

    let (handle, app) = open_app(cx, MockPeripheralService::new());
    let stored = |cx: &mut TestAppContext| device_snapshot(cx, &app, &mouse()).macros.unwrap();

    // Captured `spam click` in slot 5 loads as editable mouse-button steps.
    let slot5 = stored(cx).slots[5].clone();
    assert!(!slot5.is_readonly);
    assert_eq!(slot5.name, "spam click");
    assert_eq!(slot5.steps.len(), 2);
    assert_eq!(slot5.steps[0].kind, MacroActionKind::MouseDown);
    assert_eq!(slot5.steps[0].mouse_button, Some(MouseButton::Left));

    click(cx, handle, ("macro-slot", 5u64));
    assert!(!is_disabled(cx, handle, "add-macro-mouse-step"));
    click(cx, handle, "add-macro-mouse-step");
    click(cx, handle, "add-macro-media-step");
    click(cx, handle, "add-macro-move-step");
    // Step 4 (index 4, id 4) is the MoveXY step; bump X by +1 and Y by -1.
    click(cx, handle, ("macro-move-x-inc", 4u64));
    click(cx, handle, ("macro-move-y-dec", 4u64));
    click(cx, handle, "save-macros");

    let saved = stored(cx).slots[5].steps.clone();
    assert_eq!(saved.len(), 5);
    assert_eq!(saved[2].kind, MacroActionKind::MouseClick);
    assert_eq!(saved[2].mouse_button, Some(MouseButton::Left));
    assert_eq!(saved[3].kind, MacroActionKind::MediaTap);
    assert_eq!(saved[3].consumer_usage, 0x00CD);
    assert_eq!(saved[4].kind, MacroActionKind::PointerMove);
    assert_eq!((saved[4].move_x, saved[4].move_y), (1, -1));
}

#[gpui_kit::test]
fn unsupported_macro_stays_readonly_and_survives_saves(cx: &mut TestAppContext) {
    use agnoperiph_driver_lamzu::crystall;

    let (handle, app) = open_app(cx, MockPeripheralService::new());
    let stored = |cx: &mut TestAppContext| device_snapshot(cx, &app, &mouse()).macros.unwrap();
    assert!(stored(cx).slots[7].is_readonly);

    click(cx, handle, ("macro-slot", 7u64));
    assert!(is_disabled(cx, handle, "add-macro-step"));
    assert!(is_disabled(cx, handle, "add-macro-mouse-step"));
    assert!(is_disabled(cx, handle, "rename-macro"));
    assert!(!exists(cx, handle, ("macro-repeat-inc", 7u64)));

    // Saving an edit to another slot leaves slot 7's record and trigger alone.
    click(cx, handle, ("macro-slot", 3u64));
    click(cx, handle, ("macro-repeat-inc", 3u64));
    cx.update(|cx| app.update(cx, AgnoPeriphApp::clear_packets));
    click(cx, handle, "save-macros");
    let slot7 = crystall::macro_slot_address(7).unwrap();
    let slot7_end = slot7 + crystall::layout::MACRO_SLOT_STRIDE;
    let key7 = crystall::key_slot_address(7).unwrap();
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(!app.packets().is_empty(), "slot 3 is saved");
        assert!(app.packets().iter().all(|p| {
            let address = u16::from_be_bytes([p.bytes[3], p.bytes[4]]);
            !(slot7..slot7_end).contains(&address) && address != key7
        }));
    });
    let after = stored(cx).slots[7].clone();
    assert!(after.is_readonly && after.name == "Power event" && after.steps.is_empty());
}

#[gpui_kit::test]
fn keyboard_macros_hide_cycle_times(cx: &mut TestAppContext) {
    let (handle, _) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Macros);
    assert!(exists(cx, handle, ("macro-slot", 0u64)));
    assert!(!exists(cx, handle, ("macro-repeat-inc", 0u64)));
}

#[gpui_kit::test]
fn advanced_settings_write_keyboard_config(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Advanced);

    click(cx, handle, ("config-rate", 4000u64));
    click(cx, handle, "config-win-lock");
    // Both edits wait as one unsaved keyboard-settings change.
    let snapshot = keyboard_snapshot(cx, &app);
    assert!(!snapshot.keyboard_config.as_ref().unwrap().snapshot.win_lock);
    cx.update(|cx| assert_eq!(app.read(cx).pending_changes().len(), 1));
    apply(cx, handle);
    let snapshot = keyboard_snapshot(cx, &app);
    let config = snapshot.keyboard_config.as_ref().unwrap().snapshot;
    assert_eq!(config.polling_rate, PollingRateHz::Hz4000);
    assert!(config.win_lock);
}

#[gpui_kit::test]
fn calibration_modal_polls_only_while_active(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));

    click(cx, handle, "open-calibration");
    assert!(exists(cx, handle, "calibration-start"));

    click(cx, handle, "calibration-start");
    cx.update(|cx| assert!(app.read(cx).is_calibrating()));
    cx.executor().advance_clock(Duration::from_millis(1600));
    settle(cx, handle);
    let polls = cx.update(|cx| app.read(cx).calibration_polls());
    assert!(polls >= 2, "polled {polls} times");

    click(cx, handle, "calibration-finish");
    cx.update(|cx| assert!(!app.read(cx).is_calibrating()));
    cx.executor().advance_clock(Duration::from_millis(1600));
    settle(cx, handle);
    cx.update(|cx| assert_eq!(app.read(cx).calibration_polls(), polls));

    // Closing the modal mid-run ends calibration and stops polling.
    click(cx, handle, "calibration-start");
    cx.update(|cx| assert!(app.read(cx).is_calibrating()));
    cx.update_window(handle, |_, window, cx| {
        gpui_kit::component::WindowExt::close_dialog(window, cx);
    })
    .unwrap();
    // Let the dialog's exit transition finish so the layer drops the modal view.
    cx.executor().advance_clock(Duration::from_millis(500));
    settle(cx, handle);
    assert!(!exists(cx, handle, "calibration-start"));
    cx.update(|cx| assert!(!app.read(cx).is_calibrating()));
    let after_close = cx.update(|cx| app.read(cx).calibration_polls());
    cx.executor().advance_clock(Duration::from_millis(1600));
    settle(cx, handle);
    cx.update(|cx| assert_eq!(app.read(cx).calibration_polls(), after_close));
}

#[gpui_kit::test]
fn software_presets_rename_and_switch(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));

    click(cx, handle, "rename-preset");
    type_into(cx, handle, "rename-input", "Valorant RT");
    click(cx, handle, "rename-save");
    cx.update(|cx| {
        let name = app
            .read(cx)
            .config()
            .preset_name(&keyboard(), ProfileSlot::new(0));
        assert_eq!(name, "Valorant RT");
    });

    click(cx, handle, ("preset", 2u64));
    cx.update(|cx| assert_eq!(app.read(cx).current_slot(&keyboard()), Some(2)));
}

#[gpui_kit::test]
fn game_profiles_add_rename_and_assign_cross_brand_targets(cx: &mut TestAppContext) {
    let options = AppOptions::default().with_game_discovery(|| {
        vec![DetectedGame::new(
            "Counter-Strike 2",
            "cs2.exe",
            r"C:\Games\cs2\cs2.exe",
            GameSource::Steam,
        )]
    });
    let (handle, app) = open_app_with(cx, MockPeripheralService::new(), options);

    click(cx, handle, "nav-game-profiles");
    cx.update(|cx| assert_eq!(app.read(cx).route(), Route::GameProfiles));

    // The scan runs in the background and lists the injected game.
    click(cx, handle, "scan-games");
    click(cx, handle, "add-detected-cs2.exe");
    let cs2 = cx.update(|cx| app.read(cx).config().game_profiles[0].id.clone());
    assert_eq!(cs2, "counter-strike-2");

    type_into(cx, handle, "manual-game-name", "Valorant");
    type_into(cx, handle, "manual-game-exe", "VALORANT-Win64-Shipping.exe");
    click(cx, handle, "add-manual-game");
    cx.update(|cx| {
        let config = app.read(cx).config();
        assert_eq!(config.game_profiles.len(), 2);
        assert_eq!(
            config.game_profiles[1].exe_name,
            "VALORANT-Win64-Shipping.exe"
        );
    });

    click(cx, handle, "rename-game");
    type_into(cx, handle, "rename-input", "Valorant Ranked");
    click(cx, handle, "rename-save");
    cx.update(|cx| {
        assert_eq!(
            app.read(cx).config().game_profiles[1].name,
            "Valorant Ranked"
        );
    });

    // Cross-brand targets: Lamzu onboard profile 2 and the ATK software preset 4.
    click(cx, handle, format!("target-{}-1", mouse()));
    click(cx, handle, format!("target-{}-3", keyboard()));
    cx.update(|cx| {
        let profile = &app.read(cx).config().game_profiles[1];
        assert_eq!(profile.device_targets.get(mouse().as_str()), Some(&1));
        assert_eq!(profile.device_targets.get(keyboard().as_str()), Some(&3));
    });

    click(cx, handle, "game-profile-default");
    click(cx, handle, format!("target-{}-0", mouse()));
    cx.update(|cx| {
        let defaults = &app.read(cx).config().default_targets;
        assert_eq!(defaults.get(mouse().as_str()), Some(&0));
    });

    // The game starting switches both devices; closing it restores the default.
    let running = HashSet::from(["valorant-win64-shipping.exe".to_owned()]);
    app.update(cx, |app, cx| app.handle_running_exes(&running, cx));
    settle(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.active_game_name(), Some("Valorant Ranked"));
        assert_eq!(app.current_slot(&mouse()), Some(1));
        assert_eq!(app.current_slot(&keyboard()), Some(3));
    });
    app.update(cx, |app, cx| app.handle_running_exes(&HashSet::new(), cx));
    settle(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.active_game_name(), None);
        assert_eq!(app.current_slot(&mouse()), Some(0));
    });
}

fn key_actuation(cx: &mut TestAppContext, app: &Entity<AgnoPeriphApp>, key: u16) -> u16 {
    let snapshot = keyboard_snapshot(cx, app);
    let he = snapshot.hall_effect.as_ref().unwrap();
    he.key(KeyIndex::new(key))
        .unwrap()
        .actuation
        .as_hundredths()
}

#[gpui_kit::test]
fn software_preset_restores_saved_lighting(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Lighting);

    // Every confirmed edit auto-saves to the active software preset (no manual Save button).
    assert!(!exists(cx, handle, "save-preset"));
    click(cx, handle, "rgb-0-preset-5");
    apply(cx, handle);
    cx.update(|cx| {
        let saved = &app.read(cx).config().software_profiles[keyboard().as_str()].slots[0].saved;
        assert_ne!(saved.rgb_zones, []);
        assert_eq!(saved.socd_pairs.len(), 1);
    });

    // Switching to an unconfigured preset clears leftover SOCD pairs instead of retaining them.
    click(cx, handle, ("preset", 1u64));
    cx.update(|cx| assert_eq!(app.read(cx).current_slot(&keyboard()), Some(1)));
    assert_eq!(
        keyboard_snapshot(cx, &app)
            .hall_effect
            .unwrap()
            .snapshot
            .socd_pairs,
        Vec::new(),
        "switching to unconfigured preset must clear previous SOCD pairs"
    );
    click(cx, handle, "rgb-0-preset-7");
    apply(cx, handle);
    assert!(matches!(
        zone_effect(&keyboard_snapshot(cx, &app), 0),
        RgbEffect::Preset { id: 7, .. }
    ));

    // Switching back restores both the saved lighting and the saved SOCD pair.
    click(cx, handle, ("preset", 0u64));
    cx.update(|cx| assert_eq!(app.read(cx).current_slot(&keyboard()), Some(0)));
    assert!(matches!(
        zone_effect(&keyboard_snapshot(cx, &app), 0),
        RgbEffect::Preset { id: 5, .. }
    ));
    assert_eq!(
        keyboard_snapshot(cx, &app)
            .hall_effect
            .unwrap()
            .snapshot
            .socd_pairs
            .len(),
        1
    );
}

#[gpui_kit::test]
fn software_preset_auto_saves_per_key_and_clears_unconfigured_pairs(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, "dry-run");
    click(cx, handle, "ok");

    // Tune E actuation on Preset 0 (auto-saves into Preset 0, keeping its SOCD A+D & RS W+S pairs).
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    let preset0_e = key_actuation(cx, &app, KEY_E);

    // Move to unconfigured Preset 1: SOCD and RS are cleared cleanly, and editing E auto-saves to Preset 1.
    click(cx, handle, ("preset", 1u64));
    let p1_he = keyboard_snapshot(cx, &app).hall_effect.unwrap().snapshot;
    assert_eq!(p1_he.socd_pairs, Vec::new());
    assert_eq!(p1_he.rapid_switch_pairs, Vec::new());
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    let preset1_e = key_actuation(cx, &app, KEY_E);
    assert_ne!(preset1_e, preset0_e);

    // Switching back via Game Profile restores E's per-key actuation and Preset 0's SOCD & RS pairs
    // without a flash-erase confirmation or lost settings.
    app.update(cx, |app, cx| {
        let id = app.add_game_profile("Valorant", "VALORANT-Win64-Shipping.exe", "", cx);
        app.set_game_target(Some(&id), &keyboard(), Some(0), cx);
    });
    let running = HashSet::from(["valorant-win64-shipping.exe".to_owned()]);
    app.update(cx, |app, cx| app.handle_running_exes(&running, cx));
    settle(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.current_slot(&keyboard()), Some(0));
    });
    assert_eq!(key_actuation(cx, &app, KEY_E), preset0_e);
    let restored = keyboard_snapshot(cx, &app).hall_effect.unwrap().snapshot;
    assert_eq!(restored.socd_pairs.len(), 1);
    assert_eq!(restored.rapid_switch_pairs.len(), 1);
}

#[gpui_kit::test]
fn paired_keys_have_separate_travel_and_rt_and_disable_matrix_selection(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));

    // W/S (RS) and A/D (SOCD) are owned by pairs: clicking them on the trigger map must NOT select
    // them for regular per-key actuation/RT edits (it focuses their paired mode instead), and
    // Select All skips them too.
    for paired in [KEY_W, KEY_S, KEY_A, KEY_D] {
        click(cx, handle, key_id(paired));
    }
    cx.update(|cx| assert!(app.read(cx).selected_keys().is_empty()));

    click(cx, handle, "select-all-keys");
    cx.update(|cx| {
        let sel = app.read(cx).selected_keys();
        for paired in [KEY_A, KEY_D, KEY_W, KEY_S] {
            assert!(!sel.contains(&KeyIndex::new(paired)));
        }
        assert!(sel.contains(&KeyIndex::new(KEY_Q)));
        assert!(sel.contains(&KeyIndex::new(KEY_E)));
    });
    click(cx, handle, "clear-keys");

    // Existing SOCD pair exposes its own independent travel and Rapid Trigger controls.
    let before = keyboard_snapshot(cx, &app)
        .hall_effect
        .unwrap()
        .snapshot
        .socd_pairs[0];
    assert_eq!(before.trigger_point, travel(150));
    assert!(!before.rapid_trigger);

    click(cx, handle, "socd-travel-inc-49-51-p");
    click(cx, handle, "socd-rt-49-51-sw");
    let after = keyboard_snapshot(cx, &app)
        .hall_effect
        .unwrap()
        .snapshot
        .socd_pairs[0];
    assert_eq!(after.trigger_point, travel(152));
    assert!(after.rapid_trigger);

    // Creating a new RS pair on free keys (Q + E) saves its dedicated travel and Rapid Trigger state.
    click(cx, handle, "paired-mode-rs");
    click(cx, handle, "paired-pick-first");
    click(cx, handle, key_id(KEY_Q));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "paired-travel-dec");
    click(cx, handle, "paired-rt");
    click(cx, handle, "paired-add");
    let rs_pairs = keyboard_snapshot(cx, &app)
        .hall_effect
        .unwrap()
        .snapshot
        .rapid_switch_pairs;
    assert_eq!(rs_pairs.len(), 2);
    assert_eq!(rs_pairs[1].trigger_point, travel(148));
    assert!(rs_pairs[1].rapid_trigger);
}

#[gpui_kit::test]
fn workspace_scrolls_at_the_default_window_size(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let service: Arc<dyn PeripheralService> = Arc::new(MockPeripheralService::new());
    let (handle, _app) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1240.), px(820.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| AgnoPeriphApp::build(service, AppOptions::default(), window, cx),
        )
        .expect("open test window")
    });
    settle(cx, handle);
    click(cx, handle, device_element_id(&keyboard()));

    // With dev tools (packet inspector) open at 1240x820, the bottom trigger deck sits below the fold.
    let visible = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, _| {
            window.find("release-sensitivity-inc").visible()
        })
        .unwrap()
    };
    assert!(
        !visible(cx),
        "bottom deck starts below the fold with inspector open"
    );
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            key_id(KEY_W),
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-4000.))),
            cx,
        );
    })
    .unwrap();
    settle(cx, handle);
    assert!(visible(cx), "precise scrolling reaches the bottom deck");
}

#[gpui_kit::test]
fn reopened_window_rebinds_controls(cx: &mut TestAppContext) {
    let (first, app) = open_app(cx, MockPeripheralService::new());
    click(cx, first, device_element_id(&keyboard()));

    // Closing to the tray drops the window but keeps the app entity alive.
    cx.update_window(first, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let reopened = cx.update(|cx| {
        let app = app.clone();
        gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
            app.update(cx, |app, cx| app.attach_window(window, cx));
            app
        })
        .expect("reopen window")
        .0
    });
    settle(cx, reopened);

    click(cx, reopened, "rename-preset");
    type_into(cx, reopened, "rename-input", "Tray return");
    click(cx, reopened, "rename-save");
    cx.update(|cx| {
        let name = app
            .read(cx)
            .config()
            .preset_name(&keyboard(), ProfileSlot::new(0));
        assert_eq!(name, "Tray return");
    });
}

/// A on the RS7 matrix (row 3, column 1); the mock pairs it with D in SOCD.
const KEY_A: u16 = 3 * 16 + 1;
/// D on the RS7 matrix (row 3, column 3).
const KEY_D: u16 = 3 * 16 + 3;

fn is_disabled(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> bool {
    cx.update_window(handle, |_, window, _| window.find(id).disabled())
        .unwrap()
        == Some(true)
}

#[gpui_kit::test]
fn advanced_keys_refuse_keys_owned_by_another_function(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    let socd = keyboard_snapshot(cx, &app)
        .hall_effect
        .unwrap()
        .snapshot
        .socd_pairs;
    assert_eq!(socd.len(), 1);
    assert_eq!(
        (socd[0].key_a, socd[0].key_b),
        (KeyIndex::new(KEY_A), KeyIndex::new(KEY_D))
    );

    for mode in [
        "paired-mode-rs",
        "paired-mode-mt",
        "paired-mode-tgl",
        "paired-mode-dks",
    ] {
        click(cx, handle, mode);
        click(cx, handle, "paired-pick-first");
        assert!(exists(cx, handle, "paired-pick-hint"), "{mode} arms a pick");

        // A and D belong to SOCD: the pick is refused and stays armed.
        for taken in [KEY_A, KEY_D] {
            click(cx, handle, key_id(taken));
            assert!(
                exists(cx, handle, "paired-conflict"),
                "{mode} refuses {taken}"
            );
            assert!(exists(cx, handle, "paired-pick-hint"), "{mode} stays armed");
            assert!(is_disabled(cx, handle, "paired-add"), "{mode} cannot add");
            cx.update(|cx| assert!(app.read(cx).selected_keys().is_empty()));
        }

        // A free key is accepted and clears the note.
        click(cx, handle, key_id(KEY_Q));
        assert!(!exists(cx, handle, "paired-conflict"));
        click(cx, handle, "paired-cancel");
    }

    // The backend refuses the same cross-function reuse, whatever the UI does.
    let service = MockPeripheralService::new();
    service.refresh().unwrap();
    let pair = RapidSwitchPair::new(KeyIndex::new(KEY_A), KeyIndex::new(KEY_Q)).unwrap();
    let err = service
        .apply(&keyboard(), SettingChange::RapidSwitch(vec![pair]))
        .expect_err("SOCD key reused by RS");
    assert!(err.rejected_before_send, "{}", err.error);
    assert!(err.error.to_string().contains("SOCD"), "{}", err.error);
    for change in [
        SettingChange::ModTap(ModTapBinding::new(
            0,
            KeyIndex::new(KEY_D),
            (0, 0x2C),
            (0, 0xE1),
            200,
        )),
        SettingChange::ToggleKey(ToggleKeyBinding::new(
            0,
            KeyIndex::new(KEY_A),
            (0, 0x2C),
            200,
        )),
    ] {
        let err = service
            .apply(&keyboard(), change)
            .expect_err("SOCD key reused by MT or TGL");
        assert!(err.error.to_string().contains("SOCD"), "{}", err.error);
    }
}

#[gpui_kit::test]
fn lighting_deck_picks_colors_and_toggles_entry_format(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Lighting);
    // Picks stage as unsaved edits; the editors show them, so read the shown values.
    let shown =
        |cx: &mut TestAppContext| cx.update(|cx| app.read(cx).selected_device().unwrap().clone());
    let color = |cx: &mut TestAppContext| match zone_effect(&shown(cx), 0) {
        RgbEffect::Preset { color, .. } => color,
        other => panic!("expected a preset, got {other:?}"),
    };

    // Back Lamp: a single-color preset, then a swatch.
    click(cx, handle, "rgb-0-preset-2");
    click(cx, handle, "rgb-swatch-Blue");
    assert_eq!(color(cx), RgbColor::new(0, 90, 255));

    // The pad's center is half saturation, half value of the same hue.
    click(cx, handle, "rgb-sv-pad");
    let picked = color(cx);
    assert!(picked.b > picked.g && picked.g > picked.r, "{picked}");
    assert!((120..=135).contains(&picked.b), "{picked}");

    // The strip's center is cyan; saturation and value are kept.
    click(cx, handle, "rgb-hue-strip");
    let cyan = color(cx);
    assert!(cyan.g.abs_diff(cyan.b) <= 6 && cyan.r < cyan.g, "{cyan}");

    // HEX is the default entry; RGB swaps in channel steppers that write.
    assert!(exists(cx, handle, "rgb-format-hex"));
    assert!(!exists(cx, handle, ("rgb-channel-inc", 0u64)));
    click(cx, handle, "rgb-format-rgb");
    let before = color(cx);
    click(cx, handle, ("rgb-channel-inc", 0u64));
    assert_eq!(color(cx).r, before.r + 5);

    // Logo Light: full color mode lives in the color column.
    click(cx, handle, ("rgb-zone", 1u64));
    click(cx, handle, "rgb-1-preset-1");
    assert!(!is_disabled(cx, handle, "rgb-colorful"));
    click(cx, handle, "rgb-colorful");
    apply(cx, handle);
    assert!(matches!(
        zone_effect(&keyboard_snapshot(cx, &app), 1),
        RgbEffect::Preset {
            id: 1,
            colorful: true,
            ..
        }
    ));
    assert!(is_disabled(cx, handle, "rgb-swatch-Red"));
}

#[gpui_kit::test]
fn keybinds_offer_verified_extra_keys(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Keybinds);
    let code_of = |cx: &mut TestAppContext| {
        keyboard_snapshot(cx, &app)
            .keymap
            .as_ref()
            .unwrap()
            .snapshot
            .layers[0][usize::from(KEY_Q)]
    };

    click(cx, handle, ("keymap-key", u64::from(KEY_Q)));
    click(cx, handle, "keymap-picker-extra");
    // Media & audio, Windows system keys, Backlight control, Logo light control.
    for id in [0x00A8u64, 0x00A9, 0x00B1, 0x7820, 0x7E1F] {
        assert!(exists(cx, handle, ("extra-key", id)), "extra key {id:#06X}");
    }
    click(cx, handle, ("extra-key", 0x00A8u64));
    assert_eq!(code_of(cx), KeyCode::new(0x00, 0xA8));
    click(cx, handle, ("extra-key", 0x00A9u64));
    assert_eq!(code_of(cx), KeyCode::new(0x00, 0xA9));
    click(cx, handle, ("extra-key", 0x7820u64));
    assert_eq!(code_of(cx), KeyCode::new(0x78, 0x20));
}

#[gpui_kit::test]
fn release_build_options_hide_developer_tools(cx: &mut TestAppContext) {
    let (handle, _app) = open_app_with(
        cx,
        MockPeripheralService::new(),
        AppOptions::default().with_dev_tools(false),
    );
    assert!(exists(cx, handle, "refresh"));
    assert!(!exists(cx, handle, "dry-run"));
    assert!(!exists(cx, handle, "inspector-toggle"));
    assert!(!exists(cx, handle, "inspector-clear"));
}

#[gpui_kit::test]
fn triggers_and_lighting_fit_the_default_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let service: Arc<dyn PeripheralService> = Arc::new(MockPeripheralService::new());
    let (handle, app) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1240.), px(820.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                AgnoPeriphApp::build(
                    service,
                    AppOptions::default().with_dev_tools(false),
                    window,
                    cx,
                )
            },
        )
        .expect("open test window")
    });
    settle(cx, handle);
    click(cx, handle, device_element_id(&keyboard()));
    settle(cx, handle);
    assert!(
        cx.update(|cx| app.read(cx).has_live_travel()),
        "mock keyboard must report live travel"
    );
    // `visible()` is true for a partly clipped element, so check whole bounds too.
    let on_screen = |cx: &mut TestAppContext, id: ElementId| {
        let found = cx
            .update_window(handle, |_, window, _| window.find(id.clone()))
            .unwrap();
        let bounds = found.bounds();
        assert!(found.visible(), "{id:?} is visible");
        assert!(
            bounds.bottom() <= px(820.) && bounds.right() <= px(1240.),
            "{id:?} lies fully inside the window: {bounds:?}"
        );
    };

    // The matrix, its selection tools, the paired-keys card and the whole trigger deck share one screen.
    for id in [
        ElementId::from(key_id(KEY_W)),
        "restore-default-keys".into(),
        "paired-mode-tgl".into(),
        "socd-remove-49-51-x".into(),
        "socd-rt-49-51-sw".into(),
        "deadzone-top-inc".into(),
        "deadzone-bottom-inc".into(),
        "release-sensitivity-inc".into(),
        "actuation-all-keys".into(),
        "key-tester".into(),
    ] {
        on_screen(cx, id);
    }

    open_tab(cx, handle, KeyboardTab::Lighting);
    for id in [
        ElementId::from("rgb-0-preset-39"),
        "rgb-swatch-White".into(),
        "rgb-format-rgb".into(),
        "rgb-colorful".into(),
        "rgb-preview".into(),
    ] {
        on_screen(cx, id);
    }

    // RGB entry fits its column: the last stepper stays clear of the preview and
    // inside the column the full color switch closes.
    click(cx, handle, "rgb-0-preset-2");
    click(cx, handle, "rgb-format-rgb");
    let bounds = |cx: &mut TestAppContext, id: ElementId| {
        cx.update_window(handle, |_, window, _| window.find(id).bounds())
            .unwrap()
    };
    let last = bounds(cx, ("rgb-channel-inc", 2u64).into());
    let preview = bounds(cx, "rgb-preview".into());
    let switch = bounds(cx, "rgb-colorful".into());
    assert!(!last.intersects(&preview), "{last:?} overlaps {preview:?}");
    assert!(last.right() <= switch.right(), "{last:?} past {switch:?}");
}

#[gpui_kit::test]
fn configured_socd_entry_keeps_remove_clear_of_its_modes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let service: Arc<dyn PeripheralService> = Arc::new(MockPeripheralService::new());
    let (handle, _app) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1240.), px(2400.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                AgnoPeriphApp::build(
                    service,
                    AppOptions::default().with_dev_tools(false),
                    window,
                    cx,
                )
            },
        )
        .expect("open test window")
    });
    settle(cx, handle);
    click(cx, handle, device_element_id(&keyboard()));
    let bounds = |cx: &mut TestAppContext, id: &str| {
        let id = ElementId::Name(id.to_owned().into());
        cx.update_window(handle, |_, window, _| window.find(id).bounds())
            .unwrap()
    };
    let remove = bounds(cx, "socd-remove-49-51-x");
    let card = bounds(cx, "paired-configured");
    for label in ["Last input", "Neutral", "First key", "Second key"] {
        let mode = bounds(cx, &format!("socd-mode-49-51-{label}"));
        assert!(
            !mode.intersects(&remove),
            "{label} {mode:?} overlaps Remove"
        );
        assert!(
            mode.right() <= card.right(),
            "{label} {mode:?} leaves the card"
        );
    }
    assert!(remove.right() <= card.right());
}

struct RecordingSystem {
    notifications: std::sync::Mutex<Vec<(String, String)>>,
    opened: std::sync::Mutex<Vec<String>>,
    launched: std::sync::Mutex<Vec<std::path::PathBuf>>,
}

impl RecordingSystem {
    fn new() -> Self {
        Self {
            notifications: std::sync::Mutex::new(Vec::new()),
            opened: std::sync::Mutex::new(Vec::new()),
            launched: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn take(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.notifications.lock().unwrap())
    }

    fn take_opened(&self) -> Vec<String> {
        std::mem::take(&mut *self.opened.lock().unwrap())
    }

    fn take_launched(&self) -> Vec<std::path::PathBuf> {
        std::mem::take(&mut *self.launched.lock().unwrap())
    }
}

impl agnoperiph::system::SystemIntegration for RecordingSystem {
    fn set_start_with_windows(&self, _enabled: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }

    fn is_start_with_windows_enabled(&self) -> bool {
        false
    }

    fn save_config(&self, _config: &agnoperiph::game_profiles::AppConfig) -> Result<(), String> {
        Ok(())
    }

    fn send_notification(&self, title: &str, body: &str) -> Result<(), String> {
        self.notifications
            .lock()
            .unwrap()
            .push((title.to_owned(), body.to_owned()));
        Ok(())
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        self.opened.lock().unwrap().push(url.to_owned());
        Ok(())
    }

    fn launch_installer(&self, path: &std::path::Path) -> Result<bool, String> {
        self.launched.lock().unwrap().push(path.to_path_buf());
        Ok(false)
    }
}

/// A release source that never touches the network.
struct FixedReleases {
    release: Option<agnoperiph::updates::UpdateInfo>,
    downloads: std::sync::Mutex<Vec<(String, std::path::PathBuf)>>,
    download_error: Option<String>,
}

impl FixedReleases {
    fn new(release: Option<agnoperiph::updates::UpdateInfo>) -> Self {
        Self {
            release,
            downloads: std::sync::Mutex::new(Vec::new()),
            download_error: None,
        }
    }

    fn failing_download(
        release: Option<agnoperiph::updates::UpdateInfo>,
        err: impl Into<String>,
    ) -> Self {
        Self {
            release,
            downloads: std::sync::Mutex::new(Vec::new()),
            download_error: Some(err.into()),
        }
    }

    fn take_downloads(&self) -> Vec<(String, std::path::PathBuf)> {
        std::mem::take(&mut *self.downloads.lock().unwrap())
    }
}

impl agnoperiph::updates::ReleaseSource for FixedReleases {
    fn latest(&self) -> Result<Option<agnoperiph::updates::UpdateInfo>, String> {
        Ok(self.release.clone())
    }

    fn download_installer(&self, url: &str, destination: &std::path::Path) -> Result<(), String> {
        self.downloads
            .lock()
            .unwrap()
            .push((url.to_owned(), destination.to_path_buf()));
        if let Some(err) = &self.download_error {
            return Err(err.clone());
        }
        Ok(())
    }
}

#[gpui_kit::test]
fn game_profile_switch_sends_batched_windows_notification(cx: &mut TestAppContext) {
    let system = Arc::new(RecordingSystem::new());
    let options = AppOptions::default().with_system(system.clone());
    let (handle, app) = open_app_with(cx, MockPeripheralService::new(), options);

    click(cx, handle, "nav-game-profiles");
    type_into(cx, handle, "manual-game-name", "VALORANT");
    type_into(cx, handle, "manual-game-exe", "VALORANT-Win64-Shipping.exe");
    click(cx, handle, "add-manual-game");

    // Assign both Lamzu mouse and ATK RS7 keyboard presets for VALORANT, and a default for mouse.
    click(cx, handle, format!("target-{}-1", mouse()));
    click(cx, handle, format!("target-{}-3", keyboard()));
    click(cx, handle, "game-profile-default");
    click(cx, handle, format!("target-{}-0", mouse()));

    // Opening VALORANT switches both devices and sends one batched notification.
    let running = HashSet::from(["valorant-win64-shipping.exe".to_owned()]);
    app.update(cx, |app, cx| app.handle_running_exes(&running, cx));
    settle(cx, handle);
    let sent = system.take();
    assert_eq!(sent.len(), 1, "expected 1 batched notification: {sent:?}");
    assert_eq!(sent[0].0, "VALORANT — Presets switched");
    assert!(sent[0].1.contains("Lamzu Atlantis"), "{}", sent[0].1);
    assert!(sent[0].1.contains("ATK RS7"), "{}", sent[0].1);

    // Polling again with the same running game does not repeat the notification.
    app.update(cx, |app, cx| app.handle_running_exes(&running, cx));
    settle(cx, handle);
    assert_eq!(system.take(), Vec::<(String, String)>::new());

    // Closing VALORANT restores the mouse preset and sends one restore notification.
    app.update(cx, |app, cx| app.handle_running_exes(&HashSet::new(), cx));
    settle(cx, handle);
    let sent = system.take();
    assert_eq!(sent.len(), 1, "expected 1 restore notification: {sent:?}");
    assert_eq!(sent[0].0, "Desktop / Default — Presets restored");
    assert!(sent[0].1.contains("Lamzu Atlantis"), "{}", sent[0].1);

    // Disabling "Notify on switch" suppresses future notifications.
    click(cx, handle, "notify-on-switch");
    app.update(cx, |app, cx| app.handle_running_exes(&running, cx));
    settle(cx, handle);
    assert_eq!(system.take(), Vec::<(String, String)>::new());
}

#[gpui_kit::test]
fn auto_update_check_offers_download_and_dismiss(cx: &mut TestAppContext) {
    let system = Arc::new(RecordingSystem::new());
    let release_url = "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v9.9.9";
    let releases = Arc::new(FixedReleases::new(Some(
        agnoperiph::updates::UpdateInfo::new("9.9.9", release_url),
    )));
    let options = AppOptions::default()
        .with_system(system.clone())
        .with_update_check(true)
        .with_release_source(releases);
    let (handle, app) = open_app_with(cx, MockPeripheralService::new(), options);
    settle(cx, handle);

    cx.update(|cx| {
        let update = app
            .read(cx)
            .update_available()
            .expect("the startup check must offer 9.9.9");
        assert_eq!(update.version(), "9.9.9");
    });

    click(cx, handle, "update-download");
    assert_eq!(system.take_opened(), [release_url.to_owned()]);
    assert_eq!(system.take_launched(), Vec::<std::path::PathBuf>::new());

    click(cx, handle, "update-dismiss");
    assert!(cx.update(|cx| app.read(cx).update_available().is_none()));
}

#[gpui_kit::test]
fn auto_update_downloads_and_launches_installer_when_asset_exists(cx: &mut TestAppContext) {
    let system = Arc::new(RecordingSystem::new());
    let release_url = "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v9.9.9";
    let installer_url = "https://github.com/D9theCoder/AgnoPeriph/releases/download/v9.9.9/AgnoPeriph-Setup-x64.exe";
    let releases = Arc::new(FixedReleases::new(Some(
        agnoperiph::updates::UpdateInfo::new("9.9.9", release_url)
            .with_installer("AgnoPeriph-Setup-x64.exe", installer_url),
    )));
    let options = AppOptions::default()
        .with_system(system.clone())
        .with_update_check(true)
        .with_release_source(releases.clone());
    let (handle, app) = open_app_with(cx, MockPeripheralService::new(), options);
    settle(cx, handle);

    click(cx, handle, "update-download");
    settle(cx, handle);

    assert_eq!(system.take_opened(), Vec::<String>::new());
    let downloads = releases.take_downloads();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].0, installer_url);
    assert_eq!(system.take_launched(), [downloads[0].1.clone()]);
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(!app.is_installing_update());
        assert_eq!(app.banner(), None);
    });
}

#[gpui_kit::test]
fn failed_update_download_shows_error_banner_without_launching(cx: &mut TestAppContext) {
    let system = Arc::new(RecordingSystem::new());
    let releases = Arc::new(FixedReleases::failing_download(
        Some(
            agnoperiph::updates::UpdateInfo::new(
                "9.9.9",
                "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v9.9.9",
            )
            .with_installer(
                "AgnoPeriph-Setup-x64.exe",
                "https://github.com/D9theCoder/AgnoPeriph/releases/download/v9.9.9/AgnoPeriph-Setup-x64.exe",
            ),
        ),
        "network timeout",
    ));
    let options = AppOptions::default()
        .with_system(system.clone())
        .with_update_check(true)
        .with_release_source(releases);
    let (handle, app) = open_app_with(cx, MockPeripheralService::new(), options);
    settle(cx, handle);

    click(cx, handle, "update-download");
    settle(cx, handle);

    assert_eq!(system.take_opened(), Vec::<String>::new());
    assert_eq!(system.take_launched(), Vec::<std::path::PathBuf>::new());
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(!app.is_installing_update());
        let banner = app
            .banner()
            .expect("failed update download must show a banner");
        assert!(banner.message().contains("network timeout"));
    });
    assert!(exists(cx, handle, "dismiss-banner"));
}

#[gpui_kit::test]
fn manual_update_check_stays_quiet_when_the_build_is_current(cx: &mut TestAppContext) {
    let system = Arc::new(RecordingSystem::new());
    let releases = Arc::new(FixedReleases::new(None));
    let options = AppOptions::default()
        .with_system(system.clone())
        .with_release_source(releases);
    let (handle, app) = open_app_with(cx, MockPeripheralService::new(), options);

    click(cx, handle, "check-updates");
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(!app.is_checking_updates());
        assert!(app.update_available().is_none());
    });
    assert_eq!(system.take_opened(), Vec::<String>::new());
}

fn device_snapshot(
    cx: &mut TestAppContext,
    app: &Entity<AgnoPeriphApp>,
    id: &DeviceId,
) -> DeviceSnapshot {
    cx.update(|cx| app.read(cx).device(id).unwrap().clone())
}

fn is_checked(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> Option<bool> {
    cx.update_window(handle, |_, window, _| window.find(id).checked())
        .unwrap()
}

#[gpui_kit::test]
fn sensor_switches_follow_pointer_capabilities(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::multi_brand());
    let pulsar = DeviceId::new(MOCK_PULSAR_ID);
    click(cx, handle, device_element_id(&pulsar));

    assert_eq!(is_checked(cx, handle, "motion-sync"), Some(true));
    assert_eq!(is_checked(cx, handle, "angle-snapping"), Some(false));
    assert_eq!(is_checked(cx, handle, "ripple-control"), Some(true));

    click(cx, handle, "angle-snapping");
    click(cx, handle, "ripple-control");
    apply(cx, handle);
    let pointer = device_snapshot(cx, &app, &pulsar).pointer.unwrap();
    assert_eq!(pointer.snapshot.angle_snapping, Some(true));
    assert_eq!(pointer.snapshot.ripple_control, Some(false));
    cx.update(|cx| {
        let app = app.read(cx);
        assert_eq!(app.banner(), None);
        assert_eq!(
            app.packets().last().unwrap().outcome,
            PacketOutcome::DryRunSuppressed
        );
    });

    // Medium lift-off is offered between Low and High and applies like the others.
    assert!(exists(cx, handle, ("lod", 1u64)));
    assert!(exists(cx, handle, ("lod", 3u64)));
    assert!(exists(cx, handle, ("lod", 2u64)));
    click(cx, handle, ("lod", 2u64));
    click(cx, handle, ("lod", 3u64));
    let pointer = device_snapshot(cx, &app, &pulsar).pointer.unwrap();
    assert_eq!(pointer.snapshot.lod, Some(LiftOffDistance::Medium));

    // A mouse without these features shows none of their switches.
    click(cx, handle, device_element_id(&DeviceId::new(MOCK_RAZER_ID)));
    assert!(!exists(cx, handle, "motion-sync"));
    assert!(!exists(cx, handle, "angle-snapping"));
    assert!(!exists(cx, handle, "ripple-control"));
}

#[gpui_kit::test]
fn mouse_with_rgb_shows_lighting_beside_pointer_settings(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::multi_brand());
    let razer = DeviceId::new(MOCK_RAZER_ID);
    click(cx, handle, device_element_id(&razer));

    assert!(exists(cx, handle, ("polling-rate", 1000u64)));
    assert!(!exists(cx, handle, ("keymap-key", 0u64)));
    // Zone ids are the device's LED ids (0, 1, 4), not list positions.
    for zone in [0u64, 1, 4] {
        assert!(exists(cx, handle, ("rgb-zone", zone)), "zone {zone}");
    }
    click(cx, handle, ("rgb-zone", 4u64));
    click(cx, handle, "rgb-4-effect-Static");
    apply(cx, handle);
    let rgb = device_snapshot(cx, &app, &razer).rgb.unwrap();
    let logo = rgb
        .snapshot
        .zones
        .iter()
        .find(|z| z.zone.value() == 4)
        .unwrap();
    assert!(matches!(logo.effect, RgbEffect::Static { .. }));
    cx.update(|cx| assert_eq!(app.read(cx).banner(), None));
}

#[gpui_kit::test]
fn via_keyboard_without_hall_effect_lands_on_its_own_tabs(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::multi_brand());
    let via = DeviceId::new(MOCK_VIA_ID);
    click(cx, handle, device_element_id(&via));

    // No Triggers tab, so the first available tab (Keybinds) is shown.
    assert!(exists(cx, handle, ("keymap-key", 17u64)));
    assert!(!exists(cx, handle, "open-calibration"));
    assert!(!exists(cx, handle, ("polling-rate", 1000u64)));

    // Key 17 is row 1, column 1 on the 16-column VIA matrix.
    click(cx, handle, ("keymap-key", 17u64));
    click(cx, handle, ("hid-key", 0x04u64));
    let keymap = device_snapshot(cx, &app, &via).keymap.unwrap();
    assert_eq!(keymap.snapshot.layers[0][17], KeyCode::standard(0x04));
    cx.update(|cx| assert_eq!(app.read(cx).banner(), None));

    // The tab bar lists only Keybinds, Lighting and Macros, in that order.
    let click_tab = |cx: &mut TestAppContext, index: usize| {
        cx.update_window(handle, |_, window, cx| {
            window.within("keyboard-tabs").click(index, cx);
        })
        .unwrap();
        settle(cx, handle);
    };
    click_tab(cx, 2);
    cx.update(|cx| assert_eq!(app.read(cx).keyboard_tab(), KeyboardTab::Macros));
    assert!(exists(cx, handle, "save-macros"));
    click_tab(cx, 1);
    cx.update(|cx| assert_eq!(app.read(cx).keyboard_tab(), KeyboardTab::Lighting));
    assert!(exists(cx, handle, "rgb-0-preset-13"));
    click(cx, handle, "rgb-0-preset-1");
    apply(cx, handle);
    let rgb = device_snapshot(cx, &app, &via).rgb.unwrap();
    assert!(matches!(
        rgb.snapshot.zones[0].effect,
        RgbEffect::Preset { id: 1, .. }
    ));
}

#[gpui_kit::test]
fn readonly_hall_effect_keyboard_reports_unsupported_writes(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::multi_brand());
    let wooting = DeviceId::new(MOCK_WOOTING_ID);
    click(cx, handle, device_element_id(&wooting));

    // 6 x 14 matrix: index 14 starts the second row, and every key is drawn.
    assert!(exists(cx, handle, key_id(14)));
    assert!(exists(cx, handle, key_id(83)));
    assert!(!exists(cx, handle, key_id(84)));
    assert!(!exists(cx, handle, "open-calibration"));

    click(cx, handle, key_id(14));
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        let banner = app.banner().expect("unsupported write shows a banner");
        assert!(banner.message().contains("Nothing was written"));
        assert!(banner.message().contains("unsupported capability"));
        let he = app.device(&wooting).unwrap().hall_effect.as_ref().unwrap();
        assert_eq!(he.key(KeyIndex::new(14)).unwrap().actuation, travel(200));
    });
}

fn pending_ids(cx: &mut TestAppContext, app: &Entity<AgnoPeriphApp>) -> Vec<ChangeId> {
    cx.update(|cx| {
        app.read(cx)
            .pending_changes()
            .into_iter()
            .map(|change| change.id)
            .collect()
    })
}

fn is_dirty(cx: &mut TestAppContext, app: &Entity<AgnoPeriphApp>) -> bool {
    cx.update(|cx| app.read(cx).is_dirty())
}

fn selected_id(cx: &mut TestAppContext, app: &Entity<AgnoPeriphApp>) -> Option<DeviceId> {
    cx.update(|cx| app.read(cx).selected_device_id().cloned())
}

#[gpui_kit::test]
fn dirty_bar_collects_edits_across_sections_and_applies_once(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    assert!(!exists(cx, handle, "dirty-apply"));

    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    click(cx, handle, "deadzone-top-inc");
    open_tab(cx, handle, KeyboardTab::Lighting);
    click(cx, handle, "rgb-0-preset-5");
    open_tab(cx, handle, KeyboardTab::Advanced);
    click(cx, handle, "config-win-lock");

    assert_eq!(
        pending_ids(cx, &app),
        [
            ChangeId::Actuation,
            ChangeId::Deadzones,
            ChangeId::Lighting(0),
            ChangeId::KeyboardConfig
        ]
    );
    // Nothing reached the keyboard yet.
    assert_eq!(key_actuation(cx, &app, KEY_E), 120);
    let snapshot = keyboard_snapshot(cx, &app);
    assert!(!snapshot.keyboard_config.as_ref().unwrap().snapshot.win_lock);

    cx.simulate_keystrokes(handle, "ctrl-s");
    settle(cx, handle);
    assert!(!is_dirty(cx, &app));
    assert!(!exists(cx, handle, "dirty-apply"));
    let snapshot = keyboard_snapshot(cx, &app);
    assert_eq!(key_actuation(cx, &app, KEY_E), 122);
    assert!(snapshot.keyboard_config.as_ref().unwrap().snapshot.win_lock);
    assert!(matches!(
        zone_effect(&snapshot, 0),
        RgbEffect::Preset { id: 5, .. }
    ));
}

#[gpui_kit::test]
fn undo_steps_back_one_edit_and_reverts_single_changes(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, ("dpi-inc", 1u64));
    click(cx, handle, ("polling-rate", 500u64));
    click(cx, handle, "angle-snapping");
    assert_eq!(pending_ids(cx, &app).len(), 3);

    cx.simulate_keystrokes(handle, "ctrl-z");
    settle(cx, handle);
    assert_eq!(
        pending_ids(cx, &app),
        [ChangeId::DpiStages, ChangeId::PollingRate]
    );
    click(cx, handle, "dirty-undo");
    assert_eq!(pending_ids(cx, &app), [ChangeId::DpiStages]);

    // The changes list reverts one entry without touching the rest.
    click(cx, handle, ("polling-rate", 250u64));
    click(cx, handle, "dirty-changes");
    assert!(cx.update(|cx| app.read(cx).is_changes_open()));
    click(cx, handle, "revert-dpi-stages");
    assert_eq!(pending_ids(cx, &app), [ChangeId::PollingRate]);
}

#[gpui_kit::test]
fn discard_clears_everything_and_offers_undo(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, ("dpi-inc", 1u64));
    click(cx, handle, ("polling-rate", 500u64));

    click(cx, handle, "dirty-discard");
    assert!(!is_dirty(cx, &app));
    // Let the notification slide in before clicking its action.
    cx.executor().advance_clock(Duration::from_millis(500));
    settle(cx, handle);
    assert!(exists(cx, handle, "discard-undo"));

    click(cx, handle, "discard-undo");
    cx.executor().advance_clock(Duration::from_millis(500));
    settle(cx, handle);
    assert_eq!(
        pending_ids(cx, &app),
        [ChangeId::DpiStages, ChangeId::PollingRate]
    );
    assert!(!exists(cx, handle, "discard-undo"));
}

#[gpui_kit::test]
fn switching_device_with_unsaved_edits_asks_first(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    let mouse = DeviceId::new(MOCK_MOUSE_ID);
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");

    // Cancel keeps the device and the edit.
    click(cx, handle, device_element_id(&mouse));
    assert!(exists(cx, handle, "guard-apply"));
    click(cx, handle, "guard-cancel");
    assert_eq!(selected_id(cx, &app), Some(keyboard()));
    assert!(is_dirty(cx, &app));

    // Discard and switch drops the edit.
    click(cx, handle, device_element_id(&mouse));
    click(cx, handle, "guard-discard");
    assert_eq!(selected_id(cx, &app), Some(mouse.clone()));
    assert!(!is_dirty(cx, &app));
    assert_eq!(key_actuation(cx, &app, KEY_E), 120);

    // Apply and switch writes first.
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    click(cx, handle, device_element_id(&mouse));
    click(cx, handle, "guard-apply");
    cx.executor().advance_clock(Duration::from_millis(500));
    settle(cx, handle);
    assert_eq!(selected_id(cx, &app), Some(mouse));
    assert!(!is_dirty(cx, &app));
    assert_eq!(key_actuation(cx, &app, KEY_E), 122);

    // Without edits, switching needs no confirmation.
    click(cx, handle, device_element_id(&keyboard()));
    assert!(!exists(cx, handle, "guard-apply"));
    assert_eq!(selected_id(cx, &app), Some(keyboard()));
}

fn active_slot(
    cx: &mut TestAppContext,
    app: &Entity<AgnoPeriphApp>,
    device: &DeviceId,
) -> Option<u8> {
    cx.update(|cx| {
        app.read(cx)
            .tray_model()
            .devices
            .iter()
            .find(|d| d.id == *device)
            .and_then(|d| d.active_slot)
    })
}

fn request_switch(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    app: &Entity<AgnoPeriphApp>,
    device: &DeviceId,
    slot: u8,
) {
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.request_profile_switch(device, slot, window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);
}

#[gpui_kit::test]
fn switching_preset_with_unsaved_edits_asks_first(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    let mouse = DeviceId::new(MOCK_MOUSE_ID);
    click(cx, handle, ("dpi-inc", 1u64));
    assert_eq!(active_slot(cx, &app, &mouse), Some(0));

    request_switch(cx, handle, &app, &mouse, 1);
    assert!(exists(cx, handle, "guard-cancel"));
    click(cx, handle, "guard-cancel");
    assert_eq!(active_slot(cx, &app, &mouse), Some(0));
    assert!(is_dirty(cx, &app));

    request_switch(cx, handle, &app, &mouse, 1);
    click(cx, handle, "guard-discard");
    assert_eq!(active_slot(cx, &app, &mouse), Some(1));
    assert!(!is_dirty(cx, &app));
}

#[gpui_kit::test]
fn quick_presets_stage_without_writing(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    let packets = cx.update(|cx| app.read(cx).packets().len());
    cx.update(|cx| {
        app.update(cx, |app, cx| {
            app.load_quick_preset(QuickPreset::FpsLowActuation, cx);
        });
    });
    settle(cx, handle);
    assert!(pending_ids(cx, &app).contains(&ChangeId::Actuation));
    assert_eq!(key_actuation(cx, &app, KEY_E), 120);
    assert_eq!(cx.update(|cx| app.read(cx).packets().len()), packets);

    apply(cx, handle);
    assert!(key_actuation(cx, &app, KEY_E) < 120);
}

#[gpui_kit::test]
fn profile_json_round_trips_through_the_import_preview(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    let (file_name, json) = cx
        .update(|cx| app.read(cx).export_profile_json())
        .expect("keyboard exports");
    assert!(
        std::path::Path::new(&file_name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    );

    // Restore the default actuation, then import the exported file.
    click(cx, handle, "restore-default-keys");
    apply(cx, handle);
    assert_eq!(key_actuation(cx, &app, KEY_E), 120);
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.open_import_preview(&file_name, &json, window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);
    assert!(exists(cx, handle, "import-confirm"));
    click(cx, handle, "import-confirm");
    assert!(pending_ids(cx, &app).contains(&ChangeId::Actuation));
    assert_eq!(key_actuation(cx, &app, KEY_E), 120, "imports stage first");
    apply(cx, handle);
    assert_eq!(key_actuation(cx, &app, KEY_E), 122);

    // A broken file shows the error and offers another file, not Import.
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.open_import_preview("notes.json", "{ not json", window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);
    assert!(!exists(cx, handle, "import-confirm"));
    assert!(exists(cx, handle, "import-choose-another"));
}

#[gpui_kit::test]
fn export_asks_for_a_path_and_writes_the_file(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    let target =
        std::env::temp_dir().join(format!("agnoperiph-export-{}.json", std::process::id()));
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| app.begin_export(window, cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_new_path());
    let chosen = target.clone();
    cx.simulate_new_path_selection(move |_| Some(chosen));
    // The write runs on the background executor; give it a few turns.
    for _ in 0..50 {
        settle(cx, handle);
        if target.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(&target).expect("export written");
    let _ = std::fs::remove_file(&target);
    let (_, expected) = cx.update(|cx| app.read(cx).export_profile_json()).unwrap();
    assert_eq!(text, expected);
}

#[gpui_kit::test]
fn live_key_tester_polls_only_while_visible(cx: &mut TestAppContext) {
    let service = MockPeripheralService::new();
    service.set_live_key_travel(
        &keyboard(),
        vec![KeyTravelSample::new(KeyIndex::new(KEY_E), travel(180))],
    );
    let (handle, app) = open_app(cx, service);
    click(cx, handle, device_element_id(&keyboard()));
    let polls = |cx: &mut TestAppContext| cx.update(|cx| app.read(cx).live_travel_polls());
    let polling = |cx: &mut TestAppContext| cx.update(|cx| app.read(cx).is_live_polling());
    assert!(cx.update(|cx| app.read(cx).has_live_travel()));

    for _ in 0..3 {
        cx.executor().advance_clock(LIVE_POLL_INTERVAL);
        settle(cx, handle);
    }
    assert!(polls(cx) >= 2);
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(app.is_live_polling());
        assert_eq!(app.tester_key(), KeyIndex::new(KEY_E));
        assert_eq!(app.tester_depth(), 180);
    });

    // Another tab stops polling.
    open_tab(cx, handle, KeyboardTab::Lighting);
    let stopped = polls(cx);
    cx.executor().advance_clock(LIVE_POLL_INTERVAL * 4);
    settle(cx, handle);
    assert!(!polling(cx));
    assert_eq!(polls(cx), stopped);

    // Back on Triggers it resumes.
    open_tab(cx, handle, KeyboardTab::Triggers);
    assert!(polling(cx));

    // Leaving the device stops it too.
    click(cx, handle, device_element_id(&DeviceId::new(MOCK_MOUSE_ID)));
    assert!(!polling(cx));
}

#[gpui_kit::test]
fn live_key_tester_recovers_from_disconnect_without_refresh(cx: &mut TestAppContext) {
    let service = MockPeripheralService::new();
    service.set_live_key_travel(
        &keyboard(),
        vec![KeyTravelSample::new(KeyIndex::new(KEY_E), travel(210))],
    );
    service.fail_live_key_travel(
        &keyboard(),
        2,
        "HID I/O error: hidapi error: WriteFile: (0x0000048F) The device is not connected.",
    );

    let (handle, app) = open_app(cx, service);
    click(cx, handle, device_element_id(&keyboard()));

    // First poll hits the simulated 0x0000048F disconnect: polling must stay alive and show a
    // clean reconnecting note instead of permanently stopping.
    cx.executor().advance_clock(LIVE_POLL_INTERVAL);
    settle(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(app.is_live_polling(), "disconnect must not stop polling");
        assert_eq!(app.tester_error(), Some("Reconnecting to keyboard…"));
        assert_eq!(app.tester_depth(), 0);
    });

    // Second retry (after backoff) hits the second simulated failure, still stays alive.
    cx.executor().advance_clock(LIVE_RETRY_INTERVAL);
    settle(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(app.is_live_polling());
        assert_eq!(app.tester_error(), Some("Reconnecting to keyboard…"));
    });

    // Third retry (after 2x backoff) succeeds automatically without clicking Refresh.
    cx.executor().advance_clock(LIVE_RETRY_INTERVAL * 2);
    settle(cx, handle);
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(app.is_live_polling());
        assert_eq!(app.tester_error(), None);
        assert_eq!(app.tester_key(), KeyIndex::new(KEY_E));
        assert_eq!(app.tester_depth(), 210);
    });
}

#[gpui_kit::test]
fn keyboard_shortcuts_drive_navigation(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());

    cx.simulate_keystrokes(handle, "ctrl-2");
    settle(cx, handle);
    assert_eq!(selected_id(cx, &app), Some(keyboard()));

    cx.simulate_keystrokes(handle, "ctrl-tab");
    settle(cx, handle);
    assert_eq!(
        cx.update(|cx| app.read(cx).keyboard_tab()),
        KeyboardTab::Keybinds
    );
    cx.simulate_keystrokes(handle, "ctrl-shift-tab ctrl-shift-tab");
    settle(cx, handle);
    assert_eq!(
        cx.update(|cx| app.read(cx).keyboard_tab()),
        KeyboardTab::Advanced
    );

    cx.simulate_keystrokes(handle, "ctrl-/");
    settle(cx, handle);
    assert!(cx.update(|cx| app.read(cx).is_shortcuts_open()));
    cx.simulate_keystrokes(handle, "escape");
    settle(cx, handle);
    assert!(!cx.update(|cx| app.read(cx).is_shortcuts_open()));
    click(cx, handle, "shortcuts-toggle");
    assert!(cx.update(|cx| app.read(cx).is_shortcuts_open()));

    cx.simulate_keystrokes(handle, "ctrl-1");
    settle(cx, handle);
    assert_eq!(selected_id(cx, &app), Some(DeviceId::new(MOCK_MOUSE_ID)));
}

#[gpui_kit::test]
fn vid_pid_shows_only_with_developer_tools(cx: &mut TestAppContext) {
    let (handle, _) = open_app(cx, MockPeripheralService::new());
    assert!(exists(cx, handle, "device-vid-pid"));
    let (handle, _) = open_app_with(
        cx,
        MockPeripheralService::new(),
        AppOptions::default().with_dev_tools(false),
    );
    assert!(!exists(cx, handle, "device-vid-pid"));
}

/// Mock keyboard that also advertises Fn inversion and game mode.
struct FunctionKeysService {
    inner: MockPeripheralService,
}

impl FunctionKeysService {
    fn advertise(mut device: DeviceSnapshot) -> DeviceSnapshot {
        if let Some(config) = device.keyboard_config.as_mut() {
            config.capabilities.supports_fn_inversion = true;
            config.capabilities.supports_game_mode = true;
        }
        device
    }
}

impl PeripheralService for FunctionKeysService {
    fn is_dry_run(&self) -> bool {
        self.inner.is_dry_run()
    }

    fn set_dry_run(&self, dry_run: bool) {
        self.inner.set_dry_run(dry_run);
    }

    fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
        Ok(self
            .inner
            .refresh()?
            .into_iter()
            .map(Self::advertise)
            .collect())
    }

    fn apply(
        &self,
        device: &DeviceId,
        change: SettingChange,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.inner.apply(device, change).map(Self::advertise)
    }

    fn packet_log(&self) -> Vec<PacketTraceEntry> {
        self.inner.packet_log()
    }

    fn clear_packet_log(&self) {
        self.inner.clear_packet_log();
    }
}

#[gpui_kit::test]
fn function_key_toggles_follow_keyboard_capabilities(cx: &mut TestAppContext) {
    let (handle, _) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Advanced);
    assert!(!exists(cx, handle, "config-fn-inversion"));
    assert!(!exists(cx, handle, "config-game-mode"));

    let (handle, app) = open_app(
        cx,
        FunctionKeysService {
            inner: MockPeripheralService::new(),
        },
    );
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Advanced);
    assert_eq!(is_checked(cx, handle, "config-fn-inversion"), Some(false));
    click(cx, handle, "config-fn-inversion");
    click(cx, handle, "config-game-mode");
    assert_eq!(pending_ids(cx, &app), [ChangeId::KeyboardConfig]);
    cx.update(|cx| {
        let app = app.read(cx);
        let shown = app
            .selected_device()
            .unwrap()
            .keyboard_config
            .as_ref()
            .unwrap();
        assert!(shown.snapshot.fn_inversion);
        assert!(shown.snapshot.game_mode);
    });
}

/// Mock devices with scripted differences: no paired keys, a fixed mouse battery level,
/// or a polling-rate write that fails.
struct ScriptedService {
    inner: MockPeripheralService,
    strip_pairs: bool,
    mouse_battery: Mutex<Option<u8>>,
    fail_polling_rate: Mutex<bool>,
}

impl ScriptedService {
    fn new() -> Self {
        Self {
            inner: MockPeripheralService::new(),
            strip_pairs: false,
            mouse_battery: Mutex::new(None),
            fail_polling_rate: Mutex::new(false),
        }
    }

    fn adjust(&self, mut device: DeviceSnapshot) -> DeviceSnapshot {
        if self.strip_pairs
            && let Some(he) = device.hall_effect.as_mut()
        {
            he.snapshot.socd_pairs.clear();
            he.snapshot.rapid_switch_pairs.clear();
        }
        if let Some(pct) = *self.mouse_battery.lock().unwrap()
            && device.info.id == mouse()
        {
            device.battery = Some(BatteryStatus::new(
                Some(pct),
                None,
                ChargingState::Discharging,
                true,
            ));
        }
        device
    }
}

impl PeripheralService for ScriptedService {
    fn is_dry_run(&self) -> bool {
        self.inner.is_dry_run()
    }

    fn set_dry_run(&self, dry_run: bool) {
        self.inner.set_dry_run(dry_run);
    }

    fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
        Ok(self
            .inner
            .refresh()?
            .into_iter()
            .map(|d| self.adjust(d))
            .collect())
    }

    fn apply(
        &self,
        device: &DeviceId,
        change: SettingChange,
    ) -> Result<DeviceSnapshot, ApplyError> {
        if *self.fail_polling_rate.lock().unwrap()
            && matches!(
                change,
                SettingChange::Pointer(PointerChange::PollingRate(_))
            )
        {
            return Err(ApplyError::rejected(CoreError::NotConnected(
                "timed out waiting for the receiver".to_owned(),
            )));
        }
        self.inner.apply(device, change).map(|d| self.adjust(d))
    }

    fn packet_log(&self) -> Vec<PacketTraceEntry> {
        self.inner.packet_log()
    }

    fn clear_packet_log(&self) {
        self.inner.clear_packet_log();
    }
}

#[gpui_kit::test]
fn rewriting_every_key_asks_first_from_the_bar_and_the_guard(cx: &mut TestAppContext) {
    // Without paired keys, a quick preset covers every key, which the keyboard writes by
    // erasing and rewriting its whole onboard actuation table.
    let service = ScriptedService {
        strip_pairs: true,
        ..ScriptedService::new()
    };
    let (handle, app) = open_app(cx, service);
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, "dry-run");
    click(cx, handle, "ok");
    cx.update(|cx| {
        app.update(cx, |app, cx| {
            app.load_quick_preset(QuickPreset::Typing, cx);
        });
    });
    settle(cx, handle);

    // Cancel keeps everything staged and writes nothing.
    apply(cx, handle);
    assert!(exists(cx, handle, "cancel"));
    click(cx, handle, "cancel");
    assert!(is_dirty(cx, &app));
    assert_eq!(key_actuation(cx, &app, KEY_E), 120);

    // "Apply and switch" asks too; cancelling it stays on the keyboard with the edit.
    let mouse = DeviceId::new(MOCK_MOUSE_ID);
    click(cx, handle, device_element_id(&mouse));
    click(cx, handle, "guard-apply");
    assert!(exists(cx, handle, "cancel"));
    click(cx, handle, "cancel");
    assert_eq!(selected_id(cx, &app), Some(keyboard()));
    assert!(is_dirty(cx, &app));

    // A later Apply from the bar doesn't run the abandoned switch.
    apply(cx, handle);
    click(cx, handle, "ok");
    assert!(!is_dirty(cx, &app));
    let preset_travel = key_actuation(cx, &app, KEY_E);
    assert_ne!(preset_travel, 120);
    assert_eq!(selected_id(cx, &app), Some(keyboard()));

    // Confirmed through the guard, it writes and then switches.
    cx.update(|cx| {
        app.update(cx, |app, cx| {
            app.load_quick_preset(QuickPreset::FpsLowActuation, cx);
        });
    });
    settle(cx, handle);
    click(cx, handle, device_element_id(&mouse));
    click(cx, handle, "guard-apply");
    assert!(exists(cx, handle, "ok"));
    click(cx, handle, "ok");
    assert_eq!(selected_id(cx, &app), Some(mouse));
    assert!(!is_dirty(cx, &app));
    assert_ne!(key_actuation(cx, &app, KEY_E), preset_travel);
}

#[gpui_kit::test]
fn tray_profile_switch_keeps_unsaved_edits_until_confirmed(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    let mouse = DeviceId::new(MOCK_MOUSE_ID);
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    click(cx, handle, "nav-game-profiles");

    // Another device switches at once and leaves the keyboard's edit alone.
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.request_tray_profile_switch(&mouse, 1, window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);
    assert!(!exists(cx, handle, "guard-apply"));
    assert_eq!(active_slot(cx, &app, &mouse), Some(1));
    assert!(is_dirty(cx, &app));

    // The edited device asks first, back on its own page.
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.request_tray_profile_switch(&keyboard(), 1, window, cx);
        });
    })
    .unwrap();
    settle(cx, handle);
    assert!(exists(cx, handle, "guard-apply"));
    assert!(is_dirty(cx, &app));
    assert_eq!(cx.update(|cx| app.read(cx).route()), Route::Device);
    assert_eq!(selected_id(cx, &app), Some(keyboard()));
    click(cx, handle, "guard-cancel");
    assert!(is_dirty(cx, &app));
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(0));
}

#[gpui_kit::test]
fn partial_apply_keeps_only_the_failed_change_and_retries(cx: &mut TestAppContext) {
    use gpui_kit::test::TestQueryExt as _;
    let service = Arc::new(ScriptedService::new());
    *service.fail_polling_rate.lock().unwrap() = true;
    let (handle, app) = open_app(cx, SharedService(service.clone()));
    click(cx, handle, ("dpi-inc", 1u64));
    click(cx, handle, ("polling-rate", 500u64));
    apply(cx, handle);

    assert_eq!(pending_ids(cx, &app), [ChangeId::PollingRate]);
    let has_retry = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, _| {
            window.try_find_by_label("Retry").is_some()
        })
        .unwrap()
    };
    assert!(has_retry(cx));
    let pointer = device_snapshot(cx, &app, &mouse()).pointer.unwrap();
    assert_eq!(pointer.snapshot.dpi_stages[1].x.value(), 900);

    *service.fail_polling_rate.lock().unwrap() = false;
    apply(cx, handle);
    assert!(!is_dirty(cx, &app));
    assert!(!has_retry(cx));
    let pointer = device_snapshot(cx, &app, &mouse()).pointer.unwrap();
    assert_eq!(
        pointer.snapshot.polling_rate,
        PollingRateHz::from_hz(500).unwrap()
    );
}

/// Lets a test keep a handle on the service it hands to the app.
struct SharedService(Arc<ScriptedService>);

impl PeripheralService for SharedService {
    fn is_dry_run(&self) -> bool {
        self.0.is_dry_run()
    }

    fn set_dry_run(&self, dry_run: bool) {
        self.0.set_dry_run(dry_run);
    }

    fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
        self.0.refresh()
    }

    fn apply(
        &self,
        device: &DeviceId,
        change: SettingChange,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.0.apply(device, change)
    }

    fn packet_log(&self) -> Vec<PacketTraceEntry> {
        self.0.packet_log()
    }

    fn clear_packet_log(&self) {
        self.0.clear_packet_log();
    }
}

#[gpui_kit::test]
fn low_battery_alerts_fire_once_per_threshold_and_respect_the_switch(cx: &mut TestAppContext) {
    let service = Arc::new(ScriptedService::new());
    *service.mouse_battery.lock().unwrap() = Some(15);
    let system = Arc::new(RecordingSystem::new());
    let options = AppOptions::default().with_system(system.clone());
    let (handle, app) = open_app_with(cx, SharedService(service.clone()), options);

    let sent = system.take();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].0.contains("battery at 15%"), "{}", sent[0].0);

    // A second reading at the same level doesn't repeat it.
    click(cx, handle, "refresh");
    assert_eq!(system.take(), Vec::<(String, String)>::new());

    // With alerts off, even the 5% threshold stays quiet.
    click(cx, handle, "pref-low-battery");
    assert!(!cx.update(|cx| app.read(cx).config().low_battery_alerts));
    *service.mouse_battery.lock().unwrap() = Some(4);
    click(cx, handle, "refresh");
    assert_eq!(system.take(), Vec::<(String, String)>::new());
}

#[gpui_kit::test]
fn shortcuts_still_work_after_overlays_close(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    let mouse = DeviceId::new(MOCK_MOUSE_ID);
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");

    // Escape on the guard keeps the edit.
    click(cx, handle, device_element_id(&mouse));
    assert!(exists(cx, handle, "guard-apply"));
    cx.simulate_keystrokes(handle, "escape");
    settle(cx, handle);
    cx.executor().advance_clock(Duration::from_millis(500));
    settle(cx, handle);
    assert!(!exists(cx, handle, "guard-apply"));
    assert!(is_dirty(cx, &app));

    click(cx, handle, device_element_id(&mouse));
    click(cx, handle, "guard-cancel");
    cx.simulate_keystrokes(handle, "ctrl-s");
    settle(cx, handle);
    assert!(!is_dirty(cx, &app));
    assert_eq!(key_actuation(cx, &app, KEY_E), 122);

    // After the discard toast's Undo, Ctrl+S still reaches the app.
    click(cx, handle, "actuation-inc");
    click(cx, handle, "dirty-discard");
    cx.executor().advance_clock(Duration::from_millis(500));
    settle(cx, handle);
    click(cx, handle, "discard-undo");
    cx.simulate_keystrokes(handle, "ctrl-s");
    settle(cx, handle);
    assert!(!is_dirty(cx, &app));
    assert_eq!(key_actuation(cx, &app, KEY_E), 124);
}

#[gpui_kit::test]
fn discard_undo_never_lands_on_another_device(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    click(cx, handle, "dirty-discard");
    cx.executor().advance_clock(Duration::from_millis(500));
    settle(cx, handle);
    click(cx, handle, device_element_id(&DeviceId::new(MOCK_MOUSE_ID)));
    if exists(cx, handle, "discard-undo") {
        click(cx, handle, "discard-undo");
    }
    assert!(!is_dirty(cx, &app));
    assert_eq!(pending_ids(cx, &app), []);
}

#[gpui_kit::test]
fn apply_and_switch_preset_writes_into_the_old_preset(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    request_switch(cx, handle, &app, &keyboard(), 1);
    click(cx, handle, "guard-apply");
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(1));
    assert!(!is_dirty(cx, &app));

    // Back on the first preset, the applied edit is still there.
    request_switch(cx, handle, &app, &keyboard(), 0);
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(0));
    assert_eq!(key_actuation(cx, &app, KEY_E), 122);
}

#[gpui_kit::test]
fn import_reads_the_picked_file(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    let (_, json) = cx.update(|cx| app.read(cx).export_profile_json()).unwrap();
    let source =
        std::env::temp_dir().join(format!("agnoperiph-import-{}.json", std::process::id()));
    std::fs::write(&source, json).unwrap();

    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| app.begin_import(window, cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    let picked = source.clone();
    cx.simulate_path_prompt_response(move |_| Some(vec![picked]));
    // The read runs on the background executor; give it a few turns.
    for _ in 0..50 {
        settle(cx, handle);
        if exists(cx, handle, "import-confirm") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(&source);
    assert!(exists(cx, handle, "import-confirm"));
}

#[gpui_kit::test]
fn profile_tools_menus_open_from_the_preset_bar(cx: &mut TestAppContext) {
    use gpui_kit::test::TestQueryExt as _;
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    let has_label = |cx: &mut TestAppContext, label: &'static str| {
        cx.update_window(handle, |_, window, _| {
            window.try_find_by_label(label).is_some()
        })
        .unwrap()
    };

    click(cx, handle, "quick-presets");
    assert!(has_label(cx, "Typing"));
    cx.simulate_keystrokes(handle, "escape");
    settle(cx, handle);

    click(cx, handle, "profile-file-menu");
    assert!(has_label(cx, "Export to JSON…"));
    assert!(!is_dirty(cx, &app));
}

#[gpui_kit::test]
fn auto_switch_retries_once_after_transient_game_exit_failure(cx: &mut TestAppContext) {
    use agnoperiph::game_profiles::{AppConfig, GameProfile};

    struct FlakyRevertService {
        inner: MockPeripheralService,
        fail_remaining: Arc<Mutex<usize>>,
    }

    impl PeripheralService for FlakyRevertService {
        fn is_dry_run(&self) -> bool {
            self.inner.is_dry_run()
        }

        fn set_dry_run(&self, dry_run: bool) {
            self.inner.set_dry_run(dry_run);
        }

        fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
            self.inner.refresh()
        }

        fn apply(
            &self,
            device: &DeviceId,
            change: SettingChange,
        ) -> Result<DeviceSnapshot, ApplyError> {
            let mut rem = self.fail_remaining.lock().unwrap();
            if *rem > 0 {
                *rem -= 1;
                drop(rem);
                let current = self
                    .inner
                    .refresh()
                    .ok()
                    .and_then(|list| list.into_iter().find(|d| d.id() == device));
                return Err(ApplyError::failed_after_send(
                    CoreError::Protocol("no matching ATK RS7 reply for cmd 0x03".to_owned()),
                    current,
                ));
            }
            drop(rem);
            self.inner.apply(device, change)
        }

        fn packet_log(&self) -> Vec<PacketTraceEntry> {
            self.inner.packet_log()
        }

        fn clear_packet_log(&self) {
            self.inner.clear_packet_log();
        }
    }

    let fail_remaining = Arc::new(Mutex::new(0usize));
    let service = FlakyRevertService {
        inner: MockPeripheralService::new(),
        fail_remaining: fail_remaining.clone(),
    };
    service.set_dry_run(false);
    let mut config = AppConfig::default();
    let mut game = GameProfile::new("cs2", "CS2", "cs2.exe", r"C:\Games\cs2.exe");
    game.device_targets
        .insert(keyboard().as_str().to_owned(), 1);
    config.upsert_game_profile(game);
    config
        .default_targets
        .insert(keyboard().as_str().to_owned(), 0);
    let options = AppOptions::default().with_config(config);
    let (handle, app) = open_app_with(cx, service, options);

    // Edit slot 0 and slot 1 so switching between them emits real HID writes.
    click(cx, handle, device_element_id(&keyboard()));
    click(cx, handle, key_id(KEY_E));
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    let desktop_actuation = key_actuation(cx, &app, KEY_E);

    // Launch game: switches cleanly to slot 1.
    let running = HashSet::from(["cs2.exe".to_owned()]);
    app.update(cx, |app, cx| app.handle_running_exes(&running, cx));
    settle(cx, handle);
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(1));
    // Make slot 1 distinct from slot 0 so reverting emits a write (KEY_E is still selected).
    click(cx, handle, "actuation-inc");
    apply(cx, handle);
    assert_ne!(key_actuation(cx, &app, KEY_E), desktop_actuation);

    // Close game with a single transient protocol failure on the first attempt.
    *fail_remaining.lock().unwrap() = 1;
    app.update(cx, |app, cx| app.handle_running_exes(&HashSet::new(), cx));
    settle(cx, handle);
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(1));
    assert!(cx.update(|cx| app.read(cx).banner().is_some()));

    // Next process poll automatically retries and reverts to Desktop without manual Refresh!
    app.update(cx, |app, cx| app.handle_running_exes(&HashSet::new(), cx));
    settle(cx, handle);
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(0));
    assert_eq!(key_actuation(cx, &app, KEY_E), desktop_actuation);
    assert!(cx.update(|cx| app.read(cx).banner().is_none()));
}

#[gpui_kit::test]
fn hardware_sync_detects_direct_keyboard_rgb_and_auto_saves_to_active_preset(
    cx: &mut TestAppContext,
) {
    use agnoperiph::app::HARDWARE_SYNC_INTERVAL;
    use agnoperiph::game_profiles::{AppConfig, GameProfile};
    use agnoperiph_core::Brightness;

    struct SharedMock(Arc<MockPeripheralService>);

    impl PeripheralService for SharedMock {
        fn is_dry_run(&self) -> bool {
            self.0.is_dry_run()
        }

        fn set_dry_run(&self, dry_run: bool) {
            self.0.set_dry_run(dry_run);
        }

        fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
            self.0.refresh()
        }

        fn apply(
            &self,
            device: &DeviceId,
            change: SettingChange,
        ) -> Result<DeviceSnapshot, ApplyError> {
            self.0.apply(device, change)
        }

        fn packet_log(&self) -> Vec<PacketTraceEntry> {
            self.0.packet_log()
        }

        fn clear_packet_log(&self) {
            self.0.clear_packet_log();
        }

        fn sync_hardware_state(&self, device: &DeviceId) -> CoreResult<Option<DeviceSnapshot>> {
            self.0.sync_hardware_state(device)
        }
    }

    let service = Arc::new(MockPeripheralService::new());
    service.set_dry_run(false);
    let mut config = AppConfig::default();
    let mut game = GameProfile::new("cs2", "CS2", "cs2.exe", r"C:\Games\cs2.exe");
    game.device_targets
        .insert(keyboard().as_str().to_owned(), 1);
    config.upsert_game_profile(game);
    config
        .default_targets
        .insert(keyboard().as_str().to_owned(), 0);
    let options = AppOptions::default()
        .with_config(config)
        .with_hardware_watcher(true);
    let (handle, app) = open_app_with(cx, SharedMock(service.clone()), options);

    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Lighting);

    // Simulate the user pressing Fn + RGB directly on the keyboard (preset 13 @ 55%).
    let onboard_effect = RgbEffect::Preset {
        id: 13,
        color: RgbColor::new(0xFF, 0x20, 0x40),
        speed: 140,
        colorful: false,
    };
    let onboard_brightness = Brightness::new(55).unwrap();
    service.simulate_hardware_change(&keyboard(), |snap| {
        let zone0 = &mut snap.rgb.as_mut().unwrap().snapshot.zones[0];
        zone0.effect = onboard_effect;
        zone0.brightness = onboard_brightness;
    });

    // Advance the background hardware watcher clock — no manual Refresh clicked.
    cx.executor().advance_clock(HARDWARE_SYNC_INTERVAL);
    settle(cx, handle);

    // 1. Live UI snapshot updated automatically.
    let live_zone0 = keyboard_snapshot(cx, &app).rgb.unwrap().snapshot.zones[0];
    assert_eq!(live_zone0.effect, onboard_effect);
    assert_eq!(live_zone0.brightness, onboard_brightness);

    // 2. Active Desktop software preset (slot 0) auto-saved the keyboard's new RGB.
    cx.update(|cx| {
        let saved_zone0 = &app.read(cx).config().software_profiles[keyboard().as_str()].slots[0]
            .saved
            .rgb_zones[0];
        assert_eq!(saved_zone0.effect, onboard_effect);
        assert_eq!(saved_zone0.brightness, onboard_brightness);
    });

    // 3. Launching a game and closing it restores the auto-saved onboard RGB on Desktop.
    let running = HashSet::from(["cs2.exe".to_owned()]);
    app.update(cx, |app, cx| app.handle_running_exes(&running, cx));
    settle(cx, handle);
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(1));
    // Give the game preset a different RGB so reverting actively writes RGB back.
    click(cx, handle, "rgb-0-preset-2");
    apply(cx, handle);
    assert_ne!(zone_effect(&keyboard_snapshot(cx, &app), 0), onboard_effect);

    app.update(cx, |app, cx| app.handle_running_exes(&HashSet::new(), cx));
    settle(cx, handle);
    assert_eq!(active_slot(cx, &app, &keyboard()), Some(0));
    assert_eq!(zone_effect(&keyboard_snapshot(cx, &app), 0), onboard_effect);

    // 4. While the user has unsaved edits in the draft bar, auto-sync defers.
    click(cx, handle, "rgb-0-preset-5");
    assert!(is_dirty(cx, &app));
    let other_effect = RgbEffect::Preset {
        id: 8,
        color: RgbColor::new(0x10, 0xFF, 0x20),
        speed: 90,
        colorful: false,
    };
    service.simulate_hardware_change(&keyboard(), |snap| {
        snap.rgb.as_mut().unwrap().snapshot.zones[0].effect = other_effect;
    });
    cx.executor().advance_clock(HARDWARE_SYNC_INTERVAL);
    settle(cx, handle);
    // Raw snapshot was NOT overwritten while the draft is dirty.
    assert_eq!(zone_effect(&keyboard_snapshot(cx, &app), 0), onboard_effect);
}

#[gpui_kit::test]
fn copy_diagnostics_writes_device_state_error_and_hid_trace_to_clipboard(cx: &mut TestAppContext) {
    let (handle, _app) = open_app(cx, MockPeripheralService::new());

    // Trigger a dry-run write so the packet log has real TX frames, then copy from the open inspector.
    click(cx, handle, ("polling-rate", 500u64));
    apply(cx, handle);
    click(cx, handle, "inspector-copy-diagnostics");

    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("diagnostic report must be copied to clipboard");
    assert!(copied.contains("## AgnoPeriph Diagnostic Report"));
    assert!(copied.contains("### Selected Device"));
    assert!(copied.contains("Lamzu Atlantis"));
    assert!(copied.contains("### Recent HID Packet Trace"));
    assert!(copied.contains("TX OUTPUT"));
    assert!(!copied.contains("SN-"));
}
