/// A catalog row stating `codewhale.protocol = "responses"` must dispatch
/// to the account API's Responses surface — `{base}/responses` — not the
/// Chat Completions default its `openai/` namespace alone would imply.
#[tokio::test]
async fn codewhale_responses_catalog_row_dispatches_to_responses_endpoint() {
    let _env = crate::test_support::lock_test_env();
    let _live = crate::provider_lake::lock_live_snapshot();
    let home = tempfile::tempdir().expect("home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"",
                    ",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
                    "data: [DONE]\n\n"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Seed the account catalog through the same refresh seam the runtime
    // uses, so the wire choice comes from the row's stated protocol.
    let fingerprint = base_url_fingerprint(&server.uri());
    let offerings = codewhale_catalog_offerings_from_body(
        r#"{"object":"list","data":[
                    {"id":"openai/gpt-5.6","object":"model","owned_by":"openai",
                     "codewhale":{"provider":"openai","model":"gpt-5.6",
                                  "protocol":"responses","endpoint":"/v1/responses",
                                  "default":true,"usable":true}}
                ]}"#,
        "codewhale",
        &fingerprint,
        now_unix(),
    )
    .expect("fixture catalog parses");
    crate::provider_catalog_live::record_success(ProviderCatalogDelta {
        provider: "codewhale".to_string(),
        base_url_fingerprint: fingerprint,
        fetched_at: now_unix(),
        offerings,
    });

    let mut client = codewhale_client(&server, "openai/gpt-5.6");
    assert_eq!(client.wire_format, WireFormat::Responses);
    client.retry.enabled = false;
    let response = client
        .create_message(minimal_zen_request("openai/gpt-5.6"))
        .await
        .expect("Codewhale responses request should succeed");

    // Responses usage arrives on the terminal `response.completed` event —
    // the dialect's equivalent of chat's `stream_options.include_usage`.
    assert_eq!(response.usage.input_tokens, 3);
    assert_eq!(response.usage.output_tokens, 1);
    let requests = server.received_requests().await.expect("recorded request");
    assert_eq!(requests.len(), 1);
    assert_codewhale_bearer(&requests[0]);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("responses JSON body");
    assert_eq!(
        body.get("model").and_then(Value::as_str),
        Some("openai/gpt-5.6")
    );
    assert!(body.get("input").is_some(), "Responses body: {body}");
    assert!(body.get("messages").is_none(), "Responses body: {body}");

    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();
}

fn opencode_zen_client(server: &MockServer, model: &str) -> CodewhaleClient {
    let config = {
        let __cw_config_init_0_0 = Some("opencode-zen".to_string());
        let __cw_config_init_0_1 = Some(ProvidersConfig {
            opencode_zen: ProviderConfig {
                api_key: Some("zen-test-key".to_string()),
                base_url: Some(server.uri()),
                model: Some(model.to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    };
    CodewhaleClient::new(&config).expect("OpenCode Zen client should resolve its model route")
}

fn minimal_zen_request(model: &str) -> MessageRequest {
    translation_message_request("hello", model.to_string(), "English", 4096)
}

fn assert_zen_bearer_without_codex_headers(request: &wiremock::Request) {
    assert_eq!(
        request
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer zen-test-key")
    );
    for forbidden in [
        "openai-beta",
        "originator",
        "chatgpt-account-id",
        "x-api-key",
    ] {
        assert!(
            request.headers.get(forbidden).is_none(),
            "Zen request must not include {forbidden}"
        );
    }
}

fn assert_zen_messages_api_key_without_bearer(request: &wiremock::Request) {
    assert_eq!(
        request
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("zen-test-key")
    );
    assert!(
        request.headers.get(AUTHORIZATION).is_none(),
        "Zen Messages request must not include Authorization"
    );
    for forbidden in ["openai-beta", "originator", "chatgpt-account-id"] {
        assert!(
            request.headers.get(forbidden).is_none(),
            "Zen request must not include {forbidden}"
        );
    }
}

#[tokio::test]
async fn opencode_go_dispatches_all_three_wires_with_gateway_auth_and_stable_session() {
    let mut session = None;
    for (model, wire, endpoint) in [
        (
            "deepseek-v4-pro",
            WireFormat::ChatCompletions,
            "/zen/go/v1/chat/completions",
        ),
        ("grok-4.6", WireFormat::Responses, "/zen/go/v1/responses"),
        (
            "minimax-m3",
            WireFormat::AnthropicMessages,
            "/zen/go/v1/messages",
        ),
    ] {
        let server = MockServer::start().await;
        let response = match wire {
                    WireFormat::Responses => ResponseTemplate::new(200)
                        .insert_header("Content-Type", "text/event-stream")
                        .set_body_string("data: [DONE]\n\n"),
                    WireFormat::AnthropicMessages => ResponseTemplate::new(200).set_body_json(json!({
                        "id":"msg_go", "type":"message", "role":"assistant",
                        "content":[{"type":"text", "text":"ok"}], "model":model,
                        "stop_reason":"end_turn", "stop_sequence":null,
                        "usage":{"input_tokens":3,"output_tokens":1}
                    })),
                    WireFormat::ChatCompletions => ResponseTemplate::new(200).set_body_json(json!({
                        "id":"chat_go", "object":"chat.completion", "created":1, "model":model,
                        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}
                    })),
                };
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let mut client = CodewhaleClient::new(&{
            let __cw_config_init_1_0 = Some("opencode-go".into());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                opencode_go: ProviderConfig {
                    api_key: Some("go-test-key".into()),
                    base_url: Some(format!("{}/zen/go/v1", server.uri())),
                    model: Some(format!("opencode-go/{model}")),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        })
        .expect("Go client resolves the model protocol");
        client.retry.enabled = false;
        assert_eq!(client.wire_format, wire);
        if wire == WireFormat::Responses {
            let mut stream = client
                .create_message_stream(minimal_zen_request(model))
                .await
                .unwrap();
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            assert!(
                client
                    .prepare_outbound_request(minimal_zen_request("minimax-m3"), false)
                    .is_err(),
                "a request cannot silently change an existing client's protocol"
            );
        } else {
            client
                .create_message(minimal_zen_request(model))
                .await
                .unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        let header = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
        };
        let observed_session = header("x-opencode-session").expect("stable session header");
        assert!(!observed_session.is_empty());
        if let Some(previous) = &session {
            assert_eq!(observed_session, previous);
        }
        session = Some(observed_session.to_string());
        assert!(
            header("user-agent")
                .is_some_and(|value| value.contains("Codewhale") || value.contains("codewhale"))
        );
        for forbidden in ["openai-beta", "originator", "chatgpt-account-id"] {
            assert!(
                header(forbidden).is_none(),
                "gateway requests cannot carry {forbidden}"
            );
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], model);
        if wire == WireFormat::AnthropicMessages {
            assert_eq!(header("x-api-key"), Some("go-test-key"));
            assert_eq!(header("anthropic-version"), Some("2023-06-01"));
            assert!(header("authorization").is_none());
            assert!(body.get("messages").is_some());
        } else {
            assert_eq!(header("authorization"), Some("Bearer go-test-key"));
            assert!(header("x-api-key").is_none());
            assert!(
                body.get(if wire == WireFormat::Responses {
                    "input"
                } else {
                    "messages"
                })
                .is_some()
            );
        }
    }
}

#[tokio::test]
async fn opencode_zen_responses_request_uses_responses_route_without_oauth_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = opencode_zen_client(&server, "gpt-5.5");
    assert_eq!(client.wire_format, WireFormat::Responses);
    let mut stream = client
        .create_message_stream(minimal_zen_request("gpt-5.5"))
        .await
        .expect("Zen Responses request should start");
    while let Some(event) = stream.next().await {
        event.expect("Zen Responses stream event");
    }

    let requests = server.received_requests().await.expect("recorded request");
    assert_eq!(requests.len(), 1);
    assert_zen_bearer_without_codex_headers(&requests[0]);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("Responses JSON body");
    assert_eq!(body.get("model").and_then(Value::as_str), Some("gpt-5.5"));
    assert!(body.get("input").is_some(), "Responses body: {body}");
    assert!(body.get("messages").is_none(), "Responses body: {body}");
}

#[tokio::test]
async fn opencode_zen_messages_request_shape_uses_api_key_anthropic_route() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_zen",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "claude-sonnet-4-6",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = opencode_zen_client(&server, "claude-sonnet-4-6");
    assert_eq!(client.wire_format, WireFormat::AnthropicMessages);
    client
        .create_message(minimal_zen_request("claude-sonnet-4-6"))
        .await
        .expect("Zen Messages request should succeed");

    let requests = server.received_requests().await.expect("recorded request");
    assert_eq!(requests.len(), 1);
    assert_zen_messages_api_key_without_bearer(&requests[0]);
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-version")
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );
}

