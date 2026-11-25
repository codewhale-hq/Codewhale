//! Utility command area: attachments, background tasks, jobs, MCP, network
//! inspection, and self-update.

mod attachment;
mod automation;
mod jobs;
mod mcp;
mod network;
mod task;
mod update;

use crate::commands::traits::{Command, CommandGroup, ContextualCommand};

pub struct UtilityCommands;

impl CommandGroup for UtilityCommands {
    fn commands(&self) -> &'static [Box<dyn Command>] {
        cached_command_list!(vec![
            Box::new(
                ContextualCommand::from_contract::<attachment::AttachCmd>()
                    .expect("attach registration"),
            ),
            Box::new(
                ContextualCommand::from_contract::<automation::AutomationCmd>()
                    .expect("automation registration"),
            ),
            Box::new(
                ContextualCommand::from_contract::<task::TaskCmd>().expect("task registration"),
            ),
            Box::new(
                ContextualCommand::from_contract::<jobs::JobsCmd>().expect("jobs registration"),
            ),
            Box::new(ContextualCommand::from_contract::<mcp::McpCmd>().expect("mcp registration"),),
            Box::new(
                ContextualCommand::from_contract::<network::NetworkCmd>()
                    .expect("network registration"),
            ),
            Box::new(
                ContextualCommand::from_contract::<update::UpdateCmd>()
                    .expect("update registration"),
            ),
        ])
    }
}
