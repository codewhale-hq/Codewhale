//! Portable session command group. Host observations, registration and action
//! execution live outside this complete source closure.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::SessionCommandResult as CommandResult;

pub(in crate::commands) mod branch;
pub(in crate::commands) mod compact;
pub(in crate::commands) mod export;
pub(in crate::commands) mod fork;
pub(in crate::commands) mod load;
pub(in crate::commands) mod new;
pub(in crate::commands) mod purge;
pub(in crate::commands) mod relay;
pub(in crate::commands) mod remote_control;
pub(in crate::commands) mod remote_env;
pub(in crate::commands) mod rename;
pub(in crate::commands) mod resume;
pub(in crate::commands) mod save;
pub(in crate::commands) mod sessions;
pub(in crate::commands) mod structcopy;
pub(in crate::commands) mod title;
pub(in crate::commands) mod tree;
// Documentation-only child, retained with the established source topology.
#[allow(clippy::module_inception)]
mod session;

pub(in crate::commands) const MAX_TITLE_LEN: usize = 100;

/// The one way the lifecycle commands report a closed transition gate: the
/// verb, the named blockers, and the exits that actually clear them.
pub(in crate::commands) fn transition_blocked_message(verb: &str, blockers: &[String]) -> String {
    let mut message = format!("Cannot {verb} while runtime work is active");
    if blockers.is_empty() {
        message.push('.');
    } else {
        message.push(':');
        for blocker in blockers {
            message.push_str("\n  • ");
            message.push_str(blocker);
        }
    }
    // `/jobs cancel-all` only kills shell processes and answers "No running
    // commands to cancel." for everything else, so it must not read as the
    // exit for a turn, dispatch, compaction, or cleanup blocker. Name both
    // exits with the scope each one actually clears.
    message.push_str(
        "\n\nWait for the work to finish, or stop what you can: \
         Ctrl+C stops a running turn, and /jobs cancel-all cancels running shell jobs.",
    );
    message
}

/// Promote the no-action structcopy result to the group's action vocabulary.
/// The leaf itself retains its narrower, impossible-action return type.
pub(in crate::commands) struct StructcopyRegistration;
impl RegisterCommand<CommandResult> for StructcopyRegistration {
    fn info() -> &'static CommandInfo {
        structcopy::StructcopyCmd::info()
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: structcopy::CAPABILITIES,
            handler: |contexts, args| {
                let result = structcopy::execute_structcopy(contexts, args);
                CommandResult {
                    message: result.message,
                    action: result.action.map(|impossible| match impossible {}),
                    is_error: result.is_error,
                }
            },
        }
    }
}

/// Complete group inventory, used by the host and independent compilation proof.
pub fn portable_handlers() -> [(&'static CommandInfo, CommandHandler<CommandResult>); 17] {
    [
        (rename::RenameCmd::info(), rename::RenameCmd::handler()),
        (title::TitleCmd::info(), title::TitleCmd::handler()),
        (save::SaveCmd::info(), save::SaveCmd::handler()),
        (fork::ForkCmd::info(), fork::ForkCmd::handler()),
        (new::NewCmd::info(), new::NewCmd::handler()),
        (
            sessions::SessionsCmd::info(),
            sessions::SessionsCmd::handler(),
        ),
        (load::LoadCmd::info(), load::LoadCmd::handler()),
        (resume::ResumeCmd::info(), resume::ResumeCmd::handler()),
        (tree::TreeCmd::info(), tree::TreeCmd::handler()),
        (branch::BranchCmd::info(), branch::BranchCmd::handler()),
        (compact::CompactCmd::info(), compact::CompactCmd::handler()),
        (purge::PurgeCmd::info(), purge::PurgeCmd::handler()),
        (relay::RelayCmd::info(), relay::RelayCmd::handler()),
        (
            remote_control::RemoteControlCmd::info(),
            remote_control::RemoteControlCmd::handler(),
        ),
        (
            remote_env::RemoteEnvCmd::info(),
            remote_env::RemoteEnvCmd::handler(),
        ),
        (export::ExportCmd::info(), export::ExportCmd::handler()),
        (
            StructcopyRegistration::info(),
            StructcopyRegistration::handler(),
        ),
    ]
}

#[cfg(test)]
mod control_test_support;
#[cfg(test)]
mod lifecycle_portable_tests;
#[cfg(test)]
mod lifecycle_test_support;
