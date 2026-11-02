//! Preserved snapshot/history safety tests outside the portable debug group.
use super::CommandResult;
use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_commands::debug::undo;
fn patch_undo(app: &mut App) -> CommandResult {
    if app.api_messages.is_empty() && app.history.is_empty() {
        return CommandResult::message("Nothing to undo");
    }
    let restored = (|| {
        let cut = crate::tui::ui::backtrack_api_cut_index(app, 0).ok_or_else(|| {
            anyhow::anyhow!("The selected request cannot be matched safely; nothing was undone.")
        })?;
        let input = super::undo_range_input(app, cut)?;
        let plan = super::plan_undo_range(input)?;
        super::restore_undo_range(plan)
    })();
    match restored {
        Ok(summary) => {
            let mut result = undo_conversation(app);
            result.message = Some(summary);
            result
        }
        Err(error) => CommandResult::error(format!("Nothing was undone. {error:#}")),
    }
}
fn undo_conversation(app: &mut App) -> CommandResult {
    super::debug_group::host_result(undo::conversation_result(
        super::contract::debug_operations::undo_conversation_for_engine(app),
        &UndoPresentation,
    ))
}
fn acknowledge_file_undo(app: &mut App, result: &CommandResult) {
    let Some(AppAction::ConversationUndo { sync, .. }) = &result.action else {
        panic!("file undo did not request a durable conversation rewind");
    };
    assert!(app.api_messages.len() > sync.messages.len());
    if let Some(index) = app
        .history
        .iter()
        .rposition(|cell| matches!(cell, HistoryCell::User { .. }))
    {
        app.truncate_history_to(index);
    }
    app.set_api_messages(std::sync::Arc::new(sync.messages.clone()));
}
fn retry(app: &mut App) -> CommandResult {
    super::execute("/retry", app)
}
use crate::tui::app::{App, AppAction, TuiOptions};
use crate::tui::history::HistoryCell;
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message, Tool};
use codewhale_runtime::config::Config;
use std::path::PathBuf;

struct UndoPresentation;
impl CommandPresentationContext for UndoPresentation {
    fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String> {
        let catalog: serde_json::Value =
            serde_json::from_str(include_str!("../../../localization/locales/en.json")).unwrap();
        let id: String = key
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .unwrap()
                    .to_uppercase()
                    .chain(chars)
                    .collect::<String>()
            })
            .collect();
        let mut text = catalog[&id]
            .as_str()
            .ok_or_else(|| format!("unknown test key {key}"))?
            .to_string();
        for (name, value) in replacements {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        Ok(text)
    }
}

pub(in crate::commands) fn create_test_app() -> App {
    let options = TuiOptions {
        skills_dir: PathBuf::from("/tmp/test-skills"),
        ..crate::tui::test_support::test_tui_options(PathBuf::from("/tmp/test-workspace"))
    };
    let mut app = App::new(options, &Config::default());
    app.ui_locale = codewhale_localization::Locale::En;
    app.cost_currency = codewhale_config::pricing::CostCurrency::Usd;
    app.api_provider = codewhale_config::ProviderKind::Deepseek;
    app
}

#[test]
fn edit_dispatch_loads_unicode_composer_without_truncating_history() {
    let mut app = create_test_app();
    app.push_history_cell(HistoryCell::User {
        content: "edit 漢字🙂".into(),
    });
    let count = app.history.len();
    let result = super::execute("/edit", &mut app);
    assert_eq!(
        result.message.as_deref(),
        Some("Last message loaded into composer — edit and press Enter to resubmit")
    );
    assert_eq!(app.input, "edit 漢字🙂");
    assert_eq!(app.cursor_position, "edit 漢字🙂".chars().count());
    assert!(app.edit_in_progress);
    assert_eq!(app.history.len(), count);
    assert!(result.action.is_none());
    assert!(!result.is_error);
}

/// A queued follow-up open for editing must not be overwritten or later sent
/// in place of the `/edit` revision: it goes back to the queue, text intact.
#[test]
fn edit_dispatch_returns_an_open_queued_draft_to_the_queue() {
    use crate::tui::app::QueuedMessage;
    let mut app = create_test_app();
    app.push_history_cell(HistoryCell::User {
        content: "last sent".into(),
    });
    app.queued_messages
        .push_back(QueuedMessage::new("queued follow-up".to_string(), None));
    assert!(app.pop_last_queued_into_draft());
    assert_eq!(app.input, "queued follow-up");
    assert!(app.queued_draft.is_some());

    let result = super::execute("/edit", &mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert!(app.queued_draft.is_none(), "draft edit is closed");
    assert_eq!(
        app.queued_messages
            .iter()
            .map(|message| message.display.as_str())
            .collect::<Vec<_>>(),
        ["queued follow-up"],
        "the queued follow-up is back in the queue, exactly once"
    );
    assert_eq!(app.input, "last sent");
    assert!(app.edit_in_progress);
}

#[test]
fn diff_dispatch_reads_only_the_apps_workspace_without_changing_files() {
    let workspace = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    };
    git(&["init", "--quiet"]);
    std::fs::write(workspace.path().join("tracked.txt"), "before\n").unwrap();
    git(&["add", "tracked.txt"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "-m",
        "fixture",
    ]);
    let mut app = create_test_app();
    app.workspace = workspace.path().to_path_buf();
    assert_eq!(
        super::execute("/diff", &mut app).message.as_deref(),
        Some("No changes since session start")
    );
    std::fs::write(workspace.path().join("tracked.txt"), "after\n").unwrap();
    let stat = git(&["diff", "--stat"]);
    let result = super::execute("/diff", &mut app);
    assert_eq!(
        result.message,
        Some(format!(
            "Changed files (1):\ntracked.txt\n\n── Stat ──\n{}",
            stat.trim()
        ))
    );
    assert!(!result.is_error);
    // The changed lines themselves, not only the names and counts.
    let Some(AppAction::OpenDiffPager { diff, .. }) = result.action else {
        panic!("expected the diff pager: {:?}", result.action);
    };
    assert!(diff.contains("-before\n+after\n"), "{diff}");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("tracked.txt")).unwrap(),
        "after\n"
    );
}

