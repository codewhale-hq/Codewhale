//! `/profile` command.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{CommandResult, CoreAction};

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "profile",
    aliases: &["dangan"],
    usage: "/profile <name>",
    description_key: "cmd_profile_description",
};

pub(super) struct ProfileCmd;

impl<A: From<CoreAction>> RegisterCommand<CommandResult<A>> for ProfileCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::PRESENTATION,
            handler: |contexts, args| {
                let Some(presentation) = contexts.into_parts().presentation else {
                    return CommandResult::error("Command capability unavailable: presentation");
                };
                let Some(profile) = args.map(str::trim).filter(|name| !name.is_empty()) else {
                    return CommandResult::error(
                        presentation
                            .translate("profile_usage", &[])
                            .unwrap_or_else(|err| err),
                    );
                };
                match presentation.translate("profile_switching", &[("name", profile)]) {
                    Ok(message) => CommandResult::with_message_and_action(
                        message,
                        CoreAction::SwitchProfile {
                            profile: profile.to_owned(),
                        }
                        .into(),
                    ),
                    Err(error) => CommandResult::error(error),
                }
            },
        }
    }
}
