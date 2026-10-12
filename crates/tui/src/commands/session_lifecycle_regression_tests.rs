//! Regression coverage retained from the pre-FEAT-023 lifecycle implementation.
//!
//! These tests dispatch through the public command seam so moving host logic
//! behind `CommandSessionLifecycleContext` cannot silently reduce the existing
//! persistence, reset, and deferred-load guarantees.

use std::time::Instant;

use tempfile::TempDir;

use crate::commands::CommandResult;
use crate::config::Config;
use crate::reasoning_preference::ReasoningEffort;
use crate::session_manager::create_saved_session_with_id_and_mode;
use crate::test_support::SealedHome;
use crate::tui::app::{App, AppAction, TuiOptions, TurnCacheRecord};
use crate::tui::history::HistoryCell;
use codewhale_config::AppMode;
use codewhale_models::Role;

fn dispatch_lifecycle(app: &mut App, name: &str, arg: Option<&str>) -> CommandResult {
    let command = match arg {
        Some(arg) => format!("/{name} {arg}"),
        None => format!("/{name}"),
    };
    crate::commands::execute(&command, app)
}

fn save(app: &mut App, path: Option<&str>) -> CommandResult {
    dispatch_lifecycle(app, "save", path)
}

fn fork(app: &mut App) -> CommandResult {
    dispatch_lifecycle(app, "fork", None)
}

fn new_session(app: &mut App, arg: Option<&str>) -> CommandResult {
    dispatch_lifecycle(app, "new", arg)
}

fn load(app: &mut App, path: Option<&str>) -> CommandResult {
    dispatch_lifecycle(app, "load", path)
}

fn compact(app: &mut App, arg: Option<&str>) -> CommandResult {
    dispatch_lifecycle(app, "compact", arg)
}

fn sessions(app: &mut App, arg: Option<&str>) -> CommandResult {
    dispatch_lifecycle(app, "sessions", arg)
}

fn create_test_app_with_tmpdir(tmpdir: &TempDir) -> App {
    let options = TuiOptions {
        skills_dir: tmpdir.path().join("skills"),
        memory_path: tmpdir.path().join("memory.md"),
        notes_path: tmpdir.path().join("notes.txt"),
        mcp_config_path: tmpdir.path().join("mcp.json"),
        ..crate::test_support::test_tui_options(tmpdir.path())
    };
    App::new(options, &Config::default())
}

#[test]
fn test_save_creates_file_and_sets_session_id() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let save_path = tmpdir.path().join("test_session.json");

    let result = save(&mut app, Some(save_path.to_str().unwrap()));
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Session saved to"));
    assert!(msg.contains("ID:"));
    assert!(app.current_session_id.is_some());
    assert!(save_path.exists());
}

#[test]
fn save_preserves_artifact_registry() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let save_path = tmpdir.path().join("artifact_session.json");
    app.session_artifacts
        .push(crate::artifacts::ArtifactRecord {
            id: "art_call_big".to_string(),
            kind: crate::artifacts::ArtifactKind::ToolOutput,
            session_id: "artifact-session".to_string(),
            tool_call_id: "call-big".to_string(),
            tool_name: "exec_shell".to_string(),
            created_at: chrono::Utc::now(),
            byte_size: 512_000,
            preview: "cargo test output".to_string(),
            storage_path: tmpdir.path().join("call-big.txt"),
        });

    let result = save(&mut app, Some(save_path.to_str().unwrap()));

    assert!(!result.is_error);
    let saved: crate::session_manager::SavedSession =
        serde_json::from_str(&std::fs::read_to_string(save_path).unwrap()).unwrap();
    assert_eq!(saved.artifacts, app.session_artifacts);
}

