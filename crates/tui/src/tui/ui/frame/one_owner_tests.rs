//! One owner per fact: the full metrics preset paints each session fact in exactly
//! one chrome row (SHELL-DESIGN-20260901 §2.0 item 3, §2.2, §2.3, §2.3b).
//!
//! Under the composer: row 1 is the posture bar (permission, mode, live
//! counts, the one hint that applies now), row 2 is the metrics line (model,
//! context, cost, ttft, tok/s, output tokens); the roster and to-do rows follow
//! only when they have content. Every fact below is asserted to appear in
//! the composed frame exactly once.

// These tests print the composed frame as failure evidence. They run under
// `cargo test`, never inside the alt-screen, so `tui/mod.rs`'s
// `#![deny(clippy::print_stderr)]` — which exists to stop the scroll demon in
// production paint paths — does not apply here. The exception is test-module
// only; production TUI code still denies unstructured stderr.
#![allow(clippy::print_stderr)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::{Terminal, backend::TestBackend};

use crate::tui::app::App;
use crate::tui::history::HistoryCell;
use codewhale_runtime::config::Config;

fn frame_app() -> App {
    let mut app = crate::tui::test_support::test_app_with_options(crate::tui::app::TuiOptions {
        model: "deepseek-v4-flash".to_string(),
        start_in_agent_mode: true,
        max_subagents: 4,
        ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
    });
    app.onboarding = crate::tui::app::OnboardingState::None;
    app.launch.visible = false;
    app.ui_locale = codewhale_localization::Locale::En;
    app.metrics_line = codewhale_runtime::config::ChromeRowPreset::Full;
    // The posture bar's permission chip carries the filesystem-scope notice
    // (`files: workspace (unenforced)`) whenever no sandbox backend can
    // actually enforce the policy — true on default Linux and all Windows,
    // false on macOS, which resolves seatbelt. That is 30 columns of chip
    // that appears or not depending on which machine runs the test, and it
    // decides what an 80-column shed ladder can still hold: these tests
    // passed locally and failed on both CI legs until it was pinned. Force
    // the wider, unenforced reading so every host asserts the same row.
    app.sandbox_backend = None;
    // The fixture config carries no key; a keyless launch paints "model not
    // connected" (U3). These rows are about a connected route.
    app.onboarding_needs_api_key = false;
    app
}

fn subagent(
    id: &str,
    status: codewhale_engine::tools::subagent::SubAgentStatus,
) -> codewhale_engine::tools::subagent::SubAgentResult {
    codewhale_engine::tools::subagent::SubAgentResult {
        usage: None,
        name: id.to_string(),
        agent_id: id.to_string(),
        context_mode: "fresh".to_string(),
        fork_context: false,
        workspace: None,
        git_branch: None,
        agent_type: codewhale_agent_contract::role::FleetRole::Worker,
        assignment: codewhale_engine::tools::subagent::SubAgentAssignment {
            native_preset: None,
            objective: format!("objective-{id}"),
            role: Some("worker".to_string()),
        },
        model: "deepseek-v4-flash".to_string(),
        nickname: None,
        status,
        worker_status: None,
        runtime_permissions: None,
        parent_run_id: None,
        spawn_depth: 0,
        child_route: None,
        result: None,
        steps_taken: 0,
        checkpoint: None,
        needs_input: None,
        duration_ms: 0,
        started_at: None,
        from_prior_session: false,
        idle_ms: None,
        heartbeat_timeout_ms: None,
    }
}

/// A working turn with two running sub-agents and a session that has
/// already reported one turn's metrics.
fn working_app() -> App {
    let mut app = frame_app();
    app.history = vec![HistoryCell::User {
        content: "audit the shell".to_string(),
    }];
    app.resync_history_revisions();
    app.is_loading = true;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(75));
    app.subagent_cache = vec![
        subagent(
            "agent_a",
            codewhale_engine::tools::subagent::SubAgentStatus::Running,
        ),
        subagent(
            "agent_b",
            codewhale_engine::tools::subagent::SubAgentStatus::Running,
        ),
    ];
    app.session_metrics
        .record_model_call(1_200, 29_600, Some(400), Some(30_000));
    app.session.last_completion_tokens = Some(1_200);
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    draw_into(app, &mut terminal).0
}

