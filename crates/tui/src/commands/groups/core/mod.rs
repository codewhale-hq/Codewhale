//! Core command area: model/provider selection, help, navigation, and the
//! persistent RLM / sub-agent entry points.

#[cfg(all(test, feature = "long-running-tests"))]
mod acceptance;
mod advisor;
mod agent;
mod clear;
mod constitution;
// This group dir intentionally has a `core.rs` child module with the same
// name. The module_inception allow is a permanent structure rationale, not
// migration scaffolding; see docs/architecture/command-dispatch.md.
#[allow(clippy::module_inception)]
mod core;
mod effort;
mod exit;
mod feedback;
pub(in crate::commands) mod fleet;
mod hf;
mod hooks;
mod lane;
mod model;
mod modeldb;
mod models;
pub(in crate::commands) mod navigation;
mod pin;
mod profile;
mod provider;
mod queue;
mod rlm;
pub(in crate::commands) mod router;
mod setup;
mod stash;
mod subagents;
pub mod util;
mod workflow;

pub(crate) use self::core::reset_conversation_state;
pub(crate) use constitution::open_manager as open_constitution_manager;

use crate::commands::CommandResult;
use crate::commands::traits::{
    Command, CommandGroup, ContextualCommand, FunctionCommand, RegisterCommand,
};

pub struct CoreCommands;

impl CommandGroup for CoreCommands {
    fn commands(&self) -> &'static [Box<dyn Command>] {
        cached_command_list!(vec![
            crate::commands::core_navigation_host::registration::<0>(),
            Box::new(
                ContextualCommand::from_contract::<advisor::AdvisorCmd>()
                    .expect("advisor registration")
            ),
            crate::commands::core_navigation_host::registration::<1>(),
            Box::new(FunctionCommand::new(
                clear::ClearCmd::info(),
                clear::ClearCmd::execute,
            )),
            crate::commands::core_navigation_host::registration::<2>(),
            Box::new(
                ContextualCommand::from_contract::<exit::ExitCmd>().expect("exit registration")
            ),
            Box::new(FunctionCommand::new(
                model::ModelCmd::info(),
                model::ModelCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                router::RouterCmd::info(),
                router::RouterCmd::execute,
            )),
            Box::new(
                ContextualCommand::from_contract::<models::ModelsCmd>()
                    .expect("models registration")
            ),
            Box::new(
                ContextualCommand::from_contract::<modeldb::ModelDbCmd>()
                    .expect("modeldb registration")
            ),
            Box::new(FunctionCommand::new(
                provider::ProviderCmd::info(),
                provider::ProviderCmd::execute,
            )),
            Box::new(
                ContextualCommand::from_contract::<queue::QueueCmd>().expect("queue registration")
            ),
            Box::new(
                ContextualCommand::from_contract::<stash::StashCmd>().expect("stash registration")
            ),
            Box::new(FunctionCommand::new(
                hooks::HooksCmd::info(),
                hooks::HooksCmd::execute,
            )),
            Box::new(
                ContextualCommand::from_contract::<subagents::SubagentsCmd>()
                    .expect("subagents registration")
            ),
            Box::new(FunctionCommand::new(
                fleet::FleetCmd::info(),
                fleet::FleetCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                lane::LaneCmd::info(),
                lane::LaneCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                workflow::WorkflowCmd::info(),
                workflow::WorkflowCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                workflow::WorkflowsCmd::info(),
                workflow::WorkflowsCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                workflow::AutoCmd::info(),
                workflow::AutoCmd::execute,
            )),
            crate::commands::core_navigation_host::registration::<3>(),
            Box::new(FunctionCommand::new(
                setup::SetupCmd::info(),
                setup::SetupCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                constitution::ConstitutionCmd::info(),
                constitution::ConstitutionCmd::execute,
            )),
            Box::new(
                ContextualCommand::from_contract::<agent::AgentCmd>().expect("agent registration")
            ),
            crate::commands::core_navigation_host::registration::<4>(),
            crate::commands::core_navigation_host::registration::<5>(),
            crate::commands::core_navigation_host::registration::<6>(),
            Box::new(FunctionCommand::new(
                feedback::FeedbackCmd::info(),
                feedback::FeedbackCmd::execute,
            )),
            Box::new(ContextualCommand::from_contract::<hf::HfCmd>().expect("hf registration")),
            crate::commands::core_navigation_host::registration::<7>(),
            crate::commands::core_navigation_host::registration::<8>(),
            crate::commands::core_navigation_host::registration::<9>(),
            Box::new(
                ContextualCommand::from_contract::<profile::ProfileCmd>()
                    .expect("profile registration")
            ),
            Box::new(ContextualCommand::from_contract::<rlm::RlmCmd>().expect("rlm registration")),
            crate::commands::core_navigation_host::registration::<10>(),
            crate::commands::core_navigation_host::registration::<11>(),
            Box::new(FunctionCommand::new(&effort::EFFORT_INFO, effort::effort,)),
            crate::commands::core_navigation_host::registration::<12>(),
            crate::commands::core_navigation_host::registration::<13>(),
            Box::new(ContextualCommand::from_contract::<pin::PinCmd>().expect("pin registration")),
        ])
    }
}

