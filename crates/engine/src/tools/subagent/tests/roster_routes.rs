//! Consumer regressions for operator-visible routes (#5915/#5955).
use super::*;

#[tokio::test]
async fn roster_matches_actual_start_receipts_and_refreshes_live_role_defaults() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let (client, calls, _, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let config = {
        let __cw_config_init_0_0 = Some(codewhale_runtime::config::SubagentsConfig {
            worker_model: Some("deepseek-v4-flash".into()),
            ..Default::default()
        });
        let mut __cw_config_init_0_value: codewhale_runtime::config::Config = Default::default();
        __cw_config_init_0_value.subagents = __cw_config_init_0_0;
        __cw_config_init_0_value
    }
    .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 8);
    let context = ToolContext::new(root.path()).with_state_namespace("roster-route-consumer");
    let mut runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-pro".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    runtime
        .role_models
        .insert("general".into(), "deepseek-v4-pro".into());
    let tool = AgentTool::new(manager.clone(), runtime);
    let query = tool
        .execute(json!({"action":"roster"}), &context)
        .await
        .unwrap();
    let roster: Value = serde_json::from_str(&query.content).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "discovery must not send inference"
    );
    let rows = roster["members"].as_array().unwrap();
    assert_eq!(rows.len(), 8);
    assert_eq!(
        rows[0]["route"]["model"], "deepseek-v4-flash",
        "live config supersedes launch default"
    );
    for row in rows {
        assert!(row["route"].is_object(), "route missing: {row}");
        assert_eq!(row["route"]["reachability"], "unverified");
        let role = row["role"].as_str().unwrap();
        let mut request = json!({"action":"start", "type":role, "prompt":"Say done."});
        if role == "custom" {
            request["allowed_tools"] = json!(["Read"]);
        }
        let started = tool.execute(request, &context).await.unwrap();
        let metadata = started.metadata.as_ref().unwrap();
        let receipt = &metadata["child_route"];
        for (discovery, dispatch) in [
            ("provider", "provider_id"),
            ("model", "model_id"),
            ("reasoning_effort", "effective_reasoning"),
            ("source", "route_source"),
        ] {
            assert_eq!(
                row["route"][discovery], receipt[dispatch],
                "{role}: {discovery}"
            );
        }
        manager
            .write()
            .await
            .cancel_agent(metadata["agent_id"].as_str().unwrap())
            .unwrap();
    }
}

#[tokio::test]
async fn roster_preserves_unknown_and_non_metered_costs_and_invalid_role_errors() {
    let _env = crate::test_support::lock_test_env();
    let _live = crate::provider_lake::lock_live_snapshot();
    crate::provider_lake::clear_live_snapshot();
    for (provider, model, vendor, expected_cost, reason) in [
        ("deepseek", "deepseek-v4-flash", None, "paid", None),
        (
            "openrouter",
            "qwen/qwen3.7-plus",
            Some("cerebras"),
            "unknown",
            Some("routing_dependent_price"),
        ),
        (
            "ollama",
            "fixture-local-model",
            None,
            "not_money_metered",
            Some("not_money_metered"),
        ),
    ] {
        let root = tempdir().unwrap();
        let mut config = {
            let __cw_config_init_1_0 = Some(provider.into());
            let mut __cw_config_init_1_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value
        };
        let selected = config
            .provider_config_for_mut(
                &config.test_identity_for_kind(ProviderKind::parse(provider).unwrap()),
            )
            .unwrap();
        selected.api_key = Some("roster-private-fixture-key".into());
        selected.model = Some(model.into());
        selected.vendor = vendor.map(str::to_string);
        let client = CodewhaleClient::new(&config).unwrap();
        let manager = new_shared_subagent_manager(root.path().to_path_buf(), 1);
        let runtime = SubAgentRuntime::new(
            client,
            model.into(),
            ToolContext::new(root.path()),
            false,
            None,
            manager,
        )
        .with_api_config(config);
        let row =
            resolved_role_roster_entry(&runtime, &spawn_roster(&runtime), &FleetRole::Worker).await;
        assert_eq!(
            row["route"]["cost_class"], expected_cost,
            "{provider}: {row}"
        );
        assert_eq!(row["route"]["unpriced_reason"], json!(reason));
        assert!(!row.to_string().contains("roster-private-fixture-key"));
    }
    let mut runtime = stub_runtime();
    runtime
        .role_models
        .insert("general".into(), "invalid\nmodel".into());
    let row =
        resolved_role_roster_entry(&runtime, &spawn_roster(&runtime), &FleetRole::Worker).await;
    assert!(row["route"].is_null());
    assert!(row["route_error"].as_str().is_some());
    let other =
        resolved_role_roster_entry(&runtime, &spawn_roster(&runtime), &FleetRole::Reviewer).await;
    assert!(
        other["route"].is_object(),
        "one bad role must not hide other routes: {other}"
    );
}

