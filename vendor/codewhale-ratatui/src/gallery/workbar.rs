//! The eight Codewhale work-dock panels, with ordinary example rows.

use ratatui::{buffer::Buffer, layout::Rect, text::Line};

use super::Entry;
use crate::{
    DiffReviewFile, DiffReviewHeader, DiffScope, Paint, Role, TaskProgressHeader, Theme, Workbar,
    WorkbarAgent, WorkbarPanel, WorkbarPlacement, WorkbarRow, WorkbarTab, WorkbarTone,
};

fn tabs() -> Vec<WorkbarTab> {
    WorkbarPanel::ORDER
        .into_iter()
        .map(|panel| match panel {
            WorkbarPanel::Tasks => WorkbarTab::new(panel).count(3),
            WorkbarPanel::Fleet => WorkbarTab::new(panel).count(2),
            WorkbarPanel::Jobs => WorkbarTab::new(panel).count(1),
            WorkbarPanel::Files => WorkbarTab::new(panel).count(3),
            WorkbarPanel::Notes => WorkbarTab::new(panel).count(2),
            _ => WorkbarTab::new(panel),
        })
        .collect()
}

fn rows(panel: WorkbarPanel) -> Vec<WorkbarRow> {
    let dot = "·";
    match panel {
        WorkbarPanel::Tasks => vec![
            WorkbarRow::new("graph:read", "Read the existing component")
                .mark("✓")
                .tone(WorkbarTone::Success)
                .detail("completed"),
            WorkbarRow::new("graph:extract", "Extract the native work dock")
                .mark("●")
                .tone(WorkbarTone::Live)
                .detail("in progress"),
            WorkbarRow::new("graph:document", "Document the reusable API")
                .mark("○")
                .detail("pending"),
        ],
        WorkbarPanel::Fleet => vec![
            WorkbarRow::new("worker:build", "Builder").mark("●").agent(
                WorkbarAgent::new(
                    "builder",
                    "running",
                    "Extract the dock and keep the native layout",
                )
                .elapsed_seconds(754)
                .tokens(111_900)
                .remaining(2),
            ),
            WorkbarRow::new("worker:review", "Reviewer")
                .mark("✓")
                .agent(
                    WorkbarAgent::new(
                        "reviewer",
                        "completed",
                        "Review tab and keyboard navigation",
                    )
                    .elapsed_seconds(183),
                ),
        ],
        WorkbarPanel::Terminal => vec![WorkbarRow::new(
            "terminal:host",
            "Terminal sessions are supplied by the host",
        )],
        WorkbarPanel::Jobs => vec![
            WorkbarRow::new("shell:gallery", "Render the component gallery")
                .mark("●")
                .tone(WorkbarTone::Live)
                .detail("running"),
            WorkbarRow::new("shell:check", "Check formatting")
                .mark("✓")
                .tone(WorkbarTone::Success)
                .detail("completed"),
        ],
        WorkbarPanel::Files => vec![
            WorkbarRow::new("files:edited", "Edited 2")
                .mark("▾")
                .tone(WorkbarTone::Heading)
                .selectable(false),
            WorkbarRow::new("files:edit:workbar", "src/components/workbar.rs")
                .mark("✎")
                .tone(WorkbarTone::Success)
                .detail("+48 -12"),
            WorkbarRow::new("files:edit:gallery", "src/gallery/workbar.rs")
                .mark("✎")
                .tone(WorkbarTone::Success)
                .detail("+22 -4"),
            WorkbarRow::new("files:read", "Read 1")
                .mark("▾")
                .tone(WorkbarTone::Heading)
                .selectable(false),
            WorkbarRow::new("files:read:source", "work_surface/render/mod.rs")
                .mark("·")
                .selectable(false),
        ],
        WorkbarPanel::Notes => vec![
            WorkbarRow::new("notes:1", "Keep the native dock tab order")
                .mark("▪")
                .tone(WorkbarTone::Live)
                .detail("/note show 1"),
            WorkbarRow::new("notes:2", "Make each row independently selectable")
                .mark("▪")
                .tone(WorkbarTone::Live)
                .detail("/note show 2"),
        ],
        WorkbarPanel::Context => vec![
            WorkbarRow::new(
                "context:budget",
                format!("48k of 128k {dot} 38% {dot} compacts at 90%"),
            )
            .mark("◔")
            .tone(WorkbarTone::Live)
            .detail("/context for the full source map"),
            WorkbarRow::new(
                "context:system",
                format!("system + tools {dot} 8.4k {dot} 24 tools"),
            )
            .mark("·")
            .selectable(false),
            WorkbarRow::new(
                "context:conversation",
                format!("conversation {dot} 18 {dot} 29.2k"),
            )
            .mark("·")
            .selectable(false),
            WorkbarRow::new("context:tool-output", format!("tool output {dot} 10.4k"))
                .mark("·")
                .selectable(false),
            WorkbarRow::new("context:compact", "compact now")
                .mark("▸")
                .tone(WorkbarTone::Live)
                .detail("/compact"),
        ],
        WorkbarPanel::Git => vec![
            WorkbarRow::new("git:branch", format!("feature/work-dock {dot} up to date"))
                .mark("⎇")
                .tone(WorkbarTone::Live)
                .detail("example/component-kit"),
            WorkbarRow::new("git:changes", "2 modified files")
                .mark("±")
                .tone(WorkbarTone::Live)
                .detail("/diff"),
            WorkbarRow::new("git:commit", "Extract reusable work dock")
                .mark("·")
                .detail("latest commit"),
        ],
        WorkbarPanel::Cost => vec![
            WorkbarRow::new("price:session", format!("session {dot} $0.42"))
                .mark("$")
                .tone(WorkbarTone::Live)
                .detail("/cost for the full ledger"),
            WorkbarRow::new("price:agents", format!("agents {dot} $0.18"))
                .mark("·")
                .detail("2 of 2 agents priced")
                .selectable(false),
            WorkbarRow::new(
                "price:cache",
                format!("cache hit {dot} parent 72% {dot} agents 64%"),
            )
            .mark("·")
            .detail("/cache for per-turn cache telemetry"),
        ],
    }
}

