use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;
use codewhale_command_contract::workflow::*;

pub struct WorkflowInstruction {
    display: String,
    instruction: String,
}

impl WorkflowInstruction {
    pub fn into_parts(self) -> (String, String) {
        (self.display, self.instruction)
    }
}

pub struct OpenWorkflowsManager;

pub struct WorkflowCmd;
impl<A: From<WorkflowInstruction>> RegisterCommand<CommandResult<A>> for WorkflowCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "workflow",
            aliases: &["wf"],
            usage: "/workflow [objective|confirm|run <path>|status [run_id]|cancel [run_id]|settings]",
            description_key: "cmd_workflow_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKFLOW_CONTROL
                .union(CommandCapabilities::WORKFLOW_REVIEW),
            handler: |mut ctx, arg| {
                let Some(host) = ctx.take_workflow() else {
                    return CommandResult::error("Command capability unavailable: workflow");
                };
                let Some(review) = ctx.take_workflow_review() else {
                    return CommandResult::error("Command capability unavailable: workflow review");
                };
                workflow(host, review, arg)
            },
        }
    }
}

pub struct WorkflowsCmd;
impl<A: From<OpenWorkflowsManager>> RegisterCommand<CommandResult<A>> for WorkflowsCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "workflows",
            aliases: &[],
            usage: "/workflows",
            description_key: "cmd_workflows_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKFLOW_CONTROL,
            handler: |mut ctx, arg| {
                let Some(host) = ctx.take_workflow() else {
                    return CommandResult::error("Command capability unavailable: workflow");
                };
                workflows(host, arg)
            },
        }
    }
}

pub struct AutoCmd;
impl<A> RegisterCommand<CommandResult<A>> for AutoCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "auto",
            aliases: &["zidong"],
            usage: "/auto",
            description_key: "cmd_auto_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::AUTO_REVIEW.union(CommandCapabilities::PRESENTATION),
            handler: |mut ctx, arg| {
                let Some(host) = ctx.take_auto_review() else {
                    return CommandResult::error("Command capability unavailable: auto_review");
                };
                let Some(presentation) = ctx.into_parts().presentation else {
                    return CommandResult::error("Command capability unavailable: presentation");
                };
                auto(host, presentation, arg)
            },
        }
    }
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

fn workflow<A: From<WorkflowInstruction>>(
    host: &mut dyn CommandWorkflowContext,
    review: &dyn CommandWorkflowReviewContext,
    arg: Option<&str>,
) -> CommandResult<A> {
    let arg = arg.map(str::trim).filter(|value| !value.is_empty());

    if let Some(action) = parse_workflow_control_action(host, review, arg) {
        return action;
    }

    let id = uuid::Uuid::new_v4().to_string();
    let objective =
        arg.map(|objective| truncate_workflow_text(objective, WORKFLOW_OBJECTIVE_MAX_CHARS));
    let display = workflow_display("Workflow draft: ", objective.as_deref());
    CommandResult::with_message_and_action(
        "Drafting a workflow for review. Nothing will run until /workflow confirm.",
        WorkflowInstruction {
            display,
            instruction: workflow_draft_instruction(&id, objective.as_deref()),
        }
        .into(),
    )
}

fn parse_workflow_control_action<A: From<WorkflowInstruction>>(
    host: &mut dyn CommandWorkflowContext,
    review: &dyn CommandWorkflowReviewContext,
    arg: Option<&str>,
) -> Option<CommandResult<A>> {
    let arg = arg?;
    let (verb, rest) = split_verb(arg);
    match verb {
        "confirm" if rest.is_empty() => Some(match review.pending_draft() {
            PendingWorkflowDraft::Ready(draft) => {
                let display = match draft.source_path.as_deref() {
                    Some(path) => workflow_display("Workflow file confirmed: ", Some(path)),
                    None => workflow_display("Workflow confirmed: ", draft.objective.as_deref()),
                };
                CommandResult::with_message_and_action(
                    "Workflow confirmed. Starting the reviewed plan...",
                    WorkflowInstruction {
                        display,
                        instruction: workflow_confirm_instruction(&draft),
                    }
                    .into(),
                )
            }
            PendingWorkflowDraft::NotReviewed => CommandResult::error(
                "The current Workflow draft has not been reviewed yet. Wait for the proposal turn to finish, then run /workflow confirm.",
            ),
            PendingWorkflowDraft::NoDraft => CommandResult::error(
                "There is no Workflow draft to confirm. Use /workflow <objective> to draft one first.",
            ),
        }),
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
                WorkflowInstruction {
                    display,
                    instruction: workflow_source_draft_instruction(&id, rest),
                }
                .into(),
            ))
        }
        _ => workflow_control(host, verb, rest, WORKFLOW_USAGE),
    }
}

fn split_verb(arg: &str) -> (&str, &str) {
    match arg.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (arg, ""),
    }
}

