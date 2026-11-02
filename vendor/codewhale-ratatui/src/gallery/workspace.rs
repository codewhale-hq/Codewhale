//! Workspace fixtures: transcript, input ledge, tool previews, agents and a fleet.
//!
//! All content is illustrative caller data. No fixture claims a provider
//! call, account state, cost, deployment or successful release.

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{
    AgentCard, Composer, Fleet, KeyHint, KeyHints, Message, Paint, Role, State, Theme, ToolCard,
    glyphs,
};

fn message(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let first = Message::native("Show every component in the README.")
        .role(Role::Accent)
        .marker(glyphs::USER);
    let first_height = first.height(area.width, theme).min(area.height);
    first.paint(
        Rect {
            height: first_height,
            ..area
        },
        buf,
        theme,
    );
    let rest = Rect {
        y: area
            .y
            .saturating_add(first_height.saturating_add(1).min(area.height)),
        height: area.height.saturating_sub(first_height.saturating_add(1)),
        ..area
    };
    Message::native( "The gallery uses the same components as the library.\n\nEach preview includes light, dark and ASCII treatments.")
        .paint(rest, buf, theme);
}

fn composer(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let hints = KeyHints::new(vec![
        KeyHint::new("Enter", "send"),
        KeyHint::new("Shift+Enter", "new line"),
        KeyHint::new("Ctrl+O", "details"),
    ]);
    Composer::new("Show the new transcript, tool cards and fleet rail.\nKeep the previews connected to the public APIs.")
        .context("codewhale-ratatui / workspace")
        .hints(&hints)
        .focused(true)
        .paint(area, buf, theme);
}

fn tool_card(area: Rect, buf: &mut Buffer, theme: &Theme) {
    ToolCard::new("verify", "cargo test --test workspace", State::Done)
        .elapsed("0.4s")
        .output("transcript preserves authored lines: checked\noutput preview counts hidden lines: checked\nfleet retains each agent's identity: checked")
        .omitted_lines(2)
        .paint(area, buf, theme);
}

fn agent_card(area: Rect, buf: &mut Buffer, theme: &Theme) {
    AgentCard::new("Terminal review", State::NeedsYou)
        .role("Reviewer")
        .route("Local / caller-selected model")
        .task("Choose translated state words.\nKeep the preview aligned with the host's locale.")
        .elapsed("48s")
        .paint(area, buf, theme);
}

fn fleet(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Fleet::new(vec![
        AgentCard::new("Transcript", State::Working)
            .role("Builder")
            .route("Local / caller-selected model")
            .task("Extract reusable message and tool surfaces.")
            .elapsed("2m 10s"),
        AgentCard::new("Accessibility", State::NeedsYou)
            .role("Reviewer")
            .task("Choose the host's localized state words.")
            .elapsed("48s"),
        AgentCard::new("Gallery", State::Done)
            .role("Verifier")
            .task("Review illustrative terminal profiles.")
            .elapsed("1m 24s"),
    ])
    .paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "message",
            width: 64,
            height: 9,
            draw: message,
        },
        Entry {
            name: "composer",
            width: 64,
            height: 5,
            draw: composer,
        },
        Entry {
            name: "tool-card",
            width: 64,
            height: 9,
            draw: tool_card,
        },
        Entry {
            name: "agent-card",
            width: 64,
            height: 8,
            draw: agent_card,
        },
        Entry {
            name: "fleet",
            width: 64,
            height: 14,
            draw: fleet,
        },
    ]
}
