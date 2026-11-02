//! Paused-command planning and dispatch preparation types
//! (TUI_MODULARIZATION.md slice 6). Actual dispatch execution stays in
//! `dispatch.rs`; this module owns the pause/resume plan and the
//! preparation/outcome types shared with it.

use super::*;

pub(crate) const INITIAL_PROMPT_DEFERRED_STATUS: &str =
    "Initial prompt ready; complete setup to send it";

pub(crate) fn paused_goal_objective_title(objective: &str) -> &str {
    objective
        .split(['\n', '\r'])
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or("the paused command")
}

pub(crate) fn is_resume_message(message: &str) -> bool {
    let words: Vec<String> = message
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        return false;
    }
    let text = words.join(" ");
    let has_resume_verb = words
        .iter()
        .any(|word| matches!(word.as_str(), "continue" | "resume"));
    if !has_resume_verb {
        return false;
    }

    let blockers = [
        "do not continue",
        "do not resume",
        "don t continue",
        "don t resume",
        "dont continue",
        "dont resume",
        "not continue",
        "not resume",
        "continue yet",
        "resume yet",
        "will continue",
        "will resume",
        "continue tomorrow",
        "resume tomorrow",
        "continue later",
        "resume later",
    ];
    if blockers.iter().any(|blocker| text.contains(blocker)) {
        return false;
    }
    if matches!(
        words.first().map(String::as_str),
        Some("how" | "what" | "when" | "where" | "why")
    ) {
        return false;
    }

    if words.len() == 1 {
        return true;
    }

    let context_words = [
        "please", "now", "paused", "pause", "command", "task", "work", "request", "goal",
        "previous", "last", "same", "it", "that", "this", "go", "ahead",
    ];
    if words
        .iter()
        .any(|word| context_words.contains(&word.as_str()))
    {
        return true;
    }

    text.starts_with("can you continue")
        || text.starts_with("can you resume")
        || text.starts_with("could you continue")
        || text.starts_with("could you resume")
}

pub(crate) fn paused_command_note(title: &str, resume: bool) -> String {
    let instruction = if resume {
        "The user is resuming that paused command. Continue the paused command."
    } else {
        "The user is not resuming that paused command. Answer only the new message and do not continue the paused command."
    };
    format!(
        "\n\nCodewhale paused custom slash command context:\n\
Paused custom slash command: {title}\n\
Paused command: {title}\n\
{instruction}"
    )
}

#[derive(Debug, Clone)]
pub(crate) enum PausedCommandDispatch {
    None,
    ClearWithoutQuarry,
    Resume { objective: String, note: String },
    Detach { note: String },
}

impl PausedCommandDispatch {
    pub(super) fn note(&self) -> Option<&str> {
        match self {
            Self::Resume { note, .. } | Self::Detach { note } => Some(note),
            Self::None | Self::ClearWithoutQuarry => None,
        }
    }

    pub(super) fn goal_objective(&self, app: &App) -> Option<String> {
        match self {
            Self::Resume { objective, .. } => Some(objective.clone()),
            Self::Detach { .. } | Self::ClearWithoutQuarry => None,
            Self::None => app.goal.objective.clone(),
        }
    }

    pub(super) fn apply(self, app: &mut App, engine_handle: &EngineHandle) {
        engine_handle.set_paused(false);
        match self {
            Self::None => {}
            Self::ClearWithoutQuarry => {
                app.paused = false;
                app.pausable = false;
            }
            Self::Resume { objective, .. } => {
                app.paused = false;
                app.paused_goal_objective = None;
                app.goal.objective = Some(objective);
                app.pausable = true;
            }
            Self::Detach { .. } => {
                app.paused = false;
                app.goal.objective = None;
                app.goal.tokens_used = 0;
                app.goal.time_used_seconds = 0;
                app.goal.continuation_count = 0;
            }
        }
    }
}

pub(crate) fn plan_paused_command_message(app: &App, user_message: &str) -> PausedCommandDispatch {
    if !app.paused && app.paused_goal_objective.is_none() {
        return PausedCommandDispatch::None;
    }

    let Some(objective) = app
        .paused_goal_objective
        .clone()
        .or_else(|| app.goal.objective.clone())
    else {
        return PausedCommandDispatch::ClearWithoutQuarry;
    };
    let title = paused_goal_objective_title(&objective).to_string();
    if is_resume_message(user_message) {
        PausedCommandDispatch::Resume {
            objective,
            note: paused_command_note(&title, true),
        }
    } else {
        PausedCommandDispatch::Detach {
            note: paused_command_note(&title, false),
        }
    }
}

