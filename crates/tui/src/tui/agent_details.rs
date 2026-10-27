//! Safe, bounded Agent Details projection (#2889).
//!
//! Since the v0.9.7 "one agent, one destination" inversion, activating an
//! agent row opens the agent's transcript surface directly
//! (`crate::tui::agent_transcript`); this bounded projection is the secondary
//! action, reached from the transcript via the same Alt+V chord that opens
//! the transcript from here.

use std::path::{Component, Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{buffer::Buffer, layout::Rect};

use crate::tools::subagent::{SubAgentResult, SubAgentStatus, localized_whale_display_names};
use crate::tui::app::{
    AgentCurrentActivityStatus, AgentProgressMeta, App, bound_agent_activity_text,
};
use crate::tui::pager::PagerView;
use crate::tui::views::{ModalKind, ModalView, ViewAction, ViewEvent};

pub(crate) struct AgentDetailsProjection {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) transcript_available: bool,
}

/// Pager-backed details view with a distinct close receipt and an explicit
/// exact-transcript action.
pub(crate) struct AgentDetailsView {
    pager: PagerView,
    agent_id: String,
    transcript_available: bool,
}

impl AgentDetailsView {
    fn new(projection: AgentDetailsProjection, agent_id: impl Into<String>, width: u16) -> Self {
        let agent_id = agent_id.into();
        let pager =
            PagerView::from_text(projection.title, &projection.body, width.saturating_sub(2))
                .with_copy_text(projection.body);
        Self {
            pager,
            agent_id,
            transcript_available: projection.transcript_available,
        }
    }

    #[cfg(test)]
    fn body_text(&self) -> String {
        self.pager.body_text()
    }

    #[cfg(test)]
    fn title(&self) -> &str {
        self.pager.title()
    }
}

impl ModalView for AgentDetailsView {
    fn kind(&self) -> ModalKind {
        ModalKind::Pager
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if matches!(key.code, KeyCode::Char('v' | 'V'))
            && key.modifiers.contains(KeyModifiers::ALT)
            && self.transcript_available
        {
            return ViewAction::Emit(ViewEvent::OpenAgentTranscript {
                agent_id: self.agent_id.clone(),
            });
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Left)
            || (key.code == KeyCode::Char('q') && key.modifiers.is_empty())
        {
            return ViewAction::EmitAndClose(ViewEvent::AgentDetailsClosed {
                agent_id: self.agent_id.clone(),
            });
        }
        self.pager.handle_key(key)
    }