/// In a folder that is not a git repository `/diff` used to answer "No
/// changes since session start" after real edits (0.10.1 tutorial, lesson 1).
#[test]
fn diff_outside_a_git_repository_says_so_instead_of_no_changes() {
    let workspace = tempfile::tempdir().unwrap();
    let inside_a_repository = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace.path())
        .output()
        .is_ok_and(|probe| probe.status.success());
    if inside_a_repository {
        // The temp directory itself lives in a repository on this machine.
        return;
    }
    std::fs::write(workspace.path().join("edited.txt"), "after\n").unwrap();
    let mut app = create_test_app();
    app.workspace = workspace.path().to_path_buf();

    let message = super::execute("/diff", &mut app)
        .message
        .unwrap_or_default();

    // No restore point and no repository: nothing to compare against, which
    // is said plainly rather than reported as "no changes" or as a failure.
    assert_eq!(
        message,
        "Nothing to compare yet: this session has not saved a restore point here, and this folder is not a git repository."
    );
}

pub(in crate::commands) fn test_tool(name: &str) -> Tool {
    Tool {
        tool_type: Some("function".to_string()),
        name: name.to_string(),
        description: format!("{name} test tool"),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            }
        }),
        allowed_callers: None,
        defer_loading: Some(false),
        input_examples: None,
        strict: Some(true),
        cache_control: None,
    }
}

#[test]
fn test_undo_conversation_stages_last_exchange_without_mutating_live_history() {
    let mut app = create_test_app();
    app.history.push(HistoryCell::User {
        content: "Hello".to_string(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "Hi".to_string(),
        streaming: false,
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Hello".to_string(),
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Hi".to_string(),
            cache_control: None,
        }],
    });

    let initial_history_len = app.history.len();
    let initial_api_len = app.api_messages.len();
    let result = undo_conversation(&mut app);

    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Removed"));
    assert_eq!(
        app.history.len(),
        initial_history_len,
        "planning leaves live UI untouched"
    );
    assert_eq!(app.api_messages.len(), initial_api_len);
    assert!(
        matches!(result.action, Some(AppAction::ConversationUndo { sync, retry_input: None, .. }) if sync.messages.is_empty())
    );
}

#[test]
fn conversation_undo_includes_following_tool_results_and_runtime_notes() {
    let mut app = create_test_app();
    app.history.push(HistoryCell::User {
        content: "undo this".into(),
    });
    let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
        {"role":"user","content":[{"type":"text","text":"keep this"}]},
        {"role":"assistant","content":[{"type":"text","text":"kept answer"}]},
        {"role":"user","content":[{"type":"text","text":"undo this"}]},
        {"role":"assistant","content":[{"type":"tool_use","id":"call-1","name":"read_file","input":{}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"undone tool result"}]},
        {"role":"assistant","content":[{"type":"text","text":"undone answer"}]}
    ])).unwrap();
    app.set_api_messages(std::sync::Arc::new(messages.clone()));
    let result = undo_conversation(&mut app);
    let Some(AppAction::ConversationUndo { sync, .. }) = result.action else {
        panic!("missing rollback")
    };
    assert_eq!(sync.messages, messages[..2]);
    assert_eq!(app.api_messages.as_ref(), &messages);
}

