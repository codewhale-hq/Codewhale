//! `/turn` command — inspect the current or latest completed turn.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{
    CoreNavigationAction as Action, CoreNavigationCommandResult as CommandResult,
};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "turn",
    aliases: &["turns"],
    usage: "/turn inspect",
    description_key: "cmd_turn_inspect_description",
};

pub struct TurnCmd;

impl RegisterCommand<CommandResult> for TurnCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Pure(turn)
    }
}

fn turn(arg: Option<&str>) -> CommandResult {
    let verb = arg.map(str::trim).unwrap_or("");
    if matches!(verb, "inspect" | "i" | "") {
        return CommandResult::action(Action::OpenTurnInspector);
    }
    CommandResult::error(format!(
        "Unknown /turn verb '{verb}'. Use /turn inspect to open the whole-turn inspector."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_turn_inspect_opens_turn_inspector() {
        let result = turn(None);
        assert!(!result.is_error);
        assert_eq!(result.action, Some(Action::OpenTurnInspector));
        assert!(result.message.is_none());
    }

    #[test]
    fn turn_inspect_verb_opens_turn_inspector() {
        let result = turn(Some("inspect"));
        assert!(!result.is_error);
        assert_eq!(result.action, Some(Action::OpenTurnInspector));
    }

    #[test]
    fn unknown_turn_verb_returns_error() {
        let result = turn(Some("bad"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|m| m.contains("/turn inspect"))
        );
        assert!(result.action.is_none());
    }
}
