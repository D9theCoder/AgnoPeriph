//! Keeps the device list and battery levels current without the Refresh button: plug and
//! unplug detection, a slow battery poll, and retries for devices that wouldn't open.
//!
//! Every check is a readonly query or a plain enumeration, and waits while something else
//! is using the devices.

use std::collections::BTreeSet;
use std::time::Duration;

use agnoperiph_core::{BatteryStatus, CoreResult, DeviceId};
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::UnavailableDevice;
use crate::tray_model::TrayChanged;

/// How often attached interfaces are compared with the last scan.
pub const DEVICE_SCAN_INTERVAL: Duration = Duration::from_secs(3);
/// In [`DEVICE_SCAN_INTERVAL`] ticks: unopened devices are retried every 15 s.
const RETRY_EVERY: u32 = 5;
/// In ticks: battery levels are read every minute.
const BATTERY_EVERY: u32 = 20;
/// In ticks: with the window closed to the tray, interfaces are compared every 9 s.
const TRAY_SCAN_EVERY: u32 = 3;

#[derive(Debug, Default)]
pub(crate) struct DeviceWatch {
    pub(crate) unavailable: Vec<UnavailableDevice>,
    /// A change seen once; it must hold for a second check before it counts, so a USB
    /// reset (such as a polling rate change) doesn't read as unplug and replug.
    pending_change: Option<BTreeSet<String>>,
    /// Devices already rescanned since they last stopped answering.
    woken: BTreeSet<DeviceId>,
    is_scanning: bool,
    is_reading_batteries: bool,
    is_retrying: bool,
}

impl AgnoPeriphApp {
    /// Recognized peripherals that are attached but couldn't be opened.
    pub fn unavailable_devices(&self) -> &[UnavailableDevice] {
        &self.watch.unavailable
    }

    pub(crate) fn spawn_device_watcher(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                cx.background_executor().timer(DEVICE_SCAN_INTERVAL).await;
                tick = tick.wrapping_add(1);
                let updated = this.update(cx, |this, cx| {
                    if this.has_window() || tick.is_multiple_of(TRAY_SCAN_EVERY) {
                        this.scan_devices_now(cx);
                    }
                    if tick.is_multiple_of(RETRY_EVERY) {
                        this.retry_unavailable_now(cx);
                    }
                    if tick.is_multiple_of(BATTERY_EVERY) {
                        this.poll_batteries_now(cx);
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        })
    }

    /// A key held on the tester counts as use; an idle tester doesn't, so devices are
    /// still detected while its tab is open.
    fn can_check_devices(&self) -> bool {
        let is_testing_a_key = self.is_live_polling() && self.tester_depth() > 0;
        self.has_loaded()
            && !self.is_loading()
            && !self.has_pending_writes()
            && !self.is_calibrating()
            && !is_testing_a_key
    }

    /// Rescans once peripherals were plugged in or removed. The watcher calls this every
    /// [`DEVICE_SCAN_INTERVAL`]; a change counts on its second sighting.
    pub fn scan_devices_now(&mut self, cx: &mut Context<Self>) {
        if self.watch.is_scanning || !self.can_check_devices() {
            return;
        }
        self.watch.is_scanning = true;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let change = cx
                .background_spawn(async move { service.changed_interfaces() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.watch.is_scanning = false;
                let change = change.ok().flatten();
                if change.is_some() && change == this.watch.pending_change {
                    if this.can_check_devices() {
                        this.watch.pending_change = None;
                        this.refresh_keeping_work(cx);
                    }
                } else {
                    this.watch.pending_change = change;
                }
            });
        })
        .detach();
    }

    /// Rescans if a device that wouldn't open now does.
    pub fn retry_unavailable_now(&mut self, cx: &mut Context<Self>) {
        if self.watch.is_retrying || self.watch.unavailable.is_empty() || !self.can_check_devices()
        {
            return;
        }
        self.watch.is_retrying = true;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let opens = cx
                .background_spawn(async move { service.can_open_unavailable() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.watch.is_retrying = false;
                if opens && this.can_check_devices() {
                    this.refresh_keeping_work(cx);
                }
            });
        })
        .detach();
    }

    /// Reads every battery, updating the levels in place.
    pub fn poll_batteries_now(&mut self, cx: &mut Context<Self>) {
        if self.watch.is_reading_batteries || !self.can_check_devices() {
            return;
        }
        let targets: Vec<DeviceId> = self
            .devices()
            .iter()
            .filter(|d| d.capabilities.has_battery())
            .map(|d| d.id().clone())
            .collect();
        if targets.is_empty() {
            return;
        }
        self.watch.is_reading_batteries = true;
        let revision = self.revision();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let readings = cx
                .background_spawn(async move {
                    targets
                        .into_iter()
                        .map(|id| {
                            let reading = service.read_battery(&id);
                            (id, reading)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.watch.is_reading_batteries = false;
                if this.revision() == revision {
                    this.finish_battery_poll(readings, cx);
                }
            });
        })
        .detach();
    }

    fn finish_battery_poll(
        &mut self,
        readings: Vec<(DeviceId, CoreResult<BatteryStatus>)>,
        cx: &mut Context<Self>,
    ) {
        let mut is_changed = false;
        let mut is_awake_again = false;
        for (id, reading) in readings {
            // A failed read keeps the last level: sleeping mice often don't answer.
            let Ok(mut battery) = reading else {
                self.watch.woken.remove(&id);
                continue;
            };
            let Some(device) = self.device(&id) else {
                continue;
            };
            let previous = device.battery;
            let has_unread_settings = !device.warnings.is_empty();
            if !battery.wireless_connected {
                self.watch.woken.remove(&id);
            } else if has_unread_settings
                && previous.is_none_or(|b| !b.wireless_connected)
                && self.watch.woken.insert(id.clone())
            {
                // It was asleep when scanned, so its settings still need reading.
                is_awake_again = true;
            }
            if let Some(previous) = previous {
                battery.percentage = battery.percentage.or(previous.percentage);
                battery.voltage_mv = battery.voltage_mv.or(previous.voltage_mv);
            }
            if previous != Some(battery) {
                self.store_battery(&id, battery);
                is_changed = true;
            }
        }
        if is_changed {
            self.observe_batteries(cx);
            cx.emit(TrayChanged);
            cx.notify();
        }
        if is_awake_again && self.can_check_devices() {
            self.refresh_keeping_work(cx);
        }
    }
}
