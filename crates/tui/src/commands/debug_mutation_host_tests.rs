//! Preserved snapshot/history safety tests outside the portable debug group.
use super::CommandResult;
use codewhale_commands::debug::undo;
fn patch_undo(app: &mut App) -> CommandResult {
    super::debug_group::host_result(undo::patch_result(
        super::contract::debug_operations::undo_files(app, false),
    ))
}
fn undo_conversation(app: &mut App) -> CommandResult {
    super::debug_group::host_result(undo::conversation_result(
        super::contract::debug_operations::undo_conversation_for_engine(app),
    ))
}
fn retry(app: &mut App) -> CommandResult {
    super::execute("/retry", app)
}
use crate::config::Config;
use crate::tui::app::{App, AppAction, TuiOptions};
use crate::tui::history::HistoryCell;
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message, Tool};
use std::path::PathBuf;

pub(in crate::commands) fn create_test_app() -> App {
    let options = TuiOptions {
        skills_dir: PathBuf::from("/tmp/test-skills"),
        ..crate::tui::test_support::test_tui_options(PathBuf::from("/tmp/test-workspace"))
    };
    let mut app = App::new(options, &Config::default());
    app.ui_locale = codewhale_localization::Locale::En;
    app.cost_currency = crate::pricing::CostCurrency::Usd;
    app.api_provider = crate::config::ProviderKind::Deepseek;
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
        content: vec![],
    });
    app.api_messages_mut().push(Message {
        role: Role::Assistant,
        content: vec![],
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
fn test_patch_undo_requests_session_resync_after_restore() {
    use crate::snapshot::SnapshotRepo;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let _guard = crate::test_support::SealedHome::at(tmp.path());

    let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
    std::fs::write(workspace.join("a.txt"), b"original").unwrap();
    repo.snapshot_with_session("pre-turn:1: please edit a.txt", Some("test-session"))
        .unwrap();
    std::fs::write(workspace.join("a.txt"), b"modified").unwrap();
    repo.snapshot_with_session("post-turn:1: please edit a.txt", Some("test-session"))
        .unwrap();

    let mut app = create_test_app();
    app.workspace = workspace.clone();
    app.current_session_id = Some("test-session".to_string());
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "please edit a.txt".to_string(),
            cache_control: None,
        }],
    });

    // A running turn owns the workspace: nothing is restored under it.
    app.is_loading = true;
    let refused = patch_undo(&mut app);
    assert!(
        refused
            .message
            .as_deref()
            .is_some_and(|message| message.contains("still running")),
        "{:?}",
        refused.message
    );
    assert!(refused.action.is_none());
    assert_eq!(
        std::fs::read(workspace.join("a.txt")).unwrap(),
        b"modified",
        "a refused undo changes no file"
    );
    app.is_loading = false;

    let result = patch_undo(&mut app);
    assert_eq!(std::fs::read(workspace.join("a.txt")).unwrap(), b"original");

    assert!(!result.is_error);
    assert!(matches!(
        result.action,
        Some(AppAction::SyncSession {
            ref messages,
            ref workspace,
            ..
        }) if messages.as_slice() == app.api_messages.as_slice()
            && workspace == &app.workspace
    ));
}

#[test]
fn test_undo_legacy_chain_falls_back_to_conversation_only() {
    use crate::snapshot::SnapshotRepo;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let _guard = crate::test_support::SealedHome::at(tmp.path());

    let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
    let file = workspace.join("a.txt");
    std::fs::write(&file, b"zero").unwrap();
    repo.snapshot("tool:first").unwrap();
    std::fs::write(&file, b"one").unwrap();
    repo.snapshot("tool:second").unwrap();
    std::fs::write(&file, b"two").unwrap();

    let mut app = create_test_app();
    app.workspace = workspace.clone();
    app.current_session_id = Some("current-session".to_string());
    app.history.push(HistoryCell::User {
        content: "chat only".to_string(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "reply".to_string(),
        streaming: false,
    });

    let result = super::execute("/undo", &mut app);
    assert!(!result.is_error);
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|m| m.contains("Removed")),
        "expected conversation fallback, got: {:?}",
        result.message
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
}