#[test]
fn save_preserves_latest_auto_route_receipt() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let save_path = tmpdir.path().join("auto_route_session.json");
    let receipt = crate::model_routing::AutoRouteReceipt {
        tier: crate::model_routing::AutoRouteTier::Fast,
        pair: crate::model_routing::AutoRoutePair {
            strong: crate::config::ZAI_GLM_5_2_MODEL.to_string(),
            fast: Some(crate::config::ZAI_GLM_5_TURBO_MODEL.to_string()),
        },
        scope: crate::model_routing::AutoRouteScope::ResolvedProvider,
        data_path: crate::model_routing::AutoRouteDataPath::LocalHeuristic,
        reason: crate::model_routing::AutoRouteReason::LocalFallback(
            crate::model_routing::AutoRouteHeuristicReason::DeclaredDefault,
        ),
        decision: None,
        router_failure: None,
    };
    app.set_model_selection("auto".to_string());
    app.last_effective_provider = Some(crate::config::ProviderKind::Zai);
    app.last_effective_provider_identity = Some("zai".to_string());
    app.last_effective_model = Some(crate::config::ZAI_GLM_5_TURBO_MODEL.to_string());
    app.last_auto_route_receipt = Some(receipt.clone());
    app.last_effective_reasoning_effort =
        Some(crate::reasoning_preference::EffectiveReasoningEffort::ThinkingEnabledGranularityUnavailable);

    let result = save(&mut app, Some(save_path.to_str().unwrap()));

    assert!(!result.is_error);
    let saved: crate::session_manager::SavedSession =
        serde_json::from_str(&std::fs::read_to_string(save_path).unwrap()).unwrap();
    let route = saved.last_auto_route.expect("latest Auto route");
    assert_eq!(route.provider, crate::config::ProviderKind::Zai);
    assert_eq!(route.provider_identity, "zai");
    assert_eq!(route.model, crate::config::ZAI_GLM_5_TURBO_MODEL);
    assert_eq!(route.receipt, receipt);
    assert_eq!(
        route.effective_reasoning_effort,
        Some(crate::work_graph::ReasoningEffortTier::ThinkingEnabledGranularityUnavailable)
    );
}

#[test]
fn fork_saves_parent_and_switches_to_child_session() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.set_provider_identity(crate::config::ProviderKind::Custom, "lm-studio");
    app.current_session_id = Some("parent-session".to_string());
    let mut cached_parent = create_saved_session_with_id_and_mode(
        "parent-session".to_string(),
        &[],
        &app.model,
        &app.workspace,
        0,
        None,
        Some(app.mode.label()),
    )
    .metadata;
    cached_parent.title = "Custom Parent".to_string();
    cached_parent.created_at = "2026-01-02T03:04:05Z"
        .parse()
        .expect("fixed parent timestamp");
    app.current_session_metadata = Some(cached_parent.clone());
    app.session_title = Some(cached_parent.title.clone());
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "try another path".to_string(),
            cache_control: None,
        }],
    });
    {
        let mut todos = app.todos.try_lock().expect("todos lock");
        todos.add(
            "preserve fork Work".to_string(),
            crate::tools::todo::TodoStatus::InProgress,
        );
    }
    {
        let mut plan = app.plan_state.try_lock().expect("plan lock");
        plan.update(crate::tools::plan::UpdatePlanArgs {
            objective: Some("Fork without Work drift".to_string()),
            ..crate::tools::plan::UpdatePlanArgs::default()
        });
    }
    app.cycle_effort();
    let expected_work = app
        .work_state_snapshot()
        .expect("Work snapshot")
        .expect("graph-backed Work state");
    assert!(
        expected_work.graph.is_some(),
        "fork fixture must use a graph"
    );

    let result = fork(&mut app);

    assert!(!result.is_error, "{:?}", result.message);
    let new_id = app.current_session_id.clone().expect("fork session id");
    assert_ne!(new_id, "parent-session");
    assert!(result.message.as_deref().unwrap_or("").contains("Forked"));
    assert!(matches!(result.action, Some(AppAction::SyncSession { .. })));

    let manager = crate::session_manager::SessionManager::default_location().unwrap();
    let parent = manager
        .load_session("parent-session")
        .expect("parent saved");
    let child = manager.load_session(&new_id).expect("child saved");
    assert_eq!(parent.messages.len(), 1);
    assert_eq!(parent.metadata.model_provider, "custom");
    assert_eq!(
        parent.metadata.model_provider_id.as_deref(),
        Some("lm-studio")
    );
    assert_eq!(parent.metadata.title, cached_parent.title);
    assert_eq!(parent.metadata.created_at, cached_parent.created_at);
    assert_eq!(
        child.metadata.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(child.metadata.forked_from_message_count, Some(1));
    assert_eq!(child.metadata.model_provider, "custom");
    assert_eq!(
        child.metadata.model_provider_id.as_deref(),
        Some("lm-studio")
    );
    assert_eq!(parent.work_state.as_ref(), Some(&expected_work));
    assert_eq!(child.work_state.as_ref(), Some(&expected_work));
    let cached_child = app
        .current_session_metadata
        .as_ref()
        .expect("child metadata cached");
    assert_eq!(cached_child.id, child.metadata.id);
    assert_eq!(cached_child.title, child.metadata.title);
    assert_eq!(cached_child.created_at, child.metadata.created_at);
    assert_eq!(
        cached_child.parent_session_id,
        child.metadata.parent_session_id
    );
    assert_eq!(
        app.session_title.as_deref(),
        Some(child.metadata.title.as_str())
    );
}

