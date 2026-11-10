//! Host registration/action adapter for the portable session group. Replaces
//! groups/session/mod.rs's concrete-App dispatcher; the existing central
//! dispatcher still constructs the declared envelope and consumes the result.

use super::CommandResult;
use super::groups::session::*;
use super::traits::{Command, CommandGroup, ContextualCommand};
use crate::tui::app::AppAction;
use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{SessionAction, SessionCommandResult};
use std::marker::PhantomData;

pub(super) struct SessionCommands;

struct HostRegistration<C>(PhantomData<C>);
impl<C: RegisterCommand<SessionCommandResult>> RegisterCommand<CommandResult>
    for HostRegistration<C>
{
    fn info() -> &'static CommandInfo {
        C::info()
    }
    fn handler() -> CommandHandler<CommandResult> {
        match C::handler() {
            CommandHandler::Pure(_) => CommandHandler::Pure(|args| match C::handler() {
                CommandHandler::Pure(run) => host_result(run(args)),
                _ => CommandResult::error("command handler shape changed"),
            }),
            CommandHandler::Contextual { capabilities, .. } => CommandHandler::Contextual {
                capabilities,
                handler: |contexts, args| match C::handler() {
                    CommandHandler::Contextual { handler, .. } => {
                        host_result(handler(contexts, args))
                    }
                    _ => CommandResult::error("command handler shape changed"),
                },
            },
        }
    }
}

impl CommandGroup for SessionCommands {
    fn commands(&self) -> &'static [Box<dyn Command>] {
        static COMMANDS: std::sync::OnceLock<Vec<Box<dyn Command>>> = std::sync::OnceLock::new();
        COMMANDS.get_or_init(|| {
            let commands: Vec<Box<dyn Command>> = vec![
                Box::new(ContextualCommand::from_contract::<HostRegistration<rename::RenameCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<title::TitleCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<color::ColorCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<save::SaveCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<fork::ForkCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<new::NewCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<sessions::SessionsCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<load::LoadCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<resume::ResumeCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<tree::TreeCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<branch::BranchCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<compact::CompactCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<purge::PurgeCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<relay::RelayCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<remote_control::RemoteControlCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<remote_env::RemoteEnvCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<export::ExportCmd>>().expect("session registration")),
                Box::new(ContextualCommand::from_contract::<HostRegistration<StructcopyRegistration>>().expect("structcopy registration")),
            ];
            assert_eq!(commands.iter().map(|command| command.info().name).collect::<Vec<_>>(),
                portable_handlers().iter().map(|(info, _)| info.name).collect::<Vec<_>>(),
                "host registry must cover the complete portable session inventory");
            commands
        }).as_slice()
    }
}

pub(in crate::commands) fn host_result(result: SessionCommandResult) -> CommandResult {
    let action = result.action.map(|action| match action {
        SessionAction::CompactContext { focus } => AppAction::CompactContext { focus },
        SessionAction::PurgeContext => AppAction::PurgeContext,
        SessionAction::LoadSession(path) => AppAction::LoadSession(path),
        SessionAction::SendMessage(message) => AppAction::SendMessage(message),
        SessionAction::OpenExternalUrl { url, label } => AppAction::OpenExternalUrl { url, label },
        SessionAction::RemoteControl(action) => AppAction::RemoteControl(match action {
            codewhale_command_contract::outcome::SessionRemoteControlAction::Start => {
                codewhale_app_server::remote_control::RemoteControlAction::Start
            }
            codewhale_command_contract::outcome::SessionRemoteControlAction::Stop => {
                codewhale_app_server::remote_control::RemoteControlAction::Stop
            }
        }),
        SessionAction::NewSession { sync, force } => AppAction::NewSession { sync, force },
        SessionAction::SyncSession(sync) => AppAction::SyncSession {
            session_id: sync.session_id,
            messages: sync.messages,
            system_prompt: sync.system_prompt,
            model: sync.model,
            workspace: sync.workspace,
            mode: super::contract::from_command_mode(sync.mode),
        },
    });
    CommandResult {
        message: result.message,
        action,
        is_error: result.is_error,
    }
}