#[test]
fn test_patch_undo_prunes_pre_turn_context() {
    use crate::snapshot::SnapshotRepo;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let _guard = crate::test_support::SealedHome::at(tmp.path());

    let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
    let file = workspace.join("a.txt");
    std::fs::write(&file, b"alpha").unwrap();
    repo.snapshot_with_session("pre-turn:1: please edit a.txt", Some("test-session"))
        .unwrap();
    std::fs::write(&file, b"alpha-fixed").unwrap();
    repo.snapshot_with_session("post-turn:1: please edit a.txt", Some("test-session"))
        .unwrap();

    let mut app = create_test_app();
    app.workspace = workspace.clone();
    app.current_session_id = Some("test-session".to_string());
    app.history.push(HistoryCell::User {
        content: "please edit a.txt".to_string(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "Done, file is fixed now.".to_string(),
        streaming: false,
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "please edit a.txt".to_string(),
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Done, file is fixed now.".to_string(),
            cache_control: None,
        }],
    });

    let result = patch_undo(&mut app);

    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha");
    // The request and its reply leave with the files; the command's own
    // message is the only receipt (no second transcript line).
    assert!(app.history.is_empty());
    assert!(app.api_messages.is_empty());
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|m| m.contains("removed from the conversation")),
        "{:?}",
        result.message
    );
}

// ── /cache stats tests ──────────────────────────────────────────────

#[test]
fn test_patch_undo_never_crosses_session_boundary() {
    use crate::snapshot::SnapshotRepo;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let _guard = crate::test_support::SealedHome::at(tmp.path());

    let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
    let file = workspace.join("a.txt");

    // Session A: an earlier conversation that modified the workspace.
    std::fs::write(&file, b"a-before").unwrap();
    repo.snapshot_with_session("pre-turn:1", Some("session-a"))
        .unwrap();
    std::fs::write(&file, b"a-after").unwrap();

    // Session B (current): a later conversation that also modified it.
    std::fs::write(&file, b"b-before").unwrap();
    repo.snapshot_with_session("pre-turn:1", Some("session-b"))
        .unwrap();
    std::fs::write(&file, b"b-after").unwrap();
    repo.snapshot_with_session("post-turn:1", Some("session-b"))
        .unwrap();

    let mut app = create_test_app();
    app.workspace = workspace.clone();
    app.current_session_id = Some("session-b".to_string());

    let result = patch_undo(&mut app);
    assert!(!result.is_error);
    // Must restore session B's pre-turn state — never session A's.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "b-before");

    let repeated = patch_undo(&mut app);
    assert!(!repeated.is_error);
    assert!(
        repeated
            .message
            .as_deref()
            .is_some_and(|m| m.contains("No undoable snapshot")),
        "repeated undo must stop at the session boundary: {:?}",
        repeated.message
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "b-before");
}

/// `/undo` used to report only `Removed N message(s)` when the snapshot repo
/// could not be opened, implying a file rollback that never happened. The
/// conversation-only fallback must say so, and say why.
#[test]
fn test_undo_reports_that_files_were_not_reverted_when_the_repo_is_unavailable() {
    use crate::test_support::SealedHome;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let _home = SealedHome::at(tmp.path());

    // The home directory itself is refused by the snapshot safety gate, so
    // `patch_undo` cannot open a repo at all.
    let mut app = create_test_app();
    app.workspace = tmp.path().to_path_buf();
    app.current_session_id = Some("test-session".to_string());
    app.yolo = true;
    app.history.push(HistoryCell::User {
        content: "change something".to_string(),
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "change something".to_string(),
            cache_control: None,
        }],
    });

    let result = super::execute("/undo", &mut app);
    let message = result.message.as_deref().unwrap_or_default();
    assert!(
        message.contains("Removed 1 message(s)"),
        "conversation undo still runs: {message}"
    );
    assert!(
        message.contains(undo::FILES_NOT_REVERTED_NOTE),
        "the user must be told files were not reverted: {message}"
    );
    assert!(
        message.contains(undo::SNAPSHOT_REPO_UNAVAILABLE_PREFIX),
        "the reason must travel with the fallback: {message}"
    );
}

