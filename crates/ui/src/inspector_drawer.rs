//! Collapsible live HID packet inspector docked below the workspace.
//!
//! Packets are pre-formatted once when the trace log changes and rendered through a
//! virtualized [`uniform_list`] so hundreds of frames stay within high-refresh frame budgets.

use std::fmt::Write as _;
use std::sync::Arc;

use agnoperiph_transport::{PacketDirection, PacketOutcome, PacketTraceEntry};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, Theme,
    button::{Button, ButtonVariants},
    h_flex,
    tag::Tag,
    v_flex,
};
use gpui_kit::*;

use crate::app::AgnoPeriphApp;
use crate::backend::DeviceSnapshot;

/// Newest frames retained for display; older ones stay in the bounded service log.
const VISIBLE_ROWS: usize = 200;
/// Maximum recent frames included in a copied diagnostic report (oldest -> newest).
const DIAGNOSTIC_TRACE_LIMIT: usize = 120;

/// Builds a self-contained Markdown diagnostic report (app info, active error, selected
/// device state, warnings, connected devices, and recent TX/RX HID hex frames) ready to
/// paste into an issue report or replay in `MockHidTransport`.
///
/// Raw serial numbers and OS device paths are redacted so personal identifiers are not
/// leaked when sharing diagnostics publicly.
pub(crate) fn build_diagnostic_report(app: &AgnoPeriphApp) -> String {
    let mut out = String::with_capacity(2048);
    let _ = writeln!(out, "## AgnoPeriph Diagnostic Report");
    let _ = writeln!(
        out,
        "- App: AgnoPeriph v{} ({} / {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let _ = writeln!(
        out,
        "- Mode: {}",
        if app.is_dry_run() { "dry-run" } else { "live" }
    );
    let _ = writeln!(out, "- Connected devices: {}", app.devices().len());

    if let Some(banner) = app.banner() {
        let _ = writeln!(out, "\n### Active Error");
        let _ = writeln!(out, "- Title: {}", banner.title());
        let _ = writeln!(out, "- Message: {}", banner.message());
    }

    if let Some(selected) = app.selected_raw() {
        let _ = writeln!(out, "\n### Selected Device");
        write_device_details(&mut out, selected);
    }

    let selected_id = app.selected_raw().map(DeviceSnapshot::id);
    let others: Vec<&DeviceSnapshot> = app
        .devices()
        .iter()
        .filter(|d| Some(d.id()) != selected_id)
        .collect();
    if !others.is_empty() {
        let _ = writeln!(out, "\n### Other Connected Devices");
        for dev in others {
            let fw = dev.info.firmware_version.as_deref().unwrap_or("unknown");
            let _ = writeln!(
                out,
                "- {} ({}, {}, {:04X}:{:04X}, fw={fw}, warnings={})",
                dev.info.model_name,
                dev.info.vendor,
                dev.info.connection,
                dev.info.vid,
                dev.info.pid,
                dev.warnings.len(),
            );
        }
    }

    let packets = app.packets();
    let start = packets.len().saturating_sub(DIAGNOSTIC_TRACE_LIMIT);
    let slice = &packets[start..];
    let _ = writeln!(
        out,
        "\n### Recent HID Packet Trace ({} of {} frames, oldest -> newest)",
        slice.len(),
        packets.len(),
    );
    if slice.is_empty() {
        let _ = writeln!(out, "(no HID packets recorded yet)");
    } else {
        let _ = writeln!(out, "```text");
        for entry in slice {
            let dir = match entry.direction {
                PacketDirection::HostToDevice => "TX",
                PacketDirection::DeviceToHost => "RX",
            };
            let hex = entry.hex_spaced();
            match &entry.note {
                Some(note) => {
                    let _ = writeln!(
                        out,
                        "#{:04} {dir} {:7} [{:11}] ({}) {hex}  // {note}",
                        entry.sequence, entry.kind, entry.outcome, entry.device_label,
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "#{:04} {dir} {:7} [{:11}] ({}) {hex}",
                        entry.sequence, entry.kind, entry.outcome, entry.device_label,
                    );
                }
            }
        }
        let _ = writeln!(out, "```");
    }

    out
}

