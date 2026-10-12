//! Shell job-center commands.

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;
use crate::tui::app::{AppAction, ShellJobAction};

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "jobs",
    aliases: &["job", "zuoye"],
    usage: "/jobs [list|show <id>|poll <id>|wait <id>|stdin <id> <input>|cancel <id|all>]",
    description_key: "cmd_jobs_description",
};

pub(in crate::commands) struct JobsCmd;

impl RegisterCommand<CommandResult> for JobsCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Pure(jobs)
    }
}

fn is_cancel_all_token(id: &str) -> bool {
    id.eq_ignore_ascii_case("all")
}

fn jobs(args: Option<&str>) -> CommandResult {
    let raw = args.unwrap_or("").trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("list") {
        return CommandResult::action(AppAction::ShellJob(ShellJobAction::List));
    }

    let mut parts = raw.splitn(3, char::is_whitespace);
    let action = parts.next().unwrap_or("").to_ascii_lowercase();
    let id = parts.next().map(str::trim).filter(|s| !s.is_empty());
    let rest = parts.next().map(str::trim).unwrap_or("");

    match action.as_str() {
        "list" => CommandResult::action(AppAction::ShellJob(ShellJobAction::List)),
        "show" | "inspect" | "poll" | "wait" => match id {
            Some(id) => CommandResult::action(AppAction::ShellJob(ShellJobAction::Show {
                id: id.to_string(),
            })),
            None => CommandResult::error(format!("Usage: /jobs {action} <id>")),
        },
        "stdin" | "send" => match id {
            Some(id) if !rest.is_empty() => {
                CommandResult::action(AppAction::ShellJob(ShellJobAction::SendStdin {
                    id: id.to_string(),
                    input: rest.to_string(),
                    close: false,
                }))
            }
            _ => CommandResult::error("Usage: /jobs stdin <id> <input>"),
        },
        "close-stdin" | "eof" => match id {
            Some(id) => CommandResult::action(AppAction::ShellJob(ShellJobAction::SendStdin {
                id: id.to_string(),
                input: String::new(),
                close: true,
            })),
            None => CommandResult::error("Usage: /jobs close-stdin <id>"),
        },
        "cancel" | "kill" | "stop" => match id {
            Some(id) if is_cancel_all_token(id) => {
                CommandResult::action(AppAction::ShellJob(ShellJobAction::CancelAll))
            }
            Some(id) => CommandResult::action(AppAction::ShellJob(ShellJobAction::Cancel {
                id: id.to_string(),
            })),
            None => CommandResult::error(
                "Usage: /jobs cancel <id|all> — id is the shell_* shown on the Shells work-strip row",
            ),
        },
        "cancel-all" | "kill-all" | "stop-all" => {
            CommandResult::action(AppAction::ShellJob(ShellJobAction::CancelAll))
        }
        _ => CommandResult::error(
            "Usage: /jobs [list|show <id>|poll <id>|wait <id>|stdin <id> <input>|close-stdin <id>|cancel <id|all>]",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_job_actions() {
        let show = jobs(Some("show shell_abcd"));
        assert!(matches!(
            show.action,
            Some(AppAction::ShellJob(ShellJobAction::Show { id })) if id == "shell_abcd"
        ));

        let send = jobs(Some("stdin shell_abcd y"));
        assert!(matches!(
            send.action,
            Some(AppAction::ShellJob(ShellJobAction::SendStdin { id, input, close: false }))
                if id == "shell_abcd" && input == "y"
        ));

        let cancel_all = jobs(Some("cancel-all"));
        assert!(matches!(
            cancel_all.action,
            Some(AppAction::ShellJob(ShellJobAction::CancelAll))
        ));

        let cancel_all_spaced = jobs(Some("cancel all"));
        assert!(matches!(
            cancel_all_spaced.action,
            Some(AppAction::ShellJob(ShellJobAction::CancelAll))
        ));

        let cancel_id = jobs(Some("cancel shell_abcd"));
        assert!(matches!(
            cancel_id.action,
            Some(AppAction::ShellJob(ShellJobAction::Cancel { id })) if id == "shell_abcd"
        ));

        let bare_cancel = jobs(Some("cancel"));
        assert!(bare_cancel.action.is_none());
        assert!(
            bare_cancel
                .message
                .as_deref()
                .is_some_and(|message| message.contains("shell_*") && message.contains("<id|all>")),
            "bare cancel must name the work-strip id space, not a task: {:?}",
            bare_cancel.message
        );
        assert!(
            !bare_cancel
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Task")),
            "bare cancel must not mention Task: {:?}",
            bare_cancel.message
        );
    }

    #[test]
    fn handler_is_pure_and_argument_only() {
        assert!(matches!(JobsCmd::handler(), CommandHandler::Pure(_)));
        assert_eq!(JobsCmd::info().description_key, "cmd_jobs_description");
        assert_eq!(JobsCmd::info().aliases, &["job", "zuoye"]);
    }
}
