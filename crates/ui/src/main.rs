//! AgnoPeriph desktop application entry point.
//!
//! Pass `--mock` to run against emulated devices instead of real HID hardware.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// AGENTS.md requires importing GPUI through the Kit facade as `use gpui_kit::*;`.
#![allow(clippy::wildcard_imports)]

use std::sync::Arc;

use agnoperiph::AgnoPeriphApp;
use agnoperiph::backend::{LiveHidService, MockPeripheralService, PeripheralService};
use gpui_kit::*;

fn main() {
    let use_mock = std::env::args().any(|arg| arg == "--mock");
    let service: Arc<dyn PeripheralService> = if use_mock {
        Arc::new(MockPeripheralService::new())
    } else {
        Arc::new(LiveHidService::new())
    };

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(None, size(px(1240.), px(820.)), cx)),
                window_min_size: Some(size(px(960.), px(640.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("AgnoPeriph".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |_, cx| {
                cx.new(|cx| AgnoPeriphApp::new(service, cx))
            })
            .expect("failed to open the AgnoPeriph window");
            cx.activate(true);
        });
}
