//! `/new` command — start a fresh saved session from the current TUI state.

use super::{CommandResult, transition_blocked_message};

use codewhale_command_contract::facets::CommandSessionLifecycleContext;
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub(in crate::commands) struct NewCmd;

// ---------------------------------------------------------------------------
// FEAT-023 Phase 4 (D3/D5/D6): portable contextual registration and handler.
// ---------------------------------------------------------------------------

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "new",
    aliases: &[],
    usage: "/new [--force]",
    description_key: "cmd_new_description",
};

impl ContractRegisterCommand<CommandResult> for NewCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities:
                codewhale_command_contract::handler::CommandCapabilities::SESSION_LIFECYCLE,
            handler: new_contextual,
        }
    }
}

pub(in crate::commands) fn new_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(lifecycle) = parts.lifecycle.as_deref_mut() else {
        return CommandResult::error(
            "Command capability unavailable: session_lifecycle".to_string(),
        );
    };
    new_portable(lifecycle, arg)
}

pub(in crate::commands) fn new_portable(
    lifecycle: &mut dyn CommandSessionLifecycleContext,
    arg: Option<&str>,
) -> CommandResult {
    let force = match arg.map(str::trim).filter(|s| !s.is_empty()) {
        None => false,
        Some("--force" | "force") => true,
        Some(other) => {
            return CommandResult::error(format!(
                "Usage: /new [--force]\n\nUnknown argument: {other}"
            ));
        }
    };
    if lifecycle.transition_blocked() {
        // The shared tail names the exits that clear a blocker; `--force` is
        // not one of them, and a user who has just read it will try exactly
        // that. Say what it does here, where the flag exists.
        let mut message =
            transition_blocked_message("start a new session", &lifecycle.transition_blockers());
        message.push_str("\n\n`/new --force` only discards draft or queued input.");
        return CommandResult::error(message);
    }
    match lifecycle.fresh_session(force) {
        Ok(receipt) => CommandResult::with_message_and_action(
            format!(
                "Started new session {} (New Session). Previous sessions remain available via /resume.",
                receipt.truncated_id
            ),
            codewhale_command_contract::outcome::SessionAction::SyncSession(receipt.sync),
        ),
        Err(error) => CommandResult::error(error),
    }
}
