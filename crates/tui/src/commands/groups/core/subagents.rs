//! `/subagents` compatibility command.

use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::SubagentRosterDisplay;
use codewhale_command_contract::outcome::{CommandResult, CoreAction};

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "subagents",
    aliases: &["agents", "zhinengti"],
    usage: "/subagents [list]",
    description_key: "cmd_subagents_description",
};

pub(super) struct SubagentsCmd;

impl<A: From<CoreAction>> RegisterCommand<CommandResult<A>> for SubagentsCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION,
            handler: |contexts, arg| {
                let display = match arg.map(str::trim).unwrap_or_default() {
                    "" => SubagentRosterDisplay::Modal,
                    "list" | "roster" | "ls" => SubagentRosterDisplay::Transcript,
                    other => {
                        return CommandResult::error(format!(
                            "Unknown /agents argument {other:?}. Usage: /agents [list]"
                        ));
                    }
                };
                let Some(navigation) = contexts.into_parts().navigation else {
                    return CommandResult::error("Command capability unavailable: navigation");
                };
                navigation.prepare_subagent_roster(display);
                CommandResult::action(CoreAction::ListSubAgents.into())
            },
        }
    }
}
