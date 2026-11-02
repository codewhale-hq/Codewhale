//! Hermetic #5305 route-receipt regressions kept out of the oversized parent
//! sub-agent test module.

use super::*;

fn consultant_runtime(
    workspace: &std::path::Path,
    manager: SharedSubAgentManager,
) -> SubAgentRuntime {
    let providers = codewhale_runtime::config::ProvidersConfig {
        deepseek: codewhale_runtime::config::ProviderConfig {
            api_key: Some("deepseek-test-key".to_string()),
            base_url: Some("http://127.0.0.1:9/v1".to_string()),
            ..Default::default()
        },
        openai_codex: codewhale_runtime::config::ProviderConfig {
            api_key: Some("codex-test-key".to_string()),
            // A custom endpoint lets a pinned codex client construct from the
            // table key alone (see the codex_credentials fallback in
            // CodewhaleClient::new) instead of requiring machine-local OAuth
            // consent. The endpoint is never contacted: these fixtures cancel
            // their children before a model step, and 127.0.0.1:9 refuses
            // instantly if one ever races.
            base_url: Some("http://127.0.0.1:9/v1".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let config = {
        let __cw_config_init_0_0 = Some("deepseek".to_string());
        let __cw_config_init_0_1 = Some(providers);
        let mut __cw_config_init_0_value: codewhale_runtime::config::Config = Default::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    }
    .with_legacy_root(Some("deepseek-test-key".to_string()), None);
    let client = CodewhaleClient::new(&config).expect("DeepSeek parent client");
    SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".to_string(),
        ToolContext::new(workspace.to_path_buf()),
        false,
        None,
        manager,
    )
    .with_api_config(config)
}

async fn start_consultant(
    workspace: &std::path::Path,
) -> (
    SharedSubAgentManager,
    ToolContext,
    crate::tools::spec::ToolResult,
) {
    let manager = new_shared_subagent_manager(workspace.to_path_buf(), 4);
    let context = ToolContext::new(workspace.to_path_buf());
    let tool = AgentTool::new(
        manager.clone(),
        consultant_runtime(workspace, manager.clone()),
    );
    let result = tool
        .execute(
            json!({
                "action": "start",
                "type": "consultant",
                "prompt": "inspect the request without writing files",
            }),
            &context,
        )
        .await
        .expect("role-only consultant starts");
    (manager, context, result)
}

fn receipt_from(result: &crate::tools::spec::ToolResult) -> serde_json::Value {
    let content: serde_json::Value =
        serde_json::from_str(&result.content).expect("start result is JSON");
    let metadata = result.metadata.as_ref().expect("ToolResult metadata");
    assert_eq!(content["child_route"], metadata["child_route"]);
    content["child_route"].clone()
}

async fn cancel_started(manager: &SharedSubAgentManager, result: &crate::tools::spec::ToolResult) {
    let agent_id = result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("agent_id"))
        .and_then(serde_json::Value::as_str)
        .expect("start agent id")
        .to_string();
    manager
        .write()
        .await
        .cancel_agent(&agent_id)
        .expect("cancel test child");
}

#[tokio::test]
async fn issue_5305_role_only_receipt_precedes_status_poll() {
    // Role-only dispatch: no saved member is read, so the receipt records the
    // resolved role and the inherited session route — never a profile.
    let workspace = tempfile::tempdir().expect("workspace tempdir");

    let (manager, _context, start) = start_consultant(workspace.path()).await;
    assert!(
        start.content.len() < 1024,
        "receipt must remain compact: {} bytes",
        start.content.len()
    );
    let receipt = receipt_from(&start);
    assert_eq!(receipt["requested_type"], json!("advisor"));
    assert_eq!(receipt["requested_profile"], serde_json::Value::Null);
    assert_eq!(receipt["resolved_profile_id"], serde_json::Value::Null);
    assert_eq!(receipt["profile_origin"], serde_json::Value::Null);
    assert_eq!(receipt["canonical_role"], json!("advisor"));
    assert_eq!(receipt["provider_id"], json!("deepseek"));
    assert_eq!(receipt["model_id"], json!("deepseek-v4-flash"));
    assert_eq!(receipt["route_source"], json!("run.model"));
    assert_eq!(receipt["requested_reasoning"], json!("inherit"));
    // The advisor role's default tier ("high") applies: roles carry a
    // reasoning default even though they carry no profile.
    assert_eq!(receipt["effective_reasoning"], json!("high"));
    assert!(receipt["runtime_version"].as_str().is_some());
    assert!(receipt["runtime_build_sha"].as_str().is_some());
    cancel_started(&manager, &start).await;
}

#[tokio::test]
async fn issue_5305_receipt_survives_status_peek() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let (manager, context, start) = start_consultant(workspace.path()).await;
    let receipt = receipt_from(&start);
    let agent_id = start.metadata.as_ref().unwrap()["agent_id"]
        .as_str()
        .expect("agent id")
        .to_string();

    let inspect = AgentTool::new(
        manager.clone(),
        consultant_runtime(workspace.path(), manager.clone()),
    );
    let status = inspect
        .execute(json!({"action": "status", "agent_id": agent_id}), &context)
        .await
        .expect("status");
    let status_json: serde_json::Value =
        serde_json::from_str(&status.content).expect("status json");
    assert_eq!(status_json["child_route"], receipt);
    assert_eq!(status.metadata.as_ref().unwrap()["child_route"], receipt);

    let peek = inspect
        .execute(json!({"action": "peek", "agent_id": agent_id}), &context)
        .await
        .expect("peek");
    let peek_json: serde_json::Value = serde_json::from_str(&peek.content).expect("peek json");
    assert_eq!(peek_json["child_route"], receipt);
    assert_eq!(peek.metadata.as_ref().unwrap()["child_route"], receipt);
    assert!(status.content.len() <= lifecycle::COMPACT_STATUS_BYTES);
    assert!(peek.content.len() <= lifecycle::COMPACT_STATUS_BYTES);
    cancel_started(&manager, &start).await;
}

