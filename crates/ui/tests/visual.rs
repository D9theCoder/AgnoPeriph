//! Visual snapshots: renders the production `AgnoPeriphApp` against the mock devices
//! through a headless GPU renderer and writes PNGs for a human or agent to review.
//!
//! Needs a GPU adapter (wgpu picks DX12/Vulkan on Windows), so it is ignored by default:
//!
//! ```powershell
//! cargo test -p agnoperiph --test visual -- --ignored --nocapture
//! # Or filter to specific views (comma-separated substrings, case-insensitive):
//! $env:AGNO_VISUAL = "triggers,mouse-sensor"; cargo test -p agnoperiph --test visual -- --ignored --nocapture
//! # Save/update baselines in target/ui-screenshots-baseline/ for pixel-diff reporting:
//! $env:AGNO_VISUAL_UPDATE_BASELINE = "1"; cargo test -p agnoperiph --test visual -- --ignored --nocapture
//! ```
//!
//! Images land in `target/ui-screenshots/`. Never touches HID hardware: only
//! `MockPeripheralService` is used.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use agnoperiph::backend::{
    MOCK_KEYBOARD_ID, MOCK_PULSAR_ID, MOCK_RAZER_ID, MOCK_VIA_ID, MOCK_WOOTING_ID,
    MockPeripheralService, PeripheralService,
};
use agnoperiph::key_tester::LIVE_POLL_INTERVAL;
use agnoperiph::{AgnoPeriphApp, AppOptions, KeyboardTab, device_element_id};
use agnoperiph_core::{DeviceId, KeyIndex, KeyTravelSample, TravelHundredthsMm};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Bounds, DevicePixels, ElementId, Entity, Font, FontId,
    FontMetrics, FontRun, GlyphId, HeadlessAppContext, Hsla, LineLayout, Pixels,
    PlatformTextSystem, RenderGlyphParams, Size, TextRenderingMode, px, size,
};

const WIDTH: f32 = 1240.;
const HEIGHT: f32 = 820.;
/// Taller window for mouse views, so every sensor row below the DPI table is on screen.
const TALL_HEIGHT: f32 = 1100.;

fn screenshot_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&dir).expect("create screenshot dir");
    dir.canonicalize().expect("resolve screenshot dir")
}

fn baseline_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots-baseline")
}

/// Returns `true` when `name` matches `AGNO_VISUAL` (comma-separated case-insensitive substrings),
/// or when `AGNO_VISUAL` is unset/empty.
fn matches_filter(name: &str) -> bool {
    let Ok(raw) = std::env::var("AGNO_VISUAL") else {
        return true;
    };
    let filters: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if filters.is_empty() {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    filters.iter().any(|f| lower.contains(f))
}

fn compare_with_baseline(name: &str, current: &image::RgbaImage) {
    let update = std::env::var("AGNO_VISUAL_UPDATE_BASELINE")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let base_dir = baseline_dir();
    let base_path = base_dir.join(format!("{name}.png"));

    if update {
        std::fs::create_dir_all(&base_dir).expect("create baseline dir");
        current.save(&base_path).expect("write baseline png");
        println!("  baseline updated: {}", base_path.display());
        return;
    }

    let Ok(baseline_img) = image::open(&base_path) else {
        return;
    };
    let baseline = baseline_img.to_rgba8();
    if baseline.dimensions() != current.dimensions() {
        println!(
            "  diff: DIMENSION MISMATCH (baseline {:?} vs current {:?})",
            baseline.dimensions(),
            current.dimensions()
        );
        return;
    }

    let total_pixels = current.width().saturating_mul(current.height());
    let mut changed_pixels = 0u32;
    for (a, b) in baseline.pixels().zip(current.pixels()) {
        let differs =
            a.0.iter()
                .zip(b.0.iter())
                .any(|(ca, cb)| ca.abs_diff(*cb) > 2);
        if differs {
            changed_pixels = changed_pixels.saturating_add(1);
        }
    }
    let pct = if total_pixels == 0 {
        0.0
    } else {
        (f64::from(changed_pixels) / f64::from(total_pixels)) * 100.0
    };
    println!("  diff vs baseline: {pct:.3}% ({changed_pixels}/{total_pixels} px)");

    if let Ok(max_str) = std::env::var("AGNO_VISUAL_MAX_DIFF")
        && let Ok(max_pct) = max_str.parse::<f64>()
    {
        assert!(
            pct <= max_pct,
            "visual regression on '{name}': {pct:.3}% changed pixels exceeds AGNO_VISUAL_MAX_DIFF={max_pct:.3}%"
        );
    }
}

/// The Windows DirectWrite text system, forced to grayscale glyphs so snapshots don't
/// depend on the machine's ClearType setting.
struct GrayscaleText(Arc<dyn PlatformTextSystem>);

impl PlatformTextSystem for GrayscaleText {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        self.0.add_fonts(fonts)
    }
    fn all_font_names(&self) -> Vec<String> {
        self.0.all_font_names()
    }
    fn font_id(&self, descriptor: &Font) -> anyhow::Result<FontId> {
        self.0.font_id(descriptor)
    }
    fn prewarm_fonts(&self, font_ids: &[FontId]) {
        self.0.prewarm_fonts(font_ids);
    }
    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.0.font_metrics(font_id)
    }
    fn typographic_bounds(
        &self,
        font_id: FontId,
        glyph_id: GlyphId,
    ) -> anyhow::Result<Bounds<f32>> {
        self.0.typographic_bounds(font_id, glyph_id)
    }
    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> anyhow::Result<Size<f32>> {
        self.0.advance(font_id, glyph_id)
    }
    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.glyph_for_char(font_id, ch)
    }
    fn glyph_raster_bounds(
        &self,
        params: &RenderGlyphParams,
    ) -> anyhow::Result<Bounds<DevicePixels>> {
        self.0.glyph_raster_bounds(params)
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> anyhow::Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.rasterize_glyph(params, raster_bounds)
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.0.layout_line(text, font_size, runs)
    }
    fn recommended_rendering_mode(&self, _: FontId, _: Pixels) -> TextRenderingMode {
        TextRenderingMode::Grayscale
    }
    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.0.glyph_dilation_for_color(color)
    }
}