    fn handle_paste(&mut self, text: &str) -> bool {
        self.pager.handle_paste(text)
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        self.pager.handle_mouse(mouse)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.pager.render(area, buf);
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub(crate) fn open_agent_details(app: &mut App, agent_id: &str) -> bool {
    let Some(projection) = project_agent_details(app, agent_id) else {
        return false;
    };
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    app.view_stack
        .push(AgentDetailsView::new(projection, agent_id, width));
    true
}

pub(crate) fn safe_agent_display_name(app: &App, agent_id: &str) -> String {
    let generated = localized_whale_display_names(
        app.subagent_cache
            .iter()
            .map(|agent| (agent.agent_id.as_str(), agent.nickname.as_deref())),
        app.ui_locale.tag(),
    );
    app.agent_given_name(agent_id)
        .or_else(|| generated.get(agent_id).cloned())
        .or_else(|| app.agent_label_map.get(agent_id).cloned())
        .and_then(|name| safe_child_value(app, &name))
        .unwrap_or_else(|| "Agent".to_string())
}

pub(crate) fn project_agent_details(app: &App, agent_id: &str) -> Option<AgentDetailsProjection> {
    // The Agents sidebar is the primary worker surface. Consume its exact row
    // projection so status precedence, model, elapsed time, and steps cannot
    // diverge between the row and the popup opened from it.
    let surface_row = crate::tui::sidebar::sidebar_agent_rows(app)
        .into_iter()
        .find(|row| row.id == agent_id)?;
    let agent = app
        .subagent_cache
        .iter()
        .find(|agent| agent.agent_id == agent_id);
    let meta = app.agent_progress_meta.get(agent_id);

    let display_name = safe_child_value(app, &surface_row.name).unwrap_or_else(|| "Agent".into());
    let mut lines = Vec::new();

    if let Some(agent) = agent {
        push_safe_line(app, &mut lines, "Assignment", &agent.assignment.objective);

        // The child's own route receipt is the authoritative resolved truth
        // for a fleet-dispatched child; `assignment.role` is only the caller's
        // advisory token and may be absent on a `type`-only dispatch.
        let route = agent.child_route.as_ref();
        let role = route
            .map(|route| route.canonical_role.trim())
            .filter(|role| !role.is_empty())
            .or(agent.assignment.role.as_deref())
            .map(str::trim)
            .filter(|role| !role.is_empty());
        if let Some(role) = role {
            push_safe_line(app, &mut lines, "Role", role);
        }
        // Profile is the member that actually resolved. A caller alias/model
        // label is useful evidence but must not be presented as who ran.
        let profile = route
            .and_then(|route| route.resolved_profile_id.as_deref())
            .map(str::trim)
            .filter(|profile| !profile.is_empty())
            .or_else(|| {
                route
                    .and_then(|route| route.requested_profile.as_deref())
                    .map(str::trim)
                    .filter(|profile| !profile.is_empty())
            })
            .unwrap_or_else(|| agent.agent_type.as_str());
        push_safe_line(app, &mut lines, "Profile", profile);
        if let Some(requested) = route
            .and_then(|route| route.requested_profile.as_deref())
            .map(str::trim)
            .filter(|requested| !requested.is_empty() && !requested.eq_ignore_ascii_case(profile))
        {
            push_safe_line(app, &mut lines, "Requested as", requested);
        }
        push_safe_line(app, &mut lines, "Type", agent.agent_type.as_str());
        lines.push(format!("Parent: {}", safe_parent_name(app, agent)));
    } else {
        lines.push(format!("Parent: {}", safe_parent_from_meta(app, meta)));
    }

    let mut state = vec![surface_row.status.clone()];
    if let Some(duration_ms) = surface_row.duration_ms {
        state.push(format!(
            "elapsed {}",
            codewhale_command_contract::elapsed::format_elapsed_ms(duration_ms)
        ));
    }
    state.push(format!(
        "{} {}",
        surface_row.steps_taken,
        if surface_row.steps_taken == 1 {
            "step"
        } else {
            "steps"
        }
    ));
    lines.push(format!("State: {}", state.join(" · ")));

    if let Some(meta) = meta
        && let Some(provider) = meta.resolved_provider.as_deref()
    {
        push_safe_line(app, &mut lines, "Provider", provider);
    } else if let Some(route) = agent.and_then(|agent| agent.child_route.as_ref())
        && !route.provider_id.trim().is_empty()
    {
        push_safe_line(app, &mut lines, "Provider", &route.provider_id);
    }
    // Model truth: the resolved route model wins, then the Work-row receipt
    // (which itself carries `agent.model` or usage-envelope route evidence),
    // then the child's spawn model.
    let model = agent
        .and_then(|agent| agent.child_route.as_ref())
        .map(|route| route.model_id.clone())
        .filter(|model| !model.trim().is_empty())
        .or_else(|| surface_row.model.clone())
        .or_else(|| {
            agent
                .map(|agent| agent.model.clone())
                .filter(|model| !model.trim().is_empty())
        })
        .or_else(|| meta.and_then(|meta| meta.resolved_model.clone()));
    if let Some(model) = model {
        push_safe_line(app, &mut lines, "Model", &model);
    }

    if let Some(agent) = agent {
        if let Some(workspace) = agent.workspace.as_deref()
            && let Some(workspace) = safe_workspace(app, workspace)
        {
            lines.push(format!("Workspace: {workspace}"));
        }
        if let Some(branch) = agent.git_branch.as_deref() {
            push_safe_line(app, &mut lines, "Branch", branch);
        }
    }

    if let Some(activity) = meta.and_then(|meta| meta.current_activity.as_ref())
        && !matches!(
            activity.status,
            AgentCurrentActivityStatus::Done
                | AgentCurrentActivityStatus::Failed
                | AgentCurrentActivityStatus::Canceled
                | AgentCurrentActivityStatus::Interrupted
                | AgentCurrentActivityStatus::Waiting
        )
    {
        let mut current = vec![activity_status_label(activity.status, app.ui_locale).into_owned()];
        if let Some(tool) = activity
            .current_tool
            .as_deref()
            .and_then(|tool| safe_child_value(app, tool))
        {
            current.push(tool);
        }
        if let Some(step) = activity.step {
            current.push(format!("step {step}"));
        }
        if let Some(detail) = activity
            .detail
            .as_deref()
            .and_then(|detail| safe_child_value(app, detail))
            && !detail.starts_with("started ")
            && !current.iter().any(|part| part == &detail)
        {
            current.push(detail);
        }
        lines.push(format!("Current: {}", current.join(" · ")));
    }

    if let Some(meta) = meta {
        for action in meta.recent_actions.iter().rev().take(3).rev() {
            if let Some(tool) = safe_child_value(app, &action.tool) {
                lines.push(format!(
                    "Recent: {} {tool} · step {}",
                    if action.ok { "✓" } else { "!" },
                    action.step
                ));
            }
        }
    }

    if let Some(question) = pending_question(agent, meta)
        && let Some(question) = safe_child_value(app, question)
    {
        lines.push(format!("Pending question: {question}"));
    }
    if let Some(blocker) = blocker(agent, meta)
        && let Some(blocker) = safe_child_value(app, blocker)
    {
        lines.push(format!("Blocker: {blocker}"));
    }
    if let Some(summary) = terminal_summary(agent, meta)
        && let Some(summary) = safe_child_value(app, summary)
    {
        lines.push(format!("Summary: {summary}"));
    }

    let transcript_available =
        crate::tui::mouse_ui::agent_transcript_evidence_available(app, agent_id);
    if transcript_available {
        // Platform glyph via display_chord (⌥V on macOS, Alt+V elsewhere) —
        // never the dual "Alt/⌥V" spelling, and cap:verb not a sentence.
        let chord = crate::tui::shell_key_routing::tool_details_chord();
        lines.push(format!("Exact evidence: available · {chord}:transcript"));
    } else {
        lines.push("Exact evidence: unavailable".to_string());
    }

    Some(AgentDetailsProjection {
        title: format!("Agent Details — {display_name}"),
        body: lines.join("\n"),
        transcript_available,
    })
}

fn push_safe_line(app: &App, lines: &mut Vec<String>, label: &str, value: &str) {
    if let Some(value) = safe_child_value(app, value) {
        lines.push(format!("{label}: {value}"));
    }
}

fn safe_child_value(app: &App, value: &str) -> Option<String> {
    let bounded = bound_agent_activity_text(value);
    let scrubbed = scrub_raw_agent_ids(app, &bounded);
    let trimmed = scrubbed.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "none" | "(none)" | "n/a" | "unknown" | "not set" | "not available" | "-"
        )
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn scrub_raw_agent_ids(app: &App, value: &str) -> String {
    let mut scrubbed = value.to_string();
    let mut ids: Vec<&str> = app
        .subagent_cache
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .chain(
            app.subagent_cache
                .iter()
                .filter_map(|agent| agent.parent_run_id.as_deref()),
        )
        .chain(app.agent_progress_meta.keys().map(String::as_str))
        .chain(
            app.agent_progress_meta
                .values()
                .filter_map(|meta| meta.parent_run_id.as_deref()),
        )
        .chain(app.agent_progress.keys().map(String::as_str))
        .chain(app.agent_label_map.keys().map(String::as_str))
        .collect();
    ids.sort_unstable_by_key(|id| std::cmp::Reverse(id.len()));
    ids.dedup();
    for id in ids {
        if !id.is_empty() {
            scrubbed = scrubbed.replace(id, "agent");
        }
    }

    let mut output = String::with_capacity(scrubbed.len());
    let mut token = String::new();
    let flush = |token: &mut String, output: &mut String| {
        if token.starts_with("agent_")
            || token.starts_with("agent-")
            || token.starts_with("worker:agent_")
            || token.starts_with("worker:agent-")
        {
            output.push_str("agent");
        } else {
            output.push_str(token);
        }
        token.clear();
    };
    for ch in scrubbed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':') {
            token.push(ch);
        } else {
            flush(&mut token, &mut output);
            output.push(ch);
        }
    }
    flush(&mut token, &mut output);
    output
}

