use super::*;
use crate::core::ops::TurnSpec;

fn tool(name: &str, deferred: bool) -> Tool {
    Tool {
        tool_type: None,
        name: name.to_string(),
        description: format!("{name} description"),
        input_schema: json!({"type": "object", "properties": {}}),
        allowed_callers: None,
        defer_loading: Some(deferred),
        input_examples: None,
        strict: None,
        cache_control: None,
    }
}

#[test]
fn active_catalog_hash_tracks_membership_order_and_schema() {
    let base = vec![tool("Bash", false), tool("File", false)];
    let baseline = active_tool_catalog_sha256(&base);

    assert_eq!(baseline, active_tool_catalog_sha256(&base.clone()));

    let mut reordered = base.clone();
    reordered.swap(0, 1);
    assert_ne!(baseline, active_tool_catalog_sha256(&reordered));

    let mut fewer = base.clone();
    fewer.pop();
    assert_ne!(baseline, active_tool_catalog_sha256(&fewer));

    let mut retyped = base.clone();
    retyped[0].input_schema = json!({"type": "object", "required": ["cmd"]});
    assert_ne!(baseline, active_tool_catalog_sha256(&retyped));
}

#[test]
fn preview_and_production_share_one_input_estimate_and_send_decision() {
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "near the context ceiling".to_string(),
            cache_control: None,
        }],
    }];
    let system = SystemPrompt::Text("stable system".to_string());

    // Production sends stored history and nothing else, and preview describes
    // that same list, so a single estimate drives the manifest number and the
    // overflow/no-exact-outbound decision. There is no second synthetic list
    // to charge a separate framing overhead for.
    let estimate = crate::compaction::estimate_input_tokens_conservative(&messages, Some(&system));

    let ceiling = estimate - 1;
    assert_eq!(
        crate::request_manifest::production_input_headroom(Some(ceiling), estimate),
        Some(-1)
    );
    assert!(crate::request_manifest::production_input_budget_exceeded(
        Some(ceiling),
        estimate
    ));
    assert!(!crate::request_manifest::production_input_budget_exceeded(
        Some(estimate),
        estimate
    ));
}

#[test]
fn standard_and_full_are_reported_collapsed_from_the_real_shaper() {
    let catalog = vec![tool("Bash", false), tool("agent", false), tool("Web", true)];
    let always_load = std::collections::HashSet::new();
    assert!(
        standard_and_full_collapse(&catalog, &always_load),
        "Standard and Full apply no narrowing today, so they must report collapsed"
    );
}

#[tokio::test]
async fn pending_shell_completion_makes_the_body_unavailable_without_draining_it() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    let owner_session_id = engine.session.id.clone();

    {
        let mut manager = engine.shell_manager.lock().expect("shell manager");
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        manager
            .execute_with_options_env_for_session(
                command,
                None,
                30_000,
                true,
                None,
                false,
                None,
                std::collections::HashMap::new(),
                &owner_session_id,
            )
            .expect("background shell starts");
        assert!(manager.may_have_undelivered_completion_for_session(&owner_session_id));
    }

    let planned = plan(&config, &identity, false, "inspect the next request").await;
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), "inspect the next request"))
        .await;
    let unavailable = match &manifest.body {
        Availability::Unavailable(unavailable) => unavailable,
        Availability::Exact(_) => panic!("pending shell completion must fail closed"),
    };
    assert_eq!(
        unavailable.reason,
        UnavailableReason::RuntimeTransformsBeforeSend
    );
    assert!(
        unavailable
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("background shell completion")),
        "{unavailable:?}"
    );

    let mut manager = engine.shell_manager.lock().expect("shell manager");
    assert!(
        manager.may_have_undelivered_completion_for_session(&owner_session_id),
        "preview must not drain or report the completion"
    );
    let _ = manager.kill_running();
    let _ = manager.drain_finished_jobs_with_evidence();
}

#[tokio::test]
async fn running_direct_child_fails_closed_without_consuming_or_mutating_state() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    let active_session_id = engine.session.id.clone();
    let mut before = {
        let mut manager = engine.subagent_manager.write().await;
        let agent_id = manager.insert_test_running_direct_child("preview_pending", tmp.path());
        manager.assign_test_session_owner(&agent_id, &active_session_id);
        serde_json::to_value(manager.list()).expect("manager snapshot")
    };
    if let Some(rows) = before.as_array_mut() {
        for row in rows {
            row.as_object_mut()
                .expect("agent object")
                .remove("duration_ms");
        }
    }
    let delivered_before = engine.delivered_subagent_completion_ids.clone();

    let planned = plan(&config, &identity, false, "inspect while child runs").await;
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), "inspect while child runs"))
        .await;
    let unavailable = match &manifest.body {
        Availability::Unavailable(unavailable) => unavailable,
        Availability::Exact(_) => panic!("running child must fail closed"),
    };
    assert_eq!(
        unavailable.reason,
        UnavailableReason::RuntimeTransformsBeforeSend
    );
    assert!(
        unavailable
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("running or undelivered sub-agent"))
    );

    let mut after = {
        let manager = engine.subagent_manager.read().await;
        assert!(manager.may_transform_next_parent_request_for_session(
            &active_session_id,
            &engine.delivered_subagent_completion_ids,
        ));
        serde_json::to_value(manager.list()).expect("manager snapshot")
    };
    if let Some(rows) = after.as_array_mut() {
        for row in rows {
            row.as_object_mut()
                .expect("agent object")
                .remove("duration_ms");
        }
    }
    assert_eq!(before, after, "preview must not mutate child state");
    assert_eq!(
        delivered_before, engine.delivered_subagent_completion_ids,
        "preview must not claim child delivery"
    );
}

#[tokio::test]
async fn terminal_undelivered_child_fails_closed_without_claiming_delivery() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    let active_session_id = engine.session.id.clone();
    let agent_id = {
        let mut manager = engine.subagent_manager.write().await;
        let agent_id = manager.insert_test_terminal_direct_child("preview_terminal", tmp.path());
        manager.assign_test_session_owner(&agent_id, &active_session_id);
        agent_id
    };

    let planned = plan(&config, &identity, false, "inspect settled child").await;
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), "inspect settled child"))
        .await;
    assert!(matches!(manifest.body, Availability::Unavailable(_)));
    assert!(
        !engine.delivered_subagent_completion_ids.contains(&agent_id),
        "preview must not claim terminal delivery"
    );
    let manager = engine.subagent_manager.read().await;
    assert!(manager.may_transform_next_parent_request_for_session(
        &active_session_id,
        &engine.delivered_subagent_completion_ids,
    ));
    assert!(matches!(
        manager
            .get_result(&agent_id)
            .expect("terminal child")
            .status,
        crate::tools::subagent::SubAgentStatus::Completed
    ));
}

#[test]
fn turn_metadata_uses_planned_cross_route_limits_not_installed_limits() {
    let config = deepseek_config();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    // Pressure advice is only surfaced when the user opts out of automatic
    // maintenance. The route-budget assertion still applies in that mode.
    engine.config.compaction.enabled = false;
    engine.api_provider = ProviderKind::Deepseek;
    let installed_limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(4_096),
        input_tokens: None,
        output_tokens: Some(512),
        output_declared: false,
    };
    engine.active_route_limits = Some(installed_limits);
    // Large enough to be critical for the installed 4K route, but safely
    // below the warning threshold for the planned 123K route.
    engine.session.messages.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "x".repeat(20_000),
            cache_control: None,
        }],
    });
    let prompt_context = NextTurnPromptContext::for_planned_turn(
        ProviderKind::Openrouter,
        "qwen/qwen3.6-flash".to_string(),
        Some(codewhale_config::route::RouteLimits {
            context_tokens: Some(123_456),
            input_tokens: None,
            output_tokens: Some(4_096),
            output_declared: false,
        }),
        AppMode::Agent,
        None,
        GoalStatus::Active,
        None,
        false,
        None,
    );
    let system_prompt = engine.compose_stable_system_prompt(&prompt_context);
    assert_eq!(
        engine.context_pressure_line(
            "cross-route budget",
            &prompt_context,
            system_prompt.as_ref()
        ),
        None,
        "the planned 123K route must not inherit the installed route's pressure"
    );
    let installed_context = NextTurnPromptContext::for_planned_turn(
        ProviderKind::Deepseek,
        "deepseek-v4-flash".to_string(),
        Some(installed_limits),
        AppMode::Agent,
        None,
        GoalStatus::Active,
        None,
        false,
        None,
    );
    let pressure = engine
        .context_pressure_line("cross-route budget", &installed_context, None)
        .unwrap();
    assert!(pressure.contains("Context pressure: critical"));
    assert!(pressure.contains("Estimated input:"));
    assert!(pressure.contains("Making room automatically is off"));
    let message = engine.user_text_message_from_snapshot(
        "cross-route budget".to_string(),
        &prompt_context.model,
        true,
        None,
        false,
        UserInputProvenance::ExternalUser,
        TurnMetadataSnapshot {
            prompt_context: &prompt_context,
            system_prompt: system_prompt.as_ref(),
            approval_mode: ApprovalMode::Suggest,
            working_set: &engine.session.working_set,
            policy_narrowing: None,
        },
    );
    let metadata = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .next_back()
        .expect("turn metadata text");
    assert!(
        !metadata.contains("Context pressure:"),
        "planned route metadata must remain below warning: {metadata}"
    );
    assert!(!metadata.contains("123456 tokens"), "{metadata}");
    assert!(!metadata.contains("4096 tokens"), "{metadata}");
}

