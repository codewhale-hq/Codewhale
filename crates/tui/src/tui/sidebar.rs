//! Sub-agent row projection shared by the agent details view and the
//! roster: display name, typed status word, and child progress per worker.
//!
//! This is what is left of the classic sidebar (Pinned / Activity / Agents /
//! Context line panels). Those panels were retired when every dock view moved
//! onto the work surface's row pipeline (2026-09-02); the projection below is
//! the part other surfaces still read.

use crate::tools::subagent::{AgentWorkerStatus, SubAgentStatus, localized_whale_display_names};

use super::app::{AgentCurrentActivityStatus, App};

#[derive(Debug, Clone, Default)]
pub struct SidebarAgentRow {
    pub id: String,
    pub parent_run_id: Option<String>,
    pub name: String,
    pub model: Option<String>,
    pub status: String,
    pub steps_taken: u32,
    pub duration_ms: Option<u64>,
    /// `(settled, total)` over this row's direct children, when it has any
    /// (#5479). A fan-out parent's own status says nothing about whether the
    /// work it launched is finished; this is the "5/6 agents done" fact the
    /// rail otherwise makes you count by eye. `None` for a leaf.
    pub children_settled: Option<(usize, usize)>,
}

pub(crate) fn sidebar_agent_rows(app: &App) -> Vec<SidebarAgentRow> {
    let cached_ids: std::collections::HashSet<&str> = app
        .subagent_cache
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    let display_names = localized_whale_display_names(
        app.subagent_cache
            .iter()
            .map(|agent| (agent.agent_id.as_str(), agent.nickname.as_deref())),
        app.ui_locale.tag(),
    );
    let mut rows: Vec<SidebarAgentRow> = app
        .subagent_cache
        .iter()
        .map(|agent| {
            let current_activity = app
                .agent_progress_meta
                .get(&agent.agent_id)
                .and_then(|meta| meta.current_activity.as_ref());
            // The dispatch name leads (#5287). Generated whales name the
            // agents that have none, locale-derived from the neutral agent
            // id; never replay a persisted label from another language.
            let display_name = app
                .agent_given_name(&agent.agent_id)
                .or_else(|| {
                    agent
                        .child_route
                        .as_ref()
                        .and_then(|route| route.resolved_profile_id.as_deref())
                        .map(str::trim)
                        .filter(|profile| !profile.is_empty())
                        .map(str::to_string)
                })
                .or_else(|| display_names.get(&agent.agent_id).cloned())
                .or_else(|| app.agent_label_map.get(&agent.agent_id).cloned())
                .unwrap_or_else(|| agent.name.clone());
            SidebarAgentRow {
                id: agent.agent_id.clone(),
                parent_run_id: agent.parent_run_id.clone(),
                name: display_name,
                model: Some(agent.model.clone()).filter(|model| !model.trim().is_empty()),
                status: current_activity
                    .map(|activity| {
                        sidebar_current_activity_status_text(activity.status, app.ui_locale)
                    })
                    .or_else(|| {
                        agent.worker_status.map(|status| {
                            std::borrow::Cow::Borrowed(sidebar_worker_status_text(status))
                        })
                    })
                    .unwrap_or_else(|| {
                        std::borrow::Cow::Borrowed(subagent_status_text(&agent.status))
                    })
                    .into_owned(),
                steps_taken: agent.steps_taken,
                duration_ms: Some(agent.duration_ms),
                // Filled in by `annotate_child_progress` once every row exists.
                children_settled: None,
            }
        })
        .collect();

    rows.extend(
        app.agent_progress
            .iter()
            .filter(|(id, _)| !cached_ids.contains(id.as_str()))
            .map(|(id, _progress)| {
                // Progress-only rows do not carry a generated whale name yet;
                // keep their existing stable Agent-N placeholder until the
                // manager snapshot arrives.
                let display_name = app
                    .agent_label_map
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_else(|| id.clone());
                let meta = app.agent_progress_meta.get(id.as_str());
                let current_activity = meta.and_then(|meta| meta.current_activity.as_ref());
                SidebarAgentRow {
                    id: id.clone(),
                    parent_run_id: meta.and_then(|meta| meta.parent_run_id.clone()),
                    name: display_name,
                    model: meta.and_then(|meta| meta.resolved_model.clone()),
                    status: current_activity
                        .map(|activity| {
                            sidebar_current_activity_status_text(activity.status, app.ui_locale)
                        })
                        .unwrap_or(std::borrow::Cow::Borrowed(sidebar_worker_status_text(
                            AgentWorkerStatus::Running,
                        )))
                        .into_owned(),
                    steps_taken: 0,
                    duration_ms: None,
                    children_settled: None,
                }
            }),
    );

    let mut rows = sort_sidebar_agent_rows_as_tree(rows);
    annotate_child_progress(&mut rows);
    rows
}

