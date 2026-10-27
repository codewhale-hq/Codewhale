//! Queue commands: queue list/edit/drop/clear

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::App;
use codewhale_localization::{Locale, MessageId, tr};

use super::CommandResult;

const PREVIEW_LIMIT: usize = 120;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "queue",
    aliases: &["queued"],
    usage: "/queue [list|send <n>|edit <n>|drop <n>|clear]",
    description_id: MessageId::CmdQueueDescription,
};

pub(in crate::commands) struct QueueCmd;

impl RegisterCommand for QueueCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        queue(app, arg)
    }
}

pub fn queue(app: &mut App, args: Option<&str>) -> CommandResult {
    let locale = app.ui_locale;
    let arg = args.unwrap_or("").trim();
    if arg.is_empty() || arg.eq_ignore_ascii_case("list") {
        return list_queue(app);
    }

    let mut parts = arg.split_whitespace();
    let action = parts.next().unwrap_or("").to_lowercase();

    match action.as_str() {
        "edit" => edit_queue(app, parts.next()),
        "drop" | "remove" | "rm" => drop_queue(app, parts.next()),
        "clear" => clear_queue(app),
        _ => CommandResult::error(tr(locale, MessageId::CmdQueueUsage)),
    }
}

fn list_queue(app: &mut App) -> CommandResult {
    let locale = app.ui_locale;
    let mut lines = Vec::new();
    let queued = app.queued_message_count();

    if let Some(draft) = app.queued_draft.as_ref() {
        lines.push("Editing queued message:".to_string());
        lines.push(format!("- {}", truncate_preview(&draft.display)));
    }

    if queued == 0 {
        if lines.is_empty() {
            return CommandResult::message(tr(locale, MessageId::CmdQueueNoMessages));
        }
        return CommandResult::message(lines.join("\n"));
    }

    lines.push(tr(locale, MessageId::CmdQueueListHeader).replace("{count}", &queued.to_string()));
    for (idx, message) in app.queued_messages.iter().enumerate() {
        lines.push(format!(
            "{}. {}",
            idx + 1,
            truncate_preview(&message.display)
        ));
    }

    lines.push(tr(locale, MessageId::CmdQueueTip).to_string());

    CommandResult::message(lines.join("\n"))
}

fn edit_queue(app: &mut App, index: Option<&str>) -> CommandResult {
    let locale = app.ui_locale;
    if app.queued_draft.is_some() {
        return CommandResult::error(tr(locale, MessageId::CmdQueueAlreadyEditing));
    }
    let index = match parse_index(index, locale) {
        Ok(index) => index,
        Err(err) => return CommandResult::error(err),
    };

    let Some(message) = app.remove_queued_message(index) else {
        return CommandResult::error(tr(locale, MessageId::CmdQueueNotFound));
    };

    app.input = message.display.clone();
    app.cursor_position = app.input.len();
    app.queued_draft = Some(message);
    let status =
        tr(locale, MessageId::CmdQueueEditingStatus).replace("{index}", &(index + 1).to_string());
    app.status_message = Some(status);

    CommandResult::message(
        tr(locale, MessageId::CmdQueueEditingMessage).replace("{index}", &(index + 1).to_string()),
    )
}

fn drop_queue(app: &mut App, index: Option<&str>) -> CommandResult {
    let locale = app.ui_locale;
    let index = match parse_index(index, locale) {
        Ok(index) => index,
        Err(err) => return CommandResult::error(err),
    };

    if app.remove_queued_message(index).is_none() {
        return CommandResult::error(tr(locale, MessageId::CmdQueueNotFound));
    }

    CommandResult::message(
        tr(locale, MessageId::CmdQueueDropped).replace("{index}", &(index + 1).to_string()),
    )
}

fn clear_queue(app: &mut App) -> CommandResult {
    let locale = app.ui_locale;
    let queued = app.queued_message_count();
    let had_draft = app.queued_draft.take().is_some();
    app.queued_messages.clear();
    app.held_restored_queue = 0;
    if queued == 0 && !had_draft {
        return CommandResult::message(tr(locale, MessageId::CmdQueueAlreadyEmpty));
    }

    CommandResult::message(tr(locale, MessageId::CmdQueueCleared))
}

fn parse_index(input: Option<&str>, locale: Locale) -> Result<usize, String> {
    let Some(input) = input else {
        return Err(tr(locale, MessageId::CmdQueueMissingIndex).to_string());
    };
    let raw = input
        .parse::<usize>()
        .map_err(|_| tr(locale, MessageId::CmdQueueIndexPositive).to_string())?;
    if raw == 0 {
        return Err(tr(locale, MessageId::CmdQueueIndexMin).to_string());
    }
    Ok(raw - 1)
}

