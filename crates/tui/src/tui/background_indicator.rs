//! Compact live "pending background work" indicator near the composer.
//!
//! When the main turn is waiting on background shells, durable tasks, or
//! running sub-agents, a single chip row renders directly above the composer
//! so the user can see — at the exact place they are looking — that the model
//! is blocked on background work and on what. It auto-updates as items start
//! and finish, and the row collapses to zero rows entirely when nothing is
//! pending.
//!
//! # Source of truth
//!
//! The indicator mirrors state the Work strip and the `/jobs` surface already
//! read — it introduces no new registry and takes no lock in the render path:
//!
//! - **Durable tasks**: `App::task_panel` entries that are not live shells.
//!   Background shells are first-class work-strip rows (`▾ Shells N`) and
//!   are not mirrored here.
//! - **Sub-agents**: the union of `App::subagent_cache` entries still in
//!   `Running` state and `App::agent_progress` keys — the same live projection
//!   `running_agent_count` uses, so spawn/completion events update the chip
//!   immediately.
//!
//! # Rendering
//!
//! `ui/frame.rs` reserves one extra layout row between the pending-input
//! preview and the composer, carves it from the auxiliary budget (so compact
//! terminals shed the chip before they shed chat/composer space), and calls
//! [`render`] only when [`PendingWork::is_empty`] is false.

use std::collections::HashSet;

use crate::tui::app::{App, TaskPanelEntry, TaskPanelEntryKind};
use codewhale_localization::truncate_to_width;

/// Per-item label cap so one long command or objective cannot eat the whole
/// row before the whole-line truncation kicks in.
const ITEM_LABEL_MAX_WIDTH: usize = 20;

/// Kind of in-flight background work the main turn is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingItemKind {
    /// Background shell job (the `/jobs` surface).
    Shell,
    /// Durable task / RLM run tracked by the TaskManager.
    Task,
    /// Running sub-agent / fleet worker.
    Agent,
}

/// Lifecycle state carried by the App's background-work projection.
///
/// The task panel currently receives wire-status tokens, so normalize them at
/// the projection boundary. Renderers then consume this typed state instead
/// of re-interpreting status strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingItemState {
    Queued,
    Running,
}

impl PendingItemKind {
    /// Singular noun used for the count summary ("1 shell", "2 agents").
    #[must_use]
    pub fn noun(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Task => "task",
            Self::Agent => "agent",
        }
    }

    pub(crate) fn plural_noun(self, count: usize) -> String {
        if count == 1 {
            self.noun().to_string()
        } else {
            format!("{}s", self.noun())
        }
    }
}

/// One in-flight item shown in the chip row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingItem {
    pub kind: PendingItemKind,
    pub state: PendingItemState,
    /// Short human label (task id / role / name / command), pre-truncated.
    pub label: String,
}

/// Snapshot of the background work the main turn is waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PendingWork {
    /// Shells and tasks first (in `task_panel` order), then agents in stable
    /// id order. Empty means the indicator row is hidden.
    pub items: Vec<PendingItem>,
}

impl PendingWork {
    #[must_use]
    pub fn count(&self, kind: PendingItemKind) -> usize {
        self.items.iter().filter(|item| item.kind == kind).count()
    }

    #[must_use]
    #[cfg(test)]
    pub fn count_state(&self, state: PendingItemState) -> usize {
        self.items.iter().filter(|item| item.state == state).count()
    }
}

fn truncate_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        "…".to_string()
    } else {
        truncate_to_width(trimmed, ITEM_LABEL_MAX_WIDTH)
    }
}

/// Build the composer pending-work snapshot from the same state the Work
/// strip and `/jobs` surface render. Read-only; no locks, no registries.
///
/// Live shells deliberately remain only on the detailed Work strip, rather
/// than being repeated in the composer crumb.
#[must_use]
pub fn pending_work_from_app(app: &App) -> PendingWork {
    collect_pending_work(app)
}

