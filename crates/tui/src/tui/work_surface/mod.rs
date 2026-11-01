//! Ocean Work Graph surface ownership.
//!
//! This is called the "workbar" or the "work surface". Fresh settings default
//! to `Bottom` (round 3, 2026-09-01); `Top`, `Left`, and `Right` remain
//! supported and `Off` hides it. It is not the header
//! ([`crate::tui::underwater`]) and not the footer.
//!
//! Two settings are orthogonal and are routinely mixed up:
//!
//! - **placement** — where it renders. `Bottom` (fresh default) | `Top` |
//!   `Left` | `Right` | `Off`. Drag-resizing the divider persists
//!   `work_surface_top_height` (5..=16) or `work_surface_side_width`
//!   (26..=80) to `settings.toml`.
//! - **panel** — what it shows. [`RailPanel`]: `Tasks` (default) | `Agents` |
//!   `Background` | `Terminal` | `Files` | `Notepad` | `Context` | `Git` | `Price`, from
//!   the `rail_panel` setting. The legacy `sidebar_focus` key migrates into
//!   it.
//!
//! So the word "Pinned" on screen is a PANEL name, not a state.
//!
//! ## Auto-fit by placement
//!
//! Placement changes *which axis is the ceiling*, not the content rule:
//!
//! | Placement | Ceiling | Auto-fit | Empty |
//! |---|---|---|---|
//! | `Top` | `top_height` (rows) | content rows + divider, clamped to ceiling | `height() == 0` |
//! | `Left`/`Right` | `side_width` (cols) | full chat height at that width | no column reserved |
//! | `Off` | — | — | nothing |
//!
//! Shared rules: content drives size; the setting is a ceiling, never padding;
//! empty work is not a rail. Top never paints a chrome panel title (a checklist
//! reads as a checklist); side rails are named by their content's own heading
//! row (`Work · …`, `▾ Subagents N`, `Goal: …`) except Context, which keeps a
//! muted panel title over its fact list. Narrow hosts that cannot fit a side
//! column fall back to Top, where height auto-fit takes over.
//!
//! ## Row lifetime
//!
//! The strip is a standing register of this session's work, not a live-only
//! view. A to-do or sub-agent row appears when the work exists and stays for
//! the rest of the session after it settles — completion is quiet (glyph,
//! tone, frozen receipt), never an eviction, and the active goal title
//! outlives the work under it. Only transient receipts (aggregated file
//! activity, settled operations) expire on the #4688/#4690 lifetimes.
//! Auto-fit and the row budget decide how many rows are *visible* at once;
//! they never decide membership.
//!
//! ## Rows are objects — in every panel
//!
//! Tasks, Agents, and Pinned all render through one row/hitbox pipeline:
//! every visible work row is selectable, hoverable, and clickable, and its
//! primary action opens the row's world (agent transcript / work inspector).
//! Keyboard Enter and mouse click dispatch identically. Context is the one
//! line-list panel; it holds facts, not rows.
//!
//! Height is decided once per frame by [`render::height`]; the row budget it is
//! given comes from `crate::tui::ui::rail_row_budget`, which is its only
//! production caller.
//!
//! Placement, scrolling, selection, and pager ownership remain local to this
//! component. Every visible work row derives from the active-session graph.

mod input;
mod interaction;
mod model;
mod render;
mod terminal;
mod views;

pub use input::{cycle_view, enter_agents, handle_key, handle_mouse};
pub(crate) use interaction::{agent_details_closed, release_focus, select_dock_panel};
pub use model::{RailPanel, WorkSurfacePlacement, WorkSurfaceState};
pub(crate) use render::collapse_strip;
pub use render::{height, render, split_chat};
pub(crate) use terminal::{handle_paste as handle_terminal_paste, poll as poll_terminal};

#[cfg(test)]
mod tests {
    use super::WorkSurfacePlacement;
    use std::path::PathBuf;

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{Terminal, backend::TestBackend};

    use crate::tui::app::{
        AgentCurrentActivity, AgentCurrentActivityStatus, App, SidebarRowAction, ToolDetailRecord,
        TuiOptions,
    };
    use crate::tui::golden_harness::assert_matches_golden;
    use crate::tui::history::{
        FileMutationReceipt, GenericToolCell, HistoryCell, PatchSummaryCell, ToolCell, ToolStatus,
    };
    use codewhale_agent_contract::role::FleetRole;
    use codewhale_config::ProviderKind;
    use codewhale_engine::tools::subagent::{
        AgentWorkerStatus, MailboxMessage, SubAgentAssignment, SubAgentResult, SubAgentStatus,
    };
    use codewhale_engine::tools::todo::TodoStatus;
    use codewhale_engine::work_graph::{
        AcceptanceRequirement, ChangeCtx, EdgeKind, EvidenceKindTag, NodeKind, NodeState,
        OperationBinding, OperationOwnerSnapshot, OwnerState, Provenance, WorkEdge, WorkEdgeId,
        WorkGraph, WorkGraphChange, WorkNode, WorkNodeId,
    };
    use codewhale_runtime::config::Config;

    const SESSION: &str = "work-surface-test";

    fn app() -> App {
        let options = TuiOptions {
            use_mouse_capture: true,
            max_subagents: 4,
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = codewhale_localization::Locale::En;
        // Dogfood guard: App::new reads the developer's real settings.toml,
        // and the 0.9.4 migration maps a legacy sidebar_focus onto the rail
        // panel. These tests exercise the Tasks panel's row machinery, so
        // pin it rather than depend on the host file.
        app.work_surface.panel = super::RailPanel::Tasks;
        // Not an explicit pick: the auto rule opens the agents view when a
        // fixture caches a running worker, exactly as the product does.
        app.work_surface.explicit_view = false;
        // Most tests in this module predate the fresh left-rail default and
        // exercise the Top strip's height, divider, overflow, and row layout.
        // Pin both requested and effective placement; dedicated placement
        // tests override these fields explicitly.
        app.work_surface.placement = WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = WorkSurfacePlacement::Top;
        app
    }

    /// The row budget `ui::render` would hand the rail on a terminal of this
    /// height with real work on screen. Calls the production formula rather
    /// than restating it, so a change to the chrome accounting shows up here
    /// instead of silently diverging. The idle-empty budget (where the
    /// ambient floor bites) is covered end-to-end in `ui::tests`.
    fn working_budget(app: &App, terminal_height: u16) -> u16 {
        crate::tui::ui::rail_row_budget(app, 80, terminal_height, false)
    }

    /// A budget wide enough never to bind, for tests about something else.
    const AMPLE_BUDGET: u16 = u16::MAX;

    fn add_todos(app: &mut App, count: usize) {
        let mut todos = app.todos.try_lock().expect("todos");
        for index in 0..count {
            todos.add(
                format!("work item {index}"),
                if index == 0 {
                    TodoStatus::InProgress
                } else {
                    TodoStatus::Pending
                },
            );
        }
    }

    fn operation_graph(state: NodeState) -> codewhale_engine::work_graph::WorkGraphSnapshot {
        let objective = WorkNodeId::derive(SESSION, "objective");
        let operation = WorkNodeId::derive(SESSION, "operation");
        let ctx = |now| ChangeCtx {
            session_id: SESSION.to_string(),
            now,
            idempotency_key: None,
        };
        let node = |id: WorkNodeId, kind, title: &str, now| WorkNode {
            id,
            kind,
            title: title.to_string(),
            state: NodeState::Ready,
            acceptance: Vec::new(),
            binding: None,
            evidence: None,
            provenance: Provenance::RuntimeReconcile {
                source: "test-owner".to_string(),
                observed_at: now,
            },
            created_at: now,
            updated_at: now,
        };
        let mut graph = WorkGraph::new();
        graph
            .apply(
                WorkGraphChange::AddNode {
                    node: node(objective.clone(), NodeKind::Objective, "Ship v0.9.1", 1),
                },
                ctx(1),
            )
            .expect("objective");
        graph
            .apply(
                WorkGraphChange::AddNode {
                    node: node(
                        operation.clone(),
                        NodeKind::Operation,
                        "Verify installed build",
                        2,
                    ),
                },
                ctx(2),
            )
            .expect("operation");
        graph
            .apply(
                WorkGraphChange::AddEdge {
                    edge: WorkEdge {
                        id: WorkEdgeId::derive(SESSION, "contains"),
                        kind: EdgeKind::Contains,
                        from: objective,
                        to: operation.clone(),
                    },
                },
                ctx(3),
            )
            .expect("contains");
        graph
            .apply(
                WorkGraphChange::BindOperation {
                    node: operation.clone(),
                    binding: OperationBinding {
                        external: "shell:shell_1234abcd".to_string(),
                        durable: false,
                        last_observation: None,
                    },
                },
                ctx(4),
            )
            .expect("binding");
        if state != NodeState::Ready {
            graph
                .apply(
                    WorkGraphChange::UpdateNode {
                        id: operation,
                        patch: codewhale_engine::work_graph::WorkNodePatch {
                            state: Some(state),
                            ..codewhale_engine::work_graph::WorkNodePatch::default()
                        },
                    },
                    ctx(5),
                )
                .expect("state");
        }
        graph.into_snapshot()
    }

    fn restore_graph(app: &mut App, graph: &codewhale_engine::work_graph::WorkGraphSnapshot) {
        app.current_session_id = Some(SESSION.to_string());
        app.runtime_services
            .work
            .as_ref()
            .expect("Work Graph runtime")
            .restore(
                SESSION,
                Some(graph),
                &codewhale_engine::work_graph::project_todos(graph),
                &codewhale_engine::work_graph::project_plan(graph),
            )
            .expect("restore graph");
    }

    fn restore_saved_graph(app: &mut App, graph: &codewhale_engine::work_graph::WorkGraphSnapshot) {
        app.current_session_id = Some(SESSION.to_string());
        let state = codewhale_engine::session_manager::SessionWorkState {
            graph: Some(graph.clone()),
            todos: codewhale_engine::work_graph::project_todos(graph),
            plan: codewhale_engine::work_graph::project_plan(graph),
        };
        app.restore_work_state(SESSION, std::path::Path::new("."), Some(&state))
            .expect("restore saved graph");
    }

    fn render_text(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::render(frame, frame.area(), app))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn render_golden_text(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::render(frame, frame.area(), app))
            .expect("draw");
        format!("{}\n", terminal_text(&terminal))
    }