fn safe_parent_name(app: &App, agent: &SubAgentResult) -> String {
    match agent.parent_run_id.as_deref() {
        Some(parent_id)
            if app
                .subagent_cache
                .iter()
                .any(|candidate| candidate.agent_id == parent_id) =>
        {
            safe_agent_display_name(app, parent_id)
        }
        Some(_) if agent.spawn_depth > 1 => "parent agent".to_string(),
        _ => "primary session".to_string(),
    }
}

fn safe_parent_from_meta(app: &App, meta: Option<&AgentProgressMeta>) -> String {
    match meta.and_then(|meta| meta.parent_run_id.as_deref()) {
        Some(parent_id)
            if app
                .subagent_cache
                .iter()
                .any(|candidate| candidate.agent_id == parent_id) =>
        {
            safe_agent_display_name(app, parent_id)
        }
        Some(_) => "parent agent".to_string(),
        None => "primary session".to_string(),
    }
}

fn activity_status_label(
    status: AgentCurrentActivityStatus,
    locale: codewhale_localization::Locale,
) -> std::borrow::Cow<'static, str> {
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
        AgentCurrentActivityStatus::ModelWait => "waiting for model",
        AgentCurrentActivityStatus::RunningTool => "running tool",
        AgentCurrentActivityStatus::Waiting => "waiting for input",
        AgentCurrentActivityStatus::Done => "completed",
        AgentCurrentActivityStatus::Failed => "failed",
        AgentCurrentActivityStatus::Canceled => "canceled",
        AgentCurrentActivityStatus::Interrupted => "interrupted",
        AgentCurrentActivityStatus::Parked => unreachable!("handled above"),
    })
}

