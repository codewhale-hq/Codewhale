//! Artifact fixtures over illustrative caller data, without filesystem or run claims.

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{
    Artifact, ArtifactChip, ArtifactChipRow, ArtifactKind, ArtifactShelf, Paint, ReceiptValue,
    State, Theme,
};

fn artifact(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Artifact::new("Release checklist.md", ArtifactKind::File, State::Done)
        .detail("docs/release-checklist.md")
        .action("Open")
        .receipt("Size", ReceiptValue::Bytes(Some(4280)))
        .paint(area, buf, theme);
}

fn shelf(area: Rect, buf: &mut Buffer, theme: &Theme) {
    ArtifactShelf::new(vec![
        Artifact::new("Release checklist.md", ArtifactKind::File, State::Done)
            .detail("docs/release-checklist.md")
            .action("Open")
            .receipt("Size", ReceiptValue::Bytes(Some(4280))),
        Artifact::new(
            "Conversation changes",
            ArtifactKind::Review,
            State::NeedsYou,
        )
        .detail("Composer and transcript")
        .action("Review")
        .receipt("Files", ReceiptValue::Count(Some(3))),
        Artifact::new("Local verification", ArtifactKind::Run, State::Done)
            .detail("Reported by the host")
            .action("Inspect")
            .receipt("Checks", ReceiptValue::Count(Some(15))),
        Artifact::new("Working notes", ArtifactKind::Link, State::Ready)
            .detail("Project reference")
            .action("Open"),
    ])
    .selected(1)
    .focused(true)
    .paint(area, buf, theme);
}

fn narrow(area: Rect, buf: &mut Buffer, theme: &Theme) {
    ArtifactShelf::new(vec![
        Artifact::new(
            "cafe\u{301} 鲸鱼 — terminal review.md",
            ArtifactKind::File,
            State::Done,
        )
        .detail("notes/terminal-review.md")
        .action("Open"),
        Artifact::new(
            "Changes still need a decision",
            ArtifactKind::Review,
            State::NeedsYou,
        )
        .detail("A long caller-provided review summary")
        .action("Review"),
        Artifact::new("Verification has gaps", ArtifactKind::Run, State::NeedsYou)
            .status_word("Finished with gaps")
            .detail("One check was not reported")
            .action("Inspect"),
    ])
    .selected(2)
    .focused(true)
    .omitted(1)
    .paint(area, buf, theme);
}

fn unknown(area: Rect, buf: &mut Buffer, theme: &Theme) {
    ArtifactShelf::new(vec![
        Artifact::new("Imported report", ArtifactKind::File, State::Unknown)
            .detail("No verification reported")
            .action("Inspect")
            .receipt("Size", ReceiptValue::Bytes(None)),
        Artifact::new("Check failed", ArtifactKind::Run, State::Failed)
            .detail("The host reported an error")
            .action("Read error"),
    ])
    .selected(0)
    .paint(area, buf, theme);
}

fn empty(area: Rect, buf: &mut Buffer, theme: &Theme) {
    ArtifactShelf::new(Vec::new()).paint(area, buf, theme);
}

fn chip(area: Rect, buf: &mut Buffer, theme: &Theme) {
    ArtifactChip::new("summary.md", ArtifactKind::File, State::Done).paint(area, buf, theme);
}

fn chip_row(area: Rect, buf: &mut Buffer, theme: &Theme) {
    ArtifactChipRow::new(vec![
        ArtifactChip::new("summary.md", ArtifactKind::File, State::Done),
        ArtifactChip::new("review.diff", ArtifactKind::Review, State::NeedsYou),
        ArtifactChip::new("ci-log.txt", ArtifactKind::Run, State::Working),
    ])
    .paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "artifact",
            width: 80,
            height: 2,
            draw: artifact,
        },
        Entry {
            name: "artifact-shelf",
            width: 80,
            height: 8,
            draw: shelf,
        },
        Entry {
            name: "artifact-narrow",
            width: 40,
            height: 5,
            draw: narrow,
        },
        Entry {
            name: "artifact-unknown",
            width: 80,
            height: 4,
            draw: unknown,
        },
        Entry {
            name: "artifact-empty",
            width: 40,
            height: 2,
            draw: empty,
        },
        Entry {
            name: "artifact-chip",
            width: 40,
            height: 1,
            draw: chip,
        },
        Entry {
            name: "artifact-chip-row",
            width: 60,
            height: 1,
            draw: chip_row,
        },
    ]
}