#[tokio::test]
async fn opencode_zen_chat_request_uses_chat_completions_route() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl_zen",
            "object": "chat.completion",
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = opencode_zen_client(&server, "deepseek-v4-pro");
    assert_eq!(client.wire_format, WireFormat::ChatCompletions);
    client
        .create_message(minimal_zen_request("deepseek-v4-pro"))
        .await
        .expect("Zen Chat Completions request should succeed");

    let requests = server.received_requests().await.expect("recorded request");
    assert_eq!(requests.len(), 1);
    assert_zen_bearer_without_codex_headers(&requests[0]);
    assert!(requests[0].headers.get("anthropic-version").is_none());
}

#[tokio::test]
async fn opencode_zen_client_fails_closed_when_request_model_changes_protocol() {
    let server = MockServer::start().await;
    let client = opencode_zen_client(&server, "gpt-5.5");

    let error = client
        .create_message(minimal_zen_request("claude-sonnet-4-6"))
        .await
        .expect_err("a Responses-bound client must not send a Messages model");
    assert!(format!("{error:#}").contains("resolve a new model route"));
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .is_empty()
    );
}

const CONFIG_SECRET_SENTINELS: [&str; 8] = [
    "deepseek-config-secret-001",
    "arcee-config-secret-002",
    "moonshot-config-secret-003",
    "openrouter-config-secret-004",
    "together-config-secret-005",
    "xiaomi-config-secret-006",
    "zai-active-config-secret-007",
    "sakana-config-secret-008",
];

#[test]
fn chatgpt_client_rejects_external_tokens_and_uses_its_owned_grant() {
    let _env = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().expect("credential fixture");
    let root = temp
        .path()
        .canonicalize()
        .expect("canonical credential home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
    let _ambient =
        crate::test_support::EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "external-token");
    let mut config = {
        let __cw_config_init_2_0 = Some("openai-codex".to_string());
        let mut __cw_config_init_2_value: Config = Config::default();
        __cw_config_init_2_value.provider = __cw_config_init_2_0;
        __cw_config_init_2_value
    };
    codewhale_runtime::external_credentials::reset_side_effect_trap();
    assert!(CodewhaleClient::new(&config).is_err());
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts(),
        (0, 0, 0, 0, 0)
    );
    let token = codewhale_runtime::oauth::install_test_chatgpt_registration(&mut config)
        .expect("owned registration");
    codewhale_runtime::external_credentials::reset_side_effect_trap();
    let client = CodewhaleClient::new(&config).expect("official ChatGPT client");
    assert_eq!(client.api_key, token);
    assert_eq!(client.base_url, "https://api.openai.com/v1");
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts(),
        (0, 0, 0, 0, 0)
    );
}

#[test]
fn chatgpt_reasoning_scope_separates_account_workspace_and_custom_routes() {
    let _env = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .canonicalize()
        .expect("canonical credential home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
    let mut config = {
        let __cw_config_init_3_0 = Some("openai-codex".into());
        let mut __cw_config_init_3_value: Config = Default::default();
        __cw_config_init_3_value.provider = __cw_config_init_3_0;
        __cw_config_init_3_value
    };
    let mut scopes = Vec::new();
    for (subject, client_id) in [
        ("account-a", "oaiapp_workspace_a"),
        ("account-b", "oaiapp_workspace_a"),
        ("account-a", "oaiapp_workspace_b"),
    ] {
        codewhale_runtime::oauth::install_test_chatgpt_registration_for(
            &mut config,
            subject,
            client_id,
        )
        .unwrap();
        let client = CodewhaleClient::new(&config).unwrap();
        assert_eq!(
            client.chatgpt_reasoning_api,
            client.clone().chatgpt_reasoning_api
        );
        scopes.push(client.chatgpt_reasoning_api.unwrap());
    }
    assert_ne!(scopes[0], scopes[1]);
    assert_ne!(scopes[0], scopes[2]);
    assert!(
        scopes
            .iter()
            .all(|scope| scope.starts_with("openai-responses-siwc-v1:")
                && !scope.contains("account-"))
    );
    let provider = config
        .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::OpenaiCodex))
        .unwrap();
    provider.base_url = Some("http://127.0.0.1:9/v1".into());
    provider.api_key = Some("configured-fixture-key".into());
    provider.auth_mode = None;
    provider.oauth_credential_generation = None;
    assert!(
        CodewhaleClient::new(&config)
            .unwrap()
            .chatgpt_reasoning_api
            .is_none()
    );
}

