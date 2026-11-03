//! `/goal` — codex-style thread goals: set, inspect, pause, resume, and close
//! a durable objective. The engine owns the goal: setting or resuming one
//! starts work through the runtime's continuation steering, never by echoing
//! the objective back as a user message.
//!
//! FEAT-021 converts this handler to the portable command contract: it
//! consumes the typed project goal projection and the presentation facet only.
//! `CommandResult` and the emitted `AppAction` variants (`SetGoalStatus`,
//! `SetGoalObjective`, `SendMessage`) remain temporary TUI-owned data-only
//! references until FEAT-037.

use codewhale_command_contract::facets::{
    CommandPresentationContext, ProjectGoalState, ProjectGoalStatus,
};
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::tui::app::AppAction;

use crate::commands::CommandResult;

/// Translate one stable project key through the presentation facet.
fn translate(presentation: &mut dyn CommandPresentationContext, key: &str) -> String {
    presentation.translate(key, &[]).unwrap_or_default()
}

/// Map the portable goal status onto the TUI-owned action payload status.
///
/// This is the bounded FEAT-037 temporary action reference: the handler only
/// constructs data-only `AppAction` payloads and never touches the goal
/// service, config, or session manager.
fn to_action_status(status: ProjectGoalStatus) -> codewhale_engine::tools::goal::GoalStatus {
    match status {
        ProjectGoalStatus::Active => codewhale_engine::tools::goal::GoalStatus::Active,
        ProjectGoalStatus::Paused => codewhale_engine::tools::goal::GoalStatus::Paused,
        ProjectGoalStatus::Complete => codewhale_engine::tools::goal::GoalStatus::Complete,
        ProjectGoalStatus::Blocked => codewhale_engine::tools::goal::GoalStatus::Blocked,
    }
}

/// Declare, show, pause, resume, or close a goal.
fn goal_command(
    goal: &ProjectGoalState,
    presentation: &mut dyn CommandPresentationContext,
    arg: Option<&str>,
) -> CommandResult {
    match arg {
        Some("clear") | Some("reset") => CommandResult::action(AppAction::SetGoalStatus {
            status: codewhale_engine::tools::goal::GoalStatus::Active,
            clear: true,
        }),
        Some("done") | Some("complete") => {
            close_goal(goal, presentation, ProjectGoalStatus::Complete)
        }
        Some("pause") | Some("paused") => close_goal(goal, presentation, ProjectGoalStatus::Paused),
        Some("resume") | Some("continue") => resume_goal(goal, presentation),
        Some("help") | Some("?") | Some("usage") => CommandResult::message(goal_usage()),
        Some("status") | Some("show") => goal_status(goal, presentation),
        Some("block") | Some("blocked") => {
            close_goal(goal, presentation, ProjectGoalStatus::Blocked)
        }
        Some(text) if !text.is_empty() => {
            let (objective, budget) = parse_goal_budget(text);
            if objective.is_empty() || objective.chars().all(|c| c == '|') {
                return CommandResult::error(goal_usage());
            }
            // The command layer never mutates the visible projection. The UI
            // first persists and accepts this typed intent; only the engine's
            // authoritative GoalUpdated event may change what the user sees.
            CommandResult::action(AppAction::SetGoalObjective {
                objective,
                token_budget: budget,
            })
        }
        _ => {
            if goal.pending_controls {
                CommandResult::message(translate(presentation, "goal_control_accepted"))
            } else if goal.objective.is_some() {
                goal_status(goal, presentation)
            } else if !goal.conversation_present {
                // Nothing has happened yet: there is no context to derive an
                // objective from, so answer with usage instead of spending a
                // model turn on a question we already know the answer to.
                CommandResult::message(goal_usage())
            } else {
                // Context-dependent bare /goal: with no active goal, the
                // invocation itself is the ask — derive the objective from
                // the conversation instead of demanding a restatement
                // (mirrors bare /workflow). The end-of-turn GoalUpdated
                // snapshot syncs the created goal into the sidebar.
                let message = BARE_GOAL_BRIEF.to_string();
                CommandResult::with_message_and_action(
                    "Declaring a goal from the current context...",
                    AppAction::SendMessage(message),
                )
            }
        }
    }
}

