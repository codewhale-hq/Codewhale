//! `/agent` command.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::{App, AppAction};
use codewhale_localization::MessageId;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "agent",
    aliases: &["daili"],
    usage: "/agent [N] <task>",
    description_id: MessageId::CmdAgentDescription,
};

pub(in crate::commands) struct AgentCmd;

impl RegisterCommand for AgentCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        agent(app, arg)
    }
}

pub fn agent(_app: &mut App, arg: Option<&str>) -> CommandResult {
    if let Some(action) = parse_agent_control_action(arg) {
        if action.action == "cancel" {
            return CommandResult::with_message_and_action(
                format!("Cancelling agent {}...", action.agent_id),
                AppAction::CancelSubAgent {
                    agent_id: action.agent_id,
                },
            );
        }
        let message = format!(
            "Call `agent` with action `{}`, agent_id `{}`, then summarize the returned status for the user. Do not start a new agent.",
            action.action, action.agent_id
        );
        return CommandResult::with_message_and_action(
            format!("Agent {} requested for {}.", action.action, action.agent_id),
            AppAction::SendMessage(message),
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
    let message = agent_dispatch_brief(&task, max_depth);
    CommandResult::with_message_and_action(
        format!("Opening persistent sub-agent at depth {max_depth}..."),
        AppAction::SendMessage(message),
    )
}

/// The model-facing /agent brief. `handle_read` is deferred on the default
/// catalog, so the brief teaches its activation path (#6747).
pub(crate) fn agent_dispatch_brief(task: &str, max_depth: impl std::fmt::Display) -> String {
    format!(
        "Launch one sub-agent for this task by calling `agent` with name `slash_agent`, `prompt: {task:?}`, and `max_depth: {max_depth}`. Use `handle_read` on the returned transcript_handle if you need more detail ({}). Verify any claimed side effects with `read` before reporting success.",
        crate::tools::handle::HANDLE_READ_ACTIVATION_HINT
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::tui::app::TuiOptions;

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &codewhale_runtime::config::Config::default())
    }

    /// #6747: the /agent brief names only callable tools and teaches the
    /// deferred `handle_read` activation path.
    #[test]
    fn agent_dispatch_brief_names_only_callable_tools() {
        crate::tools::canonical_action_tests::assert_text_names_only_callable_tools(
            "/agent brief",
            &agent_dispatch_brief("inspect the repo", 1),
        );
    }

    #[test]
    fn agent_control_actions_route_to_existing_agent_tool() {
        let mut app = test_app();
        let result = agent(&mut app, Some("peek agent_123"));

        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(message.contains("action `peek`"));
        assert!(message.contains("agent_id `agent_123`"));
        assert!(message.contains("Do not start a new agent"));

        let result = agent(&mut app, Some("cancel agent_123"));
        let Some(AppAction::CancelSubAgent { agent_id }) = result.action else {
            panic!("expected CancelSubAgent action");
        };
        assert_eq!(agent_id, "agent_123");
    }
}