    #[test]
    fn scheduled_automations_do_not_create_background_work() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.automation_panel.active_automations = 2;
        assert!(!super::model::background_has_live_work(&mut app));
    }

    #[test]
    fn projection_keeps_every_legacy_todo_as_a_graph_row() {
        let mut app = app();
        add_todos(&mut app, 4);

        let rows = super::model::project(&mut app);

        assert!(
            rows[0].label.starts_with("Work · Running:")
                || rows[0]
                    .label
                    .starts_with("Work · 1 active · 0 needs input · 3 ready"),
            "unexpected heading {}",
            rows[0].label
        );
        for index in 0..4 {
            assert!(
                rows.iter()
                    .any(|row| row.label == format!("work item {index}"))
            );
        }
        assert!(rows.iter().all(|row| !row.id.0.starts_with("todo:")));
    }

    #[test]
    fn coordination_projection_is_one_selectable_work_row_with_shared_details() {
        use codewhale_engine::tools::subagent::CoordinationDetailProjection;
        use codewhale_engine::tools::subagent::coord::{
            CoordinationDetailMetrics, DecisionRecord, DecisionStatus,
        };

        let mut app = app();
        app.coordination_detail = Some(CoordinationDetailProjection {
            schema_version: 1,
            sequence: 7,
            decisions: vec![DecisionRecord {
                decision_id: "decision-work".to_string(),
                subject: "coordination row".to_string(),
                status: DecisionStatus::Accepted,
                owner: "release-owner".to_string(),
                scope: Vec::new(),
                constraints: vec!["PRIVATE-TRANSCRIPT-MARKER".to_string()],
                evidence_handles: Vec::new(),
                version: 2,
                sequence: 7,
            }],
            write_claims: Vec::new(),
            reconciliations: Vec::new(),
            context_projections: Vec::new(),
            contentions: Vec::new(),
            metrics: CoordinationDetailMetrics {
                hottest_paths: Vec::new(),
                package_or_module_growth: None,
                route_or_cost: None,
                note: "No active claims".to_string(),
            },
            bounded: true,
            limit: 24,
            process_lock_held: true,
            process_lock_note: None,
        });

        let rows = super::model::project(&mut app);
        assert_eq!(
            rows[0].label,
            "Work · 0 active · 0 needs input · 0 ready · 1 recent"
        );
        let row = rows
            .iter()
            .find(|row| row.id.0 == "coordination")
            .expect("coordination Work row");
        assert_eq!(row.label, "Coordination Work");
        assert_eq!(row.detail, "1 decisions · 0 contentions · 0 reconciled");
        let Some(SidebarRowAction::InspectWork { title, body, .. }) = row.primary_action.as_ref()
        else {
            panic!("coordination row must open the shared Work inspector");
        };
        assert_eq!(title, "Coordination Work");
        assert!(body.contains("decision-work · coordination row"), "{body}");
        assert!(
            body.contains("status accepted · owner release-owner · version 2"),
            "{body}"
        );
        assert!(!body.contains("PRIVATE-TRANSCRIPT-MARKER"), "{body}");

        app.work_surface.placement = WorkSurfacePlacement::Right;
        app.work_surface.effective_placement = WorkSurfacePlacement::Right;
        let narrow = render_text(&mut app, 32, 4);
        assert!(narrow.contains("Coordination Work"), "{narrow}");
        let _ = super::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
        );
        let action = super::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .expect("Work surface handled Enter")
            .expect("coordination inspector action");
        assert!(matches!(action, SidebarRowAction::InspectWork { .. }));
    }

    #[test]
    fn empty_coordination_projection_does_not_create_work_chrome() {
        use codewhale_engine::tools::subagent::CoordinationDetailProjection;
        use codewhale_engine::tools::subagent::coord::{
            ContextProjectionReceipt, CoordinationDetailMetrics,
        };

        let mut app = app();
        app.coordination_detail = Some(CoordinationDetailProjection {
            schema_version: 1,
            sequence: 3,
            decisions: Vec::new(),
            write_claims: Vec::new(),
            reconciliations: Vec::new(),
            context_projections: ["agent-a", "agent-b", "agent-c"]
                .into_iter()
                .enumerate()
                .map(|(index, child_id)| ContextProjectionReceipt {
                    child_id: child_id.to_string(),
                    decision_ids: Vec::new(),
                    projected_bytes: 0,
                    deduplicated: 0,
                    omitted: 0,
                    sequence: u64::try_from(index + 1).expect("small fixture sequence"),
                })
                .collect(),
            contentions: Vec::new(),
            metrics: CoordinationDetailMetrics {
                hottest_paths: Vec::new(),
                package_or_module_growth: None,
                route_or_cost: None,
                note: "growth and route/cost stay null when the coordination ledger has no authoritative source".to_string(),
            },
            bounded: true,
            limit: 24,
            process_lock_held: true,
            process_lock_note: None,
        });

        let rows = super::model::project(&mut app);
        assert!(
            rows.is_empty(),
            "zero-byte, no-decision coordination receipts must not create Work chrome: {rows:?}"
        );
    }

    #[test]
    fn nonempty_context_projection_remains_inspectable_work() {
        use codewhale_engine::tools::subagent::CoordinationDetailProjection;
        use codewhale_engine::tools::subagent::coord::{
            ContextProjectionReceipt, CoordinationDetailMetrics,
        };

        let mut app = app();
        app.coordination_detail = Some(CoordinationDetailProjection {
            schema_version: 1,
            sequence: 1,
            decisions: Vec::new(),
            write_claims: Vec::new(),
            reconciliations: Vec::new(),
            context_projections: vec![ContextProjectionReceipt {
                child_id: "agent-a".to_string(),
                decision_ids: vec!["decision-a".to_string()],
                projected_bytes: 32,
                deduplicated: 0,
                omitted: 0,
                sequence: 1,
            }],
            contentions: Vec::new(),
            metrics: CoordinationDetailMetrics {
                hottest_paths: Vec::new(),
                package_or_module_growth: None,
                route_or_cost: None,
                note: String::new(),
            },
            bounded: true,
            limit: 24,
            process_lock_held: true,
            process_lock_note: None,
        });

        let rows = super::model::project(&mut app);
        assert!(
            rows.iter().any(|row| row.id.0 == "coordination"),
            "non-empty context projection must remain inspectable: {rows:?}"
        );
    }

    #[test]
    fn current_blocked_contention_uses_attention_bucket_mark_and_tone() {
        use codewhale_engine::tools::subagent::CoordinationDetailProjection;
        use codewhale_engine::tools::subagent::coord::{
            CoordinationDetailMetrics, PersistedWriteClaim, WriteContentionDisposition,
            WriteContentionReceipt, WriteScopeClaim,
        };

        let mut app = app();
        app.coordination_detail = Some(CoordinationDetailProjection {
            schema_version: 1,
            sequence: 2,
            decisions: Vec::new(),
            write_claims: vec![PersistedWriteClaim {
                claim: WriteScopeClaim {
                    owner: "worker-a".to_string(),
                    roots: vec!["crates/tui".to_string()],
                    exact_files: Vec::new(),
                    contracts: vec!["ui-contract".to_string()],
                },
                sequence: 1,
                isolated_worktree: false,
                present_at_claim: Vec::new(),
            }],
            reconciliations: Vec::new(),
            context_projections: Vec::new(),
            contentions: vec![WriteContentionReceipt {
                claimant: "worker-b".to_string(),
                conflicting_owner: "worker-a".to_string(),
                roots: vec!["crates/tui".to_string()],
                exact_files: Vec::new(),
                contracts: vec!["ui-contract".to_string()],
                disposition: WriteContentionDisposition::BlockedPendingIsolationOrSerialization,
                resolution_sequence: None,
                sequence: 2,
            }],
            metrics: CoordinationDetailMetrics {
                hottest_paths: Vec::new(),
                package_or_module_growth: None,
                route_or_cost: None,
                note: "No authoritative metric source".to_string(),
            },
            bounded: true,
            limit: 24,
            process_lock_held: true,
            process_lock_note: None,
        });

        let rows = super::model::project(&mut app);
        assert_eq!(
            rows[0].label,
            "Work · Needs input: Coordination Work · 1 blocked"
        );
        let row = rows
            .iter()
            .find(|row| row.id.0 == "coordination")
            .expect("blocked coordination Work row");
        assert_eq!(row.mark, codewhale_tui_kit::glyphs::ATTENTION);
        assert_eq!(row.tone, super::model::WorkTone::Attention);
        assert_eq!(row.detail, "0 decisions · 1 contentions · 0 reconciled");
    }

    #[test]
    fn todos_share_one_canonical_work_projection_without_a_second_heading() {
        let mut app = app();
        {
            let mut todos = app.todos.try_lock().expect("todos");
            todos.add("finished".to_string(), TodoStatus::Completed);
            todos.add("current".to_string(), TodoStatus::InProgress);
            todos.add("next".to_string(), TodoStatus::Pending);
        }

        let rows = super::model::project(&mut app);

        assert!(
            rows[0].label.starts_with("Work · Running:")
                || rows[0].label.starts_with("Work · Ready:"),
            "expected actionable title heading, got {}",
            rows[0].label
        );
        assert_eq!(
            rows.iter()
                .skip(1)
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["finished", "current", "next"]
        );
    }

    #[test]
    fn top_surface_pins_one_progress_receipt_and_numbers_canonical_rows() {
        let mut app = app();
        {
            let mut todos = app.todos.try_lock().expect("todos");
            todos.add("finished".to_string(), TodoStatus::Completed);
            todos.add("current".to_string(), TodoStatus::InProgress);
            todos.add("next".to_string(), TodoStatus::Pending);
        }

        let text = render_text(&mut app, 80, 7);
        let done = format!("1 · {} finished", codewhale_tui_kit::glyphs::DONE);
        let current = format!("2 · {} current", codewhale_tui_kit::glyphs::SELECTION);
        let next = format!("3 · {} next", codewhale_tui_kit::glyphs::READY);

        assert!(text.contains("To-do · 1/3 · 2 left"), "{text:?}");
        assert_eq!(text.matches("To-do ·").count(), 1, "{text:?}");
        assert!(text.contains(&done), "{text:?}");
        assert!(text.contains(&current), "{text:?}");
        assert!(text.contains(&next), "{text:?}");
        assert!(
            text.find(&done) < text.find(&current) && text.find(&current) < text.find(&next),
            "canonical order drifted: {text:?}"
        );
        assert_eq!(app.work_surface.hitboxes.len(), 3);
        assert_eq!(app.work_surface.hitboxes[0].row_y, 2);
    }

    #[test]
    fn top_strip_auto_fits_step_count_up_to_caps() {
        // Two steps need four literal lines, but the readable surface floor
        // wins so the same saved size can also seat goal + Agent state.
        let mut two_steps = app();
        two_steps.work_surface.top_height = 8;
        add_todos(&mut two_steps, 2);
        let budget = working_budget(&two_steps, 40);
        assert_eq!(
            super::height(&mut two_steps, 100, 40, budget),
            super::model::TOP_HEIGHT_MIN
        );

        // Ten steps: content wants 12 lines, the default 8-line cap wins.
        let mut ten_steps = app();
        ten_steps.work_surface.top_height = 8;
        add_todos(&mut ten_steps, 10);
        let budget = working_budget(&ten_steps, 40);
        assert_eq!(super::height(&mut ten_steps, 100, 40, budget), 8);

        // Short terminal: the transcript's spare rows beat both content and
        // the configured cap. A 12-row terminal spends 1 on the header, 1 on
        // the phase strip and 3 on the bordered composer, and owes the
        // transcript its 3-row floor — so only 4 rows are spare. That is below
        // the readable floor, so the whole rail yields rather than painting a
        // divider over clipped work.
        let mut short_terminal = app();
        short_terminal.work_surface.top_height = 8;
        add_todos(&mut short_terminal, 10);
        let budget = working_budget(&short_terminal, 12);
        assert_eq!(super::height(&mut short_terminal, 100, 12, budget), 0);

        // Nothing to show: no strip at all.
        let mut empty = app();
        empty.work_surface.top_height = 8;
        assert_eq!(super::height(&mut empty, 100, 40, AMPLE_BUDGET), 0);
    }

    /// A strip that reports zero rows is not on screen, so the interaction
    /// state describing it must go with it. Stale hitboxes outlive the rows
    /// they described: the transcript rows that replaced the strip would keep
    /// routing clicks into a panel that is not there.
    #[test]
    fn a_yielded_strip_drops_its_interaction_state() {
        // Each case is a distinct zero-return inside `height`, and every one
        // of them has to tear down. `starve` turns a rendered strip into a
        // yielded one; the assertions are identical either way. The first two
        // are the returns this yield rule introduced — the ones that had no
        // teardown at all.
        type Starve = fn(&mut App) -> (u16, u16, u16);
        let cases: [(&str, Starve); 3] = [
            ("budget starves the Tasks strip", |_app| (100, 40, 0)),
            ("budget starves a switched-to panel", |app| {
                app.work_surface.panel = super::RailPanel::Tasks;
                (100, 40, 0)
            }),
            ("placement off", |app| {
                app.work_surface.placement = WorkSurfacePlacement::Off;
                (100, 40, AMPLE_BUDGET)
            }),
        ];

        for (label, starve) in cases {
            let mut app = app();
            app.work_surface.placement = WorkSurfacePlacement::Top;
            // `app()` reads the developer's real settings.toml. Pin the height
            // too, or the strip this test renders to earn its hitboxes depends
            // on whoever runs the suite.
            app.work_surface.top_height = 8;
            add_todos(&mut app, 4);

            // Earn a real strip, so the hitboxes under test are the ones the
            // renderer actually produces rather than a fixture's guess.
            render_text(&mut app, 100, 12);
            assert!(
                !app.work_surface.hitboxes.is_empty(),
                "{label}: setup never rendered a strip to tear down"
            );
            app.work_surface.focused = true;
            app.work_surface.resizing = true;
            app.work_surface.divider_hovered = true;

            let (width, height, budget) = starve(&mut app);
            assert_eq!(
                super::height(&mut app, width, height, budget),
                0,
                "{label}: expected the strip to yield"
            );
            assert!(
                app.work_surface.hitboxes.is_empty(),
                "{label}: left {} stale hitboxes behind",
                app.work_surface.hitboxes.len()
            );
            assert!(
                app.work_surface.last_area.is_none(),
                "{label}: stale last_area"
            );
            assert!(!app.work_surface.focused, "{label}: focus survived");
            assert!(!app.work_surface.resizing, "{label}: resize drag survived");
            assert!(
                !app.work_surface.divider_hovered,
                "{label}: divider hover survived"
            );
        }
    }

    /// `top_height` is a ceiling, not a fixed size. The compact floor must
    /// still seat the goal, work progress, and actionable rows; content longer
    /// than the ceiling is clamped rather than padded with blank water.
    #[test]
    fn a_short_top_height_caps_content_rather_than_collapsing() {
        let mut capped = app();
        capped.work_surface.placement = WorkSurfacePlacement::Top;
        capped.work_surface.panel = super::RailPanel::Tasks;
        capped.work_surface.top_height = super::model::TOP_HEIGHT_MIN;
        capped.composer_border = true;
        // Goal + several checklist rows: content wants more than the readable
        // floor, so the cap wins without hiding every actionable row.
        capped.goal.objective = Some("ship the release".to_string());
        add_todos(&mut capped, 6);
        let budget = working_budget(&capped, 40);
        assert_eq!(
            super::height(&mut capped, 100, 40, budget),
            super::model::TOP_HEIGHT_MIN,
            "short top_height is a cap the strip must fit under, not a cliff"
        );

        // Content shorter than the cap shrinks to the readable floor rather
        // than padding all the way out to the saved 8-row cap.
        let mut short = app();
        short.work_surface.placement = WorkSurfacePlacement::Top;
        short.work_surface.panel = super::RailPanel::Tasks;
        short.work_surface.top_height = 8;
        short.goal.objective = Some("one goal only".to_string());
        let budget = working_budget(&short, 40);
        let h = super::height(&mut short, 100, 40, budget);
        assert_eq!(h, super::model::TOP_HEIGHT_MIN);
    }

    /// Non-Tasks Top panels auto-fit the same way Tasks always did: content
    /// rows + divider, never a fixed four-row chrome band. An active goal
    /// adds exactly one title row (not a panel name).
    #[test]
    fn top_panel_auto_fits_content_like_tasks() {
        let mut pinned = app();
        pinned.work_surface.placement = WorkSurfacePlacement::Top;
        pinned.work_surface.panel = super::RailPanel::Tasks;
        pinned.work_surface.top_height = 12;
        pinned.goal.objective = Some("goal".to_string());
        add_todos(&mut pinned, 3);
        let budget = working_budget(&pinned, 40);
        let h = super::height(&mut pinned, 100, 40, budget);
        // goal title + 3 checklist + divider ≈ 5; must not be the old fixed 4,
        // and must not pad out to the 12-row cap.
        assert!(
            (4..=8).contains(&h),
            "Pinned should auto-fit checklist content, got {h}"
        );

        // Empty Pinned collapses entirely.
        let mut empty = app();
        empty.work_surface.placement = WorkSurfacePlacement::Top;
        empty.work_surface.panel = super::RailPanel::Tasks;
        empty.work_surface.top_height = 12;
        assert_eq!(
            super::height(&mut empty, 100, 40, AMPLE_BUDGET),
            0,
            "empty Pinned is not a panel"
        );

        // Empty Agents collapses too (no "No agents" chrome strip).
        let mut agents = app();
        agents.work_surface.placement = WorkSurfacePlacement::Top;
        agents.work_surface.panel = super::RailPanel::Agents;
        agents.work_surface.top_height = 12;
        assert_eq!(
            super::height(&mut agents, 100, 40, AMPLE_BUDGET),
            0,
            "empty Agents is not a panel"
        );
    }

    /// A chosen panel remains usable when only one content row fits.
    #[test]
    fn compact_explicit_view_keeps_content_ahead_of_goal_chrome() {
        let mut app = app();
        app.goal.objective = Some("ship the release".to_string());
        super::select_dock_panel(&mut app, super::RailPanel::Agents);
        let text = render_text(&mut app, 40, 3);
        assert!(text.contains("no agents have run this session"), "{text:?}");
        assert!(app.work_surface.focused);
        assert!(
            super::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_some()
        );
        assert!(!app.work_surface.explicit_view);
    }

    /// Top titles only when a live goal is set — never the panel name.
    #[test]
    fn top_title_is_goal_only_never_panel_chrome() {
        // With a goal: title is "Goal: …".
        let mut with_goal = app();
        with_goal.work_surface.placement = WorkSurfacePlacement::Top;
        with_goal.work_surface.panel = super::RailPanel::Tasks;
        with_goal.work_surface.top_height = 8;
        with_goal.goal.objective = Some("ship 0.9.4".to_string());
        let text = render_text(&mut with_goal, 80, 8);
        assert!(
            text.contains("Goal: ship 0.9.4"),
            "active goal must be the Top title: {text:?}"
        );
        assert!(
            !render_rows(&mut with_goal, 80, 8)
                .iter()
                .skip(1)
                .any(|row| row.contains("Pinned")),
            "panel name is not a Top title: {text:?}"
        );

        // Without a goal, only checklist: no Goal title, no Pinned chrome.
        let mut no_goal = app();
        no_goal.work_surface.placement = WorkSurfacePlacement::Top;
        no_goal.work_surface.panel = super::RailPanel::Tasks;
        no_goal.work_surface.top_height = 8;
        add_todos(&mut no_goal, 2);
        let text = render_text(&mut no_goal, 80, 6);
        assert!(
            !text.contains("Goal:"),
            "no live goal → no Goal title: {text:?}"
        );
        assert!(
            !render_rows(&mut no_goal, 80, 6)
                .iter()
                .skip(1)
                .any(|row| row.contains("Pinned")),
            "panel name is never a Top title: {text:?}"
        );
    }

    /// Tasks with only a goal (no todos/agents) still shows a strip.
    #[test]
    fn top_tasks_goal_alone_still_renders_a_strip() {
        let mut app = app();
        app.work_surface.placement = WorkSurfacePlacement::Top;
        app.work_surface.panel = super::RailPanel::Tasks;
        app.work_surface.top_height = 8;
        app.goal.objective = Some("only a goal".to_string());
        let budget = working_budget(&app, 40);
        let h = super::height(&mut app, 100, 40, budget);
        assert!(h >= 2, "goal alone must reserve title + divider, got {h}");
        let text = render_text(&mut app, 80, h);
        assert!(
            text.contains("Goal: only a goal"),
            "goal-alone strip must paint the title: {text:?}"
        );
    }

    /// Side rails share the empty-collapse rule: no content → no column.
    /// Width stays the configured ceiling when content exists.
    #[test]
    fn side_rail_collapses_when_empty_and_reserves_when_contentful() {
        let area = ratatui::layout::Rect::new(0, 0, 120, 32);

        // Empty Pinned: no side column.
        let mut empty = app();
        empty.work_surface.placement = WorkSurfacePlacement::Right;
        empty.work_surface.panel = super::RailPanel::Tasks;
        empty.work_surface.side_width = 30;
        assert_eq!(
            super::split_chat(&mut empty, area, 0),
            (area, None),
            "empty Pinned must not reserve a side column"
        );

        // Contentful Pinned: full-height column at configured width.
        let mut full = app();
        full.work_surface.placement = WorkSurfacePlacement::Right;
        full.work_surface.panel = super::RailPanel::Tasks;
        full.work_surface.side_width = 30;
        full.goal.objective = Some("ship it".to_string());
        let (chat, rail) = super::split_chat(&mut full, area, 0);
        let rail = rail.expect("contentful Pinned reserves a side rail");
        assert_eq!(rail.width, 30);
        assert_eq!(chat.width, area.width - 30);
        assert_eq!(rail.height, area.height);
    }

    #[test]
    fn minimum_top_surface_keeps_a_numbered_todo_selectable() {
        let mut app = app();
        add_todos(&mut app, 2);

        let text = render_text(&mut app, 40, 5);

        assert!(text.contains("1 ·"), "{text:?}");
        assert!(!app.work_surface.hitboxes.is_empty());
        assert_eq!(app.work_surface.hitboxes[0].row_y, 2);
    }

    #[test]
    fn compact_progress_window_reveals_current_without_reordering() {
        let mut app = app();
        {
            let mut todos = app.todos.try_lock().expect("todos");
            todos.add("finished".to_string(), TodoStatus::Completed);
            todos.add("current".to_string(), TodoStatus::InProgress);
            todos.add("next".to_string(), TodoStatus::Pending);
        }

        // The current item must win the compact window while retaining its
        // canonical ordinal.
        let text = render_text(&mut app, 80, 6);

        assert!(text.contains("To-do · 1/3 · 2 left"), "{text:?}");
        assert!(
            text.contains(&format!(
                "2 · {} current",
                codewhale_tui_kit::glyphs::SELECTION
            )),
            "{text:?}"
        );
        assert_eq!(app.work_surface.hitboxes[0].row_y, 2);
    }

    #[test]
    fn settled_file_tools_aggregate_once_and_keep_only_safe_targets() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.workspace = PathBuf::from("/workspace/project");
        for (id, name, input, status) in [
            (
                "read-1",
                "read_file",
                serde_json::json!({"path": "/workspace/project/src/lib.rs"}),
                ToolStatus::Success,
            ),
            (
                "search-1",
                "grep_files",
                serde_json::json!({"pattern": "WorkSurfaceState"}),
                ToolStatus::Success,
            ),
            (
                "write-1",
                "edit_file",
                serde_json::json!({"path": "src/lib.rs"}),
                ToolStatus::Success,
            ),
            (
                "read-external",
                "read_file",
                serde_json::json!({"path": "/Users/alice/private.txt"}),
                ToolStatus::Failed,
            ),
        ] {
            app.add_message(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
                name: name.to_string(),
                status,
                input_summary: None,
                output: Some("done".to_string()),
                prompts: None,
                spillover_path: None,
                output_summary: None,
                is_diff: false,
            })));
            let index = app.history.len() - 1;
            app.tool_details_by_cell.insert(
                index,
                ToolDetailRecord {
                    tool_id: id.to_string(),
                    tool_name: name.to_string(),
                    input,
                    output: Some("done".to_string()),
                },
            );
        }

        let rows = super::model::project(&mut app);
        let activity = rows
            .iter()
            .find(|row| row.id.0 == "activity:aggregate")
            .expect("aggregated activity row");
        assert!(
            activity.label.contains("Read 1 file ·")
                && activity.label.contains("Searched 1 pattern ·")
                && activity.label.ends_with("Wrote 1 file"),
            "aggregated label: {}",
            activity.label
        );
        assert!(!activity.detail.contains("/Users/alice"));
        assert!(!activity.label.contains("WorkSurfaceState"));
    }

    #[test]
    fn agent_rows_show_role_assignment_and_open_the_agent_transcript() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(SubAgentResult {
            usage: None,
            name: "agent_worker".to_string(),
            agent_id: "agent_worker".to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: FleetRole::Builder,
            assignment: SubAgentAssignment {
                native_preset: None,
                objective: "Wire settled file activity".to_string(),
                role: Some("general".to_string()),
            },
            model: "test-model".to_string(),
            nickname: Some(
                codewhale_engine::tools::subagent::whale_name_for_id_in_locale(
                    "agent_worker",
                    "en",
                ),
            ),
            status: SubAgentStatus::Running,
            worker_status: Some(AgentWorkerStatus::RunningTool),
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 1,
            child_route: None,
            result: None,
            steps_taken: 2,
            checkpoint: None,
            needs_input: None,
            duration_ms: 50,
            started_at: None,
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        });
        app.agent_progress_meta.insert(
            "agent_worker".to_string(),
            crate::tui::app::AgentProgressMeta {
                current_activity: Some(AgentCurrentActivity::bounded(
                    AgentCurrentActivityStatus::RunningTool,
                    None,
                    Some("File.apply_patch".to_string()),
                    Some(2),
                )),
                current_tool: Some("apply_patch".to_string()),
                files_touched: 2,
                ..crate::tui::app::AgentProgressMeta::default()
            },
        );

        let rows = super::model::project(&mut app);
        let row = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_worker")
            .expect("agent work row");
        // The identity column leads with the agent's nickname and keeps the
        // fleet role as the fallback spelling. It is never the raw agent id
        // (#36), and carries no `(+N)` while the agent is childless.
        assert_eq!(
            row.label,
            codewhale_engine::tools::subagent::whale_name_for_id_in_locale("agent_worker", "en")
        );
        let facts = row.agent.as_ref().expect("agent row facts");
        assert_eq!(facts.role_label, "general");
        assert_eq!(facts.objective, "Wire settled file activity");
        assert_eq!(facts.elapsed_secs, Some(0));
        // No usage envelope has been seen, so there is no token figure at all.
        assert_eq!(facts.tokens, None);
        assert!(row.detail.contains("Wire settled file activity"));
        assert!(row.detail.contains("using File.apply_patch"));
        assert!(row.detail.contains("step 2"));
        assert!(row.detail.contains("2 files changed"));
        // One agent, one destination (v0.9.7): activation opens the agent's
        // transcript directly; Agent Details is the secondary action.
        assert_eq!(
            row.primary_action,
            Some(SidebarRowAction::OpenAgentTranscript {
                agent_id: "agent_worker".to_string(),
            })
        );
    }

    fn cached_worker(
        id: &str,
        role: &str,
        nickname: Option<&str>,
        parent_run_id: Option<&str>,
        status: SubAgentStatus,
    ) -> SubAgentResult {
        SubAgentResult {
            // `name` is the raw session id in production snapshots — the
            // strip must never render it (#36).
            usage: None,
            name: id.to_string(),
            agent_id: id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: FleetRole::Builder,
            assignment: SubAgentAssignment {
                native_preset: None,
                objective: format!("objective for {id}"),
                role: Some(role.to_string()),
            },
            model: "test-model".to_string(),
            nickname: nickname.map(str::to_string),
            status,
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: parent_run_id.map(str::to_string),
            spawn_depth: u32::from(parent_run_id.is_some()) + 1,
            child_route: None,
            result: None,
            steps_taken: 1,
            checkpoint: None,
            needs_input: None,
            duration_ms: 50,
            started_at: None,
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        }
    }

    #[test]
    fn agent_rows_identify_by_fleet_role_and_never_leak_raw_ids() {
        // #36: the strip identifies an agent by its fleet role; the raw agent
        // id hash is noise and must never render as the "name". Flat fan-outs
        // carry no nesting chrome.
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent_e0b2dcf1",
            "builder",
            None,
            None,
            SubAgentStatus::Running,
        ));
        app.subagent_cache.push(cached_worker(
            "agent_99aa77bb",
            "scout",
            None,
            None,
            SubAgentStatus::Running,
        ));

        let rows = super::model::project(&mut app);
        let first = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_e0b2dcf1")
            .expect("first agent row");
        let second = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_99aa77bb")
            .expect("second agent row");
        assert_eq!(first.label, "builder");
        assert_eq!(second.label, "scout");
        assert!(first.detail.starts_with("running"), "{}", first.detail);
        for row in rows.iter().filter(|row| row.id.0.starts_with("worker:")) {
            assert!(!row.label.contains("agent_e0b2dcf1"), "{}", row.label);
            assert!(!row.label.contains("agent_99aa77bb"), "{}", row.label);
            assert!(
                !row.label.contains('↳'),
                "flat fan-out must not show nesting chrome: {}",
                row.label
            );
        }
    }

    #[test]
    fn agent_rows_order_and_indent_nested_spawns_under_their_parent() {
        // #36: nesting is visible only when actually present — the child
        // renders directly under its parent with a `↳` indent, and the parent
        // advertises the child it spawned as `(+1)`.
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent_child",
            "scout",
            None,
            Some("agent_parent"),
            SubAgentStatus::Running,
        ));
        app.subagent_cache.push(cached_worker(
            "agent_parent",
            "builder",
            None,
            None,
            SubAgentStatus::Running,
        ));

        let rows = super::model::project(&mut app);
        let worker_labels = rows
            .iter()
            .filter(|row| row.id.0.starts_with("worker:"))
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();
        let parent_pos = worker_labels
            .iter()
            .position(|label| *label == "builder (+1)")
            .expect("parent row label with child count");
        let child_pos = worker_labels
            .iter()
            .position(|label| *label == "↳ scout")
            .expect("indented child row label");
        assert!(
            child_pos == parent_pos + 1,
            "child must render directly under its parent: {worker_labels:?}"
        );
    }

    #[test]
    fn agent_rows_completed_agents_render_quietly_without_spawn_metadata() {
        // #36: quiet completion — a finished agent keeps status + what it
        // did; in-flight metadata (tool, step counters) must not linger as a
        // receipt dump. #6565: what it changed is the receipt, and once its
        // result is known the row says what it produced.
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent_done",
            "builder",
            None,
            None,
            SubAgentStatus::Completed,
        ));
        app.agent_progress_meta.insert(
            "agent_done".to_string(),
            crate::tui::app::AgentProgressMeta {
                current_activity: Some(AgentCurrentActivity::bounded(
                    AgentCurrentActivityStatus::Done,
                    Some("apply_patch finished".to_string()),
                    Some("File.apply_patch".to_string()),
                    Some(7),
                )),
                current_tool: Some("apply_patch".to_string()),
                files_touched: 4,
                ..crate::tui::app::AgentProgressMeta::default()
            },
        );

        let rows = super::model::project(&mut app);
        let row = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_done")
            .expect("completed agent row");
        assert!(row.detail.contains("completed"), "{}", row.detail);
        assert!(
            row.detail.contains("objective for agent_done"),
            "{}",
            row.detail
        );
        assert!(!row.detail.contains("using "), "{}", row.detail);
        assert!(!row.detail.contains("step 7"), "{}", row.detail);
        assert!(row.detail.contains("4 files changed"), "{}", row.detail);

        app.subagent_cache[0].result =
            Some("## Summary\n\nPatched the parser. Tests pass.".to_string());
        let rows = super::model::project(&mut app);
        let row = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_done")
            .expect("completed agent row");
        assert_eq!(
            row.detail, "completed · Patched the parser. · 4 files changed",
            "the headline replaces the assignment"
        );
        assert_eq!(
            row.agent.as_ref().map(|facts| facts.objective.as_str()),
            Some("Patched the parser.")
        );
    }

    // ---- Fleet row layout -------------------------------------------------

    /// Painted lines, one per terminal row, trailing padding removed.
    fn render_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::render(frame, frame.area(), app))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn fleet_row(rows: &[String]) -> String {
        rows.iter()
            .find(|line| line.contains("Streaming"))
            .cloned()
            .unwrap_or_else(|| panic!("no fleet row in {rows:?}"))
    }

    fn fleet_worker(
        id: &str,
        role: &str,
        objective: &str,
        duration_ms: u64,
        status: SubAgentStatus,
    ) -> SubAgentResult {
        let mut agent = cached_worker(id, role, None, None, status);
        agent.assignment.objective = objective.to_string();
        agent.duration_ms = duration_ms;
        agent
    }

    /// Seed a live fleet of one, with a reported token spend.
    fn fleet_app(tokens: Option<u64>) -> App {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(fleet_worker(
            "agent_stream",
            "general-purpose",
            "Streaming dead-code removal",
            753_000,
            SubAgentStatus::Running,
        ));
        app.agent_progress_meta.insert(
            "agent_stream".to_string(),
            crate::tui::app::AgentProgressMeta {
                received_tokens: tokens,
                ..crate::tui::app::AgentProgressMeta::default()
            },
        );
        app
    }

    #[test]
    fn fleet_row_lays_out_type_objective_and_a_right_aligned_receipt() {
        let mut app = fleet_app(Some(111_900));
        let rows = render_rows(&mut app, 100, 4);

        assert_eq!(
            fleet_row(&rows),
            " ▸ general-purpose  running  Streaming dead-code removal                  \
12m 33s · ↓ 111.9k tokens"
        );
        // The group header the strip already had stays put.
        assert!(
            rows.iter().any(|line| line.contains("Subagents 1")),
            "{rows:?}"
        );
    }

    #[test]
    fn focused_worker_row_carries_the_left_marker_and_queued_follow_ups() {
        let mut app = fleet_app(Some(111_900));
        // No focus, nothing queued: the row is exactly as before.
        let plain = fleet_row(&render_rows(&mut app, 100, 4));
        assert!(!plain.starts_with("❯"), "{plain}");
        assert!(!plain.contains("queued"), "{plain}");

        crate::tui::agent_focus::focus_agent(&mut app, "agent_stream");
        app.agent_queued_follow_ups
            .insert("agent_stream".to_string(), 1);
        let focused = fleet_row(&render_rows(&mut app, 110, 4));
        assert!(
            focused.trim_start().starts_with("❯ ▸ general-purpose"),
            "left-edge marker names the addressed fork: {focused}"
        );
        assert!(focused.ends_with("· 1 queued"), "{focused}");

        // The counter is the runtime's truth: once the child takes the input
        // the next AgentList refresh clears it and the suffix disappears.
        app.agent_queued_follow_ups.clear();
        let drained = fleet_row(&render_rows(&mut app, 110, 4));
        assert!(!drained.contains("queued"), "{drained}");
        // Leaving focus removes the gutter again.
        crate::tui::agent_focus::exit_focus(&mut app);
        let back = fleet_row(&render_rows(&mut app, 100, 4));
        assert_eq!(back, plain);
    }

    #[test]
    fn fleet_row_repaints_resolved_model_and_each_distinct_usage_total() {
        let mut app = fleet_app(None);
        crate::tui::ui::record_agent_spawned_route(&mut app, "agent_stream", "deepseek-v4-pro");
        let launched = fleet_row(&render_rows(&mut app, 120, 4));
        assert!(launched.contains("deepseek-v4-pro"), "{launched}");
        assert!(!launched.contains("tokens"), "{launched}");

        let route = codewhale_engine::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Deepseek,
            ProviderKind::Deepseek.as_str(),
            "deepseek-v4-pro",
            Some(ProviderKind::Deepseek.provider().default_base_url()),
            chrono::Utc::now(),
        );
        let usage = |source_id: &str, input_tokens, output_tokens| MailboxMessage::TokenUsage {
            agent_id: "agent_stream".to_string(),
            source_id: source_id.to_string(),
            route: Box::new(route.clone()),
            usage: codewhale_models::Usage {
                input_tokens,
                output_tokens,
                ..Default::default()
            },
        };

        crate::tui::subagent_routing::handle_subagent_mailbox(
            &mut app,
            99,
            &usage("response-1", 10_000, 1_000),
        );
        let first = fleet_row(&render_rows(&mut app, 120, 4));
        assert!(first.contains("deepseek-v4-pro"), "{first}");
        assert!(first.contains("11.0k tokens"), "{first}");

        // Replaying the same mailbox envelope must not inflate the receipt.
        crate::tui::subagent_routing::handle_subagent_mailbox(
            &mut app,
            1,
            &usage("response-1", 10_000, 1_000),
        );
        let replay = fleet_row(&render_rows(&mut app, 120, 4));
        assert!(replay.contains("11.0k tokens"), "{replay}");

        crate::tui::subagent_routing::handle_subagent_mailbox(
            &mut app,
            2,
            &usage("response-2", 20_000, 2_000),
        );
        let second = fleet_row(&render_rows(&mut app, 120, 4));
        assert!(second.contains("deepseek-v4-pro"), "{second}");
        assert!(second.contains("33.0k tokens"), "{second}");
    }

    #[test]
    fn fleet_row_shows_remaining_todos_only_when_the_ledger_has_unsettled_work() {
        let mut app = fleet_app(Some(1_200));
        app.agent_progress_meta
            .get_mut("agent_stream")
            .expect("meta")
            .todos_remaining = Some(3);

        let with_left = fleet_row(&render_rows(&mut app, 100, 4));
        assert!(
            with_left.contains("3 left"),
            "unsettled ledger must surface on the receipt: {with_left}"
        );
        assert!(
            with_left.contains("↓") && with_left.contains("tokens"),
            "tokens stay alongside the remaining chip: {with_left}"
        );

        // Fully settled list → quiet (no fabricated zero chip).
        app.agent_progress_meta
            .get_mut("agent_stream")
            .expect("meta")
            .todos_remaining = Some(0);
        let settled = fleet_row(&render_rows(&mut app, 100, 4));
        assert!(
            !settled.contains("left"),
            "zero remaining must not paint a chip: {settled}"
        );

        // No ledger published → quiet.
        app.agent_progress_meta
            .get_mut("agent_stream")
            .expect("meta")
            .todos_remaining = None;
        let absent = fleet_row(&render_rows(&mut app, 100, 4));
        assert!(
            !absent.contains("left"),
            "missing ledger must not invent a chip: {absent}"
        );
    }

    #[test]
    fn fleet_identity_prefers_the_nickname_and_falls_back_to_the_role() {
        // Nicknames are CodeWhale identity, so they lead. An agent that has
        // none falls back to its fleet role rather than showing a blank or a
        // fabricated name.
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        let mut named = fleet_worker(
            "agent_named",
            "general-purpose",
            "Streaming dead-code removal",
            753_000,
            SubAgentStatus::Running,
        );
        named.nickname = Some("Fluke".to_string());
        app.subagent_cache.push(named);
        app.subagent_cache.push(fleet_worker(
            "agent_plain",
            "general-purpose",
            "Ambient visual calm-down",
            741_000,
            SubAgentStatus::Running,
        ));

        let rows = super::model::project(&mut app);
        let row = |id: &str| {
            rows.iter()
                .find(|row| row.id.0 == format!("worker:{id}"))
                .unwrap_or_else(|| panic!("row for {id}"))
        };
        assert_eq!(row("agent_named").label, "Fluke");
        assert_eq!(
            row("agent_named").agent.as_ref().expect("facts").role_label,
            "general-purpose"
        );
        // No nickname: the identity and its fallback are the same string.
        assert_eq!(row("agent_plain").label, "general-purpose");

        // Both spellings share one column, so the objectives stay aligned.
        let painted = render_rows(&mut app, 100, 5);
        let named_line = painted
            .iter()
            .find(|line| line.contains("Fluke"))
            .expect("nicknamed row");
        let plain_line = painted
            .iter()
            .find(|line| line.contains("general-purpose"))
            .expect("un-nicknamed row");
        assert_eq!(
            named_line.find("Streaming"),
            plain_line.find("Ambient"),
            "objectives must share a column:\n{named_line}\n{plain_line}"
        );
    }

    #[test]
    fn an_identity_too_wide_for_the_column_falls_back_without_widening_it() {
        // The identity column is shared, so one outlier must not starve every
        // other objective — and a name is shown whole or not at all.
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        let mut long = fleet_worker(
            "agent_long",
            "general-purpose",
            "Streaming dead-code removal",
            753_000,
            SubAgentStatus::Running,
        );
        long.nickname = Some("Bartholomew the Extremely Long-Winded Humpback".to_string());
        app.subagent_cache.push(long);
        app.subagent_cache.push(fleet_worker(
            "agent_plain",
            "scout",
            "Ambient visual calm-down",
            741_000,
            SubAgentStatus::Running,
        ));

        let painted = render_rows(&mut app, 100, 5);
        let joined = painted.join("\n");
        // The oversized nickname never renders, whole or truncated.
        assert!(!joined.contains("Bartholomew"), "{joined}");
        assert!(!joined.contains("Bartholom"), "{joined}");
        // It falls back to its role, and the other row is untouched.
        assert!(joined.contains("general-purpose"), "{joined}");
        assert!(joined.contains("scout"), "{joined}");
        // Neither objective was starved by the outlier.
        assert!(joined.contains("Streaming dead-code removal"), "{joined}");
        assert!(joined.contains("Ambient visual calm-down"), "{joined}");
    }

    #[test]
    fn fleet_row_drops_tokens_then_elapsed_then_type_as_the_surface_narrows() {
        // Settled degradation order: tokens first, then elapsed, then the
        // type and status columns together. The objective is the last thing
        // to go and every column truncates rather than wrapping. The status
        // word outlives the whole receipt — a fleet row that cannot say its
        // state in words has lost the fact the strip exists to show.
        let mut app = fleet_app(Some(111_900));
        let medium = fleet_row(&render_rows(&mut app, 72, 4));
        assert!(medium.contains("12m 33s"), "{medium}");
        assert!(!medium.contains("tokens"), "{medium}");
        assert!(medium.contains("general-purpose"), "{medium}");
        assert!(medium.contains("running"), "{medium}");

        let narrow = fleet_row(&render_rows(&mut app, 56, 4));
        assert!(!narrow.contains("tokens"), "{narrow}");
        assert!(!narrow.contains("12m 33s"), "{narrow}");
        assert!(narrow.contains("general-purpose"), "{narrow}");
        assert!(narrow.contains("running"), "{narrow}");

        let tight = fleet_row(&render_rows(&mut app, 28, 4));
        assert!(!tight.contains("general-purpose"), "{tight}");
        assert!(!tight.contains("running"), "{tight}");
        assert!(tight.contains("Streaming"), "{tight}");

        for line in [&medium, &narrow, &tight] {
            assert!(line.chars().all(|ch| ch != '\n'), "{line}");
        }
    }

    #[test]
    fn fleet_row_elapsed_freezes_once_the_agent_is_finished() {
        // The manager recomputes `duration_ms` as `started_at.elapsed()` on
        // every snapshot, so a finished agent's raw duration keeps growing.
        // The row must latch the first terminal reading instead.
        let mut app = fleet_app(None);
        app.subagent_cache[0].status = SubAgentStatus::Completed;
        app.subagent_cache[0].duration_ms = 753_000;

        let first = super::model::project(&mut app);
        let finished = first
            .iter()
            .find(|row| row.id.0 == "worker:agent_stream")
            .and_then(|row| row.agent.as_ref())
            .expect("finished agent facts");
        assert_eq!(finished.elapsed_secs, Some(753));

        // A later snapshot reports a larger duration for the same dead agent.
        app.subagent_cache[0].duration_ms = 999_000;
        let second = super::model::project(&mut app);
        let still = second
            .iter()
            .find(|row| row.id.0 == "worker:agent_stream")
            .and_then(|row| row.agent.as_ref())
            .expect("finished agent facts");
        assert_eq!(
            still.elapsed_secs,
            Some(753),
            "finished elapsed must freeze"
        );
    }

    #[test]
    fn fleet_row_elapsed_still_advances_while_the_agent_runs() {
        let mut app = fleet_app(None);
        app.subagent_cache[0].duration_ms = 10_000;
        let early = super::model::project(&mut app);
        assert_eq!(
            early
                .iter()
                .find(|row| row.id.0 == "worker:agent_stream")
                .and_then(|row| row.agent.as_ref())
                .expect("running agent facts")
                .elapsed_secs,
            Some(10)
        );

        app.subagent_cache[0].duration_ms = 40_000;
        let later = super::model::project(&mut app);
        assert_eq!(
            later
                .iter()
                .find(|row| row.id.0 == "worker:agent_stream")
                .and_then(|row| row.agent.as_ref())
                .expect("running agent facts")
                .elapsed_secs,
            Some(40)
        );
    }

    #[test]
    fn fleet_row_with_no_reported_usage_shows_no_token_figure_at_all() {
        // An unknown number is rendered as nothing. Never `0`, which would
        // claim the agent spent nothing.
        let mut app = fleet_app(None);
        let row = fleet_row(&render_rows(&mut app, 100, 4));
        assert!(!row.contains("tokens"), "{row}");
        assert!(!row.contains('↓'), "{row}");
        assert!(row.contains("12m 33s"), "{row}");

        let mut spent = fleet_app(Some(0));
        let zero = fleet_row(&render_rows(&mut spent, 100, 4));
        // A *reported* zero is a fact and does render.
        assert!(zero.contains("↓ 0 tokens"), "{zero}");
    }

    #[test]
    fn fleet_row_child_badge_counts_children_that_are_on_the_surface() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent_lead",
            "general-purpose",
            None,
            None,
            SubAgentStatus::Running,
        ));
        for child in ["agent_c1", "agent_c2", "agent_c3"] {
            app.subagent_cache.push(cached_worker(
                child,
                "scout",
                None,
                Some("agent_lead"),
                SubAgentStatus::Running,
            ));
        }
        // A child whose parent is not on the surface must not be counted for
        // anyone, and must not inflate the lead's badge.
        app.subagent_cache.push(cached_worker(
            "agent_orphan",
            "scout",
            None,
            Some("agent_missing"),
            SubAgentStatus::Running,
        ));

        let rows = super::model::project(&mut app);
        let label = |id: &str| {
            rows.iter()
                .find(|row| row.id.0 == format!("worker:{id}"))
                .map(|row| row.label.clone())
                .unwrap_or_else(|| panic!("row for {id}"))
        };
        assert_eq!(label("agent_lead"), "general-purpose (+3)");
        assert_eq!(label("agent_c1"), "↳ scout");
        assert_eq!(label("agent_orphan"), "scout");
    }

    #[test]
    fn a_capped_fleet_list_announces_how_many_rows_it_is_hiding() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        for index in 0..8 {
            app.subagent_cache.push(cached_worker(
                &format!("agent_{index}"),
                "general-purpose",
                None,
                None,
                SubAgentStatus::Running,
            ));
        }

        // Four content rows for nine projected rows (header + eight workers).
        let rows = render_rows(&mut app, 100, 5);
        let more = rows
            .iter()
            .find(|line| line.contains("more"))
            .unwrap_or_else(|| panic!("no overflow line in {rows:?}"));
        // Nine projected rows (header + eight workers); two fit, seven do not.
        assert!(more.contains("↓ 7 more"), "{more}");
        // Right-aligned against the content column, not the left margin.
        assert!(more.starts_with("        "), "{more}");
    }

    #[test]
    fn fleet_rows_render_in_top_left_and_right_placements() {
        for placement in [
            super::WorkSurfacePlacement::Top,
            super::WorkSurfacePlacement::Left,
            super::WorkSurfacePlacement::Right,
        ] {
            let mut app = fleet_app(Some(111_900));
            app.work_surface.placement = placement;
            app.work_surface.effective_placement = placement;
            let rows = render_rows(&mut app, 40, 8);
            let row = fleet_row(&rows);
            assert!(
                row.contains("Streaming"),
                "{placement:?} lost the objective: {rows:?}"
            );
        }
    }

    #[test]
    fn progress_only_work_rows_use_typed_activity_not_display_substrings() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.agent_progress.insert(
            "agent_progress_only".to_string(),
            "queued waiting failed completed".to_string(),
        );

        let rows = super::model::project(&mut app);
        let row = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_progress_only")
            .expect("progress-only work row");
        assert_eq!(row.detail, "running");

        app.agent_progress_meta.insert(
            "agent_progress_only".to_string(),
            crate::tui::app::AgentProgressMeta {
                current_activity: Some(AgentCurrentActivity::bounded(
                    AgentCurrentActivityStatus::Waiting,
                    Some("approval required".to_string()),
                    None,
                    Some(5),
                )),
                ..crate::tui::app::AgentProgressMeta::default()
            },
        );

        let rows = super::model::project(&mut app);
        let row = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_progress_only")
            .expect("typed progress-only work row");
        assert!(row.detail.contains("waiting for input"), "{}", row.detail);
        assert!(row.detail.contains("approval required"), "{}", row.detail);
        assert!(row.detail.contains("step 5"), "{}", row.detail);
    }

    /// #6565: when the activity detail already names the step and the tool,
    /// the dock row says them once, not again as `using …` and `step N`.
    #[test]
    fn dock_agent_row_states_each_step_fact_once() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.agent_progress
            .insert("agent_reader".to_string(), String::new());
        app.agent_progress_meta.insert(
            "agent_reader".to_string(),
            crate::tui::app::AgentProgressMeta {
                current_activity: Some(AgentCurrentActivity::bounded(
                    AgentCurrentActivityStatus::RunningTool,
                    Some("step 6: finished tool 'read_file'".to_string()),
                    Some("read_file".to_string()),
                    Some(6),
                )),
                ..crate::tui::app::AgentProgressMeta::default()
            },
        );

        let rows = super::model::project(&mut app);
        let row = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_reader")
            .expect("progress-only work row");
        assert_eq!(row.detail.matches("step 6").count(), 1, "{}", row.detail);
        assert_eq!(row.detail.matches("read_file").count(), 1, "{}", row.detail);
        assert!(!row.detail.contains("using read_file"), "{}", row.detail);
    }

    // === #5906: a parked husk is not an agent waiting for input ==========

    /// Build a child exactly the way the turn-end parking projection does:
    /// `Interrupted` + `WaitingForUser` + a `needs_input` note phrased as a
    /// question, distinguished from a real question only by the checkpoint's
    /// `parked_at_turn_end` flag.
    fn parked_worker(id: &str, objective: &str) -> SubAgentResult {
        let mut agent = fleet_worker(
            id,
            "general-purpose",
            objective,
            753_000,
            SubAgentStatus::Interrupted(
                "Parent turn ended before this turn-owned child settled.".to_string(),
            ),
        );
        agent.worker_status = Some(AgentWorkerStatus::WaitingForUser);
        agent.needs_input = Some(codewhale_engine::tools::subagent::SubAgentNeedsInput {
            question: format!(
                "Resume this parked child with agent(action=\"start\", resume_from=\"{id}\")."
            ),
        });
        agent.checkpoint = Some(codewhale_engine::tools::subagent::SubAgentCheckpoint {
            checkpoint_id: format!("{id}:step:2"),
            agent_id: id.to_string(),
            continuation_handle: format!("agent:{id}:checkpoint"),
            reason: "Parent turn ended before this turn-owned child settled.".to_string(),
            continuable: true,
            steps_taken: 2,
            message_count: 4,
            created_at_ms: 1_000,
            messages: Vec::new(),
            omitted_messages: 0,
            parked_at_turn_end: true,
        });
        agent
    }

    fn asking_worker(id: &str, objective: &str) -> SubAgentResult {
        let mut agent = fleet_worker(
            id,
            "general-purpose",
            objective,
            120_000,
            SubAgentStatus::Running,
        );
        agent.worker_status = Some(AgentWorkerStatus::WaitingForUser);
        agent.needs_input = Some(codewhale_engine::tools::subagent::SubAgentNeedsInput {
            question: "Which path should I use?".to_string(),
        });
        agent
    }

    fn parked_fixture() -> App {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache
            .push(parked_worker("agent_parked", "Parked dead-code removal"));
        app.subagent_cache
            .push(asking_worker("agent_asking", "Asking about the path"));
        app.subagent_cache.push(fleet_worker(
            "agent_live",
            "general-purpose",
            "Streaming dead-code removal",
            30_000,
            SubAgentStatus::Running,
        ));
        crate::tui::subagent_routing::reconcile_subagent_activity_state(&mut app);
        app
    }

    #[test]
    fn a_parked_work_row_says_parked_and_names_its_recovery() {
        let mut app = parked_fixture();
        let rows = super::model::project(&mut app);

        let parked = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_parked")
            .expect("parked work row");
        assert!(parked.detail.starts_with("parked"), "{}", parked.detail);
        assert!(
            !parked.detail.contains("waiting for input"),
            "a parked husk must not wear the answerable label: {}",
            parked.detail
        );
        // The recovery names verbs the runtime actually exposes.
        assert!(parked.detail.contains("resume_from"), "{}", parked.detail);
        assert!(parked.detail.contains("cancel"), "{}", parked.detail);
        assert!(
            !parked.detail.contains("Resume this parked child"),
            "the parking note is not a question to replay at the operator: {}",
            parked.detail
        );

        let asking = rows
            .iter()
            .find(|row| row.id.0 == "worker:agent_asking")
            .expect("asking work row");
        assert!(
            asking.detail.contains("waiting for input"),
            "a child that really asked keeps the answerable label: {}",
            asking.detail
        );
        assert!(
            asking.detail.contains("Which path should I use?"),
            "{}",
            asking.detail
        );
    }

    #[test]
    fn parked_rows_sort_below_live_work_and_leave_the_needs_input_count_alone() {
        let mut app = parked_fixture();
        let rows = super::model::project(&mut app);

        let heading = rows
            .iter()
            .find(|row| row.id.0 == "section:work")
            .expect("work heading");
        // The attention chip counts children a person is actually blocking:
        // one, the child that asked. Two would mean the parked husk had been
        // counted as waiting for input all over again.
        assert!(
            heading.label.contains("1 blocked"),
            "only the child that actually asked is blocked on a person: {}",
            heading.label
        );

        let position = |id: &str| {
            rows.iter()
                .position(|row| row.id.0 == id)
                .unwrap_or_else(|| panic!("{id} missing from {rows:?}"))
        };
        assert!(
            position("worker:agent_parked") > position("worker:agent_live"),
            "a parked husk must not sort above live work"
        );
        assert!(
            position("worker:agent_parked") > position("worker:agent_asking"),
            "a parked husk must not sort above a child a person can answer"
        );
    }

    #[test]
    fn the_parked_status_word_survives_the_narrow_row_ladder() {
        // The status word outlives the whole receipt as the strip narrows
        // (see the degradation test above); `parked` is the fact the row
        // exists to carry, so it must survive the same ladder `running` does.
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache
            .push(parked_worker("agent_parked", "Parked dead-code removal"));
        crate::tui::subagent_routing::reconcile_subagent_activity_state(&mut app);

        for width in [96u16, 72, 56] {
            let rows = render_rows(&mut app, width, 6);
            let row = rows
                .iter()
                .find(|line| line.contains("Parked dead-code"))
                .unwrap_or_else(|| panic!("no parked row at width {width} in {rows:?}"));
            assert!(row.contains("parked"), "width {width}: {row}");
            assert!(!row.contains("waiting for input"), "width {width}: {row}");
        }
    }

    #[test]
    fn agent_transcript_keyboard_mouse_and_return_selection_converge() {
        fn add_worker(app: &mut App) {
            app.current_session_id = Some(SESSION.to_string());
            app.subagent_cache.push(SubAgentResult {
                usage: None,
                name: "agent_converge".to_string(),
                agent_id: "agent_converge".to_string(),
                context_mode: "fresh".to_string(),
                fork_context: false,
                workspace: None,
                git_branch: Some("codex/details".to_string()),
                agent_type: FleetRole::Builder,
                assignment: SubAgentAssignment {
                    native_preset: None,
                    objective: "Verify keyboard and mouse convergence".to_string(),
                    role: Some("worker".to_string()),
                },
                model: "test-model".to_string(),
                nickname: Some(
                    codewhale_engine::tools::subagent::whale_name_for_id_in_locale(
                        "agent_converge",
                        "en",
                    ),
                ),
                status: SubAgentStatus::Running,
                worker_status: Some(AgentWorkerStatus::Running),
                runtime_permissions: None,
                parent_run_id: None,
                spawn_depth: 1,
                child_route: None,
                result: None,
                steps_taken: 1,
                checkpoint: None,
                needs_input: None,
                duration_ms: 100,
                started_at: None,
                from_prior_session: false,
                idle_ms: None,
                heartbeat_timeout_ms: None,
            });
        }

        let mut keyboard = app();
        add_worker(&mut keyboard);
        let _ = render_text(&mut keyboard, 100, 6);
        let _ = super::handle_key(
            &mut keyboard,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
        );
        let keyboard_action = super::handle_key(
            &mut keyboard,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .expect("Work key handled")
        .expect("agent transcript action");
        let keyboard_selection = keyboard.work_surface.selected.clone();

        let mut mouse = app();
        add_worker(&mut mouse);
        let _ = render_text(&mut mouse, 100, 6);
        let row_y = mouse
            .work_surface
            .hitboxes
            .iter()
            .find(|hit| hit.id.0 == "worker:agent_converge")
            .expect("agent hitbox")
            .row_y;
        let mouse_action = super::handle_mouse(
            &mut mouse,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: row_y,
                modifiers: KeyModifiers::NONE,
            },
        )
        .action
        .expect("mouse agent transcript action");
        assert_eq!(mouse_action, keyboard_action);
        assert_eq!(mouse.work_surface.selected, keyboard_selection);

        crate::tui::mouse_ui::apply_sidebar_row_action(&mut mouse, mouse_action);
        // One agent, one destination: activation focuses the worker in place
        // (its full transcript owns the conversation area) instead of opening
        // a modal, and leaving focus keeps the rail selection where it was.
        assert!(
            mouse
                .agent_focus
                .as_ref()
                .is_some_and(|focus| focus.is("agent_converge")),
            "activation must focus the worker"
        );
        let selected_before_close = mouse.work_surface.selected.clone();
        assert!(crate::tui::agent_focus::exit_focus(&mut mouse));
        assert_eq!(mouse.work_surface.selected, selected_before_close);
        assert!(mouse.work_surface.opened.is_none());
    }

    #[test]
    fn active_session_without_work_keeps_surface_invisible() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());

        let rows = super::model::project(&mut app);

        assert!(rows.is_empty());
        assert_eq!(super::height(&mut app, 120, 32, AMPLE_BUDGET), 0);
    }

    #[test]
    fn empty_work_stays_hidden_after_cached_session_state_is_cleared() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.work_surface.cached_graph = Some(operation_graph(NodeState::Active));

        let rows = super::model::project(&mut app);

        assert!(rows.is_empty());
        assert!(app.work_surface.cached_graph.is_none());
    }

    #[test]
    fn empty_work_reserves_no_side_rail() {
        for placement in [
            super::WorkSurfacePlacement::Left,
            super::WorkSurfacePlacement::Right,
        ] {
            let mut app = app();
            app.current_session_id = Some(SESSION.to_string());
            app.work_surface.placement = placement;
            let area = ratatui::layout::Rect::new(0, 0, 120, 32);

            assert_eq!(
                super::height(&mut app, area.width, area.height, AMPLE_BUDGET),
                0
            );
            assert_eq!(super::split_chat(&mut app, area, 0), (area, None));
        }
    }

    fn terminal_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render-level smoke coverage for the ported rail panels — reinstates
    /// the sidebar render smoke tests removed with the classic shell
    /// (739616787). Top never spends a row on panel chrome (content is
    /// self-evident). Side rails are named by their content's own heading
    /// row (`▾ Subagents N`, `Goal: …`); Context is the one line-list panel
    /// and keeps its muted panel title.
    #[test]
    fn rail_panels_render_in_all_placements() {
        for panel in [
            super::RailPanel::Agents,
            super::RailPanel::Context,
            super::RailPanel::Tasks,
        ] {
            for placement in [
                super::WorkSurfacePlacement::Bottom,
                super::WorkSurfacePlacement::Top,
                super::WorkSurfacePlacement::Left,
                super::WorkSurfacePlacement::Right,
            ] {
                let mut app = app();
                app.work_surface.placement = placement;
                super::interaction::select_dock_panel(&mut app, panel);
                app.work_surface.focused = false;
                // Content so empty-collapse does not hide the panel. Agents
                // needs a cached worker; Tasks needs a goal; Context always
                // has a budget.
                app.goal.objective = Some("ship the release".to_string());
                if panel == super::RailPanel::Agents {
                    app.subagent_cache.push(cached_worker(
                        "agent-a",
                        "explore",
                        Some("scout"),
                        None,
                        SubAgentStatus::Running,
                    ));
                }
                let area = ratatui::layout::Rect::new(0, 0, 100, 24);

                // Render coverage, not yield coverage: a 24-row terminal with
                // work on screen has rows to spare, so the panel is expected
                // to draw. The idle-empty budget is exercised end-to-end in
                // `ui::tests::rail_strip_yields_the_ambient_floor_*`.
                let budget = working_budget(&app, area.height);
                let strip = super::height(&mut app, area.width, area.height, budget);
                let (_chat, rail) = super::split_chat(&mut app, area, 0);
                let backend = TestBackend::new(area.width, area.height);
                let mut terminal = Terminal::new(backend).expect("terminal");
                terminal
                    .draw(|frame| {
                        if strip > 0 {
                            super::render(
                                frame,
                                ratatui::layout::Rect::new(0, 0, area.width, strip),
                                &mut app,
                            );
                        } else if let Some(rail) = rail {
                            super::render(frame, rail, &mut app);
                        }
                    })
                    .expect("draw");
                let text = terminal_text(&terminal);
                match placement {
                    super::WorkSurfacePlacement::Bottom => {
                        assert!(
                            strip > 0,
                            "{panel:?} on Bottom should auto-fit a content strip; got height 0"
                        );
                    }
                    super::WorkSurfacePlacement::Top => {
                        assert!(
                            strip > 0,
                            "{panel:?} on Top should auto-fit a content strip; got height 0"
                        );
                        // Panel chrome ("Pinned"/"Agents") never on Top.
                        // An active goal *is* a title — and this fixture sets one.
                        // A chrome title would be a row saying only the
                        // view's name; the tab row and `▾ Subagents N` both
                        // legitimately contain the lowercase word.
                        let strip_body = text.lines().skip(1).collect::<Vec<_>>().join("\n");
                        assert!(
                            !strip_body.lines().any(|line| line.trim() == panel.title()),
                            "{panel:?} on Top must not spend a row on panel chrome; got: {text}"
                        );
                        // Goal title when a live goal is set.
                        assert!(
                            text.contains("Goal:") && text.contains("ship the release"),
                            "Top with an active goal must title with Goal: …; got: {text}"
                        );
                    }
                    super::WorkSurfacePlacement::Left | super::WorkSurfacePlacement::Right => {
                        assert!(
                            rail.is_some() || strip > 0,
                            "{panel:?} in {placement:?} should reserve a rail"
                        );
                        // Work-row panels are named by their content heading;
                        // only the Context fact list keeps a panel title.
                        match panel {
                            super::RailPanel::Agents => {
                                assert!(
                                    text.contains("Subagents 1"),
                                    "{panel:?} in {placement:?} should render its \
                                     Subagents heading; got: {text}"
                                );
                                assert!(
                                    !app.work_surface.hitboxes.is_empty(),
                                    "{panel:?} in {placement:?} must record hitboxes — \
                                     every work row is a door"
                                );
                            }
                            super::RailPanel::Tasks => {
                                assert!(
                                    text.contains("Goal: ship the release"),
                                    "{panel:?} in {placement:?} should render the goal \
                                     heading; got: {text}"
                                );
                            }
                            _ => {
                                assert!(
                                    text.contains("compact now"),
                                    "{panel:?} in {placement:?} should render the budget \
                                     rows; got: {text}"
                                );
                            }
                        }
                    }
                    super::WorkSurfacePlacement::Off => {}
                }
            }
        }
    }

    #[test]
    fn off_placement_reserves_no_rail_in_any_panel() {
        for panel in [
            super::RailPanel::Tasks,
            super::RailPanel::Agents,
            super::RailPanel::Context,
            super::RailPanel::Tasks,
        ] {
            let mut app = app();
            add_todos(&mut app, 2);
            app.work_surface.placement = super::WorkSurfacePlacement::Off;
            app.work_surface.panel = panel;
            let area = ratatui::layout::Rect::new(0, 0, 120, 32);

            assert_eq!(
                super::height(&mut app, area.width, area.height, AMPLE_BUDGET),
                0
            );
            assert_eq!(super::split_chat(&mut app, area, 0), (area, None));
            assert_eq!(app.work_surface.last_area, None);
        }
    }

    #[test]
    fn context_view_renders_the_budget_in_a_side_rail() {
        let mut app = app();
        let (_, max, _) = crate::tui::ui::context_usage_snapshot(&app).expect("context budget");
        app.auto_compact = true;
        app.auto_compact_threshold_percent = 80.0;
        app.compact_threshold = usize::try_from(max / 2).expect("context window");
        app.work_surface.placement = super::WorkSurfacePlacement::Right;
        super::interaction::select_dock_panel(&mut app, super::RailPanel::Context);
        let area = ratatui::layout::Rect::new(0, 0, 100, 24);

        let budget = working_budget(&app, area.height);
        let strip = super::height(&mut app, area.width, area.height, budget);
        assert_eq!(strip, 0, "side placements take no top strip");
        let (_chat, rail) = super::split_chat(&mut app, area, 0);
        let rail = rail.expect("context panel reserves a side rail");

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::render(frame, rail, &mut app))
            .expect("draw");
        let text = terminal_text(&terminal);
        assert!(text.contains(" of "), "budget row; got: {text}");
        assert!(
            text.contains("auto ~50%"),
            "effective threshold; got: {text}"
        );
        assert!(text.contains("compact now"), "compact door; got: {text}");
        assert!(
            app.work_surface
                .hitboxes
                .iter()
                .any(|hit| hit.id.0 == "context:compact"),
            "the compact row is a hit target"
        );

        let rows = super::views::context_rows(&mut app);
        let automatic = rows
            .iter()
            .find(|row| row.id.0 == "context:automatic")
            .unwrap();
        assert_eq!(automatic.label, "auto ~50% · target 80%");
        assert!(automatic.detail.contains("other routes may differ"));
        app.compact_threshold = usize::try_from(max * 4 / 5).expect("context window");
        let rows = super::views::context_rows(&mut app);
        let automatic = rows
            .iter()
            .find(|row| row.id.0 == "context:automatic")
            .unwrap();
        assert_eq!(automatic.label, "auto ~80% · target 80%");
        app.auto_compact = false;
        let rows = super::views::context_rows(&mut app);
        let automatic = rows
            .iter()
            .find(|row| row.id.0 == "context:automatic")
            .unwrap();
        assert_eq!(automatic.label, "automatic compaction off");
        assert!(automatic.detail.contains("/compact"));
    }

    #[test]
    fn missing_runtime_renders_disconnected_state() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.runtime_services.work = None;

        let rows = super::model::project(&mut app);

        assert_eq!(rows[0].label, "Work · disconnected");
    }

    #[test]
    fn busy_graph_authority_renders_truthful_error_without_leaking_it_into_header() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        let todos = app.todos.clone();
        let _guard = todos.try_lock().expect("hold To-do authority lock");

        let rows = super::model::project(&mut app);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Work · error");
        assert!(rows[0].detail.contains("To-do state is busy"));
        assert!(!rows[0].label.contains("busy"));
    }

    #[test]
    fn graph_error_without_an_active_session_stays_suppressed() {
        let mut app = app();
        let todos = app.todos.clone();
        let _guard = todos.try_lock().expect("hold To-do authority lock");

        let rows = super::model::project(&mut app);

        assert!(rows.is_empty());
    }

    #[test]
    fn waiting_operation_is_not_counted_as_running() {
        let mut app = app();
        let graph = operation_graph(NodeState::Waiting);
        restore_graph(&mut app, &graph);
        app.runtime_services
            .work
            .as_ref()
            .expect("Work Graph runtime")
            .reconcile_operation(
                SESSION,
                OperationOwnerSnapshot::new("shell:shell_1234abcd", OwnerState::Waiting, 1, 6),
            )
            .expect("waiting shell owner");

        let rows = super::model::project(&mut app);

        assert!(
            rows[0].label.starts_with("Work · Needs input:")
                || rows[0]
                    .label
                    .starts_with("Work · 0 active · 1 needs input · 0 ready · 0 recent"),
            "{}",
            rows[0].label
        );
        assert!(
            rows[0].label.contains("blocked") || rows[0].label.contains("needs input"),
            "{}",
            rows[0].label
        );
    }

    #[test]
    fn stale_operation_is_blocked_attention_with_bounded_output_section() {
        let mut app = app();
        let graph = operation_graph(NodeState::Stale);
        restore_graph(&mut app, &graph);

        let rows = super::model::project(&mut app);
        assert!(
            rows[0].label.contains("Needs input") || rows[0].label.contains("1 needs input"),
            "{}",
            rows[0].label
        );
        let row = rows.iter().find(|row| row.selectable).expect("stale row");
        assert_eq!(row.mark, "?");
        assert!(row.detail.starts_with("stale · operation"));
        let Some(SidebarRowAction::InspectWork {
            body, stop_action, ..
        }) = row.primary_action.as_ref()
        else {
            panic!("stale row must open inspector");
        };
        assert!(
            stop_action.is_none(),
            "a stale owner cannot truthfully expose a stop action"
        );
        assert!(
            body.contains("Last bounded output\nNo output receipt"),
            "{body}"
        );
        assert!(body.contains("Owner cannot confirm liveness"), "{body}");
    }

    /// A durable failed operation, as a fleet agent task from a crashed or
    /// sibling instance leaves behind in the persisted graph (#4416).
    fn durable_failed_operation_graph() -> codewhale_engine::work_graph::WorkGraphSnapshot {
        let mut graph = WorkGraph::from_snapshot(operation_graph(NodeState::Failed));
        let operation = WorkNodeId::derive(SESSION, "operation");
        graph
            .apply(
                WorkGraphChange::BindOperation {
                    node: operation,
                    binding: OperationBinding {
                        external: "fleet:run_1/task_1".to_string(),
                        durable: true,
                        last_observation: None,
                    },
                },
                ChangeCtx {
                    session_id: SESSION.to_string(),
                    now: 6,
                    idempotency_key: None,
                },
            )
            .expect("durable binding");
        graph.into_snapshot()
    }

    // Regression for #4416: a persisted failed-agent record stamped by
    // another session instance (boot id) must not appear in the default
    // work listing of a fresh session in the same workspace.
    #[test]
    fn prior_instance_failed_rows_stay_out_of_the_default_listing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager =
            codewhale_engine::session_manager::SessionManager::new(dir.path().to_path_buf())
                .expect("manager");
        manager
            .record_session_boot_owner(SESSION, "boot_other_instance")
            .expect("stamp other instance");

        let mut app = app();
        app.work_surface.session_owner_probe_dir = Some(dir.path().to_path_buf());
        let graph = durable_failed_operation_graph();
        restore_saved_graph(&mut app, &graph);

        let rows = super::model::project(&mut app);
        assert!(
            rows.iter()
                .all(|row| !row.label.contains("Verify installed build")),
            "prior-instance failed row leaked into the default listing: {rows:#?}"
        );
        assert!(
            rows.iter()
                .all(|row| !row.label.contains("needs input") && !row.label.contains("1 active")),
            "prior-instance residue must not count as live work: {rows:#?}"
        );
        // The record stays reachable through the explicit catalog, clearly
        // marked historical.
        let historical = app
            .work_surface
            .catalog_rows
            .iter()
            .find(|row| row.label.contains("Verify installed build"))
            .expect("historical row remains in the catalog");
        assert!(
            historical.detail.starts_with("prior session · "),
            "historical row must be labeled: {}",
            historical.detail
        );
    }

    // Ownership control for #4416: the same failed record owned by this
    // session instance still renders as actionable work.
    #[test]
    fn current_instance_failed_rows_still_render_in_the_default_listing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager =
            codewhale_engine::session_manager::SessionManager::new(dir.path().to_path_buf())
                .expect("manager");
        manager
            .record_session_boot_owner(
                SESSION,
                codewhale_engine::session_manager::current_session_boot_id(),
            )
            .expect("stamp current instance");

        let mut app = app();
        app.work_surface.session_owner_probe_dir = Some(dir.path().to_path_buf());
        let graph = durable_failed_operation_graph();
        restore_graph(&mut app, &graph);

        let rows = super::model::project(&mut app);
        assert!(
            rows.iter()
                .any(|row| row.label.contains("Verify installed build")),
            "this instance's own failed work must stay visible: {rows:#?}"
        );
    }

    // Regression for review of #5063: if a prior session persisted no graph,
    // the first graph captured later belongs to this process and must not be
    // mistaken for restored residue.
    #[test]
    fn first_live_graph_after_empty_prior_restore_stays_visible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager =
            codewhale_engine::session_manager::SessionManager::new(dir.path().to_path_buf())
                .expect("manager");
        manager
            .record_session_boot_owner(SESSION, "boot_other_instance")
            .expect("stamp other instance");

        let mut app = app();
        app.work_surface.session_owner_probe_dir = Some(dir.path().to_path_buf());
        app.current_session_id = Some(SESSION.to_string());
        app.restore_work_state(SESSION, std::path::Path::new("."), None)
            .expect("restore empty prior session");

        let graph = durable_failed_operation_graph();
        restore_graph(&mut app, &graph);
        let rows = super::model::project(&mut app);
        assert!(
            rows.iter()
                .any(|row| row.label.contains("Verify installed build")),
            "this instance's first live graph must stay visible: {rows:#?}"
        );
    }

    #[test]
    fn completed_operation_with_acceptance_is_not_rendered_done() {
        let mut graph = WorkGraph::from_snapshot(operation_graph(NodeState::Ready));
        let operation = WorkNodeId::derive(SESSION, "operation");
        graph
            .apply(
                WorkGraphChange::UpdateNode {
                    id: operation,
                    patch: codewhale_engine::work_graph::WorkNodePatch {
                        state: Some(NodeState::Completed),
                        acceptance: Some(vec![AcceptanceRequirement::EvidenceOfKind {
                            kind: EvidenceKindTag::ToolRun,
                        }]),
                        ..codewhale_engine::work_graph::WorkNodePatch::default()
                    },
                },
                ChangeCtx {
                    session_id: SESSION.to_string(),
                    now: 6,
                    idempotency_key: None,
                },
            )
            .expect("completed pending evidence");
        let graph = graph.into_snapshot();
        let mut app = app();
        restore_graph(&mut app, &graph);

        let rows = super::model::project(&mut app);
        assert!(
            rows[0].label.contains("Needs input") || rows[0].label.contains("1 needs input"),
            "{}",
            rows[0].label
        );
        let row = rows
            .iter()
            .find(|row| row.selectable)
            .expect("operation row");
        assert_eq!(row.mark, codewhale_tui_kit::glyphs::ATTENTION);
        assert!(row.detail.contains("completed · evidence pending"));
        assert_ne!(row.mark, "✓");
        let Some(SidebarRowAction::InspectWork { body, .. }) = row.primary_action.as_ref() else {
            panic!("completed operation must remain inspectable");
        };
        assert!(body.contains("evidence of kind tool run"), "{body}");
        assert!(
            body.contains("acceptance evidence is still missing"),
            "{body}"
        );
    }

    #[test]
    fn work_rows_open_graph_inspector_without_inline_controls() {
        let mut app = app();
        app.work_surface.placement = WorkSurfacePlacement::Right;
        app.work_surface.effective_placement = WorkSurfacePlacement::Right;
        let graph = operation_graph(NodeState::Active);
        restore_graph(&mut app, &graph);
        app.runtime_services
            .work
            .as_ref()
            .expect("Work Graph runtime")
            .reconcile_operation(
                SESSION,
                OperationOwnerSnapshot::new("shell:shell_1234abcd", OwnerState::Running, 1, 6),
            )
            .expect("live shell owner");

        let text = render_text(&mut app, 100, 6);
        assert!(!text.contains("[open]"), "{text}");
        assert!(!text.contains("[stop]"), "{text}");
        let row_y = app
            .work_surface
            .hitboxes
            .iter()
            .find(|hit| hit.id.0.starts_with("graph:"))
            .expect("graph hitbox")
            .row_y;
        let outcome = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: row_y,
                modifiers: KeyModifiers::NONE,
            },
        );
        let action = outcome.action.expect("inspector action");
        let SidebarRowAction::InspectWork {
            body, stop_action, ..
        } = &action
        else {
            panic!("expected Work inspector");
        };
        for section in [
            "Objective",
            "Prerequisites",
            "Downstream impact",
            "Binding + lifecycle owner",
            "Evidence vs acceptance",
            "Blockers / approvals",
            "Why next",
            "Provenance + last reconcile",
        ] {
            assert!(body.contains(section), "missing {section}: {body}");
        }
        assert!(matches!(
            stop_action.as_deref(),
            Some(SidebarRowAction::Command(command)) if command == "/jobs cancel shell_1234abcd"
        ));
        crate::tui::mouse_ui::apply_sidebar_row_action(&mut app, action);
        assert_eq!(
            app.view_stack.top_kind(),
            Some(crate::tui::views::ModalKind::Pager)
        );
    }

    #[test]
    fn narrow_render_hover_keeps_full_untruncated_row() {
        let mut app = app();
        app.todos.try_lock().expect("todos").add(
            "A deliberately long graph-owned work row".to_string(),
            TodoStatus::InProgress,
        );

        let _ = render_text(&mut app, 24, 4);
        let hover = app
            .sidebar_hover
            .sections
            .last()
            .and_then(|section| section.rows.first())
            .expect("hover row");
        assert!(hover.is_truncated);
        assert!(hover.full_text.contains("deliberately long graph-owned"));
        assert!(hover.stop_action.is_none());
    }

    #[test]
    fn narrow_file_activity_prioritizes_the_canonical_aggregate_label() {
        let mut app = app();
        app.workspace = PathBuf::from("/workspace/project");
        let result = codewhale_engine::tools::spec::ToolResult::success("ok").with_metadata(
            serde_json::json!({
                "mutation": {
                    "diff": "--- a/update.rs\n+++ b/update.rs\n@@ -1 +1 @@\n-old\n+new\n--- /dev/null\n+++ b/create.rs\n@@ -0,0 +1 @@\n+created\n--- a/delete.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-deleted\n",
                    "files": [
                        { "path": "update.rs", "outcome": "updated" },
                        { "path": "create.rs", "outcome": "created" },
                        { "path": "delete.rs", "outcome": "deleted" }
                    ],
                    "renames": [{ "from": "old.rs", "to": "new.rs" }]
                }
            }),
        );
        let receipt = FileMutationReceipt::from_success(&app.workspace, &result).expect("receipt");
        app.add_message(HistoryCell::Tool(ToolCell::PatchSummary(
            PatchSummaryCell {
                path: "4 files".to_string(),
                summary: "ok".to_string(),
                status: ToolStatus::Success,
                error: None,
                receipt: Some(receipt),
            },
        )));
        app.tool_details_by_cell.insert(
            0,
            ToolDetailRecord {
                tool_id: "file-multi".to_string(),
                tool_name: "File".to_string(),
                input: serde_json::json!({"action": "patch"}),
                output: Some("ok".to_string()),
            },
        );

        app.work_surface.placement = WorkSurfacePlacement::Right;
        app.work_surface.effective_placement = WorkSurfacePlacement::Right;
        let text = render_text(&mut app, 80, 6);
        assert!(text.contains("Wrote 4 files"), "{text}");
    }

    #[test]
    fn overflow_scroll_and_selection_remain_panel_owned() {
        let mut app = app();
        add_todos(&mut app, 8);
        let _ = render_text(&mut app, 80, 5);
        assert!(app.work_surface.total_rows > app.work_surface.visible_rows);

        let transcript_delta = app.viewport.pending_scroll_delta;
        let outcome = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 10,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(outcome.consumed);
        assert_eq!(app.viewport.pending_scroll_delta, transcript_delta);
        assert!(app.work_surface.scroll_offset > 0);
    }

    #[test]
    fn mouse_wheel_reaches_last_todo_across_top_surface_heights() {
        for height in [3, 5, 6, 8] {
            let mut app = app();
            add_todos(&mut app, 10);
            let _ = render_text(&mut app, 80, height);
            assert!(app.work_surface.total_rows > app.work_surface.visible_rows);
            let transcript_delta = app.viewport.pending_scroll_delta;

            let mut text = String::new();
            for _ in 0..16 {
                let outcome = super::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind: MouseEventKind::ScrollDown,
                        column: 10,
                        row: 1,
                        modifiers: KeyModifiers::NONE,
                    },
                );
                assert!(outcome.consumed, "height {height}");
                text = render_text(&mut app, 80, height);
            }

            assert!(
                text.contains("work item 9"),
                "last To-do was unreachable at surface height {height}: {text:?}"
            );
            assert_eq!(
                app.work_surface.scroll_offset,
                app.work_surface
                    .total_rows
                    .saturating_sub(app.work_surface.visible_rows.max(1)),
                "wheel did not reach the legal tail at surface height {height}"
            );
            assert_eq!(app.viewport.pending_scroll_delta, transcript_delta);
        }
    }

    #[test]
    fn mouse_wheel_reaches_last_todo_in_side_rail_placements() {
        for placement in [
            super::WorkSurfacePlacement::Left,
            super::WorkSurfacePlacement::Right,
        ] {
            let mut app = app();
            add_todos(&mut app, 10);
            app.work_surface.placement = placement;
            app.work_surface.effective_placement = placement;
            let _ = render_text(&mut app, 30, 6);

            let mut text = String::new();
            for _ in 0..16 {
                let outcome = super::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind: MouseEventKind::ScrollDown,
                        column: 10,
                        row: 1,
                        modifiers: KeyModifiers::NONE,
                    },
                );
                assert!(outcome.consumed, "placement {placement:?}");
                text = render_text(&mut app, 30, 6);
            }

            assert!(
                text.contains("work item 9"),
                "last To-do was unreachable in {placement:?}: {text:?}"
            );
        }
    }

    #[test]
    fn keyboard_end_reveals_last_todo_after_redraw() {
        let mut app = app();
        add_todos(&mut app, 10);
        let _ = render_text(&mut app, 80, 5);
        let _ = super::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
        );
        let _ = super::handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));

        let text = render_text(&mut app, 80, 5);

        assert!(text.contains("work item 9"), "{text:?}");
        assert_eq!(
            app.work_surface.scroll_offset,
            app.work_surface
                .total_rows
                .saturating_sub(app.work_surface.visible_rows.max(1))
        );
    }

    #[test]
    fn keyboard_navigation_is_panel_local_when_focused() {
        let mut app = app();
        add_todos(&mut app, 3);
        let _ = render_text(&mut app, 80, super::model::TOP_HEIGHT_MIN);
        assert!(
            super::handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT)
            )
            .is_some()
        );
        let first = app.work_surface.selected.clone();
        let _ = super::handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_ne!(app.work_surface.selected, first);
        assert!(app.work_surface.focused);
    }

    #[test]
    fn clicking_agents_tab_switches_active_panel() {
        let mut app = app();
        add_todos(&mut app, 1);
        app.subagent_cache.push(cached_worker(
            "agent-tab",
            "explore",
            Some("scout"),
            None,
            SubAgentStatus::Running,
        ));
        let _ = render_text(&mut app, 80, 8);
        // The to-do list opened first; the running worker is one tab over.
        assert_eq!(app.work_surface.panel, super::RailPanel::Tasks);
        let tab_area = app
            .work_surface
            .dock_tabs
            .iter()
            .find(|hitbox| {
                hitbox.target == super::model::DockTabTarget::Panel(super::RailPanel::Agents)
            })
            .map(|hitbox| hitbox.area)
            .expect("Agents tab");

        let down = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: tab_area.x,
                row: tab_area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(down.consumed);
        let up = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: tab_area.x,
                row: tab_area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(up.consumed);
        assert_eq!(app.work_surface.panel, super::RailPanel::Agents);
        assert!(app.work_surface.explicit_view);
        assert!(!app.work_surface.dismissed);
    }

    #[test]
    fn clicking_active_tab_dismisses_the_dock_until_new_work_arrives() {
        let mut app = app();
        app.work_surface.top_height = 8;
        add_todos(&mut app, 2);
        let _ = render_text(&mut app, 80, 8);
        let tab_area = app
            .work_surface
            .dock_tabs
            .iter()
            .find(|hitbox| {
                hitbox.target == super::model::DockTabTarget::Panel(super::RailPanel::Tasks)
            })
            .map(|hitbox| hitbox.area)
            .expect("Tasks tab");

        super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: tab_area.x,
                row: tab_area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: tab_area.x,
                row: tab_area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.work_surface.dismissed);
        assert_eq!(
            super::height(&mut app, 80, 24, AMPLE_BUDGET),
            0,
            "dismissed dock remains collapsed"
        );

        app.subagent_cache.push(cached_worker(
            "new-work",
            "explore",
            Some("new work"),
            None,
            SubAgentStatus::Running,
        ));
        assert!(
            super::height(&mut app, 80, 24, AMPLE_BUDGET) > 0,
            "new work re-shows dismissed dock"
        );
        assert!(!app.work_surface.dismissed);
    }

    #[test]
    fn dock_tabs_match_80x24_golden() {
        let mut app = app();
        add_todos(&mut app, 3);
        app.subagent_cache.push(cached_worker(
            "dock-golden-agent",
            "explore",
            Some("scout"),
            None,
            SubAgentStatus::Running,
        ));

        assert_matches_golden("dock_80x24", &render_golden_text(&mut app, 80, 24));
    }

    #[test]
    fn dock_selection_is_readable_and_close_target_stays_inside_small_hosts() {
        use super::model::DockTabTarget;
        use ratatui::style::Modifier;
        for theme_id in codewhale_palette::SELECTABLE_THEMES {
            let mut app = app();
            app.ui_theme = theme_id.ui_theme();
            app.work_surface.explicit_view = true;
            add_todos(&mut app, 1);
            let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
            terminal
                .draw(|frame| super::render(frame, frame.area(), &mut app))
                .unwrap();
            let tab = app
                .work_surface
                .dock_tabs
                .iter()
                .find(|tab| tab.target == DockTabTarget::Panel(super::RailPanel::Tasks))
                .unwrap();
            let cell = &terminal.backend().buffer()[(tab.area.x + 1, tab.area.y)];
            assert_eq!(cell.bg, app.ui_theme.selection_bg, "{theme_id:?}");
            assert!(!cell.modifier.contains(Modifier::REVERSED), "{theme_id:?}");
            if let Some(ratio) = codewhale_palette::contrast_ratio(cell.fg, cell.bg) {
                assert!(ratio >= 4.5, "{theme_id:?}: {cell:?} ({ratio})");
            } else {
                // Native terminal colors are user supplied and cannot be measured here.
                assert_eq!(*theme_id, codewhale_palette::ThemeId::Terminal);
            }
        }
        for width in [1, 2, 3, 8, 16, 40, 60, 80] {
            for placement in [WorkSurfacePlacement::Top, WorkSurfacePlacement::Bottom] {
                let mut app = app();
                app.work_surface.explicit_view = true;
                app.work_surface.effective_placement = placement;
                let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
                terminal
                    .draw(|frame| super::render(frame, frame.area(), &mut app))
                    .unwrap();
                let close = app
                    .work_surface
                    .dock_tabs
                    .iter()
                    .find(|tab| tab.target == DockTabTarget::Close)
                    .unwrap();
                assert!(close.area.right() <= width);
                assert!(close.area.width > 0);
            }
        }
    }

    /// #6502: the close control names Esc only while Esc closes the dock.
    /// Unfocused, Esc stops the running turn, so the `×` stands alone and
    /// the Esc hint stays with the turn status.
    #[test]
    fn close_control_names_esc_only_while_esc_closes_the_dock() {
        let tab_row = |app: &mut App| {
            render_rows(app, 80, 8)
                .into_iter()
                .find(|row| row.contains("Tasks"))
                .expect("dock tab row")
        };
        let mut app = app();
        add_todos(&mut app, 3);
        app.is_loading = true;

        let row = tab_row(&mut app);
        assert!(
            !row.contains("Esc"),
            "unfocused dock must not claim Esc: {row:?}"
        );

        app.work_surface.focused = true;
        let row = tab_row(&mut app);
        assert!(
            row.contains("Esc"),
            "focused dock names its close key: {row:?}"
        );
        assert!(
            super::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_some(),
            "the advertised Esc is the one that closes the dock"
        );
        assert!(app.work_surface.dismissed);
        assert!(!app.work_surface.focused);
    }

    /// #6502 review: a row action that opens no view (the Context panel's
    /// `/compact` row) leaves a stale `opened` owner. The hint must follow
    /// what Esc will really do: close the dock when nothing is stacked above
    /// it, and stay quiet while a detail pager owns Esc.
    #[test]
    fn esc_hint_follows_the_pending_detail_state() {
        let tab_row = |app: &mut App| {
            render_rows(app, 80, 8)
                .into_iter()
                .find(|row| row.contains("Tasks"))
                .expect("dock tab row")
        };
        let mut app = app();
        add_todos(&mut app, 3);
        app.is_loading = true;
        app.work_surface.focused = true;
        let row = super::model::project(&mut app)
            .into_iter()
            .find(|row| row.selectable)
            .expect("work row");

        // A detail pager is on screen: Esc closes it, not the dock.
        app.work_surface.opened = Some(row.id.clone());
        app.view_stack.push(crate::tui::pager::PagerView::from_text(
            "Work · test".to_string(),
            "body",
            40,
        ));
        let rendered = tab_row(&mut app);
        assert!(
            !rendered.contains("Esc"),
            "Esc belongs to the open detail: {rendered:?}"
        );
        app.view_stack.pop();

        // The row's command opened nothing: `opened` is stale, and the
        // advertised Esc closes the dock in one press.
        let rendered = tab_row(&mut app);
        assert!(
            rendered.contains("Esc"),
            "stale owner must not hide the dock's close key: {rendered:?}"
        );
        assert!(
            super::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_some()
        );
        assert!(app.work_surface.dismissed, "one Esc closes the dock");
        assert!(app.work_surface.opened.is_none());
    }

    #[test]
    fn narrow_dock_drops_counts_before_optional_tabs() {
        let mut app = app();
        add_todos(&mut app, 3);
        app.subagent_cache.push(cached_worker(
            "agent-count",
            "explore",
            Some("scout"),
            None,
            SubAgentStatus::Running,
        ));
        let first_row = render_rows(&mut app, 40, 8)
            .into_iter()
            .next()
            .expect("dock tab row");

        assert!(first_row.contains("Tasks"), "{first_row:?}");
        assert!(first_row.contains("Fleet"), "{first_row:?}");
        assert!(!first_row.contains("Tasks 3"), "{first_row:?}");
        assert!(!first_row.contains("Fleet 1"), "{first_row:?}");
        assert!(first_row.contains("Terminal"), "{first_row:?}");
        assert!(!first_row.contains("Context"), "{first_row:?}");
        // Shed from the right: price goes before any work view.
        assert!(!first_row.contains("Cost"), "{first_row:?}");
    }

    #[test]
    fn empty_panel_releases_plain_y_before_composer_dispatch() {
        let mut app = app();
        app.work_surface.last_area = Some(ratatui::layout::Rect::new(0, 0, 80, 8));
        app.work_surface.focused = true;
        app.work_surface.explicit_view = false;
        let outcome = super::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(outcome.is_none());
        assert!(!app.work_surface.focused);
    }

    #[test]
    fn printable_keys_release_panel_focus_for_composer() {
        let mut app = app();
        add_todos(&mut app, 1);
        let _ = render_text(&mut app, 80, super::model::TOP_HEIGHT_MIN);
        assert!(
            super::handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
            )
            .is_some()
        );

        let outcome = super::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        assert!(outcome.is_none());
        assert!(!app.work_surface.focused);
    }

    #[test]
    fn side_placements_reuse_the_same_graph_rows() {
        for (placement, expected_chat_x, expected_rail_x) in [
            (super::WorkSurfacePlacement::Left, 30, 0),
            (super::WorkSurfacePlacement::Right, 0, 70),
        ] {
            let mut app = app();
            add_todos(&mut app, 2);
            app.work_surface.placement = placement;
            assert_eq!(super::height(&mut app, 100, 24, AMPLE_BUDGET), 0);
            let area = ratatui::layout::Rect::new(0, 0, 100, 12);
            let (chat, rail) = super::split_chat(&mut app, area, 0);
            let rail = rail.expect("side rail");
            assert_eq!(chat.x, expected_chat_x);
            assert_eq!(rail.x, expected_rail_x);
            assert_eq!(rail.width, 30);
            assert!(
                app.work_surface
                    .latest_rows
                    .iter()
                    .any(|row| row.label == "work item 1")
            );
        }
    }

    #[test]
    fn divider_drag_resizes_top_left_and_right_surfaces() {
        let mut top = app();
        add_todos(&mut top, 3);
        let _ = render_text(&mut top, 80, 3);
        let down = super::handle_mouse(
            &mut top,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(down.consumed);
        let _ = super::handle_mouse(
            &mut top,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 20,
                row: 7,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(top.work_surface.top_height, 8);

        for (placement, drag_column, expected_width) in [
            (WorkSurfacePlacement::Left, 39, 40),
            (WorkSurfacePlacement::Right, 10, 26),
        ] {
            let mut side = app();
            add_todos(&mut side, 2);
            side.work_surface.placement = placement;
            side.work_surface.effective_placement = placement;
            let _ = render_text(&mut side, 30, 8);
            let divider_column = if placement == WorkSurfacePlacement::Left {
                29
            } else {
                0
            };
            let _ = super::handle_mouse(
                &mut side,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: divider_column,
                    row: 2,
                    modifiers: KeyModifiers::NONE,
                },
            );
            let _ = super::handle_mouse(
                &mut side,
                MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column: drag_column,
                    row: 2,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert_eq!(
                side.work_surface.side_width, expected_width,
                "{placement:?}"
            );
        }
    }

    #[test]
    fn divider_hover_and_drag_render_a_discoverable_handle() {
        let mut app = app();
        add_todos(&mut app, 3);
        let resting = render_text(&mut app, 80, 3);
        assert!(resting.contains('─'), "{resting}");

        let hover = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 20,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(hover.consumed);
        assert!(app.work_surface.divider_hovered);
        let hovered = render_text(&mut app, 80, 3);
        assert!(hovered.contains('━'), "{hovered}");

        let _ = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
        );
        let dragging = render_text(&mut app, 80, 3);
        assert!(dragging.contains('━'), "{dragging}");
    }

    #[test]
    fn top_bar_excludes_generic_operations() {
        let mut operation_app = app();
        let graph = operation_graph(NodeState::Failed);
        restore_graph(&mut operation_app, &graph);

        assert_eq!(super::height(&mut operation_app, 100, 24, AMPLE_BUDGET), 0);
        assert!(operation_app.work_surface.latest_rows.is_empty());

        let mut todo_app = app();
        add_todos(&mut todo_app, 2);
        assert!(super::height(&mut todo_app, 100, 24, AMPLE_BUDGET) > 0);
        assert!(
            todo_app
                .work_surface
                .latest_rows
                .iter()
                .all(|row| row.id.0.starts_with("graph:") || row.id.0.starts_with("worker:"))
        );
        assert!(
            todo_app
                .work_surface
                .latest_rows
                .iter()
                .all(|row| !row.label.starts_with("Work ·"))
        );
    }

    #[test]
    fn opened_row_toggles_closed_without_losing_selection() {
        let mut app = app();
        add_todos(&mut app, 1);
        let row = super::model::project(&mut app)
            .into_iter()
            .find(|row| row.selectable)
            .expect("work row");
        let open = row.primary_action.clone();

        assert!(super::interaction::activate_primary(&mut app, &row.id, open.clone()).is_some());
        // The action's pager is on screen, so the second activation is a
        // toggle-close.
        app.view_stack.push(crate::tui::pager::PagerView::from_text(
            "Work · test".to_string(),
            "body",
            40,
        ));
        assert!(super::interaction::activate_primary(&mut app, &row.id, open).is_none());
        assert!(app.work_surface.opened.is_none());
        assert_eq!(app.work_surface.selected.as_ref(), Some(&row.id));
    }

    #[test]
    fn a_click_after_the_pager_closed_itself_reopens_instead_of_going_dead() {
        // q/Esc inside the pager pops it without clearing `opened`. The next
        // click on that row must reopen its world, not be swallowed by a
        // stale toggle (owner regression report, 2026-08-04).
        let mut app = app();
        add_todos(&mut app, 1);
        let row = super::model::project(&mut app)
            .into_iter()
            .find(|row| row.selectable)
            .expect("work row");
        let open = row.primary_action.clone();

        assert!(super::interaction::activate_primary(&mut app, &row.id, open.clone()).is_some());
        // The pager was closed from inside itself; `opened` is now stale.
        assert_eq!(app.work_surface.opened.as_ref(), Some(&row.id));
        assert!(app.view_stack.is_empty());

        let reopened = super::interaction::activate_primary(&mut app, &row.id, open);
        assert!(
            reopened.is_some(),
            "a stale opened owner must not swallow the next activation"
        );
        assert_eq!(app.work_surface.opened.as_ref(), Some(&row.id));
    }

    /// Settled to-dos keep their rows across the recent-only TTL and new user
    /// turns. Finished sub-agents collapse into the Subagents Archived count
    /// (still reachable via the Agents panel) so fan-outs do not permanently
    /// eat the transcript.
    #[test]
    fn settled_todos_stay_and_finished_workers_collapse_after_ttl() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        {
            let mut todos = app.todos.try_lock().expect("todos");
            todos.add("ship the fix".to_string(), TodoStatus::Completed);
            todos.add("verify the fix".to_string(), TodoStatus::Completed);
        }
        app.subagent_cache.push(cached_worker(
            "agent-settled",
            "builder",
            None,
            None,
            SubAgentStatus::Completed,
        ));

        app.work_surface.set_presentation_now_ms(0);
        let first = super::model::project_visible(&mut app);
        assert!(
            first.iter().any(|row| row.id.0.starts_with("graph:")),
            "settled to-dos must be listed: {first:?}"
        );
        assert!(
            !first.iter().any(|row| row.id.0.starts_with("worker:")),
            "workers are the agents view's rows, never the tasks view's: {first:?}"
        );
        let roster = super::model::visible_rows_for(&mut app, super::RailPanel::Agents);
        assert!(
            roster.iter().any(|row| row.id.0 == "worker:agent-settled"),
            "the roster retains a finished worker: {roster:?}"
        );

        app.work_surface
            .set_presentation_now_ms(super::model::RECENT_ONLY_TTL_MS + 1);
        app.work_surface.note_user_turn_or_new_operation();
        let later = super::model::project_visible(&mut app);
        assert!(
            later.iter().any(|row| row.id.0.starts_with("graph:")),
            "a settled to-do must survive the TTL and the next user turn: {later:?}"
        );
        assert!(
            super::height(&mut app, 100, 40, AMPLE_BUDGET) > 0,
            "the strip must keep its height while it holds settled work"
        );
    }

    /// A to-do row says its state in words, in the `/task digest` vocabulary.
    /// Dropping the words (2011b9b11 conflated them with the redundant kind
    /// label) was half of owner regression A1.
    #[test]
    fn todo_rows_carry_their_status_words() {
        let mut app = app();
        add_todos(&mut app, 3);
        let rows = super::model::project(&mut app);
        let todo_details: Vec<&str> = rows
            .iter()
            .filter(|row| row.id.0.starts_with("graph:"))
            .map(|row| row.detail.as_str())
            .collect();
        assert!(
            todo_details.contains(&"in progress"),
            "the active step says so in words: {todo_details:?}"
        );
        assert!(
            todo_details.contains(&"pending"),
            "a pending step is labeled, not blank: {todo_details:?}"
        );

        // And the words are painted, not just projected.
        let text = render_text(&mut app, 100, 6);
        assert!(text.contains("in progress"), "{text}");
        assert!(text.contains("pending"), "{text}");
    }

    /// Top strip collapses completed/cancelled workers into an Archived count
    /// while keeping live (and failed) workers as rows. Agents panel still
    /// lists every worker — see the click test below.
    #[test]
    fn the_roster_keeps_live_failed_and_finished_workers() {
        let mut app = app();
        app.work_surface.placement = super::WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = super::WorkSurfacePlacement::Top;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-live",
            "scout",
            None,
            None,
            SubAgentStatus::Running,
        ));
        app.subagent_cache.push(cached_worker(
            "agent-done",
            "builder",
            None,
            None,
            SubAgentStatus::Completed,
        ));
        app.subagent_cache.push(cached_worker(
            "agent-failed",
            "verifier",
            None,
            None,
            SubAgentStatus::Failed("boom".to_string()),
        ));

        // The roster is a history: every worker keeps its row, in every
        // state, and the tasks view never lists one.
        let rows = super::model::visible_rows_for(&mut app, super::RailPanel::Agents);
        let ids: Vec<&str> = rows.iter().map(|row| row.id.0.as_str()).collect();
        assert!(ids.contains(&"section:agents"), "{ids:?}");
        for id in [
            "worker:agent-live",
            "worker:agent-failed",
            "worker:agent-done",
        ] {
            assert!(ids.contains(&id), "{id} in the roster: {ids:?}");
        }
        let tasks = super::model::project_visible(&mut app);
        assert!(
            !tasks.iter().any(|row| row.id.0.starts_with("worker:")),
            "{tasks:?}"
        );
        assert_eq!(super::model::live_agent_row_count(&mut app), 2);
    }

    #[test]
    fn subagent_header_opens_the_full_agents_register() {
        let mut app = app();
        app.work_surface.placement = super::WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = super::WorkSurfacePlacement::Top;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-archived",
            "builder",
            None,
            None,
            SubAgentStatus::Completed,
        ));

        // Nothing live: the dock stays down until the user opens agents.
        assert_eq!(super::height(&mut app, 100, 24, AMPLE_BUDGET), 0);
        super::interaction::select_dock_panel(&mut app, super::RailPanel::Agents);
        let top = render_text(&mut app, 100, 4);
        assert!(top.contains("Subagents 1"), "{top}");
        let header_y = app
            .work_surface
            .hitboxes
            .iter()
            .find(|hit| hit.id.0 == "section:agents")
            .expect("subagent header must be a real hit target")
            .row_y;
        let action = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: header_y,
                modifiers: KeyModifiers::NONE,
            },
        )
        .action
        .expect("subagent header must dispatch its primary action");
        assert_eq!(action, SidebarRowAction::ShowSubagentsPanel);
        super::interaction::select_dock_panel(&mut app, super::RailPanel::Agents);

        let agents = render_text(&mut app, 100, 6);
        assert!(
            agents.contains("agent-archived") || agents.contains("builder"),
            "the full Agents register keeps the archived worker reachable: {agents}"
        );
    }

    #[test]
    fn agent_entry_focuses_a_visible_row_and_esc_returns_to_composer() {
        let mut app = app();
        app.work_surface.placement = super::WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = super::WorkSurfacePlacement::Top;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-live",
            "builder",
            None,
            None,
            SubAgentStatus::Running,
        ));

        let rendered = render_text(&mut app, 100, 8);
        assert!(rendered.contains("builder"), "{rendered}");

        assert!(super::enter_agents(&mut app));
        assert_eq!(app.work_surface.panel, super::RailPanel::Agents);
        assert!(app.work_surface.focused);
        assert_eq!(
            app.work_surface.selected.as_ref().map(|row| row.0.as_str()),
            Some("worker:agent-live")
        );

        let handled = super::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(handled.is_some());
        assert!(
            !app.work_surface.focused,
            "Esc returns ownership to composer"
        );
    }

    #[test]
    fn agent_entry_rejects_a_surface_that_is_not_rendered() {
        let mut app = app();
        app.work_surface.placement = super::WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = super::WorkSurfacePlacement::Top;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-live",
            "builder",
            None,
            None,
            SubAgentStatus::Running,
        ));
        // A previous frame's rectangle is not evidence that this Agent row
        // was painted. Only the renderer's current hitboxes may transfer
        // keyboard ownership away from the composer.
        app.work_surface.last_area = Some(ratatui::layout::Rect::new(0, 0, 100, 5));
        assert!(app.work_surface.hitboxes.is_empty());

        assert!(!super::enter_agents(&mut app));
        assert!(
            !app.work_surface.focused,
            "hidden surface cannot own arrows"
        );
    }

    #[test]
    fn compact_top_surface_keeps_goal_todos_and_named_agent_visible() {
        for (width, terminal_height) in [(160, 48), (120, 32), (80, 24)] {
            let mut app = app();
            app.work_surface.placement = super::WorkSurfacePlacement::Top;
            app.work_surface.panel = super::RailPanel::Tasks;
            app.work_surface.top_height = super::model::TOP_HEIGHT_MIN;
            app.goal.objective = Some("ship the release".to_string());
            add_todos(&mut app, 3);
            app.current_session_id = Some(SESSION.to_string());
            app.subagent_cache.push(cached_worker(
                "agent-harbor",
                "builder",
                Some("Harbor"),
                None,
                SubAgentStatus::Running,
            ));

            let budget = crate::tui::ui::rail_row_budget(&app, width, terminal_height, false);
            let height = super::height(&mut app, width, terminal_height, budget);
            assert_eq!(
                height,
                super::model::TOP_HEIGHT_MIN,
                "{width}x{terminal_height} must seat the readable compact surface"
            );
            // The to-do list opens first: goal title + the progress
            // receipt. The named agent lives one view over.
            let rendered = render_text(&mut app, width, height);
            assert!(
                rendered.contains("ship the release"),
                "{width}x{terminal_height}: {rendered}"
            );
            assert!(
                rendered.contains("3 left"),
                "{width}x{terminal_height}: {rendered}"
            );
            super::interaction::select_dock_panel(&mut app, super::RailPanel::Agents);
            let height = super::height(&mut app, width, terminal_height, budget);
            let agents = render_text(&mut app, width, height);
            assert!(
                agents.contains("Harbor"),
                "{width}x{terminal_height}: {agents}"
            );
            app.work_surface.explicit_view = false;
            let height = super::height(&mut app, width, terminal_height, budget);
            let _ = render_text(&mut app, width, height);

            assert!(super::enter_agents(&mut app));
            assert_eq!(
                app.work_surface.selected.as_ref().map(|row| row.0.as_str()),
                Some("worker:agent-harbor"),
                "the advertised Left control must focus the named visible Agent"
            );
        }
    }

    #[test]
    fn starved_surface_cannot_take_keyboard_focus() {
        let mut app = app();
        app.work_surface.placement = super::WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = super::WorkSurfacePlacement::Top;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-hidden",
            "builder",
            Some("Harbor"),
            None,
            SubAgentStatus::Running,
        ));

        assert_eq!(super::height(&mut app, 80, 12, 0), 0);
        assert!(app.work_surface.last_area.is_none());
        assert!(!super::enter_agents(&mut app));
        assert!(!app.work_surface.focused);
        assert!(
            super::handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
            )
            .is_none()
        );
        assert!(!app.work_surface.focused);
    }

    /// Acceptance for owner regression A2: an agent row is a door in the
    /// Agents panel too, and a FINISHED agent's world still opens — the
    /// panel is a standing register, not a live-only view. Since v0.9.7 the
    /// door leads to the agent's transcript (which explains itself when no
    /// capture exists yet), not to the details projection.
    #[test]
    fn agents_panel_click_opens_the_transcript_even_for_finished_agents() {
        let mut app = app();
        app.work_surface.panel = super::RailPanel::Agents;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-finished",
            "builder",
            None,
            None,
            SubAgentStatus::Completed,
        ));

        let _ = render_text(&mut app, 100, 6);
        let row_y = app
            .work_surface
            .hitboxes
            .iter()
            .find(|hit| hit.id.0 == "worker:agent-finished")
            .expect("finished agent row must keep a hitbox in the Agents panel")
            .row_y;
        let action = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: row_y,
                modifiers: KeyModifiers::NONE,
            },
        )
        .action
        .expect("click on a finished agent row must dispatch its primary action");
        assert_eq!(
            action,
            SidebarRowAction::OpenAgentTranscript {
                agent_id: "agent-finished".to_string()
            }
        );
        crate::tui::mouse_ui::apply_sidebar_row_action(&mut app, action);
        assert!(
            app.agent_focus
                .as_ref()
                .is_some_and(|focus| focus.is("agent-finished")),
            "the finished agent's transcript must actually take focus"
        );
    }

    /// Acceptance for owner regression A1: to-do rows are doors in the
    /// Pinned panel too — clicking one opens the work inspector.
    #[test]
    fn tasks_view_todo_rows_stay_clickable() {
        let mut app = app();
        app.work_surface.panel = super::RailPanel::Tasks;
        add_todos(&mut app, 2);

        let _ = render_text(&mut app, 100, 6);
        let hit = app
            .work_surface
            .hitboxes
            .iter()
            .find(|hit| hit.id.0.starts_with("graph:"))
            .expect("tasks view to-do rows must keep hitboxes")
            .clone();
        let action = super::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: hit.row_y,
                modifiers: KeyModifiers::NONE,
            },
        )
        .action
        .expect("click on a to-do row must dispatch its primary action");
        assert!(
            matches!(action, SidebarRowAction::InspectWork { .. }),
            "a to-do row opens the work inspector: {action:?}"
        );
    }

    /// Opening the sub-agent register must not hide the to-do list — both
    /// durable surfaces stay visible together (owner report, 0.9.6). The
    /// dock opens on TODO; AGENTS is the next tab (founder, 2026-09-03).
    #[test]
    fn agents_and_tasks_are_separate_views_and_the_dock_opens_on_todo() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-live",
            "scout",
            None,
            None,
            SubAgentStatus::Running,
        ));
        add_todos(&mut app, 2);

        // The auto rule: the to-do list first, and only to-dos in it.
        super::model::resolve_view(&mut app);
        assert_eq!(app.work_surface.panel, super::RailPanel::Tasks);
        let ids: Vec<String> = super::model::visible_rows_for_panel(&mut app)
            .iter()
            .map(|row| row.id.0.clone())
            .collect();
        assert!(ids.iter().any(|id| id.starts_with("graph:")), "{ids:?}");
        assert!(!ids.iter().any(|id| id.starts_with("worker:")), "{ids:?}");

        // One key forward: the agents view, the roster only.
        super::cycle_view(&mut app, true);
        assert_eq!(app.work_surface.panel, super::RailPanel::Agents);
        assert!(app.work_surface.explicit_view);
        let ids: Vec<String> = super::model::visible_rows_for_panel(&mut app)
            .iter()
            .map(|row| row.id.0.clone())
            .collect();
        assert!(ids.iter().any(|id| id.starts_with("worker:")), "{ids:?}");
        assert!(
            !ids.iter().any(|id| id.starts_with("graph:")),
            "the agents view is the roster, not the to-do list: {ids:?}"
        );

        // Back, and Esc hands the choice back to the auto rule.
        super::cycle_view(&mut app, false);
        assert_eq!(app.work_surface.panel, super::RailPanel::Tasks);
        let _ = render_text(&mut app, 80, 8);
        assert!(app.work_surface.focused);
        let handled = super::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(handled.is_some());
        assert!(!app.work_surface.explicit_view);
        assert!(app.work_surface.dismissed);
    }

    #[test]
    fn cycling_visits_every_view_in_order_and_an_empty_view_still_paints() {
        let mut app = app();
        add_todos(&mut app, 1);
        let mut seen = vec![];
        for _ in 0..super::RailPanel::ORDER.len() {
            super::cycle_view(&mut app, true);
            seen.push(app.work_surface.panel);
            let height = super::height(&mut app, 80, 24, AMPLE_BUDGET);
            assert!(
                height > 0,
                "{:?} must keep a strip while explicitly open",
                app.work_surface.panel
            );
        }
        let mut expected = super::RailPanel::ORDER.to_vec();
        expected.rotate_left(1); // the fixture starts on tasks, the first tab
        assert_eq!(seen, expected);
        // An empty explicit view names itself instead of going blank.
        super::interaction::select_dock_panel(&mut app, super::RailPanel::Files);
        let text = render_text(&mut app, 80, 5);
        assert!(text.contains("no files touched this session"), "{text}");
    }

    /// The register header is a two-way door: open the Agents panel, then the
    /// same click returns to Tasks, so the to-do list is never stranded.
    #[test]
    fn subagent_header_returns_to_tasks_from_the_agents_view() {
        let mut app = app();
        app.work_surface.placement = super::WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = super::WorkSurfacePlacement::Top;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-archived",
            "builder",
            None,
            None,
            SubAgentStatus::Completed,
        ));
        // A finished worker alone opens nothing; the user cycles to agents.
        assert_eq!(super::height(&mut app, 100, 24, AMPLE_BUDGET), 0);
        super::interaction::select_dock_panel(&mut app, super::RailPanel::Agents);

        let click_header = |app: &mut App| -> SidebarRowAction {
            let header_y = app
                .work_surface
                .hitboxes
                .iter()
                .find(|hit| hit.id.0 == "section:agents")
                .expect("subagent header is a real hit target")
                .row_y;
            super::handle_mouse(
                app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 2,
                    row: header_y,
                    modifiers: KeyModifiers::NONE,
                },
            )
            .action
            .expect("subagent header dispatches its primary action")
        };

        let _ = render_text(&mut app, 100, 6);
        let action = click_header(&mut app);
        assert_eq!(action, SidebarRowAction::ShowSubagentsPanel);
        crate::tui::mouse_ui::apply_sidebar_row_action(&mut app, action);
        assert_eq!(
            app.work_surface.panel,
            super::RailPanel::Tasks,
            "clicking the header inside the register returns to Tasks"
        );
    }

    /// ⌥V opens the selected work row's own details; the transcript pager is
    /// only the fallback when no row is selected (owner report, 0.9.6).
    #[test]
    fn details_chord_opens_the_selected_work_row() {
        let mut app = app();
        app.current_session_id = Some(SESSION.to_string());
        app.work_surface.panel = super::RailPanel::Agents;
        add_todos(&mut app, 2);
        let _ = render_text(&mut app, 100, 6);

        let rows = super::model::visible_rows_for_panel(&mut app);
        let todo_row = rows
            .iter()
            .find(|row| row.id.0.starts_with("graph:"))
            .expect("a to-do row projects")
            .clone();
        app.work_surface.focused = true;
        app.work_surface.selected = Some(todo_row.id.clone());

        let handled = super::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT),
        );
        assert!(
            matches!(handled, Some(Some(SidebarRowAction::InspectWork { .. }))),
            "⌥V opens the selected row's own details: {handled:?}"
        );
    }

    /// T1: a real right-click over an agent row opens that row's menu. The
    /// work surface's catch-all arm marked every event consumed, so
    /// `handle_mouse_event` returned before its right-click branch and no
    /// menu ever opened over the dock.
    #[test]
    fn right_click_on_an_agent_row_opens_its_menu() {
        use crate::tui::views::{ContextMenuAction, ModalKind, ViewEvent};
        use ratatui::{buffer::Buffer, layout::Rect};

        let mut app = app();
        app.work_surface.panel = super::RailPanel::Agents;
        app.current_session_id = Some(SESSION.to_string());
        app.subagent_cache.push(cached_worker(
            "agent-live",
            "builder",
            None,
            None,
            SubAgentStatus::Running,
        ));
        let _ = render_text(&mut app, 100, 6);
        let row_y = app
            .work_surface
            .hitboxes
            .iter()
            .find(|hit| hit.id.0 == "worker:agent-live")
            .expect("the live agent row is painted")
            .row_y;

        let events = crate::tui::mouse_ui::handle_mouse_event(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 2,
                row: row_y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(events.is_empty());
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::ContextMenu));

        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        app.view_stack.render(area, &mut buf);
        let text: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        assert_eq!(text.matches("Focus agent").count(), 1, "{text}");
        assert!(
            text.contains("Stop agent…"),
            "a running agent can be stopped"
        );
        assert!(!text.contains("Message agent") && !text.contains("Open transcript"));
        assert!(
            !text.contains("Command palette"),
            "app chrome stays off a row's menu"
        );

        // Enter runs the primary entry: the same focus a left click runs.
        let events = app
            .view_stack
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(
                events.as_slice(),
                [ViewEvent::ContextMenuSelected {
                    action: ContextMenuAction::Row(SidebarRowAction::OpenAgentTranscript { agent_id }),
                }] if agent_id == "agent-live"
            ),
            "{events:?}"
        );
    }
}
