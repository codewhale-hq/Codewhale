//! Focused Gherkin acceptance evidence for FEAT-012 palette and slash
//! completion discovery filtering. Bound through separate scenario-level
//! cucumber worlds that prove the six FEAT-012 acceptance criteria plus the
//! alias-aware canonical unification decision (AT-010) through the live
//! palette builder, the live completion function, and the live dispatch
//! entry point.

use cucumber::{World as _, given, then, when, writer::Stats as _};
use tempfile::TempDir;

use crate::commands::{self, CommandResult};
use crate::tui::app::{App, TuiOptions};
use crate::tui::command_palette::{self, CommandPaletteEntry};
use crate::tui::widgets::{self, SlashMenuEntry};
use codewhale_config::ProviderKind;
use codewhale_localization::Locale;
use codewhale_runtime::config::Config;

// --- FEAT-012 discovery filtering constants ---

const DISCOVERY_FEATURE_NAME: &str = "FEAT-012 Discovery Filtering (Palette And Slash Completion)";
const DISCOVERY_FEATURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/features/feat-012-discovery-filtering.feature"
);

const AC1_SCENARIO: &str = "AC1 Visible user command appears once in the palette with metadata";
const AC2_SCENARIO: &str = "AC2 Hidden user command is runnable but excluded from the palette";
const AC3_SCENARIO: &str = "AC3 Visible user command appears in matching slash completion";
const AC4_SCENARIO: &str = "AC4 Hidden user command is excluded from slash completion";
const AC5_SCENARIO: &str = "AC5 User canonical shadow suppresses a built-in in both surfaces";
const AC6_SCENARIO: &str =
    "AC6 User command shadows a built-in alias without hiding canonical access";
const AT010_SCENARIO: &str =
    "AT-010 Alias-aware unification - accepted user alias claims a built-in canonical token";

// --- Shared helpers ---

fn create_discovery_app(tmpdir: &TempDir) -> App {
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

fn palette_entries(tmpdir: &TempDir) -> Vec<CommandPaletteEntry> {
    command_palette::build_entries(
        Locale::En,
        tmpdir.path().join("skills").as_path(),
        &Default::default(),
        tmpdir.path(),
        tmpdir.path().join("mcp.json").as_path(),
        None,
    )
}

fn completion_hints(tmpdir: &TempDir, input: &str) -> Vec<SlashMenuEntry> {
    widgets::slash_completion_hints(
        input,
        128,
        &[],
        Locale::En,
        Some(tmpdir.path()),
        ProviderKind::Deepseek,
    )
}

fn sent_message(result: &CommandResult) -> String {
    match &result.action {
        Some(crate::tui::app::AppAction::SendMessage(message)) => message.clone(),
        other => panic!("expected SendMessage action, got {other:?}"),
    }
}

// --- AC1: Visible user command appears once in the palette with metadata ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscoveryWorld01 {
    tmpdir: Option<TempDir>,
    entries: Option<Vec<CommandPaletteEntry>>,
}

impl std::fmt::Debug for DiscoveryWorld01 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWorld01")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_entries", &self.entries.is_some())
            .finish()
    }
}

impl DiscoveryWorld01 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            entries: None,
        }
    }
}

#[given("a workspace with a visible user command that has a description and usage")]
fn ac1_given_visible_with_metadata(world: &mut DiscoveryWorld01) {
    let tmpdir = TempDir::new().expect("AC1 TempDir");
    write_user_command(
        &tmpdir,
        "review",
        "---\ndescription: Review with context\nusage: /review <path>\n---\nreview $ARGUMENTS",
    );
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
}

#[when("the command palette is queried")]
fn ac1_when_palette_queried(world: &mut DiscoveryWorld01) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.entries = Some(palette_entries(tmpdir));
}