fn write_device_details(out: &mut String, device: &DeviceSnapshot) {
    let info = &device.info;
    let _ = writeln!(out, "- Model: {} ({})", info.model_name, info.vendor);
    let _ = writeln!(
        out,
        "- Connection: {} (VID 0x{:04X}, PID 0x{:04X})",
        info.connection, info.vid, info.pid,
    );
    let _ = writeln!(
        out,
        "- Firmware: {}",
        info.firmware_version.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(
        out,
        "- Serial: {}",
        if info.serial_number.is_some() {
            "present (redacted)"
        } else {
            "none"
        }
    );

    let caps = device.capabilities;
    let mut cap_names = Vec::new();
    if caps.has_pointer() {
        cap_names.push("pointer");
    }
    if caps.has_hall_effect() {
        cap_names.push("hall_effect");
    }
    if caps.has_rgb() {
        cap_names.push("rgb");
    }
    if caps.has_battery() {
        cap_names.push("battery");
    }
    if caps.has_profiles() {
        cap_names.push("profiles");
    }
    if caps.has_keymap() {
        cap_names.push("keymap");
    }
    if caps.has_macros() {
        cap_names.push("macros");
    }
    if caps.has_keyboard_config() {
        cap_names.push("keyboard_config");
    }
    let _ = writeln!(
        out,
        "- Capabilities: {}",
        if cap_names.is_empty() {
            "none".to_owned()
        } else {
            cap_names.join(", ")
        }
    );

    if let Some(battery) = &device.battery {
        let pct = battery
            .percentage
            .map_or_else(|| "unknown".to_owned(), |p| format!("{p}%"));
        let mv = battery
            .voltage_mv
            .map_or_else(String::new, |v| format!(", {v} mV"));
        let _ = writeln!(out, "- Battery: {pct} ({:?}{mv})", battery.state);
    }

    if let Some(pointer) = &device.pointer {
        let snap = &pointer.snapshot;
        let caps = &pointer.capabilities;
        let stages: Vec<String> = snap
            .dpi_stages
            .iter()
            .map(|s| s.x.value().to_string())
            .collect();
        let rates: Vec<String> = caps
            .supported_polling_rates
            .iter()
            .map(|r| r.as_hz().to_string())
            .collect();
        let _ = writeln!(
            out,
            "- Pointer: active_stage={}, stages=[{}], rate={} Hz, dpi_bounds={}..={} step {}, supported_rates=[{}]",
            snap.active_dpi_stage,
            stages.join(", "),
            snap.polling_rate.as_hz(),
            caps.dpi_min,
            caps.dpi_max,
            caps.dpi_step,
            rates.join(", "),
        );
    }

    if let Some(profiles) = &device.profiles {
        let snap = &profiles.snapshot;
        let slots: Vec<String> = snap
            .profiles
            .iter()
            .map(|p| {
                format!(
                    "{}:{}",
                    p.slot.display_number(),
                    if p.is_enabled() {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )
            })
            .collect();
        let _ = writeln!(
            out,
            "- Profiles: active={}, mode={}, slot_count={}, slots=[{}]",
            snap.active_slot,
            snap.mode,
            profiles.capabilities.slot_count,
            slots.join(", "),
        );
    }

    if let Some(he) = &device.hall_effect {
        let snap = &he.snapshot;
        let _ = writeln!(
            out,
            "- Hall Effect: keys={}, socd_pairs={}, rapid_switch_pairs={}, dks={}, mod_tap={}, toggle={}",
            snap.keys.len(),
            snap.socd_pairs.len(),
            snap.rapid_switch_pairs.len(),
            snap.dks_bindings.len(),
            snap.mod_tap_bindings.len(),
            snap.toggle_bindings.len(),
        );
    }

    if let Some(rgb) = &device.rgb {
        let _ = writeln!(
            out,
            "- RGB: zones={}, per_key={}",
            rgb.snapshot.zones.len(),
            rgb.capabilities.supports_per_key_rgb,
        );
    }

    if !device.warnings.is_empty() {
        let _ = writeln!(out, "- Warnings:");
        for w in &device.warnings {
            let _ = writeln!(out, "  - {}: {}", w.capability, w.message);
        }
    }
}

/// Pre-formatted packet row cached on [`AgnoPeriphApp`] to avoid per-frame string allocations.
#[derive(Debug, Clone)]
pub(crate) struct FormattedPacket {
    sequence: u64,
    sequence_label: SharedString,
    direction: &'static str,
    kind: SharedString,
    outcome: PacketOutcome,
    outcome_label: SharedString,
    payload: SharedString,
    device_label: SharedString,
    aria_label: SharedString,
}

/// Formats packets newest-first for virtualized rendering.
pub(crate) fn format_packets(packets: &[PacketTraceEntry]) -> Vec<FormattedPacket> {
    packets
        .iter()
        .rev()
        .take(VISIBLE_ROWS)
        .map(|entry| {
            let direction = match entry.direction {
                PacketDirection::HostToDevice => "TX",
                PacketDirection::DeviceToHost => "RX",
            };
            let hex = entry.hex_spaced();
            let payload: SharedString = match &entry.note {
                Some(note) => format!("{hex}  ·  {note}").into(),
                None => hex.into(),
            };
            FormattedPacket {
                sequence: entry.sequence,
                sequence_label: entry.sequence.to_string().into(),
                direction,
                kind: entry.kind.to_string().into(),
                outcome: entry.outcome,
                outcome_label: entry.outcome.to_string().into(),
                payload,
                device_label: entry.device_label.clone().into(),
                aria_label: format!(
                    "{direction} {} {} packet {}",
                    entry.kind, entry.outcome, entry.sequence
                )
                .into(),
            }
        })
        .collect()
}

pub(crate) fn render(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let total = app.packets().len();
    let suppressed = app.suppressed_packets();
    let rows = Arc::clone(app.formatted_packets());
    let theme = cx.theme();

    let header = h_flex()
        .flex_shrink_0()
        .h_10()
        .px_4()
        .gap_3()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("HID packets"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(packet_summary(total, suppressed)),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new("inspector-copy-diagnostics")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Copy)
                        .label("Copy diagnostics")
                        .tooltip(
                            "Copy device state, warnings, and recent HID hex trace to clipboard",
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.copy_diagnostics(window, cx);
                        })),
                )
                .child(
                    Button::new("inspector-clear")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Delete)
                        .label("Clear")
                        .disabled(total == 0)
                        .on_click(cx.listener(|this, _, _, cx| this.clear_packets(cx))),
                )
                .child(
                    Button::new("inspector-close")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Hide packet inspector")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_inspector(cx))),
                ),
        );

    let body = if rows.is_empty() {
        div()
            .p_4()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("No packets yet. Frames appear here as devices are read and changed.")
            .into_any_element()
    } else {
        let count = rows.len();
        uniform_list("inspector-rows", count, move |range, _, cx| {
            let theme = cx.theme();
            range.map(|ix| render_row(&rows[ix], theme)).collect()
        })
        .flex_1()
        .min_h_0()
        .size_full()
        .py_1()
        .into_any_element()
    };

    v_flex()
        .id("inspector")
        .flex_shrink_0()
        .h_56()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(header)
        .child(body)
}

