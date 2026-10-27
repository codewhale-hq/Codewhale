//! Gherkin acceptance coverage for session command workflows.

use std::path::PathBuf;

use chrono::{Duration as ChronoDuration, Utc};
use cucumber::{World as _, given, then, when, writer::Stats as _};
use tempfile::TempDir;

use crate::commands::{self, CommandResult};
use crate::session_manager::{SavedSession, SessionManager, create_saved_session_with_id_and_mode};
use crate::test_support::{EnvVarGuard, lock_test_env};
use crate::tui::app::{App, AppAction, TuiOptions};
use crate::tui::history::HistoryCell;
use crate::tui::views::ModalKind;
use codewhale_models::{ContentBlock, Message, Role};
use codewhale_runtime::config::Config;

const FEATURE_NAME: &str = "Session command workflows";
const FEATURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/features/session_command_workflows.feature"
);
const SAVE_LOAD_SCENARIO: &str = "Save and export preserve data while load defers restoration";
const FORK_RESUMABLE_SCENARIO: &str = "Fork keeps the original session resumable";
const NEW_THEN_FORK_SCENARIO: &str = "New session cannot be forked before messages exist";
const CLEAR_THEN_FORK_SCENARIO: &str = "Cleared session cannot be forked before messages exist";
const FORK_THEN_NEW_SCENARIO: &str = "Fork followed by new keeps both saved sessions";
const FORK_THEN_CLEAR_SCENARIO: &str = "Fork followed by clear keeps both saved sessions";
const RENAME_SCENARIO: &str = "Rename updates the active saved session title";
const SESSIONS_LIST_SCENARIO: &str = "Sessions list opens the saved session picker";
const SESSIONS_PRUNE_SCENARIO: &str = "Sessions prune removes only stale sessions";
const CONTEXT_MANAGEMENT_SCENARIO: &str =
    "Context management commands emit actions without clearing the active session";
const SINGULAR_SESSION_SCENARIO: &str = "Singular session command is not registered";

#[derive(Default, cucumber::World)]
struct SessionCommandWorld {
    tmpdir: Option<TempDir>,
    app: Option<Box<App>>,
    save_path: Option<PathBuf>,
    export_path: Option<PathBuf>,
    home_path: Option<PathBuf>,
    original_session_id: Option<String>,
    fork_session_id: Option<String>,
    new_session_id: Option<String>,
    fresh_session_id: Option<String>,
    stale_session_id: Option<String>,
    last_message: Option<String>,
    last_result_is_error: Option<bool>,
    last_action: Option<AppAction>,
}

impl std::fmt::Debug for SessionCommandWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCommandWorld")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_app", &self.app.is_some())
            .field("save_path", &self.save_path)
            .field("export_path", &self.export_path)
            .field("home_path", &self.home_path)
            .field("original_session_id", &self.original_session_id)
            .field("fork_session_id", &self.fork_session_id)
            .field("new_session_id", &self.new_session_id)
            .field("fresh_session_id", &self.fresh_session_id)
            .field("stale_session_id", &self.stale_session_id)
            .field("last_message", &self.last_message)
            .field("last_result_is_error", &self.last_result_is_error)
            .finish()
    }
}

#[given("a CodeWhale session workspace with one user message")]
fn workspace_with_one_user_message(world: &mut SessionCommandWorld) {
    let tmpdir = TempDir::new().expect("session workflow TempDir");
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Remember the whale migration".to_string(),
            cache_control: None,
        }],
    });
    app.add_message(HistoryCell::User {
        content: "Remember the whale migration".to_string(),
    });
    app.session.total_tokens = 321;
    app.session.total_conversation_tokens = 321;

    world.save_path = Some(tmpdir.path().join("saved-session.json"));
    world.export_path = Some(tmpdir.path().join("transcript.md"));
    world.home_path = Some(tmpdir.path().join("home"));
    world.app = Some(Box::new(app));
    world.tmpdir = Some(tmpdir);
}