#[test]
fn chatgpt_roster_preserves_visible_account_order_and_labels() {
    let roster = parse_models_response_for_provider(
        r#"{"models":[
                {"slug":"gpt-z","display_name":"GPT Z","visibility":"list"},
                {"slug":"gpt-hidden","display_name":"Hidden","visibility":"hidden"},
                {"slug":"gpt-a","display_name":"GPT A","visibility":"list"},
                {"slug":"gpt-z","display_name":"Duplicate","visibility":"list"}
            ]}"#,
        ProviderKind::OpenaiCodex,
    )
    .expect("roster");
    assert_eq!(
        roster.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["gpt-z", "gpt-a"]
    );
    assert_eq!(roster[0].display_name.as_deref(), Some("GPT Z"));
    assert!(
        parse_models_response_for_provider(
            r#"{"data":[{"id":"gpt-z"}]}"#,
            ProviderKind::OpenaiCodex
        )
        .is_err()
    );
    assert!(
        parse_models_response_for_provider(
            r#"{"models":[{"slug":"gpt-bad\n","display_name":"Bad","visibility":"list"}]}"#,
            ProviderKind::OpenaiCodex
        )
        .is_err()
    );
}

fn client_with_config_secret_sentinels() -> CodewhaleClient {
    let _ = rustls::crypto::ring::default_provider().install_default();
    CodewhaleClient::new(
        &{
            let __cw_config_init_4_0 = Some("zai".to_string());
            let __cw_config_init_4_1 = Some(ProvidersConfig {
                arcee: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[1].to_string()),
                    ..ProviderConfig::default()
                },
                moonshot: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[2].to_string()),
                    ..ProviderConfig::default()
                },
                openrouter: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[3].to_string()),
                    ..ProviderConfig::default()
                },
                together: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[4].to_string()),
                    ..ProviderConfig::default()
                },
                xiaomi_mimo: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[5].to_string()),
                    ..ProviderConfig::default()
                },
                zai: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[6].to_string()),
                    ..ProviderConfig::default()
                },
                sakana: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[7].to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_4_value: Config = Config::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value.providers = __cw_config_init_4_1;
            __cw_config_init_4_value
        }
        .with_legacy_root(Some(CONFIG_SECRET_SENTINELS[0].to_string()), None),
    )
    .expect("client with secret sentinels")
}

fn request_with_tool_result(content: impl Into<String>) -> MessageRequest {
    MessageRequest {
        model: "glm-5.2".to_string(),
        messages: vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-secret-test".to_string(),
                    name: "read_file".to_string(),
                    input: json!({"path": "config.toml"}),
                    caller: None,
                    thought_signature: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-secret-test".to_string(),
                    content: content.into(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ],
        max_tokens: 128,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: None,
        temperature: None,
        top_p: None,
    }
}

fn tool_result_content(request: &MessageRequest) -> &str {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .expect("tool result content")
}

#[test]
fn model_bound_request_repairs_dangling_tool_call_before_adapter_projection() {
    let client = client_with_config_secret_sentinels();
    let mut request = request_with_tool_result("unused");
    request.messages.pop();

    let prepared = client.prepare_model_bound_request(request);

    assert!(prepared.messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error: Some(true),
                    ..
                } if tool_use_id == "call-secret-test"
                    && content.contains("crashed_and_repaired")
            )
        })
    }));
    assert_eq!(
        prepared.messages.last().expect("repaired result").role,
        "user"
    );
    assert!(!prepared.messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Text { text, .. }
                    if text.contains("[tool_history_repair]")
            )
        })
    }));
}

