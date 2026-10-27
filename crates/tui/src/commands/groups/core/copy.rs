//! `/copy` command — copy the last completed assistant response.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::App;
use codewhale_localization::MessageId;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "copy",
    aliases: &[],
    usage: "/copy",
    description_id: MessageId::CmdCopyDescription,
};

pub(in crate::commands) struct CopyCmd;

impl RegisterCommand for CopyCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, _arg: Option<&str>) -> CommandResult {
        let result = execute_copy(app);
        if let Some(message) = result.message.as_ref() {
            use crate::tui::app::{StatusToast, StatusToastLevel};
            let level = if result.is_error {
                StatusToastLevel::Error
            } else {
                StatusToastLevel::Info
            };
            app.push_status_toast_record(
                StatusToast::new(message.clone(), level, Some(8_000)).for_command_receipt(),
            );
        }
        result
    }
}

fn last_completed_assistant_output(app: &App) -> Option<String> {
    app.completed_assistant_output_receipt().map(str::to_owned)
}

fn execute_copy(app: &mut App) -> CommandResult {
    let Some(content) = last_completed_assistant_output(app) else {
        return CommandResult::message(app.tr(MessageId::CmdCopyNoOutput).into_owned());
    };

    let terminal_client = app.clipboard.requires_terminal_paste();
    // Any native-host attempt may fall through to the asynchronous terminal
    // transport. Preserve /export's durable recovery contract before the
    // write so every optimistic receipt names (or explicitly lacks) a backup.
    let recovery = crate::commands::session_export_host::write_last_copy(&content);
    match app.clipboard.write_text(&content) {
        Ok(()) if terminal_client => match recovery {
            Some(path) => CommandResult::message(
                app.tr(MessageId::CmdCopyQueued)
                    .replace("{path}", &path.display().to_string()),
            ),
            None => CommandResult::message(app.tr(MessageId::CmdCopyQueuedNoBackup).into_owned()),
        },
        Ok(()) => match recovery {
            Some(path) => CommandResult::message(
                app.tr(MessageId::CmdCopySuccess)
                    .replace("{path}", &path.display().to_string()),
            ),
            None => CommandResult::message(app.tr(MessageId::CmdCopySuccessNoBackup).into_owned()),
        },
        Err(error) => match recovery {
            Some(path) => CommandResult::error(
                app.tr(MessageId::CmdCopyFailed)
                    .replace("{error}", &error.to_string())
                    .replace("{path}", &path.display().to_string()),
            ),
            None => CommandResult::error(
                app.tr(MessageId::CmdCopyFailedNoBackup)
                    .replace("{error}", &error.to_string()),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use crate::tui::clipboard::ClipboardHandler;
    use crate::tui::history::{HistoryCell, history_cells_from_message};
    use codewhale_models::{ContentBlock, Message, Role};
    use codewhale_runtime::config::Config;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn test_app() -> App {
        App::new(
            TuiOptions {
                model: "deepseek-v4-flash".to_string(),
                ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
            },
            &Config::default(),
        )
    }

    fn add_completed_assistant(app: &mut App, text: &str) -> usize {
        let history_index = app.history.len();
        app.add_message(HistoryCell::Assistant {
            content: text.to_string(),
            streaming: false,
        });
        app.record_completed_assistant_output(history_index, text);
        history_index
    }

    fn isolate_state_home(
        path: &Path,
    ) -> (
        crate::test_support::EnvVarGuard,
        crate::test_support::EnvVarGuard,
    ) {
        (
            crate::test_support::EnvVarGuard::set("HOME", path),
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", path),
        )
    }

    #[test]
    fn copies_the_latest_completed_assistant_output_only() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        add_completed_assistant(&mut app, "older");
        app.add_message(HistoryCell::System {
            content: "system".to_string(),
        });
        app.add_message(HistoryCell::Thinking {
            content: "hidden reasoning".to_string(),
            streaming: false,
            duration_secs: None,
        });
        add_completed_assistant(&mut app, "latest **answer**\nsecond line");
        // Interrupted salvage may be visible, but it never gets a completion
        // receipt and therefore cannot become `/copy` authority.
        app.add_message(HistoryCell::Assistant {
            content: "active partial".to_string(),
            streaming: false,
        });

        let result = execute_copy(&mut app);

        let expected = format!(
            "Accepted the last completed assistant response for clipboard delivery; a recovery copy is at {}",
            tmp.path().join("exports").join("last-copy.md").display()
        );
        assert_eq!(result.message.as_deref(), Some(expected.as_str()));
        assert_eq!(
            app.clipboard.last_written_text(),
            Some("latest **answer**\nsecond line")
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("exports").join("last-copy.md"))
                .expect("recovery copy"),
            "latest **answer**\nsecond line"
        );
    }

    #[test]
    fn skips_empty_and_non_assistant_history() {
        let mut app = test_app();
        add_completed_assistant(&mut app, "  \n");
        app.add_message(HistoryCell::System {
            content: "system".to_string(),
        });
        app.add_message(HistoryCell::Assistant {
            content: "partial".to_string(),
            streaming: false,
        });

        let result = execute_copy(&mut app);

        assert_eq!(
            result.message.as_deref(),
            Some("No completed assistant response is available to copy")
        );
    }

    #[test]
    fn active_turn_does_not_change_which_completed_output_is_copied() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        add_completed_assistant(&mut app, "completed before the active turn");
        app.is_loading = true;

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        assert_eq!(
            app.clipboard.last_written_text(),
            Some("completed before the active turn")
        );
        assert!(app.is_loading);
    }

    #[test]
    fn interrupted_assistant_role_never_replaces_the_last_completed_answer() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        add_completed_assistant(&mut app, "completed answer");
        app.add_message(HistoryCell::Assistant {
            content: "salvaged partial".to_string(),
            streaming: false,
        });

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        assert_eq!(app.clipboard.last_written_text(), Some("completed answer"));
    }

    #[test]
    fn compacted_context_uses_the_typed_completed_output_receipt() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        add_completed_assistant(&mut app, "visible answer before compaction");
        app.api_messages = std::sync::Arc::new(vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "compaction checkpoint".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::InterruptedAssistant,
                content: vec![ContentBlock::Text {
                    text: "partial after compaction".to_string(),
                    cache_control: None,
                }],
            },
        ]);

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        assert_eq!(
            app.clipboard.last_written_text(),
            Some("visible answer before compaction")
        );
    }

    #[test]
    fn completed_receipt_survives_history_folding() {
        let mut app = test_app();
        for index in 0..(App::HISTORY_SOFT_CAP - 1) {
            app.add_message(HistoryCell::System {
                content: format!("status {index}"),
            });
        }
        add_completed_assistant(&mut app, "completed answer before fold");
        app.add_message(HistoryCell::System {
            content: "trigger fold".to_string(),
        });

        assert_eq!(
            app.completed_assistant_output_receipt(),
            Some("completed answer before fold")
        );
    }

    #[test]
    fn popping_the_latest_cell_reveals_the_prior_completed_receipt() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        add_completed_assistant(&mut app, "older answer");
        add_completed_assistant(&mut app, "newer answer");
        app.api_messages_mut().clear();
        app.pop_history();

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        assert_eq!(app.clipboard.last_written_text(), Some("older answer"));
    }

    #[test]
    fn backtrack_truncation_drops_receipts_after_the_selected_boundary() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        add_completed_assistant(&mut app, "older answer");
        app.add_message(HistoryCell::User {
            content: "next prompt".to_string(),
        });
        add_completed_assistant(&mut app, "newer answer");
        app.api_messages_mut().clear();
        app.truncate_history_to(2);

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        assert_eq!(app.clipboard.last_written_text(), Some("older answer"));
    }

    #[test]
    fn restored_repair_receipt_is_never_copyable_assistant_output() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        let visible = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "real assistant answer".to_string(),
                cache_control: None,
            }],
        };
        let repair = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "[tool_history_repair] synthetic recovery receipt".to_string(),
                cache_control: None,
            }],
        };
        app.extend_history(history_cells_from_message(&visible));
        app.extend_history(history_cells_from_message(&repair));
        app.rebuild_completed_assistant_outputs_from_restored_history();

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        assert_eq!(
            app.clipboard.last_written_text(),
            Some("real assistant answer")
        );
    }

    #[test]
    fn successful_delivery_without_recovery_file_is_explicit() {
        let tmp = TempDir::new().expect("tempdir");
        let unusable_home = tmp.path().join("home-file");
        std::fs::write(&unusable_home, "not a directory").expect("home fixture");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(&unusable_home);
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(false, false);
        add_completed_assistant(&mut app, "answer");

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        let message = result.message.as_deref().unwrap_or_default();
        assert!(
            message.contains("no recovery file could be written"),
            "{message}"
        );
        assert!(message.contains("/export file <path>"), "{message}");
    }

    #[test]
    fn terminal_client_copy_is_queued_and_names_the_recovery_file() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::for_test(true, true);
        add_completed_assistant(&mut app, "remote answer");

        let result = execute_copy(&mut app);

        assert!(!result.is_error);
        let message = result.message.as_deref().unwrap_or_default();
        assert!(message.contains("Queued"), "{message}");
        assert!(message.contains("last-copy.md"), "{message}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("exports").join("last-copy.md"))
                .expect("recovery copy"),
            "remote answer"
        );
    }

    #[test]
    fn clipboard_failure_names_the_written_recovery_file() {
        let tmp = TempDir::new().expect("tempdir");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::unavailable_for_test(false);
        add_completed_assistant(&mut app, "recoverable answer");

        let result = execute_copy(&mut app);

        assert!(result.is_error);
        let message = result.message.as_deref().unwrap_or_default();
        assert!(message.contains("last-copy.md"), "{message}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("exports").join("last-copy.md"))
                .expect("recovery copy"),
            "recoverable answer"
        );
    }

    #[test]
    fn clipboard_and_recovery_failure_explain_the_explicit_export_path() {
        let tmp = TempDir::new().expect("tempdir");
        let unusable_home = tmp.path().join("home-file");
        std::fs::write(&unusable_home, "not a directory").expect("home fixture");
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _codewhale_home) = isolate_state_home(&unusable_home);
        let mut app = test_app();
        app.clipboard = ClipboardHandler::unavailable_for_test(false);
        add_completed_assistant(&mut app, "answer");

        let result = execute_copy(&mut app);

        assert!(result.is_error);
        let message = result.message.as_deref().unwrap_or_default();
        assert!(message.contains("/export file <path>"), "{message}");
    }
    #[test]
    fn copy_command_failure_surfaces_the_existing_recovery_receipt_over_a_finished_turn() {
        let tmp = TempDir::new().unwrap();
        let _env_lock = crate::test_support::lock_test_env();
        let (_home, _state) = isolate_state_home(tmp.path());
        let mut app = test_app();
        app.clipboard = ClipboardHandler::unavailable_for_test(false);
        add_completed_assistant(&mut app, "recoverable answer");
        let result = CopyCmd::execute(&mut app, None);
        assert!(result.is_error);
        let toast = app
            .active_status_toast(crate::tui::underwater::ShellPhase::Done)
            .expect("copy failure must stay visible");
        assert_eq!(toast.text, result.message.unwrap());
        assert_eq!(toast.level, crate::tui::app::StatusToastLevel::Error);
        assert_eq!(toast.ttl_ms, Some(8_000));
        assert!(toast.text.contains("last-copy.md"));
    }
}