#[given("a CodeWhale persisted session workspace with one user message")]
fn persisted_workspace_with_one_user_message(world: &mut SessionCommandWorld) {
    workspace_with_one_user_message(world);
    let original_id = "original-session".to_string();
    let app = world.app.as_deref_mut().expect("app should exist");
    app.current_session_id = Some(original_id.clone());
    world.original_session_id = Some(original_id);
    persist_active_session(world);
}

#[given("a CodeWhale session workspace with stale and fresh saved sessions")]
fn workspace_with_stale_and_fresh_saved_sessions(world: &mut SessionCommandWorld) {
    workspace_with_one_user_message(world);
    persist_session_with_age(world, "fresh-session", "Fresh session", 1);
    persist_session_with_age(world, "stale-session", "Stale session", 30);
    world.fresh_session_id = Some("fresh-session".to_string());
    world.stale_session_id = Some("stale-session".to_string());
}

#[when("the user saves the active session")]
fn user_saves_active_session(world: &mut SessionCommandWorld) {
    let save_path = world
        .save_path
        .as_ref()
        .expect("save path should exist")
        .to_string_lossy()
        .to_string();
    let result = execute_isolated(world, &format!("/save {save_path}"));
    remember_result(world, &result);

    assert!(!result.is_error, "save failed: {:?}", result.message);
    assert!(
        world.save_path.as_ref().expect("save path").exists(),
        "save command should write the session file"
    );
}

#[when("the user exports the active transcript")]
fn user_exports_active_transcript(world: &mut SessionCommandWorld) {
    let export_path = world
        .export_path
        .as_ref()
        .expect("export path should exist")
        .to_string_lossy()
        .to_string();
    let result = execute_isolated(world, &format!("/export {export_path}"));
    remember_result(world, &result);

    assert!(!result.is_error, "export failed: {:?}", result.message);
    assert!(
        world.export_path.as_ref().expect("export path").exists(),
        "export command should write the transcript"
    );
}

#[when("the user clears the active conversation")]
fn user_clears_active_conversation(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/clear");
    remember_result(world, &result);

    assert!(!result.is_error, "clear failed: {:?}", result.message);
    let app = world.app.as_deref().expect("app should exist");
    assert!(
        app.api_messages.is_empty(),
        "clear command should remove active API messages"
    );
    assert_eq!(app.session.total_tokens, 0);
}

#[when("the user loads the saved session")]
fn user_loads_saved_session(world: &mut SessionCommandWorld) {
    let save_path = world
        .save_path
        .as_ref()
        .expect("save path should exist")
        .to_string_lossy()
        .to_string();
    let result = execute_isolated(world, &format!("/load {save_path}"));
    remember_result(world, &result);

    assert!(!result.is_error, "load failed: {:?}", result.message);
    world.last_message = result.message;
}

#[when("the user forks the active session")]
fn user_forks_active_session(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/fork");
    remember_result(world, &result);

    assert!(!result.is_error, "fork failed: {:?}", result.message);
    let fork_id = world
        .app
        .as_deref()
        .and_then(|app| app.current_session_id.clone())
        .expect("fork command should switch to a child session");
    let forked = load_saved_session(world, &fork_id);
    if world.original_session_id.is_none() {
        world.original_session_id = forked.metadata.parent_session_id.clone();
    }
    world.fork_session_id = Some(fork_id);
}

#[when("the user tries to fork the active session")]
fn user_tries_to_fork_active_session(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/fork");
    remember_result(world, &result);
}

#[when("the user starts a new session")]
fn user_starts_new_session(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/new");
    remember_result(world, &result);

    assert!(!result.is_error, "new session failed: {:?}", result.message);
    let new_id = world
        .app
        .as_deref()
        .and_then(|app| app.current_session_id.clone())
        .expect("new command should set an active session id");
    world.new_session_id = Some(new_id);
}

#[when(regex = r#"^the user renames the active session to "([^"]+)"$"#)]
fn user_renames_active_session(world: &mut SessionCommandWorld, title: String) {
    let result = execute_isolated(world, &format!("/rename {title}"));
    remember_result(world, &result);

    assert!(!result.is_error, "rename failed: {:?}", result.message);
}