#[test]
fn fork_rejects_active_runtime_without_switching_sessions() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("parent-session".to_string());
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "still running".to_string(),
            cache_control: None,
        }],
    });
    app.is_loading = true;

    let result = fork(&mut app);

    assert!(result.is_error);
    assert!(result.action.is_none());
    assert_eq!(app.current_session_id.as_deref(), Some("parent-session"));
    assert_eq!(app.api_messages.len(), 1);
}

#[test]
fn new_session_from_resumed_state_creates_distinct_empty_session() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("old-session".to_string());
    app.session_title = Some("Old Session".to_string());
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "continue this thread".to_string(),
            cache_control: None,
        }],
    });
    app.add_message(HistoryCell::System {
        content: "old transcript".to_string(),
    });
    app.system_prompt = Some(codewhale_models::SystemPrompt::Text(
        "old prompt".to_string(),
    ));
    app.session.total_tokens = 123;
    app.session.session_cost = 1.25;

    let result = new_session(&mut app, None);

    assert!(!result.is_error, "{:?}", result.message);
    let new_id = app.current_session_id.clone().expect("new session id");
    assert_ne!(new_id, "old-session");
    assert_eq!(app.session_title.as_deref(), Some("New Session"));
    assert!(app.api_messages.is_empty());
    assert!(app.history.is_empty());
    assert!(app.system_prompt.is_none());
    assert_eq!(app.session.total_tokens, 0);
    assert_eq!(app.session.session_cost, 0.0);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("/resume")
    );
    match result.action {
        Some(AppAction::SyncSession {
            session_id,
            messages,
            system_prompt,
            ..
        }) => {
            assert_eq!(session_id.as_deref(), Some(new_id.as_str()));
            assert!(messages.is_empty());
            assert!(system_prompt.is_none());
        }
        other => panic!("expected SyncSession action, got {other:?}"),
    }
}

#[test]
fn new_session_forgets_denials_and_session_grants() {
    conversation_reset_forgets_approvals("new");
}

#[test]
fn clear_session_forgets_denials_and_session_grants() {
    conversation_reset_forgets_approvals("clear");
}