#[test]
fn test_undo_conversation_nothing_to_undo() {
    let mut app = create_test_app();
    // Clear any default history
    app.history.clear();
    app.api_messages_mut().clear();
    let result = undo_conversation(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Nothing to undo") || msg.contains("Removed"));
}

#[test]
fn test_retry_with_previous_message() {
    let mut app = create_test_app();
    app.history.push(HistoryCell::User {
        content: "Test message".to_string(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "Response".to_string(),
        streaming: false,
    });

    let result = retry(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Retrying"));
    assert!(msg.contains("Test message"));
    assert!(matches!(
        result.action.as_ref(),
        Some(AppAction::ConversationUndo { retry_input: Some(input), .. }) if input == "Test message"
    ));
}

#[test]
fn test_retry_no_previous_message() {
    let mut app = create_test_app();
    let result = retry(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("No previous request to retry"));
    assert!(result.action.is_none());
}

#[test]
fn test_retry_truncates_long_input() {
    let mut app = create_test_app();
    let long_input = "x".repeat(100);
    app.history.push(HistoryCell::User {
        content: long_input.clone(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "Response".to_string(),
        streaming: false,
    });

    let result = retry(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Retrying"));
    assert!(msg.contains("..."));
}

#[test]
fn test_patch_undo_requests_durable_rewind_after_restore() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "original");
    let messages = fx.record_turn("s1", 1, "please edit a.txt", &[("a.txt", "modified")], true);
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages.clone());
    app.is_loading = true;
    for command in ["/undo", "/undo force"] {
        let result = super::execute(command, &mut app);
        assert!(matches!(
            result.action,
            Some(AppAction::UndoLatestRequest { force }) if force == command.ends_with(" force")
        ));
        assert_eq!(fx.read("a.txt"), "modified");
        assert_eq!(app.api_messages.as_ref(), &messages);
    }
    app.is_loading = false;
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "original");
    assert_eq!(app.api_messages.as_ref(), &messages);
    assert!(
        matches!(result.action, Some(AppAction::ConversationUndo { ref sync, .. }) if sync.messages.is_empty())
    );
    acknowledge_file_undo(&mut app, &result);
    assert!(app.api_messages.is_empty() && app.history.is_empty());
}

#[test]
fn test_undo_legacy_chain_preserves_files_without_writer_receipts() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "zero");
    fx.snapshot("tool:provider-call", "s1");
    fx.write("a.txt", "two");
    let mut app = fx.app("s1");
    push_exchange(&mut app, "chat only");
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "two");
    assert!(matches!(
        result.action,
        Some(AppAction::ConversationUndo { .. })
    ));
}

#[test]
fn test_patch_undo_prunes_pre_turn_context() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "alpha");
    let messages = fx.record_turn(
        "s1",
        1,
        "please edit a.txt",
        &[("a.txt", "alpha-fixed")],
        true,
    );
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "alpha");
    assert_eq!(app.history.len(), 2);
    acknowledge_file_undo(&mut app, &result);
    assert!(app.history.is_empty() && app.api_messages.is_empty());
}

#[test]
fn test_patch_undo_never_crosses_session_boundary() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a-before");
    let foreign = fx.record_turn("session-a", 1, "edit a.txt", &[("a.txt", "a-after")], true);
    fx.write("a.txt", "b-before");
    let current = fx.record_turn("session-b", 1, "edit a.txt", &[("a.txt", "b-after")], true);
    let mut wrong = fx.app("session-b");
    push_recorded_turn(&mut wrong, foreign);
    let refused = patch_undo(&mut wrong);
    assert!(refused.is_error, "{:?}", refused.message);
    assert_eq!(fx.read("a.txt"), "b-after");
    let mut app = fx.app("session-b");
    push_recorded_turn(&mut app, current);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "b-before");
    acknowledge_file_undo(&mut app, &result);
    let repeated = patch_undo(&mut app);
    assert_eq!(repeated.message.as_deref(), Some("Nothing to undo"));
    assert_eq!(fx.read("a.txt"), "b-before");
}

#[test]
fn test_undo_reports_that_files_were_not_reverted_when_the_repo_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::test_support::SealedHome::at(tmp.path());
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, "user bytes").unwrap();
    let mut app = create_test_app();
    app.workspace = tmp.path().to_path_buf();
    app.current_session_id = Some("s1".to_string());
    push_exchange(&mut app, "change something");
    app.api_messages_mut()
        .insert(1, fixture_call("host-call", "a.txt", "new bytes"));
    let before = app.api_messages.as_ref().clone();
    let result = patch_undo(&mut app);
    assert!(result.is_error, "{:?}", result.message);
    assert!(result.action.is_none());
    assert_eq!(app.api_messages.as_ref(), &before);
    assert_eq!(std::fs::read_to_string(file).unwrap(), "user bytes");
}

struct UndoFixture {
    workspace: PathBuf,
    repo: codewhale_snapshot::SnapshotRepo,
    _home: crate::test_support::SealedHome,
    _tmp: tempfile::TempDir,
}

impl UndoFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = crate::test_support::SealedHome::at(tmp.path());
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let repo = codewhale_snapshot::SnapshotRepo::open_or_init(&workspace).unwrap();
        Self {
            workspace,
            repo,
            _home: home,
            _tmp: tmp,
        }
    }
    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.workspace.join(name), body).unwrap();
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.workspace.join(name)).unwrap()
    }
    fn snapshot(&self, label: &str, session: &str) {
        self.repo.take_snapshot(label, Some(session)).unwrap();
    }
    fn app(&self, session: &str) -> App {
        let mut app = create_test_app();
        app.workspace = self.workspace.clone();
        app.current_session_id = Some(session.to_string());
        app
    }
    fn record_turn(
        &self,
        session: &str,
        seq: u64,
        prompt: &str,
        writes: &[(&str, &str)],
        complete: bool,
    ) -> Vec<Message> {
        self.snapshot(
            &codewhale_engine::core::turn::format_snapshot_label("pre-turn", seq, Some(prompt)),
            session,
        );
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
                cache_control: None,
            }],
        }];
        for (path, body) in writes {
            let id = uuid::Uuid::new_v4().to_string();
            self.snapshot(&format!("tool:{id}"), session);
            messages.push(fixture_call(&id, path, body));
            self.write(path, body);
            self.snapshot(&format!("post-tool:{id}"), session);
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: Some(id),
                    tool_use_id: "reused-provider-call".to_string(),
                    content: "written".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            });
        }
        if complete {
            self.snapshot(
                &codewhale_engine::core::turn::format_snapshot_label(
                    "post-turn",
                    seq,
                    Some(prompt),
                ),
                session,
            );
        }
        messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "done".to_string(),
                cache_control: None,
            }],
        });
        messages
    }
}

