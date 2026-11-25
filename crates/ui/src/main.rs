//! AgnoPeriph desktop application entry point.
//!
//! Pass `--mock` to run against emulated devices instead of real HID hardware, and
//! `--minimized` to start in the notification area without opening the window.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// AGENTS.md requires importing GPUI through the Kit facade as `use gpui_kit::*;`.
#![allow(clippy::wildcard_imports)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph::backend::{LiveHidService, MockPeripheralService, PeripheralService};
use agnoperiph::game_profiles::AppConfig;
use agnoperiph::system::{MINIMIZED_FLAG, WindowsSystem};
use agnoperiph::{AgnoPeriphApp, AppOptions};
use gpui_kit::*;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// How often tray clicks and menu choices are picked up.
const TRAY_POLL_INTERVAL: Duration = Duration::from_millis(120);
const TRAY_ICON_SIZE: u32 = 32;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let use_mock = args.iter().any(|arg| arg == "--mock");
    let start_minimized = args.iter().any(|arg| arg == MINIMIZED_FLAG);
    let dev_tools = cfg!(debug_assertions) || args.iter().any(|arg| arg == "--dev-tools");
    let start_dry_run =
        cfg!(debug_assertions) || use_mock || args.iter().any(|arg| arg == "--dry-run");
    let service: Arc<dyn PeripheralService> = if use_mock {
        Arc::new(MockPeripheralService::new())
    } else {
        Arc::new(LiveHidService::new_with_dry_run(start_dry_run))
    };
    let config_path = AppConfig::default_path();
    let config = AppConfig::load_or_default(config_path.as_deref());
    let system = Arc::new(WindowsSystem::new(config_path));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            // The process outlives its window while it sits in the notification area.
            cx.set_quit_mode(QuitMode::Explicit);
            let options = AppOptions::live(system, config).with_dev_tools(dev_tools);
            let app = cx.new(|cx| AgnoPeriphApp::with_options(service, options, cx));
            let shell = Rc::new(RefCell::new(Shell {
                app,
                window: None,
                tray: Tray::new(),
            }));
            let has_tray = shell.borrow().tray.is_some();
            if !start_minimized || !has_tray {
                Shell::show(&shell, cx);
            }
            if has_tray {
                cx.spawn(async move |cx| {
                    loop {
                        cx.background_executor().timer(TRAY_POLL_INTERVAL).await;
                        let shell = shell.clone();
                        let () = cx.update(move |cx| Shell::poll_tray(&shell, cx));
                    }
                })
                .detach();
            }
        });
}

/// Owns the app state across window close / reopen cycles and the tray icon.
struct Shell {
    app: Entity<AgnoPeriphApp>,
    window: Option<AnyWindowHandle>,
    tray: Option<Tray>,
}

impl Shell {
    fn show(shell: &Rc<RefCell<Self>>, cx: &mut App) {
        let existing = shell.borrow().window;
        if let Some(handle) = existing
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            cx.activate(true);
            return;
        }
        let app = shell.borrow().app.clone();
        let has_tray = shell.borrow().tray.is_some();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(None, size(px(1240.), px(820.)), cx)),
            window_min_size: Some(size(px(960.), px(640.))),
            titlebar: Some(TitlebarOptions {
                title: Some("AgnoPeriph".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let close_shell = Rc::downgrade(shell);
        let opened = gpui_kit::open_window(options, cx, move |window, cx| {
            app.update(cx, |app, cx| app.attach_window(window, cx));
            let app_for_close = app.clone();
            window.on_window_should_close(cx, move |_, cx| {
                let keep_running = has_tray && app_for_close.read(cx).config().close_to_tray;
                if keep_running {
                    if let Some(shell) = close_shell.upgrade() {
                        shell.borrow_mut().window = None;
                    }
                } else {
                    cx.quit();
                }
                true
            });
            app
        });
        match opened {
            Ok((handle, _)) => {
                shell.borrow_mut().window = Some(handle);
                cx.activate(true);
            }
            Err(err) => {
                eprintln!("failed to open the AgnoPeriph window: {err}");
                cx.quit();
            }
        }
    }

    fn poll_tray(shell: &Rc<RefCell<Self>>, cx: &mut App) {
        let mut show = false;
        let mut quit = false;
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            match event {
                TrayIconEvent::DoubleClick { .. }
                | TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => show = true,
                _ => {}
            }
        }
        {
            let this = shell.borrow();
            let Some(tray) = this.tray.as_ref() else {
                return;
            };
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if event.id == *tray.show.id() {
                    show = true;
                } else if event.id == *tray.quit.id() {
                    quit = true;
                }
            }
            let status = match this.app.read(cx).active_game_name() {
                Some(name) => format!("Game Profile: {name}"),
                None => "Game Profile: Desktop / Default".to_owned(),
            };
            tray.set_status(&status);
        }
        if quit {
            cx.quit();
        } else if show {
            Self::show(shell, cx);
        }
    }
}

/// Notification-area icon with Show, the active Game Profile, and Quit.
struct Tray {
    _icon: TrayIcon,
    show: MenuItem,
    status: MenuItem,
    quit: MenuItem,
    status_text: RefCell<String>,
}

impl Tray {
    /// Creates the tray icon; `None` when the shell refuses it, so closing quits instead.
    fn new() -> Option<Self> {
        let show = MenuItem::new("Show AgnoPeriph", true, None);
        let status = MenuItem::new("Game Profile: Desktop / Default", false, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &status,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .ok()?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("AgnoPeriph")
            .with_icon(tray_image())
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()?;
        Some(Self {
            _icon: icon,
            show,
            status,
            quit,
            status_text: RefCell::new(String::new()),
        })
    }

    fn set_status(&self, text: &str) {
        let mut current = self.status_text.borrow_mut();
        if *current != text {
            self.status.set_text(text);
            text.clone_into(&mut current);
        }
    }
}

/// Embedded 32×32 RGBA tray icon compiled into the binary so standalone executables
/// always render the AgnoPeriph logo in the notification area.
fn tray_image() -> Icon {
    let rgba = include_bytes!("../assets/tray_icon_32x32.rgba").to_vec();
    Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE).expect("valid embedded tray icon")
}
