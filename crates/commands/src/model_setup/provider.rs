//! Provider switching: flip between DeepSeek, hosted providers, and self-hosted
//! OpenAI-compatible DeepSeek V4 servers at runtime.
//!
//! `/provider` with no args opens the picker modal (#52). `/provider <name>`
//! keeps the v0.6.6 CLI form for muscle-memory + scripted use.

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::model_setup::ProviderAction;
use codewhale_command_contract::model_setup::resolve_action;
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "provider",
    aliases: &[],
    usage: "/provider [setup [name]|name [model]]",
    description_key: "cmd_provider_description",
};

pub struct ProviderCmd;

impl<A: for<'a> TryFrom<ProviderAction<'a>, Error = String>> RegisterCommand<CommandResult<A>>
    for ProviderCmd
{
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::PROVIDER_ROUTES,
            handler: provider,
        }
    }
}

fn provider<A: for<'a> TryFrom<ProviderAction<'a>, Error = String>>(
    mut contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult<A> {
    let mut words = arg.unwrap_or_default().split_whitespace();
    let Some(name) = words.next() else {
        return resolve_action(ProviderAction::OpenPicker, None);
    };
    let model = words.next();
    if name.eq_ignore_ascii_case("setup") {
        return resolve_action(ProviderAction::Setup(model), None);
    }
    let Some(routes) = contexts.take_provider_routes() else {
        return CommandResult::error("Command capability unavailable: provider routes");
    };
    if name.eq_ignore_ascii_case("fallback") {
        return provider_fallback(routes, model);
    }
    match routes.resolve_switch(name, model) {
        Ok(selection) if selection.already_active => {
            CommandResult::message(format!("Already on provider: {}", selection.id.0))
        }
        Ok(selection) => resolve_action(
            ProviderAction::Switch {
                provider: selection.id,
                model: selection.model,
            },
            None,
        ),
        Err(reason) => CommandResult::error(reason),
    }
}

fn provider_fallback<A: for<'a> TryFrom<ProviderAction<'a>, Error = String>>(
    routes: &dyn codewhale_command_contract::model_setup::CommandProviderRoutesContext,
    subcommand: Option<&str>,
) -> CommandResult<A> {
    match subcommand {
        Some("reset") => {
            let Some((_, primary, _)) = routes.fallback().entries.into_iter().next() else {
                return CommandResult::message(
                    "No fallback providers configured. Add `fallback_providers` to your config.",
                );
            };
            let message = format!("Fallback chain reset to primary provider: {}.", primary.0);
            resolve_action(
                ProviderAction::Switch {
                    provider: primary,
                    model: None,
                },
                Some(message),
            )
        }
        Some(other) => CommandResult::error(format!(
            "Unknown fallback command '{other}'. Usage: /provider fallback [reset]"
        )),
        None => {
            let fallback = routes.fallback();
            let entries = fallback.entries;
            if entries.is_empty() {
                return CommandResult::message(
                    "No fallback providers configured. Add `fallback_providers` to your config.",
                );
            }

            let mut lines = vec![
                format!("Current provider: {}", fallback.current),
                "Fallback chain:".to_string(),
            ];
            for (index, provider, is_current) in entries {
                let role = if index == 0 { "primary" } else { "fallback" };
                let marker = if is_current { " <- current" } else { "" };
                lines.push(format!("  [{index}] {} ({role}){marker}", provider.0));
            }
            if let Some(reason) = fallback.last_reason {
                lines.push(format!("Last fallback: {reason}"));
            }
            lines.push("Use `/provider fallback reset` to return to the primary provider.".into());
            CommandResult::message(lines.join("\n"))
        }
    }
}
