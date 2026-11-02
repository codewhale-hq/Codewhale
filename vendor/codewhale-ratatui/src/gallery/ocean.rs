//! Fixed caller phases over the actual Codewhale terminal water column.
//! Context values and completion clocks are illustrative inputs, not receipts.

use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::Entry;
use crate::{
    Depth, Heading, Message, MotionMode, Paint, Panel, Role, State, StatusMark, Theme,
    ocean::{OceanCausticFacts, OceanColumn, OceanPaintFacts, OceanPhase, ocean_semantic_surfaces},
};

fn band(area: Rect, top: u16, height: u16) -> Rect {
    let top = top.min(area.height);
    Rect::new(
        area.x,
        area.y.saturating_add(top),
        area.width,
        height.min(area.height.saturating_sub(top)),
    )
}

fn column(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    Heading::new("Codewhale / native ocean")
        .meta("Working / fixture")
        .paint(band(area, 0, 1), buf, theme);
    Message::new("You", "Keep the code and the decision clear.")
        .role(Role::Accent)
        .paint(band(area, 2, 3), buf, theme);
    Message::new(
        "Codewhale",
        "One continuous column behind the conversation.",
    )
    .paint(band(area, 6, 3), buf, theme);

    let selected = band(area, 10, 1);
    buf.set_style(selected, theme.bg(Role::Selected));
    Heading::new("Selected / src/ocean.rs")
        .sub()
        .paint(selected, buf, theme);
    let surface = Panel::new(Depth::Raised)
        .title("Semantic surface / own ground")
        .draw(band(area, 12, 4), buf, theme);
    StatusMark::new(State::Working).paint(surface, buf, theme);

    OceanColumn::new(Duration::from_millis(22_500), MotionMode::Full)
        .phase(OceanPhase::Working)
        .apply(area, buf, theme);
}

fn phases_with(area: Rect, buf: &mut Buffer, theme: &Theme, motion: MotionMode) {
    let area = area.intersection(buf.area);
    let phases = [
        (OceanPhase::Working, State::Working, "Working / quiet light"),
        (
            OceanPhase::Verifying,
            State::Working,
            "Verifying / quiet light",
        ),
        (
            OceanPhase::Approval,
            State::NeedsYou,
            "Approval / steady warm water",
        ),
        (OceanPhase::Failed, State::Failed, "Failed / steady tint"),
        (
            OceanPhase::Done,
            State::Done,
            if motion.animates() {
                "Done / completion at 320 ms"
            } else {
                "Done / static completion"
            },
        ),
    ];
    for (index, (phase, state, label)) in phases.into_iter().enumerate() {
        let strip = band(area, index as u16 * 4, 4);
        Heading::new(label).paint(band(strip, 0, 1), buf, theme);
        StatusMark::new(state).paint(band(strip, 1, 1), buf, theme);
        OceanColumn::new(Duration::from_millis(22_500), motion)
            .phase(phase)
            .completion_elapsed(Duration::from_millis(320))
            .viewport(area)
            .apply(strip, buf, theme);
    }
}

fn phases(area: Rect, buf: &mut Buffer, theme: &Theme) {
    phases_with(area, buf, theme, MotionMode::Full);
}

fn reduced(area: Rect, buf: &mut Buffer, theme: &Theme) {
    phases_with(area, buf, theme, MotionMode::Reduced);
}

fn context(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    for (index, percent) in [0, 50, 100].into_iter().enumerate() {
        let left = (u32::from(area.width) * index as u32 / 3) as u16;
        let right = (u32::from(area.width) * (index as u32 + 1) / 3) as u16;
        let column = Rect {
            x: area.x.saturating_add(left),
            width: right.saturating_sub(left),
            ..area
        };
        Heading::new(format!("{percent}% context")).paint(band(column, 0, 1), buf, theme);
        Message::new("Fixture", "Fullness raises the abyss.").paint(
            band(column, 3, column.height.saturating_sub(3)),
            buf,
            theme,
        );
        OceanColumn::new(Duration::ZERO, MotionMode::Still)
            .context_percent(percent)
            .apply(column, buf, theme);
    }
}

// This native path supplies presentation facts from a real rendered pane.
// It keeps raw ink/style facts and semantic padding while sharing cached rows
// between the base water and caustic passes. No terminal evidence is invented.
fn native_guarded(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let base = theme.bg(Role::Background);
    let mut rows = vec![Line::from(""); 4];
    rows.push(Line::from(vec![Span::styled(
        "Native / guarded ocean",
        theme.fg(Role::Foreground),
    )]));
    rows.push(Line::from(vec![Span::styled(
        "[Selected] source keeps its own ground",
        theme.bg(Role::Selected).patch(theme.fg(Role::Foreground)),
    )]));
    rows.push(Line::styled("", base));
    rows.push(Line::from(vec![Span::styled(
        "Explicit blank padding is semantic",
        base.patch(theme.fg(Role::Foreground)),
    )]));
    rows.push(Line::from(vec![Span::styled(
        "Reversed surface",
        theme.fg(Role::Foreground).add_modifier(Modifier::REVERSED),
    )]));
    rows.push(Line::styled(
        "Caller phase: Working / fixture",
        theme.fg(Role::Dim),
    ));
    Paragraph::new(rows.clone()).render(area, buf);
    let column = OceanColumn::new(Duration::from_millis(22_500), MotionMode::Full)
        .phase(OceanPhase::Working)
        .viewport(area);
    let samples: Vec<_> = (area.y..area.bottom())
        .map_while(|y| column.color_at_y(y, area, theme))
        .collect();
    let protected = ocean_semantic_surfaces(&rows, area, str::width);
    let paint = OceanPaintFacts {
        ground: base.bg.unwrap_or(ratatui::style::Color::Reset),
        sample_top: area.y,
        samples: &samples,
        protected: &protected,
    };
    column.apply_native(area, buf, theme, &paint, |cell, _| cell.fg);
    column.apply_caustics(
        area,
        buf,
        theme,
        &OceanCausticFacts {
            paint,
            elapsed: Duration::from_millis(480),
            band_rows: 4,
        },
    );
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "ocean-column",
            width: 80,
            height: 16,
            draw: column,
        },
        Entry {
            name: "ocean-phases",
            width: 80,
            height: 20,
            draw: phases,
        },
        Entry {
            name: "ocean-context",
            width: 80,
            height: 12,
            draw: context,
        },
        Entry {
            name: "ocean-reduced",
            width: 80,
            height: 20,
            draw: reduced,
        },
        Entry {
            name: "ocean-native-guarded",
            width: 80,
            height: 16,
            draw: native_guarded,
        },
    ]
}