/// Model-facing brief for a context-dependent bare `/goal`. `create_goal`
/// is eager on the default catalog, but a narrowed surface can defer it, so
/// the brief carries the activation path (#6747).
pub(crate) const BARE_GOAL_BRIEF: &str = "The user invoked /goal with no objective — declare a goal for the \
     CURRENT work. Synthesize the objective from the conversation context (the \
     task in flight, recent findings, open items) and set it by calling \
     `create_goal` with the full objective (and a token_budget only if one was \
     discussed); if `create_goal` is not in your tool list, load it with `tool_search` \
     first. Then continue working toward it. Only if the conversation \
     genuinely contains no work yet, ask the user what the goal should be.";

/// Plain status line: objective, state, elapsed, budget, continuations, and
/// — for an active goal that no turn is driving right now — how to continue.
fn goal_status(
    goal: &ProjectGoalState,
    presentation: &mut dyn CommandPresentationContext,
) -> CommandResult {
    let Some(obj) = goal.objective.as_deref() else {
        return CommandResult::message(goal_usage());
    };
    let elapsed = if goal.time_used_seconds > 0 {
        codewhale_command_contract::elapsed::format_elapsed_secs(goal.time_used_seconds)
    } else if let Some(secs) = goal.started_at_elapsed_seconds {
        codewhale_command_contract::elapsed::format_elapsed_secs(secs)
    } else {
        "unknown".to_string()
    };
    let budget_str = goal
        .token_budget
        .map(|b| {
            let used = if goal.tokens_used > 0 {
                goal.tokens_used
            } else {
                u64::from(goal.session_total_tokens)
            };
            let pct = if b > 0 {
                (used as f64 / f64::from(b) * 100.0).min(100.0)
            } else {
                0.0
            };
            format!(" · tokens {used}/{b} ({pct:.0}%)")
        })
        .unwrap_or_default();
    let mut state = goal_status_label(goal.status).to_string();
    if let (ProjectGoalStatus::Paused, Some(reason)) = (goal.status, goal.pause_reason.as_deref()) {
        state = format!("{state} ({reason})");
    }
    let mut line = format!(
        "Goal {state}: \"{obj}\" · elapsed {elapsed}{budget_str} · continuations {}",
        goal.continuation_count
    );
    if goal.status == ProjectGoalStatus::Active
        && !goal.is_loading
        && !goal.goal_continuation_waiting
    {
        line.push_str(" · ");
        line.push_str(&translate(presentation, "goal_status_idle_hint"));
    }
    CommandResult::message(line)
}

/// Close out the goal at `status`. Pure control plane: the engine stops (or
/// re-arms) the continuation loop from the `SetGoalStatus` op; no model turn
/// is dispatched.
fn close_goal(
    goal: &ProjectGoalState,
    presentation: &mut dyn CommandPresentationContext,
    status: ProjectGoalStatus,
) -> CommandResult {
    if effective_goal_objective(goal).is_none_or(str::is_empty) {
        return CommandResult::error("No goal set. Use /goal <objective> [budget: N] first.");
    }
    if effective_goal_status(goal) == status {
        if goal.pending_controls {
            return CommandResult::message(translate(presentation, "goal_control_accepted"));
        }
        return goal_status(goal, presentation);
    }

    CommandResult::action(AppAction::SetGoalStatus {
        status: to_action_status(status),
        clear: false,
    })
}

/// Resume a paused goal. The engine restarts the continuation loop itself
/// (`SetGoalStatus` → schedule kickoff); the objective is never re-sent as a
/// user message.
fn resume_goal(
    goal: &ProjectGoalState,
    presentation: &mut dyn CommandPresentationContext,
) -> CommandResult {
    if effective_goal_objective(goal)
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        return CommandResult::error("No paused goal set. Use /goal <objective> first.");
    }

    if effective_goal_status(goal) == ProjectGoalStatus::Active {
        if goal.pending_controls {
            return CommandResult::message(translate(presentation, "goal_control_accepted"));
        }
        if goal.is_loading || goal.goal_continuation_waiting {
            return goal_status(goal, presentation);
        }
    }

    CommandResult::action(AppAction::SetGoalStatus {
        status: codewhale_engine::tools::goal::GoalStatus::Active,
        clear: false,
    })
}

fn effective_goal_objective(goal: &ProjectGoalState) -> Option<&str> {
    if goal.pending_controls {
        goal.last_known_objective.as_deref()
    } else {
        goal.objective.as_deref()
    }
}

fn effective_goal_status(goal: &ProjectGoalState) -> ProjectGoalStatus {
    if goal.pending_controls {
        goal.last_known_status.unwrap_or(ProjectGoalStatus::Active)
    } else {
        goal.status
    }
}

