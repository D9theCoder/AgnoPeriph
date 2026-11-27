//! AgnoPeriph desktop application entry point.
//!
//! Pass `--mock` to run against emulated devices instead of real HID hardware, and
//! `--minimized` to start in the notification area without opening the window.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The project guidelines require importing GPUI through the Kit facade as `use gpui_kit::*;`.
#![allow(clippy::wildcard_imports)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph::backend::{LiveHidService, MockPeripheralService, PeripheralService};
use agnoperiph::game_profiles::AppConfig;
use agnoperiph::system::{MINIMIZED_FLAG, WindowsSystem};
use agnoperiph::tray_model::TrayModel;
use agnoperiph::{AgnoPeriphApp, AppOptions};
use agnoperiph_core::DeviceId;
use gpui_kit::*;
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
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
        .with_assets(agnoperiph::icons::AppAssets)
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
                ..gpui_kit::component::TitleBar::title_bar_options()
            }),
            ..gpui_kit::component::TitleBar::window_options()
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
                    app_for_close.update(cx, AgnoPeriphApp::detach_window);
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
        let mut toggle_alerts = false;
        let mut switch: Option<(DeviceId, u8)> = None;
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
            let mut this = shell.borrow_mut();
            let model = this.app.read(cx).tray_model();
            let alerts_on = this.app.read(cx).config().low_battery_alerts;
            let Some(tray) = this.tray.as_mut() else {
                return;
            };
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if event.id == *tray.show.id() {
                    show = true;
                } else if event.id == *tray.quit.id() {
                    quit = true;
                } else if event.id == *tray.low_battery.id() {
                    toggle_alerts = true;
                } else if let Some(target) = tray.slot_for(&event.id) {
                    switch = Some(target);
                }
            }
            tray.sync(&model, alerts_on);
        }
        if quit {
            cx.quit();
            return;
        }
        let app = shell.borrow().app.clone();
        if toggle_alerts {
            app.update(cx, |app, cx| {
                let enabled = !app.config().low_battery_alerts;
                app.set_low_battery_alerts(enabled, cx);
            });
        }
        if let Some((device, slot)) = switch {
            // Unsaved edits on that device are confirmed in the window, so open it first.
            Self::show(shell, cx);
            let handle = shell.borrow().window;
            if let Some(handle) = handle {
                let _ = handle.update(cx, |_, window, cx| {
                    app.update(cx, |app, cx| {
                        app.request_tray_profile_switch(&device, slot, window, cx);
                    });
                });
            }
        } else if show {
            Self::show(shell, cx);
        }
    }
}

/// Escapes `&`, which Win32 menus read as a mnemonic marker.
fn menu_text(text: &str) -> String {
    text.replace('&', "&&")
}

/// One device's submenu with a checkable item per profile slot.
struct TrayDeviceMenu {
    id: DeviceId,
    submenu: Submenu,
    slots: Vec<(u8, CheckMenuItem)>,
}

/// Notification-area icon: Show, one submenu per device with its battery and profiles,
/// the active Game Profile, the low-battery alert switch, and Quit.
struct Tray {
    icon: TrayIcon,
    show: MenuItem,
    status: MenuItem,
    low_battery: CheckMenuItem,
    quit: MenuItem,
    devices: Vec<TrayDeviceMenu>,
    /// The model the current menu was built from; labels update in place, and a new
    /// structure rebuilds the menu.
    shown: TrayModel,
    tooltip: String,
}

impl Tray {
    /// Creates the tray icon; `None` when the shell refuses it, so closing quits instead.
    fn new() -> Option<Self> {
        let show = MenuItem::new("Open AgnoPeriph", true, None);
        let status = MenuItem::new("Game Profile: Desktop / Default", false, None);
        let low_battery = CheckMenuItem::new("Low-battery alerts", true, true, None);
        let quit = MenuItem::new("Quit", true, None);
        let icon = TrayIconBuilder::new()
            .with_tooltip("AgnoPeriph")
            .with_icon(tray_image())
            .with_menu_on_left_click(false)
            .build()
            .ok()?;
        let mut tray = Self {
            icon,
            show,
            status,
            low_battery,
            quit,
            devices: Vec::new(),
            shown: TrayModel::default(),
            tooltip: String::new(),
        };
        tray.rebuild(&TrayModel::default())?;
        Some(tray)
    }

    fn rebuild(&mut self, model: &TrayModel) -> Option<()> {
        let menu = Menu::new();
        menu.append_items(&[&self.show, &PredefinedMenuItem::separator()])
            .ok()?;
        self.devices.clear();
        for device in &model.devices {
            let submenu = Submenu::new(menu_text(&device.label()), !device.slots.is_empty());
            let slots: Vec<(u8, CheckMenuItem)> = device
                .slots
                .iter()
                .map(|(slot, name)| {
                    let item = CheckMenuItem::new(
                        menu_text(name),
                        true,
                        device.active_slot == Some(*slot),
                        None,
                    );
                    let _ = submenu.append(&item);
                    (*slot, item)
                })
                .collect();
            menu.append(&submenu).ok()?;
            self.devices.push(TrayDeviceMenu {
                id: device.id.clone(),
                submenu,
                slots,
            });
        }
        if !model.devices.is_empty() {
            menu.append(&PredefinedMenuItem::separator()).ok()?;
        }
        menu.append_items(&[
            &self.status,
            &PredefinedMenuItem::separator(),
            &self.low_battery,
            &self.quit,
        ])
        .ok()?;
        self.icon.set_menu(Some(Box::new(menu)));
        self.shown = TrayModel::default();
        Some(())
    }

    /// Brings the menu, checks and tooltip in line with `model`.
    fn sync(&mut self, model: &TrayModel, alerts_on: bool) {
        let needs_rebuild = model.structure_key() != self.shown.structure_key()
            || self.devices.len() != model.devices.len();
        if needs_rebuild && self.rebuild(model).is_none() {
            return;
        }
        if *model == self.shown && self.low_battery.is_checked() == alerts_on {
            return;
        }
        for (menu, device) in self.devices.iter().zip(&model.devices) {
            menu.submenu.set_text(menu_text(&device.label()));
            for ((slot, item), (_, name)) in menu.slots.iter().zip(&device.slots) {
                item.set_text(menu_text(name));
                item.set_checked(device.active_slot == Some(*slot));
            }
        }
        self.status.set_text(menu_text(&model.status));
        self.low_battery.set_checked(alerts_on);
        let tooltip = model.tooltip();
        if tooltip != self.tooltip {
            let _ = self.icon.set_tooltip(Some(&tooltip));
            self.tooltip = tooltip;
        }
        self.shown = model.clone();
    }

    /// The device and slot a profile item stands for.
    fn slot_for(&self, id: &MenuId) -> Option<(DeviceId, u8)> {
        self.devices.iter().find_map(|device| {
            device
                .slots
                .iter()
                .find(|(_, item)| item.id() == id)
                .map(|(slot, _)| (device.id.clone(), *slot))
        })
    }
}

/// Embedded 32×32 RGBA tray icon compiled into the binary so standalone executables
/// always render the AgnoPeriph logo in the notification area.
fn tray_image() -> Icon {
    let rgba = include_bytes!("../assets/tray_icon_32x32.rgba").to_vec();
    Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE).expect("valid embedded tray icon")
}