fn fixture_call(execution: &str, path: &str, body: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: Some(execution.to_string()),
            id: "reused-provider-call".to_string(),
            name: "write_file".to_string(),
            input: serde_json::json!({"path":path,"content":body}),
            caller: None,
            thought_signature: None,
        }],
    }
}

fn push_recorded_turn(app: &mut App, messages: Vec<Message>) {
    let ContentBlock::Text { text, .. } = &messages[0].content[0] else {
        panic!("missing fixture prompt")
    };
    app.history.push(HistoryCell::User {
        content: text.clone(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "done".to_string(),
        streaming: false,
    });
    app.api_messages_mut().extend(messages);
}

fn push_exchange(app: &mut App, prompt: &str) {
    push_recorded_turn(
        app,
        vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: prompt.to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "done".to_string(),
                    cache_control: None,
                }],
            },
        ],
    );
}

/// `/diff` in a folder that is not a git repository compares against the
/// session's first restore point, so real edits are shown, including a file
/// created after the newest snapshot. It said "No changes since session
/// start" there, because `git diff` prints nothing outside a repository.
#[test]
fn diff_in_a_folder_without_git_shows_the_changes_since_the_sessions_first_restore_point() {
    let fx = UndoFixture::new();
    fx.write("duration.mjs", "export const unit = 'ms';\n");
    // Another session's older restore point is not this session's start.
    fx.snapshot("pre-turn:1", "other");
    fx.write("duration.mjs", "export const unit = 's';\n");
    fx.snapshot("pre-turn:1", "s1");
    fx.write("duration.mjs", "export const unit = 'min';\n");
    fx.snapshot("post-turn:1", "s1");
    fx.write("notes.txt", "written after the turn\n");
    let snapshots = fx.repo.list(usize::MAX).unwrap().len();

    let mut app = fx.app("s1");
    let result = super::execute("/diff", &mut app);

    assert!(!result.is_error, "{:?}", result.message);
    let message = result.message.clone().unwrap_or_default();
    assert!(
        message.starts_with("Changed files (2):\nduration.mjs\nnotes.txt"),
        "{message}"
    );
    let Some(AppAction::OpenDiffPager { title, diff }) = result.action else {
        panic!("expected the diff pager: {:?}", result.action);
    };
    assert_eq!(title, "Changes since session start");
    assert!(
        diff.contains("-export const unit = 's';\n+export const unit = 'min';\n"),
        "{diff}"
    );
    assert!(diff.contains("+written after the turn"), "{diff}");
    assert!(!diff.contains("'ms'"), "another session's edit: {diff}");
    // Reading the diff takes no snapshot and changes no file.
    assert_eq!(fx.repo.list(usize::MAX).unwrap().len(), snapshots);
    assert_eq!(fx.read("notes.txt"), "written after the turn\n");

    // A session with no restore point here has nothing to compare against,
    // and says so instead of reporting no changes.
    let mut fresh = fx.app("s2");
    let result = super::execute("/diff", &mut fresh);
    assert_eq!(
        result.message.as_deref(),
        Some(
            "Nothing to compare yet: this session has not saved a restore point here, and this folder is not a git repository."
        )
    );
    assert!(result.action.is_none());
}

#[test]
fn patch_undo_restores_only_the_paths_the_undone_step_changed() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.write("b.txt", "b0");
    let messages = fx.record_turn(
        "s1",
        1,
        "edit a.txt and create new.txt",
        &[("a.txt", "a1"), ("new.txt", "created by the turn")],
        true,
    );
    fx.write("b.txt", "b-user");
    fx.write("mine.txt", "user file");
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    assert!(!fx.workspace.join("new.txt").exists());
    assert_eq!(fx.read("b.txt"), "b-user");
    assert_eq!(fx.read("mine.txt"), "user file");
    let message = result.message.unwrap_or_default();
    assert!(
        message.contains("a.txt") && message.contains("new.txt"),
        "{message}"
    );
}

#[test]
fn patch_undo_refuses_when_a_changed_path_changed_since() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.write("b.txt", "b0");
    let messages = fx.record_turn(
        "s1",
        1,
        "edit both files",
        &[("a.txt", "a1"), ("b.txt", "b1")],
        true,
    );
    fx.write("a.txt", "a-user");
    let before = fx.repo.list(usize::MAX).unwrap().len();
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages.clone());
    let result = patch_undo(&mut app);
    assert!(result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a-user");
    assert_eq!(fx.read("b.txt"), "b1");
    assert_eq!(app.api_messages.as_ref(), &messages);
    assert!(result.action.is_none());
    assert_eq!(fx.repo.list(usize::MAX).unwrap().len(), before);
}

