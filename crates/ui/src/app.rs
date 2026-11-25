//! Root window view: owns device state, selection, dry-run mode and async orchestration.

use std::sync::Arc;

use agnoperiph_core::{
    CoreResult, DeviceId, DpiStage, KeyIndex, RapidTriggerConfig, TravelHundredthsMm,
};
use agnoperiph_transport::PacketTraceEntry;
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, Theme, ThemeMode,
    alert::Alert,
    button::{Button, ButtonVariants},
    h_flex,
    scroll::ScrollableElement,
    spinner::Spinner,
    switch::Switch,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::backend::{DeviceSnapshot, PeripheralService, PointerChange, SettingChange};
use crate::{hall_effect_panel, inspector_drawer, pointer_panel, rgb_panel, sidebar};

/// Notice shown above the workspace after a failed operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    title: SharedString,
    message: SharedString,
}

impl Banner {
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn message(&self) -> &SharedString {
        &self.message
    }
}

/// Unsaved edits for the selected key; `None` fields fall back to the device snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct KeyDraft {
    pub(crate) actuation: Option<TravelHundredthsMm>,
    pub(crate) rapid_trigger: Option<RapidTriggerConfig>,
}

/// Which unsaved edits a change commits, so a successful write clears only those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DraftScope {
    DpiStages,
    Actuation,
    RapidTrigger,
    None,
}

impl DraftScope {
    fn of(change: &SettingChange) -> Self {
        match change {
            SettingChange::Pointer(PointerChange::DpiStages { .. }) => Self::DpiStages,
            SettingChange::Actuation { .. } => Self::Actuation,
            SettingChange::RapidTrigger { .. } => Self::RapidTrigger,
            _ => Self::None,
        }
    }
}

/// The AgnoPeriph main window.
///
/// Owns the loaded device snapshots and all view state. Every service call runs on the
/// background executor; replies to superseded refreshes are dropped by revision.
pub struct AgnoPeriphApp {
    service: Arc<dyn PeripheralService>,
    devices: Vec<DeviceSnapshot>,
    selected: Option<DeviceId>,
    /// Incremented by every refresh; a reply carrying an older revision is stale.
    revision: u64,
    has_loaded: bool,
    is_loading: bool,
    /// A write is in flight. Only one runs at a time and it is never cancelled,
    /// because a superseded write has still reached the device.
    is_busy: bool,
    dry_run: bool,
    is_inspector_open: bool,
    banner: Option<Banner>,
    packets: Vec<PacketTraceEntry>,
    pub(crate) selected_key: KeyIndex,
    pub(crate) key_draft: KeyDraft,
    pub(crate) dpi_draft: Option<Vec<DpiStage>>,
    pub(crate) socd_first: Option<KeyIndex>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for AgnoPeriphApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgnoPeriphApp")
            .field("devices", &self.devices.len())
            .field("selected", &self.selected)
            .field("dry_run", &self.dry_run)
            .field("is_busy", &self.is_busy)
            .finish_non_exhaustive()
    }
}

impl AgnoPeriphApp {
    /// Creates the view and starts the initial device scan.
    pub fn new(service: Arc<dyn PeripheralService>, cx: &mut Context<Self>) -> Self {
        let dry_run = service.is_dry_run();
        let theme_subscription = cx.observe_global::<Theme>(|_, cx| cx.notify());
        let mut app = Self {
            service,
            devices: Vec::new(),
            selected: None,
            revision: 0,
            has_loaded: false,
            is_loading: false,
            is_busy: false,
            dry_run,
            is_inspector_open: true,
            banner: None,
            packets: Vec::new(),
            selected_key: KeyIndex::new(0),
            key_draft: KeyDraft::default(),
            dpi_draft: None,
            socd_first: None,
            _subscriptions: vec![theme_subscription],
        };
        app.refresh(cx);
        app
    }

    pub fn devices(&self) -> &[DeviceSnapshot] {
        &self.devices
    }