fn collect_pending_work(app: &App) -> PendingWork {
    let mut items: Vec<PendingItem> = Vec::new();

    // Background shells and durable tasks: the merged task_panel snapshot
    // refreshed by `refresh_active_task_panel` on the event loop. Shell jobs
    // carry a `shell: <command>` summary; durable/RLM tasks carry their own
    // prompt summary and their task id is the stable label.
    for entry in &app.task_panel {
        let Some(state) = pending_item_state(entry) else {
            continue;
        };
        // Live shells belong on the work strip (`▾ Shells N`), not this
        // composer crumb. A dual surface hid the PTY behind hourglasses.
        let is_shell = is_live_shell_entry(entry);
        if is_shell {
            continue;
        }
        items.push(PendingItem {
            kind: PendingItemKind::Task,
            state,
            label: truncate_label(entry.id.as_str()),
        });
    }

    // Running sub-agents: subagent_cache first (richer: nickname/role), then
    // agent_progress-only ids, deduped by id exactly like running_agent_count.
    let mut seen: HashSet<&str> = HashSet::new();
    for agent in app.subagent_cache.iter().filter(|agent| {
        matches!(
            agent.status,
            crate::tools::subagent::SubAgentStatus::Running
        )
    }) {
        if !seen.insert(agent.agent_id.as_str()) {
            continue;
        }
        let role = agent
            .assignment
            .role
            .as_deref()
            .filter(|role| !role.trim().is_empty())
            .unwrap_or_else(|| agent.agent_type.as_str());
        // The name this lane was dispatched under is what the operator
        // thinks in; the whale nickname only names an unnamed one (#5287).
        let name = app
            .agent_given_name(&agent.agent_id)
            .or_else(|| {
                agent
                    .nickname
                    .clone()
                    .filter(|name| !name.trim().is_empty() && *name != agent.agent_id)
            })
            .or_else(|| app.agent_label_map.get(&agent.agent_id).cloned());
        let label = match name.as_deref() {
            Some(name) if name != role => format!("{name}·{role}"),
            _ => role.to_string(),
        };
        items.push(PendingItem {
            kind: PendingItemKind::Agent,
            state: PendingItemState::Running,
            label: truncate_label(&label),
        });
    }
    for id in app.agent_progress.keys() {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let label = app.agent_display_label(id);
        items.push(PendingItem {
            kind: PendingItemKind::Agent,
            state: PendingItemState::Running,
            label: truncate_label(&label),
        });
    }

    PendingWork { items }
}

/// Normalize the task-panel's serialized lifecycle token once at the
/// projection boundary. Consumers should use [`PendingItemState`] rather than
/// comparing these wire values in their render paths.
#[must_use]
pub(crate) fn pending_item_state(entry: &TaskPanelEntry) -> Option<PendingItemState> {
    match entry.status.as_str() {
        "queued" => Some(PendingItemState::Queued),
        "running" => Some(PendingItemState::Running),
        _ => None,
    }
}

