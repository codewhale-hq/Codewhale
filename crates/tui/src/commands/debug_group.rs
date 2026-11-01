//! Host registration/action adapter for the portable debug group. Replaces
//! the portable command crate's handlers; the existing central
//! dispatcher still constructs the declared envelope and consumes the result.

use super::CommandResult;
use super::traits::{Command, CommandGroup, ContextualCommand};
use crate::tui::app::AppAction;
use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{DebugAction, DebugCommandResult};
use codewhale_commands::debug::*;
use std::marker::PhantomData;

pub(super) struct DebugCommands;

struct HostRegistration<C>(PhantomData<C>);
impl<C: RegisterCommand<DebugCommandResult>> RegisterCommand<CommandResult>
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

impl CommandGroup for DebugCommands {
    fn commands(&self) -> &'static [Box<dyn Command>] {
        static COMMANDS: std::sync::OnceLock<Vec<Box<dyn Command>>> = std::sync::OnceLock::new();
        COMMANDS.get_or_init(|| {
            let commands: Vec<Box<dyn Command>> = vec![
            Box::new(ContextualCommand::from_contract::<HostRegistration<tokens::TokensCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<tokens::CostCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<receipts::ReceiptsCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<balance::BalanceCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<cache::CacheCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<preview_request::PreviewRequestCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<tool_inspection::ToolsCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<change::ChangeCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<tokens::SystemCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<tokens::ContextCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<undo::EditCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<undo::DiffCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<undo::UndoCmd>>().expect("debug registration")),
            Box::new(ContextualCommand::from_contract::<HostRegistration<undo::RetryCmd>>().expect("debug registration")),
            ];
            assert_eq!(commands.iter().map(|command| command.info().name).collect::<Vec<_>>(),
                portable_handlers().iter().map(|(info, _)| info.name).collect::<Vec<_>>(),
                "host registry must cover the complete portable debug inventory");
            commands
        }).as_slice()
    }
}

fn sync_session(sync: codewhale_command_contract::facets::SessionSyncPayload) -> AppAction {
    AppAction::SyncSession {
        session_id: sync.session_id,
        messages: sync.messages,
        system_prompt: sync.system_prompt,
        model: sync.model,
        workspace: sync.workspace,
        mode: super::contract::from_command_mode(sync.mode),
    }
}

pub(in crate::commands) fn host_result(result: DebugCommandResult) -> CommandResult {
    let action = result.action.map(|action| match action {
        DebugAction::FetchBalance => AppAction::FetchBalance,
        DebugAction::CacheWarmup => AppAction::CacheWarmup,
        DebugAction::PreviewOutboundRequest {
            json,
            base_prompt_only,
            hypothetical_prompt,
        } => AppAction::PreviewOutboundRequest {
            json,
            base_prompt_only,
            hypothetical_prompt,
        },
        DebugAction::OpenTextPager { title, content } => {
            AppAction::OpenTextPager { title, content }
        }
        DebugAction::OpenDiffPager { title, diff } => AppAction::OpenDiffPager { title, diff },
        DebugAction::OpenContextInspector => AppAction::OpenContextInspector,
        DebugAction::SendMessage(input) => AppAction::SendMessage(input),
        DebugAction::SyncSession(sync) => sync_session(sync),
        DebugAction::ConversationUndo {
            sync,
            restored_files,
            retry_input,
        } => AppAction::ConversationUndo {
            sync,
            restored_files,
            retry_input,
            edit_replacement: false,
        },
    });
    CommandResult {
        message: result.message,
        action,
        is_error: result.is_error,
    }
}