/// #perf-r5: the pressure-line helper must estimate the history IN PLACE and
/// add the composer text arithmetically. Guards two things at once:
///
/// 1. Equivalence — the arithmetic form must equal the naive
///    "clone + push + estimate" reference for non-trivial inputs (Unicode
///    multi-byte content included, since Text blocks count *chars* for the
///    conservative estimator but the delta path counts... the same rule as
///    `estimate_tokens_for_message`: bytes/4).
/// 2. The contract that empty/no-op composer text costs nothing extra.
#[test]
fn context_pressure_delta_matches_clone_and_push_reference() {
    let config = deepseek_config();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.api_provider = ProviderKind::Deepseek;
    let installed_limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(64_000),
        input_tokens: None,
        output_tokens: Some(512),
        output_declared: false,
    };
    engine.active_route_limits = Some(installed_limits);
    // Multi-byte content on purpose: chars().count() != len() here, so an
    // arity mistake between the byte rule (estimator) would surface.
    engine.session.messages.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "héllo wörld — ünïcode ✓ ".repeat(500),
            cache_control: None,
        }],
    });
    engine.session.messages.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Thinking {
            thinking: "step".repeat(100),
            signature: None,
            state: None,
            redacted_data: None,
        }],
    });
    // Replayed-reasoning case (#perf-r5 fresh-eyes fix): an assistant message
    // carrying BOTH thinking and a tool call keeps its reasoning content in
    // every subsequent request — the estimator counts those bytes, and this
    // was the exact arm the delta helper originally missed. Both parity
    // variants of the thinking byte-count are exercised below.
    engine.session.messages.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "replayed".repeat(300), // 8 bytes per unit -> even count
                signature: None,
                state: None,
                redacted_data: None,
            },
            ContentBlock::ToolUse {
                execution_id: None,
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: json!({"command": "echo hello"}),
                caller: None,
                thought_signature: None,
            },
        ],
    });
    engine.session.messages.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "odd replay".to_string(), // 11 bytes / 4 = 2 (even)... use odd total
                signature: None,
                state: None,
                redacted_data: None,
            },
            ContentBlock::ToolUse {
                execution_id: None,
                id: "call_2".to_string(),
                name: "read".to_string(),
                input: json!({"path": "x"}), // 13-byte JSON -> 3
                caller: None,
                thought_signature: None,
            },
        ],
    });
    let _prompt_context = NextTurnPromptContext::for_planned_turn(
        ProviderKind::Deepseek,
        "deepseek-v4-flash".to_string(),
        Some(installed_limits),
        AppMode::Agent,
        None,
        GoalStatus::Active,
        None,
        false,
        None,
    );
    let _ = &_prompt_context;

    // Naive reference implementation: clone the transcript, push a
    // hypothetical user message, run the full conservative estimator.
    let reference = |engine: &Engine, text: &str| -> usize {
        let mut messages: Vec<Message> =
            crate::prompt_zones::AppendLog::clone(&engine.session.messages).into();
        if !text.trim().is_empty() {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: text.to_string(),
                    cache_control: None,
                }],
            });
        }
        crate::compaction::estimate_input_tokens_conservative(&messages, None)
    };

    for text in [
        "",
        "   ",
        "short",
        "a much longer composer draft with punctuation…",
    ] {
        let via_pressure_line_input = engine.active_input_tokens_with_current_text(text, None);
        assert_eq!(
            via_pressure_line_input,
            reference(&engine, text),
            "delta arithmetic diverged from clone+push+estimate for {text:?}"
        );
    }
}

/// #perf-r5 guard: billed input above the threshold must report pressure with
/// a provably-empty history — proving the short-circuit answers from billing
/// alone without consulting message contents.
#[test]
fn billed_pressure_above_threshold_answers_from_billing_alone() {
    let config = CompactionConfig {
        enabled: true,
        token_threshold: 1_000,
        ..Default::default()
    };
    let pressure = crate::compaction::compaction_pressure_reached_with_billed(
        &[], // empty history: only billing can prove pressure
        None,
        &config,
        Some(2_000),
    );
    assert!(pressure, "billed 2000 >= threshold 1000 must be pressure");
}

/// #perf-r5 guard: under-threshold billing keeps the old max() semantics —
/// an estimate above the trigger still fires even when billing is quiet.
#[test]
fn billed_below_threshold_still_fires_on_estimate() {
    let config = CompactionConfig {
        enabled: true,
        token_threshold: 100,
        ..Default::default()
    };
    let big = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "x".repeat(4 * 200),
            cache_control: None,
        }],
    };
    let pressure = crate::compaction::compaction_pressure_reached_with_billed(
        std::slice::from_ref(&big),
        None,
        &config,
        Some(10), // below threshold; must not short-circuit to false either
    );
    assert!(pressure, "estimate 200 (+1.0 framing) >= 100 must fire");
}

/// #perf-r5 guard: a direct `session.messages` overwrite (the SyncSession
/// restore path) must advance `messages_revision` so the token-estimate
/// cache invalidates instead of serving the pre-sync value.
#[test]
fn sync_restore_bumps_messages_revision_for_estimate_cache() {
    use crate::core::engine::token_estimate_cache::TokenEstimateCache;

    let config = deepseek_config();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "before restore".to_string(),
            cache_control: None,
        }],
    });
    let revision_before = engine.session.messages_revision;
    let mut cache = TokenEstimateCache::new();
    let stale = cache.lookup_or_compute(
        revision_before,
        engine.session.system_prompt.as_ref(),
        &engine.session.messages,
    );

    // Simulate the restore's direct field assignment.
    engine.session.messages = Vec::new().into();
    engine.session.bump_messages_revision();

    assert_ne!(
        engine.session.messages_revision, revision_before,
        "restore must bump the revision the estimate cache keys on"
    );
    let fresh = cache.lookup_or_compute(
        engine.session.messages_revision,
        engine.session.system_prompt.as_ref(),
        &engine.session.messages,
    );
    assert_ne!(
        fresh, stale,
        "cache must recompute after a restore-driven revision bump"
    );
}

#[tokio::test]
async fn compaction_preview_uses_the_planned_routes_system_prompt() {
    let config = deepseek_config();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);

    let messages: Vec<Message> = (0..30)
        .map(|index| Message {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: vec![ContentBlock::Text {
                text: "x".repeat(10_000),
                cache_control: None,
            }],
        })
        .collect();
    let installed_prompt = SystemPrompt::Text("installed route".to_string());
    let planned_prompt = SystemPrompt::Text("planned-route-system ".repeat(1_500));
    engine.session.system_prompt = Some(installed_prompt.clone());

    let installed_pressure =
        crate::compaction::estimate_input_tokens_for_pressure(&messages, Some(&installed_prompt));
    let planned_pressure =
        crate::compaction::estimate_input_tokens_for_pressure(&messages, Some(&planned_prompt));
    assert!(planned_pressure > installed_pressure);
    let compaction = crate::compaction::CompactionConfig {
        enabled: true,
        token_threshold: installed_pressure + (planned_pressure - installed_pressure) / 2,
        ..Default::default()
    };

    let planned_reasons = engine
        .preview_runtime_transforms(&messages, Some(&planned_prompt), &compaction)
        .await;
    assert!(
        planned_reasons.contains(&"making room would summarize the conversation first"),
        "the planned route prompt crosses the compaction threshold: {planned_reasons:?}"
    );

    let installed_reasons = engine
        .preview_runtime_transforms(&messages, Some(&installed_prompt), &compaction)
        .await;
    assert!(
        !installed_reasons.contains(&"making room would summarize the conversation first"),
        "the installed route prompt is the below-threshold control: {installed_reasons:?}"
    );
}

#[tokio::test]
async fn planned_route_builds_subagent_catalog_without_installed_client() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    let _ = engine.config.features.enable(Feature::Subagents);
    engine.config.subagents_enabled = true;
    engine.codewhale_client = None;
    let planned = plan(&config, &identity, false, "planned child route").await;
    let route = planned.route.validate().expect("planned route validates");
    let planned_model = route.model.clone();
    let policy = TurnAuthority::from_effective_fields(
        AppMode::Agent,
        false,
        false,
        false,
        ApprovalMode::Suggest,
    );
    let build = engine
        .build_turn_tool_registry_and_catalog(
            &policy,
            &[],
            None,
            SubAgentWiring::Inert,
            McpAccess::PassiveSnapshot,
            TurnRouteContext {
                provider: route.identity.provider,
                model: route.model.clone(),
                capabilities: route.candidate.capabilities(),
                limits: crate::route_budget::known_route_limits(route.candidate.limits()),
                client: Some(route.client),
                api_config: route.config,
                locale_tag: engine.config.locale_tag.clone(),
                role_models: engine.subagent_role_models(),
                auto_model: false,
                reasoning_effort: planned.effective_reasoning_effort,
                reasoning_effort_auto: planned.auto_controls_reasoning,
            },
            "",
            None,
        )
        .await;
    assert!(
        build
            .surface
            .catalog
            .iter()
            .any(|tool| tool.name == "agent"),
        "the planned route client must make sub-agent tools available"
    );
    assert_eq!(
        build.subagent_runtime_model.as_deref(),
        Some(planned_model.as_str()),
        "the child runtime must carry the planned route model"
    );
}