fn conversation_reset_forgets_approvals(command: &str) {
    // UX-8: a Deny used to outlive `/new` for the whole process ("Restart
    // Codewhale to reconsider it"); a fresh conversation starts clean.
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("old-session".to_string());
    app.approval_session_denied
        .insert("shell:rm -rf build:call-1".to_string());
    app.approval_session_approved
        .insert("shell:git status".to_string());
    // A child agent's pending approval card from the old conversation.
    let child_id = "agent:agent_a:approval:boot:1";
    crate::tui::ui::push_approval_request_view(
        &mut app,
        child_id,
        "exec_shell",
        "agent_a wants to run 'exec_shell'",
        &serde_json::json!({"command": "cargo build"}),
        "k",
        "g",
        None,
        crate::config::ApprovalDefaultSelection::Deny,
        None,
    );
    crate::tui::pending_requests::record(
        &mut app,
        child_id,
        crate::tui::pending_requests::PendingChildRequest {
            agent_id: "agent_a".to_string(),
            tool_name: "exec_shell".to_string(),
            description: String::new(),
            input: serde_json::json!({}),
            approval_key: "k".to_string(),
            approval_grouping_key: "g".to_string(),
            intent_summary: None,
            requested_at: std::time::Instant::now(),
        },
    );

    let result = dispatch_lifecycle(&mut app, command, None);

    assert!(matches!(result.action, Some(AppAction::SyncSession { .. })));
    assert!(
        app.pending_child_requests.is_empty() && app.view_stack.is_empty(),
        "an old conversation's child approval card must not follow /{command}"
    );
    assert!(
        app.approval_session_denied.is_empty(),
        "a denied call must prompt again after /{command}"
    );
    assert!(
        app.approval_session_approved.is_empty(),
        "an approve-for-session grant must not follow the user into /{command}"
    );
}

#[test]
fn new_session_blocks_unsent_input_without_force() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("old-session".to_string());
    app.input = "draft text".to_string();

    let result = new_session(&mut app, None);

    assert!(result.is_error);
    assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
    assert_eq!(app.input, "draft text");
    assert!(result.action.is_none());
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("/new --force")
    );
}

#[test]
fn new_session_force_discards_unsent_input() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("old-session".to_string());
    app.input = "draft text".to_string();

    let result = new_session(&mut app, Some("--force"));

    assert!(!result.is_error, "{:?}", result.message);
    assert_ne!(app.current_session_id.as_deref(), Some("old-session"));
    assert!(app.input.is_empty());
    assert!(matches!(result.action, Some(AppAction::SyncSession { .. })));
}

#[test]
fn new_session_blocks_in_flight_turn_without_force() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("old-session".to_string());
    app.is_loading = true;

    let result = new_session(&mut app, None);

    assert!(result.is_error);
    assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
    assert!(result.action.is_none());
}

#[test]
fn new_session_force_cannot_detach_an_in_flight_turn() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("old-session".to_string());
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![],
    });
    app.is_loading = true;
    app.runtime_turn_status = Some("in_progress".to_string());

    let result = new_session(&mut app, Some("--force"));

    assert!(result.is_error);
    assert!(result.action.is_none());
    assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
    assert_eq!(app.api_messages.len(), 1);
    let message = result.message.as_deref().unwrap_or_default();
    assert!(
        message.contains("Cannot start a new session while runtime work is active"),
        "{message}"
    );
    assert!(
        message.contains("the session is still busy with the current turn"),
        "the refusal names the live turn: {message}"
    );
}

#[test]
fn load_rejects_an_active_runtime_before_reading_or_mutating() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.current_session_id = Some("old-session".to_string());
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![],
    });
    app.task_panel.push(crate::tui::app::TaskPanelEntry {
        exit_code: None,
        id: "queued-late-producer".to_string(),
        status: "queued".to_string(),
        prompt_summary: "queued".to_string(),
        duration_ms: None,
        kind: crate::tui::app::TaskPanelEntryKind::Background,
        stale: false,
        elapsed_since_output_ms: None,
        owner_agent_id: None,
        owner_agent_name: None,
        current_tool: None,
        role: None,
        files_touched: 0,
    });

    let result = load(&mut app, Some("does-not-exist.json"));

    assert!(result.is_error);
    assert!(result.action.is_none());
    assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
    assert_eq!(app.api_messages.len(), 1);
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|message| message.contains("runtime work is active"))
    );
}