#[tokio::test]
async fn advertised_task_route_overrides_reach_start_and_foreign_models_fail_before_admission() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let (client, _, _, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let config = <codewhale_runtime::config::Config as Default>::default()
        .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 2);
    let context = ToolContext::new(root.path()).with_state_namespace("explicit-task-route");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    let tool = AgentTool::new(manager.clone(), runtime);
    let schema = tool.input_schema();
    for field in ["model", "model_strength", "thinking"] {
        assert!(schema["properties"].get(field).is_some());
    }
    let started = tool.execute(json!({"action":"start", "type":"explore", "prompt":"Say done.", "model":"deepseek-v4-pro", "model_strength":"faster", "thinking":"high"}), &context).await.unwrap();
    let metadata = started.metadata.as_ref().unwrap();
    let receipt = &metadata["child_route"];
    assert_eq!(receipt["model_id"], "deepseek-v4-pro");
    assert_eq!(receipt["route_source"], "task.model");
    assert_eq!(receipt["effective_reasoning"], "high");
    manager
        .write()
        .await
        .cancel_agent(metadata["agent_id"].as_str().unwrap())
        .unwrap();
    let error = tool.execute(json!({"action":"start", "type":"explore", "prompt":"Say done.", "model":"claude-fable-5"}), &context).await.unwrap_err();
    assert!(error.to_string().contains("provider"), "{error}");
}

struct ProjectProfilesGuard(bool);
impl ProjectProfilesGuard {
    fn enabled() -> Self {
        let previous = crate::fleet::roster::project_agent_profiles_enabled();
        crate::fleet::roster::set_project_agent_profiles_enabled(true);
        Self(previous)
    }
}
impl Drop for ProjectProfilesGuard {
    fn drop(&mut self) {
        crate::fleet::roster::set_project_agent_profiles_enabled(self.0);
    }
}

