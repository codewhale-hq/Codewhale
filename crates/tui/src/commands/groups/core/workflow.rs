//! `/workflow` command — review, confirm, then orchestrate.
//!
//! Ordinary objectives produce a bounded, tool-less planning turn. The user
//! reviews that draft and explicitly runs `/workflow confirm`; only that later
//! turn can reach the canonical `workflow` tool. Control verbs (`status`,
//! `cancel`, `settings`, `help`) remain host-owned and spend no model turn.
//!
//! `/workflows` (separate command, below) is the observation surface: the
//! live run dashboard. It never orchestrates — that authority belongs to
//! `/workflow` alone.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::WORKFLOW_DRAFT_INSTRUCTION_PREFIX;
use crate::tui::app::{App, AppAction};
use codewhale_config::AppMode;
#[cfg(test)]
use codewhale_execpolicy::ApprovalMode;
use codewhale_localization::MessageId;
use codewhale_models::ContentBlock;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "workflow",
    aliases: &["wf"],
    usage: "/workflow [objective|confirm|run <path>|status [run_id]|cancel [run_id]|settings]",
    description_id: MessageId::CmdWorkflowDescription,
};

pub(in crate::commands) struct WorkflowCmd;

impl RegisterCommand for WorkflowCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        workflow(app, arg)
    }
}

const WORKFLOW_CONFIRM_INSTRUCTION_PREFIX: &str = "[codewhale.workflow-confirm.v1]";
const WORKFLOW_OBJECTIVE_MAX_CHARS: usize = 1_000;
const WORKFLOW_DISPLAY_MAX_CHARS: usize = 160;

#[derive(serde::Serialize, serde::Deserialize)]
struct WorkflowDraftEnvelope {
    id: String,
    objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
}

fn truncate_workflow_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let mut truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn workflow_display(prefix: &str, objective: Option<&str>) -> String {
    let display = objective.map_or_else(
        || format!("{prefix}current work"),
        |objective| {
            // Transcript rows are single-line summaries. The full (separately
            // bounded) objective remains in the typed envelope reviewed by the
            // model and later used by confirmation.
            let objective = objective.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("{prefix}{objective}")
        },
    );
    truncate_workflow_text(&display, WORKFLOW_DISPLAY_MAX_CHARS)
}

fn workflow_draft_instruction(id: &str, objective: Option<&str>) -> String {
    let envelope = WorkflowDraftEnvelope {
        id: id.to_string(),
        objective: objective.map(ToOwned::to_owned),
        source_path: None,
    };
    let encoded =
        serde_json::to_string(&envelope).expect("workflow draft envelope is serializable");
    format!(
        "{WORKFLOW_DRAFT_INSTRUCTION_PREFIX}{encoded}\n\
         Draft a short, plain-language Workflow proposal for review. Include the objective, 1–4 \
         phases, estimated workers, and material risks. Do not call tools or execute work. End by \
         asking the user to run `/workflow confirm` to start or revise the objective instead."
    )
}

fn workflow_source_draft_instruction(id: &str, source_path: &str) -> String {
    let envelope = WorkflowDraftEnvelope {
        id: id.to_string(),
        objective: None,
        source_path: Some(source_path.to_string()),
    };
    let encoded = serde_json::to_string(&envelope).expect("workflow source draft is serializable");
    format!(
        "{WORKFLOW_DRAFT_INSTRUCTION_PREFIX}{encoded}\n\
         Review the request to run this checked-in Workflow source. State the exact path, explain \
         that its saved definition will run as-is, identify material risks, and ask the user to \
         run `/workflow confirm` to start it. Do not call tools, inspect files, or execute work."
    )
}

fn workflow_confirm_instruction(draft: &WorkflowDraftEnvelope) -> String {
    let encoded = serde_json::to_string(draft).expect("workflow confirmation is serializable");
    if let Some(source_path) = draft.source_path.as_deref() {
        return format!(
            "{WORKFLOW_CONFIRM_INSTRUCTION_PREFIX}{encoded}\n\
             The user explicitly confirmed the saved Workflow source at {source_path:?}. Call the \
             canonical `workflow` tool with `source_path` set to that exact relative path. Run the \
             saved definition as-is; do not rewrite or replace it. Keep the existing approval, \
             budget, cancellation, and receipt behavior."
        );
    }
    format!(
        "{WORKFLOW_CONFIRM_INSTRUCTION_PREFIX}{encoded}\n\
         The user explicitly confirmed the Workflow proposal from the preceding review turn. \
         Execute that reviewed plan through the canonical `workflow` tool now. Keep the existing \
         approval, budget, scheduling, cancellation, and receipt behavior; do not teach or restate \
         the tool schema."
    )
}

