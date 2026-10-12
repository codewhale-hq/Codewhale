use super::{CommandResult, transition_blocked_message};

use codewhale_command_contract::facets::CommandSessionLifecycleContext;
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub(in crate::commands) struct BranchCmd;

// ---------------------------------------------------------------------------
// FEAT-023 Phase 4 (D3/D5/D6): portable contextual registration and handler.
// The handler owns parsing, branch order, exact messages, guidance appends,
// and action composition; all concrete host work stays behind the lifecycle
// facet. Missing lifecycle authority fails safely with the exact capability
// error (never a panic).
// ---------------------------------------------------------------------------

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "branch",
    aliases: &[],
    usage: "/branch <entry_id>",
    description_key: "cmd_branch_description",
};

impl ContractRegisterCommand<CommandResult> for BranchCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities:
                codewhale_command_contract::handler::CommandCapabilities::SESSION_LIFECYCLE,
            handler: branch_contextual,
        }
    }
}

pub(in crate::commands) fn branch_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(lifecycle) = parts.lifecycle.as_deref_mut() else {
        return CommandResult::error(
            "Command capability unavailable: session_lifecycle".to_string(),
        );
    };
    branch_portable(lifecycle, arg)
}

pub(in crate::commands) fn branch_portable(
    lifecycle: &mut dyn CommandSessionLifecycleContext,
    arg: Option<&str>,
) -> CommandResult {
    if lifecycle.transition_blocked() {
        return CommandResult::error(transition_blocked_message(
            "branch",
            &lifecycle.transition_blockers(),
        ));
    }
    let Some(entry_id) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
        if let Some(leaf) = lifecycle.branch_current_leaf_hint() {
            return CommandResult::message(format!(
                "Current leaf: {leaf}\nUse `/branch <entry_id>` to move the leaf (history is never rewritten).\nUse `/tree` to list entry ids."
            ));
        }
        return CommandResult::message(
            "Usage: /branch <entry_id>\nMoves the active leaf to an existing entry. Future appends become children of that entry.\nHistory is never rewritten — branching only moves the leaf.\n\nUse `/tree` to see entry ids."
                .to_string(),
        );
    };
    match lifecycle.branch_to(entry_id) {
        Ok(outcome) => CommandResult::with_message_and_action(
            format!(
                "Branched to entry {entry_id} (leaf now {}); journal entries {} (history preserved, leaf moved only)",
                outcome.leaf_display, outcome.journal_entries_before
            ),
            codewhale_command_contract::outcome::SessionAction::SyncSession(outcome.sync),
        ),
        Err(error) => CommandResult::error(error),
    }
}