fn headless() -> HeadlessAppContext {
    // `current_platform(true)` (headless) hands back a no-op text system on Windows that
    // lays text out but rasterizes every glyph empty, so take the real DirectWrite one.
    let text_system: Arc<dyn PlatformTextSystem> = Arc::new(GrayscaleText(
        gpui_kit::platform::current_platform(false).text_system(),
    ));
    HeadlessAppContext::with_platform(text_system, Arc::new(agnoperiph::icons::AppAssets), || {
        gpui_wgpu::WgpuHeadlessRenderer::new()
            .map(|r| Some(Box::new(r) as Box<dyn gpui_kit::PlatformHeadlessRenderer>))
    })
}

fn open(cx: &mut HeadlessAppContext) -> (AnyWindowHandle, Entity<AgnoPeriphApp>) {
    open_with(cx, MockPeripheralService::new(), HEIGHT)
}

fn open_with(
    cx: &mut HeadlessAppContext,
    service: MockPeripheralService,
    height: f32,
) -> (AnyWindowHandle, Entity<AgnoPeriphApp>) {
    open_with_options(cx, service, height, AppOptions::default())
}

fn open_with_options(
    cx: &mut HeadlessAppContext,
    service: MockPeripheralService,
    height: f32,
    options: AppOptions,
) -> (AnyWindowHandle, Entity<AgnoPeriphApp>) {
    cx.update(gpui_kit::init);
    let service: Arc<dyn PeripheralService> = Arc::new(service);
    // Mount `Root` as `gpui_kit::open_window` does, so dialogs and popovers have their layers.
    let mut built = None;
    let window = cx
        .open_window(size(px(WIDTH), px(height)), |window, cx| {
            let view = AgnoPeriphApp::build(service, options.with_dev_tools(false), window, cx);
            built = Some(view.clone());
            cx.new(|cx| gpui_kit::base::Root::new(view, window, cx))
        })
        .expect("open headless window");
    let app = built.expect("app root");
    settle(cx, window.into());
    (window.into(), app)
}

fn settle(cx: &mut HeadlessAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("render frame");
}

fn click(cx: &mut HeadlessAppContext, handle: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(handle, |_, window, cx| window.click(id, cx))
        .expect("click");
    settle(cx, handle);
}

fn open_tab(cx: &mut HeadlessAppContext, handle: AnyWindowHandle, tab: KeyboardTab) {
    let index = KeyboardTab::ALL.iter().position(|t| *t == tab).unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.within("keyboard-tabs").click(index, cx);
    })
    .expect("open tab");
    settle(cx, handle);
}

fn set_theme(cx: &mut HeadlessAppContext, handle: AnyWindowHandle, mode: ThemeMode) {
    cx.update_window(handle, |_, window, cx| {
        Theme::change(mode, Some(window), cx);
    })
    .expect("theme");
    settle(cx, handle);
}