fn envelope_from_instruction(prefix: &str, instruction: &str) -> Option<WorkflowDraftEnvelope> {
    let first_line = instruction.lines().next()?;
    let encoded = first_line.strip_prefix(prefix)?;
    serde_json::from_str(encoded).ok()
}

fn user_instruction(message: &codewhale_models::Message) -> Option<&str> {
    if message.role != "user" {
        return None;
    }
    message.content.iter().find_map(|block| match block {
        ContentBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

enum PendingWorkflowDraft {
    Ready(WorkflowDraftEnvelope),
    NoDraft,
    NotReviewed,
}

/// Find the latest reviewed, not-yet-confirmed Workflow draft.
///
/// The rule set is deliberately small (the old five-branch scan kept biting
/// users: any ordinary message between the draft and `/workflow confirm`
/// cancelled the draft, so a confirm attempt after a failed first attempt
/// could never succeed):
///
/// 1. An explicit confirm always picks the *latest* unconfirmed draft.
/// 2. Ordinary user/assistant messages in between are ignored — only a newer
///    draft instruction supersedes an older one (the scan is latest-first).
/// 3. The draft must have at least one assistant text reply after it; a draft
///    whose review turn is still in flight cannot be confirmed yet.
/// 4. A draft that was already confirmed (a confirm instruction exists for its
///    id) is never picked again.
fn pending_workflow_draft(app: &App) -> PendingWorkflowDraft {
    let mut resolved = std::collections::HashSet::new();

    for queued in app.queued_messages.iter().chain(app.queued_draft.iter()) {
        if let Some(instruction) = queued.skill_instruction.as_deref()
            && let Some(envelope) =
                envelope_from_instruction(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX, instruction)
        {
            resolved.insert(envelope.id);
        }
    }

    let mut reviewed = false;
    for message in app.api_messages.iter().rev() {
        let Some(instruction) = user_instruction(message) else {
            // Assistant text after the draft is the review reply. Tool-call
            // turns and other assistant messages do not count as review.
            if message.role == "assistant"
                && message.content.iter().any(|block| {
                    matches!(block, ContentBlock::Text { text, .. } if !text.trim().is_empty())
                })
            {
                reviewed = true;
            }
            continue;
        };
        if let Some(envelope) =
            envelope_from_instruction(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX, instruction)
        {
            resolved.insert(envelope.id);
            continue;
        }
        if let Some(envelope) =
            envelope_from_instruction(WORKFLOW_DRAFT_INSTRUCTION_PREFIX, instruction)
            && !resolved.contains(&envelope.id)
        {
            return if reviewed {
                PendingWorkflowDraft::Ready(envelope)
            } else {
                PendingWorkflowDraft::NotReviewed
            };
        }
        // Ordinary user requests between draft and confirm are ignored: the
        // explicit `/workflow confirm` means the current draft, not a new one.
    }
    PendingWorkflowDraft::NoDraft
}

pub fn workflow(app: &mut App, arg: Option<&str>) -> CommandResult {
    let arg = arg.map(str::trim).filter(|value| !value.is_empty());

    if let Some(action) = parse_workflow_control_action(app, arg) {
        return action;
    }

    let id = uuid::Uuid::new_v4().to_string();
    let objective =
        arg.map(|objective| truncate_workflow_text(objective, WORKFLOW_OBJECTIVE_MAX_CHARS));
    let display = workflow_display("Workflow draft: ", objective.as_deref());
    CommandResult::with_message_and_action(
        "Drafting a workflow for review. Nothing will run until /workflow confirm.",
        AppAction::WorkflowInstruction {
            display,
            instruction: workflow_draft_instruction(&id, objective.as_deref()),
        },
    )
}

/// Host-side `status` / `runs` / `cancel` / `settings`: read the run journal and
/// live run state directly and answer without a model turn, so a status
/// check is free and a cancel lands even while the model is busy.
fn parse_workflow_control_action(app: &App, arg: Option<&str>) -> Option<CommandResult> {
    let arg = arg?;
    let (verb, rest) = match arg.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (arg, ""),
    };
    match verb {
        "confirm" if rest.is_empty() => Some(match pending_workflow_draft(app) {
            PendingWorkflowDraft::Ready(draft) => {
                let display = match draft.source_path.as_deref() {
                    Some(path) => workflow_display("Workflow file confirmed: ", Some(path)),
                    None => workflow_display("Workflow confirmed: ", draft.objective.as_deref()),
                };
                CommandResult::with_message_and_action(
                    "Workflow confirmed. Starting the reviewed plan...",
                    AppAction::WorkflowInstruction {
                        display,
                        instruction: workflow_confirm_instruction(&draft),
                    },
                )
            }
            PendingWorkflowDraft::NotReviewed => CommandResult::error(
                "The current Workflow draft has not been reviewed yet. Wait for the proposal turn to finish, then run /workflow confirm.",
            ),
            PendingWorkflowDraft::NoDraft => CommandResult::error(
                "There is no Workflow draft to confirm. Use /workflow <objective> to draft one first.",
            ),
        }),
        "status" | "runs" | "list" | "inspect" => Some(workflow_status(app, rest)),
        "cancel" | "stop" | "abort" => Some(workflow_cancel(app, rest)),
        "settings" | "config" => Some(crate::commands::config_main_host::workflow_settings(app)),
        "help" | "?" => Some(CommandResult::message(WORKFLOW_USAGE)),
        // Saved definitions use the same two-turn review/confirm gate as a
        // conversational Workflow. The draft turn has an empty tool catalog;
        // only the later confirmation can launch the exact checked-in path.
        "run" if !rest.is_empty() && !rest.contains(char::is_whitespace) => {
            let path = std::path::Path::new(rest);
            let unsafe_path = path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                });
            if unsafe_path || rest.chars().count() > WORKFLOW_OBJECTIVE_MAX_CHARS {
                return Some(CommandResult::error(
                    "Workflow source must be a bounded relative path inside this workspace.",
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let display = workflow_display("Workflow file draft: ", Some(rest));
            Some(CommandResult::with_message_and_action(
                "Reviewing the saved workflow. Nothing will run until /workflow confirm.",
                AppAction::WorkflowInstruction {
                    display,
                    instruction: workflow_source_draft_instruction(&id, rest),
                },
            ))
        }
        _ => None,
    }
}

const WORKFLOW_USAGE: &str =
    "/workflow <objective> — draft a Workflow for review (does not execute)
/workflow — draft a Workflow for the current work
/workflow run <path> — review a saved Workflow before it can run
/workflow confirm — explicitly start the latest reviewed draft
/workflow status [run_id] — runs known to this workspace (no model turn)
/workflow cancel [run_id] — stop a running workflow (no model turn)
/workflow settings — the effective [workflow] configuration
/workflows — the live run dashboard (opens in the TUI)";

fn describe_run(
    line: &codewhale_engine::tools::workflow::HostWorkflowRunLine,
    now_ms: u64,
) -> String {
    let elapsed = line
        .completed_at_ms
        .unwrap_or(now_ms)
        .saturating_sub(line.started_at_ms)
        / 1000;
    let mut text = format!(
        "{}  {}  {}  {}  {} children",
        line.run_id,
        line.status,
        line.label,
        codewhale_command_contract::elapsed::format_elapsed_secs(elapsed),
        line.child_count
    );
    if let Some(progress) = line.last_progress.as_deref() {
        text.push_str("  ·  ");
        text.push_str(progress);
    }
    if let Some(error) = line.error.as_deref() {
        text.push_str("  ·  ");
        text.push_str(error);
    }
    text
}

fn workflow_status(app: &App, run_id: &str) -> CommandResult {
    let runs = codewhale_engine::tools::workflow::host_workflow_runs(
        &app.workspace,
        app.current_session_id.as_deref(),
    );
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    if !run_id.is_empty() {
        return match runs.iter().find(|line| line.run_id == run_id) {
            Some(line) => CommandResult::message(describe_run(line, now_ms)),
            None => CommandResult::error(format!(
                "Unknown workflow run '{run_id}'. /workflow status lists the runs this workspace knows."
            )),
        };
    }
    if runs.is_empty() {
        return CommandResult::message(
            "No workflow runs in this workspace yet. /workflow <objective> starts one.",
        );
    }
    let active = runs.iter().filter(|line| line.active).count();
    let mut lines = vec![format!(
        "{} workflow run{} · {active} active",
        runs.len(),
        if runs.len() == 1 { "" } else { "s" }
    )];
    // Newest first; the journal can hold every run the workspace ever made.
    for line in runs.iter().rev().take(20) {
        lines.push(describe_run(line, now_ms));
    }
    if runs.len() > 20 {
        lines.push(format!(
            "… {} older runs in .codewhale/workflow-runs.jsonl",
            runs.len() - 20
        ));
    }
    CommandResult::message(lines.join("\n"))
}

fn workflow_cancel(app: &App, run_id: &str) -> CommandResult {
    if run_id.contains(char::is_whitespace) {
        return CommandResult::error("Usage: /workflow cancel [run_id]");
    }
    let target = if run_id.is_empty() {
        let running: Vec<_> = codewhale_engine::tools::workflow::host_workflow_runs(
            &app.workspace,
            app.current_session_id.as_deref(),
        )
        .into_iter()
        .filter(|line| line.active)
        .collect();
        match running.as_slice() {
            [] => return CommandResult::message("No workflow is active."),
            [only] => only.run_id.clone(),
            many => {
                let ids: Vec<&str> = many.iter().map(|line| line.run_id.as_str()).collect();
                return CommandResult::error(format!(
                    "{} workflows are active; name one: {}",
                    many.len(),
                    ids.join(", ")
                ));
            }
        }
    } else {
        run_id.to_string()
    };
    match codewhale_engine::tools::workflow::host_cancel_workflow(
        &app.workspace,
        &target,
        app.current_session_id.as_deref(),
    ) {
        Ok(line) => CommandResult::message(format!(
            "Workflow {} {} · {}",
            line.run_id, line.status, line.label
        )),
        Err(reason) => CommandResult::error(reason),
    }
}

/// `/workflows` — the live **run** dashboard (Grok-build parity for the
/// observation surface). Bare opens the manager view; the host control verbs
/// (`status`, `cancel`, `settings`) still answer inline from the run journal
/// so muscle memory from the old `/workflows` alias keeps working — none of
/// them spend a model turn. Anything else is redirected to `/workflow`, the
/// only surface that carries orchestration authority: `/workflows` observes
/// and cancels, it never launches.
pub(in crate::commands) const WORKFLOWS_COMMAND_INFO: CommandInfo = CommandInfo {
    name: "workflows",
    aliases: &[],
    usage: "/workflows",
    description_id: MessageId::CmdWorkflowsDescription,
};

pub(in crate::commands) struct WorkflowsCmd;

impl RegisterCommand for WorkflowsCmd {
    fn info() -> &'static CommandInfo {
        &WORKFLOWS_COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        workflows(app, arg)
    }
}

pub fn workflows(app: &mut App, arg: Option<&str>) -> CommandResult {
    let arg = arg.map(str::trim).filter(|value| !value.is_empty());
    let Some(arg) = arg else {
        return CommandResult::action(AppAction::OpenWorkflowsManager);
    };
    let (verb, rest) = match arg.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (arg, ""),
    };
    match verb {
        "status" | "runs" | "list" | "inspect" => workflow_status(app, rest),
        "cancel" | "stop" | "abort" => workflow_cancel(app, rest),
        "settings" | "config" => crate::commands::config_main_host::workflow_settings(app),
        "help" | "?" => CommandResult::message(WORKFLOWS_USAGE),
        _ => CommandResult::error(
            "/workflows observes runs — it never launches one. Use /workflow <objective> to run one, or bare /workflow to orchestrate the current work.",
        ),
    }
}

const WORKFLOWS_USAGE: &str = "/workflows — open the live run dashboard (no model turn)
/workflows status [run_id] — the same listing as text
/workflows cancel [run_id] — stop a running workflow (no model turn)
/workflows settings — the effective [workflow] configuration";

/// `/auto` is the third orchestration choice: work with Auto-Review.
/// Host-only alias for the existing permission posture — no new runtime (#5439).
pub(in crate::commands) const AUTO_COMMAND_INFO: CommandInfo = CommandInfo {
    name: "auto",
    aliases: &[],
    usage: "/auto",
    description_id: MessageId::CmdAutoDescription,
};

pub(in crate::commands) struct AutoCmd;

impl RegisterCommand for AutoCmd {
    fn info() -> &'static CommandInfo {
        &AUTO_COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        auto(app, arg)
    }
}

