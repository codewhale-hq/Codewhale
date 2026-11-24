use super::*;
use crate::conversation_modes::{CREATOR, CreatorAvailabilityGuard, ModeRefusal, STANDARD};
use crate::core::engine::Engine;
use crate::llm_client::mock::MockLlmClient;

fn refusal_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<ModeRefusal>()
        .expect("typed refusal")
        .code
}

fn create_request(preset: Option<&str>) -> CreateThreadRequest {
    CreateThreadRequest {
        agent_preset: preset.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn thread_record_without_agent_preset_loads_as_standard() -> Result<()> {
    let thread = sample_thread("thr_modes_legacy");
    let mut raw = serde_json::to_value(&thread)?;
    assert!(raw.get("agent_preset").is_none());
    let legacy: ThreadRecord = serde_json::from_value(raw.clone())?;
    assert_eq!(legacy.agent_preset, None);
    assert_eq!(
        crate::conversation_modes::effective_id(legacy.agent_preset.as_deref()),
        STANDARD
    );
    raw["agent_preset"] = json!(CREATOR);
    let creator: ThreadRecord = serde_json::from_value(raw)?;
    assert_eq!(creator.agent_preset.as_deref(), Some(CREATOR));
    Ok(())
}

#[tokio::test]
async fn mode_is_set_at_creation_and_refused_when_unknown_or_unavailable() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let manager = test_manager(tmp.path().to_path_buf())?;
    {
        let _available = CreatorAvailabilityGuard::set(None);
        let created = manager.create_thread(create_request(Some(CREATOR))).await?;
        assert_eq!(created.agent_preset.as_deref(), Some(CREATOR));
        let standard = manager
            .create_thread(create_request(Some(STANDARD)))
            .await?;
        assert_eq!(standard.agent_preset, None);
        let unknown = manager
            .create_thread(create_request(Some("minimal")))
            .await
            .expect_err("unknown id");
        assert_eq!(refusal_code(&unknown), ModeRefusal::UNKNOWN_MODE);
    }
    let _missing = CreatorAvailabilityGuard::set(Some("no runtime"));
    let unavailable = manager
        .create_thread(create_request(Some(CREATOR)))
        .await
        .expect_err("unavailable");
    assert_eq!(refusal_code(&unavailable), ModeRefusal::MODE_UNAVAILABLE);
    assert_eq!(unavailable.to_string(), "no runtime");
    let table =
        crate::conversation_modes::current_table(manager.extension_host_options(), true).await;
    assert!(table.modes[0].available && table.modes[0].reason.is_none());
    assert!(!table.modes[1].available);
    assert_eq!(table.modes[1].reason.as_deref(), Some("no runtime"));
    Ok(())
}

#[tokio::test]
async fn mode_changes_only_before_the_first_turn_and_never_with_an_executor() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let manager = test_manager(tmp.path().to_path_buf())?;
    let _available = CreatorAvailabilityGuard::set(None);
    let thread = manager.create_thread(create_request(None)).await?;
    let patch = |preset: &str| UpdateThreadRequest {
        agent_preset: Some(preset.to_string()),
        ..Default::default()
    };

    let updated = manager.update_thread(&thread.id, patch(CREATOR)).await?;
    assert_eq!(updated.agent_preset.as_deref(), Some(CREATOR));
    let unknown = manager
        .update_thread(&thread.id, patch("nope"))
        .await
        .expect_err("unknown id");
    assert_eq!(refusal_code(&unknown), ModeRefusal::UNKNOWN_MODE);

    let mut with_turn = manager.store.load_thread(&thread.id)?;
    with_turn.latest_turn_id = Some("turn_first".to_string());
    manager.store.save_thread(&with_turn)?;
    let locked = manager
        .update_thread(&thread.id, patch(STANDARD))
        .await
        .expect_err("locked");
    assert_eq!(refusal_code(&locked), ModeRefusal::MODE_LOCKED);
    manager.update_thread(&thread.id, patch(CREATOR)).await?;

    let with_executor = CreateThreadRequest {
        agent_preset: Some(CREATOR.to_string()),
        executor: Some(CreateThreadExecutor {
            kind: "acp".to_string(),
            agent_id: "opencode".to_string(),
            config: CreateThreadExecutorConfig::default(),
        }),
        ..Default::default()
    };
    let refused = manager
        .create_thread(with_executor)
        .await
        .expect_err("executor");
    assert_eq!(
        refusal_code(&refused),
        ModeRefusal::MODE_NOT_SUPPORTED_WITH_EXECUTOR
    );
    Ok(())
}