#[then("the user command appears exactly once with its description and usage")]
fn ac1_then_user_entry_once(world: &mut DiscoveryWorld01) {
    let entries = world.entries.as_ref().expect("entries should exist");
    let rows: Vec<_> = entries
        .iter()
        .filter(|entry| entry.label == "/review")
        .collect();
    assert_eq!(rows.len(), 1, "exactly one /review row must exist");
    assert!(
        rows[0].description.contains("Review with context"),
        "row must carry the description: {}",
        rows[0].description
    );
    assert!(
        rows[0].description.contains("/review <path>"),
        "row must carry the usage: {}",
        rows[0].description
    );
}

#[then("no duplicate entry appears for the same effective token")]
fn ac1_then_no_duplicate(world: &mut DiscoveryWorld01) {
    let entries = world.entries.as_ref().expect("entries should exist");
    let labels: Vec<_> = entries.iter().map(|entry| entry.label.clone()).collect();
    let duplicates: Vec<_> = labels
        .iter()
        .filter(|label| labels.iter().filter(|other| other == label).count() > 1)
        .collect();
    assert!(
        duplicates.is_empty(),
        "no duplicate labels may appear: {duplicates:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat012_ac1_visible_user_command_appears_once_in_palette() {
    let writer = DiscoveryWorld01::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISCOVERY_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISCOVERY_FEATURE_NAME && scenario.name == AC1_SCENARIO
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {AC1_SCENARIO}");
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped: {AC1_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        4,
        "scenario did not run: {AC1_SCENARIO}"
    );
}

// --- AC2: Hidden user command is runnable but excluded from the palette ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscoveryWorld02 {
    tmpdir: Option<TempDir>,
    app: Option<Box<App>>,
    result: Option<CommandResult>,
    entries: Option<Vec<CommandPaletteEntry>>,
}

impl std::fmt::Debug for DiscoveryWorld02 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWorld02")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_app", &self.app.is_some())
            .field("has_result", &self.result.is_some())
            .field("has_entries", &self.entries.is_some())
            .finish()
    }
}

impl DiscoveryWorld02 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            app: None,
            result: None,
            entries: None,
        }
    }
}

#[given("a workspace with a hidden user command")]
fn ac2_given_hidden(world: &mut DiscoveryWorld02) {
    let tmpdir = TempDir::new().expect("AC2 TempDir");
    write_user_command(
        &tmpdir,
        "secret",
        "---\ndescription: Internal workflow\nhidden: true\n---\nsecret $ARGUMENTS",
    );
    let mut app = create_discovery_app(&tmpdir);
    app.workspace = tmpdir.path().to_path_buf();
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
    world.app = Some(Box::new(app));
}

#[when("the user runs the hidden command directly")]
fn ac2_when_run_hidden(world: &mut DiscoveryWorld02) {
    let app = world.app.as_deref_mut().expect("app should exist");
    let result = commands::execute("/secret now", app);
    world.result = Some(result);
}

#[then("the user command executes")]
fn ac2_then_user_executes(world: &mut DiscoveryWorld02) {
    let result = world.result.as_ref().expect("result should exist");
    assert!(!result.is_error, "hidden command should run: {result:?}");
    assert_eq!(sent_message(result), "secret now");
}

#[when("the command palette is queried")]
fn ac2_when_palette_queried(world: &mut DiscoveryWorld02) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.entries = Some(palette_entries(tmpdir));
}

#[then("the hidden command is absent from the palette")]
fn ac2_then_hidden_absent(world: &mut DiscoveryWorld02) {
    let entries = world.entries.as_ref().expect("entries should exist");
    assert!(
        !entries.iter().any(|entry| entry.label == "/secret"),
        "hidden command must not be listed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat012_ac2_hidden_user_command_runnable_but_excluded_from_palette() {
    let writer = DiscoveryWorld02::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISCOVERY_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISCOVERY_FEATURE_NAME && scenario.name == AC2_SCENARIO
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {AC2_SCENARIO}");
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped: {AC2_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        5,
        "scenario did not run: {AC2_SCENARIO}"
    );
}

// --- AC3: Visible user command appears in matching slash completion ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscoveryWorld03 {
    tmpdir: Option<TempDir>,
    hints: Option<Vec<SlashMenuEntry>>,
}