fn pending_question<'a>(
    agent: Option<&'a SubAgentResult>,
    meta: Option<&'a AgentProgressMeta>,
) -> Option<&'a str> {
    // A parked child carries a `needs_input` note that is phrased as a
    // question but asks the *operator* nothing — it is the resume recipe.
    // Reporting it as a pending question is the #5906 complaint verbatim, so
    // this surface reports the parked state and its recovery instead.
    if agent.is_some_and(crate::tui::subagent_routing::subagent_is_parked) {
        return None;
    }
    agent
        .and_then(|agent| agent.needs_input.as_ref())
        .map(|needs_input| needs_input.question.as_str())
        .or_else(|| {
            meta.and_then(|meta| meta.current_activity.as_ref())
                .filter(|activity| activity.status == AgentCurrentActivityStatus::Waiting)
                .and_then(|activity| activity.detail.as_deref())
        })
        .or_else(|| match agent.map(|agent| &agent.status) {
            Some(SubAgentStatus::Interrupted(reason)) => Some(reason.as_str()),
            _ => None,
        })
}

fn blocker<'a>(
    agent: Option<&'a SubAgentResult>,
    meta: Option<&'a AgentProgressMeta>,
) -> Option<&'a str> {
    match agent.map(|agent| &agent.status) {
        Some(SubAgentStatus::Failed(error) | SubAgentStatus::Interrupted(error)) => {
            Some(error.as_str())
        }
        Some(SubAgentStatus::BudgetExhausted) => Some("worker budget exhausted"),
        _ => meta
            .and_then(|meta| meta.current_activity.as_ref())
            .filter(|activity| activity.status == AgentCurrentActivityStatus::Failed)
            .and_then(|activity| activity.detail.as_deref()),
    }
}

fn terminal_summary<'a>(
    agent: Option<&'a SubAgentResult>,
    meta: Option<&'a AgentProgressMeta>,
) -> Option<&'a str> {
    let agent = agent?;
    if !matches!(
        agent.status,
        SubAgentStatus::Completed | SubAgentStatus::Cancelled
    ) {
        return None;
    }
    agent.result.as_deref().or_else(|| {
        meta.and_then(|meta| meta.current_activity.as_ref())
            .and_then(|activity| activity.detail.as_deref())
    })
}