fn truncate_preview(text: &str) -> String {
    if text.chars().count() <= PREVIEW_LIMIT {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars().take(PREVIEW_LIMIT.saturating_sub(3)) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, QueuedMessage, TuiOptions};
    use codewhale_runtime::config::Config;
    use tempfile::TempDir;

    fn create_test_app_with_tmpdir(tmpdir: &TempDir) -> App {
        let options = TuiOptions {
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(tmpdir.path())
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn test_queue_list_empty() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        let result = queue(&mut app, None);
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(msg.contains(&*tr(app.ui_locale, MessageId::CmdQueueNoMessages)));
    }

    #[test]
    fn test_queue_list_with_messages() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        app.queued_messages
            .push_back(QueuedMessage::new("First message".to_string(), None));
        app.queued_messages
            .push_back(QueuedMessage::new("Second message".to_string(), None));
        let result = queue(&mut app, Some("list"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&tr(app.ui_locale, MessageId::CmdQueueListHeader).replace("{count}", "2"))
        );
        assert!(msg.contains("1. First message"));
        assert!(msg.contains("2. Second message"));
    }

    #[test]
    fn test_queue_edit_missing_index() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        app.queued_messages
            .push_back(QueuedMessage::new("Test".to_string(), None));
        let result = queue(&mut app, Some("edit"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&*tr(Locale::En, MessageId::CmdQueueMissingIndex)),
            "msg={msg:?}"
        );
    }

    #[test]
    fn test_queue_edit_invalid_index() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        let result = queue(&mut app, Some("edit abc"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&*tr(Locale::En, MessageId::CmdQueueIndexPositive)),
            "msg={msg:?}"
        );
    }

    #[test]
    fn test_queue_edit_not_found() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        let result = queue(&mut app, Some("edit 1"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&*tr(Locale::En, MessageId::CmdQueueNotFound)),
            "msg={msg:?}"
        );
    }

    #[test]
    fn test_queue_edit_already_editing() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        app.queued_messages
            .push_back(QueuedMessage::new("First".to_string(), None));
        app.queued_messages
            .push_back(QueuedMessage::new("Second".to_string(), None));
        // Start editing
        queue(&mut app, Some("edit 1"));
        // Try to edit another
        let result = queue(&mut app, Some("edit 2"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&*tr(Locale::En, MessageId::CmdQueueAlreadyEditing)),
            "msg={msg:?}"
        );
    }

    #[test]
    fn test_queue_edit_success() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        app.queued_messages
            .push_back(QueuedMessage::new("Original message".to_string(), None));
        let result = queue(&mut app, Some("edit 1"));
        assert!(result.message.is_some());
        assert_eq!(app.input, "Original message");
        assert_eq!(app.cursor_position, app.input.len());
        assert!(app.queued_draft.is_some());
    }

    #[test]
    fn test_queue_drop_success() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        app.queued_messages
            .push_back(QueuedMessage::new("To drop".to_string(), None));
        let initial_count = app.queued_messages.len();
        let result = queue(&mut app, Some("drop 1"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&tr(Locale::En, MessageId::CmdQueueDropped).replace("{index}", "1")),
            "msg={msg:?}"
        );
        assert_eq!(app.queued_messages.len(), initial_count - 1);
    }

    #[test]
    fn test_queue_clear() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        app.queued_messages
            .push_back(QueuedMessage::new("Message 1".to_string(), None));
        app.queued_messages
            .push_back(QueuedMessage::new("Message 2".to_string(), None));
        let result = queue(&mut app, Some("clear"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&*tr(Locale::En, MessageId::CmdQueueCleared)),
            "msg={msg:?}"
        );
        assert!(app.queued_messages.is_empty());
    }

    #[test]
    fn test_queue_clear_already_empty() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::En;
        let result = queue(&mut app, Some("clear"));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(
            msg.contains(&*tr(Locale::En, MessageId::CmdQueueAlreadyEmpty)),
            "msg={msg:?}"
        );
    }

    #[test]
    fn queue_messages_are_localized() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.ui_locale = Locale::ZhHans;
        app.queued_messages
            .push_back(QueuedMessage::new("M1".to_string(), None));
        app.queued_messages
            .push_back(QueuedMessage::new("M2".to_string(), None));
        let result = queue(&mut app, Some("list"));
        let msg = result.message.unwrap();
        assert!(msg.contains("已排队的消息"), "zh list header: {msg}");
        assert!(msg.contains("提示"), "zh tip: {msg}");
    }

    #[test]
    fn test_truncate_preview_short_text() {
        let result = truncate_preview("Short text");
        assert_eq!(result, "Short text");
    }

    #[test]
    fn test_truncate_preview_long_text() {
        let long_text = "x".repeat(200);
        let result = truncate_preview(&long_text);
        assert!(result.len() <= PREVIEW_LIMIT + 3);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_preview_unicode() {
        let text = "Hello 世界 🌍";
        let result = truncate_preview(text);
        assert_eq!(result, text);
    }
}