/// Auto routing with no hypothetical prompt: every route-derived fact is
/// structurally absent, and the flag is never cleared just because the
/// session happens to have an installed route.
#[tokio::test]
async fn auto_route_without_a_prompt_omits_every_final_fact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut engine, _handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            ..Default::default()
        },
        &crate::config::Config::default(),
    );

    let manifest = engine
        .build_request_manifest(PreviewRequestInputs {
            mode: AppMode::Agent,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            provenance: UserInputProvenance::ExternalUser,
            requested_model: "auto".to_string(),
            requested_reasoning: "auto".to_string(),
            auto_model: true,
            hypothetical_prompt_supplied: false,
            next_turn: None,
            unresolved: PreviewUnresolved::AutoRouteNeedsPrompt,
        })
        .await;

    assert!(manifest.route.exact().is_none());
    assert!(manifest.tools.exact().is_none());
    assert!(manifest.body.exact().is_none());
    assert_eq!(manifest.session.requested_model.as_str(), "auto");
    assert!(manifest.session.auto_model_routing);
    assert!(!manifest.session.hypothetical_prompt_supplied);

    let json = manifest.to_json();
    for forbidden in [
        "provider_id",
        "wire_model",
        "endpoint_fingerprint",
        "body_sha256",
        "tool_surface_budget",
        "billing",
    ] {
        assert!(!json.contains(forbidden), "{forbidden} leaked:\n{json}");
    }
}

fn deepseek_config() -> crate::config::Config {
    let providers = crate::config::ProvidersConfig {
        deepseek: crate::config::ProviderConfig {
            api_key: Some("sk-test-deepseek".to_string()),
            model: Some("deepseek-chat".to_string()),
            ..crate::config::ProviderConfig::default()
        },
        ..crate::config::ProvidersConfig::default()
    };
    crate::config::Config {
        provider: Some("deepseek".to_string()),
        providers: Some(providers),
        ..crate::config::Config::default()
    }
}

fn deepseek_identity() -> crate::config::ProviderIdentity {
    crate::config::Config::default()
        .builtin_provider_identity(ProviderKind::Deepseek)
        .unwrap()
}

/// Run the *production* route planner, provider-free: with `auto_model`
/// the classifier short-circuits to the inventory heuristic under `cfg!(test)`.
async fn plan(
    config: &crate::config::Config,
    identity: &crate::config::ProviderIdentity,
    auto_model: bool,
    prompt: &str,
) -> crate::turn_route_plan::PlannedTurnRoute {
    plan_for(
        config,
        identity,
        ProviderKind::Deepseek,
        "deepseek-chat",
        auto_model,
        prompt,
    )
    .await
}

async fn plan_for(
    config: &crate::config::Config,
    identity: &crate::config::ProviderIdentity,
    provider: ProviderKind,
    model: &str,
    auto_model: bool,
    prompt: &str,
) -> crate::turn_route_plan::PlannedTurnRoute {
    plan_with_reasoning(
        config,
        identity,
        provider,
        model,
        auto_model,
        if auto_model {
            crate::reasoning_preference::ReasoningEffort::Auto
        } else {
            crate::reasoning_preference::ReasoningEffort::High
        },
        prompt,
    )
    .await
}

/// `plan_for` with the requested reasoning tier under test control. The
/// exact-route matrix needs `off` to observe a route that normalizes it
/// (direct Moonshot K3 sends `low`).
async fn plan_with_reasoning(
    config: &crate::config::Config,
    identity: &crate::config::ProviderIdentity,
    provider: ProviderKind,
    model: &str,
    auto_model: bool,
    reasoning_effort: crate::reasoning_preference::ReasoningEffort,
    prompt: &str,
) -> crate::turn_route_plan::PlannedTurnRoute {
    crate::turn_route_plan::plan_turn_route(crate::turn_route_plan::TurnRoutePlanRequest {
        route_config: config,
        app_route_identity: identity,
        api_provider: provider,
        app_model: model,
        auto_model,
        reasoning_effort,
        mode: AppMode::Agent,
        content: prompt,
        auto_router_context: "",
        should_auto_resolve: auto_model,
        allow_auto_router_response_cache: false,
        preflight_required: false,
        auto_compact_user_configured: false,
        auto_compact: true,
        auto_compact_threshold_percent: 80.0,
    })
    .await
    .expect("the shared planner resolves a configured route")
}

fn preview_engine(config: &crate::config::Config) -> (Engine, EngineHandle, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (engine, handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            ..Default::default()
        },
        config,
    );
    (engine, handle, tmp)
}

/// The engine plus its handle: a turn is admitted only while its event
/// consumer is alive, as in every production embedding, so callers keep the
/// handle for the engine's lifetime even though these fixtures read no events.
fn wire_preview_engine(
    config: &crate::config::Config,
) -> (Engine, crate::core::engine::EngineHandle, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            max_steps: 1,
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..Default::default()
        },
        config,
    );
    engine.config.features.disable(Feature::Mcp);
    engine.config.subagents_enabled = false;
    (engine, handle, tmp)
}

fn inputs(
    auto_model: bool,
    planned: Option<crate::turn_route_plan::PlannedTurnRoute>,
    prompt: &str,
) -> PreviewRequestInputs {
    PreviewRequestInputs {
        mode: AppMode::Agent,
        allow_shell: false,
        trust_mode: false,
        auto_approve: false,
        approval_mode: ApprovalMode::Suggest,
        allowed_tools: None,
        dynamic_tools: Vec::new(),
        provenance: UserInputProvenance::ExternalUser,
        requested_model: if auto_model {
            "auto".to_string()
        } else {
            "deepseek-chat".to_string()
        },
        requested_reasoning: if auto_model { "auto" } else { "high" }.to_string(),
        auto_model,
        hypothetical_prompt_supplied: true,
        next_turn: planned.map(|planned| {
            let prompt_context = NextTurnPromptContext::for_planned_turn(
                planned.route.identity.provider,
                planned.route.model.clone(),
                crate::route_budget::known_route_limits(planned.route.candidate.limits()),
                AppMode::Agent,
                None,
                GoalStatus::Active,
                None,
                false,
                None,
            );
            Box::new(PreviewNextTurn {
                content: prompt.to_string(),
                route: Box::new(planned.route),
                prompt_context,
                reasoning_effort: planned.effective_reasoning_effort,
                reasoning_effort_auto: planned.auto_controls_reasoning,
                auto_route_source: planned
                    .auto_selection
                    .as_ref()
                    .map(|selection| selection.source.label().to_string()),
                routing_source: planned.routing_source,
                compaction: planned.compaction,
            })
        }),
        unresolved: PreviewUnresolved::NoPrompt,
    }
}

/// Typed controls for the preview/wire parity fixture. Defaults mirror the
/// ordinary active DeepSeek turn; individual tests override only the
/// production context they are proving.
struct PreviewWireFixture {
    goal_objective: Option<String>,
    goal_status: GoalStatus,
    translation_enabled: bool,
    verbosity: Option<String>,
    requested_model: Option<String>,
    requested_reasoning: Option<String>,
}

impl Default for PreviewWireFixture {
    fn default() -> Self {
        Self {
            goal_objective: None,
            goal_status: GoalStatus::Active,
            translation_enabled: false,
            verbosity: None,
            requested_model: None,
            requested_reasoning: None,
        }
    }
}

