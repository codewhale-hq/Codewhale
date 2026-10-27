//! TUI-hosted regression coverage retained from the pre-FEAT-024 control handlers.
//!
//! These tests dispatch through the public command seam. They deliberately
//! stay outside `groups/session`, which FEAT-043 moves to
//! `codewhale-commands`, while pinning persistence, sanitization, Git-target,
//! localization, and first-snapshot behavior against the real TUI adapter.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use crate::commands::groups::session::MAX_TITLE_LEN;
use crate::commands::{CommandResult, execute};
use crate::session_manager::{SessionManager, create_saved_session_with_mode};
use crate::test_support::{EnvVarGuard, TestEnvLock};
use crate::tui::app::{App, AppAction, TuiOptions};
use codewhale_config::AppMode;
use codewhale_config::ProviderKind;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_models::{ContentBlock, Message, Role, SystemPrompt};
use codewhale_runtime::config::Config;

/// Owns the global environment lock for as long as its CODEWHALE_HOME guard.
/// Fields are ordered so the guard restores the environment before the lock
/// is released and before the temporary directory is removed.
struct ControlHarness {
    app: App,
    manager: SessionManager,
    _home: EnvVarGuard,
    _env_lock: TestEnvLock,
    temp: TempDir,
}

impl ControlHarness {
    fn new() -> Self {
        let env_lock = crate::test_support::lock_test_env();
        let temp = TempDir::new().expect("tempdir");
        let home = EnvVarGuard::set("CODEWHALE_HOME", temp.path().join("home"));
        let options = TuiOptions {
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory.md"),
            notes_path: temp.path().join("notes.txt"),
            mcp_config_path: temp.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(temp.path())
        };
        let app = App::new(options, &Config::default());
        let manager = SessionManager::default_location().expect("session manager");
        Self {
            app,
            manager,
            _home: home,
            _env_lock: env_lock,
            temp,
        }
    }

    fn seed_session(&mut self) -> String {
        let session =
            create_saved_session_with_mode(&[], "deepseek-v4-pro", self.temp.path(), 0, None, None);
        let session_id = session.metadata.id.clone();
        self.manager.save_session(&session).expect("save session");
        self.app.current_session_id = Some(session_id.clone());
        session_id
    }
}

fn dispatch(app: &mut App, name: &str, arg: Option<&str>) -> CommandResult {
    let command = match arg {
        Some(arg) => format!("/{name} {arg}"),
        None => format!("/{name}"),
    };
    execute(&command, app)
}

fn result_text(result: &CommandResult) -> &str {
    result.message.as_deref().unwrap_or_default()
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    }
}

#[test]
fn rename_usage_empty_active_and_oversized_boundaries_are_preserved() {
    let mut harness = ControlHarness::new();

    for arg in [None, Some("   ")] {
        let result = dispatch(&mut harness.app, "rename", arg);
        assert!(result.is_error);
        assert_eq!(result_text(&result), "Error: Usage: /rename <new title>");
    }

    let no_session = dispatch(&mut harness.app, "rename", Some("task-7"));
    assert!(no_session.is_error);
    assert!(result_text(&no_session).contains("No active session"));

    harness.seed_session();
    let too_long = "a".repeat(MAX_TITLE_LEN + 1);
    let result = dispatch(&mut harness.app, "rename", Some(&too_long));
    assert!(result.is_error);
    assert!(result_text(&result).contains("Title too long (max 100 characters)"));
}