#[test]
fn compact_receipt_bounds_long_labels_and_declared_outputs_without_hiding_limits() {
    let metadata = spawn_route_metadata("deepseek", &"🐋\\\"".repeat(4000), "run.model");
    let paths = (0..32)
        .map(|index| format!("reports/{index}-{}.md", "長".repeat(4000)))
        .collect::<Vec<_>>();
    let mut receipt = json!({
        "agent_id": "agent_bounded_receipt", "run_id": "agent_bounded_receipt",
        "name": "🐋".repeat(4000), "status": "starting", "terminal": false,
        "context_mode": "fresh", "child_route": metadata.child_route,
        "follow_up": {"tool": "agent", "agent_id": "agent_bounded_receipt", "session_name": "🐋".repeat(4000)},
        "usage": {"status": "unknown", "note": "No provider receipt yet"},
        "worker_record": {"spec": {
            "runtime_profile": {"spawn_depth": 1, "max_spawn_depth": 2, "max_steps": 8, "wall_time_secs": 30, "wall_deadline_ms": 10000},
            "launch_manifest": {"deliverables": paths}
        }}
    });
    compact_spawn_receipt(&mut receipt, false);
    assert!(serde_json::to_vec(&receipt).unwrap().len() <= lifecycle::COMPACT_SPAWN_BYTES);
    assert_eq!(receipt["effective_limits"]["max_steps"], 8);
    assert_eq!(receipt["effective_limits"]["wall_deadline_ms"], 10000);
    assert_eq!(receipt["child_route"]["truncated"], true);
    let shown = receipt["deliverables"].as_array().unwrap();
    assert_eq!(
        shown.len() + receipt["deliverables_omitted"].as_u64().unwrap() as usize,
        paths.len()
    );
    for (path, original) in shown.iter().zip(&paths) {
        assert_eq!(
            path, original,
            "declared paths must remain exact when shown"
        );
    }
}