fn save(cx: &mut HeadlessAppContext, handle: AnyWindowHandle, name: &str) {
    if !matches_filter(name) {
        return;
    }
    // Let dialog and popover enter animations finish.
    cx.advance_clock(std::time::Duration::from_millis(600));
    settle(cx, handle);
    let image = cx.capture_screenshot(handle).expect("capture screenshot");
    let path = screenshot_dir().join(format!("{name}.png"));
    image.save(&path).expect("write png");
    println!("screenshot: {}", path.display());
    compare_with_baseline(name, &image);
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored to write PNGs"]
fn capture_keyboard_screens() {
    let mut cx = headless();
    let (handle, _app) = open(&mut cx);
    click(
        &mut cx,
        handle,
        device_element_id(&DeviceId::new(MOCK_KEYBOARD_ID)),
    );

    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        set_theme(&mut cx, handle, mode);
        open_tab(&mut cx, handle, KeyboardTab::Triggers);
        save(&mut cx, handle, &format!("triggers-{suffix}"));
        open_tab(&mut cx, handle, KeyboardTab::Lighting);
        save(&mut cx, handle, &format!("lighting-{suffix}"));
        open_tab(&mut cx, handle, KeyboardTab::Keybinds);
        save(&mut cx, handle, &format!("keybinds-{suffix}"));
    }
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored to write PNGs"]
fn capture_multi_brand_screens() {
    let mut cx = headless();
    let (handle, app) = open_with(&mut cx, MockPeripheralService::multi_brand(), HEIGHT);
    let (tall, _) = open_with(&mut cx, MockPeripheralService::multi_brand(), TALL_HEIGHT);

    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        set_theme(&mut cx, tall, mode);
        click(
            &mut cx,
            tall,
            device_element_id(&DeviceId::new(MOCK_PULSAR_ID)),
        );
        save(&mut cx, tall, &format!("mouse-sensor-{suffix}"));
        click(
            &mut cx,
            tall,
            device_element_id(&DeviceId::new(MOCK_RAZER_ID)),
        );
        save(&mut cx, tall, &format!("mouse-rgb-{suffix}"));

        set_theme(&mut cx, handle, mode);
        click(
            &mut cx,
            handle,
            device_element_id(&DeviceId::new(MOCK_VIA_ID)),
        );
        save(&mut cx, handle, &format!("via-keybinds-{suffix}"));
        cx.update(|cx| {
            app.update(cx, |app, cx| {
                app.set_keyboard_tab(KeyboardTab::Lighting, cx);
            });
        });
        save(&mut cx, handle, &format!("via-lighting-{suffix}"));
        cx.update(|cx| {
            app.update(cx, |app, cx| {
                app.set_keyboard_tab(KeyboardTab::Keybinds, cx);
            });
        });

        // Own window, so the expected error banner stays out of the other views.
        let (wooting, _) = open_with(&mut cx, MockPeripheralService::multi_brand(), HEIGHT);
        set_theme(&mut cx, wooting, mode);
        click(
            &mut cx,
            wooting,
            device_element_id(&DeviceId::new(MOCK_WOOTING_ID)),
        );
        save(&mut cx, wooting, &format!("wooting-triggers-{suffix}"));
        click(&mut cx, wooting, ("key", 14u64));
        click(&mut cx, wooting, "actuation-inc");
        click(&mut cx, wooting, "dirty-apply");
        save(&mut cx, wooting, &format!("wooting-unsupported-{suffix}"));
    }
}