#[test]
fn rename_persists_all_live_metadata_through_public_dispatch() {
    let mut harness = ControlHarness::new();
    let stale_prompt = SystemPrompt::Text("stale prompt".to_string());
    let session = create_saved_session_with_mode(
        &[],
        "deepseek-v4-pro",
        harness.temp.path(),
        0,
        Some(&stale_prompt),
        None,
    );
    let session_id = session.metadata.id.clone();
    harness
        .manager
        .save_session(&session)
        .expect("save session");
    harness.app.current_session_id = Some(session_id.clone());
    harness
        .app
        .set_model_selection("local-code-model".to_string());
    harness
        .app
        .set_provider_identity(ProviderKind::Custom, "lm-studio");
    harness.app.mode = AppMode::Operate;
    harness.app.system_prompt = None;
    harness.app.todos.try_lock().expect("todos lock").add(
        "live rename state".to_string(),
        crate::tools::todo::TodoStatus::InProgress,
    );
    let expected_work_state = harness.app.work_state_snapshot().expect("work snapshot");

    let result = dispatch(&mut harness.app, "rename", Some("Brand New Title"));

    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        result_text(&result),
        "Session renamed to \"Brand New Title\""
    );
    let reloaded = harness.manager.load_session(&session_id).expect("reload");
    assert_eq!(reloaded.metadata.title, "Brand New Title");
    assert_eq!(reloaded.work_state, expected_work_state);
    assert!(reloaded.system_prompt.is_none());
    assert_eq!(reloaded.metadata.model, "local-code-model");
    assert_eq!(reloaded.metadata.model_provider, "custom");
    assert_eq!(
        reloaded.metadata.model_provider_id.as_deref(),
        Some("lm-studio")
    );
    assert_eq!(reloaded.metadata.workspace, harness.app.workspace);
    assert_eq!(reloaded.metadata.mode.as_deref(), Some("operate"));
    assert_eq!(
        harness.app.session_title.as_deref(),
        Some("Brand New Title")
    );
    assert_eq!(
        harness
            .app
            .current_session_metadata
            .as_ref()
            .map(|metadata| metadata.title.as_str()),
        Some("Brand New Title")
    );
}

#[test]
fn rename_sanitizes_controls_and_accepts_exact_character_limit() {
    let mut harness = ControlHarness::new();
    let session_id = harness.seed_session();

    let hostile = "Ev\u{1b}]0;PWNED\u{7}il\u{202e} Beta";
    let result = dispatch(&mut harness.app, "rename", Some(hostile));
    assert!(!result.is_error, "{result:?}");
    let reloaded = harness.manager.load_session(&session_id).expect("reload");
    assert_eq!(reloaded.metadata.title, "Ev]0;PWNEDil Beta");
    assert_eq!(
        harness.app.session_title.as_deref(),
        Some("Ev]0;PWNEDil Beta")
    );

    let controls_only = dispatch(&mut harness.app, "rename", Some("\u{1b}\u{7}\u{200b}"));
    assert!(controls_only.is_error);

    let max_title = "中".repeat(MAX_TITLE_LEN);
    let result = dispatch(&mut harness.app, "rename", Some(&max_title));
    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        harness
            .manager
            .load_session(&session_id)
            .expect("reload")
            .metadata
            .title,
        max_title
    );
}

#[test]
fn rename_recovers_first_snapshot_from_checkpoint() {
    let mut harness = ControlHarness::new();
    let checkpoint =
        create_saved_session_with_mode(&[], "deepseek-v4-pro", harness.temp.path(), 0, None, None);
    let session_id = checkpoint.metadata.id.clone();
    harness
        .manager
        .save_checkpoint(&checkpoint)
        .expect("save checkpoint");
    harness.app.current_session_id = Some(session_id.clone());
    harness.app.api_messages =
        std::sync::Arc::new(vec![user_message("first turn still streaming")]);

    let result = dispatch(&mut harness.app, "rename", Some("Midturn Rename"));

    assert!(!result.is_error, "{result:?}");
    assert_eq!(harness.app.session_title.as_deref(), Some("Midturn Rename"));
    let persisted = harness
        .manager
        .load_session(&session_id)
        .expect("persisted");
    assert_eq!(persisted.metadata.title, "Midturn Rename");
    assert_eq!(persisted.messages.len(), 1);
}

#[test]
fn rename_builds_from_app_state_before_any_checkpoint_exists() {
    let mut harness = ControlHarness::new();
    let session_id = "live-before-first-checkpoint";
    harness.app.current_session_id = Some(session_id.to_string());
    harness.app.api_messages =
        std::sync::Arc::new(vec![user_message("turn one, nothing persisted yet")]);

    let result = dispatch(&mut harness.app, "rename", Some("Earliest Rename"));

    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        harness.app.session_title.as_deref(),
        Some("Earliest Rename")
    );
    let persisted = harness.manager.load_session(session_id).expect("persisted");
    assert_eq!(persisted.metadata.title, "Earliest Rename");
    assert_eq!(persisted.messages.len(), 1);
}

