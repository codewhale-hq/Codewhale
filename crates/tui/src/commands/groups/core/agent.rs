//! `/agent` command.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{CommandResult, CoreAction};

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "agent",
    aliases: &["daili"],
    usage: "/agent [N] <task>",
    description_key: "cmd_agent_description",
};

pub(super) struct AgentCmd;

impl<A: From<CoreAction>> RegisterCommand<CommandResult<A>> for AgentCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Pure(agent)
    }
}

fn agent<A: From<CoreAction>>(arg: Option<&str>) -> CommandResult<A> {
    if let Some(action) = parse_agent_control_action(arg) {
        if action.action == "cancel" {
            return CommandResult::with_message_and_action(
                format!("Cancelling agent {}...", action.agent_id),
                CoreAction::CancelSubAgent {
                    agent_id: action.agent_id,
                }
                .into(),
            );
        }
        let message = format!(
            "Call `agent` with action `{}`, agent_id `{}`, then summarize the returned status for the user. Do not start a new agent.",
            action.action, action.agent_id
        );
        return CommandResult::with_message_and_action(
            format!("Agent {} requested for {}.", action.action, action.agent_id),
            CoreAction::SendMessage(message).into(),
        );
    }

    let (max_depth, task) = match super::util::parse_depth_prefixed_arg(arg, 1) {
        Ok(parsed) => parsed,
        Err(message) => return CommandResult::error(message),
    };
    let task = match task {
        Some(task) if !task.trim().is_empty() => task.trim().to_string(),
        _ => {
            return CommandResult::error(
                "Usage: /agent [N] <task>\n\n\
                 Opens a persistent sub-agent session with recursive agent depth N (0-3, default 1).",
            );
        }
    };
    CommandResult::with_message_and_action(
        format!("Opening persistent sub-agent at depth {max_depth}..."),
        CoreAction::LaunchSubAgent { task, max_depth }.into(),
    )
}

struct AgentControlAction {
    action: &'static str,
    agent_id: String,
}

fn parse_agent_control_action(arg: Option<&str>) -> Option<AgentControlAction> {
    let arg = arg?.trim();
    let (action, rest) = arg.split_once(char::is_whitespace)?;
    let action = match action {
        "status" | "inspect" => "status",
        "peek" | "progress" => "peek",
        "cancel" | "stop" | "abort" => "cancel",
        _ => return None,
    };
    let agent_id = rest.trim();
    if agent_id.is_empty() || agent_id.contains(char::is_whitespace) {
        return None;
    }
    Some(AgentControlAction {
        action,
        agent_id: agent_id.to_string(),
    })
}
