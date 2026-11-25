//! UI integration tests: the production `AgnoPeriphApp` in a headless window, driven
//! by real pointer events against deterministic peripheral services.

use std::sync::{Arc, Mutex};

use agnoperiph::backend::{
    ApplyError, DeviceSnapshot, MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, MockPeripheralService,
    PeripheralService, SettingChange,
};
use agnoperiph::{AgnoPeriphApp, device_element_id};
use agnoperiph_core::{
    CoreError, CoreResult, DeviceId, KeyIndex, PollingRateHz, TravelHundredthsMm,
};
use agnoperiph_transport::{PacketOutcome, PacketTraceEntry};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Bounds, ElementId, Entity, Point, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};

/// W on the RS7 matrix (row 2, column 2): Rapid Trigger on, 0.80 mm actuation.
const KEY_W: u16 = 2 * 16 + 2;
/// Q on the RS7 matrix (row 2, column 1): Rapid Trigger off, 1.20 mm actuation.
const KEY_Q: u16 = 2 * 16 + 1;

fn open_app(
    cx: &mut TestAppContext,
    service: impl PeripheralService + 'static,
) -> (AnyWindowHandle, Entity<AgnoPeriphApp>) {
    cx.update(gpui_kit::init);
    let service: Arc<dyn PeripheralService> = Arc::new(service);
    let opened = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                // Tall enough that every editor control is on screen without scrolling.
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1440.), px(1600.)),
                ))),
                ..Default::default()
            },
            cx,
            |_, cx| cx.new(|cx| AgnoPeriphApp::new(service, cx)),
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
    assert!(exists(cx, handle, "rgb-0-effect-Static"));
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
    assert!(exists(cx, handle, "live-writes"));

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

    click(cx, handle, key_id(KEY_W));
    cx.update(|cx| assert_eq!(app.read(cx).selected_key(), KeyIndex::new(KEY_W)));

    click(cx, handle, "actuation-inc");
    click(cx, handle, "actuation-inc");
    click(cx, handle, "apply-actuation-key");
    cx.update(|cx| {
        let app = app.read(cx);
        let he = app.selected_device().unwrap().hall_effect.as_ref().unwrap();
        assert_eq!(he.key(KeyIndex::new(KEY_W)).unwrap().actuation, travel(84));
        assert_eq!(he.key(KeyIndex::new(KEY_Q)).unwrap().actuation, travel(120));
        assert_eq!(
            app.packets().last().unwrap().outcome,
            PacketOutcome::DryRunSuppressed
        );
    });

    // Enabling Rapid Trigger on a key that reports 0 mm clamps to the device minimum.
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
        assert_eq!(he.key(KeyIndex::new(KEY_W)).unwrap().actuation, travel(80));
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
        assert_eq!(he.key(KeyIndex::new(KEY_W)).unwrap().actuation, travel(122));
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