#[when("the user lists saved sessions")]
fn user_lists_saved_sessions(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/sessions list");
    remember_result(world, &result);

    assert!(
        !result.is_error,
        "sessions list failed: {:?}",
        result.message
    );
}

#[when(regex = r#"^the user prunes sessions older than (\d+) days$"#)]
fn user_prunes_sessions_older_than(world: &mut SessionCommandWorld, days: String) {
    let result = execute_isolated(world, &format!("/sessions prune {days}"));
    remember_result(world, &result);

    assert!(
        !result.is_error,
        "sessions prune failed: {:?}",
        result.message
    );
}

#[when("the user compacts context")]
fn user_compacts_context(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/compact");
    remember_result(world, &result);

    assert!(!result.is_error, "compact failed: {:?}", result.message);
}

#[when("the user purges context")]
fn user_purges_context(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/purge");
    remember_result(world, &result);

    assert!(!result.is_error, "purge failed: {:?}", result.message);
}

#[when(regex = r#"^the user prepares a session relay focused on "([^"]+)"$"#)]
fn user_prepares_session_relay_focused_on(world: &mut SessionCommandWorld, focus: String) {
    let result = execute_isolated(world, &format!("/relay {focus}"));
    remember_result(world, &result);

    assert!(!result.is_error, "relay failed: {:?}", result.message);
}

#[when("the user runs the singular session command")]
fn user_runs_singular_session_command(world: &mut SessionCommandWorld) {
    let result = execute_isolated(world, "/session");
    remember_result(world, &result);
}

#[then("the active session should contain the saved message")]
fn active_session_contains_saved_message(world: &mut SessionCommandWorld) {
    let app = world.app.as_deref().expect("app should exist");
    let message = app
        .api_messages
        .first()
        .expect("loaded session should have one message");
    let content = message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .expect("loaded message should have text content");

    assert_eq!(message.role, "user");
    assert_eq!(content, "Remember the whale migration");
}

#[then("the saved session file should contain the saved message")]
fn saved_session_file_contains_saved_message(world: &mut SessionCommandWorld) {
    let session = read_saved_session_file(world);

    assert_saved_session_contains_message(&session, "Remember the whale migration");
}

#[then("the load action should target the saved session file")]
fn load_action_targets_saved_session_file(world: &mut SessionCommandWorld) {
    let save_path = world.save_path.as_ref().expect("save path should exist");

    assert!(matches!(
        world.last_action.as_ref(),
        Some(AppAction::LoadSession(path)) if path == save_path
    ));
}

#[then("the exported markdown should contain the active transcript")]
fn exported_markdown_contains_active_transcript(world: &mut SessionCommandWorld) {
    let export_path = world
        .export_path
        .as_ref()
        .expect("export path should exist");
    let content = std::fs::read_to_string(export_path)
        .unwrap_or_else(|err| panic!("read exported transcript {export_path:?}: {err}"));

    assert!(content.contains("# Codewhale conversation export"));
    assert!(content.contains("## 1. user"));
    assert!(content.contains("Remember the whale migration"));
}

#[then("CodeWhale should defer the session-loaded receipt to the event loop")]
fn codewhale_defers_session_loaded_receipt(world: &mut SessionCommandWorld) {
    assert_eq!(
        world.last_message, None,
        "the command layer must not report success before the event loop applies the load action"
    );
}

#[then("the forked session should reference the original session")]
fn forked_session_references_original_session(world: &mut SessionCommandWorld) {
    let original_id = world
        .original_session_id
        .as_deref()
        .expect("original session id should exist");
    let fork_id = world
        .fork_session_id
        .as_deref()
        .expect("fork session id should exist");
    let forked = load_saved_session(world, fork_id);

    assert_eq!(
        forked.metadata.parent_session_id.as_deref(),
        Some(original_id)
    );
    assert_eq!(forked.metadata.forked_from_message_count, Some(1));
}