fn draw_into(
    app: &mut App,
    terminal: &mut Terminal<TestBackend>,
) -> (Vec<String>, Option<(u16, u16)>) {
    let config = Config::default();
    let mut cursor = None;
    super::prepare_frame_cursor(terminal).unwrap();
    terminal
        .draw(|frame| {
            cursor = super::render(frame, app, &config);
        })
        .unwrap();
    super::finish_frame_cursor(terminal, cursor).unwrap();
    let buf = terminal.backend().buffer();
    let rows = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    (rows, cursor)
}

fn count_rows_containing(rows: &[String], needle: &str) -> usize {
    rows.iter().filter(|row| row.contains(needle)).count()
}

/// Every chrome fact paints in exactly one row of the composed default
/// frame: the context reading, the mode and permission chips, the model,
/// the cost, the agent count, and the help hint.
///
/// 160 columns joins the blocker sizes so both working-clock halves can
/// paint together when the session half is present; at 80 and 120 the
/// clocks shed by design (#5914) against the pinned scope notice. The
/// #6084 shed-order fix (sole turn clock uses the session rung) is pinned
/// in `phase_strip::tideline_tests`, where the narrower permission chip
/// exposes the width band the one-owner fixture's notice collapses.
#[test]
fn composed_frame_paints_each_fact_in_exactly_one_row() {
    for (width, height) in [(80u16, 24u16), (120, 32), (160, 40), (200, 40)] {
        let mut app = working_app();
        let rows = draw(&mut app, width, height);
        let pct = super::info_context_percent(&app);
        let (_, model) = app.effective_route_identity_display();
        let (mode, permission) = crate::tui::underwater::posture_chips(&app);
        let mode = mode.expect("mode chip").0.into_owned();
        let permission = permission.expect("permission chip").0.into_owned();
        // The context reading paints exactly once, at every fullness
        // (#5950 — it used to go silent below 50%).
        let mut facts = vec![
            ("mode chip", format!("   {mode} (")),
            // Mark 8: the permission in force is marked `●` and followed by
            // what its key does, not a parenthesised chord.
            ("permission chip", format!("● {permission}")),
            ("model", model),
            ("cost", super::session_cost_label(&app)),
            ("agent count", "2 agents".to_string()),
            ("ttft", "ttft 400ms".to_string()),
        ];
        facts.push(("context reading", format!("context {pct}%")));
        facts.push(("output rate", "40 avg tok/s".to_string()));
        if width >= 120 {
            facts.push((
                "help hint",
                crate::tui::shell_key_routing::info_help_hint(app.ui_locale),
            ));
        }
        for (name, needle) in facts {
            if needle.is_empty() {
                continue;
            }
            assert_eq!(
                count_rows_containing(&rows, &needle),
                1,
                "{width}x{height}: {name} {needle:?} must paint in exactly one row:\n{}",
                rows.join("\n")
            );
        }
        // Rows under the composer: posture bar, then metrics line, then the
        // roster — never the other way round.
        let posture = rows
            .iter()
            .position(|row| row.contains("● "))
            .expect("posture bar");
        let metrics = rows
            .iter()
            .position(|row| row.contains("context "))
            .expect("metrics line");
        let composer = app
            .viewport
            .last_composer_area
            .expect("composer area")
            .bottom();
        assert_eq!(
            posture,
            usize::from(composer),
            "posture bar is row 1 under the composer"
        );
        assert_eq!(metrics, posture + 1, "metrics line is row 2");
        // The scope notice is pinned on, not inherited from the host: if
        // this ever goes quiet the widths below stop meaning what they say.
        assert!(
            rows[posture].contains("files: workspace (unenforced)"),
            "{width}x{height}: the fixture must pin the scope notice: {}",
            rows[posture]
        );
        // The bar carries the working clock (#5914) — how long the current
        // turn has been doing what it is doing, and how long the session has
        // worked. Both halves shed before the hint and the counts, so a
        // narrow row keeps the affordances and drops the stopwatch. When
        // both would paint, the session half needs ~125 columns here (the
        // running hint also names the agent arrows, `← for agents · ↓ to
        // manage`) and the turn half ~200; each paints in exactly one row wherever it
        // paints. The metrics line carries no repository, branch or provider.
        // First turn: the turn half names the phase and stays; the session
        // reading is the identical duration, so it is suppressed rather than
        // stated twice (#6041). With no session half to paint, the turn
        // clock sheds at the session rung (#6084) rather than first — but
        // against this fixture's pinned scope notice, turn+counts+hint is
        // still just over a 120-column budget, so the hint wins here and
        // the turn half needs ~160. The shed-order contract itself lives
        // in tideline_tests.
        let turn_needle = "agents underway 1m 15s";
        if width >= 160 {
            assert!(rows[posture].contains(turn_needle), "{}", rows[posture]);
            assert_eq!(
                count_rows_containing(&rows, turn_needle),
                1,
                "{width}x{height}: {turn_needle:?} paints in exactly one row:\n{}",
                rows.join("\n")
            );
        }
        assert!(
            !rows[posture].contains("worked 1m 15s"),
            "{width}x{height}: the duplicate session reading must not be stated: {}",
            rows[posture]
        );
        // After a finished turn the totals differ and the worked chip
        // returns: 1m of finished turns plus the live 1m 15s reads 2m 15s.
        let mut worked = working_app();
        worked.cumulative_turn_duration = Duration::from_secs(60);
        let rows = draw(&mut worked, width, height);
        if width >= 160 {
            let worked_needle = "worked 2m 15s";
            assert!(rows[posture].contains(worked_needle), "{}", rows[posture]);
            assert_eq!(
                count_rows_containing(&rows, worked_needle),
                1,
                "{width}x{height}: {worked_needle:?} paints in exactly one row:\n{}",
                rows.join("\n")
            );
        }
        // With both clock halves and the running arrow hints, the turn half
        // needs more than 160 columns.
        if width >= 200 {
            assert!(rows[posture].contains(turn_needle), "{}", rows[posture]);
        }
        assert!(!rows[metrics].contains('⑂'), "{}", rows[metrics]);
        // No dead key hints anywhere in the frame.
        for row in &rows {
            assert!(!row.contains("F1"), "F1 is not receivable: {row}");
            assert!(!row.contains("? help"), "bare ? is composer text: {row}");
        }
    }
}

