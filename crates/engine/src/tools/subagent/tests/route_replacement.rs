//! Operator-approved route replacement at the first-request seam.
//!
//! The observed failure: a saved reviewer pin answered its first request with
//! `Provider plan quota exhausted: You have run out of credits or need a Grok
//! subscription.` and no review was produced.
use super::*;

const REFUSAL: &str = "You have run out of credits or need a Grok subscription.";

/// A provider fixture that refuses every request with HTTP 403.
async fn refusing_chat_server() -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/{*path}",
        post({
            let calls = Arc::clone(&calls);
            move |Json(_body): Json<Value>| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::FORBIDDEN,
                        Json(json!({"error": {"message": REFUSAL}})),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind refusing server");
    let addr = listener.local_addr().expect("refusing server addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), calls)
}

async fn write_replacement_config(
    path: &std::path::Path,
    pin_url: &str,
    backup_url: &str,
    pin: &str,
) {
    tokio::fs::write(
        path,
        format!(
            r#"
provider = "deepseek"
model = "deepseek-v4-flash"
api_key = "fixture-key"
base_url = "{backup_url}"

[retry]
enabled = false
max_retries = 0

[providers.PinRoute]
kind = "openai-compatible"
api_key = "fixture-pin-key"
base_url = "{pin_url}"
model = "fixture-pin-model"

[providers.BackupRoute]
kind = "openai-compatible"
api_key = "fixture-backup-key"
base_url = "{backup_url}"
model = "fixture-backup-model"

{pin}
"#
        ),
    )
    .await
    .unwrap();
}

async fn reviewer_tool(
    root: &std::path::Path,
    pin: &str,
) -> (
    AgentTool,
    ToolContext,
    SharedSubAgentManager,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    reviewer_tool_with(root, pin, false).await
}

/// [`reviewer_tool`], optionally with the parent route refusing as well.
async fn reviewer_tool_with(
    root: &std::path::Path,
    pin: &str,
    parent_refuses: bool,
) -> (
    AgentTool,
    ToolContext,
    SharedSubAgentManager,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let (backup, backup_calls, _, _) = delayed_chat_client(Duration::ZERO, "review done").await;
    let (pin_url, pin_calls) = refusing_chat_server().await;
    let config_path = root.join("config.toml");
    let parent_url = if parent_refuses {
        pin_url.as_str()
    } else {
        backup.base_url()
    };
    write_replacement_config(&config_path, &pin_url, parent_url, pin).await;
    let config = codewhale_plugins::providers::load_config(Some(config_path), None).unwrap();
    let client = CodewhaleClient::new(&config).unwrap();
    let manager = new_shared_subagent_manager(root.to_path_buf(), 2);
    let context = ToolContext::new(root).with_state_namespace("route-replacement");
    let runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".into(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_api_config(config);
    (
        AgentTool::new(manager.clone(), runtime),
        context,
        manager,
        pin_calls,
        backup_calls,
    )
}

async fn run_reviewer(pin: &str) -> (Value, SubAgentResult, usize, usize) {
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "deepseek");
    let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "deepseek-v4-flash");
    let (tool, context, manager, pin_calls, backup_calls) = reviewer_tool(root.path(), pin).await;
    let started = tool
        .execute(
            json!({"type": "reviewer", "prompt": "Review the change."}),
            &context,
        )
        .await
        .unwrap();
    let meta = started.metadata.clone().unwrap();
    let id = meta["agent_id"].as_str().unwrap().to_string();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = manager.read().await.get_result(&id).expect("registered");
            if result.status != SubAgentStatus::Running {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child settles");
    (
        meta,
        result,
        pin_calls.load(Ordering::SeqCst),
        backup_calls.load(Ordering::SeqCst),
    )
}

#[tokio::test]
async fn approved_replacement_takes_a_refused_first_request_and_is_receipted() {
    let _env = crate::test_support::lock_test_env();
    let (meta, result, pin_calls, backup_calls) = run_reviewer(
        r#"
[subagents.roles.reviewer]
model = "PinRoute/fixture-pin-model"
replacements = ["BackupRoute/fixture-backup-model"]
"#,
    )
    .await;
    assert_eq!(meta["child_route"]["provider_id"], "PinRoute");
    assert_eq!(pin_calls, 1, "the refused route is asked exactly once");
    assert!(backup_calls >= 1, "the approved route took the request");
    assert_eq!(
        result.status,
        SubAgentStatus::Completed,
        "{:?}",
        result.status
    );
    assert_eq!(result.result.as_deref(), Some("review done"));
    let route = result.child_route.expect("route receipt");
    assert_eq!(route.provider_id, "BackupRoute");
    assert_eq!(route.model_id, "fixture-backup-model");
    assert_eq!(route.route_source, "role.replacement");
    let note = route.fallback_note.expect("replacement note");
    for fact in [
        "fixture-pin-model",
        "quota exhausted",
        "run out of credits",
        "BackupRoute/fixture-backup-model",
        "attempt 1 of 1",
    ] {
        assert!(note.contains(fact), "{fact} missing from {note}");
    }
    assert!(!note.contains("fixture-backup-key") && !note.contains("fixture-pin-key"));
}

#[tokio::test]
async fn a_pin_without_approved_replacements_stays_exact() {
    let _env = crate::test_support::lock_test_env();
    let (_meta, result, pin_calls, backup_calls) = run_reviewer(
        r#"
[subagents.roles.reviewer]
model = "PinRoute/fixture-pin-model"
"#,
    )
    .await;
    assert_eq!(pin_calls, 1);
    assert_eq!(backup_calls, 0, "no provider was asked without approval");
    let SubAgentStatus::Failed(error) = &result.status else {
        panic!("an exact refused pin fails: {:?}", result.status);
    };
    for fact in [
        "quota exhausted",
        "run out of credits",
        "requested model `fixture-pin-model`",
        "config.toml role pin for \"reviewer\" routes PinRoute/fixture-pin-model",
    ] {
        assert!(error.contains(fact), "{fact} missing from {error}");
    }
    assert!(!error.contains("fixture-pin-key") && !error.contains("fixture-backup-key"));
    assert_eq!(result.child_route.unwrap().provider_id, "PinRoute");
}

#[test]
fn replacement_reasons_are_typed_never_message_matched() {
    let refusal = |status| anyhow::Error::new(LlmError::from_http_response(status, REFUSAL));
    // A 403 whose body is quota evidence is a typed quota refusal.
    assert_eq!(
        route_replacement_reason(&refusal(403)),
        Some("quota exhausted")
    );
    // A 403 without quota evidence is still an authorization refusal.
    assert_eq!(
        route_replacement_reason(&anyhow::Error::new(LlmError::from_http_response(
            403,
            "Forbidden"
        ))),
        Some("provider refused authorization")
    );
    assert_eq!(
        route_replacement_reason(&refusal(401)),
        Some("credentials rejected")
    );
    // A credits-themed message without typed evidence is not a route refusal.
    assert_eq!(route_replacement_reason(&anyhow!(REFUSAL)), None);
    assert_eq!(
        route_replacement_reason(&anyhow::Error::new(LlmError::ContentPolicyError(
            "blocked".into()
        ))),
        None,
        "content refusals are never shopped to another provider"
    );
    assert_eq!(
        route_replacement_reason(&anyhow::Error::new(LlmError::ContextLengthError(
            "too long".into()
        ))),
        None
    );
}

#[test]
fn replacements_must_name_their_provider() {
    let config: codewhale_runtime::config::Config = toml::from_str(
        r#"
[subagents.roles.reviewer]
model = "deepseek-v4-pro"
replacements = ["deepseek/deepseek-v4-flash", "bare-model"]
"#,
    )
    .unwrap();
    let routes = config.subagent_route_replacements("reviewer");
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].provider.as_deref(), Some("deepseek"));
    assert_eq!(routes[1].provider, None);
    assert!(config.subagent_route_replacements("builder").is_empty());
}

