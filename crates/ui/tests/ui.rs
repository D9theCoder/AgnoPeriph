//! UI integration tests: the production `AgnoPeriphApp` in a headless window, driven
//! by real pointer events against deterministic peripheral services.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agnoperiph::backend::{
    ApplyError, DeviceSnapshot, MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, MOCK_PULSAR_ID, MOCK_RAZER_ID,
    MOCK_VIA_ID, MOCK_WOOTING_ID, MockPeripheralService, PeripheralService, SettingChange,
};
use agnoperiph::game_profiles::{DetectedGame, GameSource};
use agnoperiph::{AgnoPeriphApp, AppOptions, KeyboardTab, Route, device_element_id};
use agnoperiph_core::{
    CoreError, CoreResult, DeviceId, KeyCode, KeyIndex, LiftOffDistance, MacroActionKind,
    ModTapBinding, PollingRateHz, ProfileSlot, RapidSwitchPair, RgbColor, RgbEffect,
    ToggleKeyBinding, TravelHundredthsMm,
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
    click(cx, handle, "apply-actuation-key");
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
    click(cx, handle, "apply-rapid-trigger");
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

    // Switching to live mode requires confirmation, and "Apply to all keys" also confirms
    // before erasing and rewriting the onboard flash actuation table.
    click(cx, handle, "dry-run");
    click(cx, handle, "ok");
    // Clicks accumulate, so start a fresh selection before picking Q alone.
    click(cx, handle, "clear-keys");
    click(cx, handle, key_id(KEY_Q));
    click(cx, handle, "actuation-inc");
    click(cx, handle, "apply-actuation-all");
    assert!(exists(cx, handle, "cancel"));
    click(cx, handle, "cancel");
    cx.update(|cx| {
        let he = app
            .read(cx)
            .selected_device()
            .unwrap()
            .hall_effect
            .as_ref()
            .unwrap();
        assert_eq!(he.key(KeyIndex::new(KEY_E)).unwrap().actuation, travel(120));
    });

    click(cx, handle, "apply-actuation-all");
    click(cx, handle, "ok");
    cx.update(|cx| {
        let he = app
            .read(cx)
            .selected_device()
            .unwrap()
            .hall_effect
            .as_ref()
            .unwrap();
        assert_eq!(he.key(KeyIndex::new(KEY_E)).unwrap().actuation, travel(122));
        assert_eq!(he.key(KeyIndex::new(KEY_Q)).unwrap().actuation, travel(122));
    });
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
    assert!(exists(cx, handle, "dpi-apply"));

    // Committing a polling rate must not discard the pending DPI edit.
    click(cx, handle, ("polling-rate", 500u64));
    assert!(exists(cx, handle, "dpi-apply"));

    click(cx, handle, "dpi-apply");
    assert!(!exists(cx, handle, "dpi-apply"));
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
    assert!(is_disabled(cx, handle, "apply-actuation-all"));
    assert!(is_disabled(cx, handle, "restore-default-keys"));

    click(cx, handle, key_id(KEY_E));
    click(cx, handle, key_id(KEY_Q));

    click(cx, handle, "restore-default-keys");
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

#[gpui_kit::test]
fn advanced_settings_write_keyboard_config(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, MockPeripheralService::new());
    click(cx, handle, device_element_id(&keyboard()));
    open_tab(cx, handle, KeyboardTab::Advanced);

    click(cx, handle, ("config-rate", 4000u64));
    click(cx, handle, "config-win-lock");
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
    click(cx, handle, "apply-actuation-key");
    let preset0_e = key_actuation(cx, &app, KEY_E);

    // Move to unconfigured Preset 1: SOCD and RS are cleared cleanly, and editing E auto-saves to Preset 1.
    click(cx, handle, ("preset", 1u64));
    let p1_he = keyboard_snapshot(cx, &app).hall_effect.unwrap().snapshot;
    assert_eq!(p1_he.socd_pairs, Vec::new());
    assert_eq!(p1_he.rapid_switch_pairs, Vec::new());
    click(cx, handle, "actuation-inc");
    click(cx, handle, "apply-actuation-key");
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
            window.find("apply-rapid-trigger").visible()
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
    let color = |cx: &mut TestAppContext| match zone_effect(&keyboard_snapshot(cx, &app), 0) {
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
        "apply-deadzones".into(),
        "deadzone-bottom-inc".into(),
        "release-sensitivity-inc".into(),
        "apply-rapid-trigger".into(),
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
}

impl RecordingSystem {
    fn new() -> Self {
        Self {
            notifications: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn take(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.notifications.lock().unwrap())
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
    click(cx, handle, "apply-actuation-key");
    cx.update(|cx| {
        let app = app.read(cx);
        let banner = app.banner().expect("unsupported write shows a banner");
        assert!(banner.message().contains("Nothing was written"));
        assert!(banner.message().contains("unsupported capability"));
        let he = app.device(&wooting).unwrap().hall_effect.as_ref().unwrap();
        assert_eq!(he.key(KeyIndex::new(14)).unwrap().actuation, travel(200));
    });
}