/// Isolated HOME + workspace for the `/undo` restore tests (#6644).
// Fields drop in order: the seal restores the environment and releases the
// lock before the directory it pointed at is deleted.
struct UndoFixture {
    workspace: PathBuf,
    repo: crate::snapshot::SnapshotRepo,
    _home: crate::test_support::SealedHome,
    _tmp: tempfile::TempDir,
}

impl UndoFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = crate::test_support::SealedHome::at(tmp.path());
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace).unwrap();
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

    /// An app in the default posture: no trust mode, no Full Access.
    fn app(&self, session: &str) -> App {
        let mut app = create_test_app();
        app.workspace = self.workspace.clone();
        app.current_session_id = Some(session.to_string());
        app
    }
}

/// One request and its reply, as the transcript and the session log hold them.
fn push_exchange(app: &mut App, prompt: &str) {
    app.history.push(HistoryCell::User {
        content: prompt.to_string(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "done".to_string(),
        streaming: false,
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: prompt.to_string(),
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "done".to_string(),
            cache_control: None,
        }],
    });
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

/// `/undo` restores only the paths the undone turn changed: an edit the user
/// made to another file after the turn survives. The whole-tree restore
/// reverted it too.
#[test]
fn patch_undo_restores_only_the_paths_the_undone_step_changed() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.write("b.txt", "b0");
    fx.snapshot("pre-turn:1", "s1");
    fx.write("a.txt", "a1");
    fx.write("new.txt", "created by the turn");
    fx.snapshot("post-turn:1", "s1");
    // The user's own edit after the turn, and a file they created.
    fx.write("b.txt", "b-user");
    fx.write("mine.txt", "user file");

    let mut app = fx.app("s1");
    let result = patch_undo(&mut app);

    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    assert!(!fx.workspace.join("new.txt").exists());
    assert_eq!(fx.read("b.txt"), "b-user");
    assert_eq!(fx.read("mine.txt"), "user file");
    let message = result.message.unwrap_or_default();
    assert!(
        message.contains("  restored a.txt") && message.contains("  removed new.txt"),
        "{message}"
    );
}

/// A path the undone step changed that changed again since is refused, not
/// overwritten, and nothing else is touched.
#[test]
fn patch_undo_refuses_when_a_changed_path_changed_since() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.write("b.txt", "b0");
    fx.snapshot("pre-turn:1", "s1");
    fx.write("a.txt", "a1");
    fx.write("b.txt", "b1");
    fx.snapshot("post-turn:1", "s1");
    fx.write("a.txt", "a-user");

    let mut app = fx.app("s1");
    let result = super::execute("/undo", &mut app);

    let message = result.message.unwrap_or_default();
    assert!(
        message.starts_with(
            "Nothing was undone. a.txt changed after the request, and undoing would overwrite that change."
        ),
        "{message}"
    );
    assert!(
        message.contains("/restore") && !message.contains("/trust"),
        "the refusal says what to do: {message}"
    );
    assert_eq!(fx.read("a.txt"), "a-user");
    assert_eq!(fx.read("b.txt"), "b1");
    assert_eq!(
        fx.repo.list(usize::MAX).unwrap().len(),
        2,
        "a refusal writes no snapshot"
    );
}