async fn assert_preview_matches_first_wire_body(
    engine: &mut Engine,
    server: &wiremock::MockServer,
    planned: crate::turn_route_plan::PlannedTurnRoute,
    prompt: &str,
    fixture: PreviewWireFixture,
) -> (RequestManifest, serde_json::Value) {
    let PreviewWireFixture {
        goal_objective,
        goal_status,
        translation_enabled,
        verbosity,
        requested_model,
        requested_reasoning,
    } = fixture;
    let production_route = planned.route.clone();
    let compaction = planned.compaction.clone();
    let reasoning_effort = planned.effective_reasoning_effort.clone();
    let reasoning_effort_auto = planned.auto_controls_reasoning;
    let mut preview_inputs = inputs(false, Some(planned), prompt);
    if let Some(requested_model) = requested_model {
        preview_inputs.requested_model = requested_model;
    }
    if let Some(requested_reasoning) = requested_reasoning {
        preview_inputs.requested_reasoning = requested_reasoning;
    }
    let next = preview_inputs.next_turn.as_mut().expect("planned preview");
    next.prompt_context = NextTurnPromptContext::for_planned_turn(
        production_route.identity.provider,
        production_route.model.clone(),
        crate::route_budget::known_route_limits(production_route.candidate.limits()),
        AppMode::Agent,
        goal_objective.clone(),
        goal_status,
        None,
        translation_enabled,
        verbosity.clone(),
    );
    let manifest = engine.build_request_manifest(preview_inputs).await;
    let preview_hash = manifest
        .body
        .exact()
        .expect("preview body is exact")
        .body_sha256
        .clone();

    let _ = engine
        .handle_send_message(TurnSpec {
            profile_constitution: None,
            content: prompt.to_string(),
            mode: AppMode::Agent,
            route: Box::new(production_route),
            compaction: Box::new(compaction),
            initial_routed_usage: Box::new(crate::cost_status::RuntimeUsageBatch::default()),
            goal_objective,
            goal_token_budget: None,
            goal_status,
            reasoning_effort,
            reasoning_effort_auto,
            auto_model: false,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            translation_enabled,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity,
            provenance: UserInputProvenance::ExternalUser,
            images: Vec::new(),
            max_output_tokens: None,
            submission_id: None,
        })
        .await;

    let requests = server
        .received_requests()
        .await
        .expect("wire mock records requests");
    assert_eq!(requests.len(), 1, "the fixture must make one provider call");
    let first_wire_body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("first HTTP body is JSON");
    let first_wire_hash =
        crate::hashing::sha256_hex(crate::client::canonical_json(&first_wire_body).as_bytes());
    assert_eq!(
        preview_hash, first_wire_hash,
        "preview hash must match the body captured at the HTTP boundary"
    );
    (manifest, first_wire_body)
}

#[tokio::test]
async fn graph_backed_todo_is_not_reinjected_into_the_first_http_body() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n"),
        )
        .mount(&server)
        .await;
    let mut config = deepseek_config();
    config
        .providers
        .as_mut()
        .expect("providers")
        .deepseek
        .base_url = Some(server.uri());
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = wire_preview_engine(&config);
    let graph_todos = crate::tools::todo::TodoListSnapshot {
        items: vec![crate::tools::todo::TodoItem {
            id: 1,
            content: "preserve this graph-authoritative Work item".to_string(),
            status: crate::tools::todo::TodoStatus::InProgress,
        }],
        completion_pct: 0,
        in_progress_id: Some(1),
    };
    let work = crate::work_graph::new_shared_work_runtime(
        engine.config.todos.clone(),
        engine.config.plan_state.clone(),
    );
    work.restore(
        "preview-graph-work",
        None,
        &graph_todos,
        &crate::tools::plan::PlanSnapshot::default(),
    )
    .expect("restore graph-backed Work state");
    *engine.config.todos.lock().await = crate::tools::todo::TodoList::new();
    assert!(
        engine.config.todos.lock().await.snapshot().is_empty(),
        "legacy projection is intentionally stale for this authority test"
    );
    engine.config.runtime_services.work = Some(work);

    let prompt = "inspect the request without restating the To-do list";
    let planned = plan(&config, &identity, false, prompt).await;
    let (_, first_wire_body) = assert_preview_matches_first_wire_body(
        &mut engine,
        &server,
        planned,
        prompt,
        PreviewWireFixture::default(),
    )
    .await;
    let body_text = first_wire_body.to_string();
    assert!(
        !body_text.contains("<codewhale:work_state>")
            && !body_text.contains("preserve this graph-authoritative Work item"),
        "provider requests must not receive a synthetic per-step To-do tail: {body_text}"
    );
}

#[tokio::test]
async fn exhausted_active_goal_remains_previewable() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    sync_goal_state_from_host(
        &engine.config.goal_state,
        Some("finish the release"),
        Some(100),
        GoalStatus::Active,
    );
    engine
        .config
        .goal_state
        .lock()
        .expect("goal state")
        .record_usage(100, 0, std::time::Instant::now());

    let prompt = "continue the release";
    let planned = plan(&config, &identity, false, prompt).await;
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), prompt))
        .await;
    assert!(manifest.route.exact().is_some());
    assert!(manifest.tools.exact().is_some());
    assert!(manifest.body.exact().is_some());
}

#[tokio::test]
async fn resumed_goal_with_raised_budget_becomes_previewable_again() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    sync_goal_state_from_host(
        &engine.config.goal_state,
        Some("finish the release"),
        Some(100),
        GoalStatus::Active,
    );
    {
        let mut state = engine.config.goal_state.lock().expect("goal state");
        state.record_usage(100, 0, std::time::Instant::now());
        state
            .mark_paused(GoalPauseReason::BudgetLimit)
            .expect("pause goal");
    }
    sync_goal_state_from_host(
        &engine.config.goal_state,
        Some("finish the release"),
        Some(200),
        GoalStatus::Active,
    );

    let prompt = "continue under the raised budget";
    let planned = plan(&config, &identity, false, prompt).await;
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), prompt))
        .await;
    assert!(manifest.body.exact().is_some());
}

#[tokio::test]
async fn lowering_active_goal_budget_below_used_tokens_keeps_preview_open() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    sync_goal_state_from_host(
        &engine.config.goal_state,
        Some("finish the release"),
        Some(200),
        GoalStatus::Active,
    );
    engine
        .config
        .goal_state
        .lock()
        .expect("goal state")
        .record_usage(100, 0, std::time::Instant::now());
    sync_goal_state_from_host(
        &engine.config.goal_state,
        Some("finish the release"),
        Some(50),
        GoalStatus::Active,
    );

    let prompt = "continue after lowering the budget";
    let planned = plan(&config, &identity, false, prompt).await;
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), prompt))
        .await;
    assert!(manifest.route.exact().is_some());
    assert!(manifest.tools.exact().is_some());
    assert!(manifest.body.exact().is_some());
}

#[tokio::test]
async fn translation_prompt_context_matches_captured_first_production_body() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n"),
        )
        .mount(&server)
        .await;
    let mut config = deepseek_config();
    config
        .providers
        .as_mut()
        .expect("providers")
        .deepseek
        .base_url = Some(server.uri());
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = wire_preview_engine(&config);
    engine.config.translation_enabled = false;
    let planned = plan(&config, &identity, false, "/translate explain this").await;
    let _ = assert_preview_matches_first_wire_body(
        &mut engine,
        &server,
        planned,
        "/translate explain this",
        PreviewWireFixture {
            translation_enabled: true,
            verbosity: Some("concise".to_string()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn paused_detach_goal_context_matches_captured_first_production_body() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n"),
        )
        .mount(&server)
        .await;
    let mut config = deepseek_config();
    config
        .providers
        .as_mut()
        .expect("providers")
        .deepseek
        .base_url = Some(server.uri());
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = wire_preview_engine(&config);
    engine.config.goal_objective = Some("stale paused objective".to_string());
    sync_goal_state_from_host(
        &engine.config.goal_state,
        Some("stale paused objective"),
        None,
        GoalStatus::Active,
    );
    let prompt = "answer only this new question\n\nCodewhale paused custom slash command context:\nThe user is not resuming that paused command.";
    let planned = plan(&config, &identity, false, prompt).await;
    let (_, first_wire_body) = assert_preview_matches_first_wire_body(
        &mut engine,
        &server,
        planned,
        prompt,
        PreviewWireFixture::default(),
    )
    .await;
    assert!(
        !first_wire_body
            .to_string()
            .contains("stale paused objective"),
        "detached paused goal leaked onto the first wire body"
    );
}

#[tokio::test]
async fn anthropic_preview_matches_the_first_native_messages_wire_body() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"type\":\"message_stop\"}\n\n"),
        )
        .mount(&server)
        .await;
    let model = "claude-sonnet-4-6";
    let config = crate::config::Config {
        provider: Some("anthropic".to_string()),
        providers: Some(crate::config::ProvidersConfig {
            anthropic: crate::config::ProviderConfig {
                api_key: Some("test-anthropic-key".to_string()),
                base_url: Some(server.uri()),
                model: Some(model.to_string()),
                ..crate::config::ProviderConfig::default()
            },
            ..crate::config::ProvidersConfig::default()
        }),
        ..crate::config::Config::default()
    };
    let identity = config
        .builtin_provider_identity(ProviderKind::Anthropic)
        .unwrap();
    let (mut engine, _handle, _tmp) = wire_preview_engine(&config);
    let prompt = "inspect the native Messages payload";
    let planned = plan_for(
        &config,
        &identity,
        ProviderKind::Anthropic,
        model,
        false,
        prompt,
    )
    .await;
    let (_, first_wire_body) = assert_preview_matches_first_wire_body(
        &mut engine,
        &server,
        planned,
        prompt,
        PreviewWireFixture::default(),
    )
    .await;

    assert!(first_wire_body.get("system").is_some());
    assert!(first_wire_body.get("messages").is_some());
    assert!(first_wire_body.get("input").is_none());
}

// ---------------------------------------------------------------------
// #4707 — provider-free exact-route request/receipt matrix.
//
// The four route wire-truths are already pinned at the client boundary
// (`client.rs`, `client/chat.rs`). What was missing is the *join*: that the
// manifest a user reads from `/preview-request` describes the very bytes
// those routes put on the wire. Each case below runs the production
// planner, previews, then sends one turn through a local capture server
// with the semantic endpoint left exact, and asserts the manifest against
// the captured body — hash, sizes, route facts, and the requested→effective
// reasoning triple.
// ---------------------------------------------------------------------