#[tokio::test]
async fn creator_is_available_when_key_is_on_and_a_runtime_is_found() -> Result<()> {
    let _real = CreatorAvailabilityGuard::unforced();
    crate::conversation_modes::clear_probe_cache_for_tests();
    let Some(_node) = crate::extension_host::test_support::node_for_tests(
        "creator_is_available_when_key_is_on_and_a_runtime_is_found",
    ) else {
        return Ok(());
    };
    let tmp = tempfile::tempdir()?;
    let manager = test_manager(tmp.path().to_path_buf())?;
    let table =
        crate::conversation_modes::current_table(manager.extension_host_options(), true).await;
    assert!(table.modes[0].available);
    assert!(table.modes[1].available, "{:?}", table.modes[1]);
    assert!(table.modes[1].reason.is_none(), "{:?}", table.modes[1]);
    let created = manager.create_thread(create_request(Some(CREATOR))).await?;
    assert_eq!(created.agent_preset.as_deref(), Some(CREATOR));
    Ok(())
}

#[tokio::test]
async fn a_runtime_thread_sync_keeps_its_conversation_mode() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let manager = test_manager(tmp.path().to_path_buf())?;
    let _available = CreatorAvailabilityGuard::set(None);
    let thread = manager.create_thread(create_request(Some(CREATOR))).await?;
    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            model: thread.model.clone(),
            subagents_enabled: false,
            snapshots_enabled: false,
            memory_enabled: false,
            terminal_chrome_enabled: false,
            ..Default::default()
        },
        &Config::default(),
        mock,
    );
    manager
        .install_test_engine(&thread.id, handle.clone())
        .await?;
    let run = tokio::spawn(engine.run());
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    }];
    let session = crate::session_manager::create_saved_session(
        &messages,
        &thread.model,
        &thread.workspace,
        0,
        None,
    );
    manager
        .synchronize_history_operation(&thread.id, &session)
        .await?;
    let observed = handle.get_session_snapshot().await?;
    assert_eq!(observed.agent_preset.as_deref(), Some(CREATOR));
    manager
        .synchronize_history_operation(&thread.id, &session)
        .await?;
    assert_eq!(
        handle.get_session_snapshot().await?.agent_preset.as_deref(),
        Some(CREATOR)
    );
    handle.send(Op::Shutdown).await?;
    tokio::time::timeout(Duration::from_secs(10), run).await??;
    Ok(())
}

#[tokio::test]
async fn reload_config_and_ensure_engine_loaded_evict_idle_engines_on_plugin_creator_toggle()
-> Result<()> {
    let _available = CreatorAvailabilityGuard::set(None);
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let mut initial = Config::default().with_legacy_root(
        Some("test-creator-reload-key".to_string()),
        Some("http://127.0.0.1:1/v1".to_string()),
    );
    initial.set_feature("mcp", false)?;
    initial.set_feature("subagents", false)?;
    let manager = RuntimeThreadManager::open(
        initial.clone(),
        workspace,
        test_manager_config(tmp.path().join("runtime")),
    )?;
    let thread = manager.create_thread(create_request(Some(CREATOR))).await?;

    let first = manager.get_engine(&thread.id).await?;
    assert!(manager.loaded_engine(&thread.id).await.is_some());

    let mut disabled = initial.clone();
    disabled.set_feature("plugin_creator", false)?;
    manager.reload_config(disabled.clone()).await?;
    assert!(manager.loaded_engine(&thread.id).await.is_none());
    tokio::time::timeout(Duration::from_secs(10), first.tx_op.closed()).await?;

    manager.reload_config(initial).await?;
    let second = manager.get_engine(&thread.id).await?;
    assert!(!first.tx_op.same_channel(&second.tx_op));

    {
        let mut active = manager.active.lock().await;
        let state = active
            .engines
            .get_mut(&thread.id)
            .expect("warmed active engine");
        state.active_turn = Some(ActiveTurnState {
            narrowing: crate::core::engine::TurnNarrowing::Inherit,
            run_origin: crate::core::authority::auto_review::RunOrigin::Background,
            owner_present: false,
            goal_progress: None,
            goal_id: None,
            turn_id: "turn_busy_reload".to_string(),
            interrupt_requested: false,
            compaction_id: None,
        });
    }
    manager.reload_config(disabled).await?;
    assert!(manager.loaded_engine(&thread.id).await.is_some());

    {
        let mut active = manager.active.lock().await;
        active
            .engines
            .get_mut(&thread.id)
            .expect("busy engine kept across reload")
            .active_turn = None;
    }
    let third = manager.get_engine(&thread.id).await?;
    assert!(!second.tx_op.same_channel(&third.tx_op));
    tokio::time::timeout(Duration::from_secs(10), second.tx_op.closed()).await?;
    let disabled_cached = {
        let active = manager.active.lock().await;
        !active
            .engines
            .get(&thread.id)
            .expect("reloaded engine")
            .plugin_creator_enabled
    };
    assert!(disabled_cached);

    let handles = manager
        .active
        .lock()
        .await
        .engines
        .drain()
        .map(|(_, state)| state.engine)
        .collect::<Vec<_>>();
    for handle in handles {
        let _ = handle.send(Op::Shutdown).await;
        let _ = tokio::time::timeout(Duration::from_secs(10), handle.tx_op.closed()).await;
    }
    Ok(())
}
