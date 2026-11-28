//! `/copy` command — copy the last completed assistant response.

use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::ResponseCopyDelivery;
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
            capabilities: CommandCapabilities::RESPONSE_COPY
                .union(CommandCapabilities::PRESENTATION),
            handler: copy,
        }
    }
}

fn copy(mut contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let Some(response_copy) = contexts.take_response_copy() else {
        return CommandResult::error("Command capability unavailable: response copy");
    };
    let Some(presentation) = contexts.into_parts().presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let result = match response_copy.copy_completed_response() {
        Some(delivery) => delivery_message(presentation, delivery),
        None => presentation.translate("cmd_copy_no_output", &[]),
    };
    let result = match result {
        Ok(message) => CommandResult::message(message),
        Err(message) => CommandResult::error(message),
    };
    if let Some(message) = &result.message {
        response_copy.show_copy_receipt(message, result.is_error);
    }
    result
}

fn delivery_message(
    presentation: &dyn CommandPresentationContext,
    delivery: ResponseCopyDelivery,
) -> Result<String, String> {
    let path = delivery.recovery_path.map(|p| p.display().to_string());
    let error = delivery.clipboard_result.err();
    match (
        error.as_deref(),
        path.as_deref(),
        delivery.requires_terminal_paste,
    ) {
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
