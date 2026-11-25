//! Calibration modal: shows which keys report as calibrated, starts and finishes a
//! calibration run, and polls per-key progress only while a run is active.

use std::time::Duration;

use agnoperiph_core::{CalibrationStatus, DeviceId};
use gpui_kit::component::{
    ActiveTheme, Disableable, WindowExt,
    button::{Button, ButtonVariant, ButtonVariants},
    h_flex, v_flex,
};
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, DraftScope, JobOrigin, WriteJob, muted_note};
use crate::backend::SettingChange;
use crate::key_matrix::{self, KeyFace, KeyTone};
use crate::keymap::KeyLayout;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// App-side calibration state shared with the modal.
#[derive(Debug, Clone, Default)]
pub(crate) struct CalibrationUi {
    /// Completed progress reads since the run started.
    pub(crate) polls: usize,
    pub(crate) is_starting: bool,
    pub(crate) is_active: bool,
}

/// Retained modal view; dropping it (the dialog closed) cancels polling.
pub(crate) struct CalibrationModal {
    app: Entity<AgnoPeriphApp>,
    device: DeviceId,
    poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for CalibrationModal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalibrationModal")
            .field("device", &self.device)
            .field("is_polling", &self.poll.is_some())
            .finish_non_exhaustive()
    }
}

/// Opens the calibration modal for the selected device.
pub(crate) fn open(app: &mut AgnoPeriphApp, window: &mut Window, cx: &mut Context<AgnoPeriphApp>) {
    let Some(device) = app.selected_device_id().cloned() else {
        return;
    };
    let app_entity = cx.entity();
    let modal = cx.new(|cx| CalibrationModal::new(app_entity, device, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Calibrate keys")
            .w(px(1040.))
            .child(modal.clone())
    });
}

impl CalibrationModal {
    fn new(app: Entity<AgnoPeriphApp>, device: DeviceId, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&app, |_, _, cx| cx.notify());
        let release = cx.on_release(|modal, cx| {
            // Closing the modal mid-run still ends calibration on the keyboard.
            let device = modal.device.clone();
            modal.app.update(cx, |app, cx| {
                if app.calibration.is_active {
                    app.calibration.is_active = false;
                    app.enqueue(
                        WriteJob {
                            device,
                            changes: vec![SettingChange::FinishCalibration],
                            origin: JobOrigin::User(DraftScope::None),
                        },
                        cx,
                    );
                }
            });
        });
        Self {
            app,
            device,
            poll: None,
            _subscriptions: vec![observe, release],
        }
    }

    fn request_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.app.read(cx).is_dry_run() {
            self.start(cx);
            return;
        }
        let modal = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let modal = modal.clone();
            alert
                .confirm()
                .title("Start calibration?")
                .description(
                    "The keyboard discards its current calibration. Press every key fully to the bottom once, then choose Finish.",
                )
                .ok_text("Start calibration")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let _ = modal.update(cx, CalibrationModal::start);
                    true
                })
        });
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let device = self.device.clone();
        let started = self.app.update(cx, |app, cx| {
            if app.is_busy()
                || app.is_loading()
                || app.calibration.is_active
                || app.calibration.is_starting
            {
                return false;
            }
            app.calibration = CalibrationUi {
                polls: 0,
                is_starting: true,
                is_active: false,
            };
            app.enqueue(
                WriteJob {
                    device,
                    changes: vec![SettingChange::StartCalibration],
                    origin: JobOrigin::CalibrationStart,
                },
                cx,
            );
            true
        });
        if started {
            self.poll = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(POLL_INTERVAL).await;
                    let Ok(keep) = this.update(cx, CalibrationModal::poll_once) else {
                        break;
                    };
                    if !keep {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    /// Queues one progress read unless another write is pending; returns whether to keep polling.
    fn poll_once(&mut self, cx: &mut Context<Self>) -> bool {
        let device = self.device.clone();
        self.app.update(cx, |app, cx| {
            if app.calibration.is_starting {
                return true;
            }
            if !app.calibration.is_active {
                return false;
            }
            if !app.has_pending_writes() {
                app.enqueue(
                    WriteJob {
                        device,
                        changes: vec![SettingChange::PollCalibrationProgress],
                        origin: JobOrigin::CalibrationPoll,
                    },
                    cx,
                );
            }
            true
        })
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.poll = None;
        let device = self.device.clone();
        self.app.update(cx, |app, cx| {
            if !app.calibration.is_active {
                return;
            }
            app.calibration.is_active = false;
            app.enqueue(
                WriteJob {
                    device,
                    changes: vec![SettingChange::FinishCalibration],
                    origin: JobOrigin::User(DraftScope::None),
                },
                cx,
            );
        });
        cx.notify();
    }
}

impl Render for CalibrationModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let Some(device) = app.device(&self.device) else {
            return v_flex().id("calibration-modal").child(muted_note(
                "The keyboard is no longer connected.",
                cx.theme(),
            ));
        };
        let Some(he) = device.hall_effect.as_ref() else {
            return v_flex().id("calibration-modal").child(muted_note(
                "This device doesn’t have magnetic switches to calibrate.",
                cx.theme(),
            ));
        };
        let is_active = app.calibration.is_active;
        let is_starting = app.calibration.is_starting;
        let is_blocked = app.is_busy() || app.is_loading() || is_starting;
        let layout = KeyLayout::for_device(&device.info, &he.capabilities);
        let flags = he.snapshot.calibrated_keys.clone();
        let total = layout.rows.iter().map(Vec::len).sum::<usize>();
        let done = layout
            .rows
            .iter()
            .flatten()
            .filter(|cap| {
                flags
                    .get(usize::from(cap.index.value()))
                    .copied()
                    .unwrap_or(false)
            })
            .count();
        let status = match he.snapshot.calibration {
            CalibrationStatus::Calibrated => "The keyboard reports it is calibrated.".to_owned(),
            CalibrationStatus::NeedsCalibration => "The keyboard asks for calibration.".to_owned(),
            CalibrationStatus::InProgress { progress_pct } => {
                format!("Calibration {progress_pct}% complete.")
            }
            CalibrationStatus::Unknown => "Calibration state unknown.".to_owned(),
        };
        let progress = if is_active {
            format!("Press every key fully to the bottom. {done} of {total} keys calibrated.")
        } else {
            format!("{done} of {total} keys calibrated. {status}")
        };
        let has_flags = he.capabilities.supports_calibration_status && !flags.is_empty();
        let matrix = key_matrix::render_static(
            "calibration-matrix",
            "calibration-key",
            &layout,
            |cap| {
                let calibrated = flags
                    .get(usize::from(cap.index.value()))
                    .copied()
                    .unwrap_or(false);
                KeyFace::plain(if calibrated {
                    KeyTone::Done
                } else {
                    KeyTone::Normal
                })
            },
            cx.theme(),
        );
        let theme = cx.theme();
        v_flex()
            .id("calibration-modal")
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(
                        "Calibration teaches the keyboard each switch’s full travel. Run it after changing switches or if keys actuate unevenly.",
                    ),
            )
            .child(matrix)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(div().size_2().rounded_full().bg(theme.success))
                            .child(div().text_xs().child("Calibrated")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("calibration-progress")
                            .text_sm()
                            .child(if has_flags {
                                progress
                            } else {
                                status
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("calibration-start")
                            .primary()
                            .label("Start calibration")
                            .disabled(is_active || is_blocked)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request_start(window, cx);
                            })),
                    )
                    .child(
                        Button::new("calibration-finish")
                            .outline()
                            .label("Finish")
                            .disabled(!is_active)
                            .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                    ),
            )
    }
}