#[test]
fn title_requires_an_active_session_and_preserves_raw_length_limit() {
    let mut harness = ControlHarness::new();
    let no_session = dispatch(&mut harness.app, "title", Some("task-7"));
    assert!(no_session.is_error);
    assert!(result_text(&no_session).contains("No active session"));

    harness.app.current_session_id = Some("any".to_string());
    let too_long = "x".repeat(MAX_TITLE_LEN + 1);
    let result = dispatch(&mut harness.app, "title", Some(&too_long));
    assert!(result.is_error);
    assert!(result_text(&result).contains("Title too long"));
}

#[test]
fn title_set_and_clear_persist_without_renaming_the_session() {
    let mut harness = ControlHarness::new();
    let mut session =
        create_saved_session_with_mode(&[], "deepseek-v4-pro", harness.temp.path(), 0, None, None);
    session.metadata.id = "title-test".to_string();
    session.metadata.title = "Original Name".to_string();
    harness
        .manager
        .save_session(&session)
        .expect("save session");
    harness.app.current_session_id = Some("title-test".to_string());

    let result = dispatch(&mut harness.app, "title", Some("parallel-task"));
    assert!(!result.is_error, "{result:?}");
    assert_eq!(harness.app.window_title.as_deref(), Some("parallel-task"));
    assert!(harness.app.session_title.is_none());
    let reloaded = harness.manager.load_session("title-test").expect("reload");
    assert_eq!(reloaded.window_title.as_deref(), Some("parallel-task"));
    assert_eq!(reloaded.metadata.title, "Original Name");

    let result = dispatch(&mut harness.app, "title", Some("off"));
    assert!(!result.is_error, "{result:?}");
    assert!(harness.app.window_title.is_none());
    assert!(
        harness
            .manager
            .load_session("title-test")
            .expect("reload")
            .window_title
            .is_none()
    );
}

#[test]
fn title_bare_reports_config_and_session_sources() {
    let mut harness = ControlHarness::new();
    harness.app.title_default = Some("workspace-x".to_string());
    let result = dispatch(&mut harness.app, "title", None);
    assert_eq!(
        result_text(&result),
        "Window title: [workspace-x] (config default)"
    );

    harness.app.window_title = Some("session-specific".to_string());
    let result = dispatch(&mut harness.app, "title", None);
    assert_eq!(
        result_text(&result),
        "Window title: [session-specific] (session)"
    );
}

#[test]
fn title_sanitizes_terminal_controls_before_persisting() {
    let mut harness = ControlHarness::new();
    let session_id = harness.seed_session();

    let hostile = "Ev\u{1b}]0;PWNED\u{7}il\u{202e} Beta";
    let result = dispatch(&mut harness.app, "title", Some(hostile));
    assert!(!result.is_error, "{result:?}");
    let reloaded = harness.manager.load_session(&session_id).expect("reload");
    assert_eq!(reloaded.window_title.as_deref(), Some("Ev]0;PWNEDil Beta"));
    assert_eq!(
        harness.app.window_title.as_deref(),
        Some("Ev]0;PWNEDil Beta")
    );

    let controls_only = dispatch(&mut harness.app, "title", Some("\u{1b}\u{7}\u{200b}"));
    assert!(controls_only.is_error);
}

#[test]
fn title_recovers_first_snapshot_from_checkpoint() {
    let mut harness = ControlHarness::new();
    let checkpoint =
        create_saved_session_with_mode(&[], "deepseek-v4-pro", harness.temp.path(), 0, None, None);
    let session_id = checkpoint.metadata.id.clone();
    harness
        .manager
        .save_checkpoint(&checkpoint)
        .expect("save checkpoint");
    harness.app.current_session_id = Some(session_id.clone());
    harness.app.api_messages =
        std::sync::Arc::new(vec![user_message("first turn still streaming")]);

    let result = dispatch(&mut harness.app, "title", Some("Midturn Title"));

    assert!(!result.is_error, "{result:?}");
    assert_eq!(harness.app.window_title.as_deref(), Some("Midturn Title"));
    let persisted = harness
        .manager
        .load_session(&session_id)
        .expect("persisted");
    assert_eq!(persisted.window_title.as_deref(), Some("Midturn Title"));
    assert_eq!(persisted.messages.len(), 1);
    assert_ne!(persisted.metadata.title, "Midturn Title");
}

