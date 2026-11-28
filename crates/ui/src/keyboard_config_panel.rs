//! Advanced keyboard settings: key layout, function keys, key conflict handling and
//! polling rate. Changes are staged as unsaved edits and sent with Apply.

use agnoperiph_core::{KeyboardConfigSnapshot, PollingRateHz};
use gpui_kit::component::{ActiveTheme, Disableable, button::Button, h_flex, v_flex};
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::DeviceSnapshot;
use crate::design::dp;
use crate::draft::{ChangeId, EditKey};
use crate::widgets::{MarkedSwitch, device_setting_row, panel, panel_head, segment, segmented};

const GAP: f32 = 16.0;
const MIN_COLUMN_WIDTH: f32 = 340.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigToggle {
    MacMode,
    ExchangeWasd,
    WinLock,
    SixKeyRollover,
    AntiAccidental,
    Extreme,
    FnInversion,
    GameMode,
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
            Self::FnInversion => cfg.fn_inversion,
            Self::GameMode => cfg.game_mode,
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
            Self::FnInversion => cfg.fn_inversion = value,
            Self::GameMode => cfg.game_mode = value,
        }
    }
}

impl AgnoPeriphApp {
    /// Stages a keyboard-config edit on top of any earlier unsaved ones.
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
        edit(&mut cfg);
        self.edit_draft(
            EditKey::Change(ChangeId::KeyboardConfig),
            false,
            move |draft, _| draft.keyboard_config = Some(cfg),
            cx,
        );
    }
}

/// What a toggle row shows and whether the keyboard offers it.
#[derive(Clone, Copy)]
struct ToggleRow {
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    kind: ConfigToggle,
    is_supported: bool,
}

/// The staged and the stored settings, so rows can mark what isn't applied.
#[derive(Clone, Copy)]
struct Config {
    staged: KeyboardConfigSnapshot,
    saved: KeyboardConfigSnapshot,
    busy: bool,
}

fn toggle_panel(
    id: &'static str,
    title: &'static str,
    rows: Vec<ToggleRow>,
    config: Config,
    cx: &mut Context<AgnoPeriphApp>,
) -> Stateful<Div> {
    let mut group = panel(id, cx).child(panel_head(title));
    for (ix, row) in rows.into_iter().enumerate() {
        group = group.child(toggle_row(row, ix == 0, config, cx));
    }
    group
}