fn goal_usage() -> &'static str {
    "No goal set. /goal <objective> [budget: N] starts one; the agent works toward it \
     across turns until it is verified complete, blocked, or you stop it.\n\
     /goal — progress of the current goal\n\
     /goal pause — pause without continuing\n\
     /goal resume — resume and continue\n\
     /goal done — mark complete (skips the model's verification)\n\
     /goal blocked — mark blocked\n\
     /goal clear — remove the current goal."
}

fn goal_status_label(status: ProjectGoalStatus) -> &'static str {
    match status {
        ProjectGoalStatus::Active => "active",
        ProjectGoalStatus::Complete => "complete",
        ProjectGoalStatus::Paused => "paused",
        ProjectGoalStatus::Blocked => "blocked",
    }
}

/// Parse text like "Implement login | budget: 50000" into (objective, budget).
fn parse_goal_budget(text: &str) -> (String, Option<u32>) {
    // Only an explicit, well-formed budget suffix splits the objective.
    // `budget:` followed by something that is not a number is prose that
    // belongs to the objective — truncating it would silently rewrite what
    // the user asked for.
    for separator in [" | budget:", " budget:", "budget:"] {
        if let Some((objective, rest)) = text.split_once(separator) {
            let budget = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok());
            if let Some(budget) = budget {
                return (objective.trim().to_string(), Some(budget));
            }
        }
    }
    (text.trim().to_string(), None)
}

pub(in crate::commands) const GOAL_INFO: CommandInfo = CommandInfo {
    name: "goal",
    aliases: &[],
    usage: "/goal [objective|status|pause|resume|done|blocked|clear] [budget: N]",
    description_key: "cmd_goal_description",
};

pub(in crate::commands) struct GoalCmd;

impl RegisterCommand<CommandResult> for GoalCmd {
    fn info() -> &'static CommandInfo {
        &GOAL_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::PROJECT
                .union(codewhale_command_contract::handler::CommandCapabilities::PRESENTATION),
            handler: goal_contextual,
        }
    }
}

/// Contextual `/goal` dispatch (FEAT-021 Phase 4).
///
/// Destructures the declared `PROJECT | PRESENTATION` facets with safe
/// missing-facet errors; the portable handler never panics on absent
/// capabilities and never emits a partial action.
fn goal_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(project) = parts.project.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: project");
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let goal = project.goal_state();
    goal_command(&goal, presentation, arg)
}

#[cfg(test)]
mod tests {
    /// #6747: the bare /goal brief names only callable tools and teaches
    /// the `tool_search` activation path.
    #[test]
    fn bare_goal_brief_names_only_callable_tools() {
        codewhale_engine::tools::canonical_action_test_support::assert_text_names_only_callable_tools(
            "bare /goal brief",
            BARE_GOAL_BRIEF,
        );
        assert!(BARE_GOAL_BRIEF.contains("`tool_search`"));
    }

    use super::*;
    use codewhale_command_contract::facets::CommandProjectContext;

    /// Deterministic fake project facet over portable values only.
    struct FakeProject;

    impl FakeProject {
        fn new() -> Self {
            Self
        }
    }

    impl CommandProjectContext for FakeProject {
        fn lsp_enabled(&self) -> bool {
            false
        }

        fn lsp_set(&mut self, _enabled: bool) -> Result<(), String> {
            Ok(())
        }

        fn goal_state(&self) -> ProjectGoalState {
            goal_state()
        }
    }

    /// Deterministic fake presentation facet over portable values only.
    struct FakePresentation;

    impl CommandPresentationContext for FakePresentation {
        fn translate(&self, key: &str, _replacements: &[(&str, &str)]) -> Result<String, String> {
            match key {
                "goal_control_accepted" => {
                    Ok("Goal control saved; applying at the next safe boundary.".to_string())
                }
                "goal_status_idle_hint" => {
                    Ok("not running now — send a message or /goal resume to continue".to_string())
                }
                other => Err(format!("unknown key {other}")),
            }
        }
    }

    fn goal_state() -> ProjectGoalState {
        ProjectGoalState {
            objective: None,
            status: ProjectGoalStatus::Active,
            pause_reason: None,
            started_at_elapsed_seconds: None,
            time_used_seconds: 0,
            token_budget: None,
            tokens_used: 0,
            session_total_tokens: 0,
            continuation_count: 0,
            pending_controls: false,
            last_known_objective: None,
            last_known_status: None,
            conversation_present: false,
            is_loading: false,
            goal_continuation_waiting: false,
        }
    }

