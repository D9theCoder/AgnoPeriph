//! Visual snapshots: renders the production `AgnoPeriphApp` against the mock devices
//! through a headless GPU renderer and writes PNGs for a human or agent to review.
//!
//! Needs a GPU adapter (wgpu picks DX12/Vulkan on Windows), so it is ignored by default:
//!
//! ```powershell
//! cargo test -p agnoperiph --test visual -- --ignored --nocapture
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
use agnoperiph::{AgnoPeriphApp, AppOptions, KeyboardTab, device_element_id};
use agnoperiph_core::DeviceId;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, Bounds, DevicePixels, ElementId, Entity, Font, FontId, FontMetrics, FontRun,
    GlyphId, HeadlessAppContext, Hsla, LineLayout, Pixels, PlatformTextSystem, RenderGlyphParams,
    Size, TextRenderingMode, px, size,
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
    cx.update(gpui_kit::init);
    let service: Arc<dyn PeripheralService> = Arc::new(service);
    let window = cx
        .open_window(size(px(WIDTH), px(height)), |window, cx| {
            AgnoPeriphApp::build(
                service,
                AppOptions::default().with_dev_tools(false),
                window,
                cx,
            )
        })
        .expect("open headless window");
    let app = cx
        .update_window(window.into(), |root, _, _| root.downcast::<AgnoPeriphApp>())
        .expect("window")
        .expect("app root");
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
    settle(cx, handle);
    let image = cx.capture_screenshot(handle).expect("capture screenshot");
    let path = screenshot_dir().join(format!("{name}.png"));
    image.save(&path).expect("write png");
    println!("screenshot: {}", path.display());
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
        click(&mut cx, wooting, "apply-actuation-key");
        save(&mut cx, wooting, &format!("wooting-unsupported-{suffix}"));
    }
}