#[tokio::test]
async fn a_replacement_without_an_explicit_provider_fails_at_spawn() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let (tool, context, manager, pin_calls, backup_calls) = reviewer_tool(
        root.path(),
        r#"
[subagents.roles.reviewer]
model = "PinRoute/fixture-pin-model"
replacements = ["fixture-backup-model"]
"#,
    )
    .await;
    let error = tool
        .execute(json!({"type": "reviewer", "prompt": "Review."}), &context)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("provider/model"), "{error}");
    assert!(manager.read().await.agents.is_empty());
    assert_eq!(
        pin_calls.load(Ordering::SeqCst) + backup_calls.load(Ordering::SeqCst),
        0
    );
}

/// The founder's shape: a saved agent profile (not a config pin) routes the
/// reviewer role to a provider whose account refuses authorization, while
/// the parent route works.
async fn write_refusing_reviewer_profile(root: &std::path::Path) {
    let dir = root.join(".codewhale/agents");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(
        dir.join("reviewer.toml"),
        "id = \"reviewer\"\ndisplay_name = \"reviewer\"\nprovider = \"PinRoute\"\nmodel = \"fixture-pin-model\"\nrole_hint = \"reviewer\"\n",
    )
    .await
    .unwrap();
}

struct ProjectProfiles(bool);
impl ProjectProfiles {
    fn enabled() -> Self {
        let previous = crate::fleet::roster::project_agent_profiles_enabled();
        crate::fleet::roster::set_project_agent_profiles_enabled(true);
        Self(previous)
    }
}
impl Drop for ProjectProfiles {
    fn drop(&mut self) {
        crate::fleet::roster::set_project_agent_profiles_enabled(self.0);
    }
}

