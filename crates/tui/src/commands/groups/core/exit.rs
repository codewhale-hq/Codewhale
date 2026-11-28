//! `/exit` command.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{CommandResult, CoreAction};

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "exit",
    aliases: &["quit", "q", "tuichu"],
    usage: "/exit",
    description_key: "cmd_exit_description",
};

pub(super) struct ExitCmd;

impl<A: From<CoreAction>> RegisterCommand<CommandResult<A>> for ExitCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Pure(|_| CommandResult::action(CoreAction::Quit.into()))
    }
}