#[tokio::test]
async fn saved_profile_discovery_and_actual_start_share_current_instructions_route_and_trust() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _project = ProjectProfilesGuard::enabled();
    let profile_dir = root.path().join(".codewhale/agents");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let profile = profile_dir.join("bug-hunter.toml");
    let (client, calls, bodies, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let config = {
        let __cw_config_init_3_0 = Some(codewhale_runtime::config::SubagentsConfig {
            explorer_model: Some("invalid\nrole-default".into()),
            ..Default::default()
        });
        let mut __cw_config_init_3_value: codewhale_runtime::config::Config = Default::default();
        __cw_config_init_3_value.subagents = __cw_config_init_3_0;
        __cw_config_init_3_value
    }
    .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 2);
    let context = ToolContext::new(root.path()).with_state_namespace("saved-profile-consumer");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    let tool = AgentTool::new(manager.clone(), runtime);
    for (model, instruction) in [
        ("deepseek-v4-pro", "Inspect only changed parser branches."),
        ("deepseek-v4-flash", "Inspect the new queue consumer."),
    ] {
        std::fs::write(&profile, format!("id = \"bug-hunter\"\nbase_role = \"scout\"\nmodel = \"{model}\"\nreasoning_effort = \"high\"\npersona = \"{instruction}\"\n")).unwrap();
        let before = calls.load(Ordering::SeqCst);
        let discovered = tool
            .execute(json!({"action":"roster"}), &context)
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            before,
            "roster must never infer"
        );
        let roster: Value = serde_json::from_str(&discovered.content).unwrap();
        let row = roster["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["member_id"] == "bug-hunter")
            .unwrap();
        assert_eq!(row["route"]["model"], model);
        assert_eq!(row["route"]["reasoning_effort"], "high");
        let started = tool
            .execute(
                json!({"profile":"bug-hunter", "prompt":"Inspect the assigned slice."}),
                &context,
            )
            .await
            .unwrap();
        let meta = started.metadata.as_ref().unwrap();
        let receipt = &meta["child_route"];
        assert_eq!(receipt["resolved_profile_id"], "bug-hunter");
        assert_eq!(receipt["profile_origin"], "project");
        assert_eq!(receipt["model_id"], row["route"]["model"]);
        assert_eq!(
            receipt["effective_reasoning"],
            row["route"]["reasoning_effort"]
        );
        assert_eq!(receipt["route_source"], "agent_profile.model");
        tokio::time::timeout(Duration::from_secs(5), async {
            while calls.load(Ordering::SeqCst) == before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("local provider receives the saved profile prompt");
        let body = bodies.lock().unwrap().last().unwrap().clone();
        assert!(
            body.to_string().contains(instruction),
            "saved instructions must reach the actual request"
        );
        let id = meta["agent_id"].as_str().unwrap();
        let mut guard = manager.write().await;
        let worker = guard.worker_records.get(id).unwrap();
        assert!(
            worker
                .spec
                .launch_manifest
                .as_ref()
                .unwrap()
                .prompt
                .contains(instruction)
        );
        assert!(!worker.spec.runtime_profile.permissions.write);
        if guard.agents[id].status == SubAgentStatus::Running {
            guard.cancel_agent(id).unwrap();
        }
    }
    crate::fleet::roster::set_project_agent_profiles_enabled(false);
    let error = tool
        .execute(
            json!({"profile":"bug-hunter", "prompt":"Inspect."}),
            &context,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("Unknown Fleet role/profile"),
        "{error}"
    );
}

#[tokio::test]
async fn saved_provider_pin_reaches_actual_request_and_conflicts_fail_before_admission() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _project = ProjectProfilesGuard::enabled();
    let profile_dir = root.path().join(".codewhale/agents");
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(profile_dir.join("router-review.toml"), "id = \"router-review\"\nbase_role = \"reviewer\"\nprovider = \"openrouter\"\nmodel = \"qwen/qwen3.7-plus\"\nreasoning_effort = \"low\"\n").unwrap();
    let (client, calls, bodies, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let mut config = <codewhale_runtime::config::Config as Default>::default()
        .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let router = config
        .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openrouter))
        .unwrap();
    router.api_key = Some("test-router-key".into());
    router.base_url = Some(client.base_url().into());
    router.vendor = Some("cerebras".into());
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 2);
    let context = ToolContext::new(root.path()).with_state_namespace("saved-provider-consumer");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    let tool = AgentTool::new(manager.clone(), runtime);
    for extra in [
        json!({"model":"deepseek-v4-pro"}),
        json!({"model_strength":"faster"}),
        json!({"type":"builder"}),
    ] {
        let mut input = json!({"profile":"router-review", "prompt":"Inspect."});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(tool.execute(input, &context).await.is_err());
    }
    assert!(manager.read().await.agents.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let discovered = tool
        .execute(json!({"action":"roster"}), &context)
        .await
        .unwrap();
    let roster: Value = serde_json::from_str(&discovered.content).unwrap();
    let row = roster["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["member_id"] == "router-review")
        .unwrap();
    assert_eq!(row["route"]["provider"], "openrouter");
    assert_eq!(row["route"]["openrouter_vendor"], "cerebras");
    assert_eq!(row["route"]["cost_class"], "unknown");
    let started = tool
        .execute(
            json!({"profile":"router-review", "prompt":"Say done.", "thinking":"high"}),
            &context,
        )
        .await
        .unwrap();
    let meta = started.metadata.as_ref().unwrap();
    assert_eq!(meta["child_route"]["provider_id"], "openrouter");
    assert_eq!(meta["child_route"]["model_id"], "qwen/qwen3.7-plus");
    assert_eq!(meta["child_route"]["effective_reasoning"], "high");
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("local provider fixture receives child request");
    let body = bodies.lock().unwrap()[0].clone();
    assert_eq!(body["model"], "qwen/qwen3.7-plus");
    assert_eq!(body["provider"]["order"], json!(["cerebras"]));
    assert_eq!(body["provider"]["allow_fallbacks"], false);
    assert!(
        !serde_json::to_string(meta)
            .unwrap()
            .contains("test-router-key")
    );
    let id = meta["agent_id"].as_str().unwrap();
    if manager.read().await.agents[id].status == SubAgentStatus::Running {
        manager.write().await.cancel_agent(id).unwrap();
    }
}

#[tokio::test]
async fn saved_profile_cannot_widen_parent_posture_or_depth_and_missing_provider_fails_closed() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let (client, calls, _, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let mut profile = codewhale_config::FleetProfile::default();
    profile.role.name = "builder".into();
    profile.model = Some("deepseek-v4-flash".into());
    profile.delegation.max_spawn_depth = Some(0);
    profile.permissions.allow_shell = true;
    profile.permissions.trust = true;
    let mut config = <codewhale_runtime::config::Config as Default>::default()
        .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let mut fleet = codewhale_config::FleetConfigToml::default();
    fleet
        .profiles
        .insert("bounded-builder".into(), profile.clone());
    profile.provider = Some("unconfigured-private-route".into());
    fleet.profiles.insert("missing-route".into(), profile);
    config.fleet = Some(fleet);
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 2);
    let context = ToolContext::new(root.path()).with_state_namespace("saved-profile-ceiling");
    let mut runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    runtime.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Scout);
    runtime.worker_profile.shell = ShellPolicy::None;
    let tool = AgentTool::new(manager.clone(), runtime);
    assert!(
        tool.execute(
            json!({"profile":"missing-route", "prompt":"Inspect."}),
            &context
        )
        .await
        .is_err()
    );
    assert!(manager.read().await.agents.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let started = tool
        .execute(
            json!({"profile":"bounded-builder", "prompt":"Inspect only.", "max_depth":2}),
            &context,
        )
        .await
        .unwrap();
    let id = started.metadata.as_ref().unwrap()["agent_id"]
        .as_str()
        .unwrap();
    let mut guard = manager.write().await;
    let worker = guard.worker_records.get(id).unwrap();
    assert!(!worker.spec.runtime_profile.permissions.write);
    assert_eq!(worker.spec.runtime_profile.shell, ShellPolicy::None);
    assert_eq!(worker.spec.runtime_profile.max_spawn_depth, 1);
    assert_eq!(worker.spec.runtime_profile.spawn_depth, 1);
    assert!(!worker.spec.runtime_profile.can_spawn_child());
    guard.cancel_agent(id).unwrap();
}

