//! App preferences: tray and startup behaviour, appearance and updates.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, ThemeMode,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::design::{Accent, dp};
use crate::updates::CURRENT_VERSION;
use crate::widgets::{page_title, panel, panel_head, segment, segmented, setting_row};

pub(crate) fn render(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    v_flex()
        .id("settings")
        .max_w(dp(640.))
        .gap(dp(14.))
        .child(page_title("Settings"))
        .child(app_panel(app, cx))
        .child(appearance_panel(app, cx))
        .child(updates_panel(app, cx))
}

fn app_panel(app: &AgnoPeriphApp, cx: &mut Context<AgnoPeriphApp>) -> impl IntoElement + use<> {
    let config = app.config();
    let close_to_tray = Switch::new("pref-close-to-tray")
        .accessibility_label("Close to tray")
        .checked(config.close_to_tray)
        .on_change(cx.listener(|this, checked: &bool, _, cx| {
            this.set_close_to_tray(*checked, cx);
        }));
    let start_with_windows = Switch::new("pref-start-with-windows")
        .accessibility_label("Start with Windows")
        .checked(config.start_with_windows)
        .on_change(cx.listener(|this, checked: &bool, _, cx| {
            this.set_start_with_windows(*checked, cx);
        }));
    let low_battery = Switch::new("pref-low-battery")
        .accessibility_label("Low-battery alerts")
        .checked(config.low_battery_alerts)
        .on_change(cx.listener(|this, checked: &bool, _, cx| {
            this.set_low_battery_alerts(*checked, cx);
        }));

    panel("settings-app", cx)
        .child(panel_head("App"))
        .child(setting_row(
            "Close to tray",
            Some("Closing the window keeps AgnoPeriph running in the notification area, so Game profiles keep switching.".into()),
            close_to_tray,
            true,
            cx,
        ))
        .child(setting_row(
            "Start with Windows",
            Some("Opens AgnoPeriph when you sign in.".into()),
            start_with_windows,
            false,
            cx,
        ))
        .child(setting_row(
            "Low-battery alerts",
            Some("Notifies below 20% and again below 5% while a wireless device runs on battery.".into()),
            low_battery,
            false,
            cx,
        ))
}

fn appearance_panel(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let is_dark = cx.theme().mode.is_dark();
    let mut themes = segmented(cx);
    for (id, label, mode) in [
        ("theme-light", "Light", ThemeMode::Light),
        ("theme-dark", "Dark", ThemeMode::Dark),
    ] {
        themes = themes.child(
            segment(Button::new(id), label, mode.is_dark() == is_dark, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.set_theme_mode(mode, cx))),
        );
    }
    let current = app.config().accent;
    let swatches = h_flex()
        .id("accent-colors")
        .flex_shrink_0()
        .gap(dp(6.))
        .children(
            Accent::ALL
                .into_iter()
                .map(|accent| swatch(accent, accent == current, cx).into_any_element()),
        );

    panel("settings-appearance", cx)
        .child(panel_head("Appearance"))
        .child(setting_row("Theme", None, themes, true, cx))
        .child(setting_row(
            "Accent color",
            Some(
                "Used for selections, switches and Apply. Each color is tuned for both themes."
                    .into(),
            ),
            swatches,
            false,
            cx,
        ))
}

/// One accent preset. The selected one carries a ring in the text color.
fn swatch(
    accent: Accent,
    is_selected: bool,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let color = accent.swatch(theme.mode);
    let ring = if is_selected {
        theme.foreground
    } else {
        theme.transparent
    };
    div()
        .p(dp(2.))
        .rounded_full()
        .border_2()
        .border_color(ring)
        .child(
            Button::new(SharedString::from(format!(
                "accent-{}",
                accent.label().to_lowercase()
            )))
            // Every state keeps the preset's own colour, including hover.
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(color)
                    .hover(color)
                    .active(color),
            )
            .selected(is_selected)
            .accessibility_label(accent.label())
            .tooltip(accent.label())
            .size(dp(22.))
            .rounded_full()
            .bg(color)
            .on_click(cx.listener(move |this, _, _, cx| this.set_accent(accent, cx))),
        )
}

fn updates_panel(app: &AgnoPeriphApp, cx: &mut Context<AgnoPeriphApp>) -> impl IntoElement + use<> {
    let is_checking = app.is_checking_updates();
    let check = Button::new("check-updates")
        .outline()
        .label("Check for updates")
        .loading(is_checking)
        .disabled(is_checking || app.is_installing_update())
        .on_click(cx.listener(|this, _, _, cx| this.check_for_updates(true, cx)))
        .when(is_checking, |this| this.tooltip("Checking for updates…"));

    panel("settings-updates", cx)
        .child(panel_head("Updates"))
        .child(setting_row(
            format!("AgnoPeriph {CURRENT_VERSION}"),
            Some("Updates install from GitHub releases.".into()),
            check,
            true,
            cx,
        ))
}