/// Fill in each row's `children_settled` from its direct children.
///
/// Counted over the rows actually present: a child whose record has aged out of
/// the ledger cannot be counted, and inventing a denominator that included it
/// would misreport progress as worse than it is.
fn annotate_child_progress(rows: &mut [SidebarAgentRow]) {
    let mut totals: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for row in rows.iter() {
        let Some(parent) = row.parent_run_id.as_deref() else {
            continue;
        };
        let entry = totals.entry(parent.to_string()).or_insert((0, 0));
        entry.1 += 1;
        if sidebar_agent_status_is_terminal(row.status.as_str()) {
            entry.0 += 1;
        }
    }
    for row in rows.iter_mut() {
        row.children_settled = totals.get(&row.id).copied();
    }
}

fn sort_sidebar_agent_rows_as_tree(rows: Vec<SidebarAgentRow>) -> Vec<SidebarAgentRow> {
    let known_ids: std::collections::HashSet<String> =
        rows.iter().map(|row| row.id.clone()).collect();
    let mut children: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    let mut roots = Vec::new();

    for (idx, row) in rows.iter().enumerate() {
        if let Some(parent) = row.parent_run_id.as_deref()
            && known_ids.contains(parent)
        {
            children.entry(parent.to_string()).or_default().push(idx);
            continue;
        }
        roots.push(idx);
    }

    fn push_tree(
        idx: usize,
        rows: &[SidebarAgentRow],
        children: &std::collections::HashMap<String, Vec<usize>>,
        seen: &mut std::collections::HashSet<usize>,
        order: &mut Vec<usize>,
    ) {
        if !seen.insert(idx) {
            return;
        }
        order.push(idx);
        if let Some(child_indices) = children.get(&rows[idx].id) {
            for child_idx in child_indices {
                push_tree(*child_idx, rows, children, seen, order);
            }
        }
    }

    let mut order = Vec::with_capacity(rows.len());
    let mut seen = std::collections::HashSet::new();
    for idx in roots {
        push_tree(idx, &rows, &children, &mut seen, &mut order);
    }
    for idx in 0..rows.len() {
        push_tree(idx, &rows, &children, &mut seen, &mut order);
    }

    // Materialize by move instead of cloning each row a second time (#3898):
    // `seen` guarantees every index lands in `order` exactly once, so each
    // slot is taken exactly once and no row is dropped.
    let mut slots: Vec<Option<SidebarAgentRow>> = rows.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|idx| slots[idx].take().expect("each row emitted exactly once"))
        .collect()
}

fn subagent_status_text(status: &SubAgentStatus) -> &'static str {
    match status {
        SubAgentStatus::Running => "running",
        SubAgentStatus::Completed => "done",
        SubAgentStatus::Interrupted(_) => "interrupted",
        SubAgentStatus::Failed(_) => "failed",
        SubAgentStatus::Cancelled => "canceled",
        SubAgentStatus::BudgetExhausted => "budget",
    }
}

fn sidebar_worker_status_text(status: AgentWorkerStatus) -> &'static str {
    match status {
        AgentWorkerStatus::Queued => "queued",
        AgentWorkerStatus::Starting => "starting",
        AgentWorkerStatus::Running => "running",
        AgentWorkerStatus::WaitingForUser => "waiting",
        AgentWorkerStatus::ModelWait => "model wait",
        AgentWorkerStatus::RunningTool => "tool",
        AgentWorkerStatus::Completed => "done",
        AgentWorkerStatus::Failed => "failed",
        AgentWorkerStatus::Cancelled => "canceled",
        AgentWorkerStatus::Interrupted => "interrupted",
    }
}