#[tokio::test]
async fn issue_5305_explicit_profile_matches_type_resolution_and_conflicts_refuse() {
    // "consultant" is the advisor legacy alias, so an explicit profile takes
    // the same route as the type — while a conflicting type still refuses.
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let (manager, context, type_start) = start_consultant(workspace.path()).await;
    let type_receipt = receipt_from(&type_start);
    cancel_started(&manager, &type_start).await;

    let explicit_tool = AgentTool::new(
        manager.clone(),
        consultant_runtime(workspace.path(), manager.clone()),
    );
    let explicit = explicit_tool
        .execute(
            json!({"action":"start", "profile":"consultant", "prompt":"same route"}),
            &context,
        )
        .await
        .expect("explicit profile starts");
    let explicit_receipt = receipt_from(&explicit);
    for field in [
        "resolved_profile_id",
        "profile_origin",
        "canonical_role",
        "provider_id",
        "model_id",
        "route_source",
        "requested_reasoning",
        "effective_reasoning",
    ] {
        assert_eq!(explicit_receipt[field], type_receipt[field], "{field}");
    }
    assert_eq!(explicit_receipt["requested_profile"], json!("consultant"));
    cancel_started(&manager, &explicit).await;

    let conflict = explicit_tool
        .execute(
            json!({"action":"start", "type":"scout", "profile":"consultant", "prompt":"must refuse"}),
            &context,
        )
        .await
        .expect_err("conflicting type/profile is refused");
    assert!(conflict.to_string().contains("conflicting explicit type"));
}

#[tokio::test]
async fn issue_5305_unbuildable_route_refuses_before_worktree_admission() {
    // An explicit model the session provider cannot serve fails model
    // resolution before any worktree is admitted.
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let manager = new_shared_subagent_manager(workspace.path().to_path_buf(), 1);
    let context = ToolContext::new(workspace.path().to_path_buf());
    let tool = AgentTool::new(
        manager.clone(),
        consultant_runtime(workspace.path(), manager.clone()),
    );
    let worktree = workspace.path().join("must-not-exist");
    let err = tool
        .execute(
            json!({
                "action":"start", "type":"consultant", "prompt":"refuse before admission",
                "model": "not-a-deepseek-model",
                "worktree": true, "cwd": worktree,
            }),
            &context,
        )
        .await
        .expect_err("unbuildable model refuses before admission");
    assert!(err.to_string().contains("model"), "{err}");
    assert!(manager.read().await.list_filtered(true).is_empty());
    assert!(!worktree.exists());
}

#[tokio::test]
async fn issue_6320_untethered_runtime_binds_exact_route() {
    // #6320 decision: binding to the already-exact route is acceptable. The
    // runtime keeps its fully-constructed client; `api_config = None` only
    // matters when a cross-protocol rebuild is needed, and that path still
    // fails closed (see untethered_cross_protocol_rebound_fails_closed_without_config).
    // Untethered means "no Config to rebuild from", not "no client at all".
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let manager = new_shared_subagent_manager(workspace.path().to_path_buf(), 1);
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(workspace.path().to_path_buf());
    runtime.manager = manager.clone();
    runtime.api_config = None;
    let context = runtime.context.clone();
    let start = AgentTool::new(manager.clone(), runtime)
        .execute(
            json!({"action":"start", "type":"consultant", "prompt":"untethered spawn"}),
            &context,
        )
        .await
        .expect("untethered runtime binds its exact route");
    let receipt = receipt_from(&start);
    assert_eq!(receipt["model_id"], json!("deepseek-v4-flash"));
    assert!(!manager.read().await.list_filtered(true).is_empty());
    cancel_started(&manager, &start).await;
}

#[test]
fn issue_5305_builtin_inheritance_and_redaction_are_bounded() {
    let request =
        parse_spawn_request(&json!({"prompt":"x", "type":"consultant"})).expect("request");
    let mut runtime = stub_runtime();
    runtime.model = "deepseek-v4-flash".to_string();
    let requested_route = RequestedChildRoute {
        requested_type: "consultant".to_string(),
        requested_profile: None,
        requested_reasoning: "inherit".to_string(),
    };
    let receipt = mint_child_route_receipt(
        &requested_route,
        &request,
        None,
        &runtime,
        "deepseek-v4-flash".to_string(),
        "run.model",
        None,
    )
    .expect("bounded receipt");
    let encoded = serde_json::to_string(&receipt).expect("receipt json");
    assert!(encoded.len() <= CHILD_ROUTE_RECEIPT_MAX_BYTES);
    assert_eq!(receipt.resolved_profile_id, None);
    assert_eq!(receipt.profile_origin, None);
    assert_eq!(receipt.canonical_role, "advisor");
    assert_eq!(receipt.route_source, "run.model");
    for forbidden in ["test-key", "127.0.0.1", "codewhale-test-stub", "/"] {
        assert!(
            !encoded.contains(forbidden),
            "receipt leaked {forbidden}: {encoded}"
        );
    }
    // #5529 mode 2: the fallback note rides the receipt inside the same
    // byte ceiling.
    let fallback = mint_child_route_receipt(
        &requested_route,
        &request,
        None,
        &runtime,
        "deepseek-v4-flash".to_string(),
        "session.fallback",
        Some("pinned provider 'xai' unavailable (no credentials); fell back to the session route"),
    )
    .expect("bounded receipt");
    assert_eq!(
        fallback.fallback_note.as_deref(),
        Some("pinned provider 'xai' unavailable (no credentials); fell back to the session route")
    );
    let fallback_encoded = serde_json::to_string(&fallback).expect("receipt json");
    assert!(fallback_encoded.len() <= CHILD_ROUTE_RECEIPT_MAX_BYTES);
}

