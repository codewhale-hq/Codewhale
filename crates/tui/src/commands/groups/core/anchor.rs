//! Anchor command: keep critical facts across compaction.
//!
//! Unlike `/note` (active lookup), anchors are passive. They are automatically
//! re-injected into context after compaction in trusted workspaces. Use anchors to
//! preserve invariants like "This API's status field is unreliable" or
//! ".ssh/ must never be touched".

use codewhale_command_contract::facets::CommandWorkspaceContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::AnchorWriteError;
use codewhale_command_contract::outcome::CoreNavigationCommandResult as CommandResult;

const USAGE: &str = "/anchor <text> | /anchor list | /anchor remove <n>";

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "anchor",
    aliases: &["maodian"],
    usage: USAGE,
    description_key: "cmd_anchor_description",
};

pub struct AnchorCmd;

impl RegisterCommand<CommandResult> for AnchorCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKSPACE,
            handler: anchor,
        }
    }
}

fn anchor(contexts: CommandContexts<'_>, content: Option<&str>) -> CommandResult {
    let input = match content {
        Some(content) => content.trim(),
        None => return CommandResult::error(format!("Usage: {USAGE}")),
    };
    if input.is_empty() {
        return CommandResult::error(format!("Usage: {USAGE}"));
    }
    let Some(workspace) = contexts.into_parts().workspace else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    if input.eq_ignore_ascii_case("list") {
        return list_anchors(workspace);
    }
    if let Some(rest) = input
        .strip_prefix("remove ")
        .or_else(|| input.strip_prefix("rm "))
        .or_else(|| input.strip_prefix("delete "))
    {
        return remove_anchor(workspace, rest.trim());
    }
    let parts: &[&[u8]] = &[b"\n---\n", input.as_bytes(), b"\n"];
    match workspace.append_anchor_bytes(parts) {
        Ok(path) => {
            CommandResult::message(format!("Anchor pinned.\nStored in: {}", path.display()))
        }
        Err(AnchorWriteError::Open(error)) => {
            CommandResult::error(format!("Failed to open anchors file: {error}"))
        }
        Err(AnchorWriteError::Write(error)) => {
            CommandResult::error(format!("Failed to write anchor: {error}"))
        }
    }
}

fn read_anchors(workspace: &dyn CommandWorkspaceContext) -> Result<Vec<String>, String> {
    let Some(content) = workspace
        .read_anchor_document()
        .map_err(|error| format!("Failed to read anchors file: {error}"))?
    else {
        return Ok(Vec::new());
    };
    Ok(content
        .split("\n---\n")
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect())
}

fn list_anchors(workspace: &dyn CommandWorkspaceContext) -> CommandResult {
    let anchors = match read_anchors(workspace) {
        Ok(anchors) => anchors,
        Err(error) => return CommandResult::error(error),
    };
    if anchors.is_empty() {
        return CommandResult::message(
            "No anchors set. Use /anchor <text> to pin a fact that survives compaction.",
        );
    }
    let mut output = format!("Pinned anchors ({} total):\n", anchors.len());
    for (index, anchor) in anchors.iter().enumerate() {
        output.push_str(&format!("\n  {}. {}", index + 1, anchor));
    }
    output.push_str("\n\nUse /anchor remove <n> to remove an anchor.");
    CommandResult::message(output)
}

fn remove_anchor(workspace: &mut dyn CommandWorkspaceContext, index_str: &str) -> CommandResult {
    let index: usize = match index_str.parse() {
        Ok(index) if index >= 1 => index,
        _ => {
            return CommandResult::error(
                "Invalid index. Use /anchor list to see anchor numbers, then /anchor remove <n>.",
            );
        }
    };
    let mut anchors = match read_anchors(workspace) {
        Ok(anchors) => anchors,
        Err(error) => return CommandResult::error(error),
    };
    if index > anchors.len() {
        return CommandResult::error(format!(
            "Anchor #{index} does not exist. You have {} anchor(s). Use /anchor list to see them.",
            anchors.len()
        ));
    }
    let removed = anchors.remove(index - 1);
    if let Err(error) = workspace.replace_anchor_document(anchors.join("\n---\n").as_bytes()) {
        return CommandResult::error(format!("Failed to write anchors file: {error}"));
    }
    CommandResult::message(format!("Removed anchor #{index}: {removed}"))
}