    fn run(goal: &ProjectGoalState, arg: Option<&str>) -> CommandResult {
        let mut presentation = FakePresentation;
        goal_command(goal, &mut presentation, arg)
    }

    #[test]
    fn test_set_goal_dispatches_control_plane_not_user_echo() {
        let goal = goal_state();
        let result = run(&goal, Some("Fix the login bug"));
        assert!(result.message.is_none());
        // The engine owns the kickoff: the objective must reach it as a
        // SetGoalObjective control op, never as a SendMessage user echo.
        assert!(matches!(
            result.action,
            Some(AppAction::SetGoalObjective { ref objective, token_budget: None })
                if objective == "Fix the login bug"
        ));
    }

    #[test]
    fn test_goal_budget_parsing_reaches_the_op() {
        let goal = goal_state();
        let result = run(&goal, Some("Ship 0.9.10 | budget: 5000"));
        assert!(matches!(
            result.action,
            Some(AppAction::SetGoalObjective { ref objective, token_budget: Some(5000) })
                if objective == "Ship 0.9.10"
        ));
    }

    #[test]
    fn pause_and_clear_are_control_ops_without_optimistic_state() {
        let mut goal = goal_state();
        goal.objective = Some("Keep the build green".to_string());
        goal.status = ProjectGoalStatus::Active;
        let paused = run(&goal, Some("pause"));
        assert!(paused.message.is_none());
        assert!(matches!(
            paused.action,
            Some(AppAction::SetGoalStatus {
                status: codewhale_engine::tools::goal::GoalStatus::Paused,
                clear: false
            })
        ));

        let cleared = run(&goal, Some("clear"));
        assert!(cleared.message.is_none());
        assert!(matches!(
            cleared.action,
            Some(AppAction::SetGoalStatus {
                status: codewhale_engine::tools::goal::GoalStatus::Active,
                clear: true
            })
        ));
    }

    #[test]
    fn test_goal_without_argument_synthesizes_goal_from_context() {
        // Bare /goal with no active goal is context-dependent: the model
        // derives the objective from the conversation and sets it via
        // create_goal — it must not error with a usage demand.
        let mut goal = goal_state();
        goal.conversation_present = true;
        let result = run(&goal, None);
        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(message.contains("Synthesize the objective from the conversation"));
        assert!(message.contains("`create_goal`"));
    }

    #[test]
    fn bare_goal_on_an_empty_session_prints_usage_without_a_model_turn() {
        // No conversation yet: there is nothing to derive an objective from,
        // so the answer is usage — free, and not a question to the model.
        let goal = goal_state();
        let result = run(&goal, None);
        assert!(!result.is_error);
        assert!(result.action.is_none());
        assert!(result.message.unwrap().contains("/goal <objective>"));
    }

    #[test]
    fn goal_status_reports_objective_and_state() {
        let mut goal = goal_state();
        goal.objective = Some("Make the suite green".to_string());
        goal.token_budget = Some(100);
        goal.status = ProjectGoalStatus::Active;
        let result = run(&goal, Some("status"));
        let line = result.message.unwrap();
        assert!(line.contains("Make the suite green"));
        assert!(line.contains("active"));
        assert!(line.contains("100"));
    }

    #[test]
    fn goal_status_includes_elapsed_and_continuations() {
        let mut goal = goal_state();
        goal.objective = Some("Ship it".to_string());
        goal.status = ProjectGoalStatus::Active;
        goal.time_used_seconds = 125;
        goal.continuation_count = 3;
        let result = run(&goal, Some("status"));
        let line = result.message.unwrap();
        assert!(line.contains("elapsed 2m 05s"), "line: {line}");
        assert!(line.contains("continuations 3"), "line: {line}");
    }

    #[test]
    fn paused_goal_status_shows_reason() {
        let mut goal = goal_state();
        goal.objective = Some("Ship it".to_string());
        goal.status = ProjectGoalStatus::Paused;
        goal.pause_reason = Some("usage limit".to_string());
        let result = run(&goal, Some("status"));
        assert!(result.message.unwrap().contains("paused (usage limit)"));
    }

