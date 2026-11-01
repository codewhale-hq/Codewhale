//! Codewhale's native composer footer rows, with representative caller data.

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{MetricKind, MetricSegment, MetricsLine, Paint, PostureBar, PostureFact, Role, Theme};

pub(crate) fn working_posture() -> PostureBar<'static> {
    PostureBar::new("ask")
        .permission_key("Shift+Tab to change")
        .mode_ink("work", crate::TuiInk::ModeWork)
        .mode_key("Tab")
        .turn_clock("working 1m 15s", Role::Live)
        .counts(vec![PostureFact::new("2 agents", Role::Live)])
        .session_clock("worked 41m 12s", Role::Live)
        .hint("Esc to interrupt", Role::Hint)
        .context_percent(61)
}

pub(crate) fn working_metrics() -> MetricsLine<'static> {
    MetricsLine::new(vec![
        MetricSegment::new(MetricKind::Model, "", "deepseek-v4").role(Role::Accent),
        MetricSegment::new(MetricKind::Context, "ctx", "61%").role(Role::Accent),
        MetricSegment::new(MetricKind::Cost, "", "$0.42"),
        MetricSegment::new(MetricKind::Ttft, "ttft", "400ms"),
        MetricSegment::new(MetricKind::Rate, "", "38 tok/s"),
        MetricSegment::new(MetricKind::OutputTokens, "↓", "1.2K"),
    ])
    .help_hint("/help")
}

fn posture(area: Rect, buf: &mut Buffer, theme: &Theme) {
    working_posture().paint(area, buf, theme);
}

fn cap(area: Rect, buf: &mut Buffer, theme: &Theme) {
    working_posture()
        .context_percent(83)
        .paint(area, buf, theme);
}

fn compact_posture(area: Rect, buf: &mut Buffer, theme: &Theme) {
    working_posture()
        .compact(true)
        .right("/rc connected", Role::Accent)
        .paint(area, buf, theme);
}

fn metrics(area: Rect, buf: &mut Buffer, theme: &Theme) {
    working_metrics().paint(area, buf, theme);
}

fn compact_metrics(area: Rect, buf: &mut Buffer, theme: &Theme) {
    working_metrics().compact(true).paint(area, buf, theme);
}

fn startup_metrics(area: Rect, buf: &mut Buffer, theme: &Theme) {
    MetricsLine::new(vec![
        MetricSegment::new(MetricKind::Model, "", "model not connected").role(Role::Attention),
        MetricSegment::new(MetricKind::Context, "ctx", "0%").role(Role::Accent),
    ])
    .help_hint("/help")
    .paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "posture-bar",
            width: 112,
            height: 1,
            draw: posture,
        },
        Entry {
            name: "posture-narrow",
            width: 40,
            height: 1,
            draw: posture,
        },
        Entry {
            name: "posture-context-cap",
            width: 112,
            height: 1,
            draw: cap,
        },
        Entry {
            name: "posture-compact",
            width: 96,
            height: 1,
            draw: compact_posture,
        },
        Entry {
            name: "metrics-line",
            width: 112,
            height: 1,
            draw: metrics,
        },
        Entry {
            name: "metrics-narrow",
            width: 32,
            height: 1,
            draw: metrics,
        },
        Entry {
            name: "metrics-compact",
            width: 96,
            height: 1,
            draw: compact_metrics,
        },
        Entry {
            name: "metrics-startup",
            width: 80,
            height: 1,
            draw: startup_metrics,
        },
    ]
}