#[tokio::test]
async fn selected_fleet_capability_and_broken_selection_refuse_actual_start() {
    use crate::fleet::store::{FleetFile, FleetScope, save_fleet, set_selected};
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let (client, calls, _, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let config = <codewhale_runtime::config::Config as Default>::default()
        .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let mut fleet = FleetFile::new("Capability fixture".into(), None).unwrap();
    fleet.members.push(serde_json::from_value(json!({
        "id":"visual-review", "role":"reviewer", "provider":"deepseek", "model":"deepseek-v4-flash", "requires":["vision"]
    })).unwrap());
    let path = save_fleet(&fleet, FleetScope::Workspace, root.path()).unwrap();
    set_selected(&fleet.name, FleetScope::Workspace, root.path()).unwrap();
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 1);
    let context =
        ToolContext::new(root.path()).with_state_namespace("selected-capability-consumer");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    let tool = AgentTool::new(manager.clone(), runtime);
    let error = tool
        .execute(
            json!({"profile":"visual-review", "prompt":"Inspect image."}),
            &context,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("requires vision"), "{error}");
    assert!(manager.read().await.agents.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    std::fs::write(path, "this is not a Fleet document").unwrap();
    let error = tool
        .execute(json!({"type":"reviewer", "prompt":"Inspect."}), &context)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Selected"), "{error}");
    assert!(manager.read().await.agents.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn selected_models_reach_exact_provider_and_off_list_refuses_before_admission() {
    use crate::fleet::store::{FleetFile, FleetScope, save_fleet, set_selected};
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let (client, calls, bodies, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let mut config = <codewhale_runtime::config::Config as Default>::default()
        .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let router = config
        .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openrouter))
        .unwrap();
    router.api_key = Some("test-router-key".into());
    router.base_url = Some(client.base_url().into());
    router.vendor = Some("cerebras".into());
    let mut fleet = FleetFile::new("Selected routes".into(), None).unwrap();
    fleet.members.push(serde_json::from_value(json!({
        "id":"review-choice", "role":"reviewer", "provider":"openrouter", "model":"qwen/qwen3.7-plus"
    })).unwrap());
    save_fleet(&fleet, FleetScope::Workspace, root.path()).unwrap();
    set_selected(&fleet.name, FleetScope::Workspace, root.path()).unwrap();
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 2);
    let context = ToolContext::new(root.path()).with_state_namespace("shortlist-consumer");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    let tool = AgentTool::new(manager.clone(), runtime);
    let roster = tool
        .execute(json!({"action":"roster"}), &context)
        .await
        .unwrap();
    let rows: Value = serde_json::from_str(&roster.content).unwrap();
    assert_eq!(rows["model_total_count"], 1);
    assert_eq!(
        rows["models"][0]["selector"]["model"],
        "openrouter/qwen/qwen3.7-plus"
    );
    assert_eq!(rows["models"][0]["route"]["provider"], "openrouter");
    assert_eq!(rows["models"][0]["route"]["openrouter_vendor"], "cerebras");
    assert_eq!(rows["models"][0]["route"]["reachability"], "unverified");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "discovery must not infer");
    let error = tool
        .execute(
            json!({"type":"explore", "model":"deepseek-v4-pro", "prompt":"Inspect."}),
            &context,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("outside the selected Fleet"),
        "{error}"
    );
    assert!(
        error.to_string().contains("openrouter/qwen/qwen3.7-plus"),
        "{error}"
    );
    assert!(
        error.to_string().contains("deepseek/deepseek-v4-flash"),
        "{error}"
    );
    assert!(manager.read().await.agents.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let started = tool
        .execute(
            json!({"type":"explore", "model":"openrouter/qwen/qwen3.7-plus", "prompt":"Say done."}),
            &context,
        )
        .await
        .unwrap();
    let meta = started.metadata.as_ref().unwrap();
    assert_eq!(meta["child_route"]["provider_id"], "openrouter");
    assert_eq!(meta["child_route"]["model_id"], "qwen/qwen3.7-plus");
    assert!(
        meta["child_route"]["resolved_profile_id"].is_null(),
        "model choice does not invent a saved profile"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("local fixture receives selected model request");
    let body = bodies.lock().unwrap()[0].clone();
    assert_eq!(body["model"], "qwen/qwen3.7-plus");
    assert_eq!(body["provider"]["order"], json!(["cerebras"]));
    assert_eq!(body["provider"]["allow_fallbacks"], false);
    let id = meta["agent_id"].as_str().unwrap();
    if manager.read().await.agents[id].status == SubAgentStatus::Running {
        manager.write().await.cancel_agent(id).unwrap();
    }
    let session = tool
        .execute(
            json!({"type":"explore", "model":"deepseek/deepseek-v4-flash", "prompt":"Say done."}),
            &context,
        )
        .await
        .unwrap();
    assert_eq!(
        session.metadata.as_ref().unwrap()["child_route"]["provider_id"],
        "deepseek"
    );
    let id = session.metadata.as_ref().unwrap()["agent_id"]
        .as_str()
        .unwrap();
    if manager.read().await.agents[id].status == SubAgentStatus::Running {
        manager.write().await.cancel_agent(id).unwrap();
    }
    fleet.members.clear();
    save_fleet(&fleet, FleetScope::Workspace, root.path()).unwrap();
    let empty = tool
        .execute(json!({"action":"roster"}), &context)
        .await
        .unwrap();
    let rows: Value = serde_json::from_str(&empty.content).unwrap();
    assert_eq!(
        rows["model_total_count"], 0,
        "live removal must not preserve stale choices"
    );
    let error = tool
        .execute(
            json!({"type":"explore", "model":"openrouter/qwen/qwen3.7-plus", "prompt":"Inspect."}),
            &context,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("provider DeepSeek"), "{error}");
}

#[tokio::test]
async fn shortlisted_model_on_multiple_providers_requires_exact_selector() {
    use crate::fleet::store::{FleetFile, FleetScope, save_fleet, set_selected};
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let (client, calls, _, fixture_config) = delayed_chat_client(Duration::ZERO, "done").await;
    let mut fleet = FleetFile::new("Ambiguous routes".into(), None).unwrap();
    for (id, provider) in [("review-a", "openrouter"), ("review-b", "openai")] {
        fleet.members.push(
            serde_json::from_value(json!({
                "id":id, "role":"reviewer", "provider":provider, "model":"shared-wire-model"
            }))
            .unwrap(),
        );
    }
    save_fleet(&fleet, FleetScope::Workspace, root.path()).unwrap();
    set_selected(&fleet.name, FleetScope::Workspace, root.path()).unwrap();
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 1);
    let context = ToolContext::new(root.path()).with_state_namespace("ambiguous-model-consumer");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(fixture_config);
    let tool = AgentTool::new(manager.clone(), runtime);
    let error = tool
        .execute(
            json!({"type":"explore", "model":"shared-wire-model", "prompt":"Inspect."}),
            &context,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("multiple providers"), "{error}");
    assert!(manager.read().await.agents.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn write_restart_route_config(path: &std::path::Path, base_url: &str, role_pins: &str) {
    std::fs::write(
        path,
        format!(
            r#"
provider = "deepseek"
model = "deepseek-v4-flash"
api_key = "fixture-key"
base_url = "{base_url}"

[providers.ReviewerRoute]
kind = "openai-compatible"
api_key = "fixture-review-key"
base_url = "{base_url}"
model = "fixture-review-model"

[providers.OtherRoute]
kind = "openai-compatible"
api_key = "fixture-other-key"
base_url = "{base_url}"
model = "fixture-review-model"

{role_pins}
"#
        ),
    )
    .unwrap();
}

async fn assert_admitted_route(
    manager: &SharedSubAgentManager,
    started: &crate::tools::spec::ToolResult,
    expected: Value,
) -> String {
    let content: Value = serde_json::from_str(&started.content).unwrap();
    let metadata = started.metadata.as_ref().unwrap();
    let receipt = &metadata["child_route"];
    assert_eq!(&content["child_route"], receipt);
    for (field, value) in expected.as_object().unwrap() {
        assert_eq!(&receipt[field], value, "admitted {field}: {receipt}");
    }
    let id = metadata["agent_id"].as_str().unwrap().to_string();
    let manager = manager.read().await;
    let spec = &manager.worker_records[&id].spec;
    assert_eq!(serde_json::to_value(&spec.child_route).unwrap(), *receipt);
    assert_eq!(spec.model, receipt["model_id"].as_str().unwrap());
    assert_eq!(spec.agent_type.as_str(), receipt["canonical_role"]);
    assert_eq!(manager.agents[&id].model, spec.model);
    let manifest = spec
        .launch_manifest
        .as_ref()
        .expect("persisted launch authority");
    assert_eq!(manifest.child_id, id);
    assert_eq!(manifest.profile, spec.runtime_profile);
    assert_eq!(manifest.profile.role, spec.agent_type);
    assert_eq!(
        manifest.profile.model,
        codewhale_agent_contract::worker_profile::ModelRoute::Fixed(spec.model.clone())
    );
    assert_eq!(
        manifest.profile.provider.as_deref(),
        receipt["provider_id"].as_str()
    );
    assert_eq!(
        manifest.profile.reasoning_effort.as_deref(),
        receipt["effective_reasoning"].as_str()
    );
    id
}

async fn wait_for_queued_child(mailbox: &mut MailboxReceiver, id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = mailbox.recv().await.expect("child progress channel");
            if matches!(envelope.message, MailboxMessage::Progress { agent_id, status }
                if agent_id == id && status.contains("queued"))
            {
                break;
            }
        }
    })
    .await
    .expect("actual child reaches the held launch gate");
}

