//! Unsaved-changes workflow: staging edits into the device draft, the dirty bar at the
//! bottom of a device page, Undo / Discard / Apply, the switch guard, the keyboard
//! shortcuts that drive them, and low-battery alerts fed from device snapshots.

use agnoperiph_core::{DeviceId, KeyIndex};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariant, ButtonVariants},
    dialog::{DialogClose, DialogFooter},
    h_flex,
    kbd::Kbd,
    notification::Notification,
    popover::Popover,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AfterApply, AgnoPeriphApp, JobOrigin, Route, WriteJob};
use crate::backend::DeviceSnapshot;
use crate::draft::{ChangeId, DeviceDraft, EditKey, PendingChange, rewrites_all_keys};
use crate::keymap::KeyLayout;
use crate::shortcuts::{
    ApplyChanges, Dismiss, NextTab, PreviousTab, REFERENCE, SelectDevice1, SelectDevice2,
    SelectDevice3, SelectDevice4, SelectDevice5, SelectDevice6, SelectDevice7, SelectDevice8,
    SelectDevice9, ToggleShortcuts, UndoChange,
};

/// How long the Discard notification offers Undo.
const DISCARD_UNDO_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);

struct DiscardNotice;

fn key_label(device: &DeviceSnapshot) -> impl Fn(KeyIndex) -> String + '_ {
    let layout = device
        .hall_effect
        .as_ref()
        .map(|he| KeyLayout::for_device(&device.info, &he.capabilities));
    move |key| {
        layout.as_ref().map_or_else(
            || format!("Key {}", key.value()),
            |l| l.label(key).to_string(),
        )
    }
}

impl AgnoPeriphApp {
    /// Whether the selected device has edits that haven't been applied.
    pub fn is_dirty(&self) -> bool {
        !self.draft.is_empty()
    }

    pub(crate) fn has_dirty_bar(&self) -> bool {
        self.is_dirty() && self.draft_device.as_ref() == self.selected_device_id()
    }

    /// The unsaved edits as the dirty bar lists them.
    pub fn pending_changes(&self) -> Vec<PendingChange> {
        let Some(raw) = self.selected_raw() else {
            return Vec::new();
        };
        let label = key_label(raw);
        self.draft.pending(raw, &label)
    }

    /// Whether an undo step is available.
    pub fn can_undo_draft(&self) -> bool {
        self.history.can_undo()
    }

    /// Whether Apply would send something right now.
    pub fn can_apply(&self) -> bool {
        self.is_dirty() && !self.has_pending_writes() && !self.is_loading()
    }

    pub(crate) fn sync_effective(&mut self) {
        self.effective = match (&self.selected, self.draft.is_empty()) {
            (Some(id), false) if self.draft_device.as_ref() == Some(id) => {
                self.device(id).map(|raw| self.draft.overlay(raw))
            }
            _ => None,
        };
    }

    /// Drops all unsaved edits without keeping them for Undo.
    pub(crate) fn reset_draft(&mut self) {
        self.draft = DeviceDraft::default();
        self.history.clear();
        self.draft_device = self.selected_device_id().cloned();
        self.apply_failure = None;
        self.is_changes_open = false;
        self.sync_effective();
    }

    /// Drops staged values the device now holds, after a refresh or write.
    pub(crate) fn renormalize_draft(&mut self) {
        let raw = self
            .draft_device
            .as_ref()
            .and_then(|id| self.device(id))
            .cloned();
        if let Some(raw) = raw {
            self.draft.normalize(&raw);
        } else {
            self.draft = DeviceDraft::default();
            self.history.clear();
        }
        if self.draft.is_empty() {
            self.apply_failure = None;
            self.is_changes_open = false;
        }
        self.sync_effective();
    }

    /// Stages an edit. `coalesce` merges consecutive edits with the same key (a slider
    /// drag) into one undo step; an edit that changes nothing records no step.
    pub(crate) fn edit_draft(
        &mut self,
        key: EditKey,
        coalesce: bool,
        edit: impl FnOnce(&mut DeviceDraft, &DeviceSnapshot),
        cx: &mut Context<Self>,
    ) {
        if self.has_pending_writes() || self.is_loading() {
            return;
        }
        let Some(raw) = self.selected_raw().cloned() else {
            return;
        };
        if self.draft_device.as_ref() != Some(raw.id()) {
            self.draft = DeviceDraft::default();
            self.history.clear();
            self.draft_device = Some(raw.id().clone());
        }
        let before = self.draft.clone();
        let mut next = before.clone();
        edit(&mut next, &raw);
        next.normalize(&raw);
        if next == before {
            if !coalesce {
                self.history.end_group();
            }
            return;
        }
        self.history.record(&before, key, coalesce);
        self.draft = next;
        self.apply_failure = None;
        self.sync_effective();
        self.request_sync(cx);
        cx.notify();
    }