impl From<codewhale_command_contract::outcome::CoreAction> for crate::tui::app::AppAction {
    fn from(action: codewhale_command_contract::outcome::CoreAction) -> Self {
        use codewhale_command_contract::outcome::CoreAction;
        match action {
            CoreAction::ListSubAgents => Self::ListSubAgents,
            CoreAction::SendMessage(message) => Self::SendMessage(message),
            CoreAction::LaunchSubAgent { task, max_depth } => {
                Self::SendMessage(agent_dispatch_brief(&task, max_depth))
            }
            CoreAction::CancelSubAgent { agent_id } => Self::CancelSubAgent { agent_id },
            CoreAction::Quit => Self::Quit,
            CoreAction::FetchModels => Self::FetchModels,
            CoreAction::SetAdvisorEnabled { enabled } => Self::SetAdvisorEnabled { enabled },
            CoreAction::SwitchProfile { profile } => Self::SwitchProfile { profile },
        }
    }
}

/// The model-facing /agent brief. `handle_read` is deferred on the default
/// catalog, so the brief teaches its activation path (#6747).
fn agent_dispatch_brief(task: &str, max_depth: u32) -> String {
    format!(
        "Launch one sub-agent for this task by calling `agent` with name `slash_agent`, `prompt: {task:?}`, and `max_depth: {max_depth}`. Use `handle_read` on the returned transcript_handle if you need more detail ({}). Verify any claimed side effects with `read` before reporting success.",
        codewhale_engine::tools::handle::HANDLE_READ_ACTIVATION_HINT
    )
}

#[cfg(test)]
mod agent_host_tests {
    use crate::tui::app::{App, AppAction};
    use std::path::PathBuf;

    use crate::tui::app::TuiOptions;

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &codewhale_runtime::config::Config::default())
    }

    /// #6747: the /agent brief names only callable tools and teaches the
    /// deferred `handle_read` activation path.
    #[test]
    fn agent_dispatch_brief_names_only_callable_tools() {
        codewhale_engine::tools::canonical_action_test_support::assert_text_names_only_callable_tools(
            "/agent brief",
            &super::agent_dispatch_brief("inspect the repo", 1),
        );
    }

    #[test]
    fn agent_control_actions_route_to_existing_agent_tool() {
        let mut app = test_app();
        let result = crate::commands::execute("/agent peek agent_123", &mut app);

        assert!(!result.is_error);
        let Some(AppAction::SendMessage(message)) = result.action else {
            panic!("expected SendMessage action");
        };
        assert!(message.contains("action `peek`"));
        assert!(message.contains("agent_id `agent_123`"));
        assert!(message.contains("Do not start a new agent"));

        let result = crate::commands::execute("/agent cancel agent_123", &mut app);
        let Some(AppAction::CancelSubAgent { agent_id }) = result.action else {
            panic!("expected CancelSubAgent action");
        };
        assert_eq!(agent_id, "agent_123");
    }
}