#[test]
fn test_save_with_default_path_uses_managed_sessions_dir() {
    let tmpdir = TempDir::new().unwrap();
    // The seal points the managed sessions directory inside its own temp dir
    // rather than the real user home.
    let home = SealedHome::new();
    let sessions_dir = home.codewhale_home().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let result = save(&mut app, None);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    // Give it a moment to ensure file is written
    std::thread::sleep(std::time::Duration::from_millis(10));
    let entries: Vec<_> = if sessions_dir.exists() {
        std::fs::read_dir(&sessions_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .collect()
    } else {
        Vec::new()
    };
    // Session should be saved to the managed dir, not the workspace root.
    assert!(
        !entries.is_empty(),
        "expected session file in {sessions_dir:?}, got none; msg: {msg}"
    );
    let session_id = app
        .current_session_id
        .as_deref()
        .expect("current session id");
    assert!(sessions_dir.join(format!("{session_id}.json")).exists());
}

#[test]
fn test_save_serialization_error() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    // This should work normally since SavedSession is serializable
    // Testing error path would require mocking, which is complex
    let save_path = tmpdir.path().join("test.json");
    let result = save(&mut app, Some(save_path.to_str().unwrap()));
    assert!(result.message.is_some());
}

#[test]
fn test_load_without_path_returns_error() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let result = load(&mut app, None);
    assert!(result.message.is_some());
    assert!(result.message.unwrap().contains("Usage: /load"));
}

#[test]
fn test_load_nonexistent_file_returns_error() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let result = load(&mut app, Some("nonexistent.json"));
    assert!(result.message.is_some());
    assert!(result.message.unwrap().contains("Failed to read"));
}

#[test]
fn test_load_invalid_json_returns_error() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let bad_file = tmpdir.path().join("bad.json");
    std::fs::write(&bad_file, "not valid json").unwrap();
    let result = load(&mut app, Some(bad_file.to_str().unwrap()));
    assert!(result.message.is_some());
    assert!(result.message.unwrap().contains("Failed to parse"));
}

#[test]
fn test_load_valid_session_defers_state_restore_to_event_loop() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app1 = create_test_app_with_tmpdir(&tmpdir);
    // Set up some state to save
    app1.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "Hello".to_string(),
            cache_control: None,
        }],
    });
    app1.session.total_tokens = 500;
    app1.set_mode(AppMode::Plan);
    let save_path = tmpdir.path().join("test.json");
    save(&mut app1, Some(save_path.to_str().unwrap()));

    // Create new app and load
    let mut app2 = create_test_app_with_tmpdir(&tmpdir);
    app2.system_prompt = Some(codewhale_models::SystemPrompt::Text(
        "stale prompt from prior session".to_string(),
    ));
    app2.session_context_references
        .push(crate::session_manager::SessionContextReference {
            message_index: 0,
            reference: codewhale_core::ContextReference {
                kind: codewhale_core::ContextReferenceKind::File,
                source: codewhale_core::ContextReferenceSource::AtMention,
                badge: "file".to_string(),
                label: "stale.rs".to_string(),
                target: tmpdir.path().join("stale.rs").display().to_string(),
                included: true,
                expanded: true,
                detail: None,
            },
        });
    let result = load(&mut app2, Some(save_path.to_str().unwrap()));
    assert_eq!(result.message, None);
    assert!(app2.api_messages.is_empty());
    assert_eq!(app2.session.total_tokens, 0);
    assert!(app2.current_session_id.is_none());
    assert!(app2.system_prompt.is_some());
    assert_eq!(app2.session_context_references.len(), 1);
    assert!(matches!(
        result.action,
        Some(AppAction::LoadSession(path)) if path == save_path
    ));
}