async fn settle(manager: &SharedSubAgentManager, id: &str) -> SubAgentResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = manager.read().await.get_result(id).expect("registered");
            if result.status != SubAgentStatus::Running {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child settles")
}

#[tokio::test]
async fn refused_saved_profile_pin_runs_on_the_parent_route_visibly_and_once() {
    let _env = crate::test_support::lock_test_env();
    let _profiles = ProjectProfiles::enabled();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "deepseek");
    let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "deepseek-v4-flash");
    write_refusing_reviewer_profile(root.path()).await;
    let (tool, context, manager, pin_calls, backup_calls) = reviewer_tool(root.path(), "").await;

    // First worker: the pin is asked once, refuses, and the same request runs
    // on the parent route with the reason on the receipt.
    let started = tool
        .execute(
            json!({"type": "reviewer", "prompt": "Review the change."}),
            &context,
        )
        .await
        .unwrap();
    let meta = started.metadata.clone().unwrap();
    assert_eq!(meta["child_route"]["route_source"], "agent_profile.model");
    assert_eq!(meta["child_route"]["provider_id"], "PinRoute");
    let first = settle(&manager, meta["agent_id"].as_str().unwrap()).await;
    assert_eq!(
        first.status,
        SubAgentStatus::Completed,
        "{:?}",
        first.status
    );
    assert_eq!(first.result.as_deref(), Some("review done"));
    assert_eq!(pin_calls.load(Ordering::SeqCst), 1, "the pin is asked once");
    assert!(
        backup_calls.load(Ordering::SeqCst) >= 1,
        "the parent route ran it"
    );
    let route = first.child_route.expect("route receipt");
    assert_eq!(route.route_source, "session.fallback");
    assert_eq!(route.provider_id, "deepseek");
    assert_eq!(route.model_id, "deepseek-v4-flash");
    let note = route.fallback_note.expect("fallback note");
    for fact in [
        "saved agent profile \"reviewer\" pins PinRoute/fixture-pin-model",
        "failed authorization",
        "run out of credits",
        "ran on deepseek/deepseek-v4-flash instead",
    ] {
        assert!(note.contains(fact), "{fact} missing from {note}");
    }
    assert!(!note.contains("fixture-pin-key") && !note.contains("fixture-backup-key"));

    // Second worker in the same session goes straight to the parent route:
    // the known-bad pin is not asked again.
    let started = tool
        .execute(
            json!({"type": "reviewer", "prompt": "Review the other change."}),
            &context,
        )
        .await
        .unwrap();
    let meta = started.metadata.clone().unwrap();
    assert_eq!(meta["child_route"]["route_source"], "session.fallback");
    assert_eq!(meta["child_route"]["provider_id"], "deepseek");
    let note = meta["child_route"]["fallback_note"].as_str().unwrap();
    assert!(note.contains("earlier this session"), "{note}");
    assert!(note.contains("PinRoute/fixture-pin-model"), "{note}");
    let second = settle(&manager, meta["agent_id"].as_str().unwrap()).await;
    assert_eq!(
        second.status,
        SubAgentStatus::Completed,
        "{:?}",
        second.status
    );
    assert_eq!(pin_calls.load(Ordering::SeqCst), 1, "no second refusal");
}

