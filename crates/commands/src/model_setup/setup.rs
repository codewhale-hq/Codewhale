//! `/setup` command. `/setup fleet` opens the fleet readiness step.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::model_setup::resolve_action;
use codewhale_command_contract::model_setup::{SetupAction, SetupStep};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "setup",
    aliases: &[],
    usage: "/setup [fleet|provider|runtime|constitution|status|hotbar|tools|remote|persistence]",
    description_key: "cmd_setup_description",
};

pub struct SetupCmd;

impl<A: for<'a> TryFrom<SetupAction<'a>, Error = String>> RegisterCommand<CommandResult<A>>
    for SetupCmd
{
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Pure(setup)
    }
}

fn setup<A: for<'a> TryFrom<SetupAction<'a>, Error = String>>(
    arg: Option<&str>,
) -> CommandResult<A> {
    let target = arg.map(str::trim).filter(|arg| !arg.is_empty());
    if let Some(arg) = target {
        let mut parts = arg.split_whitespace();
        if matches!(parts.next(), Some("provider" | "providers"))
            && let Some(raw_provider) = parts.next()
        {
            if parts.next().is_some() {
                return CommandResult::error("Usage: /setup provider [provider-name]".to_string());
            }
            return resolve_action(SetupAction::Provider(raw_provider), None);
        }
    }

    let step = match target {
        None | Some("open" | "wizard" | "checkpoint") => None,
        Some("provider" | "providers" | "model" | "models" | "route") => {
            Some(SetupStep::ProviderModel)
        }
        Some("runtime" | "posture" | "trust" | "sandbox") => Some(SetupStep::TrustSandbox),
        Some("constitution" | "law") => Some(SetupStep::Constitution),
        Some("status" | "report" | "verification" | "verify") => Some(SetupStep::Verification),
        Some("fleet" | "operate" | "operate-fleet" | "operate_fleet") => {
            Some(SetupStep::OperateFleet)
        }
        Some("hotbar" | "hotkeys" | "shortcuts" | "keys") => Some(SetupStep::Hotbar),
        Some("tools" | "tool" | "mcp" | "tools-mcp" | "tools_mcp" | "skills" | "plugins") => {
            Some(SetupStep::ToolsMcp)
        }
        Some(
            "remote" | "remote-runtime" | "remote_runtime" | "cloud" | "bridge" | "mobile"
            | "phone",
        ) => Some(SetupStep::RemoteRuntime),
        Some("persistence" | "persist" | "storage") => Some(SetupStep::Persistence),
        Some(other) => {
            return CommandResult::error(format!(
                "Unknown /setup target '{other}'. Try `/setup fleet` to configure saved teams, or \
                 `/setup` to open the full setup wizard."
            ));
        }
    };
    resolve_action(SetupAction::OpenWizard(step), None)
}