pub(crate) fn sample(panel: WorkbarPanel) -> Workbar {
    let rows = rows(panel);
    let selected = rows
        .iter()
        .find(|row| row.selectable)
        .map(|row| row.id.clone());
    let mut dock = Workbar::new(panel, rows).tabs(tabs()).focused(true);
    if let Some(selected) = selected {
        dock = dock.selected(selected);
    }
    if panel == WorkbarPanel::Tasks {
        let dot = "·";
        dock = dock
            .goal("Make the work dock reusable")
            .progress(format!("TODO {dot} 1/3 {dot} 2 left"));
    }
    dock
}

fn tasks(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Tasks).paint(area, buf, theme);
}
fn fleet(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Fleet).paint(area, buf, theme);
}
fn jobs(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Jobs).paint(area, buf, theme);
}
fn files(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Files).paint(area, buf, theme);
}
fn notes(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Notes).paint(area, buf, theme);
}
fn context(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Context).paint(area, buf, theme);
}
fn git(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Git).paint(area, buf, theme);
}
fn cost(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Cost).paint(area, buf, theme);
}
fn top(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample(WorkbarPanel::Tasks)
        .placement(WorkbarPlacement::Top)
        .paint(area, buf, theme);
}

fn narrow(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let rows = (0..12)
        .map(|index| {
            WorkbarRow::new(format!("graph:{index}"), format!("Plan step {}", index + 1))
                .mark(if index == 3 { "●" } else { "○" })
                .tone(if index == 3 {
                    WorkbarTone::Live
                } else {
                    WorkbarTone::Muted
                })
        })
        .collect();
    Workbar::new(WorkbarPanel::Tasks, rows)
        .tabs(tabs())
        .goal("Extract the native dock")
        .progress(if theme.ascii() {
            "TODO . 3/12 . 9 left"
        } else {
            "TODO · 3/12 · 9 left"
        })
        .focused(true)
        .selected("graph:3")
        .offset(3)
        .paint(area, buf, theme);
}

fn side_with(placement: WorkbarPlacement, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let dock = sample(WorkbarPanel::Fleet).placement(placement);
    let regions = dock.regions(area);
    if regions.content.height > 0 && regions.content.width > 0 {
        buf.set_line(
            regions.content.x,
            regions.content.y,
            &Line::styled("Codewhale", theme.fg(Role::Foreground)),
            regions.content.width,
        );
        if regions.content.height > 2 {
            buf.set_line(
                regions.content.x,
                regions.content.y + 2,
                &Line::styled("The conversation keeps its space.", theme.fg(Role::Muted)),
                regions.content.width,
            );
        }
    }
    if let Some(area) = regions.dock {
        dock.placement(regions.placement).paint(area, buf, theme);
    }
}
fn left(area: Rect, buf: &mut Buffer, theme: &Theme) {
    side_with(WorkbarPlacement::Left, area, buf, theme);
}
fn right(area: Rect, buf: &mut Buffer, theme: &Theme) {
    side_with(WorkbarPlacement::Right, area, buf, theme);
}

fn task_progress(area: Rect, buf: &mut Buffer, theme: &Theme) {
    TaskProgressHeader::new(2, 5)
        .active("Verify workbar snapshots")
        .paint(area, buf, theme);
}
fn diff_review(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let files = [
        DiffReviewFile::new("src/workbar.rs", 24, 6).viewed(true),
        DiffReviewFile::new("src/diff.rs", 18, 2),
    ];
    DiffReviewHeader::from_files(DiffScope::Branch, &files).paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    [
        (
            "workbar-tasks",
            104,
            7,
            tasks as fn(Rect, &mut Buffer, &Theme),
        ),
        ("workbar-fleet", 104, 5, fleet),
        ("workbar-jobs", 104, 5, jobs),
        ("workbar-files", 104, 8, files),
        ("workbar-notes", 104, 5, notes),
        ("workbar-context", 104, 8, context),
        ("workbar-git", 104, 6, git),
        ("workbar-cost", 104, 6, cost),
        ("workbar-top", 104, 7, top),
        ("workbar-narrow", 38, 8, narrow),
        ("workbar-left", 104, 12, left),
        ("workbar-right", 104, 12, right),
        ("workbar-task-progress", 64, 1, task_progress),
        ("diff-review-header", 48, 1, diff_review),
    ]
    .into_iter()
    .map(|(name, width, height, draw)| Entry {
        name,
        width,
        height,
        draw,
    })
    .collect()
}