#[test]
fn model_bound_tool_results_keep_ordinary_code_byte_exact() {
    // #5546: key-only hits in source files must reach the model unchanged
    // so exact-match edits and read-back verification keep working.
    let client = client_with_config_secret_sentinels();
    let source = "\
        \"jsonwebtoken\": \"^9.0.2\",
          password: credentials?.password,
        token = generate_verification_token()
      secret: process.env.NEXTAUTH_SECRET!,
    {\"id\":1, \"password\": \"x\", \"language\": \"en\"}
    ";
    let prepared = client.prepare_model_bound_request(request_with_tool_result(source.to_string()));
    assert_eq!(tool_result_content(&prepared), source);

    // A configured credential and a credential-shaped value are still hidden.
    let leaking = format!(
        "api_key = \"{}\"\nsession = \"{}\"\n",
        CONFIG_SECRET_SENTINELS[0],
        ["sk-", "abcdef1234567890abcdef"].concat()
    );
    let prepared = client.prepare_model_bound_request(request_with_tool_result(leaking));
    let content = tool_result_content(&prepared);
    assert!(!content.contains(CONFIG_SECRET_SENTINELS[0]));
    assert!(!content.contains("abcdef1234567890abcdef"));
    assert_eq!(
        content
            .matches(codewhale_config::persistence::REDACTED)
            .count(),
        2
    );
}

#[test]
fn model_bound_scenario() {
    // Scenario consolidation of: model_bound_request_redacts_configured_secrets_and_bare_active_key, model_bound_request_leaves_ordinary_tool_output_unchanged
    // from model_bound_request_redacts_configured_secrets_and_bare_active_key
    {
        let client = client_with_config_secret_sentinels();
        let config_dump = format!(
            "api_key = \"{}\"\n[providers.arcee]\napi_key = \"{}\"\n\
                     ordinary_setting = \"keep-me\"\nall bare values: {}",
            CONFIG_SECRET_SENTINELS[0],
            CONFIG_SECRET_SENTINELS[1],
            CONFIG_SECRET_SENTINELS.join(" ")
        );

        let prepared = client.prepare_model_bound_request(request_with_tool_result(config_dump));
        let content = tool_result_content(&prepared);

        for secret in CONFIG_SECRET_SENTINELS {
            assert!(!content.contains(secret), "secret survived redaction");
        }
        assert!(content.contains(codewhale_config::persistence::REDACTED));
        assert!(content.contains("ordinary_setting"));
        assert!(content.contains("keep-me"));
    }
    // from model_bound_request_leaves_ordinary_tool_output_unchanged
    {
        let client = client_with_config_secret_sentinels();
        let ordinary = "tests passed: 42\nREADME.md updated\n";
        let prepared =
            client.prepare_model_bound_request(request_with_tool_result(ordinary.to_string()));
        assert_eq!(tool_result_content(&prepared), ordinary);
    }
}

/// The `[redaction] model_bound = "disabled"` opt-out, once confirmed on
/// the startup gate, must let the model see tool output byte-for-byte —
/// including configured secrets and credential-shaped values that the
/// default masking would have removed (#5546 keeps code quotable; this
/// opt-out goes further and keeps credentials quotable too).
#[test]
fn confirmed_opt_out_keeps_configured_secrets_visible_to_the_model() {
    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("create isolated home");
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let codewhale_home = tmp.path().join("codewhale-home");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    std::fs::create_dir_all(&codewhale_home).expect("create config home");
    std::fs::write(
        codewhale_home.join("config.toml"),
        "[redaction]\nmodel_bound = \"disabled\"\n",
    )
    .expect("write opt-out request");
    codewhale_config::redaction::record_model_bound_disabled_confirmation(
        &codewhale_home.join("config.toml"),
    )
    .expect("record opt-out confirmation");

    let client = CodewhaleClient::new(
        &{
            let __cw_config_init_5_0 = Some(codewhale_home.join("config.toml"));
            let __cw_config_init_5_1 = Some("zai".to_string());
            let __cw_config_init_5_2 = Some(ProvidersConfig {
                zai: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[6].to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let __cw_config_init_5_3 = Some(codewhale_config::redaction::RedactionToml {
                model_bound: Some(codewhale_config::redaction::ModelBoundMasking::Disabled),
            });
            let mut __cw_config_init_5_value: Config = Config::default();
            __cw_config_init_5_value.loaded_config_path = __cw_config_init_5_0;
            __cw_config_init_5_value.provider = __cw_config_init_5_1;
            __cw_config_init_5_value.providers = __cw_config_init_5_2;
            __cw_config_init_5_value.redaction = __cw_config_init_5_3;
            __cw_config_init_5_value
        }
        .with_legacy_root(Some(CONFIG_SECRET_SENTINELS[0].to_string()), None),
    )
    .expect("client with confirmed opt-out");

    let tool_output = format!(
        "api_key = \"{}\"\n[providers.arcee]\napi_key = \"{}\"\nbearer {}",
        CONFIG_SECRET_SENTINELS[0], CONFIG_SECRET_SENTINELS[1], CONFIG_SECRET_SENTINELS[3]
    );
    let prepared =
        client.prepare_model_bound_request(request_with_tool_result(tool_output.clone()));
    assert_eq!(
        tool_result_content(&prepared),
        tool_output,
        "a confirmed opt-out must keep tool output byte-exact"
    );
}

#[test]
fn redaction_confirmation_follows_explicit_and_environment_config_loading() {
    use crate::test_support::{EnvVarGuard, lock_test_env};
    let _lock = lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", temp.path());
    let default = temp.path().join("config.toml");
    let custom = temp.path().join("selected.toml");
    let body = format!(
        "provider = \"zai\"\n[providers.zai]\napi_key = \"{}\"\n[redaction]\nmodel_bound = \"disabled\"\n",
        CONFIG_SECRET_SENTINELS[6]
    );
    std::fs::write(&default, &body).unwrap();
    std::fs::write(&custom, &body).unwrap();
    codewhale_config::redaction::record_model_bound_disabled_confirmation(&default).unwrap();
    let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &custom);
    let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let tool_output = format!("api_key = \"{}\"", CONFIG_SECRET_SENTINELS[6]);
    for explicit in [Some(custom.clone()), None] {
        let config = codewhale_plugins::providers::load_config(explicit, None).unwrap();
        assert_eq!(
            config
                .loaded_config_path
                .as_ref()
                .unwrap()
                .canonicalize()
                .unwrap(),
            custom.canonicalize().unwrap()
        );
        assert!(config.redaction_confirmation_required());
        let client = CodewhaleClient::new(&config).unwrap();
        let prepared =
            client.prepare_model_bound_request(request_with_tool_result(tool_output.clone()));
        assert!(!tool_result_content(&prepared).contains(CONFIG_SECRET_SENTINELS[6]));
    }
    let config = codewhale_plugins::providers::load_config(None, None).unwrap();
    config.record_redaction_confirmation().unwrap();
    for explicit in [Some(custom.clone()), None] {
        let config = codewhale_plugins::providers::load_config(explicit, None).unwrap();
        assert!(!config.redaction_confirmation_required());
        let client = CodewhaleClient::new(&config).unwrap();
        let prepared =
            client.prepare_model_bound_request(request_with_tool_result(tool_output.clone()));
        assert_eq!(tool_result_content(&prepared), tool_output);
    }
    // Local provenance is never accepted from serialized configuration.
    let decoded: Config =
        toml::from_str("loaded_config_path = \"/untrusted/config.toml\"\n").unwrap();
    assert!(decoded.loaded_config_path.is_none());
}

/// Without a confirmation receipt the same config request stays masked:
/// the gate is what separates a wish from an effective opt-out.
#[test]
fn unconfirmed_opt_out_request_stays_masked() {
    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("create isolated home");
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let codewhale_home = tmp.path().join("codewhale-home");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);

    let client = CodewhaleClient::new(
        &{
            let __cw_config_init_6_0 = Some("zai".to_string());
            let __cw_config_init_6_1 = Some(ProvidersConfig {
                zai: ProviderConfig {
                    api_key: Some(CONFIG_SECRET_SENTINELS[6].to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let __cw_config_init_6_2 = Some(codewhale_config::redaction::RedactionToml {
                model_bound: Some(codewhale_config::redaction::ModelBoundMasking::Disabled),
            });
            let mut __cw_config_init_6_value: Config = Config::default();
            __cw_config_init_6_value.provider = __cw_config_init_6_0;
            __cw_config_init_6_value.providers = __cw_config_init_6_1;
            __cw_config_init_6_value.redaction = __cw_config_init_6_2;
            __cw_config_init_6_value
        }
        .with_legacy_root(Some(CONFIG_SECRET_SENTINELS[0].to_string()), None),
    )
    .expect("client with unconfirmed opt-out request");

    let secret = CONFIG_SECRET_SENTINELS[0];
    let prepared = client
        .prepare_model_bound_request(request_with_tool_result(format!("api_key = \"{secret}\"")));
    let content = tool_result_content(&prepared);
    assert!(
        !content.contains(secret),
        "an unconfirmed request must stay on the safe default"
    );
}

#[test]
fn model_bound_request_redacts_inactive_file_store_and_environment_secrets() {
    const FILE_STORED_INACTIVE: &str = "inactive-arcee-file-secret-901";
    const BUILTIN_ENV_SECRET: &str = "inactive-arcee-env-secret-902";
    const CUSTOM_ENV_NAME: &str = "CW_TEST_CUSTOM_PROVIDER_API_KEY";
    const CUSTOM_ENV_SECRET: &str = "inactive-custom-env-secret-903";

    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let codewhale_home = tmp.path().join("codewhale-home");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("create isolated home");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let _secret_backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let builtin_env_name = ProviderKind::Arcee
        .provider()
        .env_vars()
        .first()
        .copied()
        .expect("Arcee API-key environment variable");
    let _builtin_env = crate::test_support::EnvVarGuard::set(builtin_env_name, BUILTIN_ENV_SECRET);
    let _custom_env = crate::test_support::EnvVarGuard::set(CUSTOM_ENV_NAME, CUSTOM_ENV_SECRET);

    codewhale_secrets::Secrets::file_backed()
        .set("arcee", FILE_STORED_INACTIVE)
        .expect("write isolated inactive provider credential");

    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = CodewhaleClient::new(&{
        let __cw_config_init_7_0 = Some("zai".to_string());
        let __cw_config_init_7_1 = Some(ProvidersConfig {
            zai: ProviderConfig {
                api_key: Some("active-zai-secret-900".to_string()),
                ..ProviderConfig::default()
            },
            custom: HashMap::from([(
                "example-custom".to_string(),
                ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    api_key_env: Some(CUSTOM_ENV_NAME.to_string()),
                    ..ProviderConfig::default()
                },
            )]),
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_7_value: Config = Config::default();
        __cw_config_init_7_value.provider = __cw_config_init_7_0;
        __cw_config_init_7_value.providers = __cw_config_init_7_1;
        __cw_config_init_7_value
    })
    .expect("client with inactive file-store credential");
    let prepared = client.prepare_model_bound_request(request_with_tool_result(format!(
                "retrieved values: {FILE_STORED_INACTIVE} {BUILTIN_ENV_SECRET} {CUSTOM_ENV_SECRET}\nordinary output survives"
            )));
    let content = tool_result_content(&prepared);

    for secret in [FILE_STORED_INACTIVE, BUILTIN_ENV_SECRET, CUSTOM_ENV_SECRET] {
        assert!(
            !content.contains(secret),
            "inactive secret survived redaction"
        );
    }
    assert!(content.contains(codewhale_config::persistence::REDACTED));
    assert!(content.contains("ordinary output survives"));
}

#[test]
fn whitespace_codewhale_home_does_not_load_ambient_redaction_secrets() {
    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let ambient_home = tmp.path().join("ambient-home");
    std::fs::create_dir_all(&ambient_home).expect("create ambient home");
    let _home = crate::test_support::EnvVarGuard::set("HOME", &ambient_home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &ambient_home);
    let _codewhale_home_unset = crate::test_support::EnvVarGuard::remove("CODEWHALE_HOME");
    let _secret_backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    codewhale_secrets::Secrets::file_backed()
        .set("arcee", "ambient-redaction-secret-sentinel")
        .expect("seed ambient file secret store");
    let _whitespace_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", " \t ");
    let mut values = Vec::new();

    push_file_backed_model_bound_secrets(&mut values);

    assert!(
        !values
            .iter()
            .any(|value| value == "ambient-redaction-secret-sentinel"),
        "whitespace must not opt tests into reading the ambient secret store"
    );
}

#[test]
fn short_chat_tool_payload_is_redacted_before_wire_serialization() {
    let client = client_with_config_secret_sentinels();
    let prepared = client.prepare_model_bound_request(request_with_tool_result(format!(
        "active token: {}",
        CONFIG_SECRET_SENTINELS[6]
    )));
    let wire = build_chat_messages_for_request(&prepared);
    let serialized = serde_json::to_string(&wire).expect("serialize chat wire messages");

    assert!(!serialized.contains(CONFIG_SECRET_SENTINELS[6]));
    assert!(serialized.contains(codewhale_config::persistence::REDACTED));
}

#[test]
fn configured_secret_redaction_reaches_all_protocol_bodies() {
    let client = client_with_config_secret_sentinels();
    let prepared = client.prepare_model_bound_request(request_with_tool_result(format!(
        "safe output then {}",
        CONFIG_SECRET_SENTINELS[6]
    )));

    let chat = serde_json::to_string(&build_chat_messages_for_request(&prepared))
        .expect("serialize Chat Completions body");
    let anthropic = client.build_anthropic_body(&prepared, false).to_string();
    let responses = build_responses_body(&prepared).to_string();

    for (route, body) in [
        ("chat", chat.as_str()),
        ("anthropic", anthropic.as_str()),
        ("responses", responses.as_str()),
    ] {
        assert!(
            !body.contains(CONFIG_SECRET_SENTINELS[6]),
            "{route} body retained the configured credential"
        );
        assert!(
            body.contains(codewhale_config::persistence::REDACTED),
            "{route} body lost the redaction marker"
        );
    }
}

// This test deliberately serializes access to process-global spillover
// state while awaiting the retrieval path.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn retrieved_turn_loop_spillover_is_sanitized_before_model_wire() {
    let _guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let spillover_root = tmp.path().join(".codewhale").join("tool_outputs");
    let prior = crate::tools::truncate::set_test_spillover_root(Some(spillover_root.clone()));
    struct Restore(Option<std::path::PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::tools::truncate::set_test_spillover_root(self.0.take());
        }
    }
    let _restore = Restore(prior);

    let head = (0..40)
        .map(|_| format!("{}\n", "safe-head".repeat(100)))
        .collect::<String>();
    let tail = (0..80)
        .map(|_| format!("{}\n", "safe-tail".repeat(100)))
        .collect::<String>();
    let raw = format!("{head}\n{}\n{tail}", CONFIG_SECRET_SENTINELS[6]);
    assert!(
        raw.len() > crate::tools::truncate::SPILLOVER_THRESHOLD_BYTES,
        "fixture must enter turn-loop spillover"
    );

    let mut spilled = crate::tools::spec::ToolResult::success(raw.clone());
    let path = crate::tools::truncate::apply_spillover(&mut spilled, "call-local-secret")
        .expect("turn-loop spillover");
    crate::tools::truncate::publish_legacy_spillover_ownership(&path, "workspace", raw.as_bytes())
        .expect("publish compatibility ownership proof");
    assert_eq!(path.parent(), Some(spillover_root.as_path()));
    assert!(
        std::fs::read_to_string(&path)
            .expect("read local spillover")
            .contains(CONFIG_SECRET_SENTINELS[6]),
        "the full raw result remains available only in the local spillover store"
    );
    assert!(
        !spilled.content.contains(CONFIG_SECRET_SENTINELS[6]),
        "middle-only secret should not be present in retained head/tail"
    );

    let context = crate::tools::spec::ToolContext::new(tmp.path().to_path_buf());
    let retrieved = crate::tools::spec::ToolSpec::execute(
        &crate::tools::tool_result_retrieval::RetrieveToolResultTool,
        json!({
            "ref": "call-local-secret",
            "mode": "query",
            "query": CONFIG_SECRET_SENTINELS[6],
        }),
        &context,
    )
    .await
    .expect("retrieve secret-bearing local spillover slice");
    assert!(retrieved.content.contains(CONFIG_SECRET_SENTINELS[6]));

    let client = client_with_config_secret_sentinels();
    let prepared = client.prepare_model_bound_request(request_with_tool_result(retrieved.content));
    let wire = serde_json::to_string(&build_chat_messages_for_request(&prepared))
        .expect("serialize sanitized retrieval result");
    assert!(!wire.contains(CONFIG_SECRET_SENTINELS[6]));
    assert!(wire.contains(codewhale_config::persistence::REDACTED));
}

#[test]
fn wire_adapter_does_not_persist_sessionless_sha_spillover() {
    let _guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let prior = crate::tools::truncate::set_test_spillover_root(Some(
        tmp.path().join(".codewhale").join("tool_outputs"),
    ));
    struct Restore(Option<std::path::PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::tools::truncate::set_test_spillover_root(self.0.take());
        }
    }
    let _restore = Restore(prior);

    let client = client_with_config_secret_sentinels();
    let raw = format!(
        "{}\ncredential={}\n{}",
        "ordinary output ".repeat(80),
        CONFIG_SECRET_SENTINELS[6],
        "tail ".repeat(80)
    );
    assert!(raw.len() > 1024, "fixture must enter wire dedup size class");
    let raw_sha = codewhale_runtime::hashing::sha256_hex(raw.as_bytes());
    let prepared = client.prepare_model_bound_request(request_with_tool_result(raw));
    let sanitized = tool_result_content(&prepared).to_string();
    let sanitized_sha = codewhale_runtime::hashing::sha256_hex(sanitized.as_bytes());

    let wire = build_chat_messages_for_request(&prepared);
    let serialized = serde_json::to_string(&wire).expect("serialize chat wire messages");
    assert!(!serialized.contains(CONFIG_SECRET_SENTINELS[6]));

    let sanitized_path = crate::tools::truncate::sha_spillover_path(&sanitized_sha)
        .expect("sanitized spillover path");
    assert!(
        !sanitized_path.exists(),
        "sessionless wire fallback must not create an ownerless SHA artifact"
    );

    let raw_path =
        crate::tools::truncate::sha_spillover_path(&raw_sha).expect("raw spillover path");
    assert!(
        !raw_path.exists(),
        "unsanitized tool output must never be persisted by the wire adapter"
    );
    assert!(!serialized.contains("retrieve_tool_result ref=sha:"));
}

fn deepseek_anthropic_client(server: &MockServer) -> CodewhaleClient {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let providers = ProvidersConfig {
        deepseek_anthropic: ProviderConfig {
            api_key: Some("ds-test".to_string()),
            base_url: Some(server.uri()),
            ..ProviderConfig::default()
        },
        ..ProvidersConfig::default()
    };
    CodewhaleClient::new(&{
        let __cw_config_init_8_0 = Some("deepseek-anthropic".to_string());
        let __cw_config_init_8_1 = Some(providers);
        let mut __cw_config_init_8_value: Config = Config::default();
        __cw_config_init_8_value.provider = __cw_config_init_8_0;
        __cw_config_init_8_value.providers = __cw_config_init_8_1;
        __cw_config_init_8_value
    })
    .expect("deepseek anthropic client")
}

fn minimax_anthropic_client_with_base_url(base_url: String) -> CodewhaleClient {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let providers = ProvidersConfig {
        minimax_anthropic: ProviderConfig {
            api_key: Some("minimax-test".to_string()),
            base_url: Some(base_url),
            ..ProviderConfig::default()
        },
        ..ProvidersConfig::default()
    };
    CodewhaleClient::new(&{
        let __cw_config_init_9_0 = Some("minimax-anthropic".to_string());
        let __cw_config_init_9_1 = Some(providers);
        let mut __cw_config_init_9_value: Config = Config::default();
        __cw_config_init_9_value.provider = __cw_config_init_9_0;
        __cw_config_init_9_value.providers = __cw_config_init_9_1;
        __cw_config_init_9_value
    })
    .expect("minimax anthropic client")
}

fn zai_client_for_test() -> CodewhaleClient {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let providers = ProvidersConfig {
        zai: ProviderConfig {
            api_key: Some("zai-test".to_string()),
            base_url: Some("https://api.z.ai/api/coding/paas/v4".to_string()),
            ..ProviderConfig::default()
        },
        ..ProvidersConfig::default()
    };
    CodewhaleClient::new(&{
        let __cw_config_init_10_0 = Some("zai".to_string());
        let __cw_config_init_10_1 = Some(providers);
        let mut __cw_config_init_10_value: Config = Config::default();
        __cw_config_init_10_value.provider = __cw_config_init_10_0;
        __cw_config_init_10_value.providers = __cw_config_init_10_1;
        __cw_config_init_10_value
    })
    .expect("zai client")
}

fn runtime_chat_gate_client(isolated: bool, unrelated: bool) -> CodewhaleClient {
    CodewhaleClient::new(&{
        let __cw_config_init_11_0 = Some("ollama".to_string());
        let __cw_config_init_11_1 = Some("fixture-local:tag".to_string());
        let __cw_config_init_11_2 = isolated;
        let __cw_config_init_11_3 = unrelated;
        let mut __cw_config_init_11_value: Config = Config::default();
        __cw_config_init_11_value.provider = __cw_config_init_11_0;
        __cw_config_init_11_value.default_text_model = __cw_config_init_11_1;
        __cw_config_init_11_value.runtime_chat_isolated = __cw_config_init_11_2;
        __cw_config_init_11_value.runtime_thread_inference_unrelated = __cw_config_init_11_3;
        __cw_config_init_11_value
    })
    .expect("runtime chat gate test client")
}

#[tokio::test]
async fn runtime_chat_provider_gate_blocks_only_attached_run_participants() {
    let participant = runtime_chat_gate_client(false, false);
    let participant_clone = participant.clone();
    assert!(participant.remote_control_inference_participant);
    assert!(participant_clone.remote_control_inference_participant);
    assert!(
        !runtime_chat_gate_client(true, false).remote_control_inference_participant,
        "the isolated relay client must not deadlock on its own exclusive lease"
    );
    assert!(
        !runtime_chat_gate_client(false, true).remote_control_inference_participant,
        "an unrelated RuntimeThreadManager must remain concurrent"
    );

    let ownership = acquire_runtime_chat_inference_ownership().await;
    let waiting = tokio::spawn(async move {
        participant_clone
            .acquire_remote_control_inference_permit()
            .await
    });
    let mut waiting = waiting;
    assert!(
        tokio::time::timeout(Duration::from_millis(40), &mut waiting)
            .await
            .is_err(),
        "an attached-run provider call must wait behind Runtime Chat ownership"
    );
    assert!(
        runtime_chat_gate_client(true, false)
            .acquire_remote_control_inference_permit()
            .await
            .is_none()
    );
    assert!(
        runtime_chat_gate_client(false, true)
            .acquire_remote_control_inference_permit()
            .await
            .is_none()
    );
    drop(ownership);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("participant should resume")
            .expect("permit task")
            .is_some()
    );
}

#[tokio::test]
async fn provider_request_scenario() {
    // Scenario consolidation of: provider_request_concurrency_limiter_is_shared_across_client_clones, provider_request_permit_lives_until_stream_is_consumed
    // from provider_request_concurrency_limiter_is_shared_across_client_clones
    {
        let client = zai_client_for_test();
        assert_eq!(
            client.provider_request_concurrency_limit(),
            Some(codewhale_runtime::config::DEFAULT_ZAI_PROVIDER_MAX_CONCURRENCY)
        );

        let clone = client.clone();
        let permit = client
            .acquire_provider_request_permit()
            .await
            .expect("zai default should install provider request limiter");

        assert_eq!(client.active_provider_requests(), 1);
        assert_eq!(clone.active_provider_requests(), 1);

        drop(permit);

        assert_eq!(client.active_provider_requests(), 0);
        assert_eq!(clone.active_provider_requests(), 0);
    }
    // from provider_request_permit_lives_until_stream_is_consumed
    {
        let client = zai_client_for_test();
        let permit = client
            .acquire_provider_request_permit()
            .await
            .expect("zai default should install provider request limiter");
        let stream: crate::llm_client::StreamEventBox =
            Box::pin(futures_util::stream::iter(vec![Ok(
                StreamEvent::MessageStop,
            )]));
        let mut wrapped =
            CodewhaleClient::hold_provider_request_permit_for_stream(stream, Some(permit));

        assert_eq!(client.active_provider_requests(), 1);
        assert!(wrapped.next().await.is_some());
        assert!(wrapped.next().await.is_none());
        assert_eq!(client.active_provider_requests(), 0);
    }
}

#[tokio::test]
async fn runtime_chat_read_permit_lives_until_stream_is_dropped() {
    let client = runtime_chat_gate_client(false, false);
    let permit = client
        .acquire_remote_control_inference_permit()
        .await
        .expect("interactive participant read permit");
    let stream: crate::llm_client::StreamEventBox = Box::pin(futures_util::stream::pending());
    let wrapped =
        CodewhaleClient::hold_remote_control_inference_permit_for_stream(stream, Some(permit));

    let mut writer = tokio::spawn(acquire_runtime_chat_inference_ownership());
    assert!(
        tokio::time::timeout(Duration::from_millis(40), &mut writer)
            .await
            .is_err(),
        "a live participant stream must retain attached-run ownership through EOF/drop"
    );
    drop(wrapped);
    let ownership = tokio::time::timeout(Duration::from_secs(1), writer)
        .await
        .expect("writer resumes after stream drop")
        .expect("writer task");
    drop(ownership);
}

#[test]
fn parse_speech_scenario() {
    // Scenario consolidation of: parse_speech_audio_response_accepts_message_audio, parse_speech_audio_response_accepts_data_uri
    // from parse_speech_audio_response_accepts_message_audio
    {
        let encoded = general_purpose::STANDARD.encode(b"hi");
        let payload = json!({
            "choices": [{
                "message": {
                    "audio": {
                        "data": encoded,
                        "transcript": "hi"
                    }
                }
            }]
        });

        let (audio, transcript) = parse_speech_audio_response(&payload).unwrap();
        assert_eq!(audio, b"hi");
        assert_eq!(transcript.as_deref(), Some("hi"));
    }
    // from parse_speech_audio_response_accepts_data_uri
    {
        let encoded = general_purpose::STANDARD.encode(b"wav");
        let payload = json!({
            "audio": {
                "data": format!("data:audio/wav;base64,{encoded}")
            }
        });

        let (audio, transcript) = parse_speech_audio_response(&payload).unwrap();
        assert_eq!(audio, b"wav");
        assert_eq!(transcript, None);
    }
}

#[test]
fn speech_synthesis_scenario() {
    // Scenario consolidation of: speech_synthesis_body_omits_user_message_without_instruction, speech_synthesis_body_ignores_blank_instruction, speech_synthesis_body_includes_non_empty_instruction_first
    // from speech_synthesis_body_omits_user_message_without_instruction
    {
        let body =
            build_speech_synthesis_body("mimo-v2.5-tts", "hello", None, json!({"format": "wav"}));
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "hello");
        assert!(
            messages
                .iter()
                .all(|message| message["content"].as_str() != Some(""))
        );
    }
    // from speech_synthesis_body_ignores_blank_instruction
    {
        let body = build_speech_synthesis_body(
            "mimo-v2.5-tts",
            "hello",
            Some("  \t\n  "),
            json!({"format": "wav"}),
        );
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
    }
    // from speech_synthesis_body_includes_non_empty_instruction_first
    {
        let body = build_speech_synthesis_body(
            "mimo-v2.5-tts-voicedesign",
            "hello",
            Some("warm and calm"),
            json!({"format": "wav"}),
        );
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "warm and calm");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "hello");
    }
}