pub(crate) fn pause_pausable_command(app: &mut App, engine_handle: &EngineHandle) {
    app.paused_goal_objective = app
        .paused_goal_objective
        .clone()
        .or_else(|| app.goal.objective.clone());
    app.goal.objective = None;
    app.goal.tokens_used = 0;
    app.goal.time_used_seconds = 0;
    app.goal.continuation_count = 0;
    app.paused = true;
    app.pausable = true;
    engine_handle.set_paused(true);
    app.status_message = Some(
        "Request paused. Send `continue` or `resume` to continue, or Esc to cancel.".to_string(),
    );
}

pub(crate) fn clear_paused_command_state(app: &mut App, engine_handle: &EngineHandle) {
    app.pausable = false;
    app.paused = false;
    app.paused_goal_objective = None;
    engine_handle.set_paused(false);
}

pub(crate) fn app_scoped_runtime_config(
    app: &App,
    config: &Config,
) -> Result<(ProviderIdentity, Config), String> {
    let identity = app.admitted_provider_identity()?.clone();
    let mut scoped = config.clone();
    scoped.scope_to_provider_identity(&identity)?;
    Ok((identity, scoped))
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DispatchRecovery {
    /// Normal immediate composer submit: restore the composer on failure.
    Immediate,
    /// A queued follow-up that was being edited in the composer.
    Draft,
    /// A queued follow-up pulled from the queue; re-insert at the prior index.
    Queued { restore_index: Option<usize> },
    /// Initial `--prompt` / startup input.
    Initial,
}

/// Snapshot of App state taken before the sync prepare phase so a failed
/// dispatch can roll back the optimistic history/api_messages changes.
#[derive(Debug, Clone)]
pub(crate) struct UserDispatchSnapshot {
    pub(crate) is_loading: bool,
    pub(crate) suppress_stream_events_until_turn_complete: bool,
    pub(crate) runtime_turn_status: Option<String>,
    pub(crate) receipt_text: Option<String>,
    pub(crate) receipt_started_at: Option<Instant>,
    pub(crate) tool_evidence: Vec<ToolEvidence>,
    pub(crate) history_len: usize,
    pub(crate) history_revisions_len: usize,
    pub(crate) api_messages_len: usize,
    pub(crate) last_send_at: Option<Instant>,
}

/// Data captured synchronously before the async dispatch phase. All values are
/// Send so the spawned task can resolve routes and send without holding `&mut App`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub(crate) struct UserDispatchPrepare {
    pub(super) message: QueuedMessage,
    pub(super) content: String,
    pub(super) references: Vec<ContextReference>,
    pub(super) paused_dispatch: PausedCommandDispatch,
    pub(super) app_route_identity: ProviderIdentity,
    pub(super) route_config: Config,
    pub(super) goal_objective: Option<String>,
    pub(super) goal_status: GoalStatus,
    pub(super) goal_token_budget: Option<u32>,
    pub(super) mode: AppMode,
    pub(super) api_provider: ProviderKind,
    pub(super) app_model: String,
    pub(super) auto_model: bool,
    pub(super) reasoning_effort: ReasoningEffort,
    pub(super) allow_shell: bool,
    pub(super) trust_mode: bool,
    pub(super) auto_approve: bool,
    pub(super) approval_mode: ApprovalMode,
    pub(super) translation_enabled: bool,
    pub(super) allowed_tools: Option<Vec<String>>,
    pub(super) hook_executor: Option<Arc<HookExecutor>>,
    pub(super) verbosity: Option<String>,
    pub(super) provenance: UserInputProvenance,
    pub(super) auto_router_context: String,
    pub(super) should_auto_resolve: bool,
    pub(super) auto_compact_user_configured: bool,
    pub(super) auto_compact: bool,
    pub(super) auto_compact_threshold_percent: f64,
    pub(super) snapshot: UserDispatchSnapshot,
    pub(super) cost_scope: codewhale_engine::cost_status::CostScopeToken,
    pub(super) message_index: usize,
    pub(super) history_cell: usize,
}

pub(crate) fn goal_status_from_snapshot(snapshot: &GoalSnapshot) -> Option<GoalStatus> {
    match snapshot.status.trim() {
        "active" => Some(GoalStatus::Active),
        "paused" => Some(GoalStatus::Paused),
        "complete" => Some(GoalStatus::Complete),
        "blocked" => Some(GoalStatus::Blocked),
        _ => None,
    }
}