fn packet_summary(total: usize, suppressed: usize) -> String {
    let frames = if total == 1 { "frame" } else { "frames" };
    if suppressed == 0 {
        format!("{total} {frames}")
    } else {
        format!("{total} {frames}, {suppressed} held back by dry run")
    }
}

fn outcome_tag(outcome: PacketOutcome, label: SharedString) -> Tag {
    let tag = match outcome {
        PacketOutcome::Transmitted | PacketOutcome::Received => Tag::secondary(),
        PacketOutcome::DryRunSuppressed => Tag::info(),
        PacketOutcome::ValidationFailed => Tag::danger(),
    };
    tag.xsmall().child(label)
}

fn render_row(entry: &FormattedPacket, theme: &Theme) -> impl IntoElement + use<> {
    let mono = theme.mono_font_family.clone();

    h_flex()
        .id(("packet", entry.sequence))
        .aria_label(entry.aria_label.clone())
        .h_6()
        .px_4()
        .gap_3()
        .items_center()
        .text_xs()
        .child(
            div()
                .w_12()
                .flex_shrink_0()
                .text_right()
                .font_family(mono.clone())
                .text_color(theme.muted_foreground)
                .child(entry.sequence_label.clone()),
        )
        .child(
            div()
                .w_6()
                .flex_shrink_0()
                .font_family(mono.clone())
                .child(entry.direction),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(entry.kind.clone()),
        )
        .child(
            div()
                .w_20()
                .flex_shrink_0()
                .child(outcome_tag(entry.outcome, entry.outcome_label.clone())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(mono)
                .child(entry.payload.clone()),
        )
        .child(
            div()
                .w_48()
                .flex_shrink_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(entry.device_label.clone()),
        )
        .test_support()
}