fn workflow_control<A>(
    host: &mut dyn CommandWorkflowContext,
    verb: &str,
    rest: &str,
    usage: &str,
) -> Option<CommandResult<A>> {
    match verb {
        "status" | "runs" | "list" | "inspect" => Some(workflow_status(host, rest)),
        "cancel" | "stop" | "abort" => Some(workflow_cancel(host, rest)),
        "settings" | "config" => Some(CommandResult::message(render_workflow_summary(
            host.settings(),
        ))),
        "help" | "?" => Some(CommandResult::message(usage)),
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

fn describe_run(line: &HostWorkflowRunLine, now_ms: u64) -> String {
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

fn workflow_status<A>(host: &mut dyn CommandWorkflowContext, run_id: &str) -> CommandResult<A> {
    let runs = host.runs();
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

fn workflow_cancel<A>(host: &mut dyn CommandWorkflowContext, run_id: &str) -> CommandResult<A> {
    if run_id.contains(char::is_whitespace) {
        return CommandResult::error("Usage: /workflow cancel [run_id]");
    }
    let target = if run_id.is_empty() {
        let running: Vec<_> = host.runs().into_iter().filter(|line| line.active).collect();
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
    match host.cancel(&target) {
        Ok(line) => CommandResult::message(format!(
            "Workflow {} {} · {}",
            line.run_id, line.status, line.label
        )),
        Err(reason) => CommandResult::error(reason),
    }
}

fn workflows<A: From<OpenWorkflowsManager>>(
    host: &mut dyn CommandWorkflowContext,
    arg: Option<&str>,
) -> CommandResult<A> {
    let arg = arg.map(str::trim).filter(|value| !value.is_empty());
    let Some(arg) = arg else {
        return CommandResult::action(OpenWorkflowsManager.into());
    };
    let (verb, rest) = split_verb(arg);
    workflow_control(host, verb, rest, WORKFLOWS_USAGE).unwrap_or_else(|| CommandResult::error(
        "/workflows observes runs — it never launches one. Use /workflow <objective> to run one, or bare /workflow to orchestrate the current work.",
    ))
}

const WORKFLOWS_USAGE: &str = "/workflows — open the live run dashboard (no model turn)
/workflows status [run_id] — the same listing as text
/workflows cancel [run_id] — stop a running workflow (no model turn)
/workflows settings — the effective [workflow] configuration";

fn auto<A>(
    host: &mut dyn CommandAutoReviewContext,
    presentation: &dyn codewhale_command_contract::facets::CommandPresentationContext,
    arg: Option<&str>,
) -> CommandResult<A> {
    if arg.map(str::trim).is_some_and(|value| !value.is_empty()) {
        return CommandResult::error("Usage: /auto");
    }
    let translations = || -> Result<_, String> {
        Ok((
            presentation.translate("auto_receipt_on", &[])?,
            presentation.translate("auto_receipt_plan_note", &[])?,
            presentation.translate("auto_receipt_broad_allows_paused", &[])?,
        ))
    };
    let (on, plan, paused) = match translations() {
        Ok(messages) => messages,
        Err(reason) => return CommandResult::error(reason),
    };
    let receipt = match host.enable_auto_review() {
        Ok(receipt) => receipt,
        Err(reason) => return CommandResult::error(reason),
    };
    let messages = [
        Some(on),
        receipt.plan_mode.then_some(plan),
        receipt.broad_allows_paused.then_some(paused),
    ];
    CommandResult::message(messages.into_iter().flatten().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_keeps_posture_when_a_receipt_translation_is_unavailable() {
        struct Host;
        impl CommandAutoReviewContext for Host {
            fn enable_auto_review(&mut self) -> Result<AutoReviewReceipt, String> {
                panic!("mutated before checking receipts")
            }
        }
        impl codewhale_command_contract::facets::CommandPresentationContext for Host {
            fn translate(&self, key: &str, _: &[(&str, &str)]) -> Result<String, String> {
                match key {
                    "auto_receipt_broad_allows_paused" => Err(key.into()),
                    _ => Ok(key.into()),
                }
            }
        }
        assert!(auto::<()>(&mut Host, &Host, None).is_error);
    }

    #[test]
    fn workflows_cannot_dispatch_confirmation_or_saved_sources() {
        struct EmptyWorkspace;
        impl CommandWorkflowContext for EmptyWorkspace {
            fn runs(&self) -> Vec<HostWorkflowRunLine> {
                panic!("unexpected journal read")
            }
            fn cancel(&mut self, _: &str) -> Result<HostWorkflowRunLine, String> {
                panic!("unexpected cancellation")
            }
            fn settings(&self) -> codewhale_command_contract::config::CommandWorkflowSummary {
                panic!("unexpected settings read")
            }
        }

        let CommandHandler::Contextual { handler, .. } =
            <WorkflowsCmd as RegisterCommand<CommandResult<OpenWorkflowsManager>>>::handler()
        else {
            panic!("expected contextual workflow command");
        };
        for input in ["confirm", "run checked-in.workflow.js"] {
            let mut host = EmptyWorkspace;
            let result = handler(
                codewhale_command_contract::handler::CommandContexts::empty()
                    .with_workflow(&mut host),
                Some(input),
            );
            assert!(result.is_error);
            assert!(result.action.is_none());
        }
    }
}
