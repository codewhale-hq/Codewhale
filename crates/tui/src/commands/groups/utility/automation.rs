//! Operator controls for durable scheduled automations.

use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;
use crate::tui::app::{AppAction, AutomationAction};

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "automation",
    aliases: &["automations", "scheduled"],
    usage: "/automation [list|show <id>|print <id>|pause <id>|resume <id>|adopt <id>|delete <id> [--confirm <token>]|run <id>]",
    description_key: "cmd_automation_description",
};

pub(in crate::commands) struct AutomationCmd;

impl RegisterCommand<CommandResult> for AutomationCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::PRESENTATION,
            handler: automation_contextual,
        }
    }
}

fn automation_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    automation(presentation, arg)
}

fn automation(
    presentation: &mut dyn CommandPresentationContext,
    args: Option<&str>,
) -> CommandResult {
    let raw = args.unwrap_or("").trim();
    // Bare `/automation` opens the room; `list` keeps the text receipt for
    // scripts and transcripts.
    if raw.is_empty() {
        return action(AutomationAction::Open { focus: None });
    }
    if raw.eq_ignore_ascii_case("list") {
        return action(AutomationAction::List);
    }

    let mut parts = raw.split_whitespace();
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();

    match verb.as_str() {
        "show" | "status" => single_id(presentation, &mut parts, |id| AutomationAction::Open {
            focus: Some(id),
        }),
        "print" => single_id(presentation, &mut parts, AutomationAction::Show),
        "pause" => single_id(presentation, &mut parts, AutomationAction::Pause),
        "resume" => single_id(presentation, &mut parts, AutomationAction::Resume),
        "adopt" => single_id(presentation, &mut parts, AutomationAction::Adopt),
        "delete" | "remove" | "rm" => delete(presentation, &mut parts),
        "run" | "trigger" => single_id(presentation, &mut parts, AutomationAction::Run),
        _ => usage_error(presentation),
    }
}

fn single_id<'a>(
    presentation: &mut dyn CommandPresentationContext,
    parts: &mut impl Iterator<Item = &'a str>,
    make_action: fn(String) -> AutomationAction,
) -> CommandResult {
    let Some(id) = parts.next() else {
        return usage_error(presentation);
    };
    if parts.next().is_some() {
        return usage_error(presentation);
    }
    action(make_action(id.to_string()))
}

fn delete<'a>(
    presentation: &mut dyn CommandPresentationContext,
    parts: &mut impl Iterator<Item = &'a str>,
) -> CommandResult {
    let Some(id) = parts.next() else {
        return usage_error(presentation);
    };
    let confirmation = match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => None,
        (Some(flag), Some(token), None) if flag.eq_ignore_ascii_case("--confirm") => {
            Some(token.to_string())
        }
        _ => return usage_error(presentation),
    };
    action(AutomationAction::Delete {
        id: id.to_string(),
        confirmation,
    })
}

fn usage_error(presentation: &mut dyn CommandPresentationContext) -> CommandResult {
    match presentation.translate("automation_usage", &[]) {
        Ok(text) => CommandResult::error(text),
        // The key is catalog-known; a translation failure must still fail
        // safely without exposing a raw lookup key (D3).
        Err(_) => CommandResult::error(
            "Usage: /automation [list|show <id>|pause <id>|resume <id>|adopt <id>|delete <id> [--confirm <token>]|run <id>]",
        ),
    }
}

fn action(action: AutomationAction) -> CommandResult {
    CommandResult::action(AppAction::Automation(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakePresentation;
    impl CommandPresentationContext for FakePresentation {
        fn translate(&self, key: &str, _r: &[(&str, &str)]) -> Result<String, String> {
            if key == "automation_usage" {
                Ok("Usage: /automation [list|show <id>|pause <id>|resume <id>|delete <id> [--confirm <token>]|run <id>]".to_string())
            } else {
                Err("unknown translation key".to_string())
            }
        }
    }

    fn parsed(args: Option<&str>) -> Option<AutomationAction> {
        match automation(&mut FakePresentation, args).action {
            Some(AppAction::Automation(action)) => Some(action),
            _ => None,
        }
    }

    #[test]
    fn parses_list_show_and_mutations() {
        assert_eq!(parsed(None), Some(AutomationAction::Open { focus: None }));
        assert_eq!(parsed(Some("list")), Some(AutomationAction::List));
        assert_eq!(
            parsed(Some("show auto_1")),
            Some(AutomationAction::Open {
                focus: Some("auto_1".to_string())
            })
        );
        assert_eq!(
            parsed(Some("print auto_1")),
            Some(AutomationAction::Show("auto_1".to_string()))
        );
        assert_eq!(
            parsed(Some("pause auto_1")),
            Some(AutomationAction::Pause("auto_1".to_string()))
        );
        assert_eq!(
            parsed(Some("resume auto_1")),
            Some(AutomationAction::Resume("auto_1".to_string()))
        );
        assert_eq!(
            parsed(Some("delete auto_1")),
            Some(AutomationAction::Delete {
                id: "auto_1".to_string(),
                confirmation: None,
            })
        );
        assert_eq!(
            parsed(Some("run auto_1")),
            Some(AutomationAction::Run("auto_1".to_string()))
        );
    }

    #[test]
    fn accepts_operator_aliases() {
        assert_eq!(
            parsed(Some("status auto_1")),
            Some(AutomationAction::Open {
                focus: Some("auto_1".to_string())
            })
        );
        assert_eq!(
            parsed(Some("rm auto_1")),
            Some(AutomationAction::Delete {
                id: "auto_1".to_string(),
                confirmation: None,
            })
        );
        assert_eq!(
            parsed(Some("trigger auto_1")),
            Some(AutomationAction::Run("auto_1".to_string()))
        );
    }

    #[test]
    fn validates_missing_ids_and_unknown_actions() {
        for verb in ["show", "pause", "resume", "delete", "run", "unknown"] {
            let result = automation(&mut FakePresentation, Some(verb));
            assert!(result.message.is_some(), "{verb} should show usage");
            assert!(result.action.is_none());
        }
    }

    #[test]
    fn delete_confirmation_is_explicit_and_exact() {
        assert_eq!(
            parsed(Some("delete auto_1 --confirm receipt")),
            Some(AutomationAction::Delete {
                id: "auto_1".to_string(),
                confirmation: Some("receipt".to_string()),
            })
        );
        for invalid in [
            "delete auto_1 --confirm",
            "delete auto_1 receipt",
            "delete auto_1 --confirm receipt extra",
        ] {
            let result = automation(&mut FakePresentation, Some(invalid));
            assert!(result.is_error, "{invalid} should be rejected");
            assert!(result.action.is_none());
        }
    }

    #[test]
    fn handler_is_contextual_and_requests_presentation_facet() {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = AutomationCmd::handler()
        else {
            panic!("automation must be contextual");
        };
        assert_eq!(capabilities, CommandCapabilities::PRESENTATION);
        let missing = handler(CommandContexts::empty(), Some("list"));
        assert!(missing.is_error);
        assert_eq!(
            missing.message.as_deref(),
            Some("Error: Command capability unavailable: presentation")
        );
        assert_eq!(
            AutomationCmd::info().description_key,
            "cmd_automation_description"
        );
        assert_eq!(AutomationCmd::info().aliases, &["automations", "scheduled"]);
    }
}