/// One exact provider route in the matrix.
struct MatrixRoute {
    /// Test-facing name; also the failure-message prefix.
    name: &'static str,
    provider: ProviderKind,
    provider_key: &'static str,
    base_url: &'static str,
    model: &'static str,
    requested_reasoning: crate::reasoning_preference::ReasoningEffort,
    requested_reasoning_label: &'static str,
    /// Reasoning-control keys the manifest must report, in receipt order.
    expect_control_keys: &'static [&'static str],
    /// Effort actually on the wire — `None` when the route publishes a
    /// thinking toggle with no granularity. Never a fabricated tier.
    expect_wire_effort: Option<&'static str>,
    expect_wire_effort_source: Option<&'static str>,
    /// The output-cap key this route writes, and the one it must not.
    expect_output_cap_key: &'static str,
    expect_absent_output_cap_key: &'static str,
}

fn glm_5_2_zai_coding() -> MatrixRoute {
    MatrixRoute {
        name: "GLM-5.2 @ Z.ai coding",
        provider: ProviderKind::Zai,
        provider_key: "zai",
        base_url: crate::config::DEFAULT_ZAI_BASE_URL,
        model: crate::config::ZAI_GLM_5_2_MODEL,
        requested_reasoning: crate::reasoning_preference::ReasoningEffort::High,
        requested_reasoning_label: "high",
        expect_control_keys: &["reasoning_effort", "thinking"],
        expect_wire_effort: Some("high"),
        expect_wire_effort_source: Some("reasoning_effort"),
        expect_output_cap_key: "max_tokens",
        expect_absent_output_cap_key: "max_completion_tokens",
    }
}

fn glm_5_turbo_zai() -> MatrixRoute {
    MatrixRoute {
        name: "GLM-5-Turbo @ Z.ai",
        provider: ProviderKind::Zai,
        provider_key: "zai",
        base_url: crate::config::DEFAULT_ZAI_BASE_URL,
        model: crate::config::ZAI_GLM_5_TURBO_MODEL,
        requested_reasoning: crate::reasoning_preference::ReasoningEffort::High,
        requested_reasoning_label: "high",
        // No invented granularity: the toggle ships, the tier does not.
        expect_control_keys: &["thinking"],
        expect_wire_effort: None,
        expect_wire_effort_source: None,
        expect_output_cap_key: "max_tokens",
        expect_absent_output_cap_key: "max_completion_tokens",
    }
}

fn kimi_k3_moonshot_direct() -> MatrixRoute {
    MatrixRoute {
        name: "kimi-k3 @ api.moonshot.ai",
        provider: ProviderKind::Moonshot,
        provider_key: "moonshot",
        base_url: crate::config::DEFAULT_MOONSHOT_BASE_URL,
        model: crate::config::MOONSHOT_KIMI_K3_MODEL,
        // The visible normalization: `off` is not a tier this route has.
        requested_reasoning: crate::reasoning_preference::ReasoningEffort::Off,
        requested_reasoning_label: "off",
        expect_control_keys: &["reasoning_effort"],
        expect_wire_effort: Some("low"),
        expect_wire_effort_source: Some("reasoning_effort"),
        expect_output_cap_key: "max_completion_tokens",
        expect_absent_output_cap_key: "max_tokens",
    }
}

fn k3_kimi_code() -> MatrixRoute {
    MatrixRoute {
        name: "k3 @ api.kimi.com/coding/v1",
        provider: ProviderKind::Moonshot,
        provider_key: "moonshot",
        base_url: crate::config::DEFAULT_KIMI_CODE_BASE_URL,
        model: crate::config::KIMI_CODE_K3_MODEL,
        requested_reasoning: crate::reasoning_preference::ReasoningEffort::Off,
        requested_reasoning_label: "off",
        expect_control_keys: &["thinking"],
        expect_wire_effort: Some("low"),
        expect_wire_effort_source: Some("thinking.effort"),
        expect_output_cap_key: "max_tokens",
        expect_absent_output_cap_key: "max_completion_tokens",
    }
}

fn minimax_m3() -> MatrixRoute {
    MatrixRoute {
        name: "MiniMax-M3 @ api.minimax.io",
        provider: ProviderKind::Minimax,
        provider_key: "minimax",
        base_url: crate::config::DEFAULT_MINIMAX_BASE_URL,
        model: crate::config::DEFAULT_MINIMAX_MODEL,
        requested_reasoning: crate::reasoning_preference::ReasoningEffort::High,
        requested_reasoning_label: "high",
        expect_control_keys: &["thinking", "reasoning_split"],
        expect_wire_effort: None,
        expect_wire_effort_source: None,
        expect_output_cap_key: "max_completion_tokens",
        expect_absent_output_cap_key: "max_tokens",
    }
}

fn matrix_routes() -> Vec<MatrixRoute> {
    vec![
        glm_5_2_zai_coding(),
        glm_5_turbo_zai(),
        kimi_k3_moonshot_direct(),
        k3_kimi_code(),
        minimax_m3(),
    ]
}

fn matrix_config(route: &MatrixRoute) -> crate::config::Config {
    let entry = crate::config::ProviderConfig {
        api_key: Some(format!("sk-test-{}-matrix-key", route.provider_key)),
        base_url: Some(route.base_url.to_string()),
        model: Some(route.model.to_string()),
        ..crate::config::ProviderConfig::default()
    };
    let mut providers = crate::config::ProvidersConfig::default();
    match route.provider {
        ProviderKind::Zai => providers.zai = entry,
        ProviderKind::Moonshot => providers.moonshot = entry,
        ProviderKind::Minimax => providers.minimax = entry,
        other => panic!("{}: unhandled matrix provider {other:?}", route.name),
    }
    crate::config::Config {
        provider: Some(route.provider_key.to_string()),
        providers: Some(providers),
        ..crate::config::Config::default()
    }
}

fn matrix_identity(route: &MatrixRoute) -> crate::config::ProviderIdentity {
    matrix_config(route)
        .resolve_provider_pin_identity(route.provider_key)
        .unwrap()
}

/// Plan the exact route through production, then redirect only the
/// *transport* at the local capture server. The endpoint identity the
/// route shaper reads is untouched, so the captured body is the body
/// `api.z.ai` / `api.moonshot.ai` / `api.kimi.com` / `api.minimax.io`
/// would have received.
async fn matrix_planned_route(
    route: &MatrixRoute,
    config: &crate::config::Config,
    transport_base_url: Option<&str>,
    prompt: &str,
) -> crate::turn_route_plan::PlannedTurnRoute {
    let identity = matrix_identity(route);
    let mut planned = plan_with_reasoning(
        config,
        &identity,
        route.provider,
        route.model,
        false,
        route.requested_reasoning,
        prompt,
    )
    .await;
    if let Some(transport_base_url) = transport_base_url {
        let validated = planned
            .route
            .clone()
            .validate()
            .expect("the matrix route validates into a concrete client");
        let mut client = validated.client.clone();
        client.set_test_chat_transport_base_url(transport_base_url.to_string());
        planned.route = crate::route_runtime::ValidatedRuntimeRoute {
            client,
            ..validated
        }
        .into_resolved();
    }
    planned
}

/// Non-system messages on a Chat Completions body — the manifest counts
/// the system region separately, so `message_count` must exclude it.
fn non_system_message_count(body: &serde_json::Value) -> usize {
    body.get("messages")
        .and_then(serde_json::Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|message| {
                    message.get("role").and_then(serde_json::Value::as_str) != Some("system")
                })
                .count()
        })
        .unwrap_or_default()
}

