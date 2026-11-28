//! `/pin` command: toggle the host terminal window into the always-on-top
//! mini window (Windows). Keyboard-friendly twin of the right-click menu
//! entry, for mouse-less users.

use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "pin",
    aliases: &["mini", "window-pin"],
    usage: "/pin",
    description_key: "cmd_pin_description",
};

pub(super) struct PinCmd;

impl<A> RegisterCommand<CommandResult<A>> for PinCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION,
            handler: |contexts, arg| {
                match arg.map(str::trim).filter(|value| !value.is_empty()) {
                    Some("help" | "?" | "--help" | "-h") => {
                        return CommandResult::message(format!("Usage: {}", COMMAND_INFO.usage));
                    }
                    Some(_) => {
                        return CommandResult::error(format!("Usage: {}", COMMAND_INFO.usage));
                    }
                    None => {}
                }
                let Some(navigation) = contexts.into_parts().navigation else {
                    return CommandResult::error("Command capability unavailable: navigation");
                };
                navigation.toggle_window_pin();
                CommandResult::ok()
            },
        }
    }
}