#[test]
fn explicit_save_persists_work_state_and_load_defers_application() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
    {
        let mut todos = saved_app.todos.try_lock().expect("todos lock");
        todos.add(
            "persist me".to_string(),
            crate::tools::todo::TodoStatus::InProgress,
        );
    }
    {
        let mut plan = saved_app.plan_state.try_lock().expect("plan lock");
        plan.update(crate::tools::plan::UpdatePlanArgs {
            objective: Some("Resume exactly".to_string()),
            ..crate::tools::plan::UpdatePlanArgs::default()
        });
    }
    let expected = saved_app.work_state_snapshot().expect("snapshot");
    let save_path = tmpdir.path().join("work_state.json");
    let saved = save(&mut saved_app, Some(save_path.to_str().unwrap()));
    assert!(!saved.is_error, "{:?}", saved.message);

    let mut loaded_app = create_test_app_with_tmpdir(&tmpdir);
    let loaded = load(&mut loaded_app, Some(save_path.to_str().unwrap()));
    assert!(!loaded.is_error, "{:?}", loaded.message);
    assert_eq!(loaded_app.work_state_snapshot().expect("snapshot"), None);
    assert!(matches!(
        loaded.action,
        Some(AppAction::LoadSession(path)) if path == save_path
    ));
    let saved_session: crate::session_manager::SavedSession =
        serde_json::from_str(&std::fs::read_to_string(&save_path).expect("saved session file"))
            .expect("saved session JSON");
    assert_eq!(saved_session.work_state, expected);
}

#[test]
fn new_session_is_all_or_nothing_when_work_state_is_busy() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![],
    });
    app.current_session_id = Some("current-session".to_string());
    let todos = app.todos.clone();
    let _held = todos.try_lock().expect("hold todos lock");

    let result = new_session(&mut app, Some("--force"));

    assert!(result.is_error);
    assert_eq!(app.api_messages.len(), 1);
    assert_eq!(app.current_session_id.as_deref(), Some("current-session"));
    assert!(result.action.is_none());
}

#[test]
fn load_auto_model_session_defers_model_restore_to_event_loop() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
    saved_app.set_model_selection("auto".to_string());
    saved_app.last_effective_model = Some("deepseek-v4-flash".to_string());
    saved_app.last_effective_reasoning_effort = Some(
        crate::reasoning_preference::EffectiveReasoningEffort::Tier(ReasoningEffort::Low),
    );
    let save_path = tmpdir.path().join("auto_model.json");
    save(&mut saved_app, Some(save_path.to_str().unwrap()));

    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.set_model_selection("deepseek-v4-flash".to_string());
    app.reasoning_effort = ReasoningEffort::High;
    let result = load(&mut app, Some(save_path.to_str().unwrap()));

    assert!(!result.is_error);
    assert!(!app.auto_model);
    assert_eq!(app.model, "deepseek-v4-flash");
    assert_eq!(app.reasoning_effort, ReasoningEffort::High);
    assert!(matches!(
        result.action,
        Some(AppAction::LoadSession(path)) if path == save_path
    ));
}

#[test]
fn load_defers_artifact_registry_restore_to_event_loop() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
    saved_app
        .session_artifacts
        .push(crate::artifacts::ArtifactRecord {
            id: "art_call_big".to_string(),
            kind: crate::artifacts::ArtifactKind::ToolOutput,
            session_id: "artifact-session".to_string(),
            tool_call_id: "call-big".to_string(),
            tool_name: "exec_shell".to_string(),
            created_at: chrono::Utc::now(),
            byte_size: 128,
            preview: "checking crate".to_string(),
            storage_path: tmpdir.path().join("call-big.txt"),
        });
    let save_path = tmpdir.path().join("artifact_load.json");
    save(&mut saved_app, Some(save_path.to_str().unwrap()));

    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.session_artifacts
        .push(crate::artifacts::ArtifactRecord {
            id: "art_stale".to_string(),
            kind: crate::artifacts::ArtifactKind::ToolOutput,
            session_id: "stale-session".to_string(),
            tool_call_id: "stale".to_string(),
            tool_name: "exec_shell".to_string(),
            created_at: chrono::Utc::now(),
            byte_size: 1,
            preview: "stale".to_string(),
            storage_path: tmpdir.path().join("stale.txt"),
        });

    let result = load(&mut app, Some(save_path.to_str().unwrap()));

    assert!(!result.is_error);
    assert_eq!(app.session_artifacts.len(), 1);
    assert_eq!(app.session_artifacts[0].id, "art_stale");
    assert!(matches!(
        result.action,
        Some(AppAction::LoadSession(path)) if path == save_path
    ));
}