/// The newcomer's case (0.10.1 tutorial, lesson 3): the agent edited two
/// files and created a third for one request, in the default posture. One
/// `/undo` takes the whole request back without `/trust on`, and says which
/// files it restored without restore-point names. Before, it was refused
/// outside trusted mode, and with trust it reverted one edit per `/undo`.
#[test]
fn undo_takes_back_a_whole_request_in_one_step_without_trust() {
    let fx = UndoFixture::new();
    fx.write("duration.mjs", "v0");
    fx.write("duration.test.mjs", "t0");
    fx.snapshot("pre-turn:1: Whole hours should read 2h", "s1");
    fx.snapshot("tool:call-1", "s1");
    fx.write("duration.mjs", "v1");
    fx.snapshot("tool:call-2", "s1");
    fx.write("duration.test.mjs", "t1");
    fx.snapshot("tool:call-3", "s1");
    fx.write("notes.md", "created by the request");
    fx.snapshot("post-turn:1: Whole hours should read 2h", "s1");
    let before = fx.repo.list(usize::MAX).unwrap().len();

    let mut app = fx.app("s1");
    push_exchange(&mut app, "Whole hours should read 2h");

    let result = super::execute("/undo", &mut app);

    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("duration.mjs"), "v0");
    assert_eq!(fx.read("duration.test.mjs"), "t0");
    assert!(!fx.workspace.join("notes.md").exists());
    assert_eq!(
        result.message.as_deref(),
        Some(
            "Undid your request \"Whole hours should read 2h\". 3 files are back as they were before it:\n  restored duration.mjs\n  restored duration.test.mjs\n  removed notes.md\nThe request and its reply were removed from the conversation."
        )
    );
    assert!(app.history.is_empty() && app.api_messages.is_empty());
    assert!(
        !app.trust_mode && !app.yolo,
        "undo neither needs nor changes the access posture"
    );
    assert!(
        fx.repo.list(usize::MAX).unwrap().len() > before,
        "the safety snapshot is still taken"
    );

    // Nothing is left to undo, and nothing is refused.
    let again = super::execute("/undo", &mut app);
    assert_eq!(again.message.as_deref(), Some("Nothing to undo"));
    assert_eq!(fx.read("duration.mjs"), "v0");
}

/// A request's tool results are stored as `user`-role messages. The undone
/// request is cut at the user's prompt, never at one of those, so the
/// conversation left behind is one a provider accepts.
#[test]
fn undo_cuts_the_conversation_at_the_prompt_not_at_a_tool_result() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:2: edit a.txt", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:2: edit a.txt", "s1");

    let mut app = fx.app("s1");
    let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
        {"role":"user","content":[{"type":"text","text":"keep this"}]},
        {"role":"assistant","content":[{"type":"text","text":"kept answer"}]},
        {"role":"user","content":[{"type":"text","text":"edit a.txt"}]},
        {"role":"assistant","content":[{"type":"tool_use","id":"call-1","name":"write_file","input":{}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"written"}]},
        {"role":"assistant","content":[{"type":"text","text":"done"}]}
    ]))
    .unwrap();
    app.set_api_messages(std::sync::Arc::new(messages.clone()));

    let result = patch_undo(&mut app);

    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    assert_eq!(app.api_messages.as_slice(), &messages[..2]);
    assert!(matches!(
        result.action,
        Some(AppAction::SyncSession { ref messages, .. })
            if messages.as_slice() == app.api_messages.as_slice()
    ));
}

/// Conversation and files move together. With a newer request that changed
/// no files still in the conversation, `/undo` takes that request off first
/// and touches no file; the next `/undo` takes back the edit and its request.
#[test]
fn undo_never_reverts_files_of_a_request_the_conversation_still_holds() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1: edit a.txt", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:1: edit a.txt", "s1");
    fx.snapshot("pre-turn:2: what did you change?", "s1");
    fx.snapshot("post-turn:2: what did you change?", "s1");

    let mut app = fx.app("s1");
    push_exchange(&mut app, "edit a.txt");
    push_exchange(&mut app, "what did you change?");

    let first = super::execute("/undo", &mut app);
    assert_eq!(
        first.message.as_deref(),
        Some("Removed 2 message(s)\nNo file changes to undo."),
        "the question comes off first"
    );
    assert!(matches!(
        first.action,
        Some(AppAction::ConversationUndo { ref sync, .. }) if sync.messages.len() == 2
    ));
    assert_eq!(fx.read("a.txt"), "a1", "no file moved");
    // The UI applies the conversation-only undo once the Engine accepts it.
    app.truncate_history_to(2);
    app.truncate_api_messages(2);

    let second = super::execute("/undo", &mut app);
    assert_eq!(fx.read("a.txt"), "a0", "{:?}", second.message);
    assert!(app.api_messages.is_empty());
}