#[test]
fn undo_takes_back_a_whole_request_in_one_step_without_trust() {
    let fx = UndoFixture::new();
    fx.write("duration.mjs", "v0");
    fx.write("duration.test.mjs", "t0");
    let messages = fx.record_turn(
        "s1",
        1,
        "Whole hours should read 2h",
        &[
            ("duration.mjs", "v1"),
            ("duration.test.mjs", "t1"),
            ("notes.md", "created by the request"),
        ],
        true,
    );
    let before = fx.repo.list(usize::MAX).unwrap().len();
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("duration.mjs"), "v0");
    assert_eq!(fx.read("duration.test.mjs"), "t0");
    assert!(!fx.workspace.join("notes.md").exists());
    assert!(!app.history.is_empty());
    acknowledge_file_undo(&mut app, &result);
    assert!(app.history.is_empty() && app.api_messages.is_empty());
    assert!(!app.trust_mode && !app.yolo);
    assert!(fx.repo.list(usize::MAX).unwrap().len() > before);
    assert_eq!(
        patch_undo(&mut app).message.as_deref(),
        Some("Nothing to undo")
    );
    assert_eq!(fx.read("duration.mjs"), "v0");
}

#[test]
fn undo_cuts_the_conversation_at_the_prompt_not_at_a_tool_result() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let messages = fx.record_turn("s1", 2, "edit a.txt", &[("a.txt", "a1")], true);
    let mut app = fx.app("s1");
    push_exchange(&mut app, "keep this");
    push_recorded_turn(&mut app, messages);
    let original = app.api_messages.as_ref().clone();
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    assert_eq!(app.api_messages.as_ref(), &original);
    assert!(
        matches!(result.action, Some(AppAction::ConversationUndo { ref sync, .. }) if sync.messages == original[..2])
    );
}

#[test]
fn undo_never_reverts_files_of_a_request_the_conversation_still_holds() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let first = fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], true);
    let kept = first.len();
    let second = fx.record_turn("s1", 2, "what did you change?", &[], true);
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, first);
    push_recorded_turn(&mut app, second);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a1");
    acknowledge_file_undo(&mut app, &result);
    assert_eq!(app.api_messages.len(), kept);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    acknowledge_file_undo(&mut app, &result);
    assert!(app.api_messages.is_empty());
}

fn assert_undo_identity_refused(fx: &UndoFixture, app: &mut App) {
    let messages = app.api_messages.as_ref().clone();
    let history = app.history.len();
    let contents = fx.read("a.txt");
    let snapshots: Vec<_> = fx
        .repo
        .list(usize::MAX)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let result = patch_undo(app);
    assert!(result.is_error, "{:?}", result.message);
    assert!(result.action.is_none());
    assert_eq!(app.api_messages.as_ref(), &messages);
    assert_eq!(app.history.len(), history);
    assert_eq!(fx.read("a.txt"), contents);
    assert_eq!(
        fx.repo
            .list(usize::MAX)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        snapshots
    );
}

#[test]
fn undo_refuses_repeated_requests_with_missing_current_execution_receipts() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let mut current = fx.record_turn("s1", 1, "continue", &[("a.txt", "a2")], true);
    fx.write("a.txt", "a-user");
    fx.write("a.txt", "a2");
    for message in &mut current {
        for block in &mut message.content {
            match block {
                ContentBlock::ToolUse { execution_id, .. }
                | ContentBlock::ToolResult { execution_id, .. } => {
                    *execution_id = Some("fresh-execution-with-no-restore-point".into())
                }
                _ => {}
            }
        }
    }
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, current);
    assert_undo_identity_refused(&fx, &mut app);
}

#[test]
fn undo_matches_execution_identity_with_lossy_shared_prefix_and_first_line_labels() {
    let prefix = "x".repeat(100);
    for (first, second) in [
        (format!("{prefix} first"), format!("{prefix} second")),
        (
            "edit a.txt\nfirst change".into(),
            "edit a.txt\nsecond change".into(),
        ),
    ] {
        let fx = UndoFixture::new();
        fx.write("a.txt", "a0");
        let first = fx.record_turn("s1", 1, &first, &[("a.txt", "a1")], true);
        let second = fx.record_turn("s1", 2, &second, &[("a.txt", "a2")], true);
        let mut app = fx.app("s1");
        push_recorded_turn(&mut app, first);
        push_recorded_turn(&mut app, second);
        let result = patch_undo(&mut app);
        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(fx.read("a.txt"), "a1");
        acknowledge_file_undo(&mut app, &result);
        let result = patch_undo(&mut app);
        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(fx.read("a.txt"), "a0");
    }
}

#[test]
fn undo_matches_unique_truncated_and_multiline_prompt_snippets() {
    for prompt in [
        "x".repeat(101),
        "edit a.txt\nwith a second line".into(),
        "edit a.txt \nwith trailing whitespace".into(),
        "\nwith an empty first line".into(),
    ] {
        let fx = UndoFixture::new();
        fx.write("a.txt", "a0");
        let first = fx.record_turn("s1", 1, &prompt, &[("a.txt", "a1")], true);
        let first_count = first.len();
        let second = fx.record_turn("s1", 2, "update a.txt again", &[("a.txt", "a2")], true);
        let total = first_count + second.len();
        let mut app = fx.app("s1");
        push_recorded_turn(&mut app, first);
        push_recorded_turn(&mut app, second);
        app.session_turn_outcomes = vec![
            codewhale_engine::session_manager::SavedTurnOutcome::failed(
                "first failure",
                first_count,
            ),
            codewhale_engine::session_manager::SavedTurnOutcome::failed("second failure", total),
        ];
        let result = patch_undo(&mut app);
        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(fx.read("a.txt"), "a1");
        acknowledge_file_undo(&mut app, &result);
        assert_eq!(app.history.len(), 2);
        assert_eq!(app.api_messages.len(), first_count);
        assert_eq!(app.session_turn_outcomes.len(), 1);
        assert_eq!(app.session_turn_outcomes[0].error, "first failure");
        if prompt.starts_with('\n') {
            assert_undo_identity_refused(&fx, &mut app);
            assert_eq!(fx.read("a.txt"), "a1");
            assert_eq!(app.session_turn_outcomes.len(), 1);
            assert_eq!(app.session_turn_outcomes[0].error, "first failure");
            continue;
        }
        let result = patch_undo(&mut app);
        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(fx.read("a.txt"), "a0");
        acknowledge_file_undo(&mut app, &result);
        assert!(app.history.is_empty() && app.api_messages.is_empty());
        assert!(app.session_turn_outcomes.is_empty());
    }
}

