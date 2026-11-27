//! Real attention components with illustrative, caller-owned requests.

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{
    AttentionItem, AttentionQueue, KeyHint, KeyHints, NeedsAttentionPill, Paint, State, Theme,
};

fn requests<'a>(hints: &'a KeyHints) -> Vec<AttentionItem<'a>> {
    vec![
        AttentionItem::new(
            "Codewhale",
            "Verification",
            "Choose the validation gate for the interrupted run.",
            State::NeedsYou,
        )
        .priority(100)
        .timestamp("2m ago")
        .action("Review run")
        .hints(hints),
        AttentionItem::new(
            "Desktop",
            "Review",
            "Confirm which files should be included in the next change.",
            State::NeedsYou,
        )
        .priority(80)
        .timestamp("8m ago")
        .action("Review changes")
        .hints(hints),
        AttentionItem::new(
            "Documentation",
            "Language",
            "Choose the tone for the Chinese quickstart.",
            State::NeedsYou,
        )
        .priority(30)
        .timestamp("14m ago")
        .action("Answer question")
        .hints(hints),
    ]
}

fn inbox(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let hints = KeyHints::new(vec![
        KeyHint::new("Enter", "open"),
        KeyHint::new("s", "later"),
    ]);
    let items = requests(&hints);
    AttentionQueue::new(&items)
        .selected(0)
        .focused(true)
        .paint(area, buf, theme);
}

fn focused(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let hints = KeyHints::new(vec![
        KeyHint::new("Enter", "open"),
        KeyHint::new("Esc", "back"),
    ]);
    AttentionItem::new(
        "codewhale-ratatui",
        "Package review",
        "The preview catalogue is ready to inspect.\nReview the package files before publishing.",
        State::NeedsYou,
    )
    .timestamp("just now")
    .action("Review package")
    .hints(&hints)
    .selected(true)
    .focused(true)
    .paint(area, buf, theme);
}

fn narrow(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let hints = KeyHints::new(vec![
        KeyHint::new("Enter", "open"),
        KeyHint::new("s", "later"),
    ]);
    let items = requests(&hints);
    AttentionQueue::new(&items)
        .selected(1)
        .focused(true)
        .paint(area, buf, theme);
}

fn empty(area: Rect, buf: &mut Buffer, theme: &Theme) {
    AttentionQueue::new(&[]).paint(area, buf, theme);
}

fn pill(area: Rect, buf: &mut Buffer, theme: &Theme) {
    NeedsAttentionPill::new(3).paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "attention-queue",
            width: 92,
            height: 16,
            draw: inbox,
        },
        Entry {
            name: "attention-focused",
            width: 84,
            height: 5,
            draw: focused,
        },
        Entry {
            name: "attention-narrow",
            width: 36,
            height: 16,
            draw: narrow,
        },
        Entry {
            name: "attention-empty",
            width: 64,
            height: 10,
            draw: empty,
        },
        Entry {
            name: "attention-pill",
            width: 32,
            height: 1,
            draw: pill,
        },
    ]
}
