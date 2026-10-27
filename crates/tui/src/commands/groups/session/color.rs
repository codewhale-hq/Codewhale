use super::CommandResult;
use codewhale_command_contract::facets::{
    CommandPresentationContext, CommandSessionControlContext,
};
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub(in crate::commands) const COMPOSER_COLORS: [&str; 8] = [
    "red", "orange", "amber", "green", "cyan", "blue", "purple", "pink",
];

pub(in crate::commands) struct ColorCmd;

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "color",
    aliases: &["colour"],
    usage: "/color [red|orange|amber|green|cyan|blue|purple|pink|off]",
    description_key: "cmd_color_description",
};

impl ContractRegisterCommand<CommandResult> for ColorCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::SESSION_CONTROL
                .union(codewhale_command_contract::handler::CommandCapabilities::PRESENTATION),
            handler: color_contextual,
        }
    }
}

pub(in crate::commands) fn color_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(control) = parts.control.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: session_control".to_string());
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation".to_string());
    };
    color_portable(control, presentation, arg)
}

pub(in crate::commands) fn color_portable(
    control: &mut dyn CommandSessionControlContext,
    presentation: &dyn CommandPresentationContext,
    arg: Option<&str>,
) -> CommandResult {
    let requested = arg
        .map(str::trim)
        .filter(|arg| !arg.is_empty())
        .map(str::to_ascii_lowercase);
    let next = match requested.as_deref() {
        None => {
            let current = control.composer_color();
            let position = current
                .as_deref()
                .and_then(|name| COMPOSER_COLORS.iter().position(|color| *color == name));
            Some(COMPOSER_COLORS[position.map_or(0, |index| (index + 1) % COMPOSER_COLORS.len())])
        }
        Some("off" | "clear" | "none" | "default") => None,
        Some(name) => match COMPOSER_COLORS.iter().find(|color| **color == name) {
            Some(color) => Some(*color),
            None => {
                return CommandResult::error(
                    presentation
                        .translate(
                            "cmd_color_unknown",
                            &[("colors", &COMPOSER_COLORS.join(", "))],
                        )
                        .unwrap_or_else(std::convert::identity),
                );
            }
        },
    };
    let message = match next {
        Some(color) => presentation.translate("cmd_color_set", &[("color", color)]),
        None => presentation.translate("cmd_color_reset", &[]),
    };
    let message = match message {
        Ok(message) => message,
        Err(error) => return CommandResult::error(error),
    };
    match control.set_composer_color(next.map(str::to_string)) {
        Ok(()) => CommandResult::message(message),
        Err(error) => CommandResult::error(error),
    }
}
