//! `/transcript` command.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{
    CoreNavigationAction as Action, CoreNavigationCommandResult as CommandResult,
};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "transcript",
    aliases: &[],
    usage: "/transcript",
    // Reuse the keybinding description so the command palette and shortcut
    // catalog describe the same live-transcript surface in every locale.
    description_key: "kb_live_transcript",
};

pub struct TranscriptCmd;

impl RegisterCommand<CommandResult> for TranscriptCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Pure(|_| CommandResult::action(Action::OpenLiveTranscript))
    }
}