/// Idle: the two rows are there, the roster is not, and the last turn's
/// metrics survive between turns.
#[test]
fn idle_frame_keeps_two_chrome_rows_and_last_turn_metrics() {
    let mut app = working_app();
    app.is_loading = false;
    app.turn_started_at = None;
    app.subagent_cache.clear();
    // 120 columns: the labeled readings ("thinking: max", "context 0%",
    // mark 8) are wider than the bare ones, so at 100 columns the output
    // count (shed priority 7, ahead of the help hint) is shed by design.
    let rows = draw(&mut app, 120, 32);
    let composer = app.viewport.last_composer_area.unwrap().bottom() as usize;
    assert!(rows[composer].contains("● "), "{}", rows[composer]);
    // The idle fixture sits at 0% context and says so: the reading is on
    // the row at every fullness (#5950), not only once it is a problem.
    assert!(
        rows[composer + 1].contains("context 0%"),
        "{}",
        rows[composer + 1]
    );
    assert!(
        rows[composer + 1].contains("40 avg tok/s"),
        "{}",
        rows[composer + 1]
    );
    assert!(
        rows[composer + 1].contains("↓ 1.2K"),
        "{}",
        rows[composer + 1]
    );
    assert_eq!(
        composer + 2,
        rows.len(),
        "nothing under the metrics line when idle"
    );
    assert!(
        !rows[composer].contains("Esc to interrupt"),
        "{}",
        rows[composer]
    );
}

/// At the cap the posture bar's hint says what to do; the reading still
/// paints once, in the metrics line.
#[test]
fn context_cap_warns_once_in_the_posture_bar() {
    let mut app = working_app();
    app.active_route_limits = Some(codewhale_config::route::RouteLimits {
        context_tokens: Some(60),
        ..Default::default()
    });
    // 140 columns: the fixture always paints the filesystem scope notice
    // (`frame_app` pins `sandbox_backend = None`), so the width has to hold
    // the warning with the notice present. The clock halves shed first.
    let rows = draw(&mut app, 140, 32);
    let pct = super::info_context_percent(&app);
    assert!(pct >= 80, "fixture must sit at the cap: {pct}");
    assert_eq!(
        count_rows_containing(&rows, "surface soon — /compact"),
        1,
        "cap warning rows:\n{}",
        rows.join("\n")
    );
    assert_eq!(
        count_rows_containing(&rows, &format!("{pct}%")),
        1,
        "context reading rows:\n{}",
        rows.join("\n")
    );
}

/// The double-tap window advertises itself in the posture bar's hint slot,
/// and keeps it: both halves of the working clock shed before the hint does
/// (#5914), so the steer stays reachable on a 120-column row that cannot
/// also hold the stopwatch.
#[test]
fn double_tap_window_shows_the_send_now_hint() {
    let mut app = working_app();
    app.arm_double_tap_window();
    let rows = draw(&mut app, 120, 32);
    let composer = app.viewport.last_composer_area.unwrap().bottom() as usize;
    assert!(
        rows[composer].contains("Enter again to send now · Ctrl+Enter steers"),
        "{}",
        rows[composer]
    );
    assert!(!rows[composer].contains("Esc to interrupt"));
}

