use super::*;
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::test_support::{EnvVarGuard, lock_test_env};
use crate::whale_soul::{CreateWhaleInput, create_whale};

#[tokio::test]
async fn whale_id_thread_applies_whale_defaults_and_injects_soul_and_memory_into_prompt()
-> Result<()> {
    let _env_lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let cw_home = root.path().join("cw-home");
    let _cw_guard = EnvVarGuard::set("CODEWHALE_HOME", &cw_home);
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;

    crate::whale_soul::save("new", "DEFAULT_SOUL_SENTINEL")?;

    let specialist = create_whale(CreateWhaleInput {
        id: Some("release-captain".into()),
        name: "Release Captain".into(),
        purpose: Some("Verify release checklists".into()),
        color: Some("amber".into()),
        soul: Some("SPECIALIST_SOUL_SENTINEL: verify changelog and tags.".into()),
        memory: Some("SPECIALIST_MEMORY_SENTINEL: tag format is v0.10.x.".into()),
        model: Some("deepseek-reasoner".into()),
        model_provider_id: None,
        external_agent_id: None,
        permission_posture: Some("auto-review".into()),
        child_posture: Some("ask".into()),
        home_thread_id: None,
    })
    .expect("create release-captain whale");
    assert_eq!(specialist.id, "release-captain");

    let mut cfg = Config::default().with_legacy_root(
        Some("whale-runtime-test-key".to_string()),
        Some("http://127.0.0.1:1/v1".to_string()),
    );
    cfg.set_feature("mcp", false)?;
    cfg.set_feature("subagents", false)?;
    let plugins = Arc::new(codewhale_plugins::PluginRegistry::empty(&workspace));
    let manager = Arc::new(RuntimeThreadManager::open_owner_host(
        cfg,
        workspace.clone(),
        test_manager_config(root.path().join("rt")),
        plugins,
        None,
        None,
    )?);

    let write_args = serde_json::json!({
        "path": "reports/release.md",
        "content": "# Release Readiness\n\nAll checks passed cleanly."
    })
    .to_string();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call_write_report", "write_file", &write_args),
        canned::simple_text_turn("Release report written."),
    ]));
    manager.set_test_model_client(mock.clone());

    let unknown = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            whale_id: Some("release-captian".into()),
            ..Default::default()
        })
        .await;
    assert!(
        unknown
            .as_ref()
            .is_err_and(|err| err.to_string().contains("does not exist")),
        "a whale_id that names no whale must be refused: {unknown:?}"
    );

    let thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            whale_id: Some("release-captain".into()),
            ..Default::default()
        })
        .await?;
    assert_eq!(thread.whale_id.as_deref(), Some("release-captain"));
    assert_eq!(thread.model, "deepseek-reasoner");
    assert_eq!(thread.permission_posture.as_deref(), Some("auto_review"));

    let child_thread = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            parent_thread_id: Some(thread.id.clone()),
            whale_id: Some("release-captain".into()),
            ..Default::default()
        })
        .await?;
    assert_eq!(child_thread.permission_posture.as_deref(), Some("ask"));

    let overridden = manager
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            whale_id: Some("release-captain".into()),
            permission_posture: Some("accept_edits".into()),
            ..Default::default()
        })
        .await?;
    assert_eq!(
        overridden.permission_posture.as_deref(),
        Some("accept_edits")
    );

    let turn = manager
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "Write the release readiness report.".into(),
                auto_approve: Some(true),
                ..Default::default()
            },
        )
        .await?;
    let settled = wait_for_terminal_turn(manager.as_ref(), &turn.id).await?;
    assert_eq!(
        settled.status,
        RuntimeTurnStatus::Completed,
        "turn failed: {:?}",
        settled.error
    );

    let requests = mock.captured_requests();
    assert!(!requests.is_empty(), "expected captured model requests");
    let request_text = serde_json::to_string(&requests[0])?;
    assert!(
        request_text.contains("SPECIALIST_SOUL_SENTINEL"),
        "expected specialist SOUL.md in model request: {request_text}"
    );
    assert!(
        request_text.contains("SPECIALIST_MEMORY_SENTINEL"),
        "expected specialist MEMORY.md in model request: {request_text}"
    );
    assert!(
        !request_text.contains("DEFAULT_SOUL_SENTINEL"),
        "specialist whale thread must not load default SOUL.md: {request_text}"
    );

    let detail = manager.get_thread_detail(&thread.id).await?;
    let card_item = detail
        .items
        .iter()
        .find(|item| {
            item.metadata
                .as_ref()
                .and_then(|meta| meta.get("whale_card"))
                .is_some()
        })
        .expect("expected write_file item with metadata.whale_card");
    let whale_card = &card_item.metadata.as_ref().unwrap()["whale_card"];
    assert_eq!(whale_card["kind"], "markdown_artifact");
    assert_eq!(whale_card["path"], "reports/release.md");
    assert_eq!(whale_card["title"], "Release Readiness");
    assert!(whale_card["wordCount"].as_u64().unwrap() >= 5);

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
    }
    Ok(())
}
