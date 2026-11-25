//! AgnoPeriph desktop frontend: a capability-driven peripheral editor built on GPUI Kit.
//!
//! [`AgnoPeriphApp`] is the window's root view. It talks to hardware only through a
//! [`backend::PeripheralService`], which the binary backs with real HID devices and
//! tests back with [`backend::MockPeripheralService`].

#![forbid(unsafe_code)]
// AGENTS.md requires importing GPUI through the Kit facade as `use gpui_kit::*;`.
#![allow(clippy::wildcard_imports)]

pub mod app;
pub mod backend;
mod hall_effect_panel;
mod inspector_drawer;
pub mod keymap;
mod pointer_panel;
mod rgb_panel;
pub mod sidebar;

pub use app::{AgnoPeriphApp, Banner};
pub use hall_effect_panel::snap_travel;
pub use pointer_panel::snap_dpi;
pub use sidebar::device_element_id;