/// `tui.posture_bar` / `tui.metrics_line` (#5950): `hidden` gives a row
/// back to the transcript — one row per hidden preset, two for both — and
/// `compact` keeps the row with its first shed rungs already gone. Every
/// other row of the frame stays where it was, so the composer is never
/// displaced by the choice.
#[test]
fn row_presets_reclaim_rows_and_quiet_them_in_the_composed_frame() {
    use codewhale_runtime::config::ChromeRowPreset;
    // 160 columns joins the blocker sizes above: the working clock's two
    // halves only both fit beside the pinned unenforced-scope permission
    // chip from that width up, and this test asserts the full row's clocks.
    let (width, height) = (160u16, 32u16);
    let posture_row = |rows: &[String]| rows.iter().position(|row| row.contains("● "));
    let metrics_row = |rows: &[String]| rows.iter().position(|row| row.contains("context "));

    let mut app = working_app();
    let full = draw(&mut app, width, height);
    let posture = posture_row(&full).expect("full frame paints the posture bar");
    let metrics = metrics_row(&full).expect("full frame paints the metrics line");
    assert_eq!(
        metrics,
        posture + 1,
        "the metrics line sits under the posture bar"
    );
    // The full row's live facts: a turn clock (this fixture waits on
    // sub-agents, so #5914 words it `sub-agents underway` rather than
    // `working`), the live counts and the hint — the three compact drops.
    assert!(full[posture].contains("1m 15s"), "{:?}", full[posture]);
    assert!(full[posture].contains("2 agents"), "{:?}", full[posture]);
    assert!(
        full[posture].contains("Esc to interrupt"),
        "{:?}",
        full[posture]
    );
    assert!(full[metrics].contains("tok/s"), "{:?}", full[metrics]);

    // Hide the posture bar: the metrics line takes its row, and the
    // transcript above gains one.
    app.posture_bar = ChromeRowPreset::Hidden;
    let rows = draw(&mut app, width, height);
    assert_eq!(posture_row(&rows), None, "no posture bar: {rows:#?}");
    assert_eq!(
        metrics_row(&rows),
        Some(metrics),
        "the metrics line keeps its row"
    );
    assert_eq!(
        count_rows_containing(&rows, "context "),
        1,
        "the context reading is still painted once"
    );

    // Hide both: two rows reclaimed.
    app.metrics_line = ChromeRowPreset::Hidden;
    let rows = draw(&mut app, width, height);
    assert_eq!(posture_row(&rows), None);
    assert_eq!(metrics_row(&rows), None);
    assert_eq!(count_rows_containing(&rows, "deepseek-v4-pro"), 0);

    // Compact both: the rows are back, quieter — the posture and the
    // route/reading/price and measured performance, without secondary counts.
    app.posture_bar = ChromeRowPreset::Compact;
    app.metrics_line = ChromeRowPreset::Compact;
    let rows = draw(&mut app, width, height);
    let posture = posture_row(&rows).expect("compact paints the posture bar");
    let metrics = metrics_row(&rows).expect("compact paints the metrics line");
    assert_eq!(metrics, posture + 1);
    let (mode, permission) = crate::tui::underwater::posture_chips(&app);
    assert!(rows[posture].contains(permission.expect("permission chip").0.as_ref()));
    assert!(rows[posture].contains(mode.expect("mode chip").0.as_ref()));
    for gone in ["working", "2 agents", "Esc to interrupt"] {
        assert!(
            !rows[posture].contains(gone),
            "{gone} in {:?}",
            rows[posture]
        );
    }
    assert!(
        rows[metrics].contains("deepseek-v4-pro"),
        "{:?}",
        rows[metrics]
    );
    let pct = super::info_context_percent(&app);
    assert!(
        rows[metrics].contains(&format!("context {pct}%")),
        "{:?}",
        rows[metrics]
    );
    assert!(rows[metrics].contains("ttft 400ms"), "{}", rows[metrics]);
    assert!(rows[metrics].contains("40 avg tok/s"), "{}", rows[metrics]);
    for gone in [
        "↓ 1.2K",
        crate::tui::shell_key_routing::info_help_hint(app.ui_locale).as_str(),
    ] {
        assert!(
            !rows[metrics].contains(gone),
            "{gone} in {:?}",
            rows[metrics]
        );
    }
}