fn sidebar_current_activity_status_text(
    status: AgentCurrentActivityStatus,
    locale: codewhale_localization::Locale,
) -> std::borrow::Cow<'static, str> {
    // A parked husk gets its own word, translated (#5906) — "waiting" here
    // would be the same lie the work surface used to tell.
    if status == AgentCurrentActivityStatus::Parked {
        return codewhale_localization::tr(
            locale,
            codewhale_localization::MessageId::AgentStatusParked,
        );
    }
    std::borrow::Cow::Borrowed(match status {
        AgentCurrentActivityStatus::Queued => "queued",
        AgentCurrentActivityStatus::Starting => "starting",
        AgentCurrentActivityStatus::Running => "running",
        AgentCurrentActivityStatus::ModelWait => "model wait",
        AgentCurrentActivityStatus::RunningTool => "tool",
        AgentCurrentActivityStatus::Waiting => "waiting",
        AgentCurrentActivityStatus::Done => "done",
        AgentCurrentActivityStatus::Failed => "failed",
        AgentCurrentActivityStatus::Canceled => "canceled",
        AgentCurrentActivityStatus::Interrupted => "interrupted",
        AgentCurrentActivityStatus::Parked => unreachable!("handled above"),
    })
}

fn sidebar_agent_status_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "done" | "canceled" | "failed" | "interrupted" | "budget"
    )
}

