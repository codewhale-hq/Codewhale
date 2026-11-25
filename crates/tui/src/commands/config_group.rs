//! Host registration/action adapter for the portable config group (FEAT-028).
//!
//! Replaces the group's former concrete-`App` dispatcher. This file is the only
//! place that knows the TUI `Command`/`CommandGroup` traits and `AppAction`; the
//! movable group (`groups/config/`) imports none of them. It mirrors
//! `commands/session_group.rs` and `commands/debug_group.rs`.

use super::CommandResult;
use super::groups::config::portable_handlers;
use super::traits::{Command, CommandGroup, ContextualCommand};
use crate::tui::app::App;
use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::ConfigCommandResult;

pub(super) struct ConfigCommands;

struct Registration<const INDEX: usize>;

impl<const INDEX: usize> RegisterCommand<CommandResult> for Registration<INDEX> {
    fn info() -> &'static CommandInfo {
        portable_handlers()[INDEX].0
    }

    fn handler() -> CommandHandler<CommandResult> {
        match portable_handlers()[INDEX].1 {
            CommandHandler::Contextual { capabilities, .. } => CommandHandler::Contextual {
                capabilities,
                handler: |contexts, args| match portable_handlers()[INDEX].1 {
                    CommandHandler::Contextual { handler, .. } => {
                        host_result(handler(contexts, args))
                    }
                    _ => CommandResult::error("config handler shape changed"),
                },
            },
            CommandHandler::Pure(_) => {
                CommandHandler::Pure(|args| match portable_handlers()[INDEX].1 {
                    CommandHandler::Pure(run) => host_result(run(args)),
                    _ => CommandResult::error("config handler shape changed"),
                })
            }
        }
    }
}

fn host_result(result: ConfigCommandResult) -> CommandResult {
    CommandResult {
        message: result.message,
        action: result
            .action
            .map(super::config_host::config_action_to_app_action),
        is_error: result.is_error,
    }
}

impl CommandGroup for ConfigCommands {
    fn commands(&self) -> &'static [Box<dyn Command>] {
        static COMMANDS: std::sync::OnceLock<Vec<Box<dyn Command>>> = std::sync::OnceLock::new();
        COMMANDS
            .get_or_init(|| {
                let commands: Vec<Box<dyn Command>> = vec![
                    registration::<0>(),  // /config
                    registration::<1>(),  // /import-claude
                    registration::<2>(),  // /permissions
                    registration::<3>(),  // /login
                    registration::<4>(),  // /connect
                    registration::<5>(),  // /disconnect
                    registration::<6>(),  // /auth
                    registration::<7>(),  // /workbar (+ /rail, /sidebar)
                    registration::<8>(),  // /whale
                    registration::<9>(),  // /settings
                    registration::<10>(), // /status
                    registration::<11>(), // /statusline
                    registration::<12>(), // /mode (+ /jihua, /zidong)
                    registration::<13>(), // /fullscreen
                    registration::<14>(), // /inline
                    registration::<15>(), // /theme
                    registration::<16>(), // /verbose
                    registration::<17>(), // /trust (+ /xinren)
                    registration::<18>(), // /logout
                ];
                assert_eq!(
                    commands
                        .iter()
                        .map(|command| command.info().name)
                        .collect::<Vec<_>>(),
                    portable_handlers()
                        .iter()
                        .map(|(info, _)| info.name)
                        .collect::<Vec<_>>(),
                    "host registry must cover the complete portable config inventory"
                );
                commands
            })
            .as_slice()
    }
}

fn registration<const INDEX: usize>() -> Box<dyn Command> {
    Box::new(
        ContextualCommand::from_contract::<Registration<INDEX>>().expect("config registration"),
    )
}

/// Dispatch a `/jihua` (`plan`) or `/zidong` (`yolo`) mode alias through the
/// registered `/mode` handler and its declared capability envelope.
pub(super) fn mode_alias(app: &mut App, arg: &str) -> CommandResult {
    match Registration::<12>::handler() {
        CommandHandler::Contextual {
            capabilities,
            handler,
        } => handler(
            app.command_contexts_with_config(None)
                .contexts(capabilities),
            Some(arg),
        ),
        _ => CommandResult::error("mode handler shape changed"),
    }
}