#[test]
fn title_builds_from_app_state_before_any_checkpoint_exists() {
    let mut harness = ControlHarness::new();
    let session_id = "live-title-before-first-checkpoint";
    harness.app.current_session_id = Some(session_id.to_string());
    harness.app.api_messages =
        std::sync::Arc::new(vec![user_message("turn one, nothing persisted yet")]);

    let result = dispatch(&mut harness.app, "title", Some("Earliest Title"));

    assert!(!result.is_error, "{result:?}");
    assert_eq!(harness.app.window_title.as_deref(), Some("Earliest Title"));
    let persisted = harness.manager.load_session(session_id).expect("persisted");
    assert_eq!(persisted.window_title.as_deref(), Some("Earliest Title"));
    assert_eq!(persisted.messages.len(), 1);
}

fn init_repo(workspace: &Path, origin: &str, branch: &str) {
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .arg(workspace)
        .status()
        .expect("git init");
    assert!(init.success());
    let origin_status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "add", "origin", origin])
        .status()
        .expect("git remote add");
    if !origin_status.success() {
        let update = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["remote", "set-url", "origin", origin])
            .status()
            .expect("git remote set-url");
        assert!(update.success());
    }
    let branch_status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["symbolic-ref", "HEAD"])
        .arg(format!("refs/heads/{branch}"))
        .status()
        .expect("set branch");
    assert!(branch_status.success());
}

fn external_url(result: &CommandResult) -> &str {
    match result.action.as_ref() {
        Some(AppAction::OpenExternalUrl { url, .. }) => url,
        other => panic!("expected external URL action, got {other:?}"),
    }
}

#[test]
fn remote_env_bare_command_preserves_every_source_boundary() {
    let mut harness = ControlHarness::new();
    let result = dispatch(&mut harness.app, "remote-env", None);
    assert!(!result.is_error);
    assert!(result.action.is_none());
    for boundary in ["unpushed", "dirty", "ignored", "secrets", "session state"] {
        assert!(
            result_text(&result).contains(boundary),
            "missing boundary: {boundary}"
        );
    }
}

#[test]
fn remote_env_open_encodes_branch_and_never_echoes_credentials() {
    let mut harness = ControlHarness::new();
    let secret = "top-secret-token";
    init_repo(
        harness.temp.path(),
        &format!("https://hunter:{secret}@github.com/codewhale-hq/CodeWhale.git"),
        "feature/mobile&cloud-{url}",
    );

    let result = dispatch(&mut harness.app, "remote-env", Some("open"));

    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        external_url(&result),
        "https://app.codewhale.net/work?repo=codewhale-hq%2FCodeWhale&branch=feature%2Fmobile%26cloud-%7Burl%7D"
    );
    assert!(result_text(&result).contains("feature/mobile&cloud-{url}"));
    assert!(!external_url(&result).contains(secret));
    assert!(!result_text(&result).contains(secret));
}

#[test]
fn remote_env_supported_https_ssh_and_cnb_origins_remain_accepted() {
    for (origin, expected) in [
        (
            "https://github.com/codewhale-hq/CodeWhale.git",
            "codewhale-hq%2FCodeWhale",
        ),
        (
            "https://user:token@github.com/codewhale-hq/CodeWhale",
            "codewhale-hq%2FCodeWhale",
        ),
        (
            "ssh://git@github.com/codewhale-hq/CodeWhale.git",
            "codewhale-hq%2FCodeWhale",
        ),
        (
            "git@github.com:codewhale-hq/CodeWhale.git",
            "codewhale-hq%2FCodeWhale",
        ),
        ("https://cnb.cool/whale/codewhale.git", "whale%2Fcodewhale"),
        (
            "ssh://git@cnb.cool:2222/whale/codewhale.git",
            "whale%2Fcodewhale",
        ),
        ("git@cnb.cool:whale/codewhale.git", "whale%2Fcodewhale"),
    ] {
        let mut harness = ControlHarness::new();
        init_repo(harness.temp.path(), origin, "main");
        let result = dispatch(&mut harness.app, "remote-env", Some("open"));
        assert!(!result.is_error, "{origin}: {result:?}");
        assert!(
            external_url(&result).contains(&format!("repo={expected}")),
            "{origin}: {:?}",
            result.action
        );
    }
}