    #[test]
    fn resume_on_an_active_goal_only_kicks_when_idle() {
        let mut goal = goal_state();
        goal.objective = Some("Keep the build green".to_string());
        goal.status = ProjectGoalStatus::Active;
        for (is_loading, waiting) in [(false, false), (true, false), (false, true)] {
            goal.is_loading = is_loading;
            goal.goal_continuation_waiting = waiting;
            let resumed = run(&goal, Some("resume"));
            assert!(!resumed.is_error);
            if is_loading || waiting {
                assert!(resumed.action.is_none());
                assert!(resumed.message.unwrap().contains("Keep the build green"));
            } else {
                assert!(matches!(
                    resumed.action,
                    Some(AppAction::SetGoalStatus {
                        status: codewhale_engine::tools::goal::GoalStatus::Active,
                        clear: false
                    })
                ));
            }
        }
    }

    #[test]
    fn invalid_budget_suffix_stays_part_of_the_objective() {
        let goal = goal_state();
        let result = run(&goal, Some("Fix budget: handling in settings"));
        assert!(matches!(
            result.action,
            Some(AppAction::SetGoalObjective { ref objective, token_budget: None })
                if objective == "Fix budget: handling in settings"
        ));
    }

    #[test]
    fn completing_a_goal_without_one_is_an_error() {
        let goal = goal_state();
        let result = run(&goal, Some("done"));
        assert!(result.is_error);
    }

    #[test]
    fn goal_help_route_returns_usage() {
        let goal = goal_state();
        for arg in ["help", "?", "usage"] {
            let result = run(&goal, Some(arg));
            assert!(!result.is_error);
            assert!(result.message.unwrap().contains("/goal <objective>"));
        }
    }

    #[test]
    fn missing_project_facet_fails_safely() {
        let result = goal_contextual(CommandContexts::empty(), Some("status"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("Command capability unavailable: project")
        );
    }

    #[test]
    fn missing_presentation_facet_fails_safely() {
        // PROJECT present but PRESENTATION absent: safe error, no partial action.
        let mut project = FakeProject::new();
        let contexts = CommandContexts::empty().with_project(&mut project);
        let result = goal_contextual(contexts, Some("status"));
        assert!(result.is_error);
        assert!(
            result.action.is_none(),
            "no partial action on missing facet"
        );
        assert!(
            result
                .message
                .unwrap()
                .contains("Command capability unavailable: presentation")
        );
    }

    #[test]
    fn goal_control_accepted_translates_through_fake_presentation() {
        // Pending controls route to the translated message, not the raw key.
        let mut goal = goal_state();
        goal.objective = Some("Keep the build green".to_string());
        goal.status = ProjectGoalStatus::Active;
        goal.pending_controls = true;
        goal.last_known_objective = Some("Keep the build green".to_string());
        goal.last_known_status = Some(ProjectGoalStatus::Paused);
        let result = run(&goal, Some("pause"));
        assert!(!result.is_error);
        let msg = result.message.expect("pending-control message");
        assert!(msg.contains("Goal control saved"));
        assert!(
            !msg.contains("goal_control_accepted"),
            "raw key must not leak"
        );
    }

    #[test]
    fn idle_hint_translates_through_fake_presentation() {
        // Active goal not being driven: the idle hint is the translated text.
        let mut goal = goal_state();
        goal.objective = Some("Ship it".to_string());
        goal.status = ProjectGoalStatus::Active;
        goal.is_loading = false;
        goal.goal_continuation_waiting = false;
        let result = run(&goal, Some("status"));
        let line = result.message.unwrap();
        assert!(
            line.contains("not running now"),
            "idle hint must be translated: {line}"
        );
        assert!(
            !line.contains("goal_status_idle_hint"),
            "raw key must not leak"
        );
    }

    #[test]
    fn goal_token_fallback_uses_session_total() {
        // tokens_used == 0 falls back to the session conversation-token total.
        let mut goal = goal_state();
        goal.objective = Some("Ship it".to_string());
        goal.token_budget = Some(100);
        goal.tokens_used = 0;
        goal.session_total_tokens = 42;
        let result = run(&goal, Some("status"));
        let line = result.message.unwrap();
        assert!(line.contains("tokens 42/100 (42%)"), "line: {line}");
    }

    #[test]
    fn goal_token_uses_engine_count_when_nonzero() {
        let mut goal = goal_state();
        goal.objective = Some("Ship it".to_string());
        goal.token_budget = Some(100);
        goal.tokens_used = 10;
        goal.session_total_tokens = 42;
        let result = run(&goal, Some("status"));
        let line = result.message.unwrap();
        assert!(line.contains("tokens 10/100 (10%)"), "line: {line}");
    }
}
