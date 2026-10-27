// === OrcaRouter: live /models discovery + capability filtering ==========
//
// OrcaRouter serves the extended OpenRouter-shaped catalog and adds
// `supported_endpoint_types` to every row. These tests use synthetic ids
// only; they never touch the network.

pub(super) fn orcarouter_client_for(server: &MockServer) -> CodewhaleClient {
    let _ = rustls::crypto::ring::default_provider().install_default();
    CodewhaleClient::new(&{
        let __cw_config_init_0_0 = Some("orcarouter".to_string());
        let __cw_config_init_0_1 = Some(ProvidersConfig {
            orcarouter: ProviderConfig {
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
    .expect("orcarouter client")
}

/// Every OrcaRouter row carries `supported_endpoint_types`; only rows that
/// advertise a chat dialect reach the roster, and the image-input fact is
/// read from `architecture.input_modalities` verbatim.
#[tokio::test]
async fn orcarouter_catalog_keeps_chat_rows_and_their_image_input_fact() {
    let server = MockServer::start().await;
    mount_models_json(
            &server,
            200,
            json!({"data": [
                {
                    "id": "synthetic/chat-text",
                    "supported_endpoint_types": ["openai", "openai-response"],
                    "context_length": 1048576,
                    "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
                    "top_provider": {"context_length": 1048576, "max_completion_tokens": 384000},
                    "pricing": {"prompt": "0.00000022", "completion": "0.00000066"}
                },
                {
                    "id": "synthetic/chat-vision",
                    "supported_endpoint_types": ["openai", "anthropic"],
                    "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}
                },
                {
                    "id": "synthetic/router-auto",
                    "supported_endpoint_types": ["openai", "openai-response", "anthropic", "gemini"],
                    "context_length": 1000000
                },
                {
                    "id": "synthetic/image-gen",
                    "supported_endpoint_types": ["image-generation"]
                },
                {
                    "id": "synthetic/video-gen",
                    "supported_endpoint_types": ["openai-video"]
                },
                {
                    "id": "synthetic/reranker",
                    "supported_endpoint_types": ["jina-rerank"]
                },
                {
                    "id": "synthetic/embedder",
                    "supported_endpoint_types": ["embeddings"]
                },
                {
                    "id": "synthetic/no-dialect"
                }
            ]}),
        )
        .await;

    let delta = orcarouter_client_for(&server)
        .fetch_catalog_delta()
        .await
        .expect("delta");
    assert_eq!(delta.provider, "orcarouter");

    let ids: Vec<&str> = delta
        .offerings
        .iter()
        .map(|offering| offering.wire_model_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "synthetic/chat-text",
            "synthetic/chat-vision",
            "synthetic/router-auto"
        ],
        "chat-only roster; non-chat dialects and dialect-less rows are excluded: {ids:?}"
    );

    let vision = delta
        .offerings
        .iter()
        .find(|offering| offering.wire_model_id == "synthetic/chat-vision")
        .expect("vision row");
    assert_eq!(
        vision.modalities.as_ref().expect("stated modalities").input,
        vec!["text", "image"],
        "a stated input-modality fact is preserved verbatim"
    );

    // OrcaRouter does not publish `supported_parameters`; reasoning and
    // tool support stay unclaimed rather than becoming a factual refusal.
    for offering in &delta.offerings {
        assert!(offering.reasoning.is_none(), "{offering:?}");
        assert!(offering.tool_call.is_none(), "{offering:?}");
    }

    // A row that names no architecture keeps modalities unclaimed instead
    // of inheriting a text-only default.
    let auto = delta
        .offerings
        .iter()
        .find(|offering| offering.wire_model_id == "synthetic/router-auto")
        .expect("router row");
    assert_eq!(auto.modalities, None);
}

/// The chat filter is a hard gate: a roster made only of non-chat dialects
/// fails closed rather than producing an empty-but-successful catalog.
#[tokio::test]
async fn orcarouter_catalog_fails_closed_when_no_row_is_chat() {
    let server = MockServer::start().await;
    mount_models_json(
        &server,
        200,
        json!({"data": [
            {"id": "synthetic/image-gen", "supported_endpoint_types": ["image-generation"]},
            {"id": "synthetic/video-gen", "supported_endpoint_types": ["openai-video"]}
        ]}),
    )
    .await;
    assert_eq!(
        orcarouter_client_for(&server)
            .fetch_catalog_delta()
            .await
            .expect_err("no chat rows"),
        CatalogRefreshError::InvalidResponse
    );
}

/// One malformed row costs only its own row, and an unauthorized refresh
/// maps to the typed auth failure with the body unread.
#[tokio::test]
async fn orcarouter_catalog_skips_malformed_rows_and_types_auth_failure() {
    let server = MockServer::start().await;
    mount_models_json(
        &server,
        200,
        json!({"data": [
            {"id": "synthetic/good", "supported_endpoint_types": ["openai"]},
            {"id": "synthetic/bad id", "supported_endpoint_types": ["openai"]},
            {"id": "~synthetic/alias", "supported_endpoint_types": ["openai"]}
        ]}),
    )
    .await;
    let delta = orcarouter_client_for(&server)
        .fetch_catalog_delta()
        .await
        .expect("delta");
    assert_eq!(
        delta
            .offerings
            .iter()
            .map(|offering| offering.wire_model_id.as_str())
            .collect::<Vec<_>>(),
        vec!["synthetic/good"]
    );

    server.reset().await;
    mount_models_json(&server, 401, json!({"error": "denied"})).await;
    assert_eq!(
        orcarouter_client_for(&server)
            .fetch_catalog_delta()
            .await
            .expect_err("unauthorized"),
        CatalogRefreshError::Unauthorized
    );
}

// === OrcaRouter: live end to end through the implemented provider ========
//
// Opt-in: needs the environment's ORCAROUTER_API_KEY and real egress. Both
// halves go through the client this PR wired — the `/v1/models` refresh and
// a `/v1/chat/completions` request — never a side channel.

fn live_orcarouter_client() -> Option<CodewhaleClient> {
    let key = std::env::var("ORCAROUTER_API_KEY").ok()?;
    if key.trim().is_empty() || std::env::var_os("CODEWHALE_SKIP_LIVE").is_some() {
        return None;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    CodewhaleClient::new(&{
        let __cw_config_init_1_0 = Some("orcarouter".to_string());
        let __cw_config_init_1_1 = Some(ProvidersConfig {
            orcarouter: ProviderConfig {
                api_key: Some(key),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_1_value: Config = Config::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.providers = __cw_config_init_1_1;
        __cw_config_init_1_value
    })
    .ok()
}

#[tokio::test]
#[ignore = "opt-in live: calls api.orcarouter.ai with ORCAROUTER_API_KEY"]
async fn orcarouter_live_catalog_and_chat_through_the_provider_path() {
    let Some(client) = live_orcarouter_client() else {
        return;
    };
    let delta = client
        .fetch_catalog_delta()
        .await
        .expect("live OrcaRouter /v1/models");
    assert!(
        !delta.offerings.is_empty(),
        "the live chat roster must not be empty"
    );
    for offering in &delta.offerings {
        assert_eq!(offering.provider, "orcarouter");
        assert!(
            offering.wire_model_id.contains('/'),
            "vendor/model namespace is kept verbatim: {}",
            offering.wire_model_id
        );
    }

    // A real inference call through the same client, on a model the live
    // roster just advertised — not a hand-written example id. OrcaRouter
    // API keys are per-key model-scoped, so the test walks the advertised
    // roster until one model answers; a key that can call none fails with
    // the provider's own error.
    let mut last_error = None;
    let mut answered = false;
    for offering in &delta.offerings {
        let model = offering.wire_model_id.clone();
        assert!(
            !model.is_empty(),
            "the live chat roster must advertise model ids"
        );
        let request = translation_message_request(
            "Reply with the single word: ready",
            model.clone(),
            "English",
            16,
        );
        match client.create_message_without_response_cache(request).await {
            Ok(response) => {
                assert!(
                    !response.content.is_empty(),
                    "a live completion for {model} must carry content"
                );
                answered = true;
                break;
            }
            // This key is not scoped for this model; the roster is wider
            // than the key. Try the next advertised model.
            Err(error) => last_error = Some((model, error.to_string())),
        }
    }
    if let Some((model, error)) = last_error {
        assert!(
            answered,
            "no advertised model answered for this key; last {model}: {error}"
        );
    }
}