#[test]
fn tool_name_scenario() {
    // Scenario consolidation of: tool_name_roundtrip_dot, tool_name_decode_mangled_dot_prefix, tool_name_decode_bare_hex_no_trailing_dash, tool_name_bare_hex_preserves_alnum, tool_name_bare_hex_preserves_underscore, tool_name_roundtrip_colon
    // from tool_name_roundtrip_dot
    {
        let original = "multi_tool_use.parallel";
        let encoded = to_api_tool_name(original);
        assert_eq!(encoded, "multi_tool_use-x00002E-parallel");
        let decoded = from_api_tool_name(&encoded);
        assert_eq!(decoded, original);
    }
    // from tool_name_decode_mangled_dot_prefix
    {
        let mangled = "multi_tool_use.x00002E-parallel";
        let decoded = from_api_tool_name(mangled);
        assert_eq!(decoded, "multi_tool_use..parallel");
    }
    // from tool_name_decode_bare_hex_no_trailing_dash
    {
        let mangled = "foo_x00002Ebar";
        let decoded = from_api_tool_name(mangled);
        assert_eq!(decoded, "foo_.bar");
    }
    // from tool_name_bare_hex_preserves_alnum
    {
        let input = "foox000041bar";
        let decoded = from_api_tool_name(input);
        assert_eq!(decoded, input);
    }
    // from tool_name_bare_hex_preserves_underscore
    {
        let input = "foox00005Fbar";
        let decoded = from_api_tool_name(input);
        assert_eq!(decoded, input);
    }
    // from tool_name_roundtrip_colon
    {
        let original = "mcp__server:tool_name";
        let encoded = to_api_tool_name(original);
        let decoded = from_api_tool_name(&encoded);
        assert_eq!(decoded, original);
    }
}