impl std::fmt::Debug for DiscoveryWorld03 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWorld03")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_hints", &self.hints.is_some())
            .finish()
    }
}

impl DiscoveryWorld03 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            hints: None,
        }
    }
}

#[given("a workspace with a visible user command that has a description")]
fn ac3_given_visible(world: &mut DiscoveryWorld03) {
    let tmpdir = TempDir::new().expect("AC3 TempDir");
    write_user_command(
        &tmpdir,
        "deploy",
        "---\ndescription: Deploy target\n---\ndeploy $ARGUMENTS",
    );
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
}

#[when("slash completion is queried with a matching prefix")]
fn ac3_when_completion_matching(world: &mut DiscoveryWorld03) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.hints = Some(completion_hints(tmpdir, "/dep"));
}

#[then("the user command appears exactly once with its description")]
fn ac3_then_user_completion_once(world: &mut DiscoveryWorld03) {
    let hints = world.hints.as_ref().expect("hints should exist");
    let rows: Vec<_> = hints.iter().filter(|hint| hint.name == "/deploy").collect();
    assert_eq!(rows.len(), 1, "exactly one /deploy hint must exist");
    assert_eq!(rows[0].description, "Deploy target");
}

#[tokio::test(flavor = "current_thread")]
async fn feat012_ac3_visible_user_command_appears_in_matching_completion() {
    let writer = DiscoveryWorld03::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISCOVERY_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISCOVERY_FEATURE_NAME && scenario.name == AC3_SCENARIO
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {AC3_SCENARIO}");
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped: {AC3_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        3,
        "scenario did not run: {AC3_SCENARIO}"
    );
}

// --- AC4: Hidden user command is excluded from slash completion ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscoveryWorld04 {
    tmpdir: Option<TempDir>,
    hints: Option<Vec<SlashMenuEntry>>,
}

impl std::fmt::Debug for DiscoveryWorld04 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWorld04")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_hints", &self.hints.is_some())
            .finish()
    }
}

impl DiscoveryWorld04 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            hints: None,
        }
    }
}

#[given("a workspace with a hidden user command")]
fn ac4_given_hidden(world: &mut DiscoveryWorld04) {
    let tmpdir = TempDir::new().expect("AC4 TempDir");
    write_user_command(
        &tmpdir,
        "secret",
        "---\ndescription: Internal workflow\nhidden: true\n---\nsecret $ARGUMENTS",
    );
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
}

#[when("slash completion is queried for its prefix")]
fn ac4_when_completion_prefix(world: &mut DiscoveryWorld04) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.hints = Some(completion_hints(tmpdir, "/sec"));
}

#[then("the hidden command is absent from slash completion")]
fn ac4_then_hidden_absent(world: &mut DiscoveryWorld04) {
    let hints = world.hints.as_ref().expect("hints should exist");
    assert!(
        !hints.iter().any(|hint| hint.name == "/secret"),
        "hidden command must not be suggested"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat012_ac4_hidden_user_command_excluded_from_completion() {
    let writer = DiscoveryWorld04::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISCOVERY_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISCOVERY_FEATURE_NAME && scenario.name == AC4_SCENARIO
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {AC4_SCENARIO}");
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped: {AC4_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        3,
        "scenario did not run: {AC4_SCENARIO}"
    );
}

// --- AC5: User canonical shadow suppresses a built-in in both surfaces ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscoveryWorld05 {
    tmpdir: Option<TempDir>,
    entries: Option<Vec<CommandPaletteEntry>>,
    hints: Option<Vec<SlashMenuEntry>>,
}

impl std::fmt::Debug for DiscoveryWorld05 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWorld05")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_entries", &self.entries.is_some())
            .field("has_hints", &self.hints.is_some())
            .finish()
    }
}

impl DiscoveryWorld05 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            entries: None,
            hints: None,
        }
    }
}

