//! `/hotbar` command.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{
    CoreNavigationAction as Action, CoreNavigationCommandResult as CommandResult,
};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "hotbar",
    aliases: &["hotkeys"],
    usage: "/hotbar",
    description_key: "cmd_hotbar_description",
};

pub struct HotbarCmd;

impl RegisterCommand<CommandResult> for HotbarCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Pure(hotbar)
    }
}

fn hotbar(arg: Option<&str>) -> CommandResult {
    match arg.map(str::trim).filter(|arg| !arg.is_empty()) {
        None | Some("setup" | "edit" | "configure" | "config") => {
            CommandResult::action(Action::OpenHotbarSetup)
        }
        // Hide the Hotbar: persist `hotbar = []` and clear the live slots.
        Some("off" | "disable" | "hide") => CommandResult::action(Action::DisableHotbar),
        // Restore the default recommended slots (explicit reset).
        Some("on" | "reset" | "defaults" | "default") => {
            CommandResult::action(Action::RestoreHotbarDefaults)
        }
        Some("help" | "?") => CommandResult::message(
            "Hotbar gives you Alt+1 through Alt+8 shortcuts (Option key on macOS, Alt \
             elsewhere). Use `/hotbar` to customize, `/hotbar off` to hide it \
             (`hotbar = []`), and `/hotbar on` to restore the default slots. \
             Hotbar slots dispatch only when no modal, inline picker, or \
             onboarding surface owns input. Bare 1-8 insert text in the \
             composer. Cmd-number and F-keys are not Hotbar shortcuts unless \
             a future release documents and implements them.",
        ),
        Some(other) => CommandResult::error(format!(
            "Unknown /hotbar target '{other}'. Try `/hotbar`, `/hotbar off`, \
             `/hotbar on`, or `/hotbar help`."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotbar_command_opens_setup_view() {
        let result = hotbar(None);
        assert_eq!(result.action, Some(Action::OpenHotbarSetup));
        assert!(result.message.is_none());
    }

    #[test]
    fn hotbar_setup_alias_opens_setup_view() {
        let result = hotbar(Some("setup"));
        assert_eq!(result.action, Some(Action::OpenHotbarSetup));
        assert!(result.message.is_none());
    }

    #[test]
    fn hotbar_help_arg_explains_customize_and_disable() {
        let result = hotbar(Some("help"));
        assert!(!result.is_error);
        assert!(result.action.is_none());
        let message = result
            .message
            .as_deref()
            .expect("help should return a message");
        assert!(
            message.contains("/hotbar")
                && message.contains("customize")
                && message.contains("Alt+1 through Alt+8"),
            "help should point at /hotbar to customize: {message:?}"
        );
        assert!(
            message.contains("hotbar = []"),
            "help should mention the explicit disabled config: {message:?}"
        );
        assert!(
            message.contains("/hotbar off") && message.contains("/hotbar on"),
            "help should mention both disable and restore paths: {message:?}"
        );
        assert!(message.contains("Bare 1-8 insert text"));
        assert!(message.contains("Cmd-number and F-keys are not Hotbar shortcuts"));
    }

    #[test]
    fn hotbar_off_and_disable_aliases_return_disable_action() {
        for arg in ["off", "disable", "hide"] {
            let result = hotbar(Some(arg));
            assert_eq!(
                result.action,
                Some(Action::DisableHotbar),
                "`/hotbar {arg}` should disable the hotbar"
            );
            assert!(
                result.message.is_none(),
                "`/hotbar {arg}` should not also emit a message"
            );
        }
    }

    #[test]
    fn hotbar_on_and_reset_aliases_return_restore_action() {
        for arg in ["on", "reset", "defaults", "default"] {
            let result = hotbar(Some(arg));
            assert_eq!(
                result.action,
                Some(Action::RestoreHotbarDefaults),
                "`/hotbar {arg}` should restore default hotbar slots"
            );
            assert!(result.message.is_none());
        }
    }

    #[test]
    fn hotbar_unknown_arg_reports_error() {
        let result = hotbar(Some("bogus"));
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown /hotbar target 'bogus'"))
        );
    }
}