fn safe_workspace(app: &App, workspace: &Path) -> Option<String> {
    if workspace.as_os_str().is_empty() {
        return None;
    }
    if workspace == app.workspace {
        return Some(".".to_string());
    }
    if let Ok(relative) = workspace.strip_prefix(&app.workspace) {
        let parts: Vec<String> = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => part.to_str().map(ToString::to_string),
                _ => None,
            })
            .collect();
        if !parts.is_empty() {
            return safe_child_value(app, &parts.join("/"));
        }
    }
    workspace
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| safe_child_value(app, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{buffer::Buffer, layout::Rect};
    use serde_json::json;
    use tempfile::tempdir;

    use crate::tools::subagent::{
        AgentWorkerStatus, ChildRouteReceipt, SubAgentAssignment, SubAgentNeedsInput,
    };
    use crate::tui::app::{
        AgentCurrentActivity, AgentRecentAction, MAX_AGENT_RECENT_ACTIONS, TuiOptions,
    };
    use codewhale_agent_contract::role::FleetRole;
    use codewhale_runtime::config::Config;

    fn test_app(workspace: PathBuf) -> App {
        App::new(
            TuiOptions {
                model: "test-model".to_string(),
                use_mouse_capture: true,
                max_subagents: 4,
                ..crate::tui::test_support::test_tui_options(workspace)
            },
            &Config::default(),
        )
    }

    fn agent(agent_id: &str, status: SubAgentStatus) -> SubAgentResult {
        SubAgentResult {
            usage: None,
            name: agent_id.to_string(),
            agent_id: agent_id.to_string(),
            context_mode: "isolated".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: FleetRole::Builder,
            assignment: SubAgentAssignment {
                native_preset: None,
                objective: "Implement the bounded details route".to_string(),
                role: Some("worker".to_string()),
            },
            model: "deepseek-v4-pro".to_string(),
            nickname: Some(crate::tools::subagent::whale_name_for_id_in_locale(
                agent_id, "en",
            )),
            status,
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 1,
            child_route: None,
            result: None,
            steps_taken: 2,
            checkpoint: None,
            needs_input: None,
            duration_ms: 2_500,
            started_at: None,
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        }
    }

    fn body_for(
        status: SubAgentStatus,
        worker_status: AgentWorkerStatus,
        detail: Option<&str>,
    ) -> String {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let agent_id = "agent_matrix_subject";
        let mut child = agent(agent_id, status);
        child.worker_status = Some(worker_status);
        match worker_status {
            AgentWorkerStatus::WaitingForUser => {
                child.needs_input = Some(SubAgentNeedsInput {
                    question: detail.unwrap_or("Which path should I use?").to_string(),
                });
            }
            AgentWorkerStatus::Completed => child.result = detail.map(str::to_string),
            _ => {}
        }
        app.subagent_cache.push(child);
        app.agent_progress_meta.insert(
            agent_id.to_string(),
            AgentProgressMeta {
                current_activity: Some(AgentCurrentActivity::bounded(
                    worker_status.into(),
                    detail.map(str::to_string),
                    (worker_status == AgentWorkerStatus::RunningTool)
                        .then(|| "read_file".to_string()),
                    Some(2),
                )),
                ..AgentProgressMeta::default()
            },
        );
        project_agent_details(&app, agent_id)
            .expect("projection")
            .body
    }

    #[test]
    fn provider_free_status_matrix_is_typed_and_bounded() {
        let running = body_for(
            SubAgentStatus::Running,
            AgentWorkerStatus::RunningTool,
            None,
        );
        assert!(running.contains("State: tool · elapsed 2s · 2 steps"));
        assert!(running.contains("Current: running tool · read_file · step 2"));
        assert!(!running.contains("Provider:"));

        let waiting = body_for(
            SubAgentStatus::Running,
            AgentWorkerStatus::WaitingForUser,
            Some("Which path should I use?"),
        );
        assert!(waiting.contains("State: waiting"));
        assert!(waiting.contains("Pending question: Which path should I use?"));

        let failed = body_for(
            SubAgentStatus::Failed("verification failed".to_string()),
            AgentWorkerStatus::Failed,
            Some("verification failed"),
        );
        assert!(failed.contains("State: failed"));
        assert!(failed.contains("Blocker: verification failed"));

        let completed = body_for(
            SubAgentStatus::Completed,
            AgentWorkerStatus::Completed,
            Some("all checks passed"),
        );
        assert!(completed.contains("State: done"));
        assert!(completed.contains("Summary: all checks passed"));
    }

    #[test]
    fn details_projection_matches_primary_agents_row() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let agent_id = "agent_row_agreement";
        let mut child = agent(
            agent_id,
            SubAgentStatus::Failed("stale failure".to_string()),
        );
        child.model = "kimi-k3".to_string();
        child.steps_taken = 7;
        child.duration_ms = 61_000;
        app.subagent_cache.push(child);
        app.agent_progress_meta.insert(
            agent_id.to_string(),
            AgentProgressMeta {
                current_activity: Some(AgentCurrentActivity::bounded(
                    AgentCurrentActivityStatus::RunningTool,
                    Some("checking tests".to_string()),
                    Some("cargo test".to_string()),
                    Some(7),
                )),
                resolved_model: Some("stale-meta-model".to_string()),
                ..AgentProgressMeta::default()
            },
        );

        let row = crate::tui::sidebar::sidebar_agent_rows(&app)
            .into_iter()
            .find(|row| row.id == agent_id)
            .expect("primary agents row");
        let details = project_agent_details(&app, agent_id).expect("details projection");

        assert_eq!(row.status, "tool");
        assert_eq!(row.steps_taken, 7);
        assert_eq!(row.model.as_deref(), Some("kimi-k3"));
        assert_eq!(row.duration_ms, Some(61_000));
        assert!(details.body.contains(&format!(
            "State: {} · elapsed {} · {} steps",
            row.status,
            codewhale_command_contract::elapsed::format_elapsed_ms(
                row.duration_ms.expect("duration")
            ),
            row.steps_taken
        )));
        assert!(details.body.contains("Model: kimi-k3"));
        assert!(!details.body.contains("stale-meta-model"));
    }

    #[test]
    fn details_projection_shows_resolved_route_truth_for_fleet_child() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let agent_id = "agent_fleet_child";
        let mut child = agent(agent_id, SubAgentStatus::Running);
        // A `type`-only fleet dispatch leaves the advisory role empty and the
        // spawn model is a requested placeholder; the child's own route receipt
        // is the authoritative resolved truth.
        child.assignment.role = None;
        child.model = "stale-requested-model".to_string();
        child.child_route = Some(ChildRouteReceipt {
            requested_type: "custom".to_string(),
            requested_profile: Some("release-lead".to_string()),
            resolved_profile_id: Some("roster-release-lead".to_string()),
            profile_origin: Some("roster".to_string()),
            canonical_role: "release-lead".to_string(),
            provider_id: "deepseek".to_string(),
            model_id: "deepseek-v4-pro".to_string(),
            route_source: "roster".to_string(),
            fallback_note: None,
            requested_reasoning: "inherit".to_string(),
            effective_reasoning: Some("high".to_string()),
            runtime_version: "test".to_string(),
            runtime_build_sha: "unknown".to_string(),
        });
        app.subagent_cache.push(child);

        let body = project_agent_details(&app, agent_id)
            .expect("projection")
            .body;
        assert!(body.contains("Role: release-lead"), "{body}");
        assert!(body.contains("Profile: roster-release-lead"), "{body}");
        assert!(body.contains("Requested as: release-lead"), "{body}");
        assert!(body.contains("Type: implement"), "{body}");
        assert!(body.contains("Model: deepseek-v4-pro"), "{body}");
        assert!(body.contains("Provider: deepseek"), "{body}");
        assert!(!body.contains("stale-requested-model"), "{body}");
    }

    #[test]
    fn display_name_prefers_the_dispatch_name_over_the_whale() {
        // #5287: the lane was dispatched as `branch-triage`; that is the
        // identity the operator glances for. An unnamed sibling keeps its
        // generated whale rather than showing a bare id.
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let mut named = agent("agent_named_lane", SubAgentStatus::Running);
        named.name = "branch-triage".to_string();
        app.subagent_cache.push(named);
        let mut unnamed = agent("agent_plain_lane", SubAgentStatus::Running);
        unnamed.nickname = None;
        app.subagent_cache.push(unnamed);

        assert_eq!(
            safe_agent_display_name(&app, "agent_named_lane"),
            "branch-triage"
        );
        assert_eq!(
            safe_agent_display_name(&app, "agent_plain_lane"),
            crate::tools::subagent::whale_name_for_id_in_locale(
                "agent_plain_lane",
                app.ui_locale.tag()
            )
        );
    }

    #[test]
    fn projection_redacts_child_strings_and_never_exposes_raw_ids_or_none() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let agent_id = "agent_secret_child";
        let parent_id = "agent_raw_parent";
        let mut parent = agent(parent_id, SubAgentStatus::Running);
        parent.nickname = Some("Parent Whale".to_string());
        let mut child = agent(agent_id, SubAgentStatus::Running);
        child.parent_run_id = Some(parent_id.to_string());
        child.spawn_depth = 2;
        child.assignment.objective = format!(
            "\u{1b}[31minspect {agent_id}\u{1b}[0m with api_key=sk-agent-details-secret-1234567890"
        );
        child.git_branch = Some(format!("work/{parent_id}"));
        child.model.clear();
        child.nickname = Some(format!("\u{1b}[35m{agent_id}\u{1b}[0m"));
        app.subagent_cache.extend([parent, child]);

        let projection = project_agent_details(&app, agent_id).expect("projection");
        let all = format!("{}\n{}", projection.title, projection.body);
        assert!(!all.contains(agent_id), "{all}");
        assert!(!all.contains(parent_id), "{all}");
        assert!(!all.contains("sk-agent-details-secret"), "{all}");
        assert!(!all.contains('\u{1b}'), "{all:?}");
        assert!(!all.contains("None"), "{all}");
        assert!(all.contains("[redacted]"), "{all}");
    }

    #[test]
    fn external_workspace_is_basename_safe_and_branch_is_bounded() {
        let mut app = test_app(PathBuf::from("/repo/main"));
        let agent_id = "agent_external_workspace";
        let mut child = agent(agent_id, SubAgentStatus::Running);
        child.workspace = Some(PathBuf::from("/private/customer/secret/repo-child"));
        child.git_branch = Some("codex/details".to_string());
        app.subagent_cache.push(child);

        let body = project_agent_details(&app, agent_id)
            .expect("projection")
            .body;
        assert!(body.contains("Workspace: repo-child"), "{body}");
        assert!(!body.contains("/private/customer/secret"), "{body}");
        assert!(body.contains("Branch: codex/details"), "{body}");
    }

    #[test]
    fn recent_actions_are_bounded_and_render_only_structured_outcomes() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let agent_id = "agent_recent_actions";
        app.subagent_cache
            .push(agent(agent_id, SubAgentStatus::Running));
        let mut meta = AgentProgressMeta::default();
        for step in 1..=MAX_AGENT_RECENT_ACTIONS as u32 {
            meta.recent_actions.push_back(AgentRecentAction::bounded(
                if step == 2 {
                    "apply_patch"
                } else {
                    "read_file"
                },
                step,
                step != 2,
            ));
        }
        app.agent_progress_meta.insert(agent_id.to_string(), meta);

        let body = project_agent_details(&app, agent_id)
            .expect("projection")
            .body;
        assert_eq!(body.matches("Recent:").count(), 3, "{body}");
        assert!(body.contains("Recent: ! apply_patch · step 2"), "{body}");
    }

    #[test]
    fn alt_v_is_truthful_for_present_and_absent_evidence() {
        let tmp = tempdir().expect("tempdir");
        let agent_id = "agent_evidence";
        let mut app = test_app(tmp.path().to_path_buf());
        app.subagent_cache
            .push(agent(agent_id, SubAgentStatus::Running));
        let absent = project_agent_details(&app, agent_id).expect("projection");
        assert!(!absent.transcript_available);
        let details_chord = crate::tui::shell_key_routing::tool_details_chord();
        let transcript_hint = format!("{details_chord}:transcript");
        assert!(!absent.body.contains(&transcript_hint));
        let mut absent_view = AgentDetailsView::new(absent, agent_id, 80);
        assert!(matches!(
            absent_view.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT)),
            ViewAction::None
        ));

        {
            let mut store = app
                .runtime_services
                .handle_store
                .try_lock()
                .expect("handle store");
            let _ = store.insert_json(
                format!("agent:{agent_id}"),
                "full_transcript",
                json!({
                    "message_count": 1,
                    "messages": [{
                        "role": "assistant",
                        "content": [{
                            "type": "text",
                            "text": "exact evidence",
                            "cache_control": null
                        }]
                    }]
                }),
            );
        }
        let present = project_agent_details(&app, agent_id).expect("projection");
        assert!(present.transcript_available);
        assert!(
            present.body.contains(&transcript_hint),
            "expected {transcript_hint:?} in {}",
            present.body
        );
        assert!(
            !present.body.contains("Alt/⌥V"),
            "dual Alt/⌥ spelling must not appear: {}",
            present.body
        );
        let mut present_view = AgentDetailsView::new(present, agent_id, 80);
        assert!(matches!(
            present_view.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT)),
            ViewAction::Emit(ViewEvent::OpenAgentTranscript { agent_id: ref id }) if id == agent_id
        ));

        assert!(open_agent_details(&mut app, agent_id));
        let events = app
            .view_stack
            .handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));
        assert!(matches!(
            events.as_slice(),
            [ViewEvent::OpenAgentTranscript { agent_id: id }] if id == agent_id
        ));
        // The transcript destination is the in-place focus; the details view
        // survives underneath and its chord still points at the transcript.
        crate::tui::agent_focus::focus_agent(&mut app, agent_id);
        assert!(
            app.agent_focus
                .as_ref()
                .is_some_and(|focus| focus.is(agent_id))
        );
        assert!(matches!(
            app.view_stack
                .handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT))
                .as_slice(),
            [ViewEvent::OpenAgentTranscript { agent_id: id }] if id == agent_id
        ));
    }

    #[test]
    fn close_keys_emit_receipt_and_80x24_render_stays_safe() {
        let tmp = tempdir().expect("tempdir");
        let agent_id = "agent_render_80x24";
        let mut app = test_app(tmp.path().to_path_buf());
        app.subagent_cache
            .push(agent(agent_id, SubAgentStatus::Running));
        let projection = project_agent_details(&app, agent_id).expect("projection");
        let mut view = AgentDetailsView::new(projection, agent_id, 80);
        assert!(view.title().starts_with("Agent Details — "));
        assert!(view.body_text().contains("Exact evidence: unavailable"));

        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);
        let rendered = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|point| buffer[point].symbol())
            .collect::<String>();
        assert!(rendered.contains("Agent Details"), "{rendered}");
        assert!(rendered.contains("Assignment"), "{rendered}");
        assert!(!rendered.contains(agent_id), "{rendered}");

        for code in [KeyCode::Esc, KeyCode::Left] {
            assert!(matches!(
                view.handle_key(KeyEvent::new(code, KeyModifiers::NONE)),
                ViewAction::EmitAndClose(ViewEvent::AgentDetailsClosed { agent_id: ref id })
                    if id == agent_id
            ));
        }
    }
    // === #5906: a parked husk is not a pending question ==================

    /// The runtime hands a parked child a `needs_input` note that reads like a
    /// question, so Agent Details used to print `Pending question: Resume this
    /// parked child with ...` — a question no operator ever asked and none can
    /// answer. Build one exactly as the runtime does and assert the view names
    /// the state and both ways out instead.
    fn parked_child(agent_id: &str) -> SubAgentResult {
        let mut child = agent(
            agent_id,
            SubAgentStatus::Interrupted(
                "Parent turn ended before this turn-owned child settled.".to_string(),
            ),
        );
        child.worker_status = Some(AgentWorkerStatus::WaitingForUser);
        child.needs_input = Some(SubAgentNeedsInput {
            question: format!(
                "Resume this parked child with agent(action=\"start\", resume_from=\"{agent_id}\")."
            ),
        });
        child.checkpoint = Some(crate::tools::subagent::SubAgentCheckpoint {
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
        child
    }

    #[test]
    fn parked_details_name_the_state_and_the_recovery_not_a_pending_question() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let agent_id = "agent_parked_details";
        app.subagent_cache.push(parked_child(agent_id));
        crate::tui::subagent_routing::reconcile_subagent_activity_state(&mut app);

        let body = project_agent_details(&app, agent_id)
            .expect("projection")
            .body;

        assert!(body.contains("State: parked"), "{body}");
        assert!(
            !body.contains("waiting for input") && !body.contains("State: waiting"),
            "a parked husk must not wear the answerable label: {body}"
        );
        assert!(
            !body.contains("Pending question"),
            "nothing asked the operator anything: {body}"
        );
        // The recovery is quoted with the verbs the runtime actually exposes.
        assert!(body.contains("resume_from"), "{body}");
        assert!(body.contains("cancel"), "{body}");
    }

    #[test]
    fn a_child_that_really_asked_still_reports_waiting_for_input() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let agent_id = "agent_really_asked";
        let mut child = agent(agent_id, SubAgentStatus::Running);
        child.worker_status = Some(AgentWorkerStatus::WaitingForUser);
        child.needs_input = Some(SubAgentNeedsInput {
            question: "Which path should I use?".to_string(),
        });
        app.subagent_cache.push(child);
        crate::tui::subagent_routing::reconcile_subagent_activity_state(&mut app);

        let body = project_agent_details(&app, agent_id)
            .expect("projection")
            .body;
        assert!(body.contains("State: waiting"), "{body}");
        assert!(
            body.contains("Pending question: Which path should I use?"),
            "{body}"
        );
        assert!(!body.contains("parked"), "{body}");
    }
}
