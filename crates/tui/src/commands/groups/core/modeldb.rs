//! `/modeldb` command — browse the factual model reference database.
//!
//! Opens a read-only pager listing each catalog model's stated attributes:
//! provider + kind, the model id verbatim, context window, max output,
//! modality (text vs multimodal), and price. This is labels only — it never
//! selects, routes, or tiers a model (#3205, #2300). Attributes the catalog
//! does not state render as `unknown`, never guessed.

use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "modeldb",
    aliases: &["model-reference", "modelref"],
    usage: "/modeldb",
    description_key: "cmd_model_db_description",
};

pub(super) struct ModelDbCmd;

impl<A> RegisterCommand<CommandResult<A>> for ModelDbCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION,
            handler: |contexts, _| {
                let Some(navigation) = contexts.into_parts().navigation else {
                    return CommandResult::error("Command capability unavailable: navigation");
                };
                navigation.open_model_reference();
                CommandResult::ok()
            },
        }
    }
}