#[cfg(test)]
mod tests {
    use super::sidebar_agent_rows;
    use crate::tui::app::{
        AgentCurrentActivity, AgentCurrentActivityStatus, AgentProgressMeta, App,
        SidebarHoverSection, SidebarHoverState, TuiOptions,
    };
    use codewhale_localization::Locale;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn create_test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        // Legacy strip geometry (see ui.rs); Bottom default has its own tests.
        app.work_surface.placement = crate::tui::work_surface::WorkSurfacePlacement::Top;
        app
    }

    // ---- Sidebar hover tooltip tests ----

    #[test]
    fn sidebar_hover_state_default_is_empty() {
        let state = SidebarHoverState::default();
        assert!(state.sections.is_empty());
    }

    #[test]
    fn sidebar_hover_section_stores_lines() {
        use ratatui::layout::Rect;
        let section = SidebarHoverSection {
            content_area: Rect::new(1, 1, 38, 8),
            lines: vec!["line 1".to_string(), "line 2".to_string()],
            rows: vec![],
        };
        assert_eq!(section.lines.len(), 2);
        assert_eq!(section.lines[0], "line 1");
        assert!(section.content_area.x > 0);
    }

    #[test]
    fn hover_line_matching_respects_content_area_offset() {
        use ratatui::layout::Rect;
        let section = SidebarHoverSection {
            content_area: Rect::new(62, 2, 36, 6),
            lines: vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
            rows: vec![],
        };

        // Mouse within content area, first line
        let line_idx = (2u16.saturating_sub(section.content_area.y)) as usize;
        assert_eq!(section.lines[line_idx], "first");

        // Mouse within content area, second line
        let line_idx = (3u16.saturating_sub(section.content_area.y)) as usize;
        assert_eq!(section.lines[line_idx], "second");

        // Mouse outside content area (above) — row < content_area.y
        assert!((1u16) < section.content_area.y);
    }

    // ── #3030: stable labels instead of raw internal ids ───────────────────

    #[test]
    fn ensure_agent_label_assigns_stable_sequential_labels() {
        let mut app = create_test_app();
        assert_eq!(app.ensure_agent_label("agent_aaa111"), "Agent 1");
        assert_eq!(app.ensure_agent_label("agent_bbb222"), "Agent 2");
        // Re-seeing a known agent keeps its original label.
        assert_eq!(app.ensure_agent_label("agent_aaa111"), "Agent 1");
        assert_eq!(app.agent_counter, 2);
        // Read-only lookup falls back to the raw id for unknown agents.
        assert_eq!(app.agent_display_label("agent_bbb222"), "Agent 2");
        assert_eq!(app.agent_display_label("agent_zzz999"), "agent_zzz999");
    }

    #[test]
    fn ensure_agent_label_prefers_identity_over_the_counter() {
        let mut app = create_test_app();
        let route = |profile: Option<&str>, role: &str| {
            Some(crate::tools::subagent::ChildRouteReceipt {
                requested_type: "custom".to_string(),
                requested_profile: profile.map(str::to_string),
                resolved_profile_id: None,
                profile_origin: None,
                canonical_role: role.to_string(),
                provider_id: "deepseek".to_string(),
                model_id: "deepseek-v4-pro".to_string(),
                route_source: "roster".to_string(),
                fallback_note: None,
                requested_reasoning: "inherit".to_string(),
                effective_reasoning: None,
                runtime_version: "test".to_string(),
                runtime_build_sha: "unknown".to_string(),
            })
        };

        let mut named = cached_agent("agent_named", None);
        named.name = "branch-triage".to_string();
        app.subagent_cache.push(named);

        let mut role = cached_agent("agent_role", None);
        role.assignment.role = Some("reviewer".to_string());
        app.subagent_cache.push(role);

        let mut profile = cached_agent("agent_profile", None);
        profile.assignment.role = None;
        profile.child_route = route(Some("release-lead"), "custom");
        app.subagent_cache.push(profile);

        let mut canonical = cached_agent("agent_canonical", None);
        canonical.assignment.role = None;
        canonical.child_route = route(None, "planner");
        app.subagent_cache.push(canonical);

        let mut typed = cached_agent("agent_typed", None);
        typed.assignment.role = None;
        typed.agent_type = codewhale_agent_contract::role::FleetRole::Builder;
        app.subagent_cache.push(typed);

        // The dispatch name leads, annotated with the role when the role is
        // not already part of the name.
        assert_eq!(
            app.ensure_agent_label("agent_named"),
            "branch-triage · general"
        );
        // Unnamed children go by their role (#6565: the engine's one name);
        // a suffix appears only when another agent already shows that name.
        assert_eq!(app.ensure_agent_label("agent_role"), "reviewer");
        assert_eq!(app.ensure_agent_label("agent_profile"), "release-lead");
        assert_eq!(app.ensure_agent_label("agent_canonical"), "planner");
        assert_eq!(app.ensure_agent_label("agent_typed"), "implement");

        // A progress-only agent first seen before its metadata arrives gets a
        // counter placeholder, then upgrades once the identity is observed.
        assert_eq!(app.ensure_agent_label("agent_late"), "Agent 1");
        let mut late = cached_agent("agent_late", None);
        late.assignment.role = Some("verifier".to_string());
        app.subagent_cache.push(late);
        assert_eq!(app.ensure_agent_label("agent_late"), "test");
    }

    #[test]
    fn ensure_agent_label_disambiguates_concurrent_same_role_children() {
        let mut app = create_test_app();

        let mut first = cached_agent("agent_builder_a", None);
        first.assignment.role = None;
        first.agent_type = codewhale_agent_contract::role::FleetRole::Builder;
        app.subagent_cache.push(first);

        let mut second = cached_agent("agent_builder_b", None);
        second.assignment.role = None;
        second.agent_type = codewhale_agent_contract::role::FleetRole::Builder;
        app.subagent_cache.push(second);

        assert_eq!(app.ensure_agent_label("agent_builder_a"), "implement");
        assert_eq!(app.ensure_agent_label("agent_builder_b"), "implement · 2");
        // Stability: re-seeing a known builder keeps its assigned label.
        assert_eq!(app.ensure_agent_label("agent_builder_a"), "implement");
        assert_eq!(app.ensure_agent_label("agent_builder_b"), "implement · 2");

        // A different role needs no suffix.
        let mut reviewer = cached_agent("agent_reviewer_a", None);
        reviewer.assignment.role = Some("reviewer".to_string());
        app.subagent_cache.push(reviewer);
        assert_eq!(app.ensure_agent_label("agent_reviewer_a"), "reviewer");
    }

    #[test]
    fn ensure_agent_label_named_child_skips_role_suffix_when_present() {
        let mut app = create_test_app();

        let mut named = cached_agent("agent_named", None);
        named.name = "release-lead".to_string();
        named.assignment.role = None;
        named.child_route = Some(crate::tools::subagent::ChildRouteReceipt {
            requested_type: "custom".to_string(),
            requested_profile: Some("release-lead".to_string()),
            resolved_profile_id: None,
            profile_origin: None,
            canonical_role: "release-lead".to_string(),
            provider_id: "deepseek".to_string(),
            model_id: "deepseek-v4-pro".to_string(),
            route_source: "roster".to_string(),
            fallback_note: None,
            requested_reasoning: "inherit".to_string(),
            effective_reasoning: None,
            runtime_version: "test".to_string(),
            runtime_build_sha: "unknown".to_string(),
        });
        app.subagent_cache.push(named);

        // The role is already part of the name, so no duplicate suffix.
        assert_eq!(app.ensure_agent_label("agent_named"), "release-lead");
    }

    fn cached_agent(
        agent_id: &str,
        nickname: Option<&str>,
    ) -> crate::tools::subagent::SubAgentResult {
        crate::tools::subagent::SubAgentResult {
            usage: None,
            // An unnamed dispatch: the manager seeds `name` with the agent id
            // and only replaces it when the caller supplied one.
            name: agent_id.to_string(),
            agent_id: agent_id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: codewhale_agent_contract::role::FleetRole::Worker,
            assignment: crate::tools::subagent::SubAgentAssignment {
                native_preset: None,
                objective: "task".to_string(),
                role: Some("worker".to_string()),
            },
            model: String::new(),
            nickname: nickname.map(str::to_string),
            status: crate::tools::subagent::SubAgentStatus::Running,
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 0,
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
        }
    }

    // === #5479: a fan-out parent shows how much of its fan-out is done ===

    #[test]
    fn a_fanout_parent_row_reports_how_many_children_have_settled() {
        let mut app = create_test_app();
        let parent = cached_agent("workflow_parent", None);
        for index in 0..6 {
            let mut child = cached_agent(&format!("child_{index}"), None);
            child.parent_run_id = Some("workflow_parent".to_string());
            child.spawn_depth = 1;
            if index < 5 {
                child.status = crate::tools::subagent::SubAgentStatus::Completed;
                child.worker_status = Some(crate::tools::subagent::AgentWorkerStatus::Completed);
            }
            app.subagent_cache.push(child);
        }
        app.subagent_cache.push(parent);

        let rows = sidebar_agent_rows(&app);
        let parent_row = rows
            .iter()
            .find(|row| row.id == "workflow_parent")
            .expect("parent row");
        assert_eq!(
            parent_row.children_settled,
            Some((5, 6)),
            "the parent's own status says nothing about its fan-out"
        );
        for row in rows.iter().filter(|row| row.id != "workflow_parent") {
            assert_eq!(
                row.children_settled, None,
                "a leaf must not claim a fan-out it does not have"
            );
        }
    }

    #[test]
    fn a_parent_whose_children_aged_out_reports_no_progress_rather_than_zero() {
        // A denominator that counted rows no longer in the ledger would report
        // progress as worse than it is.
        let mut app = create_test_app();
        app.subagent_cache.push(cached_agent("lonely_parent", None));
        let rows = sidebar_agent_rows(&app);
        assert_eq!(rows[0].children_settled, None);
    }

    #[test]
    fn sidebar_agent_rows_use_worker_status_from_cached_agents() {
        let mut app = create_test_app();
        let mut agent = cached_agent("agent_model_wait", Some("Blue"));
        agent.worker_status = Some(crate::tools::subagent::AgentWorkerStatus::ModelWait);
        app.subagent_cache.push(agent);

        let rows = sidebar_agent_rows(&app);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "model wait");
    }

    #[test]
    fn sidebar_agent_rows_project_typed_lifecycle_fixtures() {
        let mut app = create_test_app();
        let fixtures = [
            (
                "agent_running",
                "Running",
                crate::tools::subagent::SubAgentStatus::Running,
                crate::tools::subagent::AgentWorkerStatus::RunningTool,
                AgentCurrentActivityStatus::RunningTool,
                "tool",
            ),
            (
                "agent_waiting",
                "Waiting",
                crate::tools::subagent::SubAgentStatus::Interrupted("approval".to_string()),
                crate::tools::subagent::AgentWorkerStatus::WaitingForUser,
                AgentCurrentActivityStatus::Waiting,
                "waiting",
            ),
            (
                "agent_failed",
                "Failed",
                crate::tools::subagent::SubAgentStatus::Failed("verification".to_string()),
                crate::tools::subagent::AgentWorkerStatus::Failed,
                AgentCurrentActivityStatus::Failed,
                "failed",
            ),
            (
                "agent_done",
                "Done",
                crate::tools::subagent::SubAgentStatus::Completed,
                crate::tools::subagent::AgentWorkerStatus::Completed,
                AgentCurrentActivityStatus::Done,
                "done",
            ),
        ];
        for (id, nickname, status, worker_status, activity_status, _) in &fixtures {
            let mut agent = cached_agent(id, Some(nickname));
            agent.status = status.clone();
            agent.worker_status = Some(*worker_status);
            app.subagent_cache.push(agent);
            app.agent_progress_meta.insert(
                (*id).to_string(),
                AgentProgressMeta {
                    current_activity: Some(AgentCurrentActivity::bounded(
                        *activity_status,
                        (*id == "agent_waiting").then_some("approval required".to_string()),
                        (*id == "agent_running").then_some("read_file".to_string()),
                        Some(2),
                    )),
                    ..AgentProgressMeta::default()
                },
            );
        }

        let rows = sidebar_agent_rows(&app);
        for (id, _, _, _, _, expected_status) in fixtures {
            let row = rows
                .iter()
                .find(|row| row.id == id)
                .expect("typed lifecycle row");
            assert_eq!(row.status, expected_status);
        }
    }

    #[test]
    fn sidebar_progress_only_rows_never_infer_status_from_display_text() {
        let mut app = create_test_app();
        app.ensure_agent_label("agent_queued");
        app.agent_progress.insert(
            "agent_queued".to_string(),
            "queued waiting failed completed".to_string(),
        );

        let rows = sidebar_agent_rows(&app);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Agent 1");
        assert_eq!(rows[0].status, "running");

        app.agent_progress_meta.insert(
            "agent_queued".to_string(),
            AgentProgressMeta {
                current_activity: Some(AgentCurrentActivity::bounded(
                    AgentCurrentActivityStatus::Queued,
                    Some("waiting for launch permit".to_string()),
                    None,
                    None,
                )),
                ..AgentProgressMeta::default()
            },
        );
        crate::tui::ui::record_agent_spawned_route(&mut app, "agent_queued", "deepseek-v4-pro");
        let rows = sidebar_agent_rows(&app);
        assert_eq!(rows[0].status, "queued");
        assert_eq!(rows[0].model.as_deref(), Some("deepseek-v4-pro"));
    }

    #[test]
    fn sidebar_agent_rows_preserve_explicit_names_and_derive_whales_from_locale() {
        let mut app = create_test_app();
        let agent_id = "agent_cafe0123";
        app.ensure_agent_label(agent_id);
        app.subagent_cache
            .push(cached_agent(agent_id, Some("doc-fixer")));

        let rows = super::sidebar_agent_rows(&app);
        assert_eq!(
            rows[0].name, "doc-fixer",
            "an explicit custom nickname remains user-owned"
        );

        // Without an explicit nickname, display is derived from the neutral id
        // in the active UI locale rather than from the old Agent-N label.
        app.subagent_cache[0].nickname = None;
        let rows = super::sidebar_agent_rows(&app);
        assert_eq!(
            rows[0].name,
            crate::tools::subagent::whale_name_for_id_in_locale(agent_id, "en")
        );
    }

    #[test]
    fn sidebar_agent_rows_lead_with_the_dispatch_name() {
        // #5287: operators dispatch by name and think by name, so the session
        // name outranks both the generated whale and the Agent-N label.
        let mut app = create_test_app();
        let agent_id = "agent_cafe0123";
        app.ensure_agent_label(agent_id);
        let whale = crate::tools::subagent::whale_name_for_id_in_locale(agent_id, "en");
        let mut agent = cached_agent(agent_id, Some(&whale));
        agent.name = "branch-triage".to_string();
        app.subagent_cache.push(agent);

        let rows = super::sidebar_agent_rows(&app);
        assert_eq!(rows[0].name, "branch-triage");
    }

    #[test]
    fn sidebar_agent_rows_prefer_resolved_profile_over_generated_whale() {
        let mut app = create_test_app();
        let agent_id = "agent_cafe0123";
        app.ensure_agent_label(agent_id);
        let whale = crate::tools::subagent::whale_name_for_id_in_locale(agent_id, "en");
        let mut agent = cached_agent(agent_id, Some(&whale));
        agent.child_route = Some(crate::tools::subagent::ChildRouteReceipt {
            requested_type: "custom".to_string(),
            requested_profile: Some("DeepSeek V4 Flash".to_string()),
            resolved_profile_id: Some("flash-scout".to_string()),
            profile_origin: Some("fleet:release".to_string()),
            canonical_role: "scout".to_string(),
            provider_id: "deepseek".to_string(),
            model_id: "deepseek-v4-flash-vision-exp".to_string(),
            route_source: "fleet".to_string(),
            fallback_note: None,
            requested_reasoning: "inherit".to_string(),
            effective_reasoning: None,
            runtime_version: "test".to_string(),
            runtime_build_sha: "unknown".to_string(),
        });
        app.subagent_cache.push(agent);

        let rows = super::sidebar_agent_rows(&app);
        assert_eq!(rows[0].name, "flash-scout");
    }

    #[test]
    fn english_sidebar_relocalizes_mixed_persisted_whale_names() {
        let mut app = create_test_app();
        app.ui_locale = Locale::En;
        for (agent_id, legacy_locale) in [
            ("agent_locale_a", "zh-Hans"),
            ("agent_locale_b", "ja"),
            ("agent_locale_c", "vi"),
        ] {
            let legacy_name =
                crate::tools::subagent::whale_name_for_id_in_locale(agent_id, legacy_locale);
            app.subagent_cache
                .push(cached_agent(agent_id, Some(&legacy_name)));
        }

        let rows = super::sidebar_agent_rows(&app);
        assert_eq!(rows.len(), 3);
        for row in rows {
            assert!(
                row.name.is_ascii(),
                "English Fleet display leaked a prior-locale whale: {}",
                row.name
            );
            assert_eq!(
                row.name,
                crate::tools::subagent::whale_name_for_id_in_locale(&row.id, "en")
            );
        }
    }

    // === #5906: parked husks vs. children that actually asked ============

    /// A child parked at the parent's turn end is handed a `needs_input` note
    /// phrased as a question ("Resume this parked child with ..."), which is
    /// why every surface used to label it `waiting`. Build one exactly the way
    /// the runtime does and assert the row names the state instead.
    fn parked_agent(agent_id: &str) -> crate::tools::subagent::SubAgentResult {
        let mut agent = cached_agent(agent_id, None);
        agent.status = crate::tools::subagent::SubAgentStatus::Interrupted(
            "Parent turn ended before this turn-owned child settled.".to_string(),
        );
        agent.worker_status = Some(crate::tools::subagent::AgentWorkerStatus::WaitingForUser);
        agent.needs_input = Some(crate::tools::subagent::SubAgentNeedsInput {
            question: format!(
                "Resume this parked child with agent(action=\"start\", resume_from=\"{agent_id}\")."
            ),
        });
        agent.checkpoint = Some(crate::tools::subagent::SubAgentCheckpoint {
            checkpoint_id: format!("{agent_id}:step:2"),
            agent_id: agent_id.to_string(),
            continuation_handle: format!("agent:{agent_id}:checkpoint"),
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

    fn asking_agent(agent_id: &str) -> crate::tools::subagent::SubAgentResult {
        let mut agent = cached_agent(agent_id, None);
        agent.worker_status = Some(crate::tools::subagent::AgentWorkerStatus::WaitingForUser);
        agent.needs_input = Some(crate::tools::subagent::SubAgentNeedsInput {
            question: "Which path should I use?".to_string(),
        });
        agent
    }

    #[test]
    fn a_parked_row_says_parked_and_a_real_question_still_says_waiting() {
        let mut app = create_test_app();
        app.subagent_cache.push(parked_agent("agent_parked"));
        app.subagent_cache.push(asking_agent("agent_asking"));
        crate::tui::subagent_routing::reconcile_subagent_activity_state(&mut app);

        let rows = sidebar_agent_rows(&app);
        let parked = rows
            .iter()
            .find(|row| row.id == "agent_parked")
            .expect("parked row");
        let asking = rows
            .iter()
            .find(|row| row.id == "agent_asking")
            .expect("asking row");

        assert_eq!(parked.status, "parked");
        assert_ne!(
            parked.status, "waiting",
            "a parked husk must not wear the label a child a user can answer wears"
        );
        assert_eq!(asking.status, "waiting");
    }

    /// The status word is registry copy, not a hardcoded English literal.
    #[test]
    fn the_parked_status_word_follows_the_ui_locale() {
        let mut app = create_test_app();
        app.ui_locale = Locale::De;
        app.subagent_cache.push(parked_agent("agent_parked_de"));
        crate::tui::subagent_routing::reconcile_subagent_activity_state(&mut app);

        let rows = sidebar_agent_rows(&app);
        assert_eq!(
            rows[0].status,
            codewhale_localization::tr(
                Locale::De,
                codewhale_localization::MessageId::AgentStatusParked
            )
        );
    }

    // --- Unicode / CJK / terminal-width QA (issue #3488) -------------------
    // The sub-agent overlay renders CJK display names next to ASCII ids,
    // numeric columns (step count, elapsed), status verbs, and branch lines.
    // These guard that a CJK name never shifts the status columns, corrupts the
    // panel border, or hides the running/completed state (#3488 dogfood case:
    // a worker named 抹香鲸).
}
