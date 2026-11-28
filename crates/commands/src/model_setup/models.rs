//! `/models` command.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{CommandResult, CoreAction};

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "models",
    aliases: &["moxingliebiao"],
    usage: "/models",
    description_key: "cmd_models_description",
};

pub struct ModelsCmd;

impl<A: From<CoreAction>> RegisterCommand<CommandResult<A>> for ModelsCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Pure(|_| CommandResult::action(CoreAction::FetchModels.into()))
    }
}
