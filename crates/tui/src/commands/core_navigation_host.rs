use super::CommandResult;
use super::groups::core::navigation::portable_handlers;
use super::traits::{Command, ContextualCommand};
use crate::tui::app::{App, AppAction};
use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{CoreNavigationAction, CoreNavigationCommandResult};

struct Registration<const INDEX: usize>;

impl<const INDEX: usize> RegisterCommand<CommandResult> for Registration<INDEX> {
    fn info() -> &'static CommandInfo {
        portable_handlers()[INDEX].0
    }

    fn handler() -> CommandHandler<CommandResult> {
        match portable_handlers()[INDEX].1 {
            CommandHandler::Pure(_) => {
                CommandHandler::Pure(|args| match portable_handlers()[INDEX].1 {
                    CommandHandler::Pure(run) => host_result(run(args)),
                    _ => CommandResult::error("navigation handler shape changed"),
                })
            }
            CommandHandler::Contextual { capabilities, .. } => CommandHandler::Contextual {
                capabilities,
                handler: |contexts, args| match portable_handlers()[INDEX].1 {
                    CommandHandler::Contextual { handler, .. } => {
                        host_result(handler(contexts, args))
                    }
                    _ => CommandResult::error("navigation handler shape changed"),
                },
            },
        }
    }
}

fn host_result(result: CoreNavigationCommandResult) -> CommandResult {
    CommandResult {
        message: result.message,
        action: result.action.map(|action| match action {
            CoreNavigationAction::OpenHotbarSetup => AppAction::OpenHotbarSetup,
            CoreNavigationAction::DisableHotbar => AppAction::DisableHotbar,
            CoreNavigationAction::RestoreHotbarDefaults => AppAction::RestoreHotbarDefaults,
            CoreNavigationAction::OpenLiveTranscript => AppAction::OpenLiveTranscript,
            CoreNavigationAction::OpenTurnInspector => AppAction::OpenTurnInspector,
            CoreNavigationAction::OpenWorktreeManager => AppAction::OpenWorktreeManager,
            CoreNavigationAction::SwitchWorkspace { workspace } => {
                AppAction::SwitchWorkspace { workspace }
            }
            CoreNavigationAction::VoiceCapture => AppAction::VoiceCapture,
        }),
        is_error: result.is_error,
    }
}

pub(in crate::commands) fn dispatch(app: &mut App, name: &str, arg: Option<&str>) -> CommandResult {
    let Some(handler) = super::registry()
        .get(name)
        .and_then(Command::contextual_handler)
    else {
        return CommandResult::error(format!("Navigation command unavailable: {name}"));
    };
    match handler {
        CommandHandler::Pure(run) => run(arg),
        CommandHandler::Contextual {
            capabilities,
            handler,
        } => {
            let mut bundle = app.command_contexts_with_config(None);
            handler(bundle.contexts(capabilities), arg)
        }
    }
}

pub(in crate::commands) fn registration<const INDEX: usize>() -> Box<dyn Command> {
    Box::new(
        ContextualCommand::from_contract::<Registration<INDEX>>().expect("navigation registration"),
    )
}
