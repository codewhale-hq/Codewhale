//! `/copy` command — copy the last completed assistant response.

use codewhale_command_contract::facets::{CommandPresentationContext, CommandSessionExportContext};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CoreNavigationCommandResult as CommandResult;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "copy",
    aliases: &[],
    usage: "/copy",
    description_key: "cmd_copy_description",
};

pub struct CopyCmd;

impl RegisterCommand<CommandResult> for CopyCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION
                .union(CommandCapabilities::SESSION_EXPORT)
                .union(CommandCapabilities::PRESENTATION),
            handler: copy,
        }
    }
}

fn copy(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(navigation) = parts.navigation else {
        return CommandResult::error("Command capability unavailable: navigation");
    };
    let Some(export) = parts.export else {
        return CommandResult::error("Command capability unavailable: export");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let result = match navigation.completed_assistant_output() {
        Some(content) => deliver(export, presentation, &content),
        None => presentation.translate("cmd_copy_no_output", &[]),
    };
    let result = match result {
        Ok(message) => CommandResult::message(message),
        Err(message) => CommandResult::error(message),
    };
    if let Some(message) = &result.message {
        navigation.show_copy_receipt(message, result.is_error);
    }
    result
}

fn deliver(
    export: &dyn CommandSessionExportContext,
    presentation: &dyn CommandPresentationContext,
    content: &str,
) -> Result<String, String> {
    let terminal = export.clipboard_requires_terminal_paste();
    let recovery = export.write_recovery_copy(content);
    let path = recovery.as_ref().map(|path| path.display().to_string());
    let error = export.write_clipboard(content).err();
    match (error.as_deref(), path.as_deref(), terminal) {
        (None, Some(path), true) => presentation.translate("cmd_copy_queued", &[("path", path)]),
        (None, None, true) => presentation.translate("cmd_copy_queued_no_backup", &[]),
        (None, Some(path), false) => presentation.translate("cmd_copy_success", &[("path", path)]),
        (None, None, false) => presentation.translate("cmd_copy_success_no_backup", &[]),
        (Some(error), Some(path), _) => {
            Err(presentation.translate("cmd_copy_failed", &[("error", error), ("path", path)])?)
        }
        (Some(error), None, _) => {
            Err(presentation.translate("cmd_copy_failed_no_backup", &[("error", error)])?)
        }
    }
}