/// Identity refusals must reach the command as refusals, rather than its
/// conversation-only fallback, and must run before a forced safety snapshot.
fn assert_undo_identity_refused(fx: &UndoFixture, app: &mut App) {
    let messages = app.api_messages.as_ref().clone();
    let history_len = app.history.len();
    let snapshots: Vec<_> = fx
        .repo
        .list(usize::MAX)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let contents = fx.read("a.txt");
    for command in ["/undo", "/undo force"] {
        let result = super::execute(command, app);
        assert!(
            result.message.as_deref().is_some_and(|message| {
                message
                    .starts_with("Nothing was undone. This restore point cannot be matched safely")
                    && message.contains("/restore")
            }),
            "{command}: {:?}",
            result.message
        );
        assert!(result.action.is_none(), "no conversation-only fallback");
        assert_eq!(app.api_messages.as_ref(), &messages);
        assert_eq!(app.history.len(), history_len);
        assert_eq!(fx.read("a.txt"), contents);
        assert_eq!(
            fx.repo
                .list(usize::MAX)
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            snapshots,
            "identity refusal creates no backup"
        );
    }
}

#[test]
fn undo_refuses_repeated_requests_even_after_a_conversation_rewind() {
    for rewind in [false, true] {
        let fx = UndoFixture::new();
        fx.write("a.txt", "a0");
        fx.snapshot("pre-turn:1: continue", "s1");
        fx.write("a.txt", "a1");
        fx.snapshot("post-turn:1: continue", "s1");
        fx.snapshot("pre-turn:2: continue", "s1");
        fx.snapshot("post-turn:2: continue", "s1");
        let mut app = fx.app("s1");
        push_exchange(&mut app, "continue");
        if !rewind {
            push_exchange(&mut app, "continue");
        }
        // Previously the no-op second request was skipped, then the first
        // request's files were restored while the last held exchange vanished.
        assert_undo_identity_refused(&fx, &mut app);
    }
}

#[test]
fn undo_refuses_lossy_shared_prefix_and_first_line_labels() {
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
        fx.snapshot(
            &crate::core::turn::format_snapshot_label("pre-turn", 1, Some(&first)),
            "s1",
        );
        fx.write("a.txt", "a1");
        fx.snapshot(
            &crate::core::turn::format_snapshot_label("post-turn", 1, Some(&first)),
            "s1",
        );
        fx.snapshot(
            &crate::core::turn::format_snapshot_label("pre-turn", 2, Some(&second)),
            "s1",
        );
        fx.snapshot(
            &crate::core::turn::format_snapshot_label("post-turn", 2, Some(&second)),
            "s1",
        );
        let mut app = fx.app("s1");
        push_exchange(&mut app, &first);
        push_exchange(&mut app, &second);
        assert_undo_identity_refused(&fx, &mut app);
    }
}

#[test]
fn undo_refuses_a_unique_lossy_prompt_even_without_a_collision() {
    for prompt in ["x".repeat(101), "edit a.txt\nwith a second line".into()] {
        let fx = UndoFixture::new();
        fx.write("a.txt", "a0");
        fx.snapshot(
            &crate::core::turn::format_snapshot_label("pre-turn", 1, Some(&prompt)),
            "s1",
        );
        fx.write("a.txt", "a1");
        fx.snapshot(
            &crate::core::turn::format_snapshot_label("post-turn", 1, Some(&prompt)),
            "s1",
        );
        let mut app = fx.app("s1");
        push_exchange(&mut app, &prompt);
        assert_undo_identity_refused(&fx, &mut app);
    }
}

#[test]
fn undo_refuses_missing_prompt_labels_and_multipart_requests() {
    for kind in 0..4 {
        let fx = UndoFixture::new();
        fx.write("a.txt", "a0");
        fx.snapshot(
            if kind == 0 {
                "pre-turn:1"
            } else {
                "pre-turn:1: edit a.txt"
            },
            "s1",
        );
        fx.write("a.txt", "a1");
        // Leave the end open so /undo force would otherwise write a backup.
        let mut app = fx.app("s1");
        if kind == 0 {
            push_exchange(&mut app, "edit a.txt");
        } else {
            let image = ContentBlock::ImageUrl {
                image_url: codewhale_models::ImageUrlContent {
                    url: "data:image/png;base64,AAAA".into(),
                },
            };
            let text = ContentBlock::Text {
                text: "edit a.txt".into(),
                cache_control: None,
            };
            app.api_messages_mut().push(Message {
                role: Role::User,
                content: match kind {
                    1 => vec![image],
                    2 => vec![text, image],
                    _ => vec![
                        text,
                        ContentBlock::Text {
                            text: "second instruction".into(),
                            cache_control: None,
                        },
                    ],
                },
            });
        }
        assert_undo_identity_refused(&fx, &mut app);
    }
}