#[tokio::test]
async fn strict_saved_profile_pin_stays_exact_and_names_its_source() {
    let _env = crate::test_support::lock_test_env();
    let _profiles = ProjectProfiles::enabled();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    write_refusing_reviewer_profile(root.path()).await;
    let (tool, _context, _manager, _pin, _backup) = reviewer_tool(root.path(), "").await;
    let runtime = tool.runtime.clone();
    let bind = |allow_fallback: bool| {
        let runtime = runtime.clone();
        async move {
            let roster = spawn_roster(&runtime);
            let mut request =
                parse_spawn_request(&json!({"type": "reviewer", "prompt": "Review."})).unwrap();
            let member = resolve_spawn_route_profile(&runtime, &mut request, &roster).unwrap();
            let mut child = runtime.child_runtime();
            bind_spawn_model_route(&mut child, &request, member.as_ref(), true, allow_fallback)
                .await
                .unwrap();
            child
        }
    };

    let fallback = bind(true).await;
    let origin = fallback.route_origin.as_deref().expect("origin");
    assert_eq!(
        origin.parent.as_ref().map(|parent| parent.label.as_str()),
        Some("deepseek/deepseek-v4-flash")
    );

    // An exact Fleet binding forbids the substitution: no parent route is
    // armed, and the failure names the saved profile and how to change it.
    let strict = bind(false).await;
    let origin = strict.route_origin.as_deref().expect("origin");
    assert!(origin.parent.is_none());
    let refusal = anyhow::Error::new(LlmError::from_http_response(403, REFUSAL));
    let message = annotate_child_model_error_with_origin(
        &subagent_failure_message(&refusal),
        &strict.model,
        strict.client.api_provider(),
        &ModelRoute::Fixed(strict.model.clone()),
        Some(origin),
    );
    for fact in [
        "run out of credits",
        "saved agent profile \"reviewer\" pins PinRoute/fixture-pin-model",
        "/fleet members",
        "reviewer.toml",
        "forbids falling back to the parent route",
    ] {
        assert!(message.contains(fact), "{fact} missing from {message}");
    }
    assert!(
        !message.contains("explicit child model override"),
        "no override was given: {message}"
    );
}

