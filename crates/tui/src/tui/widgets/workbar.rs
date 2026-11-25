//! Engine facts and localization for the shared terminal kit's workflow rows.
//!
//! The kit replaces the former RowCells, column fitter, bar partition and
//! clipping renderer. WorkflowPanel remains the Engine's lifecycle authority;
//! this adapter passes its reported facts into WorkflowProgress. The existing
//! backend still owns terminal capabilities and custom palette adaptation.

use std::time::Duration;

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette::UiTheme;
use codewhale_ratatui::{
    Caps, Role, Theme, TuiInk, TuiPalette, WorkflowProgress, WorkflowProgressWords, WorkflowRun,
    WorkflowRunState, color::ColorDepth, detect::Appearance,
};
use ratatui::text::Line;

use crate::tui::widgets::workflow_panel::{WorkflowPanel, WorkflowPanelLifecycle};

#[cfg(test)]
const LARGE_WORKFLOW_AGENTS: usize = 25;
#[cfg(test)]
const MAX_RUN_ROWS: usize = WorkflowProgress::desired_rows_for(usize::MAX) as usize - 1;

pub(crate) struct WorkbarRun<'a> {
    pub panel: &'a WorkflowPanel,
    pub queued: usize,
}

#[must_use]
pub(crate) fn desired_rows(runs: usize) -> u16 {
    WorkflowProgress::desired_rows_for(runs)
}