#[test]
fn undo_identity_guard_includes_history_only_requests() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1: continue", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:1: continue", "s1");
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
    let prompt = "漢".repeat(100);
    fx.write("a.txt", "a0");
    fx.snapshot(
        &crate::core::turn::format_snapshot_label("pre-turn", 1, Some(&prompt)),
        "s1",
    );
    fx.write("a.txt", "a1");
    fx.snapshot(
        &crate::core::turn::format_snapshot_label("post-turn", 1, Some(&prompt)),
        "s1",
    );
    let mut app = fx.app("s1");
    push_exchange(&mut app, &prompt);
    let result = super::execute("/undo", &mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0");
    assert!(app.history.is_empty() && app.api_messages.is_empty());
}

/// A file the older request changed was edited since. That refusal belongs
/// to the older request: the newer one, which changed no files, still comes
/// off first, and the refusal is shown once the older request is the last.
/// The refusal writes nothing and keeps the conversation.
#[test]
fn undo_takes_a_newer_request_off_before_refusing_an_older_one() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1: edit a.txt", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:1: edit a.txt", "s1");
    fx.snapshot("pre-turn:2: why?", "s1");
    fx.snapshot("post-turn:2: why?", "s1");
    fx.write("a.txt", "a-user");
    let before = fx.repo.list(usize::MAX).unwrap().len();

    let mut app = fx.app("s1");
    push_exchange(&mut app, "edit a.txt");
    push_exchange(&mut app, "why?");

    let first = super::execute("/undo", &mut app);
    assert!(
        first
            .message
            .as_deref()
            .is_some_and(|m| m.contains(undo::NO_FILE_CHANGES_NOTE)),
        "{:?}",
        first.message
    );
    assert!(matches!(
        first.action,
        Some(AppAction::ConversationUndo { .. })
    ));
    app.truncate_history_to(2);
    app.truncate_api_messages(2);

    let second = super::execute("/undo", &mut app);
    assert!(
        second
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("Nothing was undone. a.txt changed after the request")),
        "{:?}",
        second.message
    );
    assert!(second.action.is_none());
    assert_eq!(fx.read("a.txt"), "a-user");
    assert_eq!(
        app.api_messages.len(),
        2,
        "a refusal keeps the conversation"
    );
    assert_eq!(app.history.len(), 2);
    assert_eq!(
        fx.repo.list(usize::MAX).unwrap().len(),
        before,
        "a refusal writes no snapshot"
    );
}

/// A restore point absent from a held conversation has no proven request
/// identity, even when both display labels are distinct and lossless.
#[test]
fn undo_refuses_an_unmatched_restore_point_after_a_partial_conversation_rewind() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1: say hello", "s1");
    fx.snapshot("post-turn:1: say hello", "s1");
    fx.snapshot("pre-turn:2: edit a.txt", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:2: edit a.txt", "s1");
    let mut app = fx.app("s1");
    push_exchange(&mut app, "say hello");
    assert_undo_identity_refused(&fx, &mut app);
}

/// File-only undo still works after all requests have been rewound away.
#[test]
fn undo_after_a_full_conversation_rewind_restores_files_without_messages() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1: edit a.txt", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:1: edit a.txt", "s1");
    let mut app = fx.app("s1");
    let result = super::execute("/undo", &mut app);
    assert_eq!(fx.read("a.txt"), "a0", "{:?}", result.message);
    assert!(app.api_messages.is_empty() && app.history.is_empty());
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|message| message.contains("The conversation was not changed."))
    );
}