async fn assert_matrix_route(route: &MatrixRoute) {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let name = route.name;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n"),
        )
        .mount(&server)
        .await;

    let config = matrix_config(route);
    let (mut engine, _handle, _tmp) = wire_preview_engine(&config);
    let prompt = "inspect the exact next request for this route";
    let uri = server.uri();
    let planned = matrix_planned_route(route, &config, Some(uri.as_str()), prompt).await;
    let planned_provider = planned.route.identity.provider;
    let planned_base_url = planned.route.candidate.endpoint().base_url.clone();
    let planned_model = planned.route.model.clone();
    let expected_wire_model = crate::config::wire_model_for_provider_route(
        planned_provider,
        &planned_base_url,
        &planned_model,
    );
    assert_eq!(
        planned_base_url.trim_end_matches('/'),
        route.base_url.trim_end_matches('/'),
        "{name}: the planner must keep the exact configured endpoint"
    );

    let (manifest, body) = assert_preview_matches_first_wire_body(
        &mut engine,
        &server,
        planned,
        prompt,
        PreviewWireFixture {
            requested_model: Some(route.model.to_string()),
            requested_reasoning: Some(route.requested_reasoning_label.to_string()),
            ..Default::default()
        },
    )
    .await;

    // --- Route facts -------------------------------------------------
    let facts = manifest
        .route
        .exact()
        .expect("a configured fixed route is exact");
    assert_eq!(facts.provider_id.as_str(), route.provider_key, "{name}");
    assert_eq!(facts.wire_model.as_str(), expected_wire_model, "{name}");
    assert_eq!(facts.dialect, "chat-completions", "{name}");
    assert_eq!(facts.routing_source, "active-fixed-route", "{name}");
    assert_eq!(
        body.get("model").and_then(serde_json::Value::as_str),
        Some(expected_wire_model.as_str()),
        "{name}: the manifest's wire model must be the model on the wire: {body}"
    );

    // --- Body identity, byte accounting, and size estimates ----------
    let facts_body = manifest.body.exact().expect("a prompted body is exact");
    let canonical = crate::client::canonical_json(&body);
    assert_eq!(
        facts_body.body_sha256,
        crate::hashing::sha256_hex(canonical.as_bytes()),
        "{name}: manifest body hash must equal the captured wire body hash"
    );
    assert_eq!(
        facts_body.body_canonical_json_bytes,
        canonical.len(),
        "{name}: canonical body size must describe the captured body"
    );
    assert_eq!(
        facts_body.system_canonical_json_bytes
            + facts_body.tool_schema_canonical_json_bytes
            + facts_body.message_canonical_json_bytes
            + facts_body.framing_canonical_json_bytes,
        facts_body.body_canonical_json_bytes,
        "{name}: the four accounting classes must sum to the body"
    );
    assert!(
        facts_body.system_canonical_json_bytes > 0,
        "{name}: this route sends a system region"
    );
    assert!(
        facts_body.tool_schema_canonical_json_bytes > 0,
        "{name}: this route sends tool schemas"
    );
    assert!(
        facts_body.message_canonical_json_bytes > 0,
        "{name}: this route sends messages"
    );
    assert_eq!(
        facts_body.message_count,
        non_system_message_count(&body),
        "{name}: message_count counts the non-system messages on the wire"
    );
    assert!(
        facts_body.tool_result_canonical_json_bytes <= facts_body.message_canonical_json_bytes,
        "{name}: tool results are a subset of messages"
    );
    assert!(
        facts_body.attachment_canonical_json_bytes <= facts_body.message_canonical_json_bytes,
        "{name}: attachments are a subset of messages"
    );
    assert_eq!(
        facts_body.tool_schema_wire_sha256,
        body.get("tools").map(|tools| {
            crate::hashing::sha256_hex(crate::client::canonical_json(tools).as_bytes())
        }),
        "{name}: the tool-schema digest must be over the schemas on the wire"
    );
    assert!(
        facts_body.estimates.system > 0 && facts_body.estimates.tool_schemas > 0,
        "{name}: per-class estimates are derived from the same wire regions"
    );
    assert!(
        facts_body.estimates.total_conservative > 0,
        "{name}: a whole-body estimate is available"
    );

    // --- Output cap: exactly the key this route writes ----------------
    let wire_cap = body
        .get(route.expect_output_cap_key)
        .and_then(serde_json::Value::as_u64);
    assert!(
        wire_cap.is_some(),
        "{name}: expected `{}` on the wire: {body}",
        route.expect_output_cap_key
    );
    assert!(
        body.get(route.expect_absent_output_cap_key).is_none(),
        "{name}: `{}` must not be on the wire: {body}",
        route.expect_absent_output_cap_key
    );
    assert_eq!(
        facts_body.wire_output_cap_tokens, wire_cap,
        "{name}: the reported output cap is the one literally on the wire"
    );

    // --- requested → effective reasoning ------------------------------
    assert_eq!(
        manifest.session.requested_model.as_str(),
        route.model,
        "{name}"
    );
    assert_eq!(
        manifest.session.requested_reasoning.as_str(),
        route.requested_reasoning_label,
        "{name}"
    );
    assert_eq!(
        facts_body.reasoning_resolution,
        ReasoningResolution::Explicit,
        "{name}: a fixed route with an explicitly requested tier"
    );
    assert_eq!(
        facts_body.reasoning_wire_control_keys, route.expect_control_keys,
        "{name}: reasoning-control keys, against the captured body {body}"
    );
    assert_eq!(
        facts_body
            .reasoning_wire_effort
            .as_ref()
            .map(|effort| effort.as_str()),
        route.expect_wire_effort,
        "{name}: wire effort, against the captured body {body}"
    );
    assert_eq!(
        facts_body.reasoning_wire_effort_source.as_deref(),
        route.expect_wire_effort_source,
        "{name}"
    );
    // Every reported control key is genuinely present on the wire, and the
    // reported effort is genuinely readable at the reported key path.
    for key in &facts_body.reasoning_wire_control_keys {
        assert!(
            body.get(key).is_some(),
            "{name}: reported control key `{key}` is not on the wire: {body}"
        );
    }
    match (
        facts_body.reasoning_wire_effort_source.as_deref(),
        route.expect_wire_effort,
    ) {
        (Some("reasoning_effort"), Some(effort)) => assert_eq!(
            body.get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some(effort),
            "{name}: {body}"
        ),
        (Some(path), Some(effort)) => {
            let pointer = format!("/{}", path.replace('.', "/"));
            assert_eq!(
                body.pointer(&pointer).and_then(serde_json::Value::as_str),
                Some(effort),
                "{name}: {body}"
            );
        }
        (None, None) => assert!(
            body.get("reasoning_effort").is_none(),
            "{name}: no effort was reported, so none may be on the wire: {body}"
        ),
        (source, effort) => panic!("{name}: inconsistent effort receipt {source:?}/{effort:?}"),
    }

    // --- Provider-authoritative usage ---------------------------------
    // A preview describes a request that has not been sent. Unknown stays
    // unknown; it never becomes a zero.
    assert!(
        matches!(
            &facts_body.provider_reported_usage,
            Availability::Unavailable(unavailable)
                if unavailable.reason == UnavailableReason::ProviderRequestNotExecuted
        ),
        "{name}: preview must not claim provider usage"
    );
    let json = manifest.to_json();
    assert!(
        !json.contains("\"input_tokens\""),
        "{name}: no fabricated usage counters reach the surface:\n{json}"
    );
}

#[tokio::test]
async fn matrix_glm_5_2_zai_coding_preview_matches_the_first_wire_body() {
    assert_matrix_route(&glm_5_2_zai_coding()).await;
}

#[tokio::test]
async fn matrix_glm_5_turbo_zai_preview_matches_the_first_wire_body() {
    assert_matrix_route(&glm_5_turbo_zai()).await;
}

#[tokio::test]
async fn matrix_kimi_k3_moonshot_direct_preview_matches_the_first_wire_body() {
    assert_matrix_route(&kimi_k3_moonshot_direct()).await;
}

#[tokio::test]
async fn matrix_k3_kimi_code_preview_matches_the_first_wire_body() {
    assert_matrix_route(&k3_kimi_code()).await;
}

#[tokio::test]
async fn matrix_minimax_m3_preview_matches_the_first_wire_body() {
    assert_matrix_route(&minimax_m3()).await;
}

/// The active tool-catalog hash is a *catalog identity*, not a wire fact:
/// the same catalog under the same posture must hash the same on every
/// route, however differently each dialect then shapes those schemas.
/// (Unit-level membership/order/schema sensitivity is pinned by
/// `active_catalog_hash_tracks_membership_order_and_schema`.)
#[tokio::test]
async fn matrix_routes_share_one_active_tool_catalog_hash() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let prompt = "describe the shared tool catalog";
    let mut observed: Vec<(&'static str, usize, String, String)> = Vec::new();

    for route in matrix_routes() {
        let config = matrix_config(&route);
        let (mut engine, _handle) = Engine::new(
            EngineConfig {
                workspace: workspace.path().to_path_buf(),
                max_steps: 1,
                snapshots_enabled: false,
                terminal_chrome_enabled: false,
                ..Default::default()
            },
            &config,
        );
        engine.config.features.disable(Feature::Mcp);
        engine.config.subagents_enabled = false;

        let planned = matrix_planned_route(&route, &config, None, prompt).await;
        let manifest = engine
            .build_request_manifest(inputs(false, Some(planned), prompt))
            .await;
        let tools = manifest
            .tools
            .exact()
            .expect("MCP is off, so the tool surface is exact");
        assert!(
            tools.standard_and_full_surfaces_collapsed,
            "{}: this fixture's catalog fits both budgets, so the surface \
                 budget label cannot change catalog membership",
            route.name
        );
        observed.push((
            route.name,
            tools.active_tool_count,
            tools.active_tool_catalog_sha256.clone(),
            tools.tool_surface_budget.clone(),
        ));
    }

    assert_eq!(observed.len(), 5, "every matrix route is represented");
    let (first_name, first_count, first_hash, _) = observed[0].clone();
    for (name, count, hash, _) in &observed {
        assert_eq!(
            *count, first_count,
            "{name} vs {first_name}: the matrix fixture holds the tool surface constant"
        );
        assert_eq!(
            hash, &first_hash,
            "{name} vs {first_name}: one catalog must hash to one identity across routes"
        );
    }

    // …and the routes really are distinct in capability posture: the shared
    // hash is a genuine cross-route agreement, not five copies of one
    // route. GLM-5.2 publishes a `Full` tool surface budget while
    // GLM-5-Turbo publishes `Standard`, and the catalog identity is
    // unchanged by that difference.
    let budgets: std::collections::BTreeSet<&str> = observed
        .iter()
        .map(|(_, _, _, budget)| budget.as_str())
        .collect();
    assert!(
        budgets.len() > 1,
        "the matrix spans routes with different surface budgets: {budgets:?}"
    );
}

