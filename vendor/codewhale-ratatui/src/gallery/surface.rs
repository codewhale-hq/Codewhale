//! Gallery: depth (panels), the horizon rule, a dialog and a sheet.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use super::Entry;
use crate::{
    Depth, Dialog, DialogWidth, HorizonRule, KeyHint, KeyHints, Paint, Panel, Role, Sheet,
    SheetEdge, Theme, glyphs,
};

fn depths(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let w = area.width / 4;
    for (i, (depth, title, body)) in [
        (Depth::Deep, "Deep", "Put away"),
        (Depth::Stage, "Stage", "Where work sits"),
        (Depth::Raised, "Raised", "A card"),
        (Depth::Overlay, "Overlay", "A decision"),
    ]
    .into_iter()
    .enumerate()
    {
        let rect = Rect {
            x: area.x + w * i as u16,
            width: w,
            ..area
        };
        let inner = Panel::new(depth).title(title).draw(rect, buf, theme);
        Line::from(Span::styled(body, theme.fg(Role::Muted))).render(inner, buf);
    }
}

fn horizon(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let row = |n: u16| {
        Rect {
            y: area.y.saturating_add(n),
            height: 1,
            ..area
        }
        .intersection(area)
    };
    Line::from(Span::styled(
        "Edited summary.md",
        theme.fg(Role::Foreground),
    ))
    .render(row(0), buf);
    HorizonRule::new()
        .aside("12% of context used")
        .paint(row(1), buf, theme);
    Line::from(vec![
        Span::styled(
            format!("{} ", glyphs::pick("›", theme.ascii())),
            theme.fg(Role::Accent),
        ),
        Span::styled("Ask Codewhale to do something", theme.fg(Role::Muted)),
    ])
    .render(row(2), buf);
}

/// Work on the stage, for a dialog or a sheet to sit over.
fn backdrop(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let lines = [
        "Edited summary.md",
        "Ran cargo test: 212 passed",
        "Read crates/tui/src/tui/views/mod.rs",
        "Searched the web for ratatui tabs",
        "Edited settings.rs",
    ];
    for (row, line) in lines
        .iter()
        .cycle()
        .take(usize::from(area.height))
        .enumerate()
    {
        buf.set_stringn(
            area.x,
            area.y + row as u16,
            line,
            usize::from(area.width),
            theme.fg(Role::Muted),
        );
    }
}

fn dialog(area: Rect, buf: &mut Buffer, theme: &Theme) {
    backdrop(area, buf, theme);
    let hints = KeyHints::new(vec![
        KeyHint::new("y", "stop"),
        KeyHint::new("n", "keep running"),
    ]);
    let body = Dialog::new()
        .title("Stop the running workflow?")
        .width(DialogWidth::Narrow)
        .body_rows(2)
        .hints(&hints)
        .draw(area, buf, theme);
    Paragraph::new("2 agents are still working. Their edits so far are kept.")
        .style(theme.fg(Role::Foreground))
        .wrap(Wrap { trim: true })
        .render(body, buf);
}

fn sheet(area: Rect, buf: &mut Buffer, theme: &Theme) {
    backdrop(area, buf, theme);
    let arrows = super::arrows(theme);
    let hints = KeyHints::new(vec![
        KeyHint::new(arrows, "move"),
        KeyHint::new("Enter", "change"),
        KeyHint::new("Esc", "close"),
    ]);
    let body = Sheet::new()
        .edge(SheetEdge::Bottom)
        .title("Settings")
        .aside("3 changed")
        .size(10)
        .max_size(9)
        .hints(&hints)
        .draw(area, buf, theme);
    Paragraph::new(vec![
        Line::from("Theme       Shoreline"),
        Line::from("Motion      Reduced"),
        Line::from("Tool detail Collapsed"),
    ])
    .style(theme.fg(Role::Foreground))
    .render(body, buf);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "dialog",
            width: 80,
            height: 12,
            draw: dialog,
        },
        Entry {
            name: "sheet",
            width: 80,
            height: 12,
            draw: sheet,
        },
        Entry {
            name: "depth",
            width: 80,
            height: 6,
            draw: depths,
        },
        Entry {
            name: "horizon",
            width: 60,
            height: 3,
            draw: horizon,
        },
    ]
}