#[tokio::test]
async fn fleet_editor_save_reload_reaches_type_only_admission_without_a_model_request() {
    use crate::fleet::store::{FleetFile, FleetScope, save_fleet, set_selected};

    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _project = ProjectProfilesGuard::enabled();
    let (fixture_client, calls, bodies, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "deepseek");
    let _endpoint =
        crate::test_support::EnvVarGuard::set("CODEWHALE_BASE_URL", fixture_client.base_url());
    let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "deepseek-v4-flash");
    let config_path = root.path().join("config.toml");
    write_restart_route_config(&config_path, fixture_client.base_url(), "");
    let mut fleet = FleetFile::new("Editor restart acceptance".into(), None).unwrap();
    fleet.members.push(
        serde_json::from_value(json!({
            "id":"review-pin", "role":"reviewer", "instructions":"SAVED_REVIEW_INSTRUCTION",
            "provider":"ReviewerRoute", "model":"fixture-review-model", "reasoning":"off",
            "shortlist":false
        }))
        .unwrap(),
    );
    save_fleet(&fleet, FleetScope::Workspace, root.path()).unwrap();
    set_selected(&fleet.name, FleetScope::Workspace, root.path()).unwrap();

    drop(fixture_client);

    // Restart from the ordinary file loader; no old UI state or runtime roster survives.
    let reloaded = codewhale_plugins::providers::load_config(Some(config_path), None).unwrap();
    let client = CodewhaleClient::new(&reloaded).unwrap();
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 1);
    let gate = manager.read().await.launch_gate.clone();
    let held_permit = gate.acquire().await;
    let (mailbox, mut mailbox_rx) = Mailbox::new(CancellationToken::new());
    let context = ToolContext::new(root.path()).with_state_namespace("fleet-editor-restarted");
    let mut runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(reloaded);
    runtime.mailbox = Some(mailbox);
    let loaded_roster = spawn_roster(&runtime);
    let loaded_pin = loaded_roster
        .members()
        .iter()
        .find(|member| member.id == "review-pin")
        .unwrap();
    assert_eq!(loaded_pin.profile.reasoning_effort.as_deref(), Some("off"));
    let tool = AgentTool::new(manager.clone(), runtime);
    for extra in [
        json!({"model":"OtherRoute/fixture-review-model"}),
        json!({"model_strength":"faster"}),
    ] {
        let mut request = json!({"type":"reviewer", "prompt":"Review without execution."});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(tool.execute(request, &context).await.is_err());
        assert!(manager.read().await.agents.is_empty());
        assert!(manager.read().await.worker_records.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let started = tool
        .execute(
            json!({"type":"reviewer", "prompt":"Review without execution."}),
            &context,
        )
        .await
        .unwrap();
    let id = assert_admitted_route(
        &manager,
        &started,
        json!({
            "requested_type":"reviewer", "requested_profile":null,
            "resolved_profile_id":"review-pin", "profile_origin":"project",
            "canonical_role":"reviewer", "provider_id":"ReviewerRoute",
            "model_id":"fixture-review-model", "route_source":"agent_profile.model",
        "requested_reasoning":"inherit", "effective_reasoning":"off"
        }),
    )
    .await;
    assert!(
        manager.read().await.worker_records[&id]
            .spec
            .launch_manifest
            .as_ref()
            .unwrap()
            .prompt
            .contains("SAVED_REVIEW_INSTRUCTION")
    );
    wait_for_queued_child(&mut mailbox_rx, &id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(bodies.lock().unwrap().is_empty());
    let requested = manager.write().await.cancel_agent(&id).unwrap();
    let settled = settle_requested_child(&manager, requested).await;
    assert_eq!(settled.status, SubAgentStatus::Cancelled);
    assert_eq!(
        manager.read().await.agents[&id].status,
        SubAgentStatus::Cancelled
    );
    drop(held_permit);
}

#[tokio::test]
async fn reloaded_manual_role_pin_and_explicit_profile_keep_distinct_shortlist_receipts() {
    use crate::fleet::store::{FleetFile, FleetScope, save_fleet, set_selected};
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _project = ProjectProfilesGuard::enabled();
    let (fixture_client, calls, bodies, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "deepseek");
    let _endpoint =
        crate::test_support::EnvVarGuard::set("CODEWHALE_BASE_URL", fixture_client.base_url());
    let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "deepseek-v4-flash");
    let config_path = root.path().join("config.toml");
    write_restart_route_config(
        &config_path,
        fixture_client.base_url(),
        r#"
[subagents.models]
reviewer = "deepseek-v4-flash"
default = "deepseek-v4-pro"
[subagents.roles.reviewer]
model = "deepseek-v4-pro"
"#,
    );
    let mut fleet = FleetFile::new("Manual pin precedence".into(), None).unwrap();
    for member in [
        json!({"id":"review-choice", "shortlist":true, "provider":"ReviewerRoute", "model":"fixture-review-model"}),
        // Off stays distinct from the default on this generic custom route.
        json!({"id":"review-pin", "role":"reviewer", "provider":"ReviewerRoute", "model":"fixture-review-model", "reasoning":"off"}),
    ] {
        fleet.members.push(serde_json::from_value(member).unwrap());
    }
    save_fleet(&fleet, FleetScope::Workspace, root.path()).unwrap();
    set_selected(&fleet.name, FleetScope::Workspace, root.path()).unwrap();
    let config = codewhale_plugins::providers::load_config(Some(config_path), None).unwrap();
    let overrides = config.subagent_model_overrides();
    assert_eq!(overrides["reviewer"].model, "deepseek-v4-pro");
    assert_eq!(overrides["reviewer"].provider, None);
    let client = CodewhaleClient::new(&config).unwrap();
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 1);
    let gate = manager.read().await.launch_gate.clone();
    let held_permit = gate.acquire().await;
    let (mailbox, mut mailbox_rx) = Mailbox::new(CancellationToken::new());
    let context = ToolContext::new(root.path()).with_state_namespace("manual-role-restarted");
    let mut runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    runtime.mailbox = Some(mailbox);
    let loaded_roster = spawn_roster(&runtime);
    let loaded_pin = loaded_roster
        .members()
        .iter()
        .find(|member| member.id == "review-pin")
        .unwrap();
    assert_eq!(loaded_pin.profile.reasoning_effort.as_deref(), Some("off"));
    let tool = AgentTool::new(manager.clone(), runtime);
    let result = tool
        .execute(json!({"action":"roster"}), &context)
        .await
        .unwrap();
    let roster: Value = serde_json::from_str(&result.content).unwrap();
    let model = roster["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["selector"]["model"] == "ReviewerRoute/fixture-review-model")
        .expect("shortlisted model row");
    assert_eq!(model["route"]["provider"], "ReviewerRoute");
    assert_eq!(model["route"]["model"], "fixture-review-model");
    assert_ne!(model["route"]["source"], "role.pin");
    for role in ["general", "reviewer"] {
        let row = roster["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["role"] == role)
            .unwrap();
        assert_eq!(row["route"]["model"], "deepseek-v4-pro");
        assert_eq!(row["route"]["source"], "role.pin");
    }
    for extra in [
        json!({"model":"ReviewerRoute/fixture-review-model"}),
        json!({"model_strength":"faster"}),
    ] {
        let mut request = json!({"type":"reviewer", "prompt":"Review."});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(tool.execute(request, &context).await.is_err());
        assert!(manager.read().await.agents.is_empty());
        assert!(manager.read().await.worker_records.is_empty());
    }
    for (request, expected) in [
        (
            json!({"type":"reviewer", "prompt":"Review."}),
            json!({
                "requested_profile":null, "resolved_profile_id":null,
                "provider_id":"deepseek", "model_id":"deepseek-v4-pro", "route_source":"role.pin"
            }),
        ),
        (
            json!({"type":"reviewer", "model":"deepseek/deepseek-v4-pro", "prompt":"Review."}),
            json!({
                "requested_profile":null, "resolved_profile_id":null,
                "provider_id":"deepseek", "model_id":"deepseek-v4-pro", "route_source":"role.pin"
            }),
        ),
        (
            json!({"profile":"member:review-pin", "prompt":"Review."}),
            json!({
                "requested_profile":"member:review-pin", "resolved_profile_id":"review-pin",
                "provider_id":"ReviewerRoute", "model_id":"fixture-review-model",
                "canonical_role":"reviewer", "requested_reasoning":"inherit",
                "effective_reasoning":"off", "route_source":"agent_profile.model"
            }),
        ),
        (
            json!({"profile":"member:review-pin", "thinking":"low", "prompt":"Review."}),
            json!({
                "requested_profile":"member:review-pin", "resolved_profile_id":"review-pin",
                "provider_id":"ReviewerRoute", "model_id":"fixture-review-model",
                "canonical_role":"reviewer", "requested_reasoning":"low",
                "effective_reasoning":"high", "route_source":"agent_profile.model"
            }),
        ),
    ] {
        let started = tool.execute(request, &context).await.unwrap();
        let id = assert_admitted_route(&manager, &started, expected).await;
        wait_for_queued_child(&mut mailbox_rx, &id).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(bodies.lock().unwrap().is_empty());
        let requested = manager.write().await.cancel_agent(&id).unwrap();
        let settled = settle_requested_child(&manager, requested).await;
        assert_eq!(settled.status, SubAgentStatus::Cancelled);
        assert_eq!(
            manager.read().await.agents[&id].status,
            SubAgentStatus::Cancelled
        );
    }
    drop(held_permit);
}

#[tokio::test]
async fn loaded_structured_role_routes_bind_exact_providers_and_legacy_namespaces_stay_opaque() {
    let _env = crate::test_support::lock_test_env();
    for (
        name,
        parent_provider,
        parent_model,
        declaration,
        pin_provider,
        pin_model,
        admitted_provider,
    ) in [
        (
            "structured-route",
            "deepseek",
            "deepseek-v4-flash",
            "[subagents.roles.reviewer]\nmodel = \"ReviewerRoute/fixture-review-model\"",
            Some("ReviewerRoute"),
            "fixture-review-model",
            Some("ReviewerRoute"),
        ),
        (
            "unknown-route",
            "deepseek",
            "deepseek-v4-flash",
            "[subagents.roles.reviewer]\nmodel = \"MissingRoute/fixture-review-model\"",
            Some("MissingRoute"),
            "fixture-review-model",
            None,
        ),
        (
            "legacy-namespace",
            "openrouter",
            "deepseek/deepseek-v4-flash",
            "[subagents.models]\nreviewer = \"deepseek/deepseek-v4-pro\"",
            None,
            "deepseek/deepseek-v4-pro",
            Some("openrouter"),
        ),
    ] {
        let root = tempdir().unwrap();
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
        let (fixture_client, calls, bodies, _) = delayed_chat_client(Duration::ZERO, "done").await;
        let _provider =
            crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", parent_provider);
        let _endpoint =
            crate::test_support::EnvVarGuard::set("CODEWHALE_BASE_URL", fixture_client.base_url());
        let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", parent_model);
        let config_path = root.path().join("config.toml");
        // `deepseek` is a real configured provider as well as the legacy wire
        // namespace. The old map must not reinterpret that namespace as a pin.
        let declarations = format!(
            r#"
[providers.deepseek]
api_key = "fixture-deepseek-key"
base_url = "{base_url}"
model = "deepseek-v4-flash"
[providers.openrouter]
api_key = "fixture-router-key"
base_url = "{base_url}"
model = "deepseek/deepseek-v4-flash"
{declaration}
"#,
            base_url = fixture_client.base_url()
        );
        write_restart_route_config(&config_path, fixture_client.base_url(), &declarations);
        let config = codewhale_plugins::providers::load_config(Some(config_path), None).unwrap();
        let overrides = config.subagent_model_overrides();
        assert_eq!(
            overrides["reviewer"].provider.as_deref(),
            pin_provider,
            "{name}"
        );
        assert_eq!(overrides["reviewer"].model, pin_model, "{name}");
        assert_eq!(
            config.active_provider_identity().unwrap().key.as_str(),
            parent_provider
        );
        let client = CodewhaleClient::new(&config).unwrap();
        let manager = new_shared_subagent_manager(root.path().to_path_buf(), 1);
        let gate = manager.read().await.launch_gate.clone();
        let held_permit = gate.acquire().await;
        let (mailbox, mut mailbox_rx) = Mailbox::new(CancellationToken::new());
        let context = ToolContext::new(root.path()).with_state_namespace(name);
        let mut runtime = SubAgentRuntime::new(
            client,
            parent_model.into(),
            context.clone(),
            false,
            None,
            manager.clone(),
        )
        .with_api_config(config);
        runtime.mailbox = Some(mailbox);
        let tool = AgentTool::new(manager.clone(), runtime);
        let roster_result = tool
            .execute(json!({"action":"roster"}), &context)
            .await
            .unwrap();
        let roster: Value = serde_json::from_str(&roster_result.content).unwrap();
        let row = roster["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["role"] == "reviewer")
            .unwrap();
        let request = json!({"type":"reviewer", "prompt":"Review before model execution."});
        let Some(expected_provider) = admitted_provider else {
            assert!(row["route"].is_null());
            assert!(
                row["route_error"]
                    .as_str()
                    .unwrap()
                    .contains("MissingRoute")
            );
            let error = tool.execute(request, &context).await.unwrap_err();
            assert!(error.to_string().contains("MissingRoute"), "{error}");
            assert!(manager.read().await.agents.is_empty());
            assert!(manager.read().await.worker_records.is_empty());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(bodies.lock().unwrap().is_empty());
            drop(held_permit);
            continue;
        };
        assert_eq!(row["route"]["provider"], expected_provider, "{name}");
        assert_eq!(row["route"]["model"], pin_model, "{name}");
        assert_eq!(row["route"]["source"], "role.pin", "{name}");
        if pin_provider.is_some() {
            for extra in [
                json!({"model":"OtherRoute/fixture-review-model"}),
                json!({"model_strength":"faster"}),
            ] {
                let mut conflicting = request.clone();
                conflicting
                    .as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                let error = tool.execute(conflicting, &context).await.unwrap_err();
                assert!(error.to_string().contains("pins"), "{error}");
                assert!(manager.read().await.agents.is_empty());
                assert!(manager.read().await.worker_records.is_empty());
                assert_eq!(calls.load(Ordering::SeqCst), 0);
            }
        }
        let mut matching = request.clone();
        matching["model"] = json!(format!("{expected_provider}/{pin_model}"));
        for request in [request, matching] {
            let started = tool.execute(request, &context).await.unwrap();
            let id = assert_admitted_route(
                &manager,
                &started,
                json!({
                    "requested_type":"reviewer", "canonical_role":"reviewer",
                    "requested_profile":null, "resolved_profile_id":null, "profile_origin":null,
                    "provider_id":expected_provider, "model_id":pin_model, "route_source":"role.pin"
                }),
            )
            .await;
            wait_for_queued_child(&mut mailbox_rx, &id).await;
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(bodies.lock().unwrap().is_empty());
            let requested = manager.write().await.cancel_agent(&id).unwrap();
            let settled = settle_requested_child(&manager, requested).await;
            assert_eq!(settled.status, SubAgentStatus::Cancelled);
            assert_eq!(
                manager.read().await.agents[&id].status,
                SubAgentStatus::Cancelled
            );
        }
        drop(held_permit);
    }
}

#[tokio::test]
async fn issue_6117_invalid_personal_profile_is_visible_and_never_admitted_as_builtin() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let home = root.path().join("state");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
    std::fs::create_dir_all(home.join("agents")).unwrap();
    let profile = home.join("agents/scout.toml");
    std::fs::write(&profile, "provider = \"openrouter\"\nmodel = \"qwen/qwen3.7-plus\"\nallow_shell = false\ntrust = false\n").unwrap();
    let (client, calls, _, _) = delayed_chat_client(Duration::ZERO, "done").await;
    let config = <codewhale_runtime::config::Config as Default>::default()
        .with_legacy_root(Some("test-key".into()), Some(client.base_url().into()));
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 2);
    let context = ToolContext::new(root.path()).with_state_namespace("issue-6117");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    let tool = AgentTool::new(manager.clone(), runtime);
    let discovered = tool
        .execute(json!({"action":"roster"}), &context)
        .await
        .unwrap();
    let roster: Value = serde_json::from_str(&discovered.content).unwrap();
    assert_eq!(roster["profile_load_issue_count"], 1);
    assert_eq!(roster["profile_load_issues"][0]["id"], "scout");
    for selector in ["scout", "explore", "member:SCOUT"] {
        let error = tool
            .execute(
                json!({"action":"start", "profile":selector, "prompt":"Inspect."}),
                &context,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("scout.toml"), "{error}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(manager.read().await.agents.is_empty());
    std::fs::write(&profile, "base_role = \"explore\"\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_effort = \"low\"\n[permissions]\nallow_shell = false\ntrust = false\n").unwrap();
    let started = tool
        .execute(
            json!({"action":"start", "profile":"scout", "prompt":"Say done."}),
            &context,
        )
        .await
        .unwrap();
    let meta = started.metadata.as_ref().unwrap();
    let receipt = &meta["child_route"];
    assert_eq!(receipt["resolved_profile_id"], "scout");
    assert_eq!(receipt["profile_origin"], "personal");
    assert_eq!(receipt["provider_id"], "deepseek");
    assert_eq!(receipt["model_id"], "deepseek-v4-pro");
    assert_eq!(receipt["effective_reasoning"], "low");
    assert_eq!(receipt["route_source"], "agent_profile.model");
    let id = meta["agent_id"].as_str().unwrap();
    let mut guard = manager.write().await;
    assert!(
        !guard.worker_records[id]
            .spec
            .runtime_profile
            .permissions
            .write
    );
    if guard.agents[id].status == SubAgentStatus::Running {
        guard.cancel_agent(id).unwrap();
    }
}