#[test]
fn undo_refuses_missing_or_legacy_execution_identity_for_multipart_requests() {
    for kind in 0..4 {
        let fx = UndoFixture::new();
        fx.write("a.txt", "a0");
        let mut messages = fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], true);
        for message in &mut messages {
            for block in &mut message.content {
                if let ContentBlock::ToolUse { execution_id, .. } = block {
                    *execution_id = if kind == 0 { None } else { Some(String::new()) };
                }
            }
        }
        if kind > 0 {
            messages[0].content.push(if kind == 3 {
                ContentBlock::Text {
                    text: "second instruction".into(),
                    cache_control: None,
                }
            } else {
                ContentBlock::ImageUrl {
                    image_url: codewhale_models::ImageUrlContent {
                        url: "data:image/png;base64,AAAA".into(),
                    },
                }
            });
        }
        let mut app = fx.app("s1");
        push_recorded_turn(&mut app, messages);
        assert_undo_identity_refused(&fx, &mut app);
    }
}

#[test]
fn undo_identity_guard_includes_history_only_requests() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.record_turn("s1", 1, "continue", &[("a.txt", "a1")], true);
    let mut app = fx.app("s1");
    for _ in 0..2 {
        app.history.push(HistoryCell::User {
            content: "continue".into(),
        });
    }
    assert_undo_identity_refused(&fx, &mut app);
}

#[test]
fn undo_keeps_lossless_hundred_character_unicode_prompts_available() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let messages = fx.record_turn("s1", 1, &"漢".repeat(100), &[("a.txt", "a1")], true);
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    acknowledge_file_undo(&mut app, &result);
    assert!(app.history.is_empty() && app.api_messages.is_empty());
}

#[test]
fn undo_takes_a_newer_request_off_before_refusing_an_older_one() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let first = fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], true);
    let kept = first.len();
    let second = fx.record_turn("s1", 2, "why?", &[], true);
    fx.write("a.txt", "a-user");
    let before = fx.repo.list(usize::MAX).unwrap().len();
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, first);
    push_recorded_turn(&mut app, second);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    acknowledge_file_undo(&mut app, &result);
    assert_eq!(app.api_messages.len(), kept);
    let result = patch_undo(&mut app);
    assert!(result.is_error, "{:?}", result.message);
    assert!(result.action.is_none());
    assert_eq!(fx.read("a.txt"), "a-user");
    assert_eq!(app.api_messages.len(), kept);
    assert_eq!(app.history.len(), 2);
    assert_eq!(fx.repo.list(usize::MAX).unwrap().len(), before);
}

#[test]
fn undo_refuses_an_unmatched_restore_point_after_a_partial_conversation_rewind() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let first = fx.record_turn("s1", 1, "say hello", &[], true);
    fx.record_turn("s1", 2, "edit a.txt", &[("a.txt", "a1")], true);
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, first);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a1");
    acknowledge_file_undo(&mut app, &result);
    assert!(app.api_messages.is_empty());
}

#[test]
fn undo_after_a_full_conversation_rewind_keeps_unbound_files() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], true);
    let mut app = fx.app("s1");
    let before = fx.repo.list(usize::MAX).unwrap().len();
    let result = patch_undo(&mut app);
    assert_eq!(result.message.as_deref(), Some("Nothing to undo"));
    assert_eq!(fx.read("a.txt"), "a1");
    assert!(app.api_messages.is_empty() && app.history.is_empty());
    assert_eq!(fx.repo.list(usize::MAX).unwrap().len(), before);
}

#[test]
fn undo_of_a_request_without_a_recorded_end_is_refused_even_when_forced() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let messages = fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], false);
    let before = fx.repo.list(usize::MAX).unwrap().len();
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages);
    for (trust, yolo) in [(false, false), (true, false), (false, true)] {
        app.trust_mode = trust;
        app.yolo = yolo;
        for command in ["/undo", "/undo force"] {
            let staged = super::execute(command, &mut app);
            assert!(matches!(
                staged.action,
                Some(AppAction::UndoLatestRequest { .. })
            ));
            assert_undo_identity_refused(&fx, &mut app);
        }
    }
    assert_eq!(fx.read("a.txt"), "a1");
    assert_eq!(fx.repo.list(usize::MAX).unwrap().len(), before);
}

#[test]
fn undo_keeps_an_edit_the_user_made_after_an_unfinished_request() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.write("mine.txt", "m0");
    let first = fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], false);
    fx.write("mine.txt", "m-user");
    let second = fx.record_turn("s1", 2, "edit c.txt", &[("c.txt", "c1")], true);
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, first);
    push_recorded_turn(&mut app, second);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert!(!fx.workspace.join("c.txt").exists());
    acknowledge_file_undo(&mut app, &result);
    assert_undo_identity_refused(&fx, &mut app);
    assert_eq!(fx.read("mine.txt"), "m-user");
    assert_eq!(fx.read("a.txt"), "a1");
}

