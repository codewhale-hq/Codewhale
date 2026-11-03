
    /// #5055: the Chat and Responses DeepSeek mappings are two spellings of
    /// one table. If they ever disagree, one of them was edited alone.
    #[test]
    fn deepseek_chat_and_responses_wires_agree_with_the_shared_effort_table() {
        use super::deepseek_effort::{DEEPSEEK_EFFORT_ALIASES, deepseek_effort_tier};

        for &(alias, tier) in DEEPSEEK_EFFORT_ALIASES {
            for provider in [ProviderKind::Deepseek, ProviderKind::Deepseek] {
                let mut body = json!({});
                apply_reasoning_effort(&mut body, Some(alias), provider);
                assert_eq!(
                    body.get("reasoning_effort").and_then(Value::as_str),
                    tier.chat_reasoning_effort(),
                    "chat wire disagrees with the table for {alias:?} on {provider:?}"
                );
                assert_eq!(
                    body.pointer("/thinking/type").and_then(Value::as_str),
                    Some(if tier.chat_thinking_enabled() {
                        "enabled"
                    } else {
                        "disabled"
                    }),
                    "chat thinking toggle disagrees with the table for {alias:?}"
                );
            }

            assert_eq!(
                super::responses::responses_reasoning_effort(alias, true),
                Some(tier.responses_effort()),
                "responses wire disagrees with the table for {alias:?}"
            );
        }

        // Auto leaves the tier to the provider on both wires.
        assert_eq!(deepseek_effort_tier("auto"), None);
        let mut body = json!({});
        apply_reasoning_effort(&mut body, Some("auto"), ProviderKind::Deepseek);
        assert_eq!(body, json!({}));
        assert_eq!(
            super::responses::responses_reasoning_effort("auto", true),
            None
        );
    }

    async fn capture_deepseek_chat_body_for_effort(
        effort: Option<&str>,
        custom: bool,
        stream: bool,
    ) -> Value {
        let server = MockServer::start().await;
        let model = if custom {
            "deepseek/deepseek-v4.1-flash"
        } else {
            "deepseek-v4-pro"
        };
        let response = if stream {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({
                        "id": "chatcmpl-deepseek-effort-ladder",
                        "object": "chat.completion.chunk",
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {"content": "ok"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    })
                ))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-deepseek-effort-ladder",
                "object": "chat.completion",
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
        };
        Mock::given(method("POST"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;

        let request = MessageRequest {
            model: model.to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "effort ladder capture".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: effort.map(|effort| {
                crate::reasoning_preference::ReasoningEffort::parse_strict(effort)
                    .expect("valid capture effort")
                    .as_setting_for_route(
                        ProviderKind::Deepseek,
                        &server.uri(),
                        model,
                    )
                    .to_string()
            }),
            stream: Some(stream),
            temperature: None,
            top_p: None,
        };
        let client = if custom {
            custom_mock_client_for_identity(&server, "aicraft")
        } else {
            deepseek_request_boundary_client(codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL, server.uri())
        };
        if stream {
            let mut events = client
                .create_message_stream(request)
                .await
                .expect("streaming request succeeds");
            while let Some(event) = events.next().await {
                event.expect("streaming event succeeds");
            }
        } else {
            client
                .create_message(request)
                .await
                .unwrap_or_else(|error| panic!("custom={custom} effort={effort:?}: {error}"));
        }

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        serde_json::from_slice(&requests[0].body).expect("captured request JSON")
    }

    /// Request-body capture per effort level on direct and custom DeepSeek chat
    /// route: the wire must carry the documented low/high/max ladder and the
    /// thinking toggle, never an invented value (#52).
    #[tokio::test]
    async fn deepseek_chat_wire_body_tracks_the_documented_effort_ladder() {
        for (effort, expected_effort, expected_thinking) in [
            (Some("low"), Some("low"), Some("enabled")),
            (Some("medium"), Some("high"), Some("enabled")),
            (Some("high"), Some("high"), Some("enabled")),
            (Some("max"), Some("max"), Some("enabled")),
            (Some("off"), None, Some("disabled")),
            (None, None, None),
        ] {
            for (custom, stream) in [(false, false), (false, true), (true, false), (true, true)] {
                let body = capture_deepseek_chat_body_for_effort(effort, custom, stream).await;
                assert_eq!(body["max_tokens"], 64);
                assert_eq!(
                    body.get("reasoning_effort").and_then(Value::as_str),
                    expected_effort,
                    "reasoning_effort on the wire for {effort:?}: {body}"
                );
                assert_eq!(
                    body.pointer("/thinking/type").and_then(Value::as_str),
                    expected_thinking,
                    "thinking on the wire for {effort:?}: {body}"
                );
            }
        }
    }

    /// TelecomJS TokenHub: the gateway's OpenAI Chat Completions API does NOT
    /// support `reasoning_effort` or `thinking` fields (#4188 review). Verify
    /// that no reasoning fields are injected for any effort level, since not
    /// every gateway model (qwen-max, deepseek-chat, gpt-4o, claude, etc.)
    /// accepts the same reasoning dialect.
    #[test]
    fn reasoning_effort_telecomjs_does_not_inject_reasoning_fields() {
        for effort in &["off", "low", "medium", "high", "max", "xhigh"] {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some(effort), ProviderKind::Telecomjs);
            assert!(
                body.get("reasoning_effort").is_none(),
                "TelecomJS must not inject reasoning_effort for effort={effort}: {body}"
            );
            assert!(
                body.get("thinking").is_none(),
                "TelecomJS must not inject thinking for effort={effort}: {body}"
            );
            assert!(
                body.get("think").is_none(),
                "TelecomJS must not inject think for effort={effort}: {body}"
            );
        }
    }

    #[test]
    fn moonshot_uses_codewhale_user_agent_not_kimi_cli_identity() {
        let user_agent = client_user_agent(ProviderKind::Moonshot);

        assert!(user_agent.contains("codewhale/"));
        assert!(!user_agent.to_ascii_lowercase().contains("kimi_cli"));
        assert!(!user_agent.to_ascii_lowercase().contains("kimi-code-cli"));
    }

    #[test]
    fn reasoning_effort_scenario_2() {
        // Scenario consolidation of: reasoning_effort_ollama_cloud_uses_openai_compatible_field, reasoning_effort_uses_nvidia_nim_chat_template_kwargs, reasoning_effort_off_disables_nvidia_nim_thinking, reasoning_effort_uses_openai_compatible_shape_for_fireworks, reasoning_effort_uses_arcee_reasoning_effort_without_thinking_object, reasoning_effort_maps_openrouter_scale_without_deepseek_max_label, reasoning_effort_uses_xiaomi_mimo_thinking_parameter_only, reasoning_effort_zai_uses_documented_thinking_shape
        // from reasoning_effort_ollama_cloud_uses_openai_compatible_field
        {
            for (effort, expected) in [
                ("off", "none"),
                ("low", "low"),
                ("medium", "medium"),
                ("high", "high"),
                ("max", "max"),
            ] {
                let mut body = json!({});
                apply_reasoning_effort(&mut body, Some(effort), ProviderKind::OllamaCloud);
                assert_eq!(body, json!({ "reasoning_effort": expected }));
            }

            let mut local = json!({});
            apply_reasoning_effort(&mut local, Some("high"), ProviderKind::Ollama);
            assert_eq!(local, json!({ "think": true }));
        }
        // from reasoning_effort_uses_nvidia_nim_chat_template_kwargs
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("max"), ProviderKind::NvidiaNim);

            assert_eq!(
                body.pointer("/chat_template_kwargs/thinking")
                    .and_then(Value::as_bool),
                Some(true)
            );
            assert_eq!(
                body.pointer("/chat_template_kwargs/reasoning_effort")
                    .and_then(Value::as_str),
                Some("max")
            );
            assert!(body.get("thinking").is_none());
            assert!(body.get("reasoning_effort").is_none());
        }
        // from reasoning_effort_off_disables_nvidia_nim_thinking
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("off"), ProviderKind::NvidiaNim);

            assert_eq!(
                body.pointer("/chat_template_kwargs/thinking")
                    .and_then(Value::as_bool),
                Some(false)
            );
            assert!(
                body.pointer("/chat_template_kwargs/reasoning_effort")
                    .is_none()
            );
        }
        // from reasoning_effort_uses_openai_compatible_shape_for_fireworks
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("max"), ProviderKind::Fireworks);

            assert_eq!(
                body.get("reasoning_effort").and_then(Value::as_str),
                Some("max")
            );
            assert!(
                body.get("thinking").is_none(),
                "Fireworks strict-validates OpenAI-compatible requests and rejects top-level thinking"
            );
        }
        // from reasoning_effort_uses_arcee_reasoning_effort_without_thinking_object
        {
            for (input, expected) in [
                ("minimal", "minimal"),
                ("low", "low"),
                ("mid", "medium"),
                ("medium", "medium"),
                ("high", "high"),
                ("max", "high"),
            ] {
                let mut body = json!({});
                apply_reasoning_effort(&mut body, Some(input), ProviderKind::Arcee);

                assert_eq!(
                    body.get("reasoning_effort").and_then(Value::as_str),
                    Some(expected)
                );
                assert!(
                    body.get("thinking").is_none(),
                    "Arcee documents reasoning_effort rather than a DeepSeek thinking object"
                );
            }
        }
        // from reasoning_effort_maps_openrouter_scale_without_deepseek_max_label
        {
            for (input, expected) in [
                ("low", "low"),
                ("minimal", "low"),
                ("medium", "medium"),
                ("mid", "medium"),
                ("high", "high"),
                ("max", "xhigh"),
                ("xhigh", "xhigh"),
            ] {
                let mut body = json!({});
                apply_reasoning_effort(&mut body, Some(input), ProviderKind::Openrouter);

                assert_eq!(
                    body.get("reasoning_effort").and_then(Value::as_str),
                    Some(expected),
                    "OpenRouter effort mapping for {input}"
                );
                assert_eq!(
                    body.pointer("/thinking/type").and_then(Value::as_str),
                    Some("enabled")
                );
            }
        }
        // from reasoning_effort_uses_xiaomi_mimo_thinking_parameter_only
        {
            for input in ["low", "medium", "max", "xhigh"] {
                let mut body = json!({});
                apply_reasoning_effort(&mut body, Some(input), ProviderKind::XiaomiMimo);

                assert_eq!(
                    body.pointer("/thinking/type").and_then(Value::as_str),
                    Some("enabled"),
                    "MiMo thinking mapping for {input}"
                );
                assert!(body.get("reasoning_effort").is_none());
            }

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("off"), ProviderKind::XiaomiMimo);
            assert_eq!(
                body.pointer("/thinking/type").and_then(Value::as_str),
                Some("disabled")
            );
            assert!(body.get("reasoning_effort").is_none());
        }
        // from reasoning_effort_zai_uses_documented_thinking_shape
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("high"), ProviderKind::Zai);
            assert_eq!(
                body,
                json!({ "thinking": { "type": "enabled", "clear_thinking": false } })
            );

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("max"), ProviderKind::Zai);
            assert_eq!(
                body,
                json!({ "thinking": { "type": "enabled", "clear_thinking": false } })
            );

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("ultracode"), ProviderKind::Zai);
            assert_eq!(
                body,
                json!({ "thinking": { "type": "enabled", "clear_thinking": false } })
            );

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("off"), ProviderKind::Zai);
            assert_eq!(body, json!({ "thinking": { "type": "disabled" } }));
        }
    }

    #[test]
    fn reasoning_effort_minimax_requires_exact_route_to_split_reasoning() {
        let mut body = json!({});
        chat::apply_route_reasoning_controls(
            &mut body,
            ProviderKind::Minimax,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL,
            Some("high"),
        );
        assert_eq!(
            body.get("reasoning_split").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("adaptive")
        );
        assert!(body.get("reasoning_effort").is_none());

        let mut body = json!({});
        chat::apply_route_reasoning_controls(
            &mut body,
            ProviderKind::Minimax,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL,
            Some("max"),
        );
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("adaptive")
        );
        assert!(body.get("reasoning_effort").is_none());

        let mut body = json!({});
        chat::apply_route_reasoning_controls(
            &mut body,
            ProviderKind::Minimax,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL,
            Some("off"),
        );
        assert_eq!(
            body.get("reasoning_split").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("disabled")
        );

        let mut body = json!({});
        chat::apply_route_reasoning_controls(
            &mut body,
            ProviderKind::Minimax,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL,
            None,
        );
        assert_eq!(body, json!({ "reasoning_split": true }));

        for (base_url, model) in [
            (
                "https://gateway.example/v1",
                codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL,
            ),
            (codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_BASE_URL, "MiniMax-M2"),
        ] {
            for effort in ["off", "high", "max"] {
                let mut body = json!({});
                chat::apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::Minimax,
                    base_url,
                    model,
                    Some(effort),
                );
                assert_eq!(body, json!({}), "{base_url} {model} {effort}");
            }
        }
    }

    #[test]
    fn chat_parser_accepts_nvidia_nim_reasoning_field() -> Result<()> {
        let response = parse_chat_message(&json!({
            "id": "chatcmpl-test",
            "model": "deepseek-ai/deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "reasoning": "thinking via NIM",
                    "content": "final answer"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 3
            }
        }))?;

        assert!(matches!(
            response.content.first(),
            Some(ContentBlock::Thinking { thinking, .. }) if thinking == "thinking via NIM"
        ));
        assert!(matches!(
            response.content.get(1),
            Some(ContentBlock::Text { text, .. }) if text == "final answer"
        ));
        Ok(())
    }

    #[test]
    fn sse_parser_accepts_nvidia_nim_reasoning_delta() {
        let mut content_index = 0;
        let mut text_started = false;
        let mut thinking_started = false;
        let mut tool_indices = std::collections::HashMap::new();
        let mut reasoning_detail_buffers = std::collections::HashMap::new();
        let events = parse_sse_chunk(
            &json!({
                "choices": [{
                    "delta": {
                        "reasoning": "nim thought"
                    }
                }]
            }),
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_detail_buffers,
            true,
        );

        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { thinking },
                ..
            } if thinking == "nim thought"
        )));
    }

    #[test]
    fn chat_tool_scenario() {
        // Scenario consolidation of: chat_tool_strict_flag_is_nested_under_function, chat_tool_wire_shape_omits_anthropic_only_metadata
        // from chat_tool_strict_flag_is_nested_under_function
        {
            let tool = Tool {
                tool_type: Some("function".to_string()),
                name: "emit_json".to_string(),
                description: "Emit JSON".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
                allowed_callers: None,
                defer_loading: None,
                input_examples: None,
                strict: Some(true),
                cache_control: None,
            };
            let encoded = tool_to_chat(&tool);
            assert_eq!(
                encoded
                    .get("function")
                    .and_then(|function| function.get("strict"))
                    .and_then(Value::as_bool),
                Some(true)
            );
            assert!(encoded.get("strict").is_none());
        }
        // from chat_tool_wire_shape_omits_anthropic_only_metadata
        {
            let tool = Tool {
                tool_type: Some("function".to_string()),
                name: "mcp_read_resource".to_string(),
                description: "Read resource".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: Some(vec![json!({"uri": "file://example"})]),
                strict: None,
                cache_control: None,
            };

            let encoded = tool_to_chat_for_base_url(&tool, "https://api.fireworks.ai/inference/v1");

            assert!(encoded.get("allowed_callers").is_none());
            assert!(encoded.get("defer_loading").is_none());
            assert!(encoded.get("input_examples").is_none());
        }
    }

    #[test]
    fn deepseek_non_beta_base_url_strips_strict_tool_flag() {
        let tool = Tool {
            tool_type: Some("function".to_string()),
            name: "emit_json".to_string(),
            description: "Emit JSON".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: Some(true),
            cache_control: None,
        };

        let encoded = tool_to_chat_for_base_url(&tool, "https://api.deepseek.com/v1");

        assert!(
            encoded
                .get("function")
                .and_then(|function| function.get("strict"))
                .is_none()
        );
    }

    #[test]
    fn deepseek_beta_and_custom_base_urls_keep_strict_tool_flag() {
        let tool = Tool {
            tool_type: Some("function".to_string()),
            name: "emit_json".to_string(),
            description: "Emit JSON".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: Some(true),
            cache_control: None,
        };

        for base_url in [
            "https://api.deepseek.com/beta",
            "https://example.com/openai/v1",
        ] {
            let encoded = tool_to_chat_for_base_url(&tool, base_url);
            assert_eq!(
                encoded
                    .get("function")
                    .and_then(|function| function.get("strict"))
                    .and_then(Value::as_bool),
                Some(true)
            );
        }
    }

    #[test]
    fn chat_messages_scenario() {
        // Scenario consolidation of: chat_messages_drop_thinking_only_assistant_for_non_reasoning_model, chat_messages_drop_orphan_tool_results
        // from chat_messages_drop_thinking_only_assistant_for_non_reasoning_model
        {
            let message = Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Thinking {
                    signature: None,
                    state: None,
                    redacted_data: None,
                    thinking: "plan".to_string(),
                }],
            };
            let out = build_chat_messages(None, &[message], "some-non-deepseek-model");
            assert!(
                !out.iter()
                    .any(|value| value.get("role").and_then(Value::as_str) == Some("assistant")),
                "non-reasoning model should drop thinking-only assistant"
            );
        }
        // from chat_messages_drop_orphan_tool_results
        {
            let messages = vec![Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "tool-1".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            }];

            let out = build_chat_messages(None, &messages, "deepseek-v4-flash");
            assert!(
                !out.iter()
                    .any(|value| { value.get("role").and_then(Value::as_str) == Some("tool") })
            );
        }
    }

    #[test]
    fn parse_sse_chunk_closes_each_tool_block_with_matching_index() {
        let chunk = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_0",
                            "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}
                        },
                        {
                            "index": 1,
                            "id": "call_1",
                            "function": {"name": "read_file", "arguments": "{\"path\":\"b\"}"}
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let mut content_index = 0;
        let mut text_started = false;
        let mut thinking_started = false;
        let mut tool_indices: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut reasoning_detail_buffers = std::collections::HashMap::new();
        let events = parse_sse_chunk(
            &chunk,
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_detail_buffers,
            false,
        );

        let starts: Vec<u32> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockStart {
                    index,
                    content_block: ContentBlockStart::ToolUse { .. },
                } => Some(*index),
                _ => None,
            })
            .collect();
        let stops: Vec<u32> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockStop { index } => Some(*index),
                _ => None,
            })
            .collect();
        let deltas: Vec<u32> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockDelta {
                    index,
                    delta: Delta::InputJsonDelta { .. },
                } => Some(*index),
                _ => None,
            })
            .collect();

        assert_eq!(starts, vec![0, 1]);
        assert_eq!(stops, vec![0, 1]);
        assert_eq!(deltas, vec![0, 1]);
    }

    #[test]
    fn parse_sse_chunk_handles_empty_choices_usage_chunk() {
        let chunk = json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "prompt_cache_hit_tokens": 70,
                "prompt_cache_miss_tokens": 30
            }
        });

        let mut content_index = 0;
        let mut text_started = false;
        let mut thinking_started = false;
        let mut tool_indices: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut reasoning_detail_buffers = std::collections::HashMap::new();
        let events = parse_sse_chunk(
            &chunk,
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_detail_buffers,
            false,
        );

        let StreamEvent::MessageDelta {
            usage: Some(usage), ..
        } = &events[0]
        else {
            panic!("expected usage delta");
        };
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.prompt_cache_hit_tokens, Some(70));
        assert_eq!(usage.prompt_cache_miss_tokens, Some(30));
    }

    #[test]
    fn chat_messages_include_tool_results_when_call_present() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        redacted_data: None,
                        thinking: "Need to inspect the directory".to_string(),
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "tool-1".to_string(),
                        name: "list_dir".to_string(),
                        input: json!({}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "tool-1".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let out = build_chat_messages(None, &messages, "deepseek-v4-flash");
        assert!(
            out.iter()
                .any(|value| { value.get("role").and_then(Value::as_str) == Some("tool") })
        );
        let assistant = out
            .iter()
            .find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant message");
        assert!(assistant.get("tool_calls").is_some());
    }

    #[test]
    fn chat_messages_encode_tool_call_names() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        redacted_data: None,
                        thinking: "Need to search".to_string(),
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "tool-1".to_string(),
                        name: "web.run".to_string(),
                        input: json!({}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "tool-1".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let out = build_chat_messages(None, &messages, "deepseek-v4-flash");
        let assistant = out
            .iter()
            .find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant message");
        let tool_calls = assistant
            .get("tool_calls")
            .and_then(Value::as_array)
            .expect("tool_calls array");
        let function_name = tool_calls
            .first()
            .and_then(|call| call.get("function"))
            .and_then(|func| func.get("name"))
            .and_then(Value::as_str)
            .expect("tool call function name");

        assert_eq!(function_name, to_api_tool_name("web.run"));
    }

    #[test]
    fn chat_messages_strips_orphaned_tool_calls_after_compaction() {
        // Simulates post-compaction state: assistant has tool_calls but the
        // tool result messages were summarized away.
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: "tool-orphan".to_string(),
                    name: "read_file".to_string(),
                    input: json!({"path": "src/main.rs"}),
                    caller: None,
                    thought_signature: None,
                }],
            },
            // No tool result follows — it was removed by compaction.
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "continue".to_string(),
                    cache_control: None,
                }],
            },
        ];

        let out = build_chat_messages(None, &messages, "deepseek-v4-flash");
        let assistant = out
            .iter()
            .find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"));
        // The safety net may drop the assistant message entirely if it only
        // contained orphaned tool_calls and no text content.
        assert!(
            assistant.is_none(),
            "assistant without content/tool_calls should be removed"
        );
        assert!(
            !out.iter()
                .any(|v| v.get("role").and_then(Value::as_str) == Some("tool")),
            "orphaned tool results should also be removed"
        );
    }

    #[test]
    fn chat_messages_keeps_valid_tool_calls_intact() {
        // Complete call+result pair should NOT be stripped.
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        redacted_data: None,
                        thinking: "Need to list files".to_string(),
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "tool-ok".to_string(),
                        name: "list_dir".to_string(),
                        input: json!({}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "tool-ok".to_string(),
                    content: "files".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let out = build_chat_messages(None, &messages, "deepseek-v4-flash");
        let assistant = out
            .iter()
            .find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant message");
        assert!(
            assistant.get("tool_calls").is_some(),
            "valid tool_calls should remain intact"
        );
        assert!(
            out.iter()
                .any(|value| value.get("role").and_then(Value::as_str) == Some("tool")),
            "tool result should remain"
        );
    }

    #[test]
    fn chat_messages_strips_partial_tool_results() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "t1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "a.rs"}),
                        caller: None,
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "t2".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "b.rs"}),
                        caller: None,
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "t3".to_string(),
                        name: "shell".to_string(),
                        input: json!({"cmd": "ls"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "t1".to_string(),
                    content: "content a".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "t2".to_string(),
                    content: "content b".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            // No result for t3
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "continue".to_string(),
                    cache_control: None,
                }],
            },
        ];

        let out = build_chat_messages(None, &messages, "deepseek-v4-flash");
        let assistant = out
            .iter()
            .find(|v| v.get("role").and_then(Value::as_str) == Some("assistant"));
        assert!(
            assistant.is_none(),
            "assistant with only partial tool_calls should be removed"
        );
        assert!(
            !out.iter()
                .any(|v| v.get("role").and_then(Value::as_str) == Some("tool")),
            "all orphaned tool results should be removed"
        );
    }

    #[test]
    fn codewhale_models_listing_carries_the_wire_protocol_per_model() {
        let payload = r#"{
                "object": "list",
                "data": [
                    {"id": "deepseek/deepseek-v4-pro", "object": "model", "owned_by": "deepseek",
                     "codewhale": {"provider": "deepseek", "model": "deepseek-v4-pro",
                                   "protocol": "chat-completions",
                                   "endpoint": "/v1/chat/completions",
                                   "default": true, "usable": true}},
                    {"id": "anthropic/claude-sonnet-5", "object": "model", "owned_by": "anthropic",
                     "codewhale": {"provider": "anthropic", "model": "claude-sonnet-5",
                                   "protocol": "anthropic-messages",
                                   "endpoint": "/v1/messages", "usable": true}},
                    {"id": "openai/gpt-5.6", "object": "model", "owned_by": "openai",
                     "codewhale": {"provider": "openai", "model": "gpt-5.6",
                                   "protocol": "responses",
                                   "endpoint": "/v1/responses", "usable": true}},
                    {"id": "xai/grok-4.6", "object": "model", "owned_by": "xai",
                     "codewhale": {"provider": "xai", "model": "grok-4.6",
                                   "protocol": "some-future-wire", "usable": true}},
                    {"id": "deepseek/deepseek-v4-pro", "object": "model"},
                    {"id": "   ", "object": "model"},
                    {"id": "anthropic/claude-opus-4-8", "object": "model"},
                    {"id": "codewhale/Qwen3.8-27B", "object": "model", "owned_by": "codewhale",
                     "codewhale": {"provider": "codewhale", "model": "Qwen3.8-27B",
                                   "protocol": "chat-completions", "usable": true,
                                   "limits": {"maxInputTokens": 7168, "maxOutputTokens": 1024,
                                              "maxContextTokens": 16384}}}
                ]
            }"#;

        let rows = codewhale_catalog_offerings_from_body(payload, "codewhale", "fp", 7)
            .expect("the account listing should parse");
        let by_id: std::collections::BTreeMap<&str, &CatalogOffering> = rows
            .iter()
            .map(|row| (row.wire_model_id.as_str(), row))
            .collect();
        assert_eq!(rows.len(), 6, "blank and duplicate ids are dropped");
        // The protocol comes from the response, not from a compiled roster.
        assert_eq!(by_id["deepseek/deepseek-v4-pro"].endpoint_key, "chat");
        assert!(by_id["deepseek/deepseek-v4-pro"].default_for_provider);
        assert_eq!(by_id["anthropic/claude-sonnet-5"].endpoint_key, "messages");
        assert!(!by_id["anthropic/claude-sonnet-5"].default_for_provider);
        assert_eq!(by_id["openai/gpt-5.6"].endpoint_key, "responses");
        // A protocol label this build predates is not an error: the row falls
        // back to the namespace rule so a forward-compatible catalog stays
        // reachable.
        assert_eq!(by_id["xai/grok-4.6"].endpoint_key, "chat");
        // A row with no `codewhale` block still resolves through the namespace
        // rule rather than being dropped: the account listed it.
        assert_eq!(by_id["anthropic/claude-opus-4-8"].endpoint_key, "messages");
        // Nothing is claimed that the account service did not state: only a
        // row that advertises `codewhale.limits` carries limits, and they are
        // the advertised numbers.
        assert!(
            rows.iter()
                .all(|row| row.canonical_model.is_none() && row.cost.is_none())
        );
        assert!(
            rows.iter()
                .filter(|row| row.wire_model_id != "codewhale/Qwen3.8-27B")
                .all(|row| row.limit.is_none())
        );
        let qwen = by_id["codewhale/Qwen3.8-27B"]
            .limit
            .as_ref()
            .expect("advertised limits");
        assert_eq!(
            (qwen.context, qwen.input, qwen.output),
            (Some(16384), Some(7168), Some(1024))
        );

        assert_eq!(
            codewhale_catalog_offerings_from_body(
                r#"{"object":"list","data":[]}"#,
                "codewhale",
                "fp",
                7
            ),
            Err(CatalogRefreshError::EmptyList)
        );
        assert_eq!(
            codewhale_catalog_offerings_from_body("not json", "codewhale", "fp", 7),
            Err(CatalogRefreshError::InvalidResponse)
        );
    }

    #[test]
    fn parse_models_response_parses_and_deduplicates() {
        let payload = r#"{
                "object": "list",
                "data": [
                    {"id": "deepseek-v4-pro", "object": "model", "owned_by": "deepseek", "created": 1},
                    {"id": "deepseek-v4-flash", "object": "model"},
                    {"id": "deepseek-v4-pro", "object": "model", "owned_by": "deepseek", "created": 1}
                ]
            }"#;

        let models = parse_models_response(payload).expect("parse models");
        assert_eq!(
            models,
            vec![
                AvailableModel {
                    id: "deepseek-v4-flash".to_string(),
                    owned_by: None,
                    created: None,
                    display_name: None
                },
                AvailableModel {
                    id: "deepseek-v4-pro".to_string(),
                    owned_by: Some("deepseek".to_string()),
                    created: Some(1),
                    display_name: None
                }
            ]
        );
    }

    #[test]
    fn parse_models_response_accepts_ollama_tag_ids() {
        let payload = r#"{
                "object": "list",
                "data": [
                    {"id": "qwen2.5-coder:7b", "object": "model", "owned_by": "library"},
                    {"id": "deepseek-coder-v2:16b", "object": "model"}
                ]
            }"#;

        let models = parse_models_response(payload).expect("parse models");
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["deepseek-coder-v2:16b", "qwen2.5-coder:7b"]
        );
    }

    // === #3385: provider live /models fetch + secret-free cache ==============
    //
    // All model ids below are SYNTHETIC (never real vendor model names), per the
    // issue's anti-hardcoding rule.

    /// Build a client whose OpenRouter base URL points at a mock server.
    pub(super) fn openrouter_client_for(server: &MockServer) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        CodewhaleClient::new(&{
            let __cw_config_init_0_0 = Some("openrouter".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: Some(server.uri()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        })
        .expect("openrouter client")
    }

    pub(super) fn custom_mock_client_for_identity(
        server: &MockServer,
        identity: &str,
    ) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut providers = ProvidersConfig::default();
        providers.custom.insert(
            identity.to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                api_key: Some("test-custom-key".to_string()),
                base_url: Some(format!("{}/v1", server.uri())),
                model: Some("synthetic/custom-model".to_string()),
                ..ProviderConfig::default()
            },
        );
        CodewhaleClient::new(&{
            let __cw_config_init_1_0 = Some(identity.to_string());
            let __cw_config_init_1_1 = Some(providers);
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        })
        .expect("Baseten client")
    }

    pub(super) fn opencode_go_client_for(server: &MockServer) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        CodewhaleClient::new(&{
            let __cw_config_init_2_0 = Some("opencode-go".to_string());
            let __cw_config_init_2_1 = Some(ProvidersConfig {
                opencode_go: ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: Some(server.uri()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        })
        .expect("OpenCode Go client")
    }

    fn telecomjs_client_for(server: &MockServer) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        CodewhaleClient::new(&{
            let __cw_config_init_3_0 = Some("telecomjs".to_string());
            let __cw_config_init_3_1 = Some(ProvidersConfig {
                telecomjs: ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: Some(server.uri()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        })
        .expect("TelecomJS client")
    }

    fn edenai_client_for(server: &MockServer) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        CodewhaleClient::new(&{
            let __cw_config_init_4_0 = Some("edenai".to_string());
            let __cw_config_init_4_1 = Some(ProvidersConfig {
                edenai: ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: Some(format!("{}/v3", server.uri())),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_4_value: Config = Config::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value.providers = __cw_config_init_4_1;
            __cw_config_init_4_value
        })
        .expect("Eden AI client")
    }

    pub(super) async fn mount_models_json(server: &MockServer, status: u16, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn verify_provider_scenario() {
        // Scenario consolidation of: verify_provider_api_key_accepts_mocked_models_success, verify_provider_api_key_returns_status_and_unicode_body_without_panic
        // from verify_provider_api_key_accepts_mocked_models_success
        {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/models"))
                .and(header("authorization", "Bearer test-key"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
                .mount(&server)
                .await;

            verify_provider_api_key(ProviderKind::Openrouter, "test-key", &server.uri())
                .await
                .expect("mocked /models success should verify");
        }
        // from verify_provider_api_key_returns_status_and_unicode_body_without_panic
        {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/models"))
                .respond_with(ResponseTemplate::new(401).set_body_string("密钥无效"))
                .mount(&server)
                .await;

            let err = verify_provider_api_key(ProviderKind::Openrouter, "bad-key", &server.uri())
                .await
                .expect_err("mocked /models failure should be reported");

            assert!(err.contains("HTTP 401"), "status is preserved: {err}");
            assert!(err.contains("密钥无效"), "unicode body is preserved: {err}");
        }
    }

    #[test]
    fn opencode_go_client_rejects_unknown_protocol_models() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        for model in ["claude-unproven", "gpt-unlisted"] {
            let config = {
                let __cw_config_init_5_0 = Some("opencode-go".to_string());
                let __cw_config_init_5_1 = Some(ProvidersConfig {
                    opencode_go: ProviderConfig {
                        api_key: Some("test-key".to_string()),
                        model: Some(model.to_string()),
                        ..ProviderConfig::default()
                    },
                    ..ProvidersConfig::default()
                });
                let mut __cw_config_init_5_value: Config = Config::default();
                __cw_config_init_5_value.provider = __cw_config_init_5_0;
                __cw_config_init_5_value.providers = __cw_config_init_5_1;
                __cw_config_init_5_value
            };
            let err = CodewhaleClient::new(&config)
                .err()
                .expect("unknown protocol must fail before client construction");
            assert!(err.to_string().contains(model), "{err:#}");
        }
    }

    #[tokio::test]
    async fn opencode_go_live_model_paths_keep_documented_protocol_rows() {
        let server = MockServer::start().await;
        let mut rows: Vec<_> = codewhale_config::opencode_go_models()
            .iter()
            .map(|id| json!({"id": id}))
            .collect();
        rows.extend([
            json!({"id": "minimax-m3"}),
            json!({"id": "minimax-m2.7"}),
            json!({"id": "minimax-m2.5"}),
            json!({"id": "qwen3.7-max"}),
            json!({"id": "qwen3.7-plus"}),
            json!({"id": "qwen3.6-plus"}),
        ]);
        mount_models_json(&server, 200, json!({"data": rows})).await;
        let client = opencode_go_client_for(&server);

        let listed = client.list_models().await.expect("filtered model list");
        let listed: std::collections::BTreeSet<_> = listed.into_iter().map(|model| model.id).collect();
        let expected: std::collections::BTreeSet<_> = codewhale_config::opencode_go_models()
            .iter()
            .map(|model| (*model).to_string())
            .collect();
        assert_eq!(listed, expected);

        let delta = client.fetch_catalog_delta().await.expect("filtered delta");
        assert_eq!(delta.provider, "opencode-go");
        let delta_ids: std::collections::BTreeSet<_> = delta
            .offerings
            .iter()
            .map(|offering| offering.wire_model_id.clone())
            .collect();
        assert_eq!(delta_ids, expected);
        assert!(
            delta
                .offerings
                .iter()
                .all(|offering| Some(offering.endpoint_key.as_str())
                    == codewhale_config::opencode_go_endpoint_key(&offering.wire_model_id))
        );
    }

    #[tokio::test]
    async fn telecomjs_live_catalog_keeps_cross_provider_metadata_unknown() {
        let server = MockServer::start().await;
        let ambiguous_id = codewhale_config::catalog::bundled_catalog_offerings()
            .into_iter()
            .find(|offering| {
                !offering.provider.eq_ignore_ascii_case("telecomjs")
                    && !offering
                        .wire_model_id
                        .eq_ignore_ascii_case(DEFAULT_TELECOMJS_MODEL)
                    && (offering.canonical_model.is_some()
                        || offering.family.is_some()
                        || offering.limit.is_some()
                        || offering.cost.is_some()
                        || offering.reasoning.is_some()
                        || offering.tool_call.is_some())
            })
            .expect("bundled catalog should contain a metadata-bearing non-TelecomJS row")
            .wire_model_id;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": ambiguous_id.clone()},
                    {"id": DEFAULT_TELECOMJS_MODEL}
                ]
            })))
            .mount(&server)
            .await;

        let delta = telecomjs_client_for(&server)
            .fetch_catalog_delta()
            .await
            .expect("TelecomJS catalog delta");
        assert_eq!(delta.provider, "telecomjs");
        assert_eq!(delta.offerings.len(), 2);

        let ambiguous = delta
            .offerings
            .iter()
            .find(|offering| offering.wire_model_id == ambiguous_id)
            .expect("ambiguous cross-provider id");
        assert!(!ambiguous.default_for_provider);
        assert_eq!(ambiguous.endpoint_key, "chat");
        assert_eq!(ambiguous.canonical_model, None);
        assert_eq!(ambiguous.family, None);
        assert_eq!(ambiguous.limit, None);
        assert_eq!(ambiguous.cost, None);
        assert_eq!(ambiguous.modalities, None);
        assert_eq!(ambiguous.attachment, None);
        assert_eq!(ambiguous.reasoning, None);
        assert_eq!(ambiguous.tool_call, None);
        assert_eq!(ambiguous.structured_output, None);
        assert!(ambiguous.reasoning_options.is_empty());
        assert!(matches!(ambiguous.source, CatalogSource::Live { .. }));

        let default = delta
            .offerings
            .iter()
            .find(|offering| offering.wire_model_id == DEFAULT_TELECOMJS_MODEL)
            .expect("TelecomJS default row");
        assert!(default.default_for_provider);
    }

    #[tokio::test]
    async fn edenai_live_catalog_marks_the_default_and_keeps_unknowns_unclaimed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/models"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "synthetic/vendor-model"},
                    {"id": DEFAULT_EDENAI_MODEL}
                ]
            })))
            .mount(&server)
            .await;

        let delta = edenai_client_for(&server)
            .fetch_catalog_delta()
            .await
            .expect("Eden AI catalog delta");
        assert_eq!(delta.provider, "edenai");
        assert_eq!(delta.offerings.len(), 2);

        let unknown = delta
            .offerings
            .iter()
            .find(|offering| offering.wire_model_id == "synthetic/vendor-model")
            .expect("synthetic Eden AI row");
        assert_eq!(unknown.canonical_model, None);
        assert_eq!(unknown.reasoning, None);
        assert_eq!(unknown.tool_call, None);
        assert!(!unknown.default_for_provider);

        let default = delta
            .offerings
            .iter()
            .find(|offering| offering.wire_model_id == DEFAULT_EDENAI_MODEL)
            .expect("Eden AI default row");
        assert!(default.default_for_provider);
    }

    #[tokio::test]
    async fn fetch_catalog_delta_success_builds_scoped_secret_free_live_delta() {
        let server = MockServer::start().await;
        mount_models_json(
            &server,
            200,
            json!({"data": [
                {"id": "synthetic-model-alpha", "owned_by": "synthetic-owner"},
                {"id": "synthetic-model-beta"}
            ]}),
        )
        .await;
        let client = openrouter_client_for(&server);

        let delta = client.fetch_catalog_delta().await.expect("delta");
        assert_eq!(delta.provider, "openrouter");
        assert_eq!(
            delta.base_url_fingerprint,
            base_url_fingerprint(&server.uri()),
            "delta is scoped to the base-URL fingerprint"
        );
        let ids: Vec<&str> = delta
            .offerings
            .iter()
            .map(|offering| offering.wire_model_id.as_str())
            .collect();
        assert!(ids.contains(&"synthetic-model-alpha"), "ids: {ids:?}");
        assert!(ids.contains(&"synthetic-model-beta"), "ids: {ids:?}");
        for offering in &delta.offerings {
            // Live rows carry honest provenance and no inferred facts/secrets.
            assert!(matches!(offering.source, CatalogSource::Live { .. }));
            assert_eq!(offering.canonical_model, None);
            assert_eq!(offering.cost, None);
            assert!(offering.reasoning.is_none());
        }
    }

    /// #6690: rows shaped like the live OpenRouter roster. A `~` "latest"
    /// alias, a router's `"-1"` variable price, an undecodable row and an id
    /// the catalog cannot hold each used to fail the whole refresh closed
    /// (`invalid_response`), so the lake never went fresh and no OpenRouter
    /// turn could be priced. Each now costs only its own row.
    #[tokio::test]
    async fn fetch_catalog_delta_skips_openrouter_latest_aliases_and_keeps_prices() {
        let server = MockServer::start().await;
        mount_models_json(
            &server,
            200,
            json!({"data": [
                {"id": "~deepseek/deepseek-pro-latest",
                 "pricing": {"prompt": "0.0000002523", "completion": "0.0000035",
                             "input_cache_read": "0.0000002518"}},
                {"id": "openrouter/auto", "context_length": 2000000,
                 "pricing": {"prompt": "-1", "completion": "-1"},
                 "top_provider": {"context_length": null, "max_completion_tokens": null}},
                {"id": "synthetic/malformed-row", "context_length": "very long",
                 "pricing": {"prompt": "0.000001", "completion": "0.000002"}},
                {"id": "synthetic/unreadable-price",
                 "pricing": {"prompt": "not-a-number", "completion": "0.000002"}},
                {"id": "synthetic/bad id"},
                {"id": "deepseek/deepseek-v4-pro", "context_length": 1048576,
                 "pricing": {"prompt": "0.00000095526", "completion": "0.00000191052",
                             "input_cache_read": "0.000000079605"},
                 "top_provider": {"context_length": 1024000, "max_completion_tokens": 384000}}
            ]}),
        )
        .await;
        let client = openrouter_client_for(&server);

        let delta = client
            .fetch_catalog_delta()
            .await
            .expect("a bad row must not fail the whole roster");
        let ids: Vec<&str> = delta
            .offerings
            .iter()
            .map(|offering| offering.wire_model_id.as_str())
            .collect();
        assert_eq!(ids, ["openrouter/auto", "deepseek/deepseek-v4-pro"]);
        assert_eq!(delta.offerings[0].cost, None, "router price is variable");
        let cost = delta.offerings[1].cost.as_ref().expect("served price");
        let close = |got: Option<f64>, want: f64| got.is_some_and(|got| (got - want).abs() < 1e-9);
        assert!(close(cost.input, 0.95526), "{cost:?}");
        assert!(close(cost.output, 1.91052), "{cost:?}");
        assert!(close(cost.cache_read, 0.079605), "{cost:?}");
        assert_eq!(cost.cache_write, None);

        // Only structurally invalid responses still fail the refresh.
        for body in [
            json!({"data": {"id": "deepseek/deepseek-v4-pro"}}),
            json!({"models": []}),
            json!({"data": [{"name": "no id"}, {"id": "synthetic/bad id"}]}),
        ] {
            let server = MockServer::start().await;
            mount_models_json(&server, 200, body.clone()).await;
            assert_eq!(
                openrouter_client_for(&server)
                    .fetch_catalog_delta()
                    .await
                    .expect_err("structurally invalid roster"),
                CatalogRefreshError::InvalidResponse,
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_catalog_scenario() {
        // Scenario consolidation of: fetch_catalog_delta_maps_http_statuses_to_typed_errors, fetch_catalog_delta_maps_invalid_json_and_empty_list
        // from fetch_catalog_delta_maps_http_statuses_to_typed_errors
        {
            for (status, expected) in [
                (401u16, CatalogRefreshError::Unauthorized),
                (403, CatalogRefreshError::Forbidden),
                (404, CatalogRefreshError::NotFound),
                (429, CatalogRefreshError::RateLimited),
                (500, CatalogRefreshError::Network),
            ] {
                let server = MockServer::start().await;
                mount_models_json(&server, status, json!({"error": "nope"})).await;
                let client = openrouter_client_for(&server);
                let err = client.fetch_catalog_delta().await.expect_err("should fail");
                assert_eq!(err, expected, "status {status} should map to {expected:?}");
            }
        }
        // from fetch_catalog_delta_maps_invalid_json_and_empty_list
        {
            // Invalid JSON -> InvalidResponse.
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/models"))
                .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
                .mount(&server)
                .await;
            let client = openrouter_client_for(&server);
            assert_eq!(
                client
                    .fetch_catalog_delta()
                    .await
                    .expect_err("invalid json"),
                CatalogRefreshError::InvalidResponse
            );

            // Empty list -> EmptyList.
            let server = MockServer::start().await;
            mount_models_json(&server, 200, json!({"data": []})).await;
            let client = openrouter_client_for(&server);
            assert_eq!(
                client.fetch_catalog_delta().await.expect_err("empty list"),
                CatalogRefreshError::EmptyList
            );
        }
    }