    /// Closes the current slider-drag undo step.
    pub(crate) fn end_edit_group(&mut self) {
        self.history.end_group();
    }

    /// Reverts the most recent edit step.
    pub fn undo_draft(&mut self, cx: &mut Context<Self>) {
        if self.has_pending_writes() {
            return;
        }
        let Some(previous) = self.history.pop() else {
            return;
        };
        self.draft = previous;
        self.renormalize_draft();
        self.request_sync(cx);
        cx.notify();
    }

    /// Reverts one pending change, as one undo step.
    pub fn revert_change(&mut self, id: ChangeId, cx: &mut Context<Self>) {
        self.edit_draft(EditKey::Bulk, false, |draft, _| draft.revert(id), cx);
        if self.draft.is_empty() {
            self.is_changes_open = false;
        }
    }

    /// Reverts every pending change and offers Undo in a notification.
    pub fn discard_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_dirty() || self.has_pending_writes() {
            return;
        }
        let Some(device) = self.draft_device.clone() else {
            return;
        };
        let count = self.pending_changes().len();
        self.discarded = Some((device, self.draft.clone()));
        self.reset_draft();
        self.request_sync(cx);
        cx.notify();

        let app = cx.entity().downgrade();
        let message = match count {
            1 => "Discarded 1 change".to_owned(),
            n => format!("Discarded {n} changes"),
        };
        window.push_notification(
            Notification::new()
                .id::<DiscardNotice>()
                .message(message)
                .action(move |_, _, _| {
                    let app = app.clone();
                    Button::new("discard-undo").small().label("Undo").on_click(
                        move |_, window, cx| {
                            let _ = app.update(cx, AgnoPeriphApp::restore_discarded);
                            window.remove_notification::<DiscardNotice>(cx);
                        },
                    )
                }),
            cx,
        );
        if let Some(handle) = self.window_handle {
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(DISCARD_UNDO_WINDOW).await;
                let _ = cx.update_window(handle, |_, window, cx| {
                    window.remove_notification::<DiscardNotice>(cx);
                });
            })
            .detach();
        }
    }

    /// Brings back the last discarded edits, when their device is still selected.
    pub fn restore_discarded(&mut self, cx: &mut Context<Self>) {
        let Some((device, draft)) = self.discarded.take() else {
            return;
        };
        if self.selected_device_id() != Some(&device) || self.has_pending_writes() {
            return;
        }
        let before = self.draft.clone();
        self.history.record(&before, EditKey::Bulk, false);
        self.draft = draft;
        self.draft_device = Some(device);
        self.renormalize_draft();
        self.request_sync(cx);
        cx.notify();
    }

    /// Applies every pending change. Rewriting every key's actuation asks first on live
    /// devices, because it erases the onboard actuation table.
    pub fn apply_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_draft_then(None, window, cx);
    }

    /// Applies the pending changes, then runs `next` once the write succeeds. `next` is
    /// only stored when a write is actually queued, so a cancelled confirmation leaves
    /// nothing behind for a later Apply to run.
    pub(crate) fn apply_draft_then(
        &mut self,
        next: Option<AfterApply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_apply() {
            return;
        }
        let Some(raw) = self.selected_raw() else {
            return;
        };
        let changes = self.draft.setting_changes(raw);
        if !self.is_dry_run() && rewrites_all_keys(&changes) {
            let key_count = raw
                .hall_effect
                .as_ref()
                .map_or(0, |he| he.snapshot.keys.len());
            let app = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let app = app.clone();
                let next = next.clone();
                alert
                    .confirm()
                    .title("Rewrite actuation for all keys?")
                    .description(format!(
                        "This erases and rewrites the onboard actuation table for all {key_count} keys."
                    ))
                    .ok_text("Rewrite all keys")
                    .ok_variant(ButtonVariant::Warning)
                    .cancel_text("Cancel")
                    .on_ok(move |_, _, cx| {
                        let next = next.clone();
                        let _ = app.update(cx, |this, cx| this.commit_draft_then(next, cx));
                        true
                    })
            });
            return;
        }
        self.commit_draft_then(next, cx);
    }

    /// Queues the pending changes as one write job.
    fn commit_draft_then(&mut self, next: Option<AfterApply>, cx: &mut Context<Self>) {
        if !self.can_apply() {
            return;
        }
        let (Some(device), Some(raw)) = (self.selected_device_id().cloned(), self.selected_raw())
        else {
            return;
        };
        let (steps, changes): (Vec<ChangeId>, Vec<_>) =
            self.draft.setting_changes(raw).into_iter().unzip();
        if changes.is_empty() {
            return;
        }
        self.history.end_group();
        self.after_apply = next;
        self.apply_failure = None;
        self.is_changes_open = false;
        self.enqueue(
            WriteJob {
                device,
                changes,
                origin: JobOrigin::ApplyDraft { steps },
            },
            cx,
        );
    }

    /// After a job stopped at `failed`, drops the changes whose writes all completed.
    pub(crate) fn finish_partial_apply(
        &mut self,
        device: &DeviceId,
        steps: &[ChangeId],
        failed: usize,
        error: &agnoperiph_core::CoreError,
    ) {
        self.after_apply = None;
        if self.draft_device.as_ref() != Some(device) {
            return;
        }
        let remaining = &steps[failed.min(steps.len())..];
        for id in &steps[..failed.min(steps.len())] {
            if !remaining.contains(id) {
                self.draft.revert(*id);
            }
        }
        // Undo steps may refer to values that are now on the device.
        self.history.clear();
        let label = steps.get(failed).and_then(|id| {
            self.pending_changes()
                .into_iter()
                .find(|c| c.id == *id)
                .map(|c| c.label)
        });
        self.apply_failure = Some(
            match label {
                Some(label) => format!("{label} wasn’t applied: {error}"),
                None => format!("Some changes weren’t applied: {error}"),
            }
            .into(),
        );
    }

    pub(crate) fn run_after_apply(&mut self, next: AfterApply, cx: &mut Context<Self>) {
        match next {
            AfterApply::SelectDevice(device) => self.select_device(device, cx),
            AfterApply::SwitchProfile { device, slot } => self.switch_profile(&device, slot, cx),
        }
    }

    /// Selects the device at `ix` (0-based) in the sidebar order.
    pub fn select_device_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.devices().get(ix).map(|d| d.id().clone()) {
            self.request_select_device(id, window, cx);
        }
    }

    pub fn is_shortcuts_open(&self) -> bool {
        self.is_shortcuts_open
    }

    pub fn is_changes_open(&self) -> bool {
        self.is_changes_open
    }

    pub fn set_shortcuts_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.is_shortcuts_open != open {
            self.is_shortcuts_open = open;
            cx.notify();
        }
    }

    pub fn set_changes_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let open = open && self.is_dirty();
        if self.is_changes_open != open {
            self.is_changes_open = open;
            cx.notify();
        }
    }

    /// Escape: closes the topmost surface the app owns.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
        } else if self.is_shortcuts_open {
            self.set_shortcuts_open(false, cx);
        } else if self.is_changes_open {
            self.set_changes_open(false, cx);
        } else if self.paired.pick.is_some() {
            self.paired.cancel_pick();
            cx.notify();
        }
    }

    pub fn set_low_battery_alerts(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.config.low_battery_alerts != enabled {
            self.config.low_battery_alerts = enabled;
            self.save_config();
            cx.notify();
        }
    }

    /// Feeds every device's battery reading to the alert tracker and sends due toasts.
    pub(crate) fn observe_batteries(&mut self, cx: &mut Context<Self>) {
        let enabled = self.config.low_battery_alerts;
        let mut alerts = Vec::new();
        let readings: Vec<_> = self
            .devices()
            .iter()
            .filter(|d| d.capabilities.has_battery())
            .map(|d| {
                (
                    d.id().as_str().to_owned(),
                    d.info.connection,
                    d.battery,
                    d.info.model_name.clone(),
                )
            })
            .collect();
        for (id, connection, battery, name) in readings {
            if let Some(alert) = self
                .battery_alerts
                .observe(&id, connection, battery, enabled)
            {
                alerts.push(alert.message(&name));
            }
        }
        for (title, body) in alerts {
            let system = self.system.clone();
            cx.background_spawn(async move {
                let _ = system.send_notification(&title, &body);
            })
            .detach();
        }
    }
}