/// Provider-authoritative usage is never a preview fact, and it is never a
/// zero standing in for "not measured". It becomes knowable only when a
/// response reports it, through the same `parse_usage` seam the turn loop
/// uses.
#[tokio::test]
async fn provider_reported_usage_is_unavailable_until_a_response_reports_it() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let usage = json!({"prompt_tokens": 137, "completion_tokens": 24, "total_tokens": 161});
    let stream = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{"index": 0, "delta": {"content": "ok"}}],
        }),
        json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": usage,
        })
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream),
        )
        .mount(&server)
        .await;

    let mut config = deepseek_config();
    config
        .providers
        .as_mut()
        .expect("providers")
        .deepseek
        .base_url = Some(server.uri());
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = wire_preview_engine(&config);

    let prompt = "count the tokens this turn will report";
    let planned = plan(&config, &identity, false, prompt).await;
    let production_route = planned.route.clone();
    let compaction = planned.compaction.clone();
    let reasoning_effort = planned.effective_reasoning_effort.clone();
    let reasoning_effort_auto = planned.auto_controls_reasoning;

    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), prompt))
        .await;
    let body = manifest.body.exact().expect("a prompted body is exact");
    assert!(
        matches!(
            &body.provider_reported_usage,
            Availability::Unavailable(unavailable)
                if unavailable.reason == UnavailableReason::ProviderRequestNotExecuted
        ),
        "no request occurred, so there is nothing the provider reported"
    );
    assert_eq!(
        engine.session.total_usage.input_tokens, 0,
        "and nothing has been recorded yet"
    );
    assert_eq!(engine.session.total_usage.output_tokens, 0);

    let _ = engine
        .handle_send_message(TurnSpec {
            profile_constitution: None,
            content: prompt.to_string(),
            mode: AppMode::Agent,
            route: Box::new(production_route),
            compaction: Box::new(compaction),
            initial_routed_usage: Box::new(crate::cost_status::RuntimeUsageBatch::default()),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: GoalStatus::Active,
            reasoning_effort,
            reasoning_effort_auto,
            auto_model: false,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            images: Vec::new(),
            max_output_tokens: None,
            submission_id: None,
        })
        .await;

    // The completed turn's counts are exactly what `parse_usage` reads off
    // the reported usage object — no rounding, no substituted estimate.
    let parsed = crate::client::parse_usage(Some(&usage));
    assert_eq!(u64::from(parsed.input_tokens), 137);
    assert_eq!(u64::from(parsed.output_tokens), 24);
    assert_eq!(
        (
            engine.session.total_usage.input_tokens,
            engine.session.total_usage.output_tokens
        ),
        (
            u64::from(parsed.input_tokens),
            u64::from(parsed.output_tokens)
        ),
        "the turn records the provider-authoritative counts, not an estimate"
    );
    let reported = crate::request_manifest::ProviderReportedUsage {
        input_tokens: engine.session.total_usage.input_tokens,
        output_tokens: engine.session.total_usage.output_tokens,
    };
    assert_eq!(reported.input_tokens, 137);
    assert_eq!(reported.output_tokens, 24);
}

/// A fixed route with a hypothetical prompt describes the next turn
/// exactly: route, tools, and body are all published, and the prompt is
/// part of the hashed body.
#[tokio::test]
async fn fixed_route_with_a_prompt_describes_the_exact_next_turn() {
    let mut config = deepseek_config();
    config
        .providers
        .as_mut()
        .expect("providers")
        .deepseek
        .context_window = Some(123_456);
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    engine.active_route_limits = Some(codewhale_config::route::RouteLimits {
        context_tokens: Some(4_096),
        input_tokens: Some(3_000),
        output_tokens: Some(512),
        output_declared: false,
    });

    let planned = plan(&config, &identity, false, "refactor the parser").await;
    let planned_limits = crate::route_budget::known_route_limits(planned.route.candidate.limits());
    let expected_input_budget = context_input_budget_for_route(
        planned.route.identity.provider,
        &planned.route.model,
        planned_limits,
        0,
    );
    let expected_wire_output = crate::route_budget::effective_max_output_tokens_for_route(
        planned.route.identity.provider,
        &planned.route.model,
        planned_limits,
    );
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), "refactor the parser"))
        .await;

    let route = manifest.route.exact().expect("a fixed route is exact");
    assert_eq!(route.provider_id.as_str(), "deepseek");
    assert_eq!(route.routing_source, "active-fixed-route");
    assert_eq!(route.dialect, "chat-completions");
    assert_eq!(route.caller_entrypoint, "streaming");
    assert_eq!(route.body_stream_field, Some(true));
    assert_eq!(route.context_limit_tokens, 123_456);
    assert_eq!(
        route.context_limit_source,
        crate::route_runtime::ContextWindowSource::Configured
    );
    assert_eq!(
        route.route_input_limit_tokens,
        planned_limits.and_then(|limits| limits.input_tokens)
    );
    assert_eq!(
        route.route_output_limit_tokens,
        planned_limits.and_then(|limits| limits.output_tokens)
    );
    assert!(!route.wire_model.is_redacted());
    assert!(
        manifest.tools.exact().is_some(),
        "MCP is off in this engine"
    );

    let body = manifest.body.exact().expect("a prompted body is exact");
    assert_eq!(body.input_budget_ceiling_tokens, expected_input_budget);
    assert_eq!(
        body.wire_output_cap_tokens,
        Some(u64::from(expected_wire_output))
    );
    assert_eq!(body.body_sha256.len(), 64);
    assert!(
        body.message_count >= 1,
        "the hypothetical prompt is a message"
    );
    assert!(body.local_system_tools_component_sha256.is_some());
    assert!(manifest.session.hypothetical_prompt_supplied);

    // The prompt is genuinely part of the request being described.
    let other_planned = plan(&config, &identity, false, "write the release notes").await;
    let other = engine
        .build_request_manifest(inputs(
            false,
            Some(other_planned),
            "write the release notes",
        ))
        .await;
    assert_ne!(
        body.body_sha256,
        other.body.exact().expect("exact").body_sha256,
        "a different next prompt must produce a different body hash"
    );
}

/// The engine can describe an Auto route receipt supplied by a trusted
/// host without consulting installed state. The human preview command
/// deliberately never obtains such a receipt, because doing so would call
/// the provider-backed classifier.
#[tokio::test]
async fn host_supplied_auto_route_receipt_matches_the_production_planner() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);

    let planned = plan(&config, &identity, true, "explain this stack trace").await;
    let planned_provider = planned.effective_provider;
    let planned_identity = planned.effective_provider_identity.clone();
    let planned_model = planned.route.model.clone();
    let planned_base_url = planned.route.candidate.endpoint().base_url.clone();
    assert!(
        planned.auto_controls_reasoning,
        "the helper requests auto reasoning for its auto-model fixture"
    );

    let manifest = engine
        .build_request_manifest(inputs(true, Some(planned), "explain this stack trace"))
        .await;

    let route = manifest
        .route
        .exact()
        .expect("auto + prompt resolves a route");
    assert_eq!(route.provider_id.as_str(), planned_provider.as_str());
    assert_eq!(route.routing_source, "auto-provider-classifier");
    assert_eq!(planned_identity, "deepseek");
    assert_eq!(
        route.wire_model.as_str(),
        crate::config::wire_model_for_provider_route(
            planned_provider,
            &planned_base_url,
            &planned_model,
        ),
        "the wire model is the planner's model after route remapping — not \
             the model the session happens to have installed"
    );
    assert_eq!(
        manifest.session.requested_model.as_str(),
        "auto",
        "the manifest never reports the resolved model as the user's selection"
    );

    let body = match &manifest.body {
        Availability::Exact(body) => body,
        Availability::Unavailable(unavailable) => {
            panic!("auto + prompt should have an exact body: {unavailable:?}")
        }
    };
    assert_ne!(
        body.reasoning_resolution,
        ReasoningResolution::Explicit,
        "an auto-routed turn never claims an explicit user tier"
    );

    // The hypothetical prompt is part of the hashed body on the auto path
    // too, not only on the fixed one.
    let other = plan(&config, &identity, true, "rename one local variable").await;
    let other = engine
        .build_request_manifest(inputs(true, Some(other), "rename one local variable"))
        .await;
    assert_ne!(
        body.body_sha256,
        other.body.exact().expect("exact").body_sha256
    );
}