#[given("a workspace with a visible user command owning a built-in canonical token")]
fn ac5_given_canonical_shadow(world: &mut DiscoveryWorld05) {
    let tmpdir = TempDir::new().expect("AC5 TempDir");
    write_user_command(
        &tmpdir,
        "my-help",
        "---\nname: help\ndescription: My private help\n---\ncustom help $ARGUMENTS",
    );
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
}

#[when("the command palette and slash completion are queried")]
fn ac5_when_both_queried(world: &mut DiscoveryWorld05) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.entries = Some(palette_entries(tmpdir));
    world.hints = Some(completion_hints(tmpdir, "/help"));
}

#[then("only the user-owned discovery entry appears for that token")]
fn ac5_then_only_user_entry(world: &mut DiscoveryWorld05) {
    let entries = world.entries.as_ref().expect("entries should exist");
    let palette_rows: Vec<_> = entries
        .iter()
        .filter(|entry| entry.label == "/help")
        .collect();
    assert_eq!(palette_rows.len(), 1, "exactly one palette /help row");
    assert!(
        palette_rows[0].description.contains("My private help"),
        "palette row must be user-owned: {}",
        palette_rows[0].description
    );

    let hints = world.hints.as_ref().expect("hints should exist");
    let completion_rows: Vec<_> = hints.iter().filter(|hint| hint.name == "/help").collect();
    assert_eq!(completion_rows.len(), 1, "exactly one completion /help row");
}

#[then("its metadata identifies the user command")]
fn ac5_then_user_metadata(world: &mut DiscoveryWorld05) {
    let hints = world.hints.as_ref().expect("hints should exist");
    let completion_rows: Vec<_> = hints.iter().filter(|hint| hint.name == "/help").collect();
    assert_eq!(completion_rows.len(), 1);
    assert!(
        completion_rows[0].description.contains("My private help"),
        "completion row must carry user metadata: {}",
        completion_rows[0].description
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat012_ac5_canonical_shadow_suppresses_builtin_in_both_surfaces() {
    let writer = DiscoveryWorld05::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISCOVERY_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISCOVERY_FEATURE_NAME && scenario.name == AC5_SCENARIO
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {AC5_SCENARIO}");
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped: {AC5_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        4,
        "scenario did not run: {AC5_SCENARIO}"
    );
}

// --- AC6: User command shadows a built-in alias without hiding canonical access ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscoveryWorld06 {
    tmpdir: Option<TempDir>,
    alias_hints: Option<Vec<SlashMenuEntry>>,
    canonical_hints: Option<Vec<SlashMenuEntry>>,
}

impl std::fmt::Debug for DiscoveryWorld06 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWorld06")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_alias_hints", &self.alias_hints.is_some())
            .field("has_canonical_hints", &self.canonical_hints.is_some())
            .finish()
    }
}

impl DiscoveryWorld06 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            alias_hints: None,
            canonical_hints: None,
        }
    }
}

#[given("a workspace with a visible user command owning a built-in alias token")]
fn ac6_given_alias_shadow(world: &mut DiscoveryWorld06) {
    let tmpdir = TempDir::new().expect("AC6 TempDir");
    write_user_command(
        &tmpdir,
        "image-review",
        "---\ndescription: Review an image\nalias: image\n---\nreview image",
    );
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
}

#[when("slash completion is queried for the shadowed alias")]
fn ac6_when_alias_completion(world: &mut DiscoveryWorld06) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.alias_hints = Some(completion_hints(tmpdir, "/image"));
}

#[then("the user command appears and the aliased built-in does not")]
fn ac6_then_alias_owned(world: &mut DiscoveryWorld06) {
    let hints = world
        .alias_hints
        .as_ref()
        .expect("alias hints should exist");
    assert!(
        hints.iter().any(|hint| hint.name == "/image-review"),
        "user command must complete through the /image alias"
    );
    assert!(
        !hints.iter().any(|hint| hint.name == "/attach"),
        "built-in /attach must not complete through shadowed /image alias"
    );
}

#[when("slash completion is queried for the built-in canonical prefix")]
fn ac6_when_canonical_completion(world: &mut DiscoveryWorld06) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.canonical_hints = Some(completion_hints(tmpdir, "/att"));
}