#[test]
fn patch_undo_finds_restore_points_beyond_the_newest_hundred_snapshots() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let messages = fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], true);
    for i in 0..101 {
        fx.snapshot(&format!("tool:other-{i}"), "other-session");
    }
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages);
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
}

#[test]
fn patch_undo_restores_turns_a_fork_inherited() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let messages = fx.record_turn("source", 1, "edit a.txt", &[("a.txt", "a1")], true);
    let fork = |created_at: chrono::DateTime<chrono::Utc>| {
        let mut app = fx.app("fork");
        let mut metadata = codewhale_engine::session_manager::create_saved_session(
            &[],
            "model",
            &fx.workspace,
            0,
            None,
        )
        .metadata;
        metadata.id = "fork".into();
        metadata.parent_session_id = Some("source".into());
        metadata.created_at = created_at;
        app.current_session_metadata = Some(metadata);
        push_recorded_turn(&mut app, messages.clone());
        app
    };
    let mut early = fork(chrono::Utc::now() - chrono::Duration::hours(1));
    let refused = patch_undo(&mut early);
    assert!(refused.is_error, "{:?}", refused.message);
    assert_eq!(fx.read("a.txt"), "a1");
    let mut app = fork(chrono::Utc::now() + chrono::Duration::seconds(5));
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
}

#[test]
fn patch_undo_waits_for_a_pending_post_turn_snapshot() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let messages = fx.record_turn("s1", 1, "edit a.txt", &[("a.txt", "a1")], false);
    let pending = codewhale_snapshot::PendingPostTurnSnapshot::reserve();
    let repo = codewhale_snapshot::SnapshotRepo::open_or_init(&fx.workspace).unwrap();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let taken = repo
            .take_snapshot("post-turn:1: edit a.txt", Some("s1"))
            .unwrap();
        drop(pending);
        taken
    });
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, messages);
    let result = patch_undo(&mut app);
    let post_turn = writer.join().unwrap();
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    let closed = fx
        .repo
        .list(usize::MAX)
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.label.starts_with("post-tool:"))
        .unwrap();
    assert!(
        fx.repo
            .changed_paths_between(&closed.tree, &post_turn.tree)
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn patch_undo_refuses_non_regular_paths_without_mutating_older_steps() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    let first = fx.record_turn("s1", 1, "first edit", &[("a.txt", "a1")], true);
    fx.snapshot("pre-turn:2: second edit", "s1");
    let id = uuid::Uuid::new_v4().to_string();
    fx.snapshot(&format!("tool:{id}"), "s1");
    fx.write("a.txt", "a2");
    std::os::unix::fs::symlink("a.txt", fx.workspace.join("current")).unwrap();
    fx.snapshot(&format!("post-tool:{id}"), "s1");
    fx.snapshot("post-turn:2: second edit", "s1");
    let mut second = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "second edit".into(),
                cache_control: None,
            }],
        },
        fixture_call(&id, "a.txt", "a2"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "done".into(),
                cache_control: None,
            }],
        },
    ];
    if let ContentBlock::ToolUse { name, input, .. } = &mut second[1].content[0] {
        *name = "bash".into();
        *input = serde_json::json!({"command":"fixture writes a.txt and symlink"});
    }
    let mut app = fx.app("s1");
    push_recorded_turn(&mut app, first);
    push_recorded_turn(&mut app, second);
    let before = fx.repo.list(usize::MAX).unwrap().len();
    let result = patch_undo(&mut app);
    assert!(result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a2");
    assert!(
        std::fs::symlink_metadata(fx.workspace.join("current"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fx.repo.list(usize::MAX).unwrap().len(), before);
}

#[test]
fn receipts_command_is_registered_and_reads_the_transcript() {
    assert_eq!(
        crate::commands::get_command_info("receipts").map(|info| info.name),
        Some("receipts")
    );
    assert_eq!(
        crate::commands::get_command_info("receipt").map(|info| info.name),
        Some("receipts")
    );
    let mut app = create_test_app();
    app.current_session_id = None;
    let empty = crate::commands::execute("/receipts", &mut app);
    assert!(!empty.is_error, "{:?}", empty.message);
    assert!(
        empty
            .message
            .as_deref()
            .is_some_and(|text| text.contains("No actions recorded.")),
        "{:?}",
        empty.message
    );

    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "run the tests".to_string(),
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "cargo test"}),
            caller: None,
            thought_signature: None,
        }],
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: "call-1".to_string(),
            content: "ok".to_string(),
            is_error: None,
            content_blocks: None,
        }],
    });
    let listed = crate::commands::execute("/receipts", &mut app);
    let text = listed.message.expect("receipt text");
    assert!(text.contains("Ran 1 command"), "{text}");
    assert!(text.contains("1. ran `cargo test`"), "{text}");

    // `$`, `*`, and `_` in a command must reach the note cell as JSON, not
    // as math or emphasis.
    let shell = r#"echo "$HOME" && echo $PATH *_x_*"#;
    app.api_messages_mut().push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-2".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({ "command": shell }),
            caller: None,
            thought_signature: None,
        }],
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: "call-2".to_string(),
            content: "ok".to_string(),
            is_error: None,
            content_blocks: None,
        }],
    });
    let json = crate::commands::execute("/receipts json", &mut app);
    let fenced = json.message.expect("json");
    let body = fenced
        .strip_prefix("```json\n")
        .and_then(|rest| rest.strip_suffix("\n```"))
        .expect("the JSON is fenced");
    let value: serde_json::Value = serde_json::from_str(body).expect("valid json");
    assert_eq!(value["totals"]["commands"], 2);
    let rendered: String = HistoryCell::System { content: fenced }
        .lines(400)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains(r#""command": "echo \"$HOME\" && echo $PATH *_x_*""#),
        "{rendered}"
    );
    let bad = crate::commands::execute("/receipts nope", &mut app);
    assert!(bad.is_error);
}

