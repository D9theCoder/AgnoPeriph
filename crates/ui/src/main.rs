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

use agnoperiph::backend::{LiveHidService, MockPeripheralService, PeripheralService};
use agnoperiph::game_profiles::AppConfig;
use agnoperiph::system::{MINIMIZED_FLAG, WindowsSystem};
use agnoperiph::tray_model::{TrayChanged, TrayModel};
use agnoperiph::{AgnoPeriphApp, AppOptions};
use agnoperiph_core::DeviceId;
use gpui_kit::*;
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TRAY_ICON_SIZE: u32 = 32;

enum TrayAction {
    Show,
    Menu(MenuEvent),
}

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

    let application = gpui_kit::application().with_assets(agnoperiph::icons::AppAssets);
    let application = if start_minimized {
        // Initialize DirectX only on the first open. Recreating it on each reopen
        // can accumulate allocations inside the Windows graphics stack.
        application.with_windowing(WindowingRequest::Headless)
    } else {
        application
    };
    application.run(move |cx| {
        gpui_kit::init(cx);
        // The process outlives its window while it sits in the notification area.
        cx.set_quit_mode(QuitMode::Explicit);
        let options = AppOptions::live(system, config).with_dev_tools(dev_tools);
        let app = cx.new(|cx| AgnoPeriphApp::with_options(service, options, cx));
        let shell = Rc::new(RefCell::new(Shell {
            app,
            window: None,
            opening: false,
            pending_profile: None,
            tray: Tray::new(),
        }));
        let has_tray = shell.borrow().tray.is_some();
        if !start_minimized || !has_tray {
            Shell::show(&shell, cx);
        }
        if has_tray {
            Shell::listen_to_tray(shell, cx);
        }
    });
}

/// Owns the app state across window close / reopen cycles and the tray icon.
struct Shell {
    app: Entity<AgnoPeriphApp>,
    window: Option<AnyWindowHandle>,
    opening: bool,
    pending_profile: Option<(DeviceId, u8)>,
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
            Self::apply_pending_profile(shell, handle, cx);
            return;
        }
        if shell.borrow().opening {
            return;
        }
        if cx.graphical_environment().is_none() {
            shell.borrow_mut().opening = true;
            let attach =
                cx.request_windowing(WindowingRequest::Windowed(GraphicalEnvironment::detect()));
            let shell = shell.clone();
            cx.spawn(async move |cx| {
                let result = attach.await;
                cx.update(|cx| {
                    shell.borrow_mut().opening = false;
                    match result {
                        Ok(()) => Self::open(&shell, cx),
                        Err(err) => {
                            eprintln!("failed to initialize AgnoPeriph graphics: {err}");
                            let mut shell = shell.borrow_mut();
                            shell.pending_profile = None;
                            if shell.tray.is_none() {
                                cx.quit();
                            }
                        }
                    }
                });
            })
            .detach();
        } else {
            Self::open(shell, cx);
        }
    }

    fn open(shell: &Rc<RefCell<Self>>, cx: &mut App) {
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
                Self::apply_pending_profile(shell, handle, cx);
            }
            Err(err) => {
                eprintln!("failed to open the AgnoPeriph window: {err}");
                cx.quit();
            }
        }
    }

    fn listen_to_tray(shell: Rc<RefCell<Self>>, cx: &mut App) {
        let app = shell.borrow().app.clone();
        let weak_shell = Rc::downgrade(&shell);
        cx.subscribe(&app, move |_, _: &TrayChanged, cx| {
            if let Some(shell) = weak_shell.upgrade() {
                shell.borrow_mut().sync_tray(cx);
            }
        })
        .detach();
        shell.borrow_mut().sync_tray(cx);

        let (sender, receiver) = async_channel::unbounded();
        let icon_sender = sender.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick { .. }
            ) {
                let _ = icon_sender.try_send(TrayAction::Show);
            }
        }));
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = sender.try_send(TrayAction::Menu(event));
        }));
        cx.spawn(async move |cx| {
            while let Ok(action) = receiver.recv().await {
                cx.update(|cx| Self::handle_tray_action(&shell, action, cx));
            }
        })
        .detach();
    }

    fn sync_tray(&mut self, cx: &App) {
        if let Some(tray) = &mut self.tray {
            let app = self.app.read(cx);
            tray.sync(app.tray_model(), app.config().low_battery_alerts);
        }
    }

    fn handle_tray_action(shell: &Rc<RefCell<Self>>, action: TrayAction, cx: &mut App) {
        let TrayAction::Menu(event) = action else {
            Self::show(shell, cx);
            return;
        };
        let this = shell.borrow();
        let Some(tray) = this.tray.as_ref() else {
            return;
        };
        if event.id == *tray.quit.id() {
            cx.quit();
        } else if event.id == *tray.show.id() {
            drop(this);
            Self::show(shell, cx);
        } else if event.id == *tray.low_battery.id() {
            let app = this.app.clone();
            drop(this);
            app.update(cx, |app, cx| {
                let enabled = !app.config().low_battery_alerts;
                app.set_low_battery_alerts(enabled, cx);
            });
        } else if let Some((device, slot)) = tray.slot_for(&event.id) {
            drop(this);
            shell.borrow_mut().pending_profile = Some((device, slot));
            Self::show(shell, cx);
        }
    }

    fn apply_pending_profile(shell: &Rc<RefCell<Self>>, handle: AnyWindowHandle, cx: &mut App) {
        let (app, pending) = {
            let mut shell = shell.borrow_mut();
            (shell.app.clone(), shell.pending_profile.take())
        };
        // Unsaved edits on that device are confirmed in the window.
        if let Some((device, slot)) = pending {
            let _ = handle.update(cx, |_, window, cx| {
                app.update(cx, |app, cx| {
                    app.request_tray_profile_switch(&device, slot, window, cx);
                });
            });
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
    fn sync(&mut self, model: TrayModel, alerts_on: bool) {
        if model == self.shown && self.low_battery.is_checked() == alerts_on {
            return;
        }
        let needs_rebuild =
            !model.has_same_structure(&self.shown) || self.devices.len() != model.devices.len();
        if needs_rebuild && self.rebuild(&model).is_none() {
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
        self.shown = model;
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