/// Opens the mock keyboard on its Triggers tab with E selected and two unsaved edits.
fn dirty_triggers(
    cx: &mut HeadlessAppContext,
    mode: ThemeMode,
) -> (AnyWindowHandle, Entity<AgnoPeriphApp>) {
    let (handle, app) = open(cx);
    set_theme(cx, handle, mode);
    click(
        cx,
        handle,
        device_element_id(&DeviceId::new(MOCK_KEYBOARD_ID)),
    );
    open_tab(cx, handle, KeyboardTab::Triggers);
    click(cx, handle, ("key", 35u64));
    click(cx, handle, "actuation-inc");
    click(cx, handle, "actuation-inc");
    click(cx, handle, "rapid-trigger");
    open_tab(cx, handle, KeyboardTab::Advanced);
    click(cx, handle, "config-win-lock");
    open_tab(cx, handle, KeyboardTab::Triggers);
    (handle, app)
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored to write PNGs"]
fn capture_unsaved_changes_screens() {
    let mut cx = headless();
    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        let (handle, _) = dirty_triggers(&mut cx, mode);
        save(&mut cx, handle, &format!("dirty-triggers-{suffix}"));
        click(&mut cx, handle, "dirty-changes");
        save(&mut cx, handle, &format!("dirty-changes-{suffix}"));
        click(&mut cx, handle, "dirty-changes");
        open_tab(&mut cx, handle, KeyboardTab::Advanced);
        save(&mut cx, handle, &format!("advanced-dirty-{suffix}"));
        click(&mut cx, handle, "dirty-discard");
        save(&mut cx, handle, &format!("discard-toast-{suffix}"));

        // Leaving the device with unsaved edits asks first.
        let (handle, _) = dirty_triggers(&mut cx, mode);
        click(
            &mut cx,
            handle,
            device_element_id(&DeviceId::new(agnoperiph::backend::MOCK_MOUSE_ID)),
        );
        save(&mut cx, handle, &format!("unsaved-guard-{suffix}"));

        let (tall, _) = open_with(&mut cx, MockPeripheralService::new(), TALL_HEIGHT);
        set_theme(&mut cx, tall, mode);
        click(&mut cx, tall, ("dpi-inc", 1u64));
        click(&mut cx, tall, ("polling-rate", 500u64));
        save(&mut cx, tall, &format!("mouse-dirty-{suffix}"));
    }
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored to write PNGs"]
fn capture_profile_file_and_shortcut_screens() {
    let mut cx = headless();
    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        let (handle, app) = open(&mut cx);
        set_theme(&mut cx, handle, mode);
        click(
            &mut cx,
            handle,
            device_element_id(&DeviceId::new(MOCK_KEYBOARD_ID)),
        );
        let (_, json) = cx
            .update(|cx| app.read(cx).export_profile_json())
            .expect("keyboard exports");
        cx.update_window(handle, |_, window, cx| {
            app.update(cx, |app, cx| {
                app.open_import_preview("RS7 FPS.json", &json, window, cx);
            });
        })
        .unwrap();
        save(&mut cx, handle, &format!("import-preview-{suffix}"));

        let (handle, app) = open(&mut cx);
        set_theme(&mut cx, handle, mode);
        click(
            &mut cx,
            handle,
            device_element_id(&DeviceId::new(MOCK_KEYBOARD_ID)),
        );
        cx.update_window(handle, |_, window, cx| {
            app.update(cx, |app, cx| {
                app.open_import_preview("notes.json", "{ not json", window, cx);
            });
        })
        .unwrap();
        save(&mut cx, handle, &format!("import-error-{suffix}"));

        let (handle, _) = open(&mut cx);
        set_theme(&mut cx, handle, mode);
        click(&mut cx, handle, "shortcuts-toggle");
        save(&mut cx, handle, &format!("shortcuts-{suffix}"));

        let (handle, _) = open(&mut cx);
        set_theme(&mut cx, handle, mode);
        click(
            &mut cx,
            handle,
            device_element_id(&DeviceId::new(MOCK_KEYBOARD_ID)),
        );
        click(&mut cx, handle, "quick-presets");
        save(&mut cx, handle, &format!("quick-presets-menu-{suffix}"));
        let (handle, _) = open(&mut cx);
        set_theme(&mut cx, handle, mode);
        click(
            &mut cx,
            handle,
            device_element_id(&DeviceId::new(MOCK_KEYBOARD_ID)),
        );
        click(&mut cx, handle, "profile-file-menu");
        save(&mut cx, handle, &format!("profile-file-menu-{suffix}"));
    }
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored to write PNGs"]
fn capture_live_tester_screens() {
    let mut cx = headless();
    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        let service = MockPeripheralService::new();
        let keyboard = DeviceId::new(MOCK_KEYBOARD_ID);
        service.set_live_key_travel(
            &keyboard,
            vec![KeyTravelSample::new(
                KeyIndex::new(35),
                TravelHundredthsMm::from_hundredths(180),
            )],
        );
        let (handle, _app) = open_with(&mut cx, service, HEIGHT);
        set_theme(&mut cx, handle, mode);
        click(&mut cx, handle, device_element_id(&keyboard));
        open_tab(&mut cx, handle, KeyboardTab::Triggers);
        // Rapid Trigger on E shows the release/press band on the meter.
        click(&mut cx, handle, ("key", 35u64));
        click(&mut cx, handle, "rapid-trigger");
        for _ in 0..5 {
            click(&mut cx, handle, "press-sensitivity-inc");
            click(&mut cx, handle, "release-sensitivity-inc");
        }
        for _ in 0..6 {
            cx.advance_clock(LIVE_POLL_INTERVAL);
            settle(&mut cx, handle);
        }
        save(&mut cx, handle, &format!("live-tester-{suffix}"));
    }
}

struct FixedReleases(Option<agnoperiph::updates::UpdateInfo>);

impl agnoperiph::updates::ReleaseSource for FixedReleases {
    fn latest(&self) -> Result<Option<agnoperiph::updates::UpdateInfo>, String> {
        Ok(self.0.clone())
    }
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored to write PNGs"]
fn capture_update_screen() {
    let mut cx = headless();
    let releases = Arc::new(FixedReleases(Some(agnoperiph::updates::UpdateInfo::new(
        "9.9.9",
        "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v9.9.9",
    ))));
    let options = AppOptions::default()
        .with_update_check(true)
        .with_release_source(releases);
    let (handle, _) = open_with_options(&mut cx, MockPeripheralService::new(), HEIGHT, options);
    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        set_theme(&mut cx, handle, mode);
        save(&mut cx, handle, &format!("update-{suffix}"));
    }
}
