//! Focused Gherkin acceptance evidence for FEAT-011 dispatch precedence and
//! error semantics. Bound through separate scenario-level cucumber worlds
//! that prove AT-004 through AT-007 with the live dispatch entry point.

use cucumber::{World as _, given, then, when, writer::Stats as _};
use tempfile::TempDir;

use crate::commands::{self, CommandResult};
use crate::tui::app::{App, TuiOptions};
use codewhale_runtime::config::Config;

// --- FEAT-011 dispatch precedence constants ---

const DISPATCH_FEATURE_NAME: &str = "FEAT-011 Dispatch Precedence And Error Semantics";
const DISPATCH_FEATURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/features/feat-011-dispatch-precedence.feature"
);

const AT004_SCENARIO: &str = "AT-004 User command shadows built-in canonical name";
const AT005_SCENARIO: &str = "AT-005 User command shadows built-in alias";
const AT006_SCENARIO: &str = "AT-006 Absent user command falls back to built-in";
const AT007_SCENARIO: &str = "AT-007 Invalid user command produces user error without fallback";

// --- Shared helpers ---

fn create_dispatch_app(tmpdir: &TempDir) -> App {
    let options = TuiOptions {
        skills_dir: tmpdir.path().join("skills"),
        memory_path: tmpdir.path().join("memory.md"),
        notes_path: tmpdir.path().join("notes.txt"),
        mcp_config_path: tmpdir.path().join("mcp.json"),
        ..crate::tui::test_support::test_tui_options(tmpdir.path())
    };
    App::new(options, &Config::default())
}

fn write_user_command(tmpdir: &TempDir, name: &str, content: &str) {
    // Workspace commands load only in a trusted workspace.
    crate::test_support::trust_workspace(tmpdir.path());
    let commands_dir = tmpdir.path().join(".codewhale").join("commands");
    std::fs::create_dir_all(commands_dir).expect("create commands dir");
    let path = tmpdir
        .path()
        .join(".codewhale")
        .join("commands")
        .join(format!("{name}.md"));
    std::fs::write(path, content).expect("write user command");
}

fn sent_message(result: &CommandResult) -> String {
    match &result.action {
        Some(crate::tui::app::AppAction::SendMessage(message)) => message.clone(),
        other => panic!("expected SendMessage action, got {other:?}"),
    }
}

// --- AT-004: User command shadows built-in canonical name ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DispatchWorld004 {
    tmpdir: Option<TempDir>,
    app: Option<Box<App>>,
    result: Option<CommandResult>,
}

impl std::fmt::Debug for DispatchWorld004 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchWorld004")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_app", &self.app.is_some())
            .field("has_result", &self.result.is_some())
            .finish()
    }
}

impl DispatchWorld004 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            app: None,
            result: None,
        }
    }
}

#[given("a workspace with a user command shadowing a built-in canonical name")]
fn at004_given_shadow_canonical(world: &mut DispatchWorld004) {
    let tmpdir = TempDir::new().expect("AT-004 TempDir");
    write_user_command(
        &tmpdir,
        "help",
        "---\ndescription: Custom help\n---\ncustom help $ARGUMENTS",
    );
    let mut app = create_dispatch_app(&tmpdir);
    app.workspace = tmpdir.path().to_path_buf();
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
    world.app = Some(Box::new(app));
}

#[when(regex = r#"^the user runs "/help config"$"#)]
fn at004_when_run_shadowed(world: &mut DispatchWorld004) {
    let app = world.app.as_deref_mut().expect("app should exist");
    let result = commands::execute("/help config", app);
    world.result = Some(result);
}

#[then("the user command executes instead of the built-in")]
fn at004_then_user_executes(world: &mut DispatchWorld004) {
    let result = world.result.as_ref().expect("result should exist");
    assert!(
        !result.is_error,
        "user command should succeed: {:?}",
        result.message
    );
    assert_eq!(
        sent_message(result),
        "custom help config",
        "user command should produce custom content"
    );
}

#[then("no built-in /help side effect occurs")]
fn at004_then_no_builtin(world: &mut DispatchWorld004) {
    let result = world.result.as_ref().expect("result should exist");
    assert!(!result.is_error, "no error");
    match &result.action {
        Some(crate::tui::app::AppAction::SendMessage(message)) => {
            assert!(
                message.contains("custom help"),
                "message should contain user command content: {message}"
            );
        }
        other => panic!("expected SendMessage, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn feat011_at004_user_command_shadows_builtin_canonical_name() {
    let writer = DispatchWorld004::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISPATCH_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISPATCH_FEATURE_NAME && scenario.name == AT004_SCENARIO
        })
        .await;
    assert_eq!(
        writer.failed_steps(),
        0,
        "scenario failed: {AT004_SCENARIO}"
    );
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped steps: {AT004_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        4,
        "scenario did not run: {AT004_SCENARIO}"
    );
}