#[test]
fn whole_debug_registry_matches_portable_inventory_and_exact_host_authority() {
    use codewhale_command_contract::handler::{
        CommandCapabilities as Caps, CommandHandler, ContextParts,
    };
    let mut app = create_test_app();
    let portable = codewhale_commands::debug::portable_handlers();
    assert_eq!(portable.len(), 14);
    for (info, portable_handler) in portable {
        for spelling in std::iter::once(info.name).chain(info.aliases.iter().copied()) {
            let registered = super::registry()
                .get(spelling)
                .expect("registered debug command");
            assert_eq!(registered.info().name, info.name);
            assert_eq!(registered.info().aliases, info.aliases);
            assert_eq!(registered.info().usage, info.usage);
            let handler = registered
                .contextual_handler()
                .expect("portable host registration");
            match (portable_handler.clone(), handler) {
                (CommandHandler::Pure(_), CommandHandler::Pure(_)) => {
                    assert_eq!(info.name, "preview-request")
                }
                (
                    CommandHandler::Contextual {
                        capabilities: expected,
                        ..
                    },
                    CommandHandler::Contextual { capabilities, .. },
                ) => {
                    assert_eq!(capabilities, expected, "/{spelling}");
                    let mut bundle = app.command_contexts();
                    let ContextParts {
                        session,
                        model,
                        cost,
                        mode_policy,
                        system_prompt,
                        skills,
                        workspace,
                        presentation,
                        media,
                        memory,
                        project,
                        skill_group,
                        plugin,
                        lifecycle,
                        control,
                        export,
                        structcopy,
                        debug_diagnostics,
                        debug_receipts,
                        debug_change,
                        debug_history,
                        debug_diff,
                        debug_undo,
                        permissions,
                        config_status,
                        config,
                        pet,
                        trust,
                        login,
                        work_surface,
                        navigation,
                        voice_input,
                    } = bundle.contexts(capabilities).into_parts();
                    assert!(
                        permissions.is_none()
                            && config_status.is_none()
                            && config.is_none()
                            && pet.is_none()
                            && trust.is_none()
                            && login.is_none()
                            && work_surface.is_none()
                            && navigation.is_none()
                            && voice_input.is_none()
                    );
                    for (name, present, capability) in [
                        ("session", session.is_some(), Caps::SESSION),
                        ("model", model.is_some(), Caps::MODEL),
                        ("cost", cost.is_some(), Caps::COST),
                        ("mode_policy", mode_policy.is_some(), Caps::MODE_POLICY),
                        (
                            "system_prompt",
                            system_prompt.is_some(),
                            Caps::SYSTEM_PROMPT,
                        ),
                        ("skills", skills.is_some(), Caps::SKILLS),
                        ("workspace", workspace.is_some(), Caps::WORKSPACE),
                        ("presentation", presentation.is_some(), Caps::PRESENTATION),
                        ("media", media.is_some(), Caps::MEDIA),
                        ("memory", memory.is_some(), Caps::MEMORY),
                        ("project", project.is_some(), Caps::PROJECT),
                        ("skill_group", skill_group.is_some(), Caps::SKILL_GROUP),
                        ("plugin", plugin.is_some(), Caps::PLUGIN),
                        ("lifecycle", lifecycle.is_some(), Caps::SESSION_LIFECYCLE),
                        ("control", control.is_some(), Caps::SESSION_CONTROL),
                        ("export", export.is_some(), Caps::SESSION_EXPORT),
                        ("structcopy", structcopy.is_some(), Caps::SESSION_STRUCTCOPY),
                        (
                            "debug_diagnostics",
                            debug_diagnostics.is_some(),
                            Caps::DEBUG_DIAGNOSTICS,
                        ),
                        (
                            "debug_receipts",
                            debug_receipts.is_some(),
                            Caps::DEBUG_RECEIPTS,
                        ),
                        ("debug_change", debug_change.is_some(), Caps::DEBUG_CHANGE),
                        (
                            "debug_history",
                            debug_history.is_some(),
                            Caps::DEBUG_HISTORY,
                        ),
                        ("debug_diff", debug_diff.is_some(), Caps::DEBUG_DIFF),
                        ("debug_undo", debug_undo.is_some(), Caps::DEBUG_UNDO),
                    ] {
                        assert_eq!(
                            present,
                            capabilities.contains(capability),
                            "/{spelling}: {name}"
                        );
                    }
                }
                _ => panic!("host changed /{spelling} handler shape"),
            }
        }
    }
}