#[then("the built-in canonical command remains available")]
fn ac6_then_canonical_available(world: &mut DiscoveryWorld06) {
    let hints = world
        .canonical_hints
        .as_ref()
        .expect("canonical hints should exist");
    assert!(
        hints.iter().any(|hint| hint.name == "/attach"),
        "canonical /attach must remain available"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat012_ac6_alias_shadow_preserves_canonical_access() {
    let writer = DiscoveryWorld06::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISCOVERY_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISCOVERY_FEATURE_NAME && scenario.name == AC6_SCENARIO
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {AC6_SCENARIO}");
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped: {AC6_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        5,
        "scenario did not run: {AC6_SCENARIO}"
    );
}

// --- AT-010: Accepted user alias claims a built-in canonical token ---

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscoveryWorld10 {
    tmpdir: Option<TempDir>,
    entries: Option<Vec<CommandPaletteEntry>>,
    hints: Option<Vec<SlashMenuEntry>>,
}

impl std::fmt::Debug for DiscoveryWorld10 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWorld10")
            .field("has_tmpdir", &self.tmpdir.is_some())
            .field("has_entries", &self.entries.is_some())
            .field("has_hints", &self.hints.is_some())
            .finish()
    }
}

impl DiscoveryWorld10 {
    fn new() -> Self {
        Self {
            tmpdir: None,
            entries: None,
            hints: None,
        }
    }
}

#[given(
    "a workspace with a visible user command whose accepted alias equals a built-in canonical token"
)]
fn at010_given_alias_claims_canonical(world: &mut DiscoveryWorld10) {
    let tmpdir = TempDir::new().expect("AT-010 TempDir");
    write_user_command(
        &tmpdir,
        "assistant",
        "---\ndescription: My assistant\nalias: help\n---\nassistant",
    );
    commands::user_registry::reload(Some(tmpdir.path()));
    world.tmpdir = Some(tmpdir);
}

#[when("the command palette and slash completion are queried for that token")]
fn at010_when_both_queried(world: &mut DiscoveryWorld10) {
    let tmpdir = world.tmpdir.as_ref().expect("tmpdir should exist");
    world.entries = Some(palette_entries(tmpdir));
    world.hints = Some(completion_hints(tmpdir, "/help"));
}

#[then("both surfaces suppress the built-in canonical entry")]
fn at010_then_both_suppress(world: &mut DiscoveryWorld10) {
    let entries = world.entries.as_ref().expect("entries should exist");
    assert!(
        !entries.iter().any(|entry| entry.label == "/help"),
        "palette must suppress built-in /help"
    );
    let hints = world.hints.as_ref().expect("hints should exist");
    assert!(
        !hints.iter().any(|hint| hint.name == "/help"),
        "completion must suppress built-in /help"
    );
}

#[then("the user command is the only discovery entry for that token")]
fn at010_then_only_user_entry(world: &mut DiscoveryWorld10) {
    let entries = world.entries.as_ref().expect("entries should exist");
    assert!(
        entries.iter().any(|entry| entry.label == "/assistant"),
        "user command must appear in the palette"
    );
    let hints = world.hints.as_ref().expect("hints should exist");
    assert!(
        hints.iter().any(|hint| hint.name == "/assistant"),
        "user command must appear in completion"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feat012_at010_alias_claims_builtin_canonical_token_consistently() {
    let writer = DiscoveryWorld10::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(DISCOVERY_FEATURE_PATH, move |feature, _, scenario| {
            feature.name == DISCOVERY_FEATURE_NAME && scenario.name == AT010_SCENARIO
        })
        .await;
    assert_eq!(
        writer.failed_steps(),
        0,
        "scenario failed: {AT010_SCENARIO}"
    );
    assert_eq!(
        writer.skipped_steps(),
        0,
        "scenario skipped: {AT010_SCENARIO}"
    );
    assert_eq!(
        writer.passed_steps(),
        4,
        "scenario did not run: {AT010_SCENARIO}"
    );
}