/// A request with no recorded end (its post-turn snapshot is missing) takes
/// in whatever was edited since. It is refused, with nothing written, in
/// every access mode: trust mode and Full Access are not the lever. The
/// refusal names `/undo force`, which runs it and says what it also put back.
#[test]
fn undo_of_a_request_without_a_recorded_end_is_refused_until_forced() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1: edit a.txt", "s1");
    fx.write("a.txt", "a1");
    let before = fx.repo.list(usize::MAX).unwrap().len();

    let mut app = fx.app("s1");
    push_exchange(&mut app, "edit a.txt");

    for (trust_mode, yolo) in [(false, false), (true, false), (false, true)] {
        app.trust_mode = trust_mode;
        app.yolo = yolo;
        let refused = super::execute("/undo", &mut app);
        assert_eq!(
            refused.message.as_deref(),
            Some(
                "Nothing was undone. There is no record of where your last request's changes end, so undoing it would also put back edits made since. To do that anyway, run `/undo force`."
            ),
            "trust_mode={trust_mode} yolo={yolo}"
        );
        assert!(refused.action.is_none());
        assert_eq!(fx.read("a.txt"), "a1");
        assert_eq!(
            app.api_messages.len(),
            2,
            "a refusal keeps the conversation"
        );
        assert_eq!(
            fx.repo.list(usize::MAX).unwrap().len(),
            before,
            "a refusal writes no snapshot"
        );
    }
    app.trust_mode = false;
    app.yolo = false;

    let forced = super::execute("/undo force", &mut app);

    assert!(!forced.is_error, "{:?}", forced.message);
    assert_eq!(fx.read("a.txt"), "a0");
    assert_eq!(
        forced.message.as_deref(),
        Some(
            "Undid your request \"edit a.txt\". 1 file is back as it was before it:\n  restored a.txt\nThe end of this request was not recorded, so edits made to these files since it started were also put back.\nThe request and its reply were removed from the conversation."
        )
    );
    assert!(app.api_messages.is_empty());
    assert!(
        fx.repo
            .list(usize::MAX)
            .unwrap()
            .iter()
            .any(|snapshot| snapshot.label.starts_with("pre-restore:")),
        "the forced undo keeps a backup of what it overwrote"
    );
}

/// The next request's start is not an unfinished request's end: what the
/// user edited between the two is theirs. Undoing the newer request works;
/// the older, open-ended one is then refused and the user's edit survives.
/// Before, `/undo` (in trusted mode) reverted that edit with the request.
#[test]
fn undo_keeps_an_edit_the_user_made_after_an_unfinished_request() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.write("mine.txt", "m0");
    fx.snapshot("pre-turn:1: edit a.txt", "s1");
    fx.write("a.txt", "a1");
    // No post-turn:1. The user edits their own file before the next request.
    fx.write("mine.txt", "m-user");
    fx.snapshot("pre-turn:2: edit c.txt", "s1");
    fx.write("c.txt", "c1");
    fx.snapshot("post-turn:2: edit c.txt", "s1");

    let mut app = fx.app("s1");
    push_exchange(&mut app, "edit a.txt");
    push_exchange(&mut app, "edit c.txt");

    let first = super::execute("/undo", &mut app);
    assert!(!first.is_error, "{:?}", first.message);
    assert!(!fx.workspace.join("c.txt").exists());
    assert_eq!(app.api_messages.len(), 2, "{:?}", first.message);

    let second = super::execute("/undo", &mut app);
    assert!(
        second
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("Nothing was undone.") && m.contains("/undo force")),
        "{:?}",
        second.message
    );
    assert!(second.action.is_none());
    assert_eq!(fx.read("mine.txt"), "m-user", "the user's edit survives");
    assert_eq!(fx.read("a.txt"), "a1");
    assert_eq!(app.api_messages.len(), 2);
}

/// Restore points older than the newest 100 snapshots are still found.
#[test]
fn patch_undo_finds_restore_points_beyond_the_newest_hundred_snapshots() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:1", "s1");
    for i in 0..101 {
        fx.repo
            .take_snapshot(&format!("tool:other-{i}"), Some("other-session"))
            .unwrap();
    }

    let mut app = fx.app("s1");
    let result = patch_undo(&mut app);

    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0", "{:?}", result.message);
}