pub(crate) fn render(
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    runs: &[WorkbarRun<'_>],
    now_ms: u64,
    theme: &UiTheme,
    locale: Locale,
) {
    use ratatui::widgets::{Paragraph, Widget};
    let area = area.intersection(buf.area);
    Paragraph::new(lines(
        runs,
        area.width,
        usize::from(area.height),
        now_ms,
        theme,
        locale,
    ))
    .render(area, buf);
}

#[must_use]
pub(crate) fn lines(
    runs: &[WorkbarRun<'_>],
    width: u16,
    max_rows: usize,
    now_ms: u64,
    theme: &UiTheme,
    locale: Locale,
) -> Vec<Line<'static>> {
    let word = |id| tr(locale, id).into_owned().into();
    let progress = WorkflowProgress::new(
        runs.iter().map(|run| facts(run, now_ms, locale)).collect(),
    )
    .words(WorkflowProgressWords {
        done: word(MessageId::WorkflowSettledOfTotal),
        failed: word(MessageId::WorkflowCountFailed),
        cancelled: word(MessageId::WorkflowCountCancelled),
        queued: word(MessageId::AgentRailQueuedCount),
        no_tasks: word(MessageId::WorkflowNoTasksYet),
        large: word(MessageId::WorkbarLargeWorkflow),
        gaps: word(MessageId::WorkflowLineFinishedWithGaps),
        stopped: word(MessageId::WorkflowLineStopped),
        more: word(MessageId::WorkbarMoreRuns),
        manage: word(MessageId::FooterHintToManage),
    });
    // The selected/custom UiTheme and existing backend retain palette authority.
    // A fixed full-color kit palette supplies distinct semantic ink identities;
    // map those identities before the backend's single capability pass.
    let source_theme = Theme::new(Caps {
        depth: ColorDepth::TrueColor,
        ascii: false,
        appearance: Appearance::Dark,
    })
    .tui_palette(TuiPalette::Underwater);
    let inks = [
        (source_theme.color(Role::Foreground), theme.text_body),
        (source_theme.color(Role::Muted), theme.text_muted),
        (source_theme.color(Role::Hint), theme.text_hint),
        (source_theme.color(Role::Danger), theme.error_fg),
        (
            source_theme.tui_ink(TuiInk::Working).fg,
            theme.accent_action,
        ),
        (source_theme.tui_ink(TuiInk::Success).fg, theme.success),
        (source_theme.tui_ink(TuiInk::Warning).fg, theme.warning),
    ];
    let mut rows = progress.lines(width, max_rows, &source_theme);
    for row in &mut rows {
        for span in &mut row.spans {
            if let Some((_, color)) = inks
                .iter()
                .find(|(ink, _)| ink.is_some() && *ink == span.style.fg)
            {
                span.style.fg = Some(*color);
            }
        }
    }
    rows
}

fn facts(run: &WorkbarRun<'_>, now_ms: u64, locale: Locale) -> WorkflowRun<'static> {
    let panel = run.panel;
    let (succeeded, _, _, total) = panel.row_outcomes();
    let (failed, cancelled) = panel.failure_cancel_counts();
    let state = match panel.lifecycle {
        WorkflowPanelLifecycle::Pending => WorkflowRunState::Pending,
        WorkflowPanelLifecycle::Running => WorkflowRunState::Running,
        WorkflowPanelLifecycle::Succeeded => WorkflowRunState::Succeeded,
        WorkflowPanelLifecycle::Degraded => WorkflowRunState::Degraded,
        WorkflowPanelLifecycle::Failed => WorkflowRunState::Failed,
        WorkflowPanelLifecycle::Cancelled => WorkflowRunState::Cancelled,
    };
    let mut facts = WorkflowRun::new(panel.short_title(), state)
        .outcomes(succeeded, failed, cancelled, total)
        .queued(run.queued);
    if panel.started_at_ms != 0 {
        let end = panel.completed_at_ms.unwrap_or(now_ms);
        facts = facts.elapsed(Duration::from_millis(
            end.saturating_sub(panel.started_at_ms),
        ));
    }
    if let Some(tokens) = panel.tokens_so_far() {
        facts = facts.tokens(tokens);
    }
    if let Some(reason) = panel.outcome_reason() {
        facts = facts.reason(reason);
    } else if state == WorkflowRunState::Failed {
        facts = facts.reason(tr(locale, MessageId::WorkflowLineFailed).into_owned());
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::widgets::workflow_panel::{WorkflowPanelEvent, WorkflowRowStatus};
    use ratatui::{Terminal, backend::TestBackend};
    use unicode_width::UnicodeWidthStr;

    const NOW: u64 = 1_000_000;

    fn run(label: &str, started: u64, agents: usize, settled: usize) -> WorkflowPanel {
        let mut panel = WorkflowPanel::new(format!("run-{label}"), label, started);
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Survey".to_string(),
            at_ms: started,
        });
        for index in 0..agents {
            let value = serde_json::json!({
                "type": "task_started",
                "task_id": format!("{label}-{index}"),
                "workflow_task_label": format!("agent-{index}"),
                "at_ms": started,
            });
            panel.apply_event(WorkflowPanelEvent::from_json_value(&value).expect("task_started"));
        }
        for index in 0..settled {
            complete(&mut panel, label, index, WorkflowRowStatus::Succeeded, None);
        }
        panel
    }

    fn complete(
        panel: &mut WorkflowPanel,
        label: &str,
        index: usize,
        status: WorkflowRowStatus,
        reason: Option<&str>,
    ) {
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: format!("{label}-{index}"),
            status,
            usage: None,
            reason: reason.map(str::to_string),
            at_ms: panel.started_at_ms + 1_000,
        });
    }

    fn settled(
        label: &str,
        status: WorkflowPanelLifecycle,
        agents: usize,
        done: usize,
    ) -> WorkflowPanel {
        let mut panel = run(label, NOW - 60_000, agents, done);
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status,
            error: None,
            at_ms: NOW - 1_000,
        });
        panel
    }

    /// The founder's run from 2026-09-28, as its status record came back: a
    /// read-only audit whose two agents both failed auth 355 ms in.
    fn founder_all_failed() -> WorkflowPanel {
        let goal = "Read-only release-readiness audit for Codewhale v0.10.1. Determine \
                    actionable remaining blockers from live evidence, separating engine \
                    release from desktop and deferred backlog. No edits.";
        let reason = "[auth] Authorization failed: You have run out of credits or need a \
                      Grok subscription. Add credits at https://grok.com/?_s=usage.\n\
                      (provider `xAI` · requested model `grok-4.6`)";
        let error = "no task produced a result: all 2 task(s) failed and 1 fan-out(s) lost \
                     every slot (no work survived them); the recorded result reflects no \
                     completed work";
        WorkflowPanel::from_run_json(&serde_json::json!({
            "run_id": "workflow_6409ebe6",
            "status": "failed",
            "started_at_ms": 1_790_651_979_185_u64,
            "completed_at_ms": 1_790_651_979_540_u64,
            "workflow_goal": goal,
            "error": error,
            "events": [
                {"at_ms": 1_790_651_979_185_u64, "type": "run_started", "workflow_goal": goal},
                {"at_ms": 1_790_651_979_187_u64, "type": "phase_started", "title": "parallel-evidence"},
                {"at_ms": 1_790_651_979_265_u64, "type": "task_started", "task_id": "agent_f6665537",
                 "workflow_task_label": "engine-readiness"},
                {"at_ms": 1_790_651_979_302_u64, "type": "task_started", "task_id": "agent_94fe8e6a",
                 "workflow_task_label": "desktop-readiness"},
                {"at_ms": 1_790_651_979_514_u64, "type": "task_completed", "task_id": "agent_f6665537",
                 "status": "failed", "reason": reason},
                {"at_ms": 1_790_651_979_523_u64, "type": "task_completed", "task_id": "agent_94fe8e6a",
                 "status": "failed", "reason": reason},
                {"at_ms": 1_790_651_979_540_u64, "type": "run_completed", "status": "failed",
                 "error": error},
            ],
        }))
        .expect("run record")
    }

    fn partial_success() -> WorkflowPanel {
        let mut panel = run("Port the fixture suite", NOW - 134_000, 4, 3);
        complete(
            &mut panel,
            "Port the fixture suite",
            3,
            WorkflowRowStatus::Failed,
            Some("Timed out waiting for the model after 600s. Retried once."),
        );
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Degraded,
            error: Some("completed with dropped slots".to_string()),
            at_ms: NOW,
        });
        panel
    }

    fn render(runs: &[WorkbarRun<'_>], width: u16, rows: u16) -> String {
        buffer_rows(&render_band(runs, width, rows)).join("\n")
    }

    fn render_band(runs: &[WorkbarRun<'_>], width: u16, height: u16) -> ratatui::buffer::Buffer {
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| {
                super::render(
                    area,
                    frame.buffer_mut(),
                    runs,
                    NOW,
                    &codewhale_palette::UI_THEME,
                    Locale::En,
                );
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    fn buffer_rows(buf: &ratatui::buffer::Buffer) -> Vec<String> {
        let area = buf.area;
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn one(panel: &WorkflowPanel) -> [WorkbarRun<'_>; 1] {
        [WorkbarRun { panel, queued: 0 }]
    }

    /// The four states the founder's screenshot needed, as the buffer paints
    /// them. These strings are the PR's evidence; keep them exact.
    #[test]
    fn snapshot_running_all_failed_partial_and_narrow() {
        let mut running = run("Compare Cline with Codewhale", NOW - 134_000, 10, 4);
        complete(
            &mut running,
            "Compare Cline with Codewhale",
            4,
            WorkflowRowStatus::Failed,
            Some("rate limited"),
        );
        running.budget_spent = 1_234_567;
        let founder = founder_all_failed();
        let partial = partial_success();
        let cases = [
            (
                "running",
                render(&one(&running), 110, 1),
                " • Compare Cline with Codewhale  ████████××░░░░░░░░░░  4/10 done · 1 failed  2m 14s  ↓1.2M",
            ),
            (
                "all failed",
                render(&one(&founder), 140, 1),
                " ✕ Read-only release-readiness audit for…  ××××××××××××××××××××  0/2 done · 2 failed  355ms  Authorization failed: You have run out of…",
            ),
            (
                "partial success",
                render(&one(&partial), 140, 1),
                " ◆ Port the fixture suite  ███████████████×××××  3/4 done · 1 failed  2m 14s  finished with gaps · Timed out waiting for the model after…",
            ),
            (
                "narrow 60",
                render(&one(&founder), 60, 1),
                " ✕ Read-only release-readiness…  0/2 done · 2 failed  355ms",
            ),
            (
                "narrow 40",
                render(&one(&founder), 40, 1),
                " ✕ Read-only…  0/2 done · 2 failed",
            ),
        ];
        let wrong: Vec<String> = cases
            .iter()
            .filter(|(_, actual, expected)| actual != expected)
            .map(|(name, actual, expected)| {
                format!("{name}:\n  got  {actual:?}\n  want {expected:?}")
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        for (_, row, _) in &cases[3..] {
            assert!(!row.contains('█') && !row.contains('×'), "{row}");
        }
        assert!(cases[3].1.width() <= 60 && cases[4].1.width() <= 40);
    }

    #[test]
    fn a_failed_run_never_paints_a_success_bar_or_rounds_to_zero_seconds() {
        let founder = founder_all_failed();
        let band = render_band(&one(&founder), 140, 1);
        let row = &buffer_rows(&band)[0];
        assert!(!row.contains('█'), "no agent succeeded: {row}");
        assert!(row.contains("0/2 done · 2 failed"), "{row}");
        assert!(row.contains("355ms") && !row.contains(" 0s"), "{row}");
        // The failed state mark carries error ink; the custom-theme regression
        // separately covers failure bar cells.
        let theme = codewhale_palette::UI_THEME;
        let failed_cell = (0..140)
            .find(|&x| band[(x, 0)].symbol() == codewhale_ratatui::glyphs::FAILED)
            .expect("failed state mark");
        assert_eq!(band[(failed_cell, 0)].fg, theme.error_fg);
        // The agents' own reason, not the run's aggregate, and not cut mid-word.
        assert!(row.contains("Authorization failed"), "{row}");
        assert!(!row.contains("no task produced"), "{row}");
        assert!(row.ends_with('…') || row.ends_with("subscription"), "{row}");
    }

    #[test]
    fn settled_rows_read_without_colour() {
        let done = settled("audit", WorkflowPanelLifecycle::Succeeded, 3, 3);
        let mut failed = run("migrate", NOW - 60_000, 2, 0);
        failed.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Failed,
            error: Some("script error".to_string()),
            at_ms: NOW - 1_000,
        });
        let gaps = settled("review", WorkflowPanelLifecycle::Degraded, 2, 1);
        let runs = [
            WorkbarRun {
                panel: &done,
                queued: 0,
            },
            WorkbarRun {
                panel: &failed,
                queued: 0,
            },
            WorkbarRun {
                panel: &gaps,
                queued: 0,
            },
        ];
        assert_eq!(
            render(&runs, 80, 3),
            [
                " ✓ audit    ████████████████████  3/3 done  59s",
                " ✕ migrate  ░░░░░░░░░░░░░░░░░░░░  0/2 done  59s  script error",
                " ◆ review   ██████████░░░░░░░░░░  1/2 done  59s  finished with gaps",
            ]
            .join("\n")
        );
    }

    #[test]
    fn chips_are_only_ever_true() {
        // Small and healthy: no chip at all.
        let small = run("small", NOW - 5_000, 3, 1);
        let plain = render(&one(&small), 100, 1);
        assert!(
            !plain.contains('⚠') && !plain.contains("queued") && !plain.contains("failed"),
            "{plain}"
        );

        // Large only at the threshold, a failure count only after a failure,
        // queued only when the runtime reports waiting follow-ups.
        let mut large = run("large", NOW - 5_000, LARGE_WORKFLOW_AGENTS, 0);
        complete(&mut large, "large", 0, WorkflowRowStatus::Failed, None);
        let busy = render(
            &[WorkbarRun {
                panel: &large,
                queued: 2,
            }],
            120,
            1,
        );
        assert!(busy.contains("0/25 done · 1 failed"), "{busy}");
        assert!(busy.contains("⚠ Large workflow"), "{busy}");
        assert!(busy.ends_with("· 2 queued"), "{busy}");
        let under = run("under", NOW - 5_000, LARGE_WORKFLOW_AGENTS - 1, 0);
        let under = render(&one(&under), 120, 1);
        assert!(!under.contains("Large"), "{under}");
    }

    #[test]
    fn one_failure_in_a_large_run_still_gets_a_bar_cell() {
        let mut panel = run("large", NOW - 5_000, 100, 99);
        complete(&mut panel, "large", 99, WorkflowRowStatus::Failed, None);
        let shown = render(&one(&panel), 160, 1);
        assert_eq!(shown.matches('×').count(), 1, "{shown}");
        assert_eq!(shown.matches('█').count(), 19, "{shown}");
    }

    #[test]
    fn many_runs_fold_into_a_more_row_that_names_the_key() {
        let panels: Vec<WorkflowPanel> = (0..12)
            .map(|index| run(&format!("wf-{index:02}"), NOW - 10_000, 8, index % 8))
            .collect();
        let runs: Vec<WorkbarRun<'_>> = panels
            .iter()
            .map(|panel| WorkbarRun { panel, queued: 0 })
            .collect();
        // Six run rows and the fold row; no rules.
        assert_eq!(desired_rows(runs.len()), MAX_RUN_ROWS as u16 + 1);
        let snapshot = render(&runs, 100, MAX_RUN_ROWS as u16 + 1);
        let rows: Vec<&str> = snapshot.lines().collect();
        assert_eq!(rows.len(), MAX_RUN_ROWS + 1);
        assert_eq!(rows[MAX_RUN_ROWS], " +6 more · ↓ to manage");
    }

    #[test]
    fn the_band_is_one_row_per_run_and_draws_no_rules() {
        let live = run("Audit the parser", NOW - 30_000, 4, 1);
        let done = settled("Port fixtures", WorkflowPanelLifecycle::Succeeded, 2, 2);
        let runs = [
            WorkbarRun {
                panel: &live,
                queued: 3,
            },
            WorkbarRun {
                panel: &done,
                queued: 0,
            },
        ];
        assert_eq!(desired_rows(runs.len()), 2);
        assert_eq!(desired_rows(0), 0);
        let rows = buffer_rows(&render_band(&runs, 90, desired_rows(runs.len())));
        assert_eq!(
            rows,
            vec![
                " • Audit the parser  █████░░░░░░░░░░░░░░░  1/4 done  30s  · 3 queued".to_string(),
                " ✓ Port fixtures     ████████████████████  2/2 done  59s".to_string(),
            ]
        );
        assert!(!rows.iter().any(|row| row.contains('─')), "{rows:?}");
    }

    /// NO_COLOR / 16-colour / ASCII terminals: every state still reads. Under
    /// monochrome the text is unchanged and each state keeps its own mark;
    /// at 16 colours the state inks stay apart; ASCII-safe marks stay apart
    /// except failed/stopped, which their words tell apart.
    #[test]
    fn states_read_on_no_color_sixteen_colour_and_ascii_terminals() {
        use crate::tui::color_compat::{adapt_cell_colors, adapt_cell_symbol_for_ascii};
        use codewhale_palette::{ColorDepth, PaletteMode, ThemeId};
        let theme = codewhale_palette::UI_THEME;
        let running = run("running", NOW - 10_000, 4, 1);
        let ok = settled("ok", WorkflowPanelLifecycle::Succeeded, 2, 2);
        let gaps = settled("gaps", WorkflowPanelLifecycle::Degraded, 2, 1);
        let failed = settled("failed", WorkflowPanelLifecycle::Failed, 2, 0);
        let stopped = settled("stopped", WorkflowPanelLifecycle::Cancelled, 2, 0);
        let panels = [&running, &ok, &gaps, &failed, &stopped];
        let runs: Vec<WorkbarRun<'_>> = panels
            .iter()
            .map(|panel| WorkbarRun { panel, queued: 0 })
            .collect();
        let source = render_band(&runs, 100, desired_rows(runs.len()));
        let text = buffer_rows(&source);
        let marks: Vec<String> = text[0..5]
            .iter()
            .map(|row| row.chars().nth(1).expect("mark").to_string())
            .collect();
        for (index, mark) in marks.iter().enumerate() {
            assert!(
                !marks[index + 1..].contains(mark),
                "state marks collide: {marks:?}"
            );
        }
        assert!(text[2].contains("finished with gaps"), "{text:?}");
        assert!(text[3].ends_with("failed"), "{text:?}");
        assert!(text[4].contains("stopped"), "{text:?}");

        let adapted = |depth: ColorDepth| {
            let mut buf = source.clone();
            for cell in buf.content.iter_mut() {
                adapt_cell_colors(cell, depth, PaletteMode::Dark, ThemeId::Whale, &theme, None);
            }
            buf
        };
        let mono = adapted(ColorDepth::Monochrome);
        assert_eq!(buffer_rows(&mono), text, "monochrome changes no text");
        assert!(
            mono.content.iter().all(|cell| {
                cell.fg == ratatui::style::Color::Reset && cell.bg == ratatui::style::Color::Reset
            }),
            "monochrome paints no colour"
        );
        let ansi16 = adapted(ColorDepth::Ansi16);
        let mark_ink: Vec<ratatui::style::Color> = (0..5).map(|y| ansi16[(1, y)].fg).collect();
        for (index, ink) in mark_ink.iter().enumerate() {
            assert!(
                !mark_ink[index + 1..].contains(ink),
                "16-colour state inks collide: {mark_ink:?}"
            );
        }

        let mut ascii = source.clone();
        for cell in ascii.content.iter_mut() {
            adapt_cell_symbol_for_ascii(cell);
        }
        let ascii_rows = buffer_rows(&ascii);
        assert!(
            ascii_rows.iter().all(|row| row.is_ascii()),
            "{ascii_rows:?}"
        );
        let ascii_marks: Vec<char> = ascii_rows[0..4]
            .iter()
            .map(|row| row.chars().nth(1).expect("mark"))
            .collect();
        for (index, mark) in ascii_marks.iter().enumerate() {
            assert!(
                !ascii_marks[index + 1..].contains(mark),
                "ASCII marks collide: {ascii_marks:?}"
            );
        }
    }

    #[test]
    fn localized_folded_counts_and_custom_failure_ink_survive_kit_adoption() {
        use ratatui::style::Color;
        let panels: Vec<_> = (0..8)
            .map(|n| settled(&format!("run-{n}"), WorkflowPanelLifecycle::Failed, 2, 0))
            .collect();
        let runs: Vec<_> = panels
            .iter()
            .map(|panel| WorkbarRun { panel, queued: 0 })
            .collect();
        let mut theme = codewhale_palette::UI_THEME;
        theme.error_fg = Color::Rgb(231, 12, 56);
        let folded = lines(&runs, 100, 1, NOW, &theme, Locale::Ja);
        let text = folded[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("ほか 8 件"), "{text}");
        assert!(!text.contains("{count}"), "{text}");
        let row = lines(&runs[..1], 100, 1, NOW, &theme, Locale::En);
        assert_eq!(row[0].spans[1].style.fg, Some(theme.error_fg));
        assert!(
            row[0]
                .spans
                .iter()
                .filter(|span| span.content.contains('×'))
                .all(|span| { span.style.fg == Some(theme.error_fg) })
        );
    }

    #[test]
    fn seventy_five_agents_across_ten_runs_render_in_one_pass() {
        let panels: Vec<WorkflowPanel> = (0..10)
            .map(|index| run(&format!("wf-{index}"), NOW - 10_000, 8, 3))
            .collect();
        let runs: Vec<WorkbarRun<'_>> = panels
            .iter()
            .map(|panel| WorkbarRun { panel, queued: 0 })
            .collect();
        let started = std::time::Instant::now();
        for _ in 0..100 {
            let _ = lines(
                &runs,
                120,
                MAX_RUN_ROWS + 1,
                NOW,
                &codewhale_palette::UI_THEME,
                Locale::En,
            );
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "100 frames of 10 runs × 8 agents took {:?}",
            started.elapsed()
        );
    }
}
