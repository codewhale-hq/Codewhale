//! Authored transcript fixtures: prose with semantic spans, a quote, a list,
//! a table, fenced code and linked text. The host owns parsing and copying.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
};

use super::Entry;
use crate::{CodeBlock, Paint, Theme, Transcript, TranscriptBlock, TranscriptSpan};

fn prose(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Transcript::new(vec![
        TranscriptBlock::heading(1, "Release notes"),
        TranscriptBlock::paragraph(vec![
            TranscriptSpan::plain("The "),
            TranscriptSpan::strong("workbar"),
            TranscriptSpan::plain(" is summoned, and the "),
            TranscriptSpan::emphasis("habitat"),
            TranscriptSpan::plain(" stays quiet under "),
            TranscriptSpan::code("MotionMode::Reduced"),
            TranscriptSpan::plain("."),
        ]),
        TranscriptBlock::paragraph(vec![
            TranscriptSpan::muted("Reported by the host. "),
            TranscriptSpan::success("Receipts read."),
        ]),
        TranscriptBlock::quote_by("Ship the quiet version first.", "a review note"),
    ])
    .paint(area, buf, theme);
}

fn list_and_table(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Transcript::new(vec![
        TranscriptBlock::heading(2, "Before landing"),
        TranscriptBlock::ordered(vec![
            TranscriptSpan::plain("Run the narrow layout checks"),
            TranscriptSpan::plain("Read the receipt and the diff"),
            TranscriptSpan::warning("Confirm the 40-column view"),
        ]),
        TranscriptBlock::table(
            vec![Cow::Borrowed("Check"), Cow::Borrowed("State")],
            vec![
                vec![Cow::Borrowed("narrow layout"), Cow::Borrowed("kept")],
                vec![Cow::Borrowed("all profiles"), Cow::Borrowed("kept")],
            ],
        ),
    ])
    .paint(area, buf, theme);
}

fn fenced_code(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let block = CodeBlock::new(
        "rust",
        "let theme = Theme::detect();\nframe.render_widget(view.themed(&theme), area);",
    );
    Transcript::new(vec![
        TranscriptBlock::heading(2, "Paint into the frame"),
        TranscriptBlock::code(block),
    ])
    .paint(area, buf, theme);
}

fn linked(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Transcript::new(vec![
        TranscriptBlock::paragraph(vec![
            TranscriptSpan::plain("Read the "),
            TranscriptSpan::link("component guide", "https://example.invalid/guide"),
            TranscriptSpan::plain(" before adding a component."),
        ]),
        TranscriptBlock::paragraph(vec![TranscriptSpan::muted(
            "Links are metadata; the host dispatches.",
        )]),
    ])
    .paint(area, buf, theme);
}

fn mounted(area: Rect, buf: &mut Buffer, theme: &Theme) {
    use crate::{Role, TranscriptScrollFacts, TranscriptViewport, TranscriptViewportStyles};
    let plain = theme.fg(Role::Foreground);
    let selected = plain.patch(theme.bg(Role::Selected)).add_modifier(
        if theme.color(Role::Foreground).is_none() {
            Modifier::REVERSED
        } else {
            Modifier::empty()
        },
    );
    let rows = vec![
        Line::styled("User: Review the changed source", theme.fg(Role::Attention)),
        Line::from(vec![
            Span::styled("The ", plain),
            Span::styled("selected words", selected),
            Span::styled(" retain their source.", plain),
        ]),
        Line::styled(
            if theme.ascii() {
                "A composed cafe draft"
            } else {
                "A composed cafe\u{0301} draft with 鲸鱼"
            },
            plain,
        ),
        Line::styled(
            "let source = exact_source;",
            theme.fg(Role::Live).patch(theme.bg(Role::Surface)),
        ),
        Line::styled(
            "Keep the permission receipt visible",
            theme.fg(Role::Attention),
        ),
        Line::styled(
            "Read the linked guide",
            theme.fg(Role::Accent).add_modifier(Modifier::UNDERLINED),
        ),
    ];
    let background = theme.bg(Role::Surface);
    let mut viewport = TranscriptViewport::new(&rows);
    viewport.style = background;
    viewport.fill = true;
    viewport.ascii = theme.ascii();
    viewport.scrollbar = Some(TranscriptScrollFacts {
        top: 4,
        visible: usize::from(area.height),
        total: 30,
    });
    viewport.jump_to_latest = true;
    viewport.styles = TranscriptViewportStyles {
        background,
        track: theme.fg(Role::Border),
        thumb: theme.fg(Role::Live),
        jump_border: theme.fg(Role::Border),
        jump_arrow: theme.fg(Role::Live),
    };
    viewport.paint(area, buf, theme);
}
fn focused(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let rows = vec![
        Line::styled(
            "Worker: Builder | paused",
            theme
                .fg(crate::Role::Attention)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled("Old source row", theme.fg(crate::Role::Muted)),
        Line::styled(
            "The host supplies the current source rows",
            theme.fg(crate::Role::Foreground),
        ),
        Line::styled(
            "The pure viewport keeps the banner pinned",
            theme.fg(crate::Role::Foreground),
        ),
    ];
    let mut viewport = crate::TranscriptViewport::new(&rows);
    viewport.pinned_rows = 1;
    viewport.offset = 1;
    viewport.style = theme.bg(crate::Role::Surface);
    viewport.paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "transcript-mounted",
            width: 52,
            height: 9,
            draw: mounted,
        },
        Entry {
            name: "transcript-mounted-focus",
            width: 52,
            height: 6,
            draw: focused,
        },
        Entry {
            name: "transcript-prose",
            width: 40,
            height: 10,
            draw: prose,
        },
        Entry {
            name: "transcript-list-table",
            width: 40,
            height: 9,
            draw: list_and_table,
        },
        Entry {
            name: "transcript-code",
            width: 40,
            height: 6,
            draw: fenced_code,
        },
        Entry {
            name: "transcript-links",
            width: 40,
            height: 5,
            draw: linked,
        },
    ]
}