#[test]
fn api_url_scenario() {
    // Scenario consolidation of: api_url_handles_default_v1_and_beta_base_urls, api_url_routes_beta_paths_from_any_deepseek_base, api_url_with_suffix_strips_version_before_chat_suffix, api_url_with_suffix_handles_leading_slash, api_url_with_suffix_ignores_suffix_for_models, api_url_with_suffix_ignores_suffix_for_beta_paths, api_url_with_suffix_default_behavior_without_suffix
    // from api_url_handles_default_v1_and_beta_base_urls
    {
        assert_eq!(
            api_url("https://api.deepseek.com", "chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            api_url("https://api.deepseek.com/v1", "chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        // Non-beta paths from a /beta base URL route to /v1.
        // Only paths with an explicit beta/ prefix use the beta surface.
        assert_eq!(
            api_url("https://api.deepseek.com/beta", "chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            api_url(
                "https://openai-compatible.example/api/coding/paas/v4",
                "chat/completions"
            ),
            "https://openai-compatible.example/api/coding/paas/v4/chat/completions"
        );
    }
    // from api_url_routes_beta_paths_from_any_deepseek_base
    {
        assert_eq!(
            api_url("https://api.deepseek.com", "beta/completions"),
            "https://api.deepseek.com/beta/completions"
        );
        assert_eq!(
            api_url("https://api.deepseek.com/v1", "beta/completions"),
            "https://api.deepseek.com/beta/completions"
        );
        assert_eq!(
            api_url("https://api.deepseek.com/beta", "beta/completions"),
            "https://api.deepseek.com/beta/completions"
        );
    }
    // from api_url_with_suffix_strips_version_before_chat_suffix
    {
        assert_eq!(
            api_url_with_suffix(
                "https://api.example.com/v1",
                "chat/completions",
                Some("/chat/completions")
            ),
            "https://api.example.com/chat/completions"
        );
        assert_eq!(
            api_url_with_suffix(
                "https://api.example.com/beta",
                "chat/completions",
                Some("/chat/completions")
            ),
            "https://api.example.com/chat/completions"
        );
    }
    // from api_url_with_suffix_handles_leading_slash
    {
        assert_eq!(
            api_url_with_suffix(
                "https://api.example.com/v1",
                "chat/completions",
                Some("chat/completions")
            ),
            "https://api.example.com/chat/completions"
        );
    }
    // from api_url_with_suffix_ignores_suffix_for_models
    {
        assert_eq!(
            api_url_with_suffix(
                "https://api.example.com/v1",
                "models",
                Some("/chat/completions")
            ),
            "https://api.example.com/v1/models"
        );
    }
    // from api_url_with_suffix_ignores_suffix_for_beta_paths
    {
        assert_eq!(
            api_url_with_suffix(
                "https://api.example.com/v1",
                "beta/completions",
                Some("/chat/completions")
            ),
            "https://api.example.com/beta/completions"
        );
    }
    // from api_url_with_suffix_default_behavior_without_suffix
    {
        assert_eq!(
            api_url_with_suffix("https://api.deepseek.com", "chat/completions", None),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }
}

#[test]
fn api_url_routes_models_and_non_beta_paths_to_v1() {
    // The /models endpoint only exists at /v1/models, never at
    // /beta/models. Non-beta paths from a /beta base URL must
    // still route to /v1.
    assert_eq!(
        api_url("https://api.deepseek.com", "models"),
        "https://api.deepseek.com/v1/models"
    );
    assert_eq!(
        api_url("https://api.deepseek.com/v1", "models"),
        "https://api.deepseek.com/v1/models"
    );
    assert_eq!(
        api_url("https://api.deepseek.com/beta", "models"),
        "https://api.deepseek.com/v1/models"
    );
    assert_eq!(
        api_url("https://api.minimax.io/anthropic", "models"),
        "https://api.minimax.io/anthropic/v1/models"
    );
    assert_eq!(
        api_url("https://api.minimaxi.com/anthropic", "models"),
        "https://api.minimaxi.com/anthropic/v1/models"
    );
    // explicit v<N> versions other than /v1 should be preserved
    assert_eq!(
        api_url(
            "https://openai-compatible.example/api/coding/paas/v4",
            "models"
        ),
        "https://openai-compatible.example/api/coding/paas/v4/models"
    );
}
