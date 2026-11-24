//! `/rlm` command.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::{App, AppAction};
use codewhale_localization::MessageId;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "rlm",
    aliases: &["recursive", "digui"],
    usage: "/rlm [N] <file_or_text>",
    description_id: MessageId::CmdRlmDescription,
};

pub(in crate::commands) struct RlmCmd;

impl RegisterCommand for RlmCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        rlm(app, arg)
    }
}

pub fn rlm(app: &mut App, arg: Option<&str>) -> CommandResult {
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

    let source = if resolves_to_existing_file(app, &target) {
        format!("the workspace file `{target}`")
    } else {
        format!("this text: {target:?}")
    };
    let message = format!(
        "Call the `rlm` tool for this request (load it with `tool_search` if it is not yet in your tool list). Work on {source}: call `open` with `file_path` or `content`, inspect it with `eval`, and finish with `close`."
    );

    CommandResult::with_message_and_action(
        "Loading that into a persistent working context...".to_string(),
        AppAction::SendMessage(message),
    )
}

fn resolves_to_existing_file(app: &App, input: &str) -> bool {
    let path = std::path::Path::new(input);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        app.workspace.join(path)
    };
    candidate.is_file()
}
