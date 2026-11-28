use codewhale_command_contract::fleet_lane::{
    ControlOperationHelp, FleetAction, FleetRouteSaveKind, FleetUsageKind,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::model_setup::resolve_action;
use codewhale_command_contract::outcome::CommandResult;

const PRIMARY_VERBS: [&str; 5] = ["members", "setup", "teams", "workers", "help"];

const PRIMARY_USAGE: &str = "/fleet [members|setup|teams|workers|help]";

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fleet",
    aliases: &[],
    usage: PRIMARY_USAGE,
    description_key: "cmd_fleet_description",
};

pub struct FleetCmd;

impl<A: TryFrom<FleetAction>> RegisterCommand<CommandResult<A>> for FleetCmd
where
    A::Error: std::fmt::Display,
{
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::FLEET,
            handler: fleet,
        }
    }
}

fn split_verb(arg: Option<&str>) -> Option<(&str, Option<&str>)> {
    let rest = arg.map(str::trim).filter(|value| !value.is_empty())?;
    Some(match rest.split_once(char::is_whitespace) {
        Some((verb, tail)) => (verb, Some(tail.trim())),
        None => (rest, None),
    })
}

fn help_text(operations: &[ControlOperationHelp]) -> String {
    let mut out = format!(
        "Usage: {PRIMARY_USAGE}\n\n\
         The fleet is who is working right now. The primary verbs cover the daily loop:\n\
         /fleet (or /fleet members) opens the roster — each member's role, model, and access; \
         Enter on a member row opens that member's editor. /fleet setup opens the authoring \
         wizard. /fleet teams (or fleets/saved/manage) switches between named saved teams. \
         /fleet workers (and /subagents) shows sub-agents in the current TUI session only, \
         which is a different set: it does not include durable fleet runs.\n\n\
         Advanced — team model routes (still typed, one level deeper):\n\
         /fleet models lists the selected team's models. \
         /fleet add <provider> <model> [role…] and /fleet remove <provider> <model> edit them.\n\n\
         Advanced — saved routes:\n\
         /fleet save persists the current session route into the selected team's operator. \
         /fleet save-as saves it as a new team.\n\n\
         Durable runs — these act on the durable .codewhale/fleet.jsonl ledger for this \
         workspace, the same records `codewhale fleet` reads and writes. the ledger file, \
         saved rosters, and config tables keep the Fleet name:\n",
    );
    for descriptor in operations {
        out.push_str(&format!(
            "\n  {:<30} {:<6} {}\n      CLI: {}\n",
            descriptor.slash_invocation,
            descriptor.authority,
            descriptor.summary,
            descriptor.cli_invocation
        ));
    }
    out
}

fn fleet<A: TryFrom<FleetAction>>(
    mut contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult<A>
where
    A::Error: std::fmt::Display,
{
    let Some((verb, target)) = split_verb(arg) else {
        return resolve_action(FleetAction::OpenRoster, None);
    };
    match verb {
        "save" | "update" => {
            let Some(fleet_ctx) = contexts.take_fleet() else {
                return CommandResult::error("Command capability unavailable: fleet");
            };
            return CommandResult::message(fleet_ctx.save_route(FleetRouteSaveKind::UpdateFleet));
        }
        "save-as" | "saveas" => {
            let Some(fleet_ctx) = contexts.take_fleet() else {
                return CommandResult::error("Command capability unavailable: fleet");
            };
            return CommandResult::message(
                fleet_ctx.save_route(FleetRouteSaveKind::SaveAsNewFleet),
            );
        }
        _ => {}
    }
    match verb {
        "models" | "model" => {
            let Some(fleet_ctx) = contexts.take_fleet() else {
                return CommandResult::error("Command capability unavailable: fleet");
            };
            CommandResult::message(fleet_ctx.fleet_models_text())
        }
        "add" => {
            let mut words = target.unwrap_or_default().split_whitespace();
            let (Some(provider), Some(model)) = (words.next(), words.next()) else {
                let Some(fleet_ctx) = contexts.take_fleet() else {
                    return CommandResult::error("Command capability unavailable: fleet");
                };
                return CommandResult::error(fleet_ctx.usage_text(FleetUsageKind::Add));
            };
            resolve_action(
                FleetAction::AddModel {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    roles: words.map(str::to_string).collect(),
                },
                None,
            )
        }
        "remove" | "rm" | "drop" => {
            let mut words = target.unwrap_or_default().split_whitespace();
            let (Some(provider), Some(model)) = (words.next(), words.next()) else {
                let Some(fleet_ctx) = contexts.take_fleet() else {
                    return CommandResult::error("Command capability unavailable: fleet");
                };
                return CommandResult::error(fleet_ctx.usage_text(FleetUsageKind::Remove));
            };
            resolve_action(
                FleetAction::RemoveModel {
                    provider: provider.to_string(),
                    model: model.to_string(),
                },
                None,
            )
        }
        "members" | "member" | "roster" | "roles" | "role" | "profiles" | "profile" => {
            resolve_action(FleetAction::OpenRoster, None)
        }
        "setup" | "edit" | "new" => resolve_action(FleetAction::OpenSetup, None),
        "teams" | "team" | "fleets" | "saved" | "manage" => {
            resolve_action(FleetAction::OpenList, None)
        }
        "workers" | "worker" | "agents" | "subagents" => {
            let Some(fleet_ctx) = contexts.take_fleet() else {
                return CommandResult::error("Command capability unavailable: fleet");
            };
            match A::try_from(FleetAction::OpenWorkers) {
                Ok(action) => {
                    fleet_ctx.prepare_workers_view();
                    CommandResult::action(action)
                }
                Err(reason) => CommandResult::error(reason.to_string()),
            }
        }
        "help" | "?" => {
            let Some(fleet_ctx) = contexts.take_fleet() else {
                return CommandResult::error("Command capability unavailable: fleet");
            };
            CommandResult::message(help_text(&fleet_ctx.control_operations()))
        }
        other => {
            let Some(fleet_ctx) = contexts.take_fleet() else {
                return CommandResult::error("Command capability unavailable: fleet");
            };
            match fleet_ctx.resolve_control_verb(other) {
                Some(operation) => {
                    let receipt = fleet_ctx.run_control(operation, target);
                    if receipt.is_error {
                        CommandResult::error(receipt.rendered)
                    } else {
                        CommandResult::message(receipt.rendered)
                    }
                }
                None => CommandResult::error(format!(
                    "Unknown /fleet target '{other}'. Use {} — or /fleet help for the full verb list.",
                    PRIMARY_VERBS[..PRIMARY_VERBS.len() - 1].join(", ")
                )),
            }
        }
    }
}
