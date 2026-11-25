//! Collapsible live HID packet inspector docked below the workspace.

use agnoperiph_transport::{PacketDirection, PacketOutcome, PacketTraceEntry};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, Theme,
    button::{Button, ButtonVariants},
    h_flex,
    scroll::ScrollableElement,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AgnoPeriphApp;

/// Newest frames rendered; older ones stay in the bounded service log.
const VISIBLE_ROWS: usize = 200;

pub(crate) fn render(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> impl IntoElement + use<> {
    let packets = app.packets();
    let theme = cx.theme();
    let rows: Vec<AnyElement> = packets
        .iter()
        .rev()
        .take(VISIBLE_ROWS)
        .map(|entry| render_row(entry, theme).into_any_element())
        .collect();
    let suppressed = packets
        .iter()
        .filter(|p| p.outcome == PacketOutcome::DryRunSuppressed)
        .count();

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
                        .child(packet_summary(packets.len(), suppressed)),
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
                        .disabled(packets.is_empty())
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
        v_flex()
            .id("inspector-rows")
            .flex_1()
            .min_h_0()
            .py_1()
            .children(rows)
            .overflow_y_scrollbar()
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

fn outcome_tag(outcome: PacketOutcome) -> Tag {
    let tag = match outcome {
        PacketOutcome::Transmitted | PacketOutcome::Received => Tag::secondary(),
        PacketOutcome::DryRunSuppressed => Tag::info(),
        PacketOutcome::ValidationFailed => Tag::danger(),
    };
    tag.xsmall().child(outcome.to_string())
}

fn render_row(entry: &PacketTraceEntry, theme: &Theme) -> impl IntoElement + use<> {
    let direction = match entry.direction {
        PacketDirection::HostToDevice => "TX",
        PacketDirection::DeviceToHost => "RX",
    };
    let mono = theme.mono_font_family.clone();

    h_flex()
        .id(("packet", entry.sequence))
        .aria_label(format!(
            "{direction} {} {} packet {}",
            entry.kind, entry.outcome, entry.sequence
        ))
        .px_4()
        .py_0p5()
        .gap_3()
        .items_start()
        .text_xs()
        .child(
            div()
                .w_12()
                .flex_shrink_0()
                .text_right()
                .font_family(mono.clone())
                .text_color(theme.muted_foreground)
                .child(entry.sequence.to_string()),
        )
        .child(
            div()
                .w_6()
                .flex_shrink_0()
                .font_family(mono.clone())
                .child(direction),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(entry.kind.to_string()),
        )
        .child(
            div()
                .w_20()
                .flex_shrink_0()
                .child(outcome_tag(entry.outcome)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().font_family(mono).child(entry.hex_spaced()))
                .when_some(entry.note.clone(), |this, note| {
                    this.child(div().text_color(theme.muted_foreground).child(note))
                }),
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