#[test]
fn only_typed_auth_and_credit_refusals_move_a_saved_pin() {
    let refusal = |status| anyhow::Error::new(LlmError::from_http_response(status, REFUSAL));
    assert!(pin_refusal_reason(&refusal(403)).is_some());
    assert!(pin_refusal_reason(&refusal(401)).is_some());
    // Transient failures keep the ordinary retry path; no route change.
    for transient in [
        LlmError::RateLimited {
            message: "slow down".into(),
            retry_after: None,
        },
        LlmError::NetworkError("connection reset".into()),
        LlmError::ServerError {
            status: 503,
            message: "unavailable".into(),
        },
        LlmError::ModelError("no such model".into()),
    ] {
        assert_eq!(pin_refusal_reason(&anyhow::Error::new(transient)), None);
    }
    assert_eq!(pin_refusal_reason(&anyhow!(REFUSAL)), None);
}

/// Finding on #6717: after the fallback, a failure on the parent route was
/// still reported against the refused pin ("change it in /fleet members").
#[tokio::test]
async fn a_failure_after_the_fallback_names_the_route_that_failed() {
    let _env = crate::test_support::lock_test_env();
    let _profiles = ProjectProfiles::enabled();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "deepseek");
    let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "deepseek-v4-flash");
    write_refusing_reviewer_profile(root.path()).await;
    let (tool, context, manager, calls, _backup) =
        reviewer_tool_with(root.path(), "", /* parent_refuses */ true).await;
    let started = tool
        .execute(
            json!({"type": "reviewer", "prompt": "Review the change."}),
            &context,
        )
        .await
        .unwrap();
    let meta = started.metadata.clone().unwrap();
    let result = settle(&manager, meta["agent_id"].as_str().unwrap()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2, "pin once, parent once");
    let SubAgentStatus::Failed(error) = &result.status else {
        panic!("both routes refused: {:?}", result.status);
    };
    for fact in [
        "requested model `deepseek-v4-flash`",
        "deepseek/deepseek-v4-flash failed too",
        "saved agent profile \"reviewer\" pins PinRoute/fixture-pin-model",
    ] {
        assert!(error.contains(fact), "{fact} missing from {error}");
    }
    assert!(
        !error.contains("requested model `fixture-pin-model`"),
        "the refused pin did not make the failing request: {error}"
    );
    assert!(
        manager.read().await.rerouted_requests.is_empty(),
        "taken when the task settles"
    );
}

/// Finding on #6717: a resumed child skips the spawn's route binding, so its
/// origin comes from the saved receipt instead of a guess from the model.
#[test]
fn a_resumed_child_names_its_saved_route_source() {
    let receipt: ChildRouteReceipt = serde_json::from_value(json!({
        "requested_type": "reviewer",
        "requested_profile": null,
        "resolved_profile_id": "reviewer",
        "profile_origin": null,
        "canonical_role": "reviewer",
        "provider_id": "PinRoute",
        "model_id": "fixture-pin-model",
        "route_source": "agent_profile.model",
        "fallback_note": null,
        "requested_reasoning": "auto",
        "effective_reasoning": null,
        "runtime_version": "0",
        "runtime_build_sha": "0"
    }))
    .expect("receipt");
    let origin = route_origin_from_receipt(&receipt).expect("known source");
    let refusal = anyhow::Error::new(LlmError::from_http_response(403, REFUSAL));
    let message = annotate_child_model_error_with_origin(
        &subagent_failure_message(&refusal),
        "fixture-pin-model",
        codewhale_config::ProviderKind::Deepseek,
        &ModelRoute::Fixed("fixture-pin-model".into()),
        Some(&origin),
    );
    assert!(
        message.contains("saved agent profile \"reviewer\" pins PinRoute/fixture-pin-model"),
        "{message}"
    );
    assert!(message.contains("/fleet members"), "{message}");
    assert!(
        !message.contains("explicit child model override"),
        "no override was given: {message}"
    );
    // Without any origin, a fixed route no longer claims an override either.
    let message = annotate_child_model_error(
        &subagent_failure_message(&refusal),
        "fixture-pin-model",
        codewhale_config::ProviderKind::Deepseek,
        &ModelRoute::Fixed("fixture-pin-model".into()),
    );
    assert!(
        !message.contains("explicit child model override"),
        "{message}"
    );
}