#[then("the original session should still be loadable")]
fn original_session_still_loadable(world: &mut SessionCommandWorld) {
    let original_id = world
        .original_session_id
        .as_deref()
        .expect("original session id should exist");
    let original = load_saved_session(world, original_id);

    assert_saved_session_contains_message(&original, "Remember the whale migration");
}

#[then("the active session should be the forked session")]
fn active_session_is_forked_session(world: &mut SessionCommandWorld) {
    let fork_id = world
        .fork_session_id
        .as_deref()
        .expect("fork session id should exist");
    let app = world.app.as_deref().expect("app should exist");

    assert_eq!(app.current_session_id.as_deref(), Some(fork_id));
    assert_app_contains_message(app, "Remember the whale migration");
}

#[then("CodeWhale should reject the fork because there are no messages")]
fn codewhale_rejects_empty_fork(world: &mut SessionCommandWorld) {
    assert_eq!(
        world.last_result_is_error,
        Some(true),
        "last command should have failed"
    );
    let message = world
        .last_message
        .as_deref()
        .expect("fork rejection should include a message");

    assert!(
        message.contains("Nothing to fork"),
        "unexpected fork rejection message: {message}"
    );
}

#[then("the active session should be empty")]
fn active_session_empty(world: &mut SessionCommandWorld) {
    let app = world.app.as_deref().expect("app should exist");

    assert!(app.api_messages.is_empty());
    assert_eq!(app.session.total_tokens, 0);
    assert_eq!(app.session.total_conversation_tokens, 0);
}

#[then("the original and forked sessions should remain loadable")]
fn original_and_forked_sessions_remain_loadable(world: &mut SessionCommandWorld) {
    let original_id = world
        .original_session_id
        .as_deref()
        .expect("original session id should exist");
    let fork_id = world
        .fork_session_id
        .as_deref()
        .expect("fork session id should exist");
    let original = load_saved_session(world, original_id);
    let forked = load_saved_session(world, fork_id);

    assert_saved_session_contains_message(&original, "Remember the whale migration");
    assert_saved_session_contains_message(&forked, "Remember the whale migration");
    assert_eq!(
        forked.metadata.parent_session_id.as_deref(),
        Some(original_id)
    );
}

#[then("the active session should be a new empty session")]
fn active_session_is_new_empty_session(world: &mut SessionCommandWorld) {
    let original_id = world
        .original_session_id
        .as_deref()
        .expect("original session id should exist");
    let fork_id = world
        .fork_session_id
        .as_deref()
        .expect("fork session id should exist");
    let new_id = world
        .new_session_id
        .as_deref()
        .expect("new session id should exist");
    let app = world.app.as_deref().expect("app should exist");

    assert_eq!(app.current_session_id.as_deref(), Some(new_id));
    assert_ne!(new_id, original_id);
    assert_ne!(new_id, fork_id);
    assert!(app.api_messages.is_empty());
    assert_eq!(app.session.total_tokens, 0);
}

#[then("the active session should be cleared into a fresh empty session")]
fn active_session_cleared_into_fresh_empty_session(world: &mut SessionCommandWorld) {
    let app = world.app.as_deref().expect("app should exist");

    // `/clear` claims the next session id in the App (like `/new`) so the
    // engine, the turn-start checkpoint, and autosave all share one id.
    let cleared_id = app
        .current_session_id
        .as_deref()
        .expect("/clear should claim a fresh session id");
    assert!(
        uuid::Uuid::parse_str(cleared_id).is_ok(),
        "fresh session id should be a uuid, got {cleared_id:?}"
    );
    for previous in [
        world.original_session_id.as_deref(),
        world.fork_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        assert_ne!(
            cleared_id, previous,
            "/clear must not reuse a saved session id"
        );
    }
    // When `/clear` was the last command its SyncSession must carry the same
    // id; a later `/load` in the same scenario leaves a different action.
    if let Some(AppAction::SyncSession {
        session_id,
        messages,
        ..
    }) = world.last_action.as_ref()
    {
        assert_eq!(session_id.as_deref(), Some(cleared_id));
        assert!(messages.is_empty());
    }
    assert!(app.api_messages.is_empty());
    assert_eq!(app.session.total_tokens, 0);
}

