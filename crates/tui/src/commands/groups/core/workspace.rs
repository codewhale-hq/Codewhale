//! `/workspace` command.

use codewhale_command_contract::facets::{CommandPresentationContext, CommandWorkspaceContext};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::WorkspaceTarget;
use codewhale_command_contract::outcome::{
    CoreNavigationAction as Action, CoreNavigationCommandResult as CommandResult,
};
use std::path::PathBuf;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "workspace",
    aliases: &["cwd"],
    usage: "/workspace [path|worktrees]",
    description_key: "cmd_workspace_description",
};

pub struct WorkspaceCmd;

impl RegisterCommand<CommandResult> for WorkspaceCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKSPACE.union(CommandCapabilities::PRESENTATION),
            handler: workspace,
        }
    }
}

fn workspace(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(workspace) = parts.workspace else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    match arg.map(str::trim).filter(|arg| !arg.is_empty()) {
        Some("worktrees" | "worktree" | "wt") => CommandResult::action(Action::OpenWorktreeManager),
        other => {
            workspace_switch(workspace, presentation, other).unwrap_or_else(CommandResult::error)
        }
    }
}

fn workspace_switch(
    workspace: &dyn CommandWorkspaceContext,
    presentation: &dyn CommandPresentationContext,
    arg: Option<&str>,
) -> Result<CommandResult, String> {
    let Some(raw_path) = arg.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(CommandResult::message(presentation.translate(
            "workspace_current",
            &[("path", &workspace.workspace().display().to_string())],
        )?));
    };
    let expanded = expand_workspace_path(workspace, presentation, raw_path)?;
    let candidate = if expanded.is_absolute() {
        expanded
    } else {
        workspace.workspace().join(expanded)
    };
    let target = match workspace.inspect_workspace_target(&candidate) {
        WorkspaceTarget::Missing => {
            return Ok(CommandResult::error(presentation.translate(
                "workspace_not_found",
                &[("path", &candidate.display().to_string())],
            )?));
        }
        WorkspaceTarget::NotDirectory => {
            return Ok(CommandResult::error(presentation.translate(
                "workspace_not_directory",
                &[("path", &candidate.display().to_string())],
            )?));
        }
        WorkspaceTarget::Directory {
            canonical_or_candidate,
        } => canonical_or_candidate,
    };
    let message = presentation.translate(
        "workspace_switching",
        &[("path", &target.display().to_string())],
    )?;
    Ok(CommandResult::with_message_and_action(
        message,
        Action::SwitchWorkspace { workspace: target },
    ))
}

fn expand_workspace_path(
    workspace: &dyn CommandWorkspaceContext,
    presentation: &dyn CommandPresentationContext,
    path: &str,
) -> Result<PathBuf, String> {
    if path == "~" || path.starts_with("~/") {
        let Some(home) = workspace.effective_home_dir() else {
            return Err(presentation.translate("workspace_home_unresolved", &[])?);
        };
        return Ok(match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => home,
        });
    }
    Ok(PathBuf::from(path))
}