// --- AT-005: User command shadows built-in alias ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DispatchWorld005 {
    tmpdir: Option<TempDir>,
    app: Option<Box<App>>,
    alias_result: Option<CommandResult>,
    canonical_result: Option<CommandResult>,
}

impl std::fmt::Debug for DispatchWorld005 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchWorld005")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_app", &self.app.is_some())
            .field("has_alias_result", &self.alias_result.is_some())
            .field("has_canonical_result", &self.canonical_result.is_some())
            .finish()
    }
}

impl DispatchWorld005 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            app: None,
            alias_result: None,
            canonical_result: None,
        }
    }
}

#[given("a workspace with a user command shadowing a built-in alias")]
fn at005_given_shadow_alias(world: &mut DispatchWorld005) {
    let tmpdir = TempDir::new().expect("AT-005 TempDir");
    // /links has alias /dashboard and /api. Create a user command that
    // shadows the /dashboard alias.
    write_user_command(
        &tmpdir,
        "attach-review",
        "---\nalias: dashboard\n---\ncustom dashboard $ARGUMENTS",
    );
    let mut app = create_dispatch_app(&tmpdir);
    app.workspace = tmpdir.path().to_path_buf();
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
    world.app = Some(Box::new(app));
}

#[when("the user runs the shadowed alias")]
fn at005_when_run_alias(world: &mut DispatchWorld005) {
    let app = world.app.as_deref_mut().expect("app should exist");
    // Use /dashboard which is shadowed by the user command's alias.
    let alias_result = commands::execute("/dashboard", app);
    world.alias_result = Some(alias_result);

    // Also test that the built-in canonical name (/links) still works.
    let canonical_result = commands::execute("/links", app);
    world.canonical_result = Some(canonical_result);
}

#[then("the user command executes")]
fn at005_then_user_executes(world: &mut DispatchWorld005) {
    let result = world
        .alias_result
        .as_ref()
        .expect("alias result should exist");
    assert!(!result.is_error, "user command dispatch should succeed");
    assert_eq!(
        sent_message(result),
        "custom dashboard ",
        "user alias should produce custom content"
    );
}

#[then("the built-in canonical name remains reachable")]
fn at005_then_canonical_reachable(world: &mut DispatchWorld005) {
    let result = world
        .canonical_result
        .as_ref()
        .expect("canonical result should exist");
    assert!(!result.is_error, "canonical built-in should still work");
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|msg| msg.contains("https://")),
        "canonical /links should return platform links: {:?}",
        result.message
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat011_at005_user_command_shadows_builtin_alias() {
    let writer = DispatchWorld005::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISPATCH_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISPATCH_FEATURE_NAME && scenario.name == AT005_SCENARIO
        })
        .await;
    assert_eq!(
        writer.failed_steps(),
        0,
        "scenario failed: {AT005_SCENARIO}"
    );
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped steps: {AT005_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        4,
        "scenario did not run: {AT005_SCENARIO}"
    );
}

// --- AT-006: Absent user command falls back to built-in ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DispatchWorld006 {
    tmpdir: Option<TempDir>,
    app: Option<Box<App>>,
    after_removal_result: Option<CommandResult>,
}

impl std::fmt::Debug for DispatchWorld006 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchWorld006")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_app", &self.app.is_some())
            .field("has_result", &self.after_removal_result.is_some())
            .finish()
    }
}

impl DispatchWorld006 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            app: None,
            after_removal_result: None,
        }
    }
}

#[given("a workspace with a previously loaded user command")]
fn at006_given_loaded_user_command(world: &mut DispatchWorld006) {
    let tmpdir = TempDir::new().expect("AT-006 TempDir");
    write_user_command(&tmpdir, "help", "user help");
    let mut app = create_dispatch_app(&tmpdir);
    app.workspace = tmpdir.path().to_path_buf();
    commands::user_registry::reload(Some(tmpdir.path()));

    // Verify user command dispatches first
    let initial_result = commands::execute("/help config", &mut app);
    assert!(
        matches!(
            &initial_result.action,
            Some(crate::tui::app::AppAction::SendMessage(_))
        ),
        "user command should dispatch initially"
    );

    world.tmpdir = Some(tmpdir);
    world.app = Some(Box::new(app));
}