/// The passive path must not create an MCP pool, connect a server, or
/// emit a UI event — it reports the tool surface unavailable instead.
#[tokio::test]
async fn preview_tool_snapshot_has_no_mcp_or_event_side_effects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = crate::config::Config {
        provider: Some("deepseek".to_string()),
        ..crate::config::Config::default()
    };
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            ..Default::default()
        },
        &config,
    );
    let _ = engine.config.features.enable(Feature::Mcp);

    let policy = TurnAuthority::from_effective_fields(
        AppMode::Agent,
        false,
        false,
        false,
        ApprovalMode::Suggest,
    );
    let build = engine
        .build_turn_tool_registry_and_catalog(
            &policy,
            &[],
            None,
            SubAgentWiring::Inert,
            McpAccess::PassiveSnapshot,
            TurnRouteContext {
                provider: engine.api_provider,
                model: engine.session.model.clone(),
                capabilities: engine.active_route_capabilities,
                limits: engine.active_route_limits,
                client: engine.codewhale_client.clone(),
                api_config: Box::new(engine.api_config.clone()),
                locale_tag: engine.config.locale_tag.clone(),
                role_models: engine.subagent_role_models(),
                auto_model: false,
                reasoning_effort: None,
                reasoning_effort_auto: false,
            },
            "",
            None,
        )
        .await;

    assert!(
        engine.mcp_pool.is_none(),
        "a passive snapshot must not create the MCP pool"
    );
    assert!(
        matches!(build.mcp, McpToolState::Unavailable { .. }),
        "with no connected pool the MCP tool state is unavailable, not empty"
    );
    assert!(build.mcp.server_count().is_none());
    drop(handle);
}

/// The reviewed blocker: with MCP enabled but nothing connected, the
/// preview built a catalog with zero MCP tools, prepared a body from it,
/// and published that body as `Exact` — a hash of a request no turn would
/// ever send. The body must inherit the tool surface's typed reason.
#[tokio::test]
async fn unavailable_mcp_state_makes_the_body_unavailable_too() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    // MCP on, pool never started: a real turn would connect and could
    // discover tools this catalog does not contain.
    let _ = engine.config.features.enable(Feature::Mcp);

    let planned = plan(&config, &identity, false, "refactor the parser").await;
    let manifest = engine
        .build_request_manifest(inputs(false, Some(planned), "refactor the parser"))
        .await;

    assert!(
        manifest.tools.exact().is_none(),
        "an unconnected MCP pool is not a snapshottable tool surface"
    );
    assert!(
        manifest.body.exact().is_none(),
        "a body built from a tool surface missing its MCP contribution \
             must not be published as exact"
    );
    assert!(
        manifest.route.exact().is_some(),
        "the route does not depend on the MCP contribution and stays exact"
    );

    // No body fact — hash, byte count, or local component fingerprint —
    // reaches either surface.
    let json = manifest.to_json();
    for forbidden in [
        "body_sha256",
        "local_system_tools_component_sha256",
        "tool_schema_wire_sha256",
        "body_canonical_json_bytes",
        "estimated_input_headroom_tokens",
    ] {
        assert!(!json.contains(forbidden), "{forbidden} leaked:\n{json}");
    }
    assert!(json.contains("mcp-state-not-snapshottable"), "{json}");
    assert!(engine.mcp_pool.is_none(), "no pool was created by looking");
}

/// A preview is an inspection. Every piece of engine state a turn would
/// have written must be byte-identical afterwards — including the ones the
/// earlier implementation wrote and restored around an `.await`.
#[tokio::test]
async fn building_a_manifest_writes_no_engine_state() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    engine.config.allowed_tools = Some(vec!["Bash".to_string()]);
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "an earlier turn".to_string(),
            cache_control: None,
        }],
    });

    let allowed_before = engine.config.allowed_tools.clone();
    let disallowed_before = engine.config.disallowed_tools.clone();
    let messages_before = engine.messages_with_turn_metadata();
    let model_before = engine.session.model.clone();
    let system_prompt_before = system_prompt_hash(engine.session.system_prompt.as_ref());
    let system_hash_before = engine.session.last_system_prompt_hash;
    let working_set_before = engine
        .session
        .working_set
        .summary_block(&engine.config.workspace);
    let provider_before = engine.api_provider;
    let mode_before = engine.current_mode;
    let narrowing_before = format!("{:?}", engine.last_policy_narrowing);
    let turn_counter_before = engine.turn_counter;

    // A *different* command-scoped gate than the installed one, and a
    // prompt that mentions a path so the working set would move if the
    // preview observed it on the session rather than on a clone.
    let mut preview_inputs = inputs(
        false,
        Some(plan(&config, &identity, false, "inspect src/lib.rs").await),
        "inspect src/lib.rs",
    );
    preview_inputs.allowed_tools = Some(vec!["Read".to_string()]);
    let manifest = engine.build_request_manifest(preview_inputs).await;
    assert!(manifest.body.exact().is_some(), "fixture should be exact");

    assert_eq!(engine.config.allowed_tools, allowed_before, "tool gate");
    assert_eq!(engine.config.disallowed_tools, disallowed_before);
    assert_eq!(
        engine.messages_with_turn_metadata(),
        messages_before,
        "history"
    );
    assert_eq!(engine.session.model, model_before);
    assert_eq!(
        system_prompt_hash(engine.session.system_prompt.as_ref()),
        system_prompt_before
    );
    assert_eq!(engine.session.last_system_prompt_hash, system_hash_before);
    assert_eq!(
        engine
            .session
            .working_set
            .summary_block(&engine.config.workspace),
        working_set_before,
        "the hypothetical message is observed on a clone, never on the session"
    );
    assert_eq!(engine.api_provider, provider_before);
    assert_eq!(engine.current_mode, mode_before);
    assert_eq!(
        format!("{:?}", engine.last_policy_narrowing),
        narrowing_before
    );
    assert_eq!(engine.turn_counter, turn_counter_before);
    assert!(engine.mcp_pool.is_none());
}

/// The gate is a parameter, so it shapes the previewed catalog without
/// ever being installed.
#[tokio::test]
async fn the_previewed_tool_gate_applies_without_being_installed() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);

    let wide = engine
        .build_request_manifest(inputs(
            false,
            Some(plan(&config, &identity, false, "do the thing").await),
            "do the thing",
        ))
        .await;

    let mut narrow_inputs = inputs(
        false,
        Some(plan(&config, &identity, false, "do the thing").await),
        "do the thing",
    );
    narrow_inputs.allowed_tools = Some(vec!["Read".to_string()]);
    let narrow = engine.build_request_manifest(narrow_inputs).await;

    let wide_tools = wide.tools.exact().expect("exact");
    let narrow_tools = narrow.tools.exact().expect("exact");
    assert!(
        narrow_tools.active_tool_count < wide_tools.active_tool_count,
        "the passed gate must narrow the previewed catalog: {} vs {}",
        narrow_tools.active_tool_count,
        wide_tools.active_tool_count
    );
    assert_eq!(
        narrow.session.allowed_tool_gate_count,
        Some(1),
        "and the session section reports the gate that was previewed"
    );
    assert_eq!(
        engine.config.allowed_tools, None,
        "…while the engine keeps its own"
    );
}

/// A plan failure still happened *because of* a supplied prompt. Reporting
/// otherwise tells the user to pass the flag they just passed.
#[tokio::test]
async fn a_failed_plan_still_reports_that_a_prompt_was_supplied() {
    let (mut engine, _handle, _tmp) = preview_engine(&crate::config::Config::default());
    let mut failed = inputs(false, None, "");
    failed.unresolved = PreviewUnresolved::PlanFailed(
        "no API key configured for route 'my-gateway' at /home/someone/.config".to_string(),
    );

    let manifest = engine.build_request_manifest(failed).await;
    assert!(manifest.session.hypothetical_prompt_supplied);
    assert!(manifest.route.exact().is_none());

    let rendered = manifest.render();
    assert!(
        !rendered.contains("Pass `--prompt <text>`"),
        "the user already did:\n{rendered}"
    );
    // …and the raw host text never reaches a surface verbatim.
    for surface in [rendered, manifest.to_json()] {
        assert!(!surface.contains("my-gateway'"), "{surface}");
        assert!(!surface.contains("/home/someone"), "{surface}");
    }
}

/// Pending runtime injections are *counted*, never consumed, and they make
/// the body unavailable rather than silently absent from it.
#[tokio::test]
async fn pending_runtime_injections_make_the_body_unavailable_without_consuming_them() {
    let config = deepseek_config();
    let identity = deepseek_identity();
    let (mut engine, _handle, _tmp) = preview_engine(&config);
    engine.config.features.disable(Feature::Mcp);
    engine.pending_lsp_blocks.push(crate::lsp::DiagnosticBlock {
        file: std::path::PathBuf::from("src/lib.rs"),
        items: Vec::new(),
    });

    let manifest = engine
        .build_request_manifest(inputs(
            false,
            Some(plan(&config, &identity, false, "fix it").await),
            "fix it",
        ))
        .await;

    assert!(
        manifest.body.exact().is_none(),
        "the turn loop would inject diagnostics before the first request"
    );
    assert!(manifest.route.exact().is_some());
    assert_eq!(
        engine.pending_lsp_blocks.len(),
        1,
        "inspecting must not flush the pending blocks"
    );
    assert!(
        manifest
            .to_json()
            .contains("runtime-transforms-before-send"),
        "{}",
        manifest.to_json()
    );
}