fn toggle_row(
    row: ToggleRow,
    is_first: bool,
    config: Config,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let ToggleRow {
        id,
        title,
        detail,
        kind,
        is_supported,
    } = row;
    let description = if is_supported {
        detail
    } else {
        "Not supported by this keyboard."
    };
    let switch = MarkedSwitch::new(id)
        .checked(kind.get(config.staged))
        .disabled(config.busy || !is_supported)
        .accessibility_label(title)
        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
            let checked = *checked;
            this.write_keyboard_config(|c| kind.set(c, checked), cx);
        }));
    let is_pending = kind.get(config.saved) != kind.get(config.staged);
    device_setting_row(
        title,
        Some(description.into()),
        switch,
        is_first,
        is_pending,
        cx,
    )
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    device: &DeviceSnapshot,
    workspace_width: f32,
    cx: &mut Context<AgnoPeriphApp>,
) -> AnyElement {
    let Some(state) = device.keyboard_config.as_ref() else {
        return panel("keyboard-config-panel", cx)
            .child(panel_head("Advanced"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Advanced settings couldn’t be read."),
            )
            .into_any_element();
    };
    let caps = state.capabilities.clone();
    let config = Config {
        staged: state.snapshot,
        saved: app
            .selected_raw()
            .and_then(|d| d.keyboard_config.as_ref())
            .map_or(state.snapshot, |kc| kc.snapshot),
        busy: app.is_busy() || app.is_loading(),
    };
    let row = |id, title, detail, kind, is_supported| ToggleRow {
        id,
        title,
        detail,
        kind,
        is_supported,
    };

    let layout = toggle_panel(
        "config-layout",
        "Key layout",
        vec![
            row(
                "config-mac-mode",
                "Mac mode",
                "Swap the Windows and Alt keys to match a Mac layout.",
                ConfigToggle::MacMode,
                caps.supports_mac_mode,
            ),
            row(
                "config-exchange-wasd",
                "Exchange WASD and arrows",
                "WASD sends the arrow keys and the arrows send WASD.",
                ConfigToggle::ExchangeWasd,
                caps.supports_wasd_swap,
            ),
        ],
        config,
        cx,
    );
    // Function-key rows only appear on keyboards that have them.
    let mut function_rows = Vec::new();
    if caps.supports_fn_inversion {
        function_rows.push(row(
            "config-fn-inversion",
            "Fn key inversion",
            "F1–F12 send media and brightness keys; hold Fn for F1–F12.",
            ConfigToggle::FnInversion,
            true,
        ));
    }
    if caps.supports_game_mode {
        function_rows.push(row(
            "config-game-mode",
            "Game mode",
            "Block keys the keyboard marks for game mode, such as the Windows key.",
            ConfigToggle::GameMode,
            true,
        ));
    }
    let function_keys = (!function_rows.is_empty()).then(|| {
        toggle_panel(
            "config-function",
            "Function keys",
            function_rows,
            config,
            cx,
        )
    });
    let conflict = toggle_panel(
        "config-conflict",
        "Key conflict",
        vec![
            row(
                "config-win-lock",
                "Windows key lock",
                "Disable the Windows keys so they can’t interrupt a game.",
                ConfigToggle::WinLock,
                caps.supports_win_lock,
            ),
            row(
                "config-six-key",
                "6-key rollover",
                "Report at most six keys at once for older BIOS and KVM switches.",
                ConfigToggle::SixKeyRollover,
                caps.supports_six_key_rollover,
            ),
            row(
                "config-anti-accidental",
                "Anti-accidental touch",
                "Ignore very light brushes against the keys.",
                ConfigToggle::AntiAccidental,
                caps.supports_anti_accidental_touch,
            ),
        ],
        config,
        cx,
    );
    let extreme = toggle_row(
        row(
            "config-extreme",
            "Extreme mode (0.01 mm)",
            "Use 0.01 mm actuation and Rapid Trigger steps instead of 0.02 mm.",
            ConfigToggle::Extreme,
            caps.supports_extreme_mode,
        ),
        true,
        config,
        cx,
    );
    let polling = polling_row(caps.supported_polling_rates, config, cx);
    let precision = panel("config-polling", cx)
        .child(panel_head("Precision and polling"))
        .child(extreme)
        .child(polling);

    let first = v_flex().gap(dp(GAP)).child(layout).children(function_keys);
    let second = v_flex().gap(dp(GAP)).child(conflict).child(precision);
    let columns = if workspace_width >= MIN_COLUMN_WIDTH * 2.0 + GAP {
        h_flex()
            .items_start()
            .gap(dp(GAP))
            .child(first.flex_1().min_w_0())
            .child(second.flex_1().min_w_0())
    } else {
        v_flex().gap(dp(GAP)).child(first).child(second)
    };
    div()
        .id("keyboard-config-panel")
        .child(columns)
        .into_any_element()
}

fn polling_row(
    mut rates: Vec<PollingRateHz>,
    config: Config,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    rates.sort_by_key(|r| r.as_hz());
    let mut choices = segmented(cx).id("config-polling-rate");
    for rate in rates {
        let label = if rate.as_hz() >= 1000 {
            format!("{}K", rate.as_hz() / 1000)
        } else {
            rate.as_hz().to_string()
        };
        choices = choices.child(
            segment(
                Button::new(("config-rate", u64::from(rate.as_hz()))),
                label,
                config.staged.polling_rate == rate,
                cx,
            )
            .px(dp(9.))
            .disabled(config.busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.write_keyboard_config(|c| c.polling_rate = rate, cx);
            })),
        );
    }
    device_setting_row(
        "Polling rate",
        Some("Reports per second, in Hz.".into()),
        choices,
        false,
        config.saved.polling_rate != config.staged.polling_rate,
        cx,
    )
}
