
    async fn assert_zai_request_truth(streaming: bool) {
        for base_url in [
            crate::config::DEFAULT_ZAI_BASE_URL,
            "https://api.z.ai/api/paas/v4",
        ] {
            let (high_path, high) = capture_zai_chat_request(
                base_url,
                crate::config::ZAI_GLM_5_2_MODEL,
                Some("high"),
                streaming,
            )
            .await;
            let (max_path, max) = capture_zai_chat_request(
                base_url,
                crate::config::ZAI_GLM_5_2_MODEL,
                Some("max"),
                streaming,
            )
            .await;
            assert_eq!(high_path, "/v1/chat/completions");
            assert_eq!(max_path, "/v1/chat/completions");
            assert_eq!(high["reasoning_effort"], "high", "{base_url}: {high}");
            assert_eq!(max["reasoning_effort"], "max", "{base_url}: {max}");
            for body in [&high, &max] {
                assert_eq!(
                    body["thinking"],
                    json!({"type": "enabled", "clear_thinking": false}),
                    "{base_url}: {body}"
                );
                assert_eq!(body["model"], crate::config::ZAI_GLM_5_2_MODEL);
            }
            let mut high_without_effort = high.clone();
            let mut max_without_effort = max.clone();
            high_without_effort
                .as_object_mut()
                .expect("object")
                .remove("reasoning_effort");
            max_without_effort
                .as_object_mut()
                .expect("object")
                .remove("reasoning_effort");
            assert_eq!(high_without_effort, max_without_effort);

            for model in [
                crate::config::ZAI_GLM_5_1_MODEL,
                crate::config::ZAI_GLM_5_TURBO_MODEL,
            ] {
                for requested in ["high", "max"] {
                    let (_, toggle_only) =
                        capture_zai_chat_request(base_url, model, Some(requested), streaming).await;
                    assert!(
                        toggle_only.get("reasoning_effort").is_none(),
                        "{model}: {toggle_only}"
                    );
                    assert_eq!(
                        toggle_only["thinking"],
                        json!({"type": "enabled", "clear_thinking": false}),
                        "{model}: {toggle_only}"
                    );
                }
            }

            let (_, unknown) =
                capture_zai_chat_request(base_url, "glm-future-unknown", Some("max"), streaming).await;
            assert!(unknown.get("reasoning_effort").is_none(), "{unknown}");
            assert!(unknown.get("thinking").is_none(), "{unknown}");
        }

        let (_, gateway) = capture_zai_chat_request(
            "https://gateway.example/v1",
            crate::config::ZAI_GLM_5_2_MODEL,
            Some("max"),
            streaming,
        )
        .await;
        assert!(gateway.get("reasoning_effort").is_none(), "{gateway}");
        assert!(gateway.get("thinking").is_none(), "{gateway}");

        let (_, gateway_turbo) = capture_zai_chat_request(
            "https://gateway.example/v1",
            crate::config::ZAI_GLM_5_TURBO_MODEL,
            Some("max"),
            streaming,
        )
        .await;
        assert!(
            gateway_turbo.get("reasoning_effort").is_none(),
            "{gateway_turbo}"
        );
        assert!(gateway_turbo.get("thinking").is_none(), "{gateway_turbo}");
    }

    async fn assert_minimax_request_truth(streaming: bool) {
        for base_url in [
            crate::config::DEFAULT_MINIMAX_BASE_URL,
            "https://api.minimaxi.com/v1",
        ] {
            for (effort, expected_thinking) in [
                ("off", json!({"type": "disabled"})),
                ("high", json!({"type": "adaptive"})),
                ("max", json!({"type": "adaptive"})),
            ] {
                let (_, body) = capture_minimax_chat_request(
                    base_url,
                    crate::config::DEFAULT_MINIMAX_MODEL,
                    Some(effort),
                    streaming,
                )
                .await;
                assert_eq!(
                    body["max_completion_tokens"], 64,
                    "{base_url} {effort}: {body}"
                );
                assert!(
                    body.get("max_tokens").is_none(),
                    "{base_url} {effort}: {body}"
                );
                assert_eq!(body["reasoning_split"], true, "{base_url}: {body}");
                assert_eq!(
                    body["thinking"], expected_thinking,
                    "{base_url} {effort}: {body}"
                );
            }
        }

        for (base_url, model) in [
            (crate::config::DEFAULT_MINIMAX_BASE_URL, "MiniMax-M2"),
            (
                "https://gateway.example/v1",
                crate::config::DEFAULT_MINIMAX_MODEL,
            ),
        ] {
            for effort in ["off", "high", "max"] {
                let (_, body) =
                    capture_minimax_chat_request(base_url, model, Some(effort), streaming).await;
                assert_eq!(
                    body["max_tokens"], 64,
                    "{base_url} {model} {effort}: {body}"
                );
                assert!(
                    body.get("max_completion_tokens").is_none(),
                    "{base_url} {model} {effort}: {body}"
                );
                assert!(
                    body.get("reasoning_split").is_none(),
                    "{base_url} {model} {effort}: {body}"
                );
                assert!(
                    body.get("thinking").is_none(),
                    "{base_url} {model} {effort}: {body}"
                );
            }
        }
    }

    async fn assert_k3_request_json_route_boundaries(streaming: bool) {
        for (requested, expected) in [("off", "low"), ("high", "high"), ("max", "max")] {
            let body = capture_moonshot_chat_request(
                crate::config::DEFAULT_MOONSHOT_BASE_URL,
                crate::config::MOONSHOT_KIMI_K3_MODEL,
                Some(requested),
                streaming,
            )
            .await;
            assert_eq!(body["reasoning_effort"], json!(expected), "{body}");
            assert!(body.get("thinking").is_none(), "{body}");
            assert_eq!(body["max_completion_tokens"], json!(64), "{body}");
            assert!(body.get("max_tokens").is_none(), "{body}");
            assert!(body.get("temperature").is_none(), "{body}");
            assert!(body.get("top_p").is_none(), "{body}");
            assert_eq!(
                body.get("stream").and_then(Value::as_bool),
                streaming.then_some(true)
            );
        }

        for (requested, expected) in [
            ("off", Some(json!({"type": "enabled", "effort": "low"}))),
            ("max", Some(json!({"type": "enabled", "effort": "max"}))),
        ] {
            let membership = capture_moonshot_chat_request(
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                crate::config::KIMI_CODE_K3_MODEL,
                Some(requested),
                streaming,
            )
            .await;
            match expected {
                Some(thinking) => assert_eq!(membership["thinking"], thinking, "{membership}"),
                None => assert!(membership.get("thinking").is_none(), "{membership}"),
            }
            assert!(membership.get("reasoning_effort").is_none(), "{membership}");
            assert_eq!(membership["max_tokens"], json!(64), "{membership}");
            assert!(
                membership.get("max_completion_tokens").is_none(),
                "{membership}"
            );
            // Kimi Code's documented membership models own their sampling
            // behavior: the exact first-party membership route strips generic
            // controls (apply_kimi_code_fixed_sampling).
            assert!(membership.get("temperature").is_none(), "{membership}");
            assert!(membership.get("top_p").is_none(), "{membership}");
        }

        let provider_default = capture_moonshot_chat_request(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            None,
            streaming,
        )
        .await;
        assert!(
            provider_default.get("thinking").is_none(),
            "only a genuinely omitted effort leaves the provider default in control: {provider_default}"
        );
        assert!(provider_default.get("reasoning_effort").is_none());

        let neighbor = capture_moonshot_chat_request(
            "https://proxy.example/v1",
            crate::config::MOONSHOT_KIMI_K3_MODEL,
            Some("max"),
            streaming,
        )
        .await;
        assert_eq!(
            neighbor["thinking"],
            json!({"type": "enabled"}),
            "{neighbor}"
        );
        assert!(neighbor.get("reasoning_effort").is_none(), "{neighbor}");
        assert!(neighbor.pointer("/thinking/effort").is_none(), "{neighbor}");
        assert_eq!(neighbor["max_tokens"], json!(64), "{neighbor}");
        assert!(
            neighbor.get("max_completion_tokens").is_none(),
            "{neighbor}"
        );
        assert_eq!(neighbor["temperature"], json!(0.25), "{neighbor}");
        assert_eq!(neighbor["top_p"], json!(0.75), "{neighbor}");
    }

    async fn assert_kimi_code_raw_off_replays_tool_history(streaming: bool) {
        let mut request = k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("off"), streaming);
        request.messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "Inspect the saved tool state".to_string(),
                        signature: None,
                        state: None,
                        redacted_data: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call-k3-replay".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "src/lib.rs"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-k3-replay".to_string(),
                    content: "file contents".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let body = capture_moonshot_chat_request_body(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            request,
        )
        .await;
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "effort": "low"}),
            "raw Off must still normalize to K3's always-thinking low tier: {body}"
        );
        let assistant = body["messages"]
            .as_array()
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["role"] == "assistant")
            })
            .expect("captured assistant tool-call history");
        assert_eq!(
            assistant["reasoning_content"],
            json!("Inspect the saved tool state"),
            "exact membership K3 must replay reasoning even for a stale raw Off caller: {body}"
        );
        assert!(assistant["tool_calls"].is_array(), "{assistant}");
    }

    async fn assert_kimi_code_apply_patch_schema_is_mfjs_compatible(streaming: bool) {
        let mut request = k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), streaming);
        request.tools = Some(vec![apply_patch_request_tool()]);

        let body = capture_moonshot_chat_request_body(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            request,
        )
        .await;
        let function = &body["tools"][0]["function"];
        let parameters = &function["parameters"];
        assert_eq!(parameters["type"], "object", "{parameters}");
        assert!(parameters.get("oneOf").is_none(), "{parameters}");
        assert!(parameters.get("anyOf").is_none(), "{parameters}");
        assert!(parameters.get("allOf").is_none(), "{parameters}");
        assert_eq!(parameters["properties"]["patch"]["type"], "string");
        assert_eq!(parameters["properties"]["replace"]["type"], "array");
        assert_eq!(parameters["properties"]["changes"]["type"], "array");
        assert!(
            function["description"]
                .as_str()
                .is_some_and(|description| description
                    .contains("Exactly one of these parameter groups must be provided")),
            "the relaxed wire schema must preserve the runtime constraint in its description: {function}"
        );
    }

    // Per-tool degradation regression: the old behavior failed the whole
    // request before transport when any tool's parameters failed MFJS
    // validation; now only the incompatible tool is dropped from the wire
    // body and the request still sends, so one bad MCP server cannot sink
    // every Moonshot-routed turn.
    async fn assert_kimi_code_invalid_root_ref_drops_only_that_tool(streaming: bool) {
        let mut request = k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), streaming);
        let mut tool = test_tool("private_schema_tool");
        tool.input_schema = json!({
            "$ref": "#/$defs/private-root-name-3158",
            "$defs": {}
        });
        request.tools = Some(vec![tool]);
        request.tool_choice = Some(json!("auto"));

        let body = capture_moonshot_chat_request_body(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            request,
        )
        .await;
        assert!(
            body.get("tools").is_none(),
            "the only tool was dropped, so the wire body must omit tools entirely: {body}"
        );
        assert!(
            body.get("tool_choice").is_none(),
            "tool_choice must not be sent when every tool was dropped: {body}"
        );
        assert!(
            !body.to_string().contains("private-root-name-3158"),
            "the wire body must not leak the private $ref value: {body}"
        );
    }

    async fn assert_kimi_code_untyped_default_drops_only_that_tool(streaming: bool) {
        let mut request = k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), streaming);
        let mut tool = test_tool("private_default_tool");
        tool.input_schema = json!({
            "type": "object",
            "properties": {
                "private-field-4401": {
                    "default": "private-default-value-4402"
                }
            }
        });
        request.tools = Some(vec![tool]);

        let body = capture_moonshot_chat_request_body(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            request,
        )
        .await;
        assert!(
            body.get("tools").is_none(),
            "the only tool was dropped, so the wire body must omit tools entirely: {body}"
        );
        assert!(
            body.get("tool_choice").is_none(),
            "tool_choice must not be sent when every tool was dropped: {body}"
        );
        assert!(!body.to_string().contains("private-field-4401"));
        assert!(!body.to_string().contains("private-default-value-4402"));
    }

    async fn assert_kimi_code_streams_mfjs_safe_deferred_dynamic_tool() {
        let tool = deferred_dynamic_request_tool();
        assert_eq!(tool.defer_loading, Some(true));
        assert_eq!(
            tool.input_schema["properties"]["query"]["nullable"], true,
            "ToolRegistry must exercise the provider-neutral nullable collapse"
        );
        assert!(
            tool.input_schema["properties"]["query"]
                .get("anyOf")
                .is_none()
        );
        assert_eq!(tool.input_schema["properties"]["mode"]["const"], "fast");

        let mut request = k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), true);
        request.tools = Some(vec![tool]);
        let body = capture_moonshot_chat_request_body(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            request,
        )
        .await;

        assert_eq!(
            body["stream"], true,
            "this must exercise the SSE path: {body}"
        );
        let parameters = &captured_function(&body, "deferred_lookup")["parameters"];
        assert_eq!(parameters["properties"]["mode"]["enum"], json!(["fast"]));
        assert!(
            parameters["properties"]["mode"].get("const").is_none(),
            "{parameters}"
        );
        assert_eq!(
            parameters["properties"]["query"]["anyOf"],
            json!([{"type": "string"}, {"type": "null"}])
        );
        assert!(
            parameters["properties"]["query"].get("nullable").is_none(),
            "{parameters}"
        );
        crate::tools::schema_sanitize::validate_mfjs_parameters(parameters).unwrap();
    }

    async fn assert_kimi_code_captures_exact_general_child_catalog() {
        let tools = crate::tools::subagent::kimi_general_child_request_tools_fixture();
        let source_len = tools.len();
        let source_names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let expected_names = crate::core::engine::default_active_native_tool_names()
            .iter()
            .copied()
            // Children discover goal inspection via search; only the
            // owning session can create a goal or change its
            // completion state, so those never ride the child surface.
            .filter(|name| !matches!(*name, "create_goal" | "update_goal"))
            .chain([crate::core::engine::tool_catalog::TOOL_SEARCH_NAME])
            .map(str::to_string)
            .collect();
        assert_eq!(source_names, expected_names);
        assert!(!source_names.contains("create_goal"));
        assert!(!source_names.contains("update_goal"));

        // Name the offending first-party tool in test-only diagnostics while
        // production errors remain fixed and non-secret.
        for tool in &tools {
            let mut parameters = tool.input_schema.clone();
            crate::tools::schema_sanitize::sanitize_for_kimi_parameters(&mut parameters)
                .unwrap_or_else(|error| panic!("General child tool {}: {error}", tool.name));
        }

        let mut request = k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), false);
        request.tools = Some(tools);
        let body = capture_moonshot_chat_request_body(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            request,
        )
        .await;

        let captured = body["tools"].as_array().expect("captured tool catalog");
        assert_eq!(captured.len(), source_len);
        for required in &source_names {
            assert!(
                captured_function(&body, required).is_object(),
                "{required} must reach the Kimi Code wire"
            );
        }
        assert!(
            captured
                .iter()
                .all(|tool| tool["function"]["name"] != "create_goal")
        );
        assert!(
            captured
                .iter()
                .all(|tool| tool["function"]["name"] != "update_goal")
        );

        for tool in captured {
            let parameters = &tool["function"]["parameters"];
            for unsupported in ["const", "nullable", "oneOf", "allOf"] {
                assert!(
                    !value_contains_key(parameters, unsupported),
                    "captured {} still contains {unsupported}: {parameters}",
                    tool["function"]["name"]
                );
            }
            crate::tools::schema_sanitize::validate_mfjs_parameters(parameters).unwrap();
        }
    }

    #[tokio::test]
    async fn create_message_scenario() {
        // Scenario consolidation of: create_message_request_json_honors_exact_k3_route_boundaries, create_message_stream_request_json_honors_exact_k3_route_boundaries, create_message_request_json_keeps_zai_effort_route_exact, create_message_stream_request_json_keeps_zai_effort_route_exact, create_message_request_json_keeps_minimax_token_dialect_exact, create_message_stream_request_json_keeps_minimax_token_dialect_exact, create_message_request_json_keeps_modelstudio_enable_thinking_exact, create_message_stream_request_json_keeps_modelstudio_enable_thinking_exact
        // from create_message_request_json_honors_exact_k3_route_boundaries
        {
            assert_k3_request_json_route_boundaries(false).await;
        }
        // from create_message_stream_request_json_honors_exact_k3_route_boundaries
        {
            assert_k3_request_json_route_boundaries(true).await;
        }
        // from create_message_request_json_keeps_zai_effort_route_exact
        {
            assert_zai_request_truth(false).await;
        }
        // from create_message_stream_request_json_keeps_zai_effort_route_exact
        {
            assert_zai_request_truth(true).await;
        }
        // from create_message_request_json_keeps_minimax_token_dialect_exact
        {
            assert_minimax_request_truth(false).await;
        }
        // from create_message_stream_request_json_keeps_minimax_token_dialect_exact
        {
            assert_minimax_request_truth(true).await;
        }
        // from create_message_request_json_keeps_modelstudio_enable_thinking_exact
        {
            assert_modelstudio_request_truth(false).await;
        }
        // from create_message_stream_request_json_keeps_modelstudio_enable_thinking_exact
        {
            assert_modelstudio_request_truth(true).await;
        }
    }

    #[tokio::test]
    async fn kimi_code_compaction_shape_omits_sampling_parameters_on_wire() {
        for model in crate::config::KIMI_CODE_MEMBERSHIP_MODELS {
            let mut request = k3_request_fixture(model, None, /*stream*/ false);
            request.temperature = Some(0.3);
            request.top_p = Some(0.8);
            let body = capture_moonshot_chat_request_body(
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                model,
                request,
            )
            .await;

            assert_eq!(body["model"], model);
            assert!(body.get("temperature").is_none(), "{model}: {body}");
            assert!(body.get("top_p").is_none(), "{model}: {body}");
        }
    }

    /// v0.9.1 kimi-k3 dogfood report: the id the user selects has to be the id on the wire. A
    /// dogfood user selecting `kimi-k3` was served `kimi-k2.7-code`, so this
    /// asserts the wire `model` field for each K3 product on its own endpoint,
    /// and that neither one's request carries the other's id.
    #[tokio::test]
    async fn selected_moonshot_k3_model_is_the_model_on_the_wire() {
        let platform = capture_moonshot_chat_request(
            crate::config::DEFAULT_MOONSHOT_BASE_URL,
            crate::config::MOONSHOT_KIMI_K3_MODEL,
            Some("high"),
            false,
        )
        .await;
        assert_eq!(
            platform["model"],
            json!(crate::config::MOONSHOT_KIMI_K3_MODEL),
            "the direct platform route must send the id the user named: {platform}"
        );
        assert_ne!(
            platform["model"],
            json!(crate::config::DEFAULT_MOONSHOT_MODEL),
            "an explicit selection is never replaced by the provider default: {platform}"
        );
        assert_ne!(
            platform["model"],
            json!(crate::config::KIMI_CODE_K3_MODEL),
            "the coding-plan id must not leak onto the platform route: {platform}"
        );

        let membership = capture_moonshot_chat_request(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            Some("high"),
            false,
        )
        .await;
        assert_eq!(
            membership["model"],
            json!(crate::config::KIMI_CODE_K3_MODEL),
            "the Kimi Code membership route must send bare `k3`: {membership}"
        );
        assert_ne!(
            membership["model"],
            json!(crate::config::MOONSHOT_KIMI_K3_MODEL),
            "the platform id must not leak onto the coding-plan route: {membership}"
        );
    }

    #[tokio::test]
    async fn create_message_scenario_2() {
        // Scenario consolidation of: create_message_routes_only_strict_deepseek_tools_to_beta, create_message_stream_routes_only_strict_deepseek_tools_to_beta, create_message_request_replays_kimi_code_history_for_raw_off, create_message_stream_replays_kimi_code_history_for_raw_off, create_message_request_sends_mfjs_compatible_apply_patch_schema, create_message_stream_sends_mfjs_compatible_apply_patch_schema, create_message_request_drops_invalid_kimi_root_ref_tool, create_message_stream_drops_invalid_kimi_root_ref_tool
        // from create_message_routes_only_strict_deepseek_tools_to_beta
        {
            assert_deepseek_strict_request_route_boundary(false).await;
        }
        // from create_message_stream_routes_only_strict_deepseek_tools_to_beta
        {
            assert_deepseek_strict_request_route_boundary(true).await;
        }
        // from create_message_request_replays_kimi_code_history_for_raw_off
        {
            assert_kimi_code_raw_off_replays_tool_history(false).await;
        }
        // from create_message_stream_replays_kimi_code_history_for_raw_off
        {
            assert_kimi_code_raw_off_replays_tool_history(true).await;
        }
        // from create_message_request_sends_mfjs_compatible_apply_patch_schema
        {
            assert_kimi_code_apply_patch_schema_is_mfjs_compatible(false).await;
        }
        // from create_message_stream_sends_mfjs_compatible_apply_patch_schema
        {
            assert_kimi_code_apply_patch_schema_is_mfjs_compatible(true).await;
        }
        // from create_message_request_drops_invalid_kimi_root_ref_tool
        {
            assert_kimi_code_invalid_root_ref_drops_only_that_tool(false).await;
        }
        // from create_message_stream_drops_invalid_kimi_root_ref_tool
        {
            assert_kimi_code_invalid_root_ref_drops_only_that_tool(true).await;
        }
    }

    #[tokio::test]
    async fn create_message_scenario_3() {
        // Scenario consolidation of: create_message_request_drops_untyped_kimi_default_tool, create_message_stream_drops_untyped_kimi_default_tool, create_message_stream_sends_mfjs_safe_deferred_dynamic_tool, create_message_captures_exact_mfjs_safe_general_child_catalog
        // from create_message_request_drops_untyped_kimi_default_tool
        {
            assert_kimi_code_untyped_default_drops_only_that_tool(false).await;
        }
        // from create_message_stream_drops_untyped_kimi_default_tool
        {
            assert_kimi_code_untyped_default_drops_only_that_tool(true).await;
        }
        // from create_message_stream_sends_mfjs_safe_deferred_dynamic_tool
        {
            assert_kimi_code_streams_mfjs_safe_deferred_dynamic_tool().await;
        }
        // from create_message_captures_exact_mfjs_safe_general_child_catalog
        {
            assert_kimi_code_captures_exact_general_child_catalog().await;
        }
    }

    #[tokio::test]
    async fn moonshot_drops_only_incompatible_tool() {
        for streaming in [false, true] {
            let mut request =
                k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), streaming);
            let good = test_tool("compatible_lookup");
            let mut bad = test_tool("mcp_pattern_tool");
            bad.input_schema = json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "pattern": "^private-regex-7701$"}
                }
            });
            request.tools = Some(vec![good, bad]);
            request.tool_choice = Some(json!("auto"));

            let body = capture_moonshot_chat_request_body(
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                crate::config::KIMI_CODE_K3_MODEL,
                request,
            )
            .await;
            let tools = body["tools"]
                .as_array()
                .expect("compatible tool must stay on the wire");
            assert_eq!(
                tools.len(),
                1,
                "only the incompatible tool may be dropped (streaming={streaming}): {body}"
            );
            assert_eq!(tools[0]["function"]["name"], "compatible_lookup");
            assert_eq!(
                body["tool_choice"],
                json!("auto"),
                "tool_choice survives while any tool remains: {body}"
            );
            assert!(
                !body.to_string().contains("private-regex-7701"),
                "the dropped tool's private schema values must not reach the wire: {body}"
            );
        }
    }

    #[tokio::test]
    async fn moonshot_rejects_named_choice_for_omitted_tool() {
        for streaming in [false, true] {
            let server = MockServer::start().await;
            let client = moonshot_request_boundary_client(
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                crate::config::KIMI_CODE_K3_MODEL,
                server.uri(),
            );
            let mut request =
                k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), streaming);
            let good = test_tool("compatible_lookup");
            let mut bad = test_tool("mcp_pattern_tool");
            bad.input_schema = json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "pattern": "^private-regex-8801$"}
                }
            });
            request.tools = Some(vec![good, bad]);
            request.tool_choice = Some(json!({
                "type": "tool",
                "name": "mcp_pattern_tool"
            }));

            let error = if streaming {
                match client.create_message_stream(request).await {
                    Ok(_) => {
                        panic!("a named choice for an omitted tool must fail before transport")
                    }
                    Err(error) => error,
                }
            } else {
                match client.create_message(request).await {
                    Ok(_) => {
                        panic!("a named choice for an omitted tool must fail before transport")
                    }
                    Err(error) => error,
                }
            };
            let diagnostic = error.to_string();
            assert!(
                diagnostic.contains("cannot force tool 'mcp_pattern_tool'"),
                "streaming={streaming}: {diagnostic}"
            );
            assert!(
                !diagnostic.contains("private-regex-8801"),
                "schema values must not leak into the user-visible diagnostic: {diagnostic}"
            );
            assert!(
                server
                    .received_requests()
                    .await
                    .expect("request log")
                    .is_empty(),
                "dangling named tool_choice must fail locally (streaming={streaming})"
            );
        }
    }

    #[tokio::test]
    async fn moonshot_stream_emits_one_projection_warning() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("data: [DONE]\n\n"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = moonshot_request_boundary_client(
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            server.uri(),
        );
        let mut request = k3_request_fixture(crate::config::KIMI_CODE_K3_MODEL, Some("low"), true);
        let good = test_tool("compatible_lookup");
        let mut bad = test_tool("mcp_pattern_tool");
        bad.input_schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "pattern": "^private-regex-9901$"}
            }
        });
        request.tools = Some(vec![good, bad]);
        request.tool_choice = Some(json!("auto"));

        let mut stream = client
            .create_message_stream(request)
            .await
            .expect("compatible tools keep the request sendable");
        let first = stream
            .next()
            .await
            .expect("projection warning precedes provider SSE")
            .expect("projection warning is not a stream error");
        let (provider, omitted_tool_names, omitted_tool_count) = match first {
            codewhale_models::StreamEvent::ToolProjectionWarning {
                provider,
                omitted_tool_names,
                omitted_tool_count,
            } => (provider, omitted_tool_names, omitted_tool_count),
            other => panic!("first event must be the projection warning, got {other:?}"),
        };
        assert!(provider.contains("Moonshot"), "{provider}");
        assert_eq!(omitted_tool_names, vec!["mcp_pattern_tool"]);
        assert_eq!(omitted_tool_count, 1);

        let mut additional_warnings = 0;
        while let Some(event) = stream.next().await {
            if matches!(
                event.expect("captured SSE response remains valid"),
                codewhale_models::StreamEvent::ToolProjectionWarning { .. }
            ) {
                additional_warnings += 1;
            }
        }
        assert_eq!(
            additional_warnings, 0,
            "warning must be emitted once per request"
        );

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).expect("request JSON");
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
        assert!(
            !body.to_string().contains("private-regex-9901"),
            "omitted schema values must not reach the wire: {body}"
        );
    }

    /// #6528 — a rejected key names the route, host, key source and the
    /// command that replaces it; an unknown model names the route and host.
    #[test]
    fn auth_and_unknown_model_errors_name_route_host_and_key_source() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = Config {
            provider: Some("openrouter".to_string()),
            providers: Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    api_key: Some("or-rejected-key-1234567890".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        let client = CodewhaleClient::new(&config).expect("openrouter client");
        let auth = client
            .http_error_with_route_context(401, "Invalid API key", None, None)
            .to_string();
        assert!(auth.contains("provider: openrouter"), "{auth}");
        assert!(auth.contains("openrouter.ai"), "{auth}");
        assert!(auth.contains("key source: config file"), "{auth}");
        assert!(
            auth.contains("fix: codewhale auth set --provider openrouter"),
            "{auth}"
        );
        assert!(!auth.contains("or-rejected-key-1234567890"), "{auth}");

        let model = client
            .http_error_with_route_context(404, "model not found: nope", None, None)
            .to_string();
        assert!(model.contains("provider route: openrouter"), "{model}");
        assert!(model.contains("host: openrouter.ai"), "{model}");
    }

    /// A key route gets no subscription guidance; a sign-in route's plan
    /// limit names the account label and the switch command, never a token.
    #[test]
    fn subscription_quota_errors_name_account_and_switch_command() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = Config {
            provider: Some("openrouter".to_string()),
            providers: Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    api_key: Some("or-quota-key-1234567890".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        let mut client = CodewhaleClient::new(&config).expect("openrouter client");
        assert_eq!(client.subscription_limit_guidance, None);
        let body =
            r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#;
        let plain = client.http_error_with_route_context(429, body, None, None);
        assert!(matches!(plain, LlmError::QuotaExhausted(_)), "{plain:?}");
        assert!(!plain.to_string().contains("codewhale auth"), "{plain}");

        client.subscription_limit_guidance = Some(crate::oauth::usage_limit_guidance(
            crate::oauth::OAuthProvider::Chatgpt,
            Some("a@example.com (plus)"),
        ));
        let guided = client
            .http_error_with_route_context(429, body, None, None)
            .to_string();
        assert!(
            guided.contains("The usage limit has been reached"),
            "{guided}"
        );
        assert!(
            guided.contains("ChatGPT account a@example.com (plus)"),
            "{guided}"
        );
        assert!(
            guided.contains("CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt"),
            "{guided}"
        );
        assert!(!guided.contains("or-quota-key-1234567890"), "{guided}");
        // Ordinary rate limits stay retryable and unannotated.
        let rate = client.http_error_with_route_context(429, "Too Many Requests", None, None);
        assert!(rate.is_retryable());
        assert!(!rate.to_string().contains("codewhale auth"), "{rate}");
    }

    /// `auth_mode = "oauth"` alone does not mean a sign-in made the request:
    /// with no usable OAuth credential the resolver falls through to the API
    /// key, and a credit error must not blame (or name) a sign-in.
    #[test]
    fn xai_oauth_mode_that_fell_back_to_an_api_key_gets_no_sign_in_guidance() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _key = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
        let _base = crate::test_support::EnvVarGuard::remove("XAI_BASE_URL");
        let config = Config {
            provider: Some("xai".to_string()),
            providers: Some(ProvidersConfig {
                xai: ProviderConfig {
                    api_key: Some("xai-fallback-key-1234567890".to_string()),
                    auth_mode: Some("oauth".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        let client = CodewhaleClient::new(&config).expect("xai client");
        assert_ne!(client.api_key_source, crate::config::XAI_OAUTH_KEY_SOURCE);
        assert_eq!(client.subscription_limit_guidance, None);
        let body =
            r#"{"error":{"code":"credit_balance_exhausted","message":"credit balance exhausted"}}"#;
        let error = client
            .http_error_with_route_context(402, body, None, None)
            .to_string();
        assert!(!error.contains("codewhale auth"), "{error}");
    }

    fn test_id_token(email: &str) -> String {
        use base64::Engine as _;
        format!(
            "header.{}.sig",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(json!({ "email": email }).to_string())
        )
    }

    /// The official ChatGPT route names the selected owned grant even when a
    /// legacy external login is present and consented for import.
    #[test]
    fn chatgpt_quota_guidance_names_owned_grant_and_ignores_legacy_import() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical temp root");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let _access = crate::test_support::EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
        let _legacy_access = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
        let path = root.join("legacy-auth.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"tokens": {
                "access_token": crate::test_support::future_test_jwt("legacy-import"),
                "id_token": test_id_token("legacy@example.com"),
            }}))
            .unwrap(),
        )
        .unwrap();
        let _auth_path = crate::test_support::EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &path);
        let mut config = Config {
            provider: Some(ProviderKind::OpenaiCodex.as_str().to_string()),
            providers: Some(ProvidersConfig {
                openai_codex: ProviderConfig {
                    external_credentials: Some(
                        codewhale_config::ExternalCredentialConsentToml::read_only(
                            codewhale_config::ProviderKind::OpenaiCodex,
                            codewhale_config::ExternalCredentialSource::CodexCli,
                            path,
                        ),
                    ),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        assert!(
            CodewhaleClient::new(&config).is_err(),
            "an import cannot authorize the official route"
        );
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        // Stored verified-grant fixture; OAuth signature validation has its
        // own signed-token boundary tests.
        crate::oauth::activate_login(
            crate::oauth::pending_login_with_id_token_for_test(
                crate::oauth::OAuthProvider::Chatgpt,
                "own-verified-access",
                "own-verified-refresh",
                Some(&test_id_token("owned@example.com")),
            ),
            Some(&config_path),
            Some(&mut config),
        )
        .unwrap();
        let client = CodewhaleClient::new(&config).expect("official owned ChatGPT client");
        assert_eq!(client.api_key, "own-verified-access");
        let guidance = client.subscription_limit_guidance.as_deref().unwrap();
        assert!(
            guidance.contains("ChatGPT account owned@example.com"),
            "{guidance}"
        );
        assert!(!guidance.contains("legacy@example.com"), "{guidance}");
        let error = client.http_error_with_route_context(
            429,
            r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}"#,
            None,
            None,
        );
        assert!(matches!(error, LlmError::QuotaExhausted(_)));
        assert!(!error.is_retryable());
        assert!(error.to_string().contains("owned@example.com"));
        assert!(!error.to_string().contains("own-verified-access"));
    }

    /// An xAI OAuth route names the signed-in account from the credential
    /// the client sends; the resolver, the guidance and the picker's
    /// credential source share that one read.
    #[test]
    fn xai_oauth_quota_guidance_names_the_signed_in_account() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical temp root");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let _key = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
        let _base = crate::test_support::EnvVarGuard::remove("XAI_BASE_URL");
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "").expect("empty config");
        let mut config = Config {
            provider: Some("xai".to_string()),
            ..Config::default()
        };
        crate::oauth::activate_login(
            crate::oauth::pending_login_with_id_token_for_test(
                crate::oauth::OAuthProvider::Xai,
                "xai-oauth-access",
                "xai-oauth-refresh",
                Some(&test_id_token("grok@example.com")),
            ),
            Some(&config_path),
            Some(&mut config),
        )
        .expect("xAI login");
        let client = CodewhaleClient::new(&config).expect("xai client");
        assert_eq!(client.api_key, "xai-oauth-access");
        assert_eq!(client.api_key_source, crate::config::XAI_OAUTH_KEY_SOURCE);
        let guidance = client
            .subscription_limit_guidance
            .as_deref()
            .expect("sign-in guidance");
        assert!(
            guidance.contains("xAI account grok@example.com"),
            "{guidance}"
        );
        assert!(!guidance.contains("xai-oauth-"), "{guidance}");
        assert_eq!(
            crate::config::resolve_credential_source(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Xai)
            )
            .source,
            crate::credentials::CredentialSource::OAuth {
                flow: "xAI".to_string(),
                account: Some("grok@example.com".to_string()),
            }
        );
    }

    /// #6715 review: a consented Grok CLI import is a sign-in the user can
    /// switch away from, and its quota error must name the account the Grok
    /// file holds (the credential this client sends), not a Codewhale-owned
    /// account that sent nothing.
    #[test]
    fn xai_quota_guidance_names_the_consented_grok_import_that_sent_the_request() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical temp root");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let _key = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
        let _base = crate::test_support::EnvVarGuard::remove("XAI_BASE_URL");
        let path = root.join("grok-auth.json");
        let token = crate::test_support::future_test_jwt("grok-cli");
        let scope = format!(
            "{}::{}",
            crate::oauth::XAI_OIDC_ISSUER,
            crate::oauth::GROK_OIDC_CLIENT_ID
        );
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                scope: {
                    "key": token.clone(),
                    "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                    "id_token": test_id_token("grok-cli@example.com"),
                    "oidc_issuer": crate::oauth::XAI_OIDC_ISSUER,
                    "oidc_client_id": crate::oauth::GROK_OIDC_CLIENT_ID,
                    "auth_mode": "oidc",
                }
            }))
            .expect("serialize fixture"),
        )
        .expect("write fixture");
        let _auth_path = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &path);
        let config = Config {
            provider: Some(ProviderKind::Xai.as_str().to_string()),
            providers: Some(ProvidersConfig {
                xai: ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    external_credentials: Some(
                        codewhale_config::ExternalCredentialConsentToml::read_only(
                            codewhale_config::ProviderKind::Xai,
                            codewhale_config::ExternalCredentialSource::GrokCli,
                            path.clone(),
                        ),
                    ),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        let client = CodewhaleClient::new(&config).expect("xai client");
        assert_eq!(client.api_key, token);
        assert_eq!(client.api_key_source, crate::config::XAI_OAUTH_KEY_SOURCE);
        let guidance = client
            .subscription_limit_guidance
            .as_deref()
            .expect("sign-in guidance");
        assert!(
            guidance.contains("xAI account grok-cli@example.com"),
            "{guidance}"
        );
        assert!(!guidance.contains(&token), "{guidance}");
    }

    /// Ambient legacy process tokens cannot authorize the official route.
    #[test]
    fn chatgpt_process_token_cannot_replace_owned_grant_or_account_guidance() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().canonicalize().expect("canonical temp root");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let token = crate::test_support::future_test_jwt("process-token");
        let _access = crate::test_support::EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", &token);
        let _legacy_access = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
        let mut config = Config {
            provider: Some(ProviderKind::OpenaiCodex.as_str().to_string()),
            ..Config::default()
        };
        assert!(CodewhaleClient::new(&config).is_err());
        let own = crate::oauth::install_test_chatgpt_registration(&mut config).unwrap();
        let client = CodewhaleClient::new(&config).expect("official ChatGPT client");
        assert_eq!(client.api_key, own);
        assert_ne!(client.api_key, token);
        assert!(client.subscription_limit_guidance.is_some());
    }

    fn concentrate_client(server: &MockServer, model: &str) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = Config {
            provider: Some("concentrate".to_string()),
            providers: Some(ProvidersConfig {
                concentrate: ProviderConfig {
                    api_key: Some("concentrate-test-key".to_string()),
                    base_url: Some(format!("{}/v1", server.uri())),
                    model: Some(model.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        CodewhaleClient::new(&config).expect("Concentrate client should resolve its route")
    }

    /// The documented Concentrate stream: `event:`-typed `response.*` frames
    /// with sequence numbers and NO `data: [DONE]` sentinel.
    /// https://concentrate.ai/docs/api-reference/endpoint/streaming
    fn concentrate_sse_fixture(model: &str) -> String {
        let completed = json!({
            "id": "resp_1",
            "object": "response",
            "status": "completed",
            "model": model,
            "output": [{
                "type": "message", "id": "msg_1", "status": "completed", "role": "assistant",
                "content": [{"type": "output_text", "text": "ok from stub", "annotations": []}]
            }],
            "usage": {"input_tokens": 12, "output_tokens": 5, "total_tokens": 17,
                      "input_tokens_details": {"cached_tokens": 0}}
        });
        let frame = |event: &str, payload: Value| format!("event: {event}\ndata: {}\n\n", payload);
        [
                frame("response.created", json!({"type": "response.created", "sequence_number": 0, "response": {"id": "resp_1", "status": "in_progress"}})),
                frame("response.output_item.added", json!({"type": "response.output_item.added", "sequence_number": 1, "output_index": 0, "item": {"type": "message", "id": "msg_1", "status": "in_progress", "role": "assistant", "content": []}})),
                frame("response.content_part.added", json!({"type": "response.content_part.added", "sequence_number": 2, "item_id": "msg_1", "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": ""}})),
                frame("response.output_text.delta", json!({"type": "response.output_text.delta", "sequence_number": 3, "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "ok "})),
                frame("response.output_text.delta", json!({"type": "response.output_text.delta", "sequence_number": 4, "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "from stub"})),
                frame("response.output_text.done", json!({"type": "response.output_text.done", "sequence_number": 5, "item_id": "msg_1", "output_index": 0, "content_index": 0, "text": "ok from stub"})),
                frame("response.output_item.done", json!({"type": "response.output_item.done", "sequence_number": 6, "output_index": 0, "item": completed["output"][0].clone()})),
                frame("response.completed", json!({"type": "response.completed", "sequence_number": 7, "response": completed})),
            ]
            .concat()
    }

    /// Request URL, bearer header, verbatim model, documented-fields body, and
    /// the typed SSE stream (no `[DONE]`) — the whole Concentrate contract on
    /// a loopback wiremock, never the live gateway.
    #[tokio::test]
    async fn concentrate_responses_request_matches_the_documented_contract() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer concentrate-test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_string(concentrate_sse_fixture("openai/gpt-5.6-sol")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = concentrate_client(&server, "openai/gpt-5.6-sol");
        assert_eq!(client.wire_format, WireFormat::Responses);
        assert_eq!(client.api_provider, ProviderKind::Concentrate);
        assert_eq!(
            responses_api_url(DEFAULT_CONCENTRATE_BASE_URL, ProviderKind::Concentrate),
            "https://api.concentrate.ai/v1/responses",
            "the official base URL maps to the documented Responses endpoint"
        );

        let mut stream = client
            .create_message_stream(minimal_zen_request("openai/gpt-5.6-sol"))
            .await
            .expect("Concentrate Responses request should start");
        let mut text = String::new();
        let mut usage = None;
        let mut stopped = false;
        while let Some(event) = stream.next().await {
            match event.expect("Concentrate stream event") {
                StreamEvent::ContentBlockDelta {
                    delta: Delta::TextDelta { text: piece },
                    ..
                } => text.push_str(&piece),
                StreamEvent::MessageDelta { usage: Some(u), .. } => usage = Some(u),
                StreamEvent::MessageStop => stopped = true,
                _ => {}
            }
        }
        assert_eq!(text, "ok from stub", "typed deltas assemble the reply");
        let usage = usage.expect("response.completed carries usage");
        assert_eq!((usage.input_tokens, usage.output_tokens), (12, 5));
        assert!(
            stopped,
            "the stream ends on response.completed without a [DONE] sentinel"
        );

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.url.path(), "/v1/responses");
        for forbidden in [
            "openai-beta",
            "originator",
            "chatgpt-account-id",
            "x-api-key",
        ] {
            assert!(
                request.headers.get(forbidden).is_none(),
                "Concentrate request must not include {forbidden}"
            );
        }
        let body: Value = serde_json::from_slice(&request.body).expect("Responses JSON body");
        assert_eq!(
            body["model"], "openai/gpt-5.6-sol",
            "model id verbatim: {body}"
        );
        assert_eq!(body["stream"], true);
        assert!(
            body.get("messages").is_none(),
            "Responses body, not Chat: {body}"
        );
        for undocumented in [
            "store",
            "include",
            "instructions",
            "metadata",
            "previous_response_id",
        ] {
            assert!(
                body.get(undocumented).is_none(),
                "undocumented field {undocumented} on the wire: {body}"
            );
        }
        assert_eq!(
            body["input"][0]["role"], "system",
            "system prompt rides as a system input item: {body}"
        );
    }

    /// The documented error body surfaces verbatim and classifies by message:
    /// 401 → authentication, 402 → quota (insufficient credits).
    /// https://concentrate.ai/docs/api-reference/endpoint/errors
    #[tokio::test]
    async fn concentrate_error_bodies_surface_verbatim_and_classify() {
        for (status, body, expected, needle) in [
            (
                401,
                r#"{"error":"Unauthorized","message":"Invalid API key"}"#,
                crate::error_taxonomy::ErrorCategory::Authentication,
                "Invalid API key",
            ),
            (
                402,
                r#"{"error":"Insufficient funds","message":"Your account has insufficient credits. Please add credits to continue."}"#,
                crate::error_taxonomy::ErrorCategory::RateLimit,
                "insufficient credits",
            ),
            (
                400,
                r#"{"error":"Bad Request","message":"Invalid model name: 'invalid-model-xyz'"}"#,
                crate::error_taxonomy::ErrorCategory::InvalidInput,
                "Invalid model name",
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/responses"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .mount(&server)
                .await;
            let client = concentrate_client(&server, "gpt-5.6-sol");
            let error = match client
                .create_message_stream(minimal_zen_request("gpt-5.6-sol"))
                .await
            {
                Ok(mut stream) => {
                    let mut failure = None;
                    while let Some(event) = stream.next().await {
                        if let Err(err) = event {
                            failure = Some(err);
                            break;
                        }
                    }
                    failure.expect("HTTP {status} must fail the stream")
                }
                Err(err) => err,
            };
            let message = format!("{error:#}");
            assert!(
                message.contains(needle),
                "HTTP {status}: message must carry the documented text, got: {message}"
            );
            assert_eq!(
                crate::error_taxonomy::classify_error_message(&message),
                expected,
                "HTTP {status}: {message}"
            );
        }
    }

    /// Concentrate's `GET /v1/models` is unauthenticated, so a 2xx must not
    /// count as key verification. Guided setup treats the probe as unobserved;
    /// health_check must not issue the request either.
    #[tokio::test]
    async fn concentrate_health_check_does_not_treat_unauthenticated_models_as_key_proof() {
        let server = MockServer::start().await;
        let client = concentrate_client(&server, DEFAULT_CONCENTRATE_MODEL);

        assert!(client.health_check().await.expect("health check"));
        assert!(!provider_api_key_verification_is_observed(
            ProviderKind::Concentrate
        ));
        let requests = server.received_requests().await.expect("recorded requests");
        assert!(
            requests.is_empty(),
            "Concentrate must not treat unauthenticated GET /v1/models as key verification"
        );
    }

    /// `GET /v1/models` needs no key and answers the OpenAI list shape; rows
    /// are provider-scoped, the default is marked, and unknowns stay unclaimed.
    /// https://concentrate.ai/docs/api-reference/endpoint/list-models
    #[tokio::test]
    async fn concentrate_live_catalog_is_provider_scoped_and_marks_the_default() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [
                    {"id": "claude-fable-5", "object": "model", "owned_by": "anthropic"},
                    {"id": DEFAULT_CONCENTRATE_MODEL, "object": "model", "owned_by": "deepseek"},
                    {"id": "gpt-5.6-sol", "object": "model", "owned_by": "openai"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let delta = concentrate_client(&server, DEFAULT_CONCENTRATE_MODEL)
            .fetch_catalog_delta()
            .await
            .expect("Concentrate catalog delta");
        assert_eq!(delta.provider, "concentrate");
        assert_eq!(delta.offerings.len(), 3);
        let default = delta
            .offerings
            .iter()
            .find(|offering| offering.wire_model_id == DEFAULT_CONCENTRATE_MODEL)
            .expect("default row");
        assert!(default.default_for_provider);
        let unknown = delta
            .offerings
            .iter()
            .find(|offering| offering.wire_model_id == "claude-fable-5")
            .expect("unclaimed row");
        assert!(!unknown.default_for_provider);
        assert_eq!(unknown.canonical_model, None);
        assert_eq!(
            unknown.cost, None,
            "no pricing claim from a gateway catalog"
        );
        assert!(matches!(unknown.source, CatalogSource::Live { .. }));
    }

    /// A Codewhale-route client pointed at a loopback stub.
    ///
    /// `base_url` goes through the provider table rather than
    /// `CODEWHALE_API_BASE` so the test does not mutate process env, but it
    /// exercises the same "declared origin" path: the key must follow the
    /// route to whatever origin the operator pointed it at.
    fn codewhale_client(server: &MockServer, model: &str) -> CodewhaleClient {
        let config = Config {
            provider: Some("codewhale".to_string()),
            providers: Some(ProvidersConfig {
                codewhale: ProviderConfig {
                    api_key: Some("cwc_key_test_value".to_string()),
                    base_url: Some(server.uri()),
                    model: Some(model.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        };
        CodewhaleClient::new(&config).expect("Codewhale client should resolve its model route")
    }

    /// The account key must ride as `Authorization: Bearer` and never as
    /// `x-api-key`, on both protocols the account API serves.
    fn assert_codewhale_bearer(request: &wiremock::Request) {
        assert_eq!(
            request
                .headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer cwc_key_test_value")
        );
        assert!(
            request.headers.get("x-api-key").is_none(),
            "the Codewhale API does not accept x-api-key"
        );
    }

    #[tokio::test]
    async fn codewhale_chat_request_carries_the_account_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl_cw",
                "object": "chat.completion",
                "model": "deepseek/deepseek-v4-pro",
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

        let client = codewhale_client(&server, "deepseek/deepseek-v4-pro");
        assert_eq!(client.wire_format, WireFormat::ChatCompletions);
        client
            .create_message(minimal_zen_request("deepseek/deepseek-v4-pro"))
            .await
            .expect("Codewhale chat request should succeed");

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        assert_codewhale_bearer(&requests[0]);
        let body: Value = serde_json::from_slice(&requests[0].body).expect("chat JSON body");
        // Model ids reach the account API exactly as its catalog returns them.
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("deepseek/deepseek-v4-pro")
        );
    }

    #[tokio::test]
    async fn codewhale_messages_request_carries_the_account_bearer_not_x_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_cw",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "ok"}],
                "model": "anthropic/claude-sonnet-5",
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = codewhale_client(&server, "anthropic/claude-sonnet-5");
        assert_eq!(client.wire_format, WireFormat::AnthropicMessages);
        client
            .create_message(minimal_zen_request("anthropic/claude-sonnet-5"))
            .await
            .expect("Codewhale messages request should succeed");

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        assert_codewhale_bearer(&requests[0]);
        assert_eq!(
            requests[0]
                .headers
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok()),
            Some("2023-06-01")
        );
    }