pub fn auto(app: &mut App, arg: Option<&str>) -> CommandResult {
    if arg.map(str::trim).is_some_and(|value| !value.is_empty()) {
        return CommandResult::error("Usage: /auto");
    }
    if let Err(reason) = app.apply_auto_review_posture() {
        return CommandResult::error(reason);
    }

    let mut message = app.tr(MessageId::AutoReceiptOn).into_owned();
    if app.mode == AppMode::Plan {
        message.push(' ');
        message.push_str(app.tr(MessageId::AutoReceiptPlanNote).as_ref());
    }
    CommandResult::message(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;
    use std::path::PathBuf;

    use crate::tui::app::TuiOptions;

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &codewhale_runtime::config::Config::default())
    }

    #[test]
    fn auto_sets_auto_review_and_explains_the_trio() {
        let mut app = test_app();
        app.ui_locale = codewhale_localization::Locale::En;
        app.set_agent_approval_posture(ApprovalMode::Suggest);

        let result = auto(&mut app, None);
        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(app.approval_mode, ApprovalMode::Auto);
        let text = result.message.as_deref().unwrap();
        assert!(text.contains("Auto-Review"));
        assert!(text.contains("/goal"));
        assert!(text.contains("/workflow"));
        assert!(result.action.is_none());
    }

    #[test]
    fn auto_rejects_arguments() {
        let mut app = test_app();
        let result = auto(&mut app, Some("now"));
        assert!(result.is_error);
        assert!(result.message.as_deref().unwrap().contains("Usage: /auto"));
    }

    #[test]
    fn ordinary_workflow_is_a_toolless_review_turn() {
        let mut app = test_app();
        let result = workflow(&mut app, Some("audit provider error handling"));
        assert!(!result.is_error);
        let Some(AppAction::WorkflowInstruction {
            display,
            instruction,
        }) = result.action
        else {
            panic!("expected WorkflowInstruction action");
        };
        assert!(display.contains("audit provider error handling"));
        assert!(!display.contains("workflow` tool"));
        assert!(
            display.len() < 80,
            "visible transcript line must stay compact"
        );
        assert!(instruction.starts_with(WORKFLOW_DRAFT_INSTRUCTION_PREFIX));
        assert!(
            instruction.len() < 700,
            "draft instruction grew into a manual"
        );
        assert!(!instruction.contains("parallel()"));
        assert!(!instruction.contains("responseSchema"));

        let queued = crate::tui::app::QueuedMessage::new(display, Some(instruction));
        assert_eq!(
            crate::tui::ui::allowed_tools_for_message(None, &queued),
            Some(Vec::new()),
            "the host, not model compliance, must prevent same-turn execution"
        );
    }

    #[test]
    fn oversized_multibyte_objective_is_bounded_once_and_confirmed_exactly() {
        let mut app = test_app();
        let original = "鲸".repeat(WORKFLOW_OBJECTIVE_MAX_CHARS + 50);
        let drafted = workflow(&mut app, Some(&original));
        let Some(AppAction::WorkflowInstruction {
            display,
            instruction,
        }) = drafted.action
        else {
            panic!("expected WorkflowInstruction action");
        };

        assert!(display.chars().count() <= WORKFLOW_DISPLAY_MAX_CHARS);
        assert!(!display.contains('\n'));
        let draft = envelope_from_instruction(WORKFLOW_DRAFT_INSTRUCTION_PREFIX, &instruction)
            .expect("typed workflow draft envelope");
        let objective = draft.objective.as_deref().expect("bounded objective");
        assert_eq!(objective.chars().count(), WORKFLOW_OBJECTIVE_MAX_CHARS);
        assert!(objective.ends_with('…'));

        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: instruction,
                cache_control: None,
            }],
        });
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Reviewed bounded objective and proposed phases.".to_string(),
                cache_control: None,
            }],
        });

        let confirmed = workflow(&mut app, Some("confirm"));
        let Some(AppAction::WorkflowInstruction {
            display,
            instruction,
        }) = confirmed.action
        else {
            panic!("expected confirmed WorkflowInstruction action");
        };
        assert!(display.chars().count() <= WORKFLOW_DISPLAY_MAX_CHARS);
        let confirmed =
            envelope_from_instruction(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX, &instruction)
                .expect("typed workflow confirmation envelope");
        assert_eq!(confirmed.objective.as_deref(), Some(objective));
    }

    #[test]
    fn confirm_survives_ordinary_messages_between_draft_and_confirm() {
        let mut app = test_app();
        let drafted = workflow(&mut app, Some("audit provider error handling"));
        let Some(AppAction::WorkflowInstruction {
            display,
            instruction,
        }) = drafted.action
        else {
            panic!("expected WorkflowInstruction action");
        };
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!("{instruction}\n\n---\n\nUser request: {display}"),
                cache_control: None,
            }],
        });
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Objective, phases, workers, and risks. Run /workflow confirm to start."
                    .to_string(),
                cache_control: None,
            }],
        });

        // The user types or the model replies again before confirming — the
        // explicit confirm must still find the reviewed draft (regression: the
        // old supersede rule cancelled the draft on any ordinary message and
        // made repeated confirm attempts impossible).
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hmm it won't let me confirm it lol".to_string(),
                cache_control: None,
            }],
        });
        let confirmed = workflow(&mut app, Some("confirm"));
        assert!(!confirmed.is_error, "{:?}", confirmed.message);
        let Some(AppAction::WorkflowInstruction { instruction, .. }) = confirmed.action else {
            panic!("expected confirmed WorkflowInstruction action");
        };
        assert!(instruction.starts_with(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX));

        // A newer DRAFT still supersedes the older one.
        let redrafted = workflow(&mut app, Some("fix the confirm flow"));
        let Some(AppAction::WorkflowInstruction {
            instruction: redraft_instruction,
            display: redraft_display,
        }) = redrafted.action
        else {
            panic!("expected WorkflowInstruction action");
        };
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!("{redraft_instruction}\n\n---\n\nUser request: {redraft_display}"),
                cache_control: None,
            }],
        });
        let still_unreviewed = workflow(&mut app, Some("confirm"));
        assert!(
            still_unreviewed.is_error
                && still_unreviewed
                    .message
                    .as_deref()
                    .unwrap_or_default()
                    .contains("not been reviewed"),
            "a newer, unreviewed draft must be confirmed only after its review: {:?}",
            still_unreviewed.message
        );
    }

    #[test]
    fn workflow_confirmation_is_a_separate_tool_enabled_turn() {
        let mut app = test_app();
        let drafted = workflow(&mut app, Some("audit provider error handling"));
        let Some(AppAction::WorkflowInstruction {
            display,
            instruction,
        }) = drafted.action
        else {
            panic!("expected WorkflowInstruction action");
        };
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!("{instruction}\n\n---\n\nUser request: {display}"),
                cache_control: None,
            }],
        });
        assert!(
            workflow(&mut app, Some("confirm")).is_error,
            "a failed or unfinished draft turn is not a reviewed plan"
        );
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Objective, phases, workers, and risks. Run /workflow confirm to start."
                    .to_string(),
                cache_control: None,
            }],
        });

        let confirmed = workflow(&mut app, Some("confirm"));
        assert!(!confirmed.is_error);
        let Some(AppAction::WorkflowInstruction {
            display,
            instruction,
        }) = confirmed.action
        else {
            panic!("expected confirmed WorkflowInstruction action");
        };
        assert!(instruction.starts_with(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX));
        let queued = crate::tui::app::QueuedMessage::new(display, Some(instruction.clone()));
        assert_eq!(
            crate::tui::ui::allowed_tools_for_message(None, &queued),
            None,
            "only the later explicit confirmation restores the normal catalog"
        );

        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: instruction,
                cache_control: None,
            }],
        });
        let replay = workflow(&mut app, Some("confirm"));
        assert!(replay.is_error, "one draft may not be confirmed twice");
    }

    #[test]
    fn workflow_status_and_cancel_answer_from_the_host_without_a_model_turn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = test_app();
        app.workspace = dir.path().to_path_buf();
        app.current_session_id = Some("workflow-host-test-session".to_string());

        // Nothing has run in this workspace: status is a plain answer, and it
        // must not create the run journal just to say so.
        let result = workflow(&mut app, Some("status"));
        assert!(!result.is_error);
        assert!(
            result.action.is_none(),
            "status must not send a model message"
        );
        assert!(
            result
                .message
                .as_deref()
                .unwrap()
                .contains("No workflow runs")
        );
        assert!(!dir.path().join(".codewhale/workflow-runs.jsonl").exists());

        let result = workflow(&mut app, Some("status wf_missing"));
        assert!(result.is_error);
        assert!(result.action.is_none());

        // A seeded run is listed and described from host state.
        codewhale_engine::tools::workflow::structcopy_test_seed_run(
            dir.path(),
            "workflow_seed",
            app.current_session_id
                .as_deref()
                .expect("test session identity"),
        );
        let result = workflow(&mut app, Some("runs"));
        let text = result.message.unwrap();
        assert!(text.contains("workflow_seed"), "{text}");
        assert!(text.contains("queued"), "{text}");
        assert!(result.action.is_none());

        // Cancel with one active run needs no id and never asks the model.
        // The seeded record has no live controller (no VM ran); cancel still
        // marks the journal cancelled with an honest nothing-live receipt.
        let result = workflow(&mut app, Some("cancel"));
        assert!(result.action.is_none());
        assert!(!result.is_error, "{:?}", result.message);
        let text = result.message.as_deref().unwrap();
        assert!(text.contains("workflow_seed"), "{text}");
        assert!(text.contains("cancelled"), "{text}");
        let after = codewhale_engine::tools::workflow::host_workflow_runs(
            &app.workspace,
            app.current_session_id.as_deref(),
        );
        assert_eq!(
            after
                .iter()
                .find(|line| line.run_id == "workflow_seed")
                .map(|line| line.status),
            Some("cancelled")
        );

        let result = workflow(&mut app, Some("cancel with spaces"));
        assert!(result.is_error);

        let result = workflow(&mut app, Some("help"));
        assert!(result.message.unwrap().contains("/workflow status"));

        // `/workflow run <path>` now drafts a tool-less review. Only a later
        // explicit confirmation may launch the exact saved source.
        let result = workflow(&mut app, Some("run workflows/tiny.workflow.js"));
        let Some(AppAction::WorkflowInstruction {
            display,
            instruction,
        }) = result.action
        else {
            panic!("expected WorkflowInstruction action");
        };
        assert!(display.contains("workflows/tiny.workflow.js"), "{display}");
        let draft = envelope_from_instruction(WORKFLOW_DRAFT_INSTRUCTION_PREFIX, &instruction)
            .expect("typed saved-workflow draft");
        assert_eq!(
            draft.source_path.as_deref(),
            Some("workflows/tiny.workflow.js")
        );
        let queued = crate::tui::app::QueuedMessage::new(display, Some(instruction.clone()));
        assert_eq!(
            crate::tui::ui::allowed_tools_for_message(None, &queued),
            Some(Vec::new())
        );

        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: instruction,
                cache_control: None,
            }],
        });
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Saved Workflow path and risks reviewed.".to_string(),
                cache_control: None,
            }],
        });
        let confirmed = workflow(&mut app, Some("confirm"));
        let Some(AppAction::WorkflowInstruction { instruction, .. }) = confirmed.action else {
            panic!("expected confirmed saved Workflow action");
        };
        assert!(instruction.contains("`source_path`"), "{instruction}");
        assert!(
            instruction.contains("workflows/tiny.workflow.js"),
            "{instruction}"
        );

        assert!(workflow(&mut app, Some("run ../outside.workflow.js")).is_error);
        assert!(workflow(&mut app, Some("run /tmp/outside.workflow.js")).is_error);
    }

    #[test]
    fn workflows_opens_the_run_dashboard_and_keeps_host_verbs_free() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = test_app();
        app.workspace = dir.path().to_path_buf();
        app.current_session_id = Some("workflow-host-test-session".to_string());

        // Bare `/workflows` opens the dashboard: a host action, never a
        // model turn — observation carries no orchestration authority.
        let result = workflows(&mut app, None);
        assert!(!result.is_error);
        assert!(matches!(
            result.action,
            Some(AppAction::OpenWorkflowsManager)
        ));

        // Host control verbs still answer inline (the old alias surface).
        let result = workflows(&mut app, Some("status"));
        assert!(!result.is_error);
        assert!(
            result.action.is_none(),
            "status must not send a model message"
        );
        assert!(
            result
                .message
                .as_deref()
                .unwrap()
                .contains("No workflow runs")
        );

        // Orchestration attempts are redirected to /workflow, the only
        // surface that carries launch authority.
        let result = workflows(&mut app, Some("audit provider errors"));
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert!(result.message.unwrap().contains("never launches"));
    }

    #[test]
    fn workflow_settings_explains_the_session_table() {
        let mut app = test_app();
        app.workflow_config.automatic = false;
        app.workflow_config.require_approval_for_writes = false;
        app.goal_max_continuations = 25;
        let result = workflow(&mut app, Some("settings"));
        assert!(result.action.is_none());
        let text = result.message.unwrap();
        assert!(text.contains("automatic = off"), "{text}");
        assert!(text.contains("require_approval_for_writes = off"), "{text}");
        assert!(text.contains("max_continuations = 25"), "{text}");
    }

    #[test]
    fn workflow_settings_and_tool_share_a_refreshed_session_table() {
        use codewhale_engine::tools::spec::{ApprovalRequirement, ToolContext, ToolSpec};
        use codewhale_engine::tools::subagent::{SubAgentRuntime, new_shared_subagent_manager};
        use codewhale_engine::tools::workflow::WorkflowTool;
        use serde_json::json;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = test_app();
        app.workspace = dir.path().to_path_buf();

        let mut table = app.workflow_config.clone();
        table.automatic = false;
        table.require_approval_for_writes = false;
        table.auto_start_read_only = false;
        codewhale_engine::tools::workflow::set_session_workflow_config(
            &app.workspace,
            table.clone(),
        );
        app.workflow_config = table;

        let result = workflow(&mut app, Some("settings"));
        assert!(result.action.is_none());
        let text = result.message.unwrap();
        assert!(text.contains("automatic = off"), "{text}");
        assert!(text.contains("require_approval_for_writes = off"), "{text}");
        assert!(text.contains("auto_start_read_only = off"), "{text}");

        let ctx = ToolContext::new(dir.path().to_path_buf());
        let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 2);
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = codewhale_engine::client::CodewhaleClient::new(
            &codewhale_runtime::config::Config::default()
                .with_legacy_root(Some("test-key".to_string()), None),
        )
        .expect("stub client");
        let mut runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx,
            true,
            None,
            manager.clone(),
        );
        // Stale snapshot: product defaults still require write approval.
        runtime.api_config = Some(std::sync::Arc::new(
            codewhale_runtime::config::Config::default(),
        ));
        let tool = WorkflowTool::new(manager, runtime);

        let write_plan = json!({
            "action": "start",
            "plan": {
                "goal": "write freely",
                "risk": "writes",
                "children": [{ "prompt": "edit", "type": "implementer" }]
            }
        });
        let read_only = json!({
            "action": "start",
            "plan": {
                "goal": "scout crates",
                "risk": "read_only",
                "children": [{ "prompt": "look", "type": "explore" }]
            }
        });
        assert_eq!(
            tool.approval_requirement_for(&write_plan),
            ApprovalRequirement::Auto,
            "refreshed require_approval_for_writes = false must win over the stale runtime snapshot"
        );
        assert_eq!(
            tool.approval_requirement_for(&read_only),
            ApprovalRequirement::Required,
            "refreshed auto_start_read_only = false must still ask"
        );
    }
}
