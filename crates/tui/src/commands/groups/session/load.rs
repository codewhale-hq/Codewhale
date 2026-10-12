//! `/load` command.

use super::{CommandResult, transition_blocked_message};

use codewhale_command_contract::facets::CommandSessionLifecycleContext;
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub(in crate::commands) struct LoadCmd;

// ---------------------------------------------------------------------------
// FEAT-023 Phase 4 (D3/D5/D6): portable contextual registration and handler.
// The handler owns parsing, branch order, exact messages, guidance appends,
// and action composition; all concrete host work stays behind the lifecycle
// facet. Missing lifecycle authority fails safely with the exact capability
// error (never a panic).
// ---------------------------------------------------------------------------

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "load",
    aliases: &["jiazai"],
    usage: "/load [path]",
    description_key: "cmd_load_description",
};

impl ContractRegisterCommand<CommandResult> for LoadCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities:
                codewhale_command_contract::handler::CommandCapabilities::SESSION_LIFECYCLE,
            handler: load_contextual,
        }
    }
}

pub(in crate::commands) fn load_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(lifecycle) = parts.lifecycle.as_deref_mut() else {
        return CommandResult::error(
            "Command capability unavailable: session_lifecycle".to_string(),
        );
    };
    load_portable(lifecycle, arg)
}

pub(in crate::commands) fn load_portable(
    lifecycle: &mut dyn CommandSessionLifecycleContext,
    arg: Option<&str>,
) -> CommandResult {
    if lifecycle.transition_blocked() {
        return CommandResult::error(transition_blocked_message(
            "load a session",
            &lifecycle.transition_blockers(),
        ));
    }
    let Some(path) = arg.map(str::trim).filter(|p| !p.is_empty()) else {
        return CommandResult::error("Usage: /load <path>".to_string());
    };
    match lifecycle.load_session(path) {
        Ok(load_path) => CommandResult::action(
            codewhale_command_contract::outcome::SessionAction::LoadSession(load_path),
        ),
        Err(error) => CommandResult::error(error),
    }
}
