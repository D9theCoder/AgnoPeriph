//! Collapsible live HID packet inspector docked below the workspace.
//!
//! Packets are pre-formatted once when the trace log changes and rendered through a
//! virtualized [`uniform_list`] so hundreds of frames stay within high-refresh frame budgets.

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

/// Newest frames retained for display; older ones stay in the bounded service log.
const VISIBLE_ROWS: usize = 200;

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
