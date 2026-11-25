//! Advanced keyboard settings: key layout, key conflict handling and polling rate.

use agnoperiph_core::{KeyboardConfigSnapshot, PollingRateHz};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonGroup},
    h_flex,
    switch::Switch,
    v_flex,
};
use gpui_kit::*;

use crate::app::{AgnoPeriphApp, group, muted_note, section};
use crate::backend::{DeviceSnapshot, SettingChange};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigToggle {
    MacMode,
    ExchangeWasd,
    WinLock,
    SixKeyRollover,
    AntiAccidental,
    Extreme,
}

impl ConfigToggle {
    fn get(self, cfg: KeyboardConfigSnapshot) -> bool {
        match self {
            Self::MacMode => cfg.mac_mode,
            Self::ExchangeWasd => cfg.exchange_wasd,
            Self::WinLock => cfg.win_lock,
            Self::SixKeyRollover => cfg.six_key_rollover,
            Self::AntiAccidental => cfg.anti_accidental_touch,
            Self::Extreme => cfg.extreme_mode,
        }
    }

    fn set(self, cfg: &mut KeyboardConfigSnapshot, value: bool) {
        match self {
            Self::MacMode => cfg.mac_mode = value,
            Self::ExchangeWasd => cfg.exchange_wasd = value,
            Self::WinLock => cfg.win_lock = value,
            Self::SixKeyRollover => cfg.six_key_rollover = value,
            Self::AntiAccidental => cfg.anti_accidental_touch = value,
            Self::Extreme => cfg.extreme_mode = value,
        }
    }
}

impl AgnoPeriphApp {
    fn write_keyboard_config(
        &mut self,
        edit: impl FnOnce(&mut KeyboardConfigSnapshot),
        cx: &mut Context<Self>,
    ) {
        let Some(mut cfg) = self
            .selected_device()
            .and_then(|d| d.keyboard_config.as_ref())
            .map(|c| c.snapshot)
        else {
            return;
        };
        let before = cfg;
        edit(&mut cfg);
        if cfg != before {
            self.apply_change(SettingChange::KeyboardConfig(cfg), cx);
        }
    }
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let base = section(
        "keyboard-config-panel",
        "Advanced",
        "Keyboard-wide behaviour. Changes are written as soon as you switch them.",
        cx.theme(),
    );
    let Some(state) = device.keyboard_config.as_ref() else {
        return base.child(muted_note(
            "Advanced settings couldn’t be read.",
            cx.theme(),
        ));
    };
    let busy = app.is_busy() || app.is_loading();
    let caps = &state.capabilities;
    let cfg = state.snapshot;

    let toggle = |id: &'static str,
                  title: &'static str,
                  detail: &'static str,
                  kind: ConfigToggle,
                  supported: bool,
                  cx: &mut Context<AgnoPeriphApp>| {
        let theme = cx.theme();
        h_flex()
            .id(SharedString::from(format!("{id}-row")))
            .gap_4()
            .py_1()
            .child(
                v_flex()
                    .flex_1()
                    .gap_0p5()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if supported {
                            detail
                        } else {
                            "Not supported by this keyboard."
                        },
                    )),
            )
            .child(
                Switch::new(id)
                    .checked(kind.get(cfg))
                    .disabled(busy || !supported)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        let checked = *checked;
                        this.write_keyboard_config(|c| kind.set(c, checked), cx);
                    })),
            )
    };

    let layout = group("config-layout", "Key layout", cx.theme())
        .child(toggle(
            "config-mac-mode",
            "Mac mode",
            "Swap the Windows and Alt keys to match a Mac layout.",
            ConfigToggle::MacMode,
            caps.supports_mac_mode,
            cx,
        ))
        .child(toggle(
            "config-exchange-wasd",
            "Exchange WASD and arrows",
            "WASD sends the arrow keys and the arrows send WASD.",
            ConfigToggle::ExchangeWasd,
            caps.supports_wasd_swap,
            cx,
        ));
    let conflict = group("config-conflict", "Key conflict", cx.theme())
        .child(toggle(
            "config-win-lock",
            "Windows key lock",
            "Disable the Windows keys so they can’t interrupt a game.",
            ConfigToggle::WinLock,
            caps.supports_win_lock,
            cx,
        ))
        .child(toggle(
            "config-six-key",
            "6-key rollover",
            "Report at most six keys at once for older BIOS and KVM switches.",
            ConfigToggle::SixKeyRollover,
            caps.supports_six_key_rollover,
            cx,
        ))
        .child(toggle(
            "config-anti-accidental",
            "Anti-accidental touch",
            "Ignore very light brushes against the keys.",
            ConfigToggle::AntiAccidental,
            caps.supports_anti_accidental_touch,
            cx,
        ))
        .child(toggle(
            "config-extreme",
            "Extreme mode (0.01 mm)",
            "Use 0.01 mm actuation and Rapid Trigger steps instead of 0.02 mm.",
            ConfigToggle::Extreme,
            caps.supports_extreme_mode,
            cx,
        ));

    let mut rates: Vec<PollingRateHz> = caps.supported_polling_rates.clone();
    rates.sort_by_key(|r| std::cmp::Reverse(r.as_hz()));
    let rate_buttons = rates.into_iter().map(|rate| {
        let label = if rate.as_hz() >= 1000 {
            format!("{}K", rate.as_hz() / 1000)
        } else {
            rate.as_hz().to_string()
        };
        Button::new(("config-rate", u64::from(rate.as_hz())))
            .label(label)
            .selected(cfg.polling_rate == rate)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.write_keyboard_config(|c| c.polling_rate = rate, cx);
            }))
    });
    let theme = cx.theme();
    let rate = group("config-polling", "Polling rate", theme)
        .child(
            h_flex().child(
                ButtonGroup::new("config-polling-rate")
                    .outline()
                    .small()
                    .children(rate_buttons),
            ),
        )
        .child(muted_note(
            format!(
                "Currently {}. Higher rates lower input latency but use more CPU.",
                cfg.polling_rate
            ),
            theme,
        ));

    base.child(layout).child(conflict).child(rate)
}
