use super::SharedCommandHost;
use crate::tui::app::{App, AppAction};
use codewhale_command_contract::config::CommandWorkflowSummary;
use codewhale_command_contract::workflow::*;
use codewhale_config::AppMode;
use codewhale_models::ContentBlock;

pub(super) struct WorkflowAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl From<codewhale_commands::workflow::OpenWorkflowsManager> for AppAction {
    fn from(_: codewhale_commands::workflow::OpenWorkflowsManager) -> Self {
        Self::OpenWorkflowsManager
    }
}

impl From<codewhale_commands::workflow::WorkflowInstruction> for AppAction {
    fn from(intent: codewhale_commands::workflow::WorkflowInstruction) -> Self {
        let (display, instruction) = intent.into_parts();
        Self::WorkflowInstruction {
            display,
            instruction,
        }
    }
}

impl CommandWorkflowReviewContext for WorkflowAdapter<'_> {
    fn pending_draft(&self) -> PendingWorkflowDraft {
        pending_workflow_draft(&self.host.app.borrow())
    }
}

impl CommandWorkflowContext for WorkflowAdapter<'_> {
    fn runs(&self) -> Vec<HostWorkflowRunLine> {
        let app = self.host.app.borrow();
        codewhale_engine::tools::workflow::host_workflow_runs(
            &app.workspace,
            app.current_session_id.as_deref(),
        )
    }

    fn cancel(&mut self, run_id: &str) -> Result<HostWorkflowRunLine, String> {
        let app = self.host.app.borrow();
        codewhale_engine::tools::workflow::host_cancel_workflow(
            &app.workspace,
            run_id,
            app.current_session_id.as_deref(),
        )
    }
    fn settings(&self) -> CommandWorkflowSummary {
        crate::commands::config_main_host::workflow_summary(&self.host.app.borrow())
    }
}

impl CommandAutoReviewContext for WorkflowAdapter<'_> {
    fn enable_auto_review(&mut self) -> Result<AutoReviewReceipt, String> {
        let mut app = self.host.app.borrow_mut();
        app.apply_auto_review_posture()?;
        Ok(AutoReviewReceipt {
            plan_mode: app.mode == AppMode::Plan,
            broad_allows_paused: app.has_suspended_broad_allows(),
        })
    }
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