#[test]
fn load_defers_telemetry_reset_to_event_loop() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
    saved_app
        .api_messages_mut()
        .push(codewhale_models::Message {
            role: Role::User,
            content: vec![codewhale_models::ContentBlock::Text {
                text: "checkpoint".to_string(),
                cache_control: None,
            }],
        });
    saved_app.session.total_tokens = 500;
    let save_path = tmpdir.path().join("checkpoint.json");
    save(&mut saved_app, Some(save_path.to_str().unwrap()));

    let mut app = create_test_app_with_tmpdir(&tmpdir);
    app.session.session_cost = 1.25;
    app.session.session_cost_cny = 9.13;
    app.session.subagent_cost = 0.75;
    app.session.subagent_cost_cny = 5.48;
    app.session
        .subagent_usage_sources
        .insert(crate::cost_status::usage_source_fingerprint(
            "response-test",
        ));
    app.session.displayed_cost_high_water = 2.0;
    app.session.displayed_cost_high_water_cny = 14.61;
    app.session.last_prompt_tokens = Some(120);
    app.session.last_completion_tokens = Some(35);
    app.session.last_prompt_cache_hit_tokens = Some(80);
    app.session.last_prompt_cache_miss_tokens = Some(40);
    app.session.last_reasoning_replay_tokens = Some(12);
    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 120,
        output_tokens: 35,
        cache_hit_tokens: Some(80),
        cache_miss_tokens: Some(40),
        reasoning_replay_tokens: Some(12),
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: Instant::now(),
    });

    let result = load(&mut app, Some(save_path.to_str().unwrap()));

    assert_eq!(result.message, None);
    assert_eq!(app.session.total_tokens, 0);
    assert_eq!(app.session.session_cost, 1.25);
    assert_eq!(app.session.session_cost_cny, 9.13);
    assert_eq!(app.session.subagent_cost, 0.75);
    assert_eq!(app.session.subagent_cost_cny, 5.48);
    assert_eq!(app.session.turn_cache_history.len(), 1);
    assert!(matches!(
        result.action,
        Some(AppAction::LoadSession(path)) if path == save_path
    ));
}

#[test]
fn test_compact_toggles_state() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);

    let result = compact(&mut app, None);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Making room"), "{msg}");
    assert!(matches!(
        result.action,
        Some(AppAction::CompactContext { focus: None })
    ));
}

#[test]
fn compact_command_forwards_a_trimmed_focus_argument() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);

    let result = compact(&mut app, Some("  the auth refactor  "));
    assert!(matches!(
        result.action,
        Some(AppAction::CompactContext { focus: Some(ref focus) }) if focus == "the auth refactor"
    ));
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|msg| msg.contains("focus: the auth refactor")),
        "{result:?}"
    );

    // Whitespace-only arguments behave like no focus at all.
    let blank = compact(&mut app, Some("   "));
    assert!(matches!(
        blank.action,
        Some(AppAction::CompactContext { focus: None })
    ));
}

#[test]
fn test_sessions_pushes_picker_view() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let initial_kind = app.view_stack.top_kind();

    let result = sessions(&mut app, None);
    assert_eq!(result.message, None);
    assert!(result.action.is_none());
    // View should have changed (session picker should be on top)
    assert_ne!(app.view_stack.top_kind(), initial_kind);
}