/// Opens the unsaved-changes guard before leaving the selected device or preset.
pub(crate) fn open_guard(
    app: &mut AgnoPeriphApp,
    next: AfterApply,
    target: &str,
    window: &mut Window,
    cx: &mut Context<AgnoPeriphApp>,
) {
    let device_name = app
        .selected_raw()
        .map_or_else(String::new, |d| d.info.model_name.clone());
    let changes = app.pending_changes();
    let reason = format!("You’re switching to {target}.");
    let entity = cx.entity().downgrade();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let theme = cx.theme();
        let list = v_flex()
            .id("guard-changes")
            .gap_1()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .children(changes.iter().take(6).map(|change| {
                h_flex()
                    .gap_3()
                    .justify_between()
                    .text_sm()
                    .child(div().min_w_0().truncate().child(change.label.clone()))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(format!("{} → {}", change.from, change.to)),
                    )
            }))
            .when(changes.len() > 6, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("and {} more", changes.len() - 6)),
                )
            });
        let discard_app = entity.clone();
        let discard_next = next.clone();
        let apply_app = entity.clone();
        let apply_next = next.clone();
        alert
            .title(format!("Apply changes to {device_name}?"))
            .description(
                v_flex()
                    .gap_3()
                    .child(format!("{reason} These changes haven’t been sent yet:"))
                    .child(list),
            )
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(
                        DialogClose::new()
                            .child(Button::new("guard-cancel").outline().label("Cancel")),
                    )
                    .child(div().flex_1())
                    // Both actions close the dialog themselves; the write that
                    // "Apply and switch" queues must not keep it open.
                    .child(
                        Button::new("guard-discard")
                            .outline()
                            .label("Discard and switch")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let next = discard_next.clone();
                                let _ = discard_app.update(cx, |this, cx| {
                                    this.reset_draft();
                                    this.run_after_apply(next, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("guard-apply")
                            .primary()
                            .label("Apply and switch")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let next = apply_next.clone();
                                let _ = apply_app.update(cx, |this, cx| {
                                    this.apply_draft_then(Some(next), window, cx);
                                });
                            }),
                    ),
            )
    });
}

