//! `/translate` command.

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CoreNavigationCommandResult as CommandResult;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "translate",
    aliases: &["translation", "transale"],
    usage: "/translate",
    description_key: "cmd_translate_description",
};

pub struct TranslateCmd;

impl RegisterCommand<CommandResult> for TranslateCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::CONFIG.union(CommandCapabilities::PRESENTATION),
            handler: translate,
        }
    }
}

fn translate(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(config) = parts.config else {
        return CommandResult::error("Command capability unavailable: config");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let enabled = !config.translation_enabled();
    let key = if enabled {
        "cmd_translate_on"
    } else {
        "cmd_translate_off"
    };
    let message = match presentation.translate(key, &[]) {
        Ok(message) => message,
        Err(error) => return CommandResult::error(error),
    };
    config.set_translation_enabled(enabled);
    CommandResult::message(message)
}
