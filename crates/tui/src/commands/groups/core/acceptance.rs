//! Gherkin acceptance coverage for visible core command surfaces.

use cucumber::{World as _, given, then, when, writer::Stats as _};
use tempfile::TempDir;

use crate::commands::{self, CommandResult};
use crate::test_support::{EnvVarGuard, lock_test_env};
use crate::tui::app::{App, TuiOptions};
use crate::tui::history::HistoryCell;
use codewhale_config::ProviderKind;
use codewhale_runtime::config::Config;

const FEATURE_NAME: &str = "Core command visible surfaces";
const FEATURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/features/core_command_surfaces.feature"
);
const INFORMATIONAL_SCENARIO: &str =
    "Core informational commands write visible transcript messages";
const STATE_SCENARIO: &str = "Core state commands report visible changes";
const CLEAR_SCENARIO: &str = "Clear replaces prior transcript with visible confirmation";
const PERSISTENT_WORK_SCENARIO: &str = "Persistent work commands report visible dispatch requests";

#[derive(Default, cucumber::World)]
struct CoreCommandWorld {
    tmpdir: Option<TempDir>,
    app: Option<Box<App>>,
    home_path: Option<std::path::PathBuf>,
    last_message: Option<String>,
    last_result_is_error: Option<bool>,
}

impl std::fmt::Debug for CoreCommandWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreCommandWorld")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_app", &self.app.is_some())
            .field("home_path", &self.home_path)
            .field("last_message", &self.last_message)
            .field("last_result_is_error", &self.last_result_is_error)
            .finish()
    }
}

#[given("a CodeWhale core command workspace")]
fn core_command_workspace(world: &mut CoreCommandWorld) {
    let tmpdir = TempDir::new().expect("core command TempDir");
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.ui_locale = codewhale_localization::Locale::En;
    app.api_provider = ProviderKind::Deepseek;
    app.model = "deepseek-v4-pro".to_string();
    app.auto_model = false;
    app.model_ids_passthrough = false;

    world.home_path = Some(tmpdir.path().join("home"));
    world.app = Some(Box::new(app));
    world.tmpdir = Some(tmpdir);
}

#[given("a CodeWhale core command workspace with one visible user message")]
fn core_command_workspace_with_one_visible_user_message(world: &mut CoreCommandWorld) {
    core_command_workspace(world);
    let app = world.app.as_deref_mut().expect("app should exist");
    app.add_message(HistoryCell::User {
        content: "Remember the whale migration".to_string(),
    });
}

#[when(regex = r#"^the user runs the core command "([^"]+)"$"#)]
fn user_runs_core_command(world: &mut CoreCommandWorld, command: String) {
    let result = execute_isolated(world, &command);
    record_visible_result(world, result);
}

#[then(regex = r#"^the message window should include "([^"]+)"$"#)]
fn message_window_should_include(world: &mut CoreCommandWorld, expected: String) {
    let visible = visible_message_window(world);

    assert!(
        visible.contains(&expected),
        "message window should include {expected:?}\nvisible transcript:\n{visible}"
    );
}

#[then(regex = r#"^the message window should not include "([^"]+)"$"#)]
fn message_window_should_not_include(world: &mut CoreCommandWorld, forbidden: String) {
    let visible = visible_message_window(world);

    assert!(
        !visible.contains(&forbidden),
        "message window should not include {forbidden:?}\nvisible transcript:\n{visible}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn core_informational_commands_write_visible_transcript_messages() {
    run_scenario(INFORMATIONAL_SCENARIO, 11).await;
}

#[tokio::test(flavor = "current_thread")]
async fn core_state_commands_report_visible_changes() {
    run_scenario(STATE_SCENARIO, 8).await;
}

#[tokio::test(flavor = "current_thread")]
async fn clear_replaces_prior_transcript_with_visible_confirmation() {
    run_scenario(CLEAR_SCENARIO, 4).await;
}

#[tokio::test(flavor = "current_thread")]
async fn persistent_work_commands_report_visible_dispatch_requests() {
    run_scenario(PERSISTENT_WORK_SCENARIO, 10).await;
}

async fn run_scenario(name: &'static str, expected_steps: usize) {
    let writer = CoreCommandWorld::cucumber()
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

fn execute_isolated(world: &mut CoreCommandWorld, command: &str) -> CommandResult {
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

fn record_visible_result(world: &mut CoreCommandWorld, result: CommandResult) {
    world.last_result_is_error = Some(result.is_error);
    world.last_message = result.message.clone();

    if let Some(message) = result.message {
        let app = world.app.as_deref_mut().expect("app should exist");
        app.add_message(HistoryCell::System { content: message });
    }
}

fn visible_message_window(world: &CoreCommandWorld) -> String {
    let app = world.app.as_deref().expect("app should exist");
    app.history
        .iter()
        .filter_map(|cell| match cell {
            HistoryCell::User { content }
            | HistoryCell::Assistant { content, .. }
            | HistoryCell::System { content }
            | HistoryCell::Thinking { content, .. } => Some(content.clone()),
            HistoryCell::Error { message, .. } => Some(message.clone()),
            HistoryCell::ArchivedContext { summary, .. } => Some(summary.clone()),
            HistoryCell::Automation(cell) => Some(cell.plain_summary()),
            HistoryCell::Tool(_) | HistoryCell::SubAgent(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