/// Registers the window shortcuts on the app root.
pub(crate) fn bind_actions(root: Stateful<Div>, cx: &mut Context<AgnoPeriphApp>) -> Stateful<Div> {
    let select = |ix: usize| {
        move |this: &mut AgnoPeriphApp, window: &mut Window, cx: &mut Context<AgnoPeriphApp>| {
            this.select_device_at(ix, window, cx);
        }
    };
    let s = [
        select(0),
        select(1),
        select(2),
        select(3),
        select(4),
        select(5),
        select(6),
        select(7),
        select(8),
    ];
    let [s1, s2, s3, s4, s5, s6, s7, s8, s9] = s;
    root.on_action(cx.listener(|this, _: &ApplyChanges, window, cx| {
        if this.route() == Route::Device {
            this.apply_draft(window, cx);
        }
    }))
    .on_action(cx.listener(|this, _: &UndoChange, _, cx| {
        if this.route() == Route::Device {
            this.undo_draft(cx);
        }
    }))
    .on_action(cx.listener(|this, _: &NextTab, _, cx| this.cycle_keyboard_tab(true, cx)))
    .on_action(cx.listener(|this, _: &PreviousTab, _, cx| this.cycle_keyboard_tab(false, cx)))
    .on_action(cx.listener(|this, _: &ToggleShortcuts, _, cx| {
        let open = !this.is_shortcuts_open;
        this.set_shortcuts_open(open, cx);
    }))
    .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice1, w, cx| s1(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice2, w, cx| s2(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice3, w, cx| s3(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice4, w, cx| s4(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice5, w, cx| s5(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice6, w, cx| s6(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice7, w, cx| s7(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice8, w, cx| s8(this, w, cx)))
    .on_action(cx.listener(move |this, _: &SelectDevice9, w, cx| s9(this, w, cx)))
}

fn kbd(keys: &'static str) -> Option<Kbd> {
    Keystroke::parse(keys).ok().map(|k| Kbd::new(k).outline())
}

/// Header button with the keyboard shortcuts reference.
pub(crate) fn shortcuts_button(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let entity = cx.entity().downgrade();
    Popover::new("shortcuts-popover")
        .anchor(Anchor::TopRight)
        .open(app.is_shortcuts_open)
        .on_open_change(move |open, _, cx| {
            let open = *open;
            let _ = entity.update(cx, |this, cx| this.set_shortcuts_open(open, cx));
        })
        .trigger(
            Button::new("shortcuts-toggle")
                .ghost()
                .small()
                .icon(Icon::new(gpui_kit::assets::IconName::Keyboard))
                .accessibility_label("Keyboard shortcuts")
                .tooltip_with_action(
                    "Keyboard shortcuts",
                    &ToggleShortcuts,
                    Some(crate::shortcuts::CONTEXT),
                ),
        )
        .content(|_, _, cx| {
            let theme = cx.theme();
            v_flex()
                .id("shortcuts-list")
                .w_80()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Keyboard shortcuts"),
                )
                .children(REFERENCE.iter().map(|(label, keys)| {
                    h_flex()
                        .gap_3()
                        .justify_between()
                        .text_sm()
                        .child(div().text_color(theme.muted_foreground).child(*label))
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_1()
                                .children(keys.iter().copied().filter_map(kbd)),
                        )
                }))
        })
}

/// The bar under a device page while it has unsaved edits.
pub(crate) fn render(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let changes = app.pending_changes();
    let count = changes.len();
    let busy = app.is_busy();
    let can_apply = app.can_apply();
    let can_undo = app.can_undo_draft() && !app.has_pending_writes();
    let failure = app.apply_failure.clone();
    let dry_run = app.is_dry_run();
    let summary: SharedString = changes
        .iter()
        .map(|c| c.label.clone())
        .collect::<Vec<_>>()
        .join(", ")
        .into();
    let entity = cx.entity().downgrade();
    let list = changes.clone();
    let list_app = entity.clone();
    let count_label = match count {
        1 => "1 unsaved change".to_owned(),
        n => format!("{n} unsaved changes"),
    };

    let changes_popover = Popover::new("dirty-changes-popover")
        .anchor(Anchor::BottomLeft)
        .open(app.is_changes_open)
        .on_open_change(move |open, _, cx| {
            let open = *open;
            let _ = entity.update(cx, |this, cx| this.set_changes_open(open, cx));
        })
        .trigger(
            Button::new("dirty-changes")
                .ghost()
                .small()
                .icon(IconName::ChevronUp)
                .label(count_label)
                .tooltip("Show unsaved changes"),
        )
        .content(move |_, _, cx| {
            let theme = cx.theme();
            v_flex()
                .id("dirty-changes-list")
                .w_96()
                .gap_1()
                .child(
                    div()
                        .pb_1()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Unsaved changes"),
                )
                .children(list.iter().map(|change| {
                    let id = change.id;
                    let app = list_app.clone();
                    h_flex()
                        .id(SharedString::from(format!("change-{}", id.slug())))
                        .gap_3()
                        .py_1()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(div().text_sm().truncate().child(change.label.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .child(match &change.scope {
                                            Some(scope) => format!("{scope} · was {}", change.from),
                                            None => format!("was {}", change.from),
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .max_w_40()
                                .truncate()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .child(change.to.clone()),
                        )
                        .child(
                            Button::new(SharedString::from(format!("revert-{}", id.slug())))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Undo2)
                                .accessibility_label(format!("Revert {}", change.label))
                                .tooltip("Revert this change")
                                .on_click(move |_, _, cx| {
                                    let _ = app.update(cx, |this, cx| this.revert_change(id, cx));
                                }),
                        )
                }))
        });

    let theme = cx.theme();
    let status = match &failure {
        Some(message) => div()
            .id("dirty-failure")
            .flex_1()
            .min_w_0()
            .truncate()
            .text_sm()
            .text_color(theme.danger)
            .child(message.clone()),
        None => div()
            .id("dirty-summary")
            .flex_1()
            .min_w_0()
            .truncate()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(summary),
    };

    h_flex()
        .id("dirty-bar")
        .flex_shrink_0()
        .gap_2()
        .px_5()
        .py_2()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .child(changes_popover)
        .child(status)
        .when(dry_run, |this| {
            this.child(Tag::secondary().xsmall().child("Dry run: logged, not sent"))
        })
        .child(
            Button::new("dirty-undo")
                .ghost()
                .small()
                .icon(IconName::Undo2)
                .label("Undo")
                .disabled(!can_undo)
                .tooltip_with_action(
                    "Undo last change",
                    &UndoChange,
                    Some(crate::shortcuts::CONTEXT),
                )
                .on_click(cx.listener(|this, _, _, cx| this.undo_draft(cx))),
        )
        .child(
            Button::new("dirty-discard")
                .outline()
                .small()
                .label("Discard")
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.discard_draft(window, cx))),
        )
        .child(
            Button::new("dirty-apply")
                .primary()
                .small()
                .label(if failure.is_some() { "Retry" } else { "Apply" })
                .loading(busy)
                .disabled(!can_apply)
                .tooltip_with_action(
                    "Send changes to the device",
                    &ApplyChanges,
                    Some(crate::shortcuts::CONTEXT),
                )
                .on_click(cx.listener(|this, _, window, cx| this.apply_draft(window, cx))),
        )
}