#[then(regex = r#"^the active saved session title should be "([^"]+)"$"#)]
fn active_saved_session_title_should_be(world: &mut SessionCommandWorld, expected: String) {
    let app = world.app.as_deref().expect("app should exist");
    let session_id = app
        .current_session_id
        .as_deref()
        .expect("active session id should exist");
    let saved = load_saved_session(world, session_id);

    assert_eq!(saved.metadata.title, expected);
}

#[then("the active session should be the original session")]
fn active_session_is_original_session(world: &mut SessionCommandWorld) {
    let original_id = world
        .original_session_id
        .as_deref()
        .expect("original session id should exist");
    let app = world.app.as_deref().expect("app should exist");

    assert_eq!(app.current_session_id.as_deref(), Some(original_id));
    assert_app_contains_message(app, "Remember the whale migration");
}

#[then("the session picker should be open")]
fn session_picker_should_be_open(world: &mut SessionCommandWorld) {
    let app = world.app.as_deref().expect("app should exist");

    assert_eq!(app.view_stack.top_kind(), Some(ModalKind::SessionPicker));
}

#[then("CodeWhale should report that one session was pruned")]
fn codewhale_reports_one_session_pruned(world: &mut SessionCommandWorld) {
    let message = world
        .last_message
        .as_deref()
        .expect("prune command should produce a message");

    assert!(
        message.contains("pruned 1 session"),
        "unexpected prune message: {message}"
    );
}

#[then("the fresh session should still be loadable")]
fn fresh_session_still_loadable(world: &mut SessionCommandWorld) {
    let fresh_id = world
        .fresh_session_id
        .as_deref()
        .expect("fresh session id should exist");
    let fresh = load_saved_session(world, fresh_id);

    assert_eq!(fresh.metadata.title, "Fresh session");
}

#[then("the stale session should no longer be loadable")]
fn stale_session_no_longer_loadable(world: &mut SessionCommandWorld) {
    let stale_id = world
        .stale_session_id
        .as_deref()
        .expect("stale session id should exist");

    assert!(
        try_load_saved_session(world, stale_id).is_err(),
        "stale session should have been pruned"
    );
}

#[then("CodeWhale should trigger context compaction")]
fn codewhale_triggers_context_compaction(world: &mut SessionCommandWorld) {
    assert_eq!(
        world.last_result_is_error,
        Some(false),
        "compact command should succeed"
    );
    assert!(matches!(
        world.last_action.as_ref(),
        Some(AppAction::CompactContext { .. })
    ));
    assert_eq!(world.last_message.as_deref(), Some("Making room…"));
}

#[then("CodeWhale should trigger context purge")]
fn codewhale_triggers_context_purge(world: &mut SessionCommandWorld) {
    assert_eq!(
        world.last_result_is_error,
        Some(false),
        "purge command should succeed"
    );
    assert!(matches!(
        world.last_action.as_ref(),
        Some(AppAction::PurgeContext)
    ));
    assert_eq!(
        world.last_message.as_deref(),
        Some("Agent context purge triggered...")
    );
}

#[then(regex = r#"^CodeWhale should send a session relay instruction focused on "([^"]+)"$"#)]
fn codewhale_sends_session_relay_instruction_focused_on(
    world: &mut SessionCommandWorld,
    focus: String,
) {
    assert_eq!(
        world.last_result_is_error,
        Some(false),
        "relay command should succeed"
    );
    let message = match world.last_action.as_ref() {
        Some(AppAction::SendMessage(message)) => message,
        other => panic!("expected relay SendMessage action, got {other:?}"),
    };

    assert!(message.contains("Write or update `.codewhale/handoff.md`."));
    assert!(message.contains("# Session relay"));
    assert!(message.contains("## Verification"));
    assert!(
        message.contains(&format!("- Requested relay focus: {focus}")),
        "relay instruction should include requested focus: {message}"
    );
    assert_eq!(
        world.last_message.as_deref(),
        Some("Preparing session relay at .codewhale/handoff.md...")
    );
}