/// A fork owns the restore points of the turns it inherited, up to the fork,
/// and none its source took afterwards.
#[test]
fn patch_undo_restores_turns_a_fork_inherited() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1", "source");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:1", "source");

    let fork = |created_at: chrono::DateTime<chrono::Utc>| {
        let mut app = fx.app("fork");
        let mut metadata =
            crate::session_manager::create_saved_session(&[], "model", &fx.workspace, 0, None)
                .metadata;
        metadata.id = "fork".to_string();
        metadata.parent_session_id = Some("source".to_string());
        metadata.created_at = created_at;
        app.current_session_metadata = Some(metadata);
        app
    };

    let owners = super::contract::debug_operations::snapshot_owners(&fork(chrono::Utc::now()));
    assert_eq!(owners.len(), 2);
    assert_eq!(owners[1].session_id, "source");

    // Forked before the source took these snapshots: they are not the fork's.
    let mut early = fork(chrono::Utc::now() - chrono::Duration::hours(1));
    let refused = patch_undo(&mut early);
    assert!(
        refused
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("No undoable snapshot")),
        "{:?}",
        refused.message
    );
    assert_eq!(fx.read("a.txt"), "a1");

    let mut app = fork(chrono::Utc::now() + chrono::Duration::seconds(5));
    let result = patch_undo(&mut app);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0", "{:?}", result.message);
}

/// `/undo` typed while the post-turn snapshot is still being written waits
/// for it (#6644). Before, the two raced on the side repo: the post-turn
/// snapshot landed after the undo and recorded the reverted workspace as
/// the turn's end, and the undone step ended at "now".
#[test]
fn patch_undo_waits_for_a_pending_post_turn_snapshot() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1", "s1");
    fx.snapshot("tool:call-1", "s1");
    fx.write("a.txt", "a1");

    // The turn has completed; its post-turn snapshot is still in flight.
    let pending = crate::snapshot::PendingPostTurnSnapshot::reserve();
    let writer = {
        let repo = crate::snapshot::SnapshotRepo::open_or_init(&fx.workspace).unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let taken = repo.take_snapshot("post-turn:1", Some("s1")).unwrap();
            drop(pending);
            taken
        })
    };

    let mut app = fx.app("s1");
    let result = patch_undo(&mut app);
    let post_turn = writer.join().unwrap();

    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(fx.read("a.txt"), "a0", "{:?}", result.message);
    let tool = fx
        .repo
        .list(usize::MAX)
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.label == "tool:call-1")
        .unwrap();
    assert_eq!(
        fx.repo
            .changed_paths_between(&tool.tree, &post_turn.tree)
            .unwrap(),
        vec![PathBuf::from("a.txt")],
        "the post-turn snapshot must record the turn's end, not the undone workspace"
    );
}

/// A step that changed a symlink restores its regular files and reports the
/// symlink, and it does not block older steps.
#[cfg(unix)]
#[test]
fn patch_undo_skips_non_regular_paths_without_blocking_older_steps() {
    let fx = UndoFixture::new();
    fx.write("a.txt", "a0");
    fx.snapshot("pre-turn:1", "s1");
    fx.write("a.txt", "a1");
    fx.snapshot("post-turn:1", "s1");
    fx.snapshot("pre-turn:2", "s1");
    fx.write("a.txt", "a2");
    std::os::unix::fs::symlink("a.txt", fx.workspace.join("current")).unwrap();
    fx.snapshot("post-turn:2", "s1");

    let mut app = fx.app("s1");
    let first = patch_undo(&mut app);
    assert!(!first.is_error, "{:?}", first.message);
    assert_eq!(fx.read("a.txt"), "a1");
    let message = first.message.unwrap_or_default();
    assert!(
        message.contains("Left in place") && message.contains("current"),
        "{message}"
    );
    assert!(
        std::fs::symlink_metadata(fx.workspace.join("current"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let second = patch_undo(&mut app);
    assert!(!second.is_error, "{:?}", second.message);
    assert_eq!(fx.read("a.txt"), "a0", "{:?}", second.message);
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
