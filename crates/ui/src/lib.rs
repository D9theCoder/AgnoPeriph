//! AgnoPeriph desktop frontend: a capability-driven peripheral editor built on GPUI Kit.
//!
//! [`AgnoPeriphApp`] is the window's root view. It talks to hardware only through a
//! [`backend::PeripheralService`], which the binary backs with real HID devices and
//! tests back with [`backend::MockPeripheralService`]. Host side effects (config file,
//! sign-in auto start) go through [`system::SystemIntegration`].

#![forbid(unsafe_code)]
// The project guidelines require importing GPUI through the Kit facade as `use gpui_kit::*;`.
#![allow(clippy::wildcard_imports)]
// Views are declarative GPUI element trees; splitting them by line count hurts readability.
#![allow(clippy::too_many_lines)]

pub mod app;
pub mod backend;
pub mod battery_alerts;
mod calibration;
mod controls;
mod dirty_bar;
pub mod draft;
pub mod game_profiles;
mod game_profiles_view;
mod hall_effect_panel;
pub mod icons;
mod inspector_drawer;
mod key_matrix;
pub mod key_tester;
mod keyboard_config_panel;
pub mod keycodes;
pub mod keymap;
mod keymap_panel;
mod macro_panel;
mod pointer_panel;
mod profile_bar;
pub mod profile_io;
pub mod quick_presets;
mod rgb_panel;
pub mod shortcuts;
pub mod sidebar;
mod smooth_scroll;
pub mod system;
pub mod tray_model;

pub use app::{AgnoPeriphApp, AppOptions, Banner, KeyboardTab, Route};
pub use hall_effect_panel::snap_travel;
pub use pointer_panel::snap_dpi;
pub use rgb_panel::{hex_to_rgb, hsv_to_rgb, rgb_to_hex, rgb_to_hsv};
pub use sidebar::device_element_id;

/// Locks a mutex, recovering the data if a panicking thread poisoned it.
pub(crate) fn backend_lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
