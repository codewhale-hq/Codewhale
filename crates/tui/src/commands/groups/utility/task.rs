//! Task commands: add/list/show/cancel

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;
use crate::tui::app::AppAction;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "task",
    aliases: &["tasks"],
    usage: "/task [add <prompt>|list|digest|show <id>|cancel <id>]",
    description_key: "cmd_task_description",
};

pub(in crate::commands) struct TaskCmd;

impl RegisterCommand<CommandResult> for TaskCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKSPACE,
            handler: task_contextual,
        }
    }
}

fn task_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(workspace) = parts.workspace.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    task(workspace, arg)
}

fn task(
    workspace: &mut dyn codewhale_command_contract::facets::CommandWorkspaceContext,
    args: Option<&str>,
) -> CommandResult {
    let raw = args.unwrap_or("").trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("list") {
        return CommandResult::action(AppAction::TaskList);
    }

    let mut parts = raw.splitn(2, char::is_whitespace);
    let action = parts.next().unwrap_or("").to_ascii_lowercase();
    let remainder = parts.next().map(str::trim).filter(|s| !s.is_empty());

    match action.as_str() {
        "add" => {
            let Some(prompt) = remainder else {
                return CommandResult::error("Usage: /task add <prompt>");
            };
            CommandResult::action(AppAction::TaskAdd {
                prompt: prompt.to_string(),
            })
        }
        "list" => CommandResult::action(AppAction::TaskList),
        "digest" => match workspace.operation_digest() {
            Ok(text) => CommandResult::message(text),
            Err(error) => CommandResult::error(error),
        },
        "show" => {
            let Some(id) = remainder else {
                return CommandResult::error("Usage: /task show <id>");
            };
            CommandResult::action(AppAction::TaskShow { id: id.to_string() })
        }
        "cancel" | "stop" => {
            let Some(id) = remainder else {
                return CommandResult::error("Usage: /task cancel <id>");
            };
            CommandResult::action(AppAction::TaskCancel { id: id.to_string() })
        }
        _ => CommandResult::error("Usage: /task [add <prompt>|list|digest|show <id>|cancel <id>]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct FakeWorkspace;
    impl codewhale_command_contract::facets::CommandWorkspaceContext for FakeWorkspace {
        fn effective_home_dir(&self) -> Option<std::path::PathBuf> {
            unreachable!("navigation is not used by task")
        }
        fn inspect_workspace_target(
            &self,
            _: &std::path::Path,
        ) -> codewhale_command_contract::navigation::WorkspaceTarget {
            unreachable!("navigation is not used by task")
        }
        fn read_anchor_document(&self) -> Result<Option<String>, String> {
            unreachable!("anchors are not used by task")
        }
        fn append_anchor_bytes(
            &mut self,
            _: &[&[u8]],
        ) -> Result<std::path::PathBuf, codewhale_command_contract::navigation::AnchorWriteError>
        {
            unreachable!("anchors are not used by task")
        }
        fn replace_anchor_document(&mut self, _: &[u8]) -> Result<(), String> {
            unreachable!("anchors are not used by task")
        }
        fn workspace(&self) -> PathBuf {
            PathBuf::from(".")
        }
        fn work_state_snapshot(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn operation_digest(&mut self) -> Result<String, String> {
            Ok("No active operations or to-do items.".to_string())
        }
    }

    struct FailingWorkspace;
    impl codewhale_command_contract::facets::CommandWorkspaceContext for FailingWorkspace {
        fn effective_home_dir(&self) -> Option<std::path::PathBuf> {
            unreachable!("navigation is not used by task")
        }
        fn inspect_workspace_target(
            &self,
            _: &std::path::Path,
        ) -> codewhale_command_contract::navigation::WorkspaceTarget {
            unreachable!("navigation is not used by task")
        }
        fn read_anchor_document(&self) -> Result<Option<String>, String> {
            unreachable!("anchors are not used by task")
        }
        fn append_anchor_bytes(
            &mut self,
            _: &[&[u8]],
        ) -> Result<std::path::PathBuf, codewhale_command_contract::navigation::AnchorWriteError>
        {
            unreachable!("anchors are not used by task")
        }
        fn replace_anchor_document(&mut self, _: &[u8]) -> Result<(), String> {
            unreachable!("anchors are not used by task")
        }
        fn workspace(&self) -> PathBuf {
            PathBuf::from(".")
        }
        fn work_state_snapshot(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn operation_digest(&mut self) -> Result<String, String> {
            Err("Operation digest is temporarily unavailable: boom".to_string())
        }
    }

    #[test]
    fn parses_add_and_cancel() {
        let add = task(&mut FakeWorkspace, Some("add write tests"));
        assert!(matches!(
            add.action,
            Some(AppAction::TaskAdd { prompt }) if prompt == "write tests"
        ));

        let cancel = task(&mut FakeWorkspace, Some("cancel task_1234"));
        assert!(matches!(
            cancel.action,
            Some(AppAction::TaskCancel { id }) if id == "task_1234"
        ));
    }

    #[test]
    fn validates_usage() {
        let result = task(&mut FakeWorkspace, Some("add"));
        assert!(result.message.is_some());
        assert!(result.action.is_none());
    }

    #[test]
    fn digest_uses_canonical_work_runtime_without_another_state_store() {
        let result = task(&mut FakeWorkspace, Some("digest"));
        assert_eq!(
            result.message.as_deref(),
            Some("No active operations or to-do items.")
        );
        assert!(result.action.is_none());

        let failing = task(&mut FailingWorkspace, Some("digest"));
        assert!(failing.is_error);
        assert!(
            failing
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Operation digest is temporarily unavailable: boom")
        );
    }

    #[test]
    fn handler_is_contextual() {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = TaskCmd::handler()
        else {
            panic!("task must be contextual");
        };
        assert_eq!(capabilities, CommandCapabilities::WORKSPACE);
        let missing = handler(CommandContexts::empty(), Some("list"));
        assert!(missing.is_error);
        assert_eq!(
            missing.message.as_deref(),
            Some("Error: Command capability unavailable: workspace")
        );
        assert_eq!(TaskCmd::info().description_key, "cmd_task_description");
        assert_eq!(TaskCmd::info().aliases, &["tasks"]);
    }
}