    pub fn selected_device_id(&self) -> Option<&DeviceId> {
        self.selected.as_ref()
    }

    pub fn selected_device(&self) -> Option<&DeviceSnapshot> {
        let id = self.selected.as_ref()?;
        self.devices.iter().find(|device| device.id() == id)
    }

    pub fn selected_key(&self) -> KeyIndex {
        self.selected_key
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn is_loading(&self) -> bool {
        self.is_loading
    }

    pub fn is_busy(&self) -> bool {
        self.is_busy
    }

    pub fn is_inspector_open(&self) -> bool {
        self.is_inspector_open
    }

    pub fn banner(&self) -> Option<&Banner> {
        self.banner.as_ref()
    }

    pub fn packets(&self) -> &[PacketTraceEntry] {
        &self.packets
    }

    /// Re-enumerates devices. Previous data stays visible until the reply lands.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        self.run_refresh(cx, move || {
            let devices = service.refresh();
            (devices, service.packet_log(), service.is_dry_run())
        });
    }

    /// Switches dry-run mode. Device handles reopen in the new mode before any further write.
    pub fn set_dry_run(&mut self, dry_run: bool, cx: &mut Context<Self>) {
        if self.is_busy || dry_run == self.dry_run {
            return;
        }
        let service = self.service.clone();
        self.run_refresh(cx, move || {
            service.set_dry_run(dry_run);
            let devices = service.refresh();
            (devices, service.packet_log(), service.is_dry_run())
        });
    }

    fn run_refresh(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> (CoreResult<Vec<DeviceSnapshot>>, Vec<PacketTraceEntry>, bool)
        + Send
        + 'static,
    ) {
        self.revision += 1;
        let revision = self.revision;
        self.is_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = cx.background_spawn(async move { work() }).await;
            let _ = this.update(cx, |this, cx| this.finish_refresh(revision, reply, cx));
        })
        .detach();
    }

    fn finish_refresh(
        &mut self,
        revision: u64,
        (devices, packets, dry_run): (CoreResult<Vec<DeviceSnapshot>>, Vec<PacketTraceEntry>, bool),
        cx: &mut Context<Self>,
    ) {
        if revision != self.revision {
            return;
        }
        self.is_loading = false;
        self.has_loaded = true;
        self.dry_run = dry_run;
        self.packets = packets;
        self.dpi_draft = None;
        self.key_draft = KeyDraft::default();
        match devices {
            Ok(devices) => {
                self.devices = devices;
                self.banner = None;
                let still_present = self
                    .selected
                    .as_ref()
                    .is_some_and(|id| self.devices.iter().any(|d| d.id() == id));
                if !still_present {
                    self.selected = self.devices.first().map(|d| d.id().clone());
                }
            }
            Err(err) => {
                self.devices.clear();
                self.selected = None;
                self.banner = Some(Banner {
                    title: "Couldn’t scan for devices".into(),
                    message: format!(
                        "{err}. Close vendor software that may hold the device, then refresh."
                    )
                    .into(),
                });
            }
        }
        cx.notify();
    }

    /// Validates and applies one change to the selected device.
    pub(crate) fn apply_change(&mut self, change: SettingChange, cx: &mut Context<Self>) {
        if self.is_busy || self.is_loading {
            return;
        }
        let Some(device) = self.selected.clone() else {
            return;
        };
        self.is_busy = true;
        cx.notify();
        let applied = DraftScope::of(&change);
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_spawn(async move {
                    let result = service.apply(&device, change);
                    (result, service.packet_log())
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_apply(applied, reply, cx));
        })
        .detach();
    }

    fn finish_apply(
        &mut self,
        applied: DraftScope,
        (result, packets): (CoreResult<DeviceSnapshot>, Vec<PacketTraceEntry>),
        cx: &mut Context<Self>,
    ) {
        self.is_busy = false;
        self.packets = packets;
        match result {
            Ok(snapshot) => {
                if let Some(slot) = self.devices.iter_mut().find(|d| d.id() == snapshot.id()) {
                    *slot = snapshot;
                }
                self.banner = None;
                // Only the edits this change committed are cleared; unrelated drafts survive.
                match applied {
                    DraftScope::DpiStages => self.dpi_draft = None,
                    DraftScope::Actuation => self.key_draft.actuation = None,
                    DraftScope::RapidTrigger => self.key_draft.rapid_trigger = None,
                    DraftScope::None => {}
                }
            }
            Err(err) => {
                self.banner = Some(Banner {
                    title: "Couldn’t apply the change".into(),
                    message: format!(
                        "{err}. Nothing was written. Adjust the value, or refresh if the device was disconnected."
                    )
                    .into(),
                });
            }
        }
        cx.notify();
    }

    pub fn select_device(&mut self, device: DeviceId, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&device) {
            return;
        }
        self.selected = Some(device);
        self.dpi_draft = None;
        self.key_draft = KeyDraft::default();
        self.socd_first = None;
        cx.notify();
    }

    pub fn select_key(&mut self, key: KeyIndex, cx: &mut Context<Self>) {
        if self.selected_key != key {
            self.selected_key = key;
            self.key_draft = KeyDraft::default();
            cx.notify();
        }
    }

    pub fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.is_inspector_open = !self.is_inspector_open;
        cx.notify();
    }

    pub fn clear_packets(&mut self, cx: &mut Context<Self>) {
        self.packets.clear();
        cx.notify();
        let service = self.service.clone();
        // The service lock can be held by a slow HID exchange, so never clear on the UI thread.
        cx.background_spawn(async move { service.clear_packet_log() })
            .detach();
    }

    fn toggle_theme(window: &mut Window, cx: &mut Context<Self>) {
        let next = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(next, Some(window), cx);
    }

    fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banner = None;
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let is_dark = theme.mode.is_dark();
        let write_state = if self.dry_run {
            Tag::secondary().small().child("Writes held back")
        } else {
            Tag::warning().small().child("Writes go to devices")
        };

        h_flex()
            .id("header")
            .flex_shrink_0()
            .h_12()
            .px_4()
            .gap_3()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                h_flex()
                    .gap_3()
                    .min_w_0()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("AgnoPeriph"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(device_count_label(self.devices.len())),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Switch::new("dry-run")
                            .label("Dry run")
                            .checked(self.dry_run)
                            .disabled(self.is_busy || self.is_loading)
                            .tooltip("Validate and log writes without sending them to devices")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_dry_run(*checked, cx);
                            })),
                    )
                    .child(write_state)
                    .child(div().w_px().h_5().bg(theme.border))
                    .child(
                        Button::new("refresh")
                            .outline()
                            .small()
                            .icon(IconName::RefreshCw)
                            .label("Refresh")
                            .loading(self.is_loading)
                            .disabled(self.is_busy)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        Button::new("theme-toggle")
                            .ghost()
                            .small()
                            .icon(if is_dark {
                                IconName::Sun
                            } else {
                                IconName::Moon
                            })
                            .tooltip(if is_dark {
                                "Use light appearance"
                            } else {
                                "Use dark appearance"
                            })
                            .on_click(cx.listener(|_, _, window, cx| {
                                Self::toggle_theme(window, cx);
                            })),
                    )
                    .child(
                        Button::new("inspector-toggle")
                            .ghost()
                            .small()
                            .icon(IconName::PanelBottom)
                            .selected(self.is_inspector_open)
                            .tooltip("HID packet inspector")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_inspector(cx))),
                    ),
            )
    }

    fn render_workspace(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let content = v_flex()
            .id("workspace")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p_6()
            .gap_6()
            .when_some(self.banner.clone(), |this, banner| {
                this.child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            Alert::error("error-banner", banner.message)
                                .title(banner.title)
                                .flex_1(),
                        )
                        .child(
                            Button::new("dismiss-banner")
                                .ghost()
                                .small()
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| this.dismiss_banner(cx))),
                        ),
                )
            })
            .when(!self.dry_run, |this| {
                this.child(
                    div()
                        .id("live-writes")
                        .child(Alert::warning(
                            "live-writes-alert",
                            "Changes are written to the device. Fully exit G Hub, ATK HUB and Lamzu software, including tray icons, so they don’t block or overwrite them.",
                        ))
                        .test_support(),
                )
            });

        let body = match self.selected_device() {
            Some(device) => self.render_device(device, cx).into_any_element(),
            None if !self.has_loaded => loading_state(theme).into_any_element(),
            None => self.render_empty_state(cx).into_any_element(),
        };

        content.child(body).overflow_y_scrollbar()
    }

    fn render_device(
        &self,
        device: &DeviceSnapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let info = &device.info;
        let mut facts: Vec<SharedString> = vec![
            info.vendor.to_string().into(),
            info.connection.to_string().into(),
        ];
        if let Some(firmware) = &info.firmware_version {
            facts.push(format!("Firmware {firmware}").into());
        }
        facts.push(format!("{:04X}:{:04X}", info.vid, info.pid).into());

        let header = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(info.model_name.clone()),
                    )
                    .when(device.is_preview, |this| {
                        this.child(
                            Tag::info()
                                .small()
                                .child("Dry-run preview, device unchanged"),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_4()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .children(facts),
            );

        let caps = device.capabilities;
        v_flex()
            .gap_8()
            .child(header)
            .children(device.warnings.iter().enumerate().map(|(ix, warning)| {
                Alert::warning(("device-warning", ix), warning.clone()).title("Partly loaded")
            }))
            .when(caps.has_pointer(), |this| {
                this.child(pointer_panel::render(self, device, cx))
            })
            .when(caps.has_hall_effect(), |this| {
                this.child(hall_effect_panel::render(self, device, cx))
            })
            .when(caps.has_rgb(), |this| {
                this.child(rgb_panel::render(self, device, cx))
            })
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        v_flex()
            .id("empty-state")
            .flex_1()
            .items_center()
            .justify_center()
            .py_16()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("No supported peripherals found"),
            )
            .child(
                div()
                    .max_w_96()
                    .text_sm()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(
                        "Connect a Logitech, Lamzu or ATK device. Fully exit G Hub, ATK HUB and Lamzu software, including tray icons, then refresh.",
                    ),
            )
            .child(
                Button::new("empty-refresh")
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label("Refresh")
                    .loading(self.is_loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }
}

fn device_count_label(count: usize) -> String {
    match count {
        0 => "No devices".to_owned(),
        1 => "1 device".to_owned(),
        n => format!("{n} devices"),
    }
}

fn loading_state(theme: &Theme) -> impl IntoElement + use<> {
    h_flex()
        .py_16()
        .gap_2()
        .justify_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(Spinner::new().small())
        .child("Looking for peripherals…")
}

impl Render for AgnoPeriphApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_header(cx))
            .child(
                h_flex()
                    .items_stretch()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar::render(self, cx))
                    .child(self.render_workspace(cx)),
            )
            .when(self.is_inspector_open, |this| {
                this.child(inspector_drawer::render(self, cx))
            })
    }
}

/// Section scaffold shared by the capability panels: title, one-line purpose, content.
pub(crate) fn section(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    v_flex()
        .id(id)
        .gap_4()
        .pt_6()
        .border_t_1()
        .border_color(theme.border)
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(description),
                ),
        )
}

/// One labelled control row. Labels share a fixed lane so controls form one spine.
pub(crate) fn field(label: impl Into<SharedString>, theme: &Theme) -> Div {
    h_flex().gap_4().min_h_8().child(
        div()
            .w_40()
            .flex_shrink_0()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(label.into()),
    )
}

/// Monospace readout for a numeric value next to its stepper.
pub(crate) fn readout(value: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .min_w_20()
        .text_sm()
        .text_center()
        .font_family(theme.mono_font_family.clone())
        .child(value.into())
}