#[when("the user command file is removed and the command is invoked again")]
fn at006_when_removed_and_invoked(world: &mut DispatchWorld006) {
    let app = world.app.as_deref_mut().expect("app should exist");
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    let command_path = tmpdir
        .path()
        .join(".codewhale")
        .join("commands")
        .join("help.md");

    // Remove the user command file
    std::fs::remove_file(&command_path).expect("remove user command file");
    commands::user_registry::reload(Some(tmpdir.path()));

    // Invoke the (now absent) command — should fall back to built-in
    let result = commands::execute("/help config", app);
    world.after_removal_result = Some(result);
}

#[then("the built-in command executes without a user-command error message")]
fn at006_then_builtin_executes(world: &mut DispatchWorld006) {
    let result = world
        .after_removal_result
        .as_ref()
        .expect("result should exist");
    assert!(!result.is_error, "built-in fallback should not error");
    let message = result.message.as_deref().unwrap_or("");
    // The built-in /help config message should mention the config command.
    assert!(
        message.contains("config"),
        "built-in /help should handle the command: {message}"
    );
    // No user-command error text should appear.
    assert!(
        !message.contains("User command"),
        "should not contain user-command error: {message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat011_at006_absent_user_command_falls_back_to_builtin() {
    let writer = DispatchWorld006::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISPATCH_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISPATCH_FEATURE_NAME && scenario.name == AT006_SCENARIO
        })
        .await;
    assert_eq!(
        writer.failed_steps(),
        0,
        "scenario failed: {AT006_SCENARIO}"
    );
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped steps: {AT006_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        3,
        "scenario did not run: {AT006_SCENARIO}"
    );
}

// --- AT-007: Invalid user command produces user error without fallback ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DispatchWorld007 {
    tmpdir: Option<TempDir>,
    app: Option<Box<App>>,
    result: Option<CommandResult>,
}

impl std::fmt::Debug for DispatchWorld007 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchWorld007")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_app", &self.app.is_some())
            .field("has_result", &self.result.is_some())
            .finish()
    }
}

impl DispatchWorld007 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            app: None,
            result: None,
        }
    }
}

#[given("a workspace with an invalid user command")]
fn at007_given_invalid_command(world: &mut DispatchWorld007) {
    let tmpdir = TempDir::new().expect("AT-007 TempDir");
    // Invalid frontmatter (not valid YAML) on a name that shadows a built-in.
    write_user_command(
        &tmpdir,
        "help",
        "---\ndescription: Custom help\nnot valid yaml\n---\ncustom help",
    );
    let mut app = create_dispatch_app(&tmpdir);
    app.workspace = tmpdir.path().to_path_buf();
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
    world.app = Some(Box::new(app));
}

#[when("the user runs the invalid command")]
fn at007_when_run_invalid(world: &mut DispatchWorld007) {
    let app = world.app.as_deref_mut().expect("app should exist");
    let result = commands::execute("/help", app);
    world.result = Some(result);
}

#[then("a user-command-specific error is returned")]
fn at007_then_user_error(world: &mut DispatchWorld007) {
    let result = world.result.as_ref().expect("result should exist");
    assert!(result.is_error, "invalid command should produce error");
    let message = result
        .message
        .as_deref()
        .expect("error message should exist");
    assert!(
        message.contains("User command"),
        "error should identify the user command: {message}"
    );
    assert!(
        message.contains("invalid frontmatter"),
        "error should describe the problem: {message}"
    );
}

#[then("no built-in fallback occurs")]
fn at007_then_no_fallback(world: &mut DispatchWorld007) {
    let result = world.result.as_ref().expect("result should exist");
    assert!(result.is_error, "result should remain an error");
    // The built-in /help would return a success result. An error result
    // with a user-command-specific message proves no built-in fallback.
    let message = result.message.as_deref().expect("error message");
    assert!(
        !message.contains("Type /help for available commands"),
        "should not suggest built-in help: {message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat011_at007_invalid_user_command_produces_user_error_without_fallback() {
    let writer = DispatchWorld007::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISPATCH_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISPATCH_FEATURE_NAME && scenario.name == AT007_SCENARIO
        })
        .await;
    assert_eq!(
        writer.failed_steps(),
        0,
        "scenario failed: {AT007_SCENARIO}"
    );
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped steps: {AT007_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        4,
        "scenario did not run: {AT007_SCENARIO}"
    );
}