/// The approved replacement owns the response grammar. Its distinct Responses
/// item IDs do not make a repeated call_id safe to dispatch under Chat rules.
#[tokio::test]
async fn responses_replacement_rejects_pairing_collisions_using_its_actual_route() {
    let _env = crate::test_support::lock_test_env();
    let root = tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "deepseek");
    let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "deepseek-v4-flash");
    let (pin_url, pin_calls) = refusing_chat_server().await;
    let backup = wiremock::MockServer::start().await;
    let mut frames = vec![
        json!({"type":"response.output_item.added", "item":{"type":"message", "id":"message"}}),
        json!({"type":"response.output_text.delta", "delta":"Retained replacement response text."}),
        json!({"type":"response.output_item.done"}),
    ];
    for item in ["item_first", "item_second"] {
        frames.extend([
            json!({"type":"response.output_item.added", "item":{
                "type":"function_call", "id":item, "call_id":"same_call", "name":"bash"
            }}),
            json!({"type":"response.function_call_arguments.delta", "delta":r#"{"command":"pwd"}"#}),
            json!({"type":"response.output_item.done"}),
        ]);
    }
    frames.push(json!({"type":"response.completed", "response":{
        "status":"completed", "usage":{"input_tokens":10, "output_tokens":5}
    }}));
    let body = frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect::<String>();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&backup)
        .await;
    let config_path = root.path().join("config.toml");
    write_replacement_config(
        &config_path,
        &pin_url,
        &format!("{}/v1", backup.uri()),
        r#"
wire = "responses"

[subagents.roles.reviewer]
model = "PinRoute/fixture-pin-model"
replacements = ["BackupRoute/fixture-backup-model"]
"#,
    )
    .await;
    let config = codewhale_plugins::providers::load_config(Some(config_path), None).unwrap();
    let client = CodewhaleClient::new(&config).unwrap();
    assert_eq!(
        client.wire_format(),
        codewhale_config::provider::WireFormat::ChatCompletions
    );
    let manager = new_shared_subagent_manager(root.path().to_path_buf(), 2);
    let context = ToolContext::new(root.path()).with_state_namespace("route-identity");
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
    let started = tool
        .execute(
            json!({"type":"reviewer", "prompt":"Inspect the workspace."}),
            &context,
        )
        .await
        .unwrap();
    let metadata = started.metadata.as_ref().unwrap();
    let id = metadata["agent_id"].as_str().unwrap();
    assert_eq!(metadata["child_route"]["provider_id"], "PinRoute");
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = manager.read().await.get_result(id).unwrap();
            if result.status != SubAgentStatus::Running {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement pairing refusal settles");
    let SubAgentStatus::Failed(error) = &result.status else {
        panic!("ambiguous replacement must fail: {:?}", result.status);
    };
    assert!(error.contains("pairing id"), "{error}");
    assert_eq!(pin_calls.load(Ordering::SeqCst), 1);
    let requests = backup.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        1,
        "no tool result round follows refused admission"
    );
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(request.get("input").is_some());
    assert!(request.get("messages").is_none());
    assert_eq!(request["model"], "fixture-backup-model");
    assert!(
        result.usage.as_ref().and_then(|usage| usage.total_tokens) == Some(15),
        "the refused response remains billed"
    );
    let checkpoint = result
        .checkpoint
        .as_ref()
        .expect("durable refused response");
    assert!(checkpoint.messages.iter().flat_map(|m| &m.content).any(
        |block| matches!(block, ContentBlock::Text { text, .. } if text == "Retained replacement response text.")
    ));
    assert!(
        checkpoint
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .all(|block| !matches!(
                block,
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
            ))
    );
    let route = result.child_route.unwrap();
    assert_eq!(route.provider_id, "BackupRoute");
    assert_eq!(route.route_source, "role.replacement");
}

#[path = "persona_receipt.rs"]
mod persona_receipt;
