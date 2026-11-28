//! Queue commands: queue list/edit/drop/clear

use codewhale_command_contract::facets::{CommandComposerContext, CommandPresentationContext};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "queue",
    aliases: &["queued"],
    usage: "/queue [list|send <n>|edit <n>|drop <n>|clear]",
    description_key: "cmd_queue_description",
};

pub(super) struct QueueCmd;

impl<A> RegisterCommand<CommandResult<A>> for QueueCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::COMPOSER.union(CommandCapabilities::PRESENTATION),
            handler: queue,
        }
    }
}

fn queue<A>(mut contexts: CommandContexts<'_>, args: Option<&str>) -> CommandResult<A> {
    let Some(composer) = contexts.take_composer() else {
        return CommandResult::error("Command capability unavailable: composer");
    };
    let Some(presentation) = contexts.into_parts().presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    match execute_queue(composer, presentation, args.unwrap_or("").trim()) {
        Ok(message) => CommandResult::message(message),
        Err(error) => CommandResult::error(error),
    }
}

fn execute_queue(
    composer: &mut dyn CommandComposerContext,
    presentation: &dyn CommandPresentationContext,
    arg: &str,
) -> Result<String, String> {
    let translate = |key| presentation.translate(key, &[]);
    if arg.is_empty() || arg.eq_ignore_ascii_case("list") {
        let queue = composer.queue_previews();
        let mut lines = Vec::new();
        if let Some(draft) = queue.editing {
            lines.push("Editing queued message:".to_string());
            lines.push(format!("- {draft}"));
        }
        if queue.waiting.is_empty() {
            return if lines.is_empty() {
                translate("cmd_queue_no_messages")
            } else {
                Ok(lines.join("\n"))
            };
        }
        lines.push(presentation.translate(
            "cmd_queue_list_header",
            &[("count", &queue.waiting.len().to_string())],
        )?);
        for (index, preview) in queue.waiting.iter().enumerate() {
            lines.push(format!("{}. {preview}", index + 1));
        }
        lines.push(translate("cmd_queue_tip")?);
        return Ok(lines.join("\n"));
    }
    let mut words = arg.split_whitespace();
    let action = words.next().unwrap_or_default().to_lowercase();
    match action.as_str() {
        "clear" => {
            let cleared = translate("cmd_queue_cleared")?;
            let empty = translate("cmd_queue_already_empty")?;
            Ok(if composer.clear_queued() {
                cleared
            } else {
                empty
            })
        }
        "edit" | "drop" | "remove" | "rm" => {
            let editing = action == "edit";
            if editing && composer.editing_queue() {
                return Err(translate("cmd_queue_already_editing")?);
            }
            let Some(raw) = words.next() else {
                return Err(translate("cmd_queue_missing_index")?);
            };
            let Ok(number) = raw.parse::<usize>() else {
                return Err(translate("cmd_queue_index_positive")?);
            };
            let Some(index) = number.checked_sub(1) else {
                return Err(translate("cmd_queue_index_min")?);
            };
            let number = number.to_string();
            let replacements = &[("index", number.as_str())];
            let message_key = if editing {
                "cmd_queue_editing_message"
            } else {
                "cmd_queue_dropped"
            };
            let message = presentation.translate(message_key, replacements)?;
            let found = if editing {
                let status = presentation.translate("cmd_queue_editing_status", replacements)?;
                composer.edit_queued(index, status)
            } else {
                composer.drop_queued(index)
            };
            if found {
                Ok(message)
            } else {
                Err(translate("cmd_queue_not_found")?)
            }
        }
        _ => Err(translate("cmd_queue_usage")?),
    }
}
