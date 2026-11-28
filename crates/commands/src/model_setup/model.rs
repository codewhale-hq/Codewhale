//! `/model` command.

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::model_setup::resolve_action;
use codewhale_command_contract::model_setup::{ModelAction, ModelSelection};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "model",
    aliases: &["moxing", "route"],
    usage: "/model [name|refresh|router]",
    description_key: "cmd_model_description",
};

pub struct ModelCmd;

impl<A: for<'a> TryFrom<ModelAction<'a>, Error = String>> RegisterCommand<CommandResult<A>>
    for ModelCmd
{
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::MODEL_SELECTION,
            handler: model,
        }
    }
}

fn model<A: for<'a> TryFrom<ModelAction<'a>, Error = String>>(
    mut contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult<A> {
    let Some(name) = arg else {
        return resolve_action(ModelAction::OpenPicker, None);
    };
    let trimmed = name.trim();
    let (head, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    if head.eq_ignore_ascii_case("router") {
        return resolve_action(ModelAction::Router(rest), None);
    }
    if trimmed.eq_ignore_ascii_case("refresh") {
        return resolve_action(ModelAction::RefreshCatalog, None);
    }
    let Some(model) = contexts.take_model_selection() else {
        return CommandResult::error("Command capability unavailable: model");
    };
    if name.eq_ignore_ascii_case("save-default") {
        return CommandResult::message(model.save_session_route_default());
    }
    let selection = if trimmed.eq_ignore_ascii_case("auto") {
        ModelSelection::Auto
    } else {
        ModelSelection::Named(name)
    };
    match model.select_session_model(selection) {
        Ok(receipt) => resolve_action(
            ModelAction::UpdateCompaction(receipt.compaction),
            Some(receipt.message),
        ),
        Err(reason) => CommandResult::error(reason),
    }
}