/// Exercise live preset transitions on the same terminal and App, including
/// restoration. Fresh buffers alone cannot expose stale chrome or hitboxes.
#[test]
fn statusline_full_frame_presets_preserve_transcript_composer_and_hitboxes() {
    use crate::tui::tideline::{InteractionAction, InteractionTargetId};
    use codewhale_runtime::config::{ChromeRowPreset, StatusItem};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Position;

    for (width, height) in [(40, 12), (60, 16), (80, 24), (100, 32)] {
        let mut app = frame_app();
        app.history = vec![HistoryCell::User {
            content: (0..60)
                .map(|row| format!("transcript-line-{row:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }];
        app.resync_history_revisions();
        app.input = "ab中文".to_string();
        app.cursor_position = app.input.chars().count();
        app.composer_border = true;
        app.status_items = StatusItem::default_footer();
        app.posture_bar = ChromeRowPreset::Full;
        app.metrics_line = ChromeRowPreset::Full;
        app.session_metrics
            .record_model_call(1_200, 29_600, Some(400), Some(30_000));
        app.session.last_completion_tokens = Some(1_200);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let (full, _) = draw_into(&mut app, &mut terminal);
        let full_buffer = terminal.backend().buffer().clone();
        let full_transcript = app.viewport.last_transcript_area.unwrap();
        let full_composer = app.viewport.last_composer_area.unwrap();
        let full_visible = count_rows_containing(&full, "transcript-line-");
        assert!(
            full_visible > 0 && full_visible < 60,
            "populated scrollback fixture"
        );

        for (name, posture, metrics, reclaimed) in [
            ("full", ChromeRowPreset::Full, ChromeRowPreset::Full, 0),
            (
                "metrics-hidden",
                ChromeRowPreset::Full,
                ChromeRowPreset::Hidden,
                1,
            ),
            (
                "posture-hidden",
                ChromeRowPreset::Hidden,
                ChromeRowPreset::Full,
                1,
            ),
            (
                "both-hidden",
                ChromeRowPreset::Hidden,
                ChromeRowPreset::Hidden,
                2,
            ),
            (
                "both-compact",
                ChromeRowPreset::Compact,
                ChromeRowPreset::Compact,
                0,
            ),
            (
                "full-restored",
                ChromeRowPreset::Full,
                ChromeRowPreset::Full,
                0,
            ),
        ] {
            app.posture_bar = posture;
            app.metrics_line = metrics;
            let (rows, cursor) = draw_into(&mut app, &mut terminal);
            let evidence = format!("{width}x{height} {name}\n{}", rows.join("\n"));
            eprintln!("{evidence}");
            let transcript = app.viewport.last_transcript_area.unwrap();
            let composer = app.viewport.last_composer_area.unwrap();
            assert_eq!(
                transcript.height,
                full_transcript.height + reclaimed,
                "{evidence}"
            );
            assert_eq!(composer.y, full_composer.y + reclaimed, "{evidence}");
            assert_eq!(composer.height, full_composer.height, "{evidence}");
            assert_eq!(transcript.bottom(), composer.y, "{evidence}");
            assert_eq!(
                count_rows_containing(&rows, "transcript-line-"),
                full_visible + usize::from(reclaimed),
                "{evidence}"
            );
            assert!(
                rows.iter().any(|row| row.contains("transcript-line-59")),
                "latest transcript survives: {evidence}"
            );
            assert_eq!(app.input, "ab中文");
            assert!(
                rows.iter().all(|row| !row.contains('\u{fffd}')),
                "{evidence}"
            );

            let cursor = cursor.expect("active composer exposes its caret");
            let inner = app.viewport.last_composer_content.unwrap();
            let text = crate::tui::widgets::composer_content_geometry(inner, false).text_area;
            let submit = crate::tui::widgets::active_composer_submit_rect(&app, composer).unwrap();
            assert!(
                text.contains(Position::from(cursor)),
                "caret inside text grid: {evidence}"
            );
            assert_eq!(
                cursor.0,
                text.x + 6,
                "two ASCII and two wide glyphs: {evidence}"
            );
            assert!(
                !submit.contains(Position::from(cursor)),
                "caret cannot hit Send: {evidence}"
            );
            assert!(terminal.backend().cursor_visible());
            terminal
                .backend_mut()
                .assert_cursor_position(Position::from(cursor));
            // Terminal backends skip continuation cells covered by a wide
            // glyph. TestBackend can retain a prior border in those cells;
            // it is not visible terminal text after the wide glyph is drawn.
            let mut painted_input = String::new();
            let mut x = text.x;
            while x < cursor.0 {
                let symbol = terminal.backend().buffer()[(x, cursor.1)].symbol();
                painted_input.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            assert_eq!(painted_input, "ab中文", "{evidence}");
            app.viewport.composer_click_trace = None;
            assert!(crate::tui::mouse_ui::handle_composer_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: cursor.0,
                    row: cursor.1,
                    modifiers: KeyModifiers::NONE,
                }
            ));
            assert_eq!(
                app.cursor_position,
                app.input.chars().count(),
                "CJK mouse/caret boundary: {evidence}"
            );

            let context = app
                .viewport
                .interaction_targets
                .iter()
                .find(|target| target.id == InteractionTargetId::HEADER_CONTEXT);
            let model = app
                .viewport
                .interaction_targets
                .iter()
                .find(|target| target.id == InteractionTargetId::HEADER_MODEL);
            if metrics == ChromeRowPreset::Hidden {
                assert!(app.viewport.last_infoline_hitboxes.is_empty(), "{evidence}");
                assert!(
                    context.is_none() && model.is_none(),
                    "hidden chrome has no stale actions: {evidence}"
                );
                assert_eq!(count_rows_containing(&rows, "context "), 0, "{evidence}");
            } else {
                let context = context.expect("visible context has an inspector hitbox");
                let model = model.expect("visible model has a picker hitbox");
                assert_eq!(
                    context.mouse_action,
                    Some(InteractionAction::InspectContext)
                );
                assert_eq!(model.mouse_action, Some(InteractionAction::OpenModelPicker));
                for target in [context, model] {
                    assert_eq!(target.keyboard_action, target.mouse_action);
                    assert_eq!(target.area.y, height - 1, "{evidence}");
                    assert_eq!(
                        app.viewport
                            .interaction_targets
                            .target_at(target.area.x, target.area.y),
                        Some(target)
                    );
                    assert!(!composer.intersects(target.area), "{evidence}");
                }
                assert_eq!(count_rows_containing(&rows, "context 0%"), 1, "{evidence}");
            }
            if metrics == ChromeRowPreset::Compact {
                if width >= 60 {
                    assert!(rows[usize::from(height - 1)].contains("ttft"), "{evidence}");
                    assert!(
                        rows[usize::from(height - 1)].contains("avg tok/s"),
                        "{evidence}"
                    );
                }
                for shed in ["↓ 1.2K", "Ctrl+/ help"] {
                    assert!(!rows[usize::from(height - 1)].contains(shed), "{evidence}");
                }
            }
            if name == "full-restored" {
                let restored = terminal.backend().buffer();
                assert_eq!(restored.area, full_buffer.area);
                for y in full_buffer.area.y..full_buffer.area.bottom() {
                    let mut x = full_buffer.area.x;
                    while x < full_buffer.area.right() {
                        let expected = &full_buffer[(x, y)];
                        assert_eq!(
                            &restored[(x, y)],
                            expected,
                            "restoration leaves no stale visible cell or style at ({x}, {y}): {evidence}"
                        );
                        // Covered continuation cells are not rendered by a
                        // terminal, so TestBackend's retained contents there
                        // are not part of visible restoration.
                        x += unicode_width::UnicodeWidthStr::width(expected.symbol()).max(1) as u16;
                    }
                }
            }
        }
    }
}

/// #5976 intentionally supersedes #5950's blanket custom-cost omission:
/// missing coverage is evidence, independent of whether today's route is known.
#[test]
fn statusline_full_frame_custom_cost_preserves_evidence_and_width_shedding() {
    use codewhale_config::ProviderKind;
    use codewhale_engine::route_billing::{BillingPresentation, UsageChip};
    use codewhale_runtime::config::{ChromeRowPreset, StatusItem};

    for (width, height) in [(40, 12), (60, 16), (80, 24), (100, 32)] {
        let mut app = frame_app();
        app.history = vec![HistoryCell::User {
            content: "Review saved usage".to_string(),
        }];
        app.resync_history_revisions();
        app.set_provider_identity(ProviderKind::Custom, "my-gateway");
        app.active_route_base_url = "https://gateway.example/v1".to_string();
        app.model = "vendor-model-x".to_string();
        app.reasoning_effort = codewhale_engine::reasoning_preference::ReasoningEffort::High;
        app.billing_presentation = BillingPresentation::Unknown;
        app.session.cost_coverage_unknown_legacy = true;
        app.status_items = vec![StatusItem::ContextPercent, StatusItem::Cost];
        app.posture_bar = ChromeRowPreset::Hidden;
        app.metrics_line = ChromeRowPreset::Compact;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let expected = "cost: unknown (saved coverage unavailable)";
        assert_eq!(app.api_provider, ProviderKind::Custom);
        assert!(matches!(app.cumulative_usage_chip(), UsageChip::Unknown(_)));
        assert_eq!(super::session_cost_label(&app), expected);
        let (rows, _) = draw_into(&mut app, &mut terminal);
        eprintln!("{width}x{height} custom-saved-unknown\n{}", rows.join("\n"));
        let metrics = rows.last().unwrap();
        assert!(metrics.contains("context 0%"), "{metrics}");
        if width >= 60 {
            assert!(metrics.contains(expected), "{width}: {metrics}");
        } else {
            // The existing whole-segment shed ladder cannot fit the reason
            // plus context in 40 columns. It must not invent a zero price.
            assert!(!metrics.contains("cost:"), "{metrics}");
            assert!(!metrics.contains('$'), "{metrics}");
        }
        assert_eq!(
            super::session_cost_label(&app),
            expected,
            "shedding changes no receipt"
        );

        app.status_items.retain(|item| *item != StatusItem::Cost);
        let (hidden, _) = draw_into(&mut app, &mut terminal);
        assert!(!hidden.last().unwrap().contains("cost:"));
        assert_eq!(
            super::session_cost_label(&app),
            expected,
            "a toggle changes no receipt"
        );
        app.status_items.push(StatusItem::Cost);
        assert_eq!(
            draw_into(&mut app, &mut terminal).0,
            rows,
            "live cost toggle restores the same frame"
        );

        app.session.cost_coverage_unknown_legacy = false;
        app.session.cost_unpriced_turns = 1;
        app.session.cost_unpriced_reasons.insert(
            codewhale_engine::pricing::UnpricedReason::NoPricingRow
                .label()
                .to_string(),
        );
        let missing_rate = "cost: unknown (rate unavailable)";
        assert_eq!(super::session_cost_label(&app), missing_rate);
        let (unpriced, _) = draw_into(&mut app, &mut terminal);
        eprintln!(
            "{width}x{height} custom-unpriced-turn\n{}",
            unpriced.join("\n")
        );
        if width >= 60 {
            assert!(
                unpriced.last().unwrap().contains(missing_rate),
                "{unpriced:?}"
            );
        } else {
            assert!(!unpriced.last().unwrap().contains("cost:"), "{unpriced:?}");
        }
        assert_eq!(app.session.cost_unpriced_turns, 1);
        assert!(matches!(app.cumulative_usage_chip(), UsageChip::Unknown(_)));

        // An unavailable effective effort is omitted independently of the
        // explicit missing-cost reason. At 100 columns both model and reason fit.
        if width == 100 {
            app.status_items.insert(0, StatusItem::Model);
            let (with_model, _) = draw_into(&mut app, &mut terminal);
            eprintln!(
                "{width}x{height} custom-model-and-unpriced-turn\n{}",
                with_model.join("\n")
            );
            let metrics = with_model.last().unwrap();
            assert!(metrics.contains("vendor-model-x"), "{metrics}");
            assert!(metrics.contains(missing_rate), "{metrics}");
            assert_eq!(app.provable_reasoning_effort_label(), None);
            assert!(!metrics.contains("high"), "{metrics}");
            assert!(!metrics.contains("effective unavailable"), "{metrics}");
            app.status_items.remove(0);
        }

        app.session.cost_unpriced_turns = 0;
        app.session.cost_unpriced_reasons.clear();
        app.session.cost_priced_turns = 1;
        app.session.session_cost = 0.42;
        let (priced, _) = draw_into(&mut app, &mut terminal);
        eprintln!(
            "{width}x{height} custom-recorded-price\n{}",
            priced.join("\n")
        );
        assert!(matches!(app.cumulative_usage_chip(), UsageChip::Money(_)));
        assert!(
            priced.last().unwrap().contains("0.42"),
            "real fixture price survives Custom: {priced:?}"
        );
        app.set_provider_identity(ProviderKind::Deepseek, "deepseek");
        app.active_route_base_url = "https://api.deepseek.com/v1".to_string();
        app.model = "deepseek-v4-pro".to_string();
        app.billing_presentation = BillingPresentation::Metered;
        let (first_party, _) = draw_into(&mut app, &mut terminal);
        assert_eq!(
            first_party.last(),
            priced.last(),
            "historical price does not follow today's provider"
        );
        app.set_provider_identity(ProviderKind::Custom, "my-gateway");
        app.active_route_base_url = "https://gateway.example/v1".to_string();
        app.model = "vendor-model-x".to_string();
        app.billing_presentation = BillingPresentation::Unknown;
        let (restored_custom, _) = draw_into(&mut app, &mut terminal);
        assert_eq!(restored_custom.last(), priced.last());
    }
}

/// Feed real estimated conversation tokens through the composed frame, then
/// change only the route window. Crossing the warning threshold must repaint
/// both text and ink without moving the transcript, composer or inspector.
#[test]
fn statusline_full_frame_context_reading_updates_below_and_at_warning() {
    use crate::tui::tideline::InteractionTargetId;
    use codewhale_models::{ContentBlock, Message, Role};
    use codewhale_palette::ChromeInk;
    use codewhale_runtime::config::{ChromeRowPreset, StatusItem};

    for (width, height) in [(40, 12), (60, 16), (80, 24), (100, 32)] {
        let mut app = frame_app();
        app.history = vec![HistoryCell::User {
            content: "Keep the context reading visible".to_string(),
        }];
        app.resync_history_revisions();
        app.api_messages = std::sync::Arc::new(vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "context ".repeat(400),
                cache_control: None,
            }],
        }]);
        app.input = "next".to_string();
        app.cursor_position = app.input.chars().count();
        app.status_items = StatusItem::default_footer();
        app.posture_bar = ChromeRowPreset::Full;
        app.metrics_line = ChromeRowPreset::Full;
        let (used, _, _) = super::context_usage_snapshot(&app).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut first_geometry = None;
        let mut first_frame = None;

        for pct in [0u8, 10, 79, 80, 10, 0] {
            let window = if pct == 0 {
                // 0.1% rounds to the displayed 0% even with real context.
                used as u64 * 1_000
            } else {
                (used as f64 * 100.0 / f64::from(pct)).round() as u64
            };
            app.active_route_limits = Some(codewhale_config::route::RouteLimits {
                context_tokens: Some(window),
                ..Default::default()
            });
            assert_eq!(super::info_context_percent(&app), pct);
            let (rows, cursor) = draw_into(&mut app, &mut terminal);
            let evidence = format!("{width}x{height} context-{pct}\n{}", rows.join("\n"));
            eprintln!("{evidence}");
            let label = format!("context {pct}%");
            assert_eq!(count_rows_containing(&rows, &label), 1, "{evidence}");
            assert!(
                rows.iter()
                    .any(|row| row.contains("Keep the context reading visible")),
                "{evidence}"
            );
            let context = app
                .viewport
                .interaction_targets
                .iter()
                .find(|target| target.id == InteractionTargetId::HEADER_CONTEXT)
                .expect("the visible reading stays inspectable");
            assert_eq!(context.area.y, height - 1, "{evidence}");
            // Attention, not Failure: the posture bar calls this same >= 80
            // threshold Attention, and the two must not disagree one row apart.
            let value_ink = if pct >= 80 {
                ChromeInk::Attention
            } else {
                ChromeInk::Info
            };
            let label_ink = if pct >= 80 {
                ChromeInk::Attention
            } else {
                ChromeInk::Metadata
            };
            let buffer = terminal.backend().buffer();
            // The value starts after the "context " label.
            for (x, ink) in [(context.area.x, label_ink), (context.area.x + 8, value_ink)] {
                assert_eq!(
                    buffer[(x, context.area.y)].fg,
                    codewhale_palette::grammar::chrome_style(&app.ui_theme, ink)
                        .fg
                        .unwrap(),
                    "warning ink must also clear after 80%: {evidence}",
                );
            }
            let geometry = (
                app.viewport.last_transcript_area,
                app.viewport.last_composer_area,
                cursor,
            );
            if let Some(first) = first_geometry {
                assert_eq!(geometry, first, "{evidence}");
            } else {
                first_geometry = Some(geometry);
            }
            if pct == 0 {
                if let Some(first) = first_frame.as_ref() {
                    assert_eq!(
                        terminal.backend().buffer(),
                        first,
                        "returning to 0% clears the warning frame and ink"
                    );
                } else {
                    first_frame = Some(terminal.backend().buffer().clone());
                }
            }
        }
    }
}