#[tokio::test]
async fn issue_5305_receipt_survives_ledger_interruption_completion_and_resume() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let manager = new_shared_subagent_manager(workspace.path().to_path_buf(), 4);
    let receipt = ChildRouteReceipt {
        requested_type: "consultant".to_string(),
        requested_profile: None,
        resolved_profile_id: Some("consultant".to_string()),
        profile_origin: Some("personal".to_string()),
        canonical_role: "advisor".to_string(),
        provider_id: "openai-codex".to_string(),
        model_id: "gpt-5.6-sol".to_string(),
        route_source: "agent_profile.model".to_string(),
        fallback_note: None,
        requested_reasoning: "inherit".to_string(),
        effective_reasoning: Some("high".to_string()),
        runtime_version: "test".to_string(),
        runtime_build_sha: "test-build".to_string(),
    };
    let agent_id = {
        let mut guard = manager.write().await;
        let (agent_id, _) = guard.insert_test_interrupted_continuable_agent(
            "receipt-child",
            workspace.path(),
            vec![text_message("assistant", "checkpointed work")],
        );
        // This is a direct root child. The generic seed's invented parent
        // actor is absent; keep the production session and descendant guards.
        let worker = guard
            .worker_records
            .get_mut(&agent_id)
            .expect("worker record");
        worker.parent_run_id = None;
        worker.spec.parent_run_id = None;
        worker.spec.child_route = Some(receipt.clone());
        let ledger = guard
            .coordination_summary_for(&agent_id, 4)
            .expect("ledger projection");
        assert_eq!(ledger.child_route, Some(receipt.clone()));
        agent_id
    };

    let context = ToolContext::new(workspace.path().to_path_buf());
    let interrupt = AgentsInterruptTool::new(manager.clone())
        .execute(
            json!({"agent_id": agent_id, "reason": "pause for review"}),
            &context,
        )
        .await
        .expect("already interrupted child projects its receipt");
    let interrupt_json: serde_json::Value =
        serde_json::from_str(&interrupt.content).expect("interrupt json");
    assert_eq!(interrupt_json["child_route"], json!(receipt));

    let interrupted = manager
        .read()
        .await
        .get_result(&agent_id)
        .expect("interrupted snapshot");
    assert_eq!(interrupted.child_route.as_ref(), Some(&receipt));
    let completion = subagent_completion_from_result(&interrupted);
    assert!(completion.payload.contains("gpt-5.6-sol"));

    // #6046: resume rebinds the receipt's provider pin, so the runtime must
    // carry a config the pinned openai-codex client can be built from
    // hermetically (api-key table, no machine-local OAuth consent), exactly
    // like the fresh-spawn fixtures in this file.
    let runtime = consultant_runtime(workspace.path(), manager.clone());
    let resumed = {
        let mut guard = manager.write().await;
        guard
            .resume_from_checkpoint(manager.clone(), runtime, &agent_id, "continue")
            .expect("resume preserves receipt")
    };
    assert_ne!(resumed.agent_id, agent_id);
    assert_eq!(resumed.child_route.as_ref(), Some(&receipt));
    manager
        .write()
        .await
        .cancel_agent(&resumed.agent_id)
        .expect("cancel resumed test child");
}
