use super::SharedCommandHost;
use crate::composer_stash;
use codewhale_command_contract::facets::{CommandComposerContext, ParkedDraft, QueuedDrafts};

pub(super) struct ComposerAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl CommandComposerContext for ComposerAdapter<'_> {
    fn queue_previews(&self) -> QueuedDrafts {
        let app = self.host.app.borrow();
        QueuedDrafts {
            editing: app
                .queued_draft
                .as_ref()
                .map(|entry| truncate_preview(&entry.display)),
            waiting: app
                .queued_messages
                .iter()
                .map(|entry| truncate_preview(&entry.display))
                .collect(),
        }
    }

    fn editing_queue(&self) -> bool {
        self.host.app.borrow().queued_draft.is_some()
    }

    fn edit_queued(&mut self, index: usize, status: String) -> bool {
        let mut app = self.host.app.borrow_mut();
        let Some(entry) = app.remove_queued_message(index) else {
            return false;
        };
        app.input = entry.display.clone();
        app.cursor_position = app.input.len();
        app.queued_draft = Some(entry);
        app.status_message = Some(status);
        true
    }

    fn drop_queued(&mut self, index: usize) -> bool {
        self.host
            .app
            .borrow_mut()
            .remove_queued_message(index)
            .is_some()
    }

    fn clear_queued(&mut self) -> bool {
        let mut app = self.host.app.borrow_mut();
        let queued = app.queued_message_count();
        let had_draft = app.queued_draft.take().is_some();
        app.queued_messages.clear();
        app.held_restored_queue = 0;
        queued != 0 || had_draft
    }

    fn parked_drafts(&self) -> Vec<ParkedDraft> {
        composer_stash::load_stash()
            .into_iter()
            .map(|entry| ParkedDraft {
                timestamp: entry.ts,
                text: entry.text,
            })
            .collect()
    }

    fn restore_parked(&mut self) -> Option<(String, usize)> {
        let entry = composer_stash::pop_stash()?;
        let mut app = self.host.app.borrow_mut();
        app.input = entry.text.clone();
        app.cursor_position = app.input.len();
        Some((entry.text, composer_stash::load_stash().len()))
    }

    fn clear_parked(&mut self) -> Result<usize, String> {
        composer_stash::clear_stash().map_err(|error| error.to_string())
    }
}

const PREVIEW_LIMIT: usize = 120;

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
    use super::{PREVIEW_LIMIT, truncate_preview};
    use crate::commands::CommandResult;
    use crate::tui::app::{App, QueuedMessage, TuiOptions};
    use codewhale_localization::{Locale, MessageId, tr};
    use codewhale_runtime::config::Config;
    use tempfile::TempDir;

    fn queue(app: &mut App, arg: Option<&str>) -> CommandResult {
        let input = arg.map_or_else(|| "/queue".to_string(), |args| format!("/queue {args}"));
        crate::commands::execute(&input, app)
    }

    fn create_test_app_with_tmpdir(tmpdir: &TempDir) -> App {
        let options = TuiOptions {
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
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