#[test]
fn test_sessions_show_subcommand_pushes_picker_view() {
    // `/sessions show` and `/sessions list` are explicit aliases
    // for the no-arg picker form. Verify they don't fall through
    // to the prune branch.
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let initial_kind = app.view_stack.top_kind();
    let result = sessions(&mut app, Some("show"));
    assert_eq!(result.message, None);
    assert_ne!(app.view_stack.top_kind(), initial_kind);
}

#[test]
fn test_sessions_prune_requires_days_argument() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let result = sessions(&mut app, Some("prune"));
    assert!(result.is_error);
    assert!(
        result.message.as_deref().unwrap_or("").contains("usage"),
        "expected usage hint: {:?}",
        result.message
    );
}

#[test]
fn test_sessions_prune_rejects_non_positive_days() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    for bad in ["0", "-3", "abc", "3.14"] {
        let result = sessions(&mut app, Some(&format!("prune {bad}")));
        assert!(result.is_error, "expected error for `{bad}`");
    }
}

#[test]
fn test_sessions_unknown_subcommand_errors() {
    let tmpdir = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&tmpdir);
    let result = sessions(&mut app, Some("teleport"));
    assert!(result.is_error);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or("")
            .contains("unknown subcommand"),
        "expected unknown-subcommand error: {:?}",
        result.message
    );
}

#[test]
fn branch_snapshot_roundtrip_preserves_siblings_ids_stamps_and_engine_projection() {
    let root = TempDir::new().unwrap();
    let _home = SealedHome::new();
    let mut app = create_test_app_with_tmpdir(&root);
    let message = |text: &str| codewhale_models::Message {
        role: Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
    };
    for text in ["root", "chosen", "abandoned"] {
        app.push_api_message(message(text));
    }
    assert!(!save(&mut app, None).is_error);
    let id = app.current_session_id.clone().unwrap();
    let manager = crate::session_manager::SessionManager::default_location().unwrap();
    let initial = manager.load_session(&id).unwrap();
    let original = initial.journal.as_ref().unwrap().entries.clone();
    let chosen = original[1].id.clone();
    let result = dispatch_lifecycle(&mut app, "branch", Some(&chosen));
    assert!(!result.is_error, "{:?}", result.message);
    let Some(AppAction::SyncSession {
        messages,
        session_id,
        ..
    }) = result.action
    else {
        panic!("branch must synchronize the existing engine");
    };
    assert_eq!(session_id.as_deref(), Some(id.as_str()));
    assert_eq!(messages, vec![message("root"), message("chosen")]);
    assert_eq!(app.api_messages.as_ref(), &messages);
    assert!(
        !app.history
            .iter()
            .any(|cell| format!("{cell:?}").contains("abandoned"))
    );
    app.push_api_message(message("new path"));
    for _ in 0..2 {
        let snapshot = crate::tui::ui::build_session_snapshot(&mut app, &manager).unwrap();
        manager.save_session_owned(snapshot).unwrap();
    }
    let saved = manager.load_session(&id).unwrap();
    let journal = saved.journal.as_ref().unwrap();
    assert_eq!(
        &journal.entries[..3],
        original.as_slice(),
        "all old IDs, stamps and content survive"
    );
    assert_eq!(journal.entries.len(), 4, "a second save appends nothing");
    assert_eq!(
        journal.entries[3].parent_id.as_deref(),
        Some(chosen.as_str())
    );
    assert_eq!(journal.leaves().len(), 2);
    assert_eq!(
        saved.messages,
        vec![message("root"), message("chosen"), message("new path")]
    );
    // Restoring and forking must retain the tree as well.
    app.restore_api_messages(saved.messages.clone(), &saved);
    let forked = fork(&mut app);
    assert!(!forked.is_error, "{:?}", forked.message);
    let child = manager
        .load_session(app.current_session_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(child.journal.as_ref().unwrap().entries, journal.entries);
    assert_eq!(child.metadata.spawn_depth, 1);
    let snapshot = crate::tui::ui::build_session_snapshot(&mut app, &manager).unwrap();
    assert_eq!(snapshot.journal.unwrap().entries, journal.entries);
}