/// Whether a task-panel row is a currently live shell job. This is shared by
/// the detailed Work strip and compact live-status projections so a shell
/// cannot be omitted or classified differently between surfaces.
#[must_use]
pub(crate) fn is_live_shell_entry(entry: &TaskPanelEntry) -> bool {
    pending_item_state(entry).is_some() && entry.kind == TaskPanelEntryKind::Shell
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn shell(label: &str) -> PendingItem {
        PendingItem {
            kind: PendingItemKind::Shell,
            state: PendingItemState::Running,
            label: truncate_label(label),
        }
    }

    fn task(label: &str) -> PendingItem {
        PendingItem {
            kind: PendingItemKind::Task,
            state: PendingItemState::Running,
            label: truncate_label(label),
        }
    }

    fn agent(label: &str) -> PendingItem {
        PendingItem {
            kind: PendingItemKind::Agent,
            state: PendingItemState::Running,
            label: truncate_label(label),
        }
    }

    #[test]
    fn empty_pending_work_hides_the_indicator() {
        let work = PendingWork::default();
        assert!(work.items.is_empty());
        assert_eq!(work.count(PendingItemKind::Shell), 0);
        assert_eq!(work.count(PendingItemKind::Task), 0);
        assert_eq!(work.count(PendingItemKind::Agent), 0);
    }

    #[test]
    fn counts_cover_shells_tasks_and_agents_with_pluralization() {
        let work = PendingWork {
            items: vec![shell("cargo test"), task("run"), agent("Agent 3·scout")],
        };
        assert!(!work.items.is_empty());
        assert_eq!(work.count(PendingItemKind::Shell), 1);
        assert_eq!(work.count(PendingItemKind::Task), 1);
        assert_eq!(work.count(PendingItemKind::Agent), 1);
        assert_eq!(PendingItemKind::Shell.plural_noun(1), "shell");
        assert_eq!(PendingItemKind::Task.plural_noun(1), "task");
        assert_eq!(PendingItemKind::Agent.plural_noun(1), "agent");
    }

    #[test]
    fn plural_counts_for_multiple_same_kind_items() {
        let work = PendingWork {
            items: vec![shell("one"), shell("two"), agent("A")],
        };
        assert_eq!(work.count(PendingItemKind::Shell), 2);
        assert_eq!(PendingItemKind::Shell.plural_noun(2), "shells");
        assert_eq!(work.count(PendingItemKind::Agent), 1);
    }

    #[test]
    fn completion_clears_the_line() {
        let mut work = PendingWork {
            items: vec![shell("cargo test")],
        };
        assert!(!work.items.is_empty());
        work.items.clear();
        assert!(work.items.is_empty());
    }

    #[test]
    fn long_labels_are_pre_truncated() {
        let long = "cargo test -p codewhale-tui --lib background_indicator -- --exact this is long";
        let work = PendingWork {
            items: vec![shell(long)],
        };
        assert!(
            work.items[0].label.contains('…'),
            "over-long command ellipsized: got {}",
            work.items[0].label
        );
        assert!(
            work.items[0].label.width() <= ITEM_LABEL_MAX_WIDTH,
            "label capped at {ITEM_LABEL_MAX_WIDTH}: got {}",
            work.items[0].label
        );
    }

    #[test]
    fn pending_work_from_app_skips_non_background_entries() {
        let options = crate::tui::test_support::test_tui_options(std::path::PathBuf::from("."));
        let app = crate::tui::test_support::test_app_with_options(options);
        assert!(
            pending_work_from_app(&app).items.is_empty(),
            "bare app has no pending background work"
        );
    }

    #[test]
    fn pending_work_from_app_picks_up_shell_and_agent_entries() {
        use crate::tui::app::TaskPanelEntry;
        let options = crate::tui::test_support::test_tui_options(std::path::PathBuf::from("."));
        let mut app = crate::tui::test_support::test_app_with_options(options);
        app.task_panel.push(TaskPanelEntry {
            exit_code: None,
            id: "shell_a1b2c3d4".to_string(),
            status: "running".to_string(),
            prompt_summary: "shell: cargo test -p codewhale-tui".to_string(),
            duration_ms: Some(42_000),
            kind: TaskPanelEntryKind::Shell,
            stale: true,
            elapsed_since_output_ms: Some(99_000),
            owner_agent_id: None,
            owner_agent_name: None,
            current_tool: None,
            role: None,
            files_touched: 0,
        });
        app.task_panel.push(TaskPanelEntry {
            exit_code: None,
            id: "run".to_string(),
            status: "running".to_string(),
            prompt_summary: "background confirmation test".to_string(),
            duration_ms: Some(99_000),
            kind: TaskPanelEntryKind::Background,
            stale: false,
            elapsed_since_output_ms: None,
            owner_agent_id: None,
            owner_agent_name: None,
            current_tool: None,
            role: None,
            files_touched: 0,
        });
        app.agent_progress
            .insert("agent_live".to_string(), "checking the build".to_string());
        app.agent_label_map
            .insert("agent_live".to_string(), "Agent 1".to_string());

        let work = pending_work_from_app(&app);
        assert_eq!(
            work.count(PendingItemKind::Shell),
            0,
            "shells are work-strip rows, not crumb items"
        );
        assert_eq!(work.count(PendingItemKind::Task), 1, "durable task counted");
        assert_eq!(
            work.count(PendingItemKind::Agent),
            1,
            "running agent counted"
        );
        let labels: Vec<&str> = work.items.iter().map(|item| item.label.as_str()).collect();
        assert!(
            !labels.iter().any(|label| label.contains("cargo test")),
            "shell command must not occupy the counts: {labels:?}"
        );
        assert!(
            labels.iter().any(|label| label.contains("run")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|label| label.contains("Agent 1")),
            "{labels:?}"
        );

        // Completion clears the snapshot: dropping the running entries hides
        // the indicator entirely.
        app.task_panel.clear();
        app.agent_progress.clear();
        let cleared = pending_work_from_app(&app);
        assert!(cleared.items.is_empty(), "completion clears the indicator");
    }

    #[test]
    fn durable_work_projection_keeps_queued_and_running_states() {
        use crate::tui::app::TaskPanelEntry;
        let options = crate::tui::test_support::test_tui_options(std::path::PathBuf::from("."));
        let mut app = crate::tui::test_support::test_app_with_options(options);
        app.task_panel.extend([
            TaskPanelEntry {
                exit_code: None,
                id: "durable-running".to_string(),
                status: "running".to_string(),
                prompt_summary: "durable work".to_string(),
                duration_ms: Some(42_000),
                kind: TaskPanelEntryKind::Background,
                stale: false,
                elapsed_since_output_ms: None,
                owner_agent_id: None,
                owner_agent_name: None,
                current_tool: None,
                role: None,
                files_touched: 0,
            },
            TaskPanelEntry {
                exit_code: None,
                id: "durable-queued".to_string(),
                status: "queued".to_string(),
                prompt_summary: "durable work".to_string(),
                duration_ms: None,
                kind: TaskPanelEntryKind::Background,
                stale: false,
                elapsed_since_output_ms: None,
                owner_agent_id: None,
                owner_agent_name: None,
                current_tool: None,
                role: None,
                files_touched: 0,
            },
        ]);

        let work = pending_work_from_app(&app);
        assert_eq!(work.count(PendingItemKind::Task), 2);
        assert_eq!(work.count_state(PendingItemState::Queued), 1);
        assert_eq!(work.count_state(PendingItemState::Running), 1);
        assert!(
            work.items
                .iter()
                .any(|item| item.label == "durable-running"
                    && item.state == PendingItemState::Running),
            "durable running task must retain its typed state: {work:?}"
        );
        assert!(
            work.items.iter().any(
                |item| item.label == "durable-queued" && item.state == PendingItemState::Queued
            ),
            "durable queued task must retain its typed state: {work:?}"
        );
    }

    #[test]
    fn pending_agents_are_labelled_by_the_name_they_were_dispatched_under() {
        use crate::tools::subagent::{SubAgentAssignment, SubAgentResult, SubAgentStatus};
        use codewhale_agent_contract::role::FleetRole;
        let options = crate::tui::test_support::test_tui_options(std::path::PathBuf::from("."));
        let mut app = crate::tui::test_support::test_app_with_options(options);
        let running = |agent_id: &str, name: &str| SubAgentResult {
            usage: None,
            name: name.to_string(),
            agent_id: agent_id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: FleetRole::Worker,
            assignment: SubAgentAssignment {
                native_preset: None,
                objective: "sweep the lane".to_string(),
                role: Some("builder".to_string()),
            },
            model: "test-model".to_string(),
            nickname: Some(crate::tools::subagent::whale_name_for_id_in_locale(
                agent_id, "en",
            )),
            status: SubAgentStatus::Running,
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
        };
        app.subagent_cache
            .push(running("agent_named_lane", "triage"));
        // An unnamed dispatch: `name` is still the agent id, so the whale
        // nickname stays the honest label (#5287).
        app.subagent_cache
            .push(running("agent_plain_lane", "agent_plain_lane"));

        let work = pending_work_from_app(&app);
        let labels: Vec<&str> = work.items.iter().map(|item| item.label.as_str()).collect();
        let whale = crate::tools::subagent::whale_name_for_id_in_locale("agent_plain_lane", "en");
        assert_eq!(
            labels,
            ["triage·builder".to_string(), format!("{whale}·builder")]
        );
    }
}