#[test]
fn remote_env_rejects_unsupported_origins_without_echoing_them() {
    for origin in [
        "http://github.com/acme/widgets.git",
        "git://github.com/acme/widgets.git",
        "https://github.com/acme/widgets/extra.git",
        "https://github.com/acme/widgets.git?token=secret",
        "file:///tmp/widgets.git",
        "/tmp/widgets.git",
    ] {
        let mut harness = ControlHarness::new();
        init_repo(harness.temp.path(), origin, "main");
        let result = dispatch(&mut harness.app, "remote-env", Some("open"));
        assert!(result.is_error, "{origin}");
        assert!(result.action.is_none(), "{origin}");
        assert!(!result_text(&result).contains(origin), "{origin}");
    }

    let mut harness = ControlHarness::new();
    let secret = "do-not-echo-this-token";
    init_repo(
        harness.temp.path(),
        &format!("https://user:{secret}@gitlab.com/acme/widgets.git"),
        "main",
    );
    let result = dispatch(&mut harness.app, "remote-env", Some("open"));
    assert!(result.is_error);
    assert!(result.action.is_none());
    assert!(!result_text(&result).contains(secret));
}

#[test]
fn remote_env_invalid_operations_remain_read_only_rejections() {
    let mut harness = ControlHarness::new();
    for operation in ["upload", "migrate", "sync", "surprise"] {
        let result = dispatch(&mut harness.app, "remote-env", Some(operation));
        assert!(result.is_error, "{operation}");
        assert!(result.action.is_none(), "{operation}");
        assert!(result_text(&result).contains("does not upload, migrate, or sync"));
        assert!(result_text(&result).contains("/remote-env open"));
    }
}

#[test]
fn remote_env_open_requires_a_symbolic_branch() {
    let mut harness = ControlHarness::new();
    init_repo(
        harness.temp.path(),
        "git@github.com:acme/widgets.git",
        "main",
    );
    for args in [
        ["config", "user.name", "Codewhale Test"],
        ["config", "user.email", "test@codewhale.invalid"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(harness.temp.path())
            .args(args)
            .status()
            .expect("git config");
        assert!(status.success());
    }
    let commit = Command::new("git")
        .arg("-C")
        .arg(harness.temp.path())
        .args(["commit", "--allow-empty", "--quiet", "-m", "fixture"])
        .status()
        .expect("create fixture commit");
    assert!(commit.success());
    let detach = Command::new("git")
        .arg("-C")
        .arg(harness.temp.path())
        .args(["checkout", "--detach", "--quiet", "HEAD"])
        .status()
        .expect("detach HEAD");
    assert!(detach.success());

    let result = dispatch(&mut harness.app, "remote-env", Some("open"));
    assert!(result.is_error);
    assert!(result.action.is_none());
}

#[test]
fn remote_env_localized_copy_preserves_composed_placeholders() {
    let cases: &[(MessageId, &[&str])] = &[
        (MessageId::CmdRemoteEnvOverview, &["{command}"]),
        (
            MessageId::CmdRemoteEnvOpening,
            &["{repo}", "{branch}", "{origin}", "{url}"],
        ),
        (
            MessageId::CmdRemoteEnvUnavailable,
            &["{origin}", "{command}"],
        ),
        (MessageId::CmdRemoteEnvSourceCustodyPolicy, &["{command}"]),
    ];
    for locale in Locale::shipped_complete() {
        for (id, placeholders) in cases {
            let message = tr(*locale, *id);
            for placeholder in *placeholders {
                assert!(
                    message.contains(placeholder),
                    "{} {id:?} lost {placeholder}",
                    locale.tag()
                );
            }
        }
    }
}