#[then("CodeWhale should reject the unknown session command")]
fn codewhale_rejects_unknown_session_command(world: &mut SessionCommandWorld) {
    assert_eq!(
        world.last_result_is_error,
        Some(true),
        "singular /session should be rejected"
    );
    let message = world
        .last_message
        .as_deref()
        .expect("unknown command should include a message");

    assert!(
        message.contains("Unknown command: /session"),
        "unexpected unknown command message: {message}"
    );
    assert!(
        message.contains("/sessions") || message.contains("/save"),
        "unknown command should include a session-related suggestion: {message}"
    );
}

/// C01-09: `/save <path>` replaces a saved session, never an arbitrary file.
#[tokio::test(flavor = "current_thread")]
async fn save_refuses_to_overwrite_a_file_that_is_not_a_saved_session() {
    let mut world = SessionCommandWorld::default();
    workspace_with_one_user_message(&mut world);
    let target = world
        .tmpdir
        .as_ref()
        .expect("tmpdir")
        .path()
        .join("notes.txt");
    std::fs::write(&target, "keep me").expect("seed an unrelated file");

    let refused = execute_isolated(&mut world, &format!("/save {}", target.display()));
    assert!(refused.is_error, "{:?}", refused.message);
    assert_eq!(
        std::fs::read_to_string(&target).expect("still readable"),
        "keep me",
        "the unrelated file is untouched"
    );

    let saved = world.save_path.clone().expect("save path");
    for attempt in ["first save", "re-save over the saved session"] {
        let result = execute_isolated(&mut world, &format!("/save {}", saved.display()));
        assert!(!result.is_error, "{attempt}: {:?}", result.message);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn save_export_and_load_session_workflow() {
    run_scenario(SAVE_LOAD_SCENARIO, 10).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fork_keeps_original_session_resumable() {
    run_scenario(FORK_RESUMABLE_SCENARIO, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn new_session_cannot_be_forked_before_messages_exist() {
    run_scenario(NEW_THEN_FORK_SCENARIO, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cleared_session_cannot_be_forked_before_messages_exist() {
    run_scenario(CLEAR_THEN_FORK_SCENARIO, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fork_followed_by_new_keeps_both_saved_sessions() {
    run_scenario(FORK_THEN_NEW_SCENARIO, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fork_followed_by_clear_keeps_both_saved_sessions() {
    run_scenario(FORK_THEN_CLEAR_SCENARIO, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rename_updates_active_saved_session_title() {
    run_scenario(RENAME_SCENARIO, 4).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_list_opens_saved_session_picker() {
    run_scenario(SESSIONS_LIST_SCENARIO, 4).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_prune_removes_only_stale_sessions() {
    run_scenario(SESSIONS_PRUNE_SCENARIO, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_management_commands_emit_actions_without_clearing_active_session() {
    run_scenario(CONTEXT_MANAGEMENT_SCENARIO, 10).await;
}

#[tokio::test(flavor = "current_thread")]
async fn singular_session_command_is_not_registered() {
    run_scenario(SINGULAR_SESSION_SCENARIO, 4).await;
}

async fn run_scenario(name: &'static str, expected_steps: usize) {
    let writer = SessionCommandWorld::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(FEATURE_PATH, move |feature, _, scenario| {
            feature.name == FEATURE_NAME && scenario.name == name
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {name}");
    assert_eq!(writer.skipped_steps(), 0, "scenario skipped steps: {name}");
    assert_eq!(
        writer.passed_steps(),
        expected_steps,
        "scenario did not run: {name}"
    );
}

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

fn execute_isolated(world: &mut SessionCommandWorld, command: &str) -> CommandResult {
    let home = world
        .home_path
        .as_ref()
        .expect("test home should exist")
        .clone();
    std::fs::create_dir_all(&home).expect("create isolated test home");

    let _lock = lock_test_env();
    let _home = EnvVarGuard::set("HOME", &home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));

    let app = world.app.as_deref_mut().expect("app should exist");
    commands::user_registry::reload(Some(&app.workspace));
    commands::execute(command, app)
}

fn remember_result(world: &mut SessionCommandWorld, result: &CommandResult) {
    world.last_result_is_error = Some(result.is_error);
    world.last_message = result.message.clone();
    world.last_action = result.action.clone();
}

fn persist_active_session(world: &SessionCommandWorld) {
    let app = world.app.as_deref().expect("app should exist");
    let session_id = app
        .current_session_id
        .as_ref()
        .expect("active session id should exist")
        .clone();
    let session = create_saved_session_with_id_and_mode(
        session_id,
        &app.api_messages,
        &app.model,
        &app.workspace,
        u64::from(app.session.total_tokens),
        app.system_prompt.as_ref(),
        Some(app.mode.label()),
    );
    let home = world
        .home_path
        .as_ref()
        .expect("test home should exist")
        .clone();
    std::fs::create_dir_all(&home).expect("create isolated test home");

    let _lock = lock_test_env();
    let _home = EnvVarGuard::set("HOME", &home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));
    let manager = SessionManager::default_location().expect("open isolated session manager");

    manager
        .save_session(&session)
        .expect("persist active session");
}

fn persist_session_with_age(world: &SessionCommandWorld, session_id: &str, title: &str, days: i64) {
    let app = world.app.as_deref().expect("app should exist");
    let mut session = create_saved_session_with_id_and_mode(
        session_id.to_string(),
        &app.api_messages,
        &app.model,
        &app.workspace,
        u64::from(app.session.total_tokens),
        app.system_prompt.as_ref(),
        Some(app.mode.label()),
    );
    let timestamp = Utc::now() - ChronoDuration::days(days);
    session.metadata.title = title.to_string();
    session.metadata.created_at = timestamp;
    session.metadata.updated_at = timestamp;

    let home = world
        .home_path
        .as_ref()
        .expect("test home should exist")
        .clone();
    std::fs::create_dir_all(&home).expect("create isolated test home");

    let _lock = lock_test_env();
    let _home = EnvVarGuard::set("HOME", &home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));
    let manager = SessionManager::default_location().expect("open isolated session manager");

    manager.save_session(&session).expect("persist session");
}

fn load_saved_session(world: &SessionCommandWorld, session_id: &str) -> SavedSession {
    try_load_saved_session(world, session_id)
        .unwrap_or_else(|_| panic!("load saved session failed"))
}

fn try_load_saved_session(
    world: &SessionCommandWorld,
    session_id: &str,
) -> std::io::Result<SavedSession> {
    let home = world
        .home_path
        .as_ref()
        .expect("test home should exist")
        .clone();
    std::fs::create_dir_all(&home).expect("create isolated test home");

    let _lock = lock_test_env();
    let _home = EnvVarGuard::set("HOME", &home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));
    let manager = SessionManager::default_location().expect("open isolated session manager");

    manager.load_session(session_id)
}

fn read_saved_session_file(world: &SessionCommandWorld) -> SavedSession {
    let save_path = world.save_path.as_ref().expect("save path should exist");
    let content = std::fs::read_to_string(save_path)
        .unwrap_or_else(|err| panic!("read saved session file {save_path:?}: {err}"));

    serde_json::from_str(&content)
        .unwrap_or_else(|err| panic!("parse saved session file {save_path:?}: {err}"))
}

fn assert_app_contains_message(app: &App, expected: &str) {
    let message = app
        .api_messages
        .first()
        .expect("active session should contain one message");
    let content = message
        .content
        .iter()
        .find_map(text_content)
        .expect("active message should contain text");

    assert_eq!(message.role, "user");
    assert_eq!(content, expected);
}

fn assert_saved_session_contains_message(session: &SavedSession, expected: &str) {
    let message = session
        .messages
        .first()
        .expect("saved session should contain one message");
    let content = message
        .content
        .iter()
        .find_map(text_content)
        .expect("saved message should contain text");

    assert_eq!(message.role, "user");
    assert_eq!(content, expected);
}

fn text_content(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    }
}
