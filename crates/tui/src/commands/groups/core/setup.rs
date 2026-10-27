//! `/setup` command. `/setup fleet` opens the fleet readiness step.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::{App, AppAction};
#[cfg(test)]
use codewhale_config::ProviderKind;
use codewhale_localization::MessageId;

use super::CommandResult;
use codewhale_config::SetupStep;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "setup",
    aliases: &[],
    usage: "/setup [fleet|provider|runtime|constitution|status|hotbar|tools|remote|persistence]",
    description_id: MessageId::CmdSetupDescription,
};

pub(in crate::commands) struct SetupCmd;

impl RegisterCommand for SetupCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(_app: &mut App, arg: Option<&str>) -> CommandResult {
        let target = arg.map(str::trim).filter(|arg| !arg.is_empty());
        if let Some(arg) = target {
            let mut parts = arg.split_whitespace();
            if matches!(parts.next(), Some("provider" | "providers"))
                && let Some(raw_provider) = parts.next()
            {
                if parts.next().is_some() {
                    return CommandResult::error(
                        "Usage: /setup provider [provider-name]".to_string(),
                    );
                }
                return match super::provider::provider_setup_action_for_name(raw_provider) {
                    Ok(action) => CommandResult::action(action),
                    Err(message) => CommandResult::error(message),
                };
            }
        }

        match target {
            None | Some("open" | "wizard" | "checkpoint") => {
                CommandResult::action(AppAction::OpenSetupWizard)
            }
            Some("provider" | "providers" | "model" | "models" | "route") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::ProviderModel,
                })
            }
            Some("runtime" | "posture" | "trust" | "sandbox") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::TrustSandbox,
                })
            }
            Some("constitution" | "law") => CommandResult::action(AppAction::OpenSetupWizardAt {
                step: SetupStep::Constitution,
            }),
            Some("status" | "report" | "verification" | "verify") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::Verification,
                })
            }
            Some("fleet" | "operate" | "operate-fleet" | "operate_fleet") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::OperateFleet,
                })
            }
            Some("hotbar" | "hotkeys" | "shortcuts" | "keys") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::Hotbar,
                })
            }
            Some("tools" | "tool" | "mcp" | "tools-mcp" | "tools_mcp" | "skills" | "plugins") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::ToolsMcp,
                })
            }
            Some(
                "remote" | "remote-runtime" | "remote_runtime" | "cloud" | "bridge" | "mobile"
                | "phone",
            ) => CommandResult::action(AppAction::OpenSetupWizardAt {
                step: SetupStep::RemoteRuntime,
            }),
            Some("persistence" | "persist" | "storage") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::Persistence,
                })
            }
            Some(other) => CommandResult::error(format!(
                "Unknown /setup target '{other}'. Try `/setup fleet` to configure saved teams, or \
                 `/setup` to open the full setup wizard."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn setup_command_opens_wizard() {
        let mut app = test_app();

        let result = SetupCmd::execute(&mut app, None);

        assert_eq!(result.action, Some(AppAction::OpenSetupWizard));
        assert!(result.message.is_none());
    }

    #[test]
    fn setup_checkpoint_alias_opens_wizard() {
        let mut app = test_app();

        let result = SetupCmd::execute(&mut app, Some("checkpoint"));

        assert_eq!(result.action, Some(AppAction::OpenSetupWizard));
        assert!(result.message.is_none());
    }

    #[test]
    fn setup_report_opens_verification_step() {
        let mut app = test_app();

        let result = SetupCmd::execute(&mut app, Some("report"));

        assert_eq!(
            result.action,
            Some(AppAction::OpenSetupWizardAt {
                step: SetupStep::Verification
            })
        );
        assert!(result.message.is_none());
    }

    #[test]
    fn setup_named_steps_open_matching_wizard_cards() {
        let cases = [
            ("provider", SetupStep::ProviderModel),
            ("model", SetupStep::ProviderModel),
            ("runtime", SetupStep::TrustSandbox),
            ("posture", SetupStep::TrustSandbox),
            ("constitution", SetupStep::Constitution),
            ("hotbar", SetupStep::Hotbar),
            ("shortcuts", SetupStep::Hotbar),
            ("tools", SetupStep::ToolsMcp),
            ("tool", SetupStep::ToolsMcp),
            ("tools-mcp", SetupStep::ToolsMcp),
            ("tools_mcp", SetupStep::ToolsMcp),
            ("mcp", SetupStep::ToolsMcp),
            ("skills", SetupStep::ToolsMcp),
            ("plugins", SetupStep::ToolsMcp),
            ("remote", SetupStep::RemoteRuntime),
            ("cloud", SetupStep::RemoteRuntime),
            ("persistence", SetupStep::Persistence),
            ("persist", SetupStep::Persistence),
            ("storage", SetupStep::Persistence),
        ];

        for (arg, step) in cases {
            let mut app = test_app();
            let result = SetupCmd::execute(&mut app, Some(arg));
            assert_eq!(
                result.action,
                Some(AppAction::OpenSetupWizardAt { step }),
                "{arg}"
            );
            assert!(result.message.is_none(), "{arg}");
        }
    }

    #[test]
    fn setup_fleet_target_opens_the_operate_fleet_step() {
        for target in ["fleet", "operate", "operate-fleet", "operate_fleet"] {
            let mut app = test_app();
            let result = SetupCmd::execute(&mut app, Some(target));

            assert_eq!(
                result.action,
                Some(AppAction::OpenSetupWizardAt {
                    step: SetupStep::OperateFleet
                }),
                "{target}"
            );
            assert!(result.message.is_none(), "{target}");
        }
    }

    #[test]
    fn setup_retired_pod_target_is_rejected() {
        let mut app = test_app();
        let result = SetupCmd::execute(&mut app, Some("pod"));

        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("/setup fleet")),
            "retired target must point at the canonical spelling, got: {result:?}"
        );
    }

    #[test]
    fn setup_usage_advertises_the_canonical_fleet_target() {
        assert!(SetupCmd::info().usage.contains("fleet"));
        assert!(!SetupCmd::info().usage.contains("pod"));
    }

    #[test]
    fn setup_unknown_target_points_to_fleet_setup() {
        let mut app = test_app();
        let result = SetupCmd::execute(&mut app, Some("bogus"));

        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("/setup fleet"))
        );
    }

    #[test]
    fn setup_provider_named_opens_provider_setup_catalog() {
        let mut app = test_app();

        let result = SetupCmd::execute(&mut app, Some("provider anthropic"));

        assert_eq!(
            result.action,
            Some(AppAction::OpenProviderSetup {
                provider: Some(ProviderKind::Anthropic.as_str().into())
            })
        );
        assert!(result.message.is_none());
    }

    #[test]
    fn setup_provider_ds4_opens_keyless_local_preset() {
        let mut app = test_app();

        let result = SetupCmd::execute(&mut app, Some("provider ds4"));

        assert_eq!(result.action, Some(AppAction::OpenDs4Setup));
        assert!(result.message.is_none());
    }

    #[test]
    fn setup_provider_agnes_rejects_retired_template_name() {
        let mut app = test_app();

        let result = SetupCmd::execute(&mut app, Some("provider agnes"));

        assert!(result.action.is_none());
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown provider 'agnes'"))
        );
    }

    #[test]
    fn setup_provider_named_rejects_unknown_provider() {
        let mut app = test_app();

        let result = SetupCmd::execute(&mut app, Some("provider imaginary"));

        assert!(result.action.is_none());
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown provider 'imaginary'"))
        );
    }
}
