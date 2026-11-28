//! `/rlm` command.

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::WorkspaceTarget;
use codewhale_command_contract::outcome::{CommandResult, CoreAction};

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "rlm",
    aliases: &["recursive", "digui"],
    usage: "/rlm [N] <file_or_text>",
    description_key: "cmd_rlm_description",
};

pub(super) struct RlmCmd;

impl<A: From<CoreAction>> RegisterCommand<CommandResult<A>> for RlmCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKSPACE,
            handler: rlm,
        }
    }
}

fn rlm<A: From<CoreAction>>(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult<A> {
    let (_legacy_depth, target) = match super::util::parse_depth_prefixed_arg(arg, 1) {
        Ok(parsed) => parsed,
        Err(message) => return CommandResult::error(message),
    };
    let target = match target {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => {
            return CommandResult::error(
                "Usage: /rlm [N] <file_or_text>\n\n\
                 Works through a large file or block of text in a context that \
                 stays loaded for the rest of the session."
                    .to_string(),
            );
        }
    };

    let Some(workspace) = contexts.into_parts().workspace else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    let candidate = workspace.workspace().join(&target);
    let source = if workspace.inspect_workspace_target(&candidate) == WorkspaceTarget::File {
        format!("the workspace file `{target}`")
    } else {
        format!("this text: {target:?}")
    };
    let message = format!(
        "Call the `rlm` tool for this request (load it with `tool_search` if it is not yet in your tool list). Work on {source}: call `open` with `file_path` or `content`, inspect it with `eval`, and finish with `close`."
    );

    CommandResult::with_message_and_action(
        "Loading that into a persistent working context...".to_string(),
        CoreAction::SendMessage(message).into(),
    )
}
