//! Local HTTP regressions for complete, bounded provider catalog observations.

use super::tests::{
    custom_mock_client_for_identity, mount_models_json, opencode_go_client_for,
    openrouter_client_for,
};
use super::*;
use codewhale_runtime::config::{ProviderConfig, ProvidersConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "catalog-key-canary-7f092";
const CURSOR: &str = "cursor/second +?&=雪-canary";

#[tokio::test]
async fn chatgpt_models_http_uses_visible_roster_and_rejects_secret_labels() {
    let server = MockServer::start().await;
    let client = CodewhaleClient::new(&{
        let __cw_config_init_0_0 = Some("openai-codex".into());
        let __cw_config_init_0_1 = Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                api_key: Some(KEY.into()),
                base_url: Some(format!("{}/v1", server.uri())),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    })
    .expect("explicit local ChatGPT protocol fixture");
    mount_models_json(
        &server,
        200,
        json!({"models":[
            {"slug":"gpt-z","display_name":"GPT Z","visibility":"list"},
            {"slug":"gpt-hidden","display_name":"Hidden","visibility":"hidden"},
            {"slug":"gpt-a","display_name":"GPT A","visibility":"list"}
        ]}),
    )
    .await;
    let models = client.list_models().await.expect("account roster");
    assert_eq!(
        models.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["gpt-z", "gpt-a"]
    );
    assert_eq!(models[0].display_name.as_deref(), Some("GPT Z"));
    let delta = client
        .fetch_catalog_delta()
        .await
        .expect("same parsed roster");
    assert_eq!(
        delta
            .offerings
            .iter()
            .map(|row| row.wire_model_id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-z", "gpt-a"]
    );
    for request in server.received_requests().await.expect("requests") {
        assert_eq!(request.url.path(), "/v1/models");
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            &format!("Bearer {KEY}")
        );
    }
    server.reset().await;
    mount_models_json(
        &server,
        200,
        json!({"models":[
            {"slug":"gpt-a","display_name":KEY,"visibility":"list"}
        ]}),
    )
    .await;
    let error = client
        .list_models()
        .await
        .expect_err("credential echo is never cached");
    assert!(!error.to_string().contains(KEY));
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", "/other"))
        .mount(&server)
        .await;
    let response = client
        .http_client
        .post(format!("{}/v1/responses", server.uri()))
        .send()
        .await
        .expect("local response");
    assert_eq!(response.status().as_u16(), 307);
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request count")
            .len(),
        1,
        "a plan grant must not follow a redirected inference endpoint"
    );
}

fn anthropic_client(base_url: &str) -> CodewhaleClient {
    let mut client = CodewhaleClient::new(&{
        let __cw_config_init_1_0 = Some("anthropic".into());
        let __cw_config_init_1_1 = Some(ProvidersConfig {
            anthropic: ProviderConfig {
                api_key: Some(KEY.into()),
                base_url: Some(base_url.into()),
                http_headers: Some(HashMap::from([(
                    "x-private-fixture".into(),
                    "custom-header-canary".into(),
                )])),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_1_value: Config = Config::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.providers = __cw_config_init_1_1;
        __cw_config_init_1_value
    })
    .expect("explicit local Anthropic fixture client");
    client.retry.enabled = false;
    client.retry.max_retries = 0;
    client
}

async fn mount_page(server: &MockServer, cursor: Option<&str>, response: ResponseTemplate) {
    let cursor = cursor.map(str::to_owned);
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(move |request: &Request| {
            request
                .url
                .query_pairs()
                .find(|(key, _)| key == "after_id")
                .map(|(_, value)| value.into_owned())
                == cursor
        })
        .respond_with(response)
        .mount(server)
        .await;
}

fn page(data: Value, next: Option<&str>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": data, "has_more": next.is_some(), "last_id": next,
    }))
}

// This is an explicit fixture continuation contract, not a claim that any
// provider other than Anthropic supports after_id in production.
async fn collect_fixture(
    server: &MockServer,
    limits: ModelsFetchLimits,
) -> Result<String, ModelsFetchError> {
    let http = codewhale_release::tls::reqwest_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    collect_models_document(
        reqwest::Url::parse(&format!("{}/v1/models", server.uri())).unwrap(),
        Some("after_id"),
        limits,
        |url| {
            let http = http.clone();
            async move {
                http.get(url)
                    .send()
                    .await
                    .map_err(|_| CatalogRefreshError::Network.into())
            }
        },
    )
    .await
    .map(|(body, _)| body)
}

fn assert_invalid(result: Result<String, ModelsFetchError>) {
    assert_eq!(
        result
            .expect_err("incomplete or invalid observation must fail")
            .into_catalog(),
        CatalogRefreshError::InvalidResponse
    );
}

#[tokio::test]
async fn anthropic_after_id_completes_both_public_consumers_with_frozen_headers() {
    let server = MockServer::start().await;
    mount_page(
        &server,
        None,
        page(
            json!([
                {"id":"z-model", "owned_by":"first-owner", "created":7},
                {"id":"a-model"}
            ]),
            Some(CURSOR),
        ),
    )
    .await;
    mount_page(
        &server,
        Some(CURSOR),
        page(
            json!([
                {"id":"middle-model", "owned_by":"second-owner", "created":9},
                {"id":"z-model", "owned_by":"later-owner", "created":10}
            ]),
            None,
        ),
    )
    .await;
    let client = anthropic_client(&server.uri());

    let listed = client.list_models().await.unwrap();
    assert_eq!(
        listed.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["a-model", "middle-model", "z-model"]
    );
    assert_eq!(listed[1].owned_by.as_deref(), Some("second-owner"));
    assert_eq!(listed[1].created, Some(9));
    assert_eq!(listed[2].owned_by.as_deref(), Some("first-owner"));
    let delta = client.fetch_catalog_delta().await.unwrap();
    assert_eq!(delta.provider, "anthropic");
    assert_eq!(
        delta.base_url_fingerprint,
        base_url_fingerprint(&server.uri())
    );
    assert_eq!(
        delta
            .offerings
            .iter()
            .map(|row| row.wire_model_id.as_str())
            .collect::<Vec<_>>(),
        ["a-model", "middle-model", "z-model"]
    );
    for row in &delta.offerings {
        assert!(
            matches!(&row.source, CatalogSource::Live { base_url_fingerprint, fetched_at }
            if base_url_fingerprint == &delta.base_url_fingerprint && *fetched_at == delta.fetched_at)
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request.headers.get("x-api-key").unwrap(), KEY);
        assert_eq!(
            request.headers.get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        assert_eq!(
            request.headers.get("x-private-fixture").unwrap(),
            "custom-header-canary"
        );
        assert!(request.headers.get("authorization").is_none());
        let pairs = request.url.query_pairs().collect::<Vec<_>>();
        if index % 2 == 0 {
            assert!(pairs.is_empty());
        } else {
            assert_eq!(pairs.len(), 1);
            assert_eq!(pairs[0].0, "after_id");
            assert_eq!(pairs[0].1, CURSOR);
        }
    }
}

#[tokio::test]
async fn unpaginated_rosters_stay_single_request_and_unknown_continuation_refuses() {
    for go in [false, true] {
        let server = MockServer::start().await;
        let id = codewhale_config::opencode_go_models()[0];
        mount_models_json(&server, 200, json!({"data":[{"id":id}]})).await;
        let mut client = if go {
            opencode_go_client_for(&server)
        } else {
            openrouter_client_for(&server)
        };
        client.retry.enabled = false;
        client.retry.max_retries = 0;
        assert_eq!(client.list_models().await.unwrap().len(), 1);
        assert_eq!(
            client.fetch_catalog_delta().await.unwrap().offerings.len(),
            1
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.reset().await;
        mount_models_json(
            &server,
            200,
            json!({"data":[{"id":id}], "has_more":true, "last_id":"unsupported-next"}),
        )
        .await;
        assert!(client.list_models().await.is_err());
        assert_eq!(
            client.fetch_catalog_delta().await.unwrap_err(),
            CatalogRefreshError::InvalidResponse
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            2,
            "unsupported contract must not speculate a second request"
        );
        assert!(requests.iter().all(|request| request.url.query().is_none()));
    }
}

#[tokio::test]
async fn opencode_go_published_unpaginated_roster_keeps_documented_protocols() {
    let server = MockServer::start().await;
    let base_url = format!("{}/zen/go/v1", server.uri());
    // Literal additions from the pinned Go documentation plus retained routes:
    // do not generate this fixture from the production allowlist it verifies.
    let positives = [
        "glm-5.3-flash",
        "glm-5.3",
        "longcat-2.0",
        "deepseek-v4-flash-vision-exp",
        "hy4-preview",
        "hy3",
        "omen-alpha",
        "deepseek-v4-pro",
        "grok-4.5",
        "qwen3.8-max",
        "qwen3.8-flash",
        "minimax-m3",
        "grok-4.6",
        "gpt-5.6-luna",
        "muse-spark-1.3-contributor",
        "muse-spark-1.2-contributor",
    ];
    let negatives = ["gpt-unlisted", "claude-unproven"];
    let rows: Vec<_> = positives
        .iter()
        .chain(negatives.iter())
        .map(
            |id| json!({"id":id, "object":"model", "created":1_700_000_000, "owned_by":"opencode"}),
        )
        .collect();
    Mock::given(method("GET"))
        .and(path("/zen/go/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"object":"list", "data":rows})),
        )
        .mount(&server)
        .await;
    let mut client = CodewhaleClient::new(&{
        let __cw_config_init_2_0 = Some("opencode-go".into());
        let __cw_config_init_2_1 = Some(ProvidersConfig {
            opencode_go: ProviderConfig {
                api_key: Some(KEY.into()),
                base_url: Some(base_url.clone()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_2_value: Config = Config::default();
        __cw_config_init_2_value.provider = __cw_config_init_2_0;
        __cw_config_init_2_value.providers = __cw_config_init_2_1;
        __cw_config_init_2_value
    })
    .expect("explicit local Go route");
    client.retry.enabled = false;
    client.retry.max_retries = 0;

    let expected: std::collections::BTreeSet<_> = positives.into_iter().collect();
    let listed = client.list_models().await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|row| row.id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert_eq!(listed.len(), expected.len());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let delta = client.fetch_catalog_delta().await.unwrap();
    assert_eq!(delta.provider, "opencode-go");
    assert_eq!(delta.base_url_fingerprint, base_url_fingerprint(&base_url));
    assert_eq!(
        delta
            .offerings
            .iter()
            .map(|row| row.wire_model_id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert_eq!(delta.offerings.len(), expected.len());
    for row in &delta.offerings {
        assert_eq!(row.provider, "opencode-go");
        assert_eq!(
            Some(row.endpoint_key.as_str()),
            codewhale_config::opencode_go_endpoint_key(&row.wire_model_id)
        );
        assert_eq!(row.canonical_model, None);
        assert_eq!(row.family, None);
        assert_eq!(row.limit, None);
        assert_eq!(row.cost, None);
        assert_eq!(row.cost_source, None);
        assert_eq!(row.modalities, None);
        assert_eq!(row.attachment, None);
        assert_eq!(row.reasoning, None);
        assert_eq!(row.tool_call, None);
        assert_eq!(row.structured_output, None);
        assert!(row.reasoning_options.is_empty());
        assert!(
            matches!(&row.source, CatalogSource::Live { base_url_fingerprint, fetched_at }
            if base_url_fingerprint == &delta.base_url_fingerprint && *fetched_at == delta.fetched_at)
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "one unpaginated request per public consumer"
    );
    for request in requests {
        assert_eq!(request.url.path(), "/zen/go/v1/models");
        assert!(request.url.query().is_none());
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            format!("Bearer {KEY}").as_str()
        );
    }
}

#[tokio::test]
async fn models_redirects_never_reach_another_server_from_any_consumer() {
    let destination = MockServer::start().await;
    for status in [302, 307] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", format!("{}/capture", destination.uri())),
            )
            .mount(&server)
            .await;
        assert!(anthropic_client(&server.uri()).list_models().await.is_err());
        assert_eq!(
            anthropic_client(&server.uri())
                .fetch_catalog_delta()
                .await
                .unwrap_err(),
            CatalogRefreshError::Network
        );
        assert!(
            !anthropic_client(&server.uri())
                .health_check()
                .await
                .unwrap()
        );
        let recovery = anthropic_client(&server.uri());
        {
            let mut health = recovery.connection_health.lock().await;
            apply_request_failure(&mut health, Instant::now());
            apply_request_failure(&mut health, Instant::now());
        }
        recovery.maybe_probe_recovery().await;
        assert!(recovery.connection_health.lock().await.last_probe.is_some());
        assert!(
            verify_provider_api_key(ProviderKind::Anthropic, KEY, &server.uri())
                .await
                .is_err()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
    }
    // Redirects after a valid page are refused by both traversal consumers too.
    let server = MockServer::start().await;
    mount_page(
        &server,
        None,
        page(json!([{"id":"first-model"}]), Some(CURSOR)),
    )
    .await;
    mount_page(
        &server,
        Some(CURSOR),
        ResponseTemplate::new(308)
            .insert_header("location", format!("{}/capture", destination.uri())),
    )
    .await;
    assert!(anthropic_client(&server.uri()).list_models().await.is_err());
    assert_eq!(
        anthropic_client(&server.uri())
            .fetch_catalog_delta()
            .await
            .unwrap_err(),
        CatalogRefreshError::Network
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
    assert!(
        destination.received_requests().await.unwrap().is_empty(),
        "no auth/header/cursor may reach a redirect target"
    );
}

#[tokio::test]
async fn malformed_continuations_and_cursor_cycles_fail_without_partial_success() {
    for body in [
        json!({"data":[], "has_more":true}),
        json!({"data":[], "has_more":true, "last_id":""}),
        json!({"data":[], "has_more":true, "last_id":42}),
        json!({"data":[], "has_more":"true", "last_id":"x"}),
        json!({"data":{}, "has_more":false}),
        json!({"data":[], "has_more":true, "last_id":"12345"}),
    ] {
        let server = MockServer::start().await;
        mount_models_json(&server, 200, body).await;
        assert_invalid(
            collect_fixture(
                &server,
                ModelsFetchLimits {
                    cursor_bytes: 4,
                    ..MODELS_FETCH_LIMITS
                },
            )
            .await,
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
    for cycle in [false, true] {
        let server = MockServer::start().await;
        mount_page(&server, None, page(json!([{"id":"first"}]), Some("a"))).await;
        mount_page(
            &server,
            Some("a"),
            page(json!([]), Some(if cycle { "b" } else { "a" })),
        )
        .await;
        if cycle {
            mount_page(&server, Some("b"), page(json!([]), Some("a"))).await;
        }
        assert_invalid(collect_fixture(&server, MODELS_FETCH_LIMITS).await);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            if cycle { 3 } else { 2 }
        );
    }
}

#[tokio::test]
async fn cumulative_raw_bytes_rows_and_pages_are_limits_not_truncation() {
    let first = r#"{"data":[{"id":"same"},{"id":"same"}],"has_more":true,"last_id":"next"}"#;
    let second = r#"{"data":[{"id":"same"}],"has_more":false}"#;
    for limits in [
        ModelsFetchLimits {
            bytes: first.len() + second.len() - 1,
            ..MODELS_FETCH_LIMITS
        },
        ModelsFetchLimits {
            rows: 2,
            ..MODELS_FETCH_LIMITS
        },
        ModelsFetchLimits {
            pages: 1,
            ..MODELS_FETCH_LIMITS
        },
    ] {
        let server = MockServer::start().await;
        mount_page(
            &server,
            None,
            ResponseTemplate::new(200).set_body_raw(first, "application/json"),
        )
        .await;
        mount_page(
            &server,
            Some("next"),
            ResponseTemplate::new(200).set_body_raw(second, "application/json"),
        )
        .await;
        assert_invalid(collect_fixture(&server, limits).await);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            if limits.pages == 1 { 1 } else { 2 }
        );
    }
    let server = MockServer::start().await;
    mount_page(
        &server,
        None,
        ResponseTemplate::new(200).set_body_raw(first, "application/json"),
    )
    .await;
    mount_page(
        &server,
        Some("next"),
        ResponseTemplate::new(200).set_body_raw(second, "application/json"),
    )
    .await;
    let body = collect_fixture(
        &server,
        ModelsFetchLimits {
            bytes: first.len() + second.len(),
            rows: 3,
            pages: 2,
            ..MODELS_FETCH_LIMITS
        },
    )
    .await
    .unwrap();
    assert_eq!(
        parse_models_response(&body).unwrap().len(),
        1,
        "raw duplicates count toward limits before final deduplication"
    );
}

async fn read_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).await.unwrap();
        assert!(
            count > 0 && bytes.len() + count <= 16_384,
            "bounded fixture request header"
        );
        bytes.extend_from_slice(&chunk[..count]);
    }
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn chunked_catalog_body_is_bounded_without_content_length() {
    let body = r#"{"data":[{"id":"chunked-model"}]}"#;
    for limit in [20, body.len()] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/models", listener.local_addr().unwrap());
        let (first, second) = body.split_at(12);
        let response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
            first.len(),
            second.len(),
        );
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_head(&mut stream).await;
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let http = codewhale_release::tls::reqwest_client_builder()
            .build()
            .unwrap();
        let result = collect_models_document(
            reqwest::Url::parse(&endpoint).unwrap(),
            None,
            ModelsFetchLimits {
                bytes: limit,
                ..MODELS_FETCH_LIMITS
            },
            |url| {
                let http = http.clone();
                async move {
                    http.get(url)
                        .send()
                        .await
                        .map_err(|_| CatalogRefreshError::Network.into())
                }
            },
        )
        .await;
        if limit < body.len() {
            assert_eq!(
                result.unwrap_err().into_catalog(),
                CatalogRefreshError::InvalidResponse
            );
        } else {
            let (collected, _) = result.expect("same valid chunked body fits exact byte budget");
            assert_eq!(
                parse_models_response(&collected).unwrap()[0].id,
                "chunked-model"
            );
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn traversal_deadline_bounds_pending_and_completed_later_pages() {
    let server = MockServer::start().await;
    mount_models_json(
        &server,
        200,
        json!({"data":[{"id":"first"}], "has_more":true, "last_id":"next"}),
    )
    .await;
    // Fetch the first fixture response before starting the short clock so host
    // scheduling/connection setup cannot make this accidentally a page-one test.
    let mut first = Some(
        codewhale_release::tls::reqwest_client_builder()
            .build()
            .unwrap()
            .get(format!("{}/v1/models", server.uri()))
            .send()
            .await
            .unwrap(),
    );
    let mut calls = 0;
    let result = collect_models_document(
        reqwest::Url::parse(&format!("{}/v1/models", server.uri())).unwrap(),
        Some("after_id"),
        ModelsFetchLimits {
            timeout: Duration::from_millis(100),
            ..MODELS_FETCH_LIMITS
        },
        |_| {
            calls += 1;
            let response = first.take();
            async move {
                match response {
                    Some(response) => Ok(response),
                    None => std::future::pending().await,
                }
            }
        },
    )
    .await;
    assert_eq!(
        result.unwrap_err().into_catalog(),
        CatalogRefreshError::Network
    );
    assert_eq!(
        calls, 2,
        "the pending later page must be covered by the collector timeout"
    );

    // Immediate response bodies avoid a scheduler-dependent network margin.
    // Each completed fetch takes less than the short budget; together they
    // exceed it. The generous control proves the same pages are otherwise valid.
    for timeout in [Duration::from_millis(400), Duration::from_secs(5)] {
        let mut responses = [
            r#"{"data":[{"id":"first"}],"has_more":true,"last_id":"next"}"#,
            r#"{"data":[{"id":"second"}],"has_more":false}"#,
        ]
        .into_iter()
        .map(|body| {
            reqwest::Response::from(
                axum::http::Response::builder()
                    .status(200)
                    .body(body)
                    .unwrap(),
            )
        });
        let mut calls = 0;
        let result = collect_models_document(
            reqwest::Url::parse("http://127.0.0.1/v1/models").unwrap(),
            Some("after_id"),
            ModelsFetchLimits {
                timeout,
                ..MODELS_FETCH_LIMITS
            },
            |_| {
                calls += 1;
                let response = responses.next().expect("exactly two fixture pages");
                async move {
                    // Deliberately ready after bounded synchronous work, like
                    // parsing, so correctness cannot rely only on timer polling.
                    std::thread::sleep(Duration::from_millis(250));
                    Ok(response)
                }
            },
        )
        .await;
        assert_eq!(calls, 2);
        if timeout == Duration::from_millis(400) {
            assert_eq!(
                result.unwrap_err().into_catalog(),
                CatalogRefreshError::Network
            );
        } else {
            let (body, _) = result.expect("complete pages fit the larger shared budget");
            assert_eq!(parse_models_response(&body).unwrap().len(), 2);
        }
    }
}

#[tokio::test]
async fn raw_rows_keep_duplicate_known_fields_invalid_in_existing_parsers() {
    for malformed in [
        r#"{"id":"second","id":"replacement"}"#,
        r#"{"id":"second","pricing":{"prompt":"0.000001","prompt":"0.000002"}}"#,
    ] {
        let server = MockServer::start().await;
        mount_page(&server, None, page(json!([{"id":"first"}]), Some("next"))).await;
        let later = format!(r#"{{"data":[{malformed}],"has_more":false}}"#);
        mount_page(
            &server,
            Some("next"),
            ResponseTemplate::new(200).set_body_raw(later, "application/json"),
        )
        .await;
        let body = collect_fixture(&server, MODELS_FETCH_LIMITS).await.unwrap();
        assert!(
            body.contains(malformed),
            "collector must retain original row fields"
        );
        // OpenRouter decodes per row (#6690): the ambiguous row is skipped as
        // malformed, never accepted with a last-wins value, and the valid row
        // survives.
        let openrouter = parse_openrouter_models_response(&body).unwrap();
        let ids: Vec<&str> = openrouter.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["first"]);
        assert_eq!(
            parse_baseten_models_response(&body).unwrap_err(),
            CatalogRefreshError::InvalidResponse
        );
    }
}

#[tokio::test]
async fn existing_provider_parsers_own_cross_page_duplicates_and_full_metadata() {
    let server = MockServer::start().await;
    mount_page(
        &server,
        None,
        page(
            json!([{
                "id":"same/model", "context_length":32000,
                "pricing":{"prompt":"0.000001", "completion":"0.000002"},
                "codewhale":{"protocol":"anthropic-messages", "default":true}
            }]),
            Some("next"),
        ),
    )
    .await;
    mount_page(&server, Some("next"), page(json!([
        {"id":"same/model", "context_length":99999, "pricing":{"prompt":"0.000009", "completion":"0.000009"}},
        {"id":"later/model", "context_length":64000, "max_completion_tokens":8000,
         "pricing":{"prompt":"0.000003", "completion":"0.000004", "input_cache_read":"0.0000003"},
         "supported_features":["vision"], "supported_parameters":["tools", "reasoning"],
         "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]},
         "codewhale":{"protocol":"chat-completions"}}
    ]), None)).await;
    let body = collect_fixture(&server, MODELS_FETCH_LIMITS).await.unwrap();
    let openrouter = parse_openrouter_models_response(&body).unwrap();
    assert_eq!(openrouter.len(), 2);
    let first =
        openrouter_to_catalog_offering(&openrouter[0], "openrouter", "fixture-fp", 42).unwrap();
    assert_eq!(first.limit.unwrap().context, Some(32000));
    assert_eq!(first.cost.unwrap().input, Some(1.0));
    let later =
        openrouter_to_catalog_offering(&openrouter[1], "openrouter", "fixture-fp", 42).unwrap();
    assert_eq!(later.limit.unwrap().context, Some(64000));
    assert_eq!(later.cost.unwrap().cache_read, Some(0.3));
    assert_eq!(later.reasoning, Some(true));
    assert_eq!(later.tool_call, Some(true));
    assert_eq!(later.modalities.unwrap().input, ["text", "image"]);
    assert_eq!(
        parse_baseten_models_response(&body).unwrap_err(),
        CatalogRefreshError::InvalidResponse
    );
    let codewhale =
        codewhale_catalog_offerings_from_body(&body, "codewhale", "fixture-fp", 42).unwrap();
    assert_eq!(codewhale.len(), 2);
    assert_eq!(codewhale[0].endpoint_key, "messages");
    assert!(codewhale[0].default_for_provider);
    assert_eq!(codewhale[1].endpoint_key, "chat");

    server.reset().await;
    mount_page(
        &server,
        None,
        page(json!([{"id":" same/model "}]), Some("next")),
    )
    .await;
    mount_page(
        &server,
        Some("next"),
        page(json!([{"id":"same/model"}]), None),
    )
    .await;
    let body = collect_fixture(&server, MODELS_FETCH_LIMITS).await.unwrap();
    assert_eq!(
        parse_baseten_models_response(&body).unwrap_err(),
        CatalogRefreshError::InvalidResponse,
        "Baseten trims before duplicate rejection across pages"
    );

    server.reset().await;
    mount_page(
        &server,
        None,
        page(json!([{"id":"first/model"}]), Some("next")),
    )
    .await;
    mount_page(&server, Some("next"), page(json!([{"id":"later/model", "context_length":"64000", "max_completion_tokens":8000,
        "pricing":{"prompt":"0.000003", "completion":"0.000004"}, "supported_features":["vision"]}]), None)).await;
    let body = collect_fixture(&server, MODELS_FETCH_LIMITS).await.unwrap();
    let rows = parse_baseten_models_response(&body).unwrap();
    let later = baseten_to_catalog_offering(&rows[1], "base-ten", "fixture-fp", 42).unwrap();
    assert_eq!(later.provider, "base-ten");
    assert_eq!(later.limit.unwrap().output, Some(8000));
    assert_eq!(later.cost.unwrap().output, Some(4.0));
    assert_eq!(later.attachment, Some(true));
}

#[tokio::test]
async fn later_page_failure_preserves_complete_same_scope_cache_and_observation_time() {
    let server = MockServer::start().await;
    mount_models_json(
        &server,
        200,
        json!({"data":[{"id":"old-first"},{"id":"old-second"}]}),
    )
    .await;
    let client = anthropic_client(&server.uri());
    let mut cache = ProviderCatalogCache::new();
    let mut original = client.fetch_catalog_delta().await.unwrap();
    original.fetched_at = 17;
    cache.record_success(original, 3600);
    let fingerprint = base_url_fingerprint(&server.uri());
    let before = cache.get("anthropic", &fingerprint).unwrap().clone();
    for (response, expected) in [
        (
            ResponseTemplate::new(401),
            CatalogRefreshError::Unauthorized,
        ),
        (ResponseTemplate::new(429), CatalogRefreshError::RateLimited),
        (ResponseTemplate::new(500), CatalogRefreshError::Network),
        (
            ResponseTemplate::new(200).set_body_string("broken-json"),
            CatalogRefreshError::InvalidResponse,
        ),
    ] {
        server.reset().await;
        mount_page(
            &server,
            None,
            page(json!([{"id":"new-partial-only"}]), Some("next")),
        )
        .await;
        mount_page(&server, Some("next"), response).await;
        assert_eq!(
            client.refresh_catalog_cache(&mut cache, 3600).await,
            CatalogStatus::Failed { reason: expected }
        );
        let retained = cache.get("anthropic", &fingerprint).unwrap();
        assert_eq!(retained.offerings, before.offerings);
        assert_eq!(retained.fetched_at, 17);
        assert_eq!(retained.ttl_secs, before.ttl_secs);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

fn assert_no_canaries(error: &anyhow::Error) {
    let surfaced = format!(
        "{error:#} {error:?} {:?}",
        codewhale_runtime::retry_status::snapshot()
    );
    for canary in [KEY, CURSOR, "cursor%2Fsecond", "custom-header-canary"] {
        assert!(
            !surfaced.contains(canary),
            "model error or retry state exposed a secret/cursor"
        );
    }
}

/// #6173: a geo-blocked key produced `Invalid request (400): ` — the colon
/// that introduces the provider's reason, with nothing after it, because the
/// catalog path discarded the body wholesale. A geo-block, a bad key and a
/// wrong endpoint were then indistinguishable, and the reporter had to change
/// VPN exits to find out which one it was. The reason is the provider's own
/// words; only this client's secrets have to go.
#[tokio::test]
async fn catalog_errors_surface_the_provider_reason_without_client_secrets() {
    const REASON: &str = "User location is not supported for the API use.";

    let server = MockServer::start().await;
    mount_page(
        &server,
        None,
        ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code": 400, "message": REASON, "status": "FAILED_PRECONDITION"}
        })),
    )
    .await;
    let client = anthropic_client(&server.uri());
    let error = client.list_models().await.unwrap_err();
    assert!(
        format!("{error:#}").contains(REASON),
        "the provider's reason must reach the user: {error:#}"
    );
    assert_no_canaries(&error);

    // The same reason, from an endpoint that also echoes back things only
    // this client could have sent it. The reason survives; they do not.
    let echoing = MockServer::start().await;
    mount_page(
        &echoing,
        None,
        ResponseTemplate::new(400).set_body_json(json!({
            "error": {"message": format!("{REASON} key={KEY} header=custom-header-canary")}
        })),
    )
    .await;
    let client = anthropic_client(&echoing.uri());
    codewhale_runtime::retry_status::clear();
    let error = client.list_models().await.unwrap_err();
    assert!(format!("{error:#}").contains(REASON), "{error:#}");
    assert_no_canaries(&error);
    codewhale_runtime::retry_status::clear();
}

#[tokio::test]
async fn later_page_http_and_transport_errors_do_not_expose_cursor_or_key() {
    for isolated in [false, true] {
        let server = MockServer::start().await;
        mount_page(
            &server,
            None,
            page(json!([{"id":"first-model"}]), Some(CURSOR)),
        )
        .await;
        mount_page(
            &server,
            Some(CURSOR),
            ResponseTemplate::new(500).set_body_json(json!({
                "error":{"message":format!("bad cursor {CURSOR}; key {KEY}; custom-header-canary")}
            })),
        )
        .await;
        let mut client = anthropic_client(&server.uri());
        client.isolated_request_state = isolated;
        client.retry.enabled = true;
        client.retry.max_retries = 1;
        client.retry.initial_delay = 0.001;
        client.retry.max_delay = 0.001;
        codewhale_runtime::retry_status::clear();
        let error = client.list_models().await.unwrap_err();
        assert_no_canaries(&error);
        if !isolated {
            assert!(codewhale_runtime::retry_status::snapshot().is_failed());
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        codewhale_runtime::retry_status::clear();

        server.reset().await;
        mount_page(
            &server,
            None,
            page(json!([{"id":"first-model"}]), Some(CURSOR)),
        )
        .await;
        mount_page(
            &server,
            Some(CURSOR),
            page(
                json!([{
                    "id":"second-model", "created":format!("{CURSOR} {KEY} custom-header-canary")
                }]),
                None,
            ),
        )
        .await;
        let error = client.list_models().await.unwrap_err();
        assert_no_canaries(&error);
        assert!(error.to_string().contains("InvalidResponse"));
        assert_eq!(
            client.fetch_catalog_delta().await.unwrap_err(),
            CatalogRefreshError::InvalidResponse
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let heads = Arc::new(StdMutex::new(Vec::new()));
    let recorded = heads.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let head = read_head(&mut stream).await;
            let is_first = !head.lines().next().unwrap().contains("after_id=");
            recorded.lock().unwrap().push(head);
            if is_first {
                let body =
                    json!({"data":[{"id":"first-model"}], "has_more":true, "last_id":CURSOR})
                        .to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            // Page two closes before response headers, producing a transport
            // error whose reqwest URL used to contain the opaque cursor.
        }
    });
    let client = anthropic_client(&base_url);
    let error = client.list_models().await.unwrap_err();
    assert_no_canaries(&error);
    assert!(
        heads
            .lock()
            .unwrap()
            .iter()
            .any(|head| head.contains("after_id="))
    );
    server.abort();
    let _ = server.await;
    codewhale_runtime::retry_status::clear();
}

#[tokio::test]
async fn unpaginated_custom_identities_keep_exact_identity_and_endpoint_ownership() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    mount_models_json(&first, 200, json!({"data":[{"id":"first/model"}]})).await;
    mount_models_json(&second, 200, json!({"data":[{"id":"second/model"}]})).await;
    let mut cache = ProviderCatalogCache::new();
    for (identity, server, expected) in [
        ("base-ten", &first, "first/model"),
        ("Base-Ten", &first, "first/model"),
        ("base-ten", &second, "second/model"),
    ] {
        let client = custom_mock_client_for_identity(server, identity);
        let delta = client.fetch_catalog_delta().await.unwrap();
        assert_eq!(delta.provider, identity);
        assert_eq!(delta.offerings[0].provider, identity);
        assert_eq!(delta.offerings[0].wire_model_id, expected);
        assert_eq!(
            delta.base_url_fingerprint,
            base_url_fingerprint(&format!("{}/v1", server.uri()))
        );
        cache.record_success(delta, 3600);
    }
    assert_eq!(cache.entries.len(), 3);
}

/// `models` is the wider spelling some relays echo beside the OpenAI `data`
/// roster, and it is often a stub. It is not a second name for `data`: when it
/// was declared as one (`alias = "models"`), a body carrying both keys failed
/// the whole probe with `duplicate field`, so a route whose roster was present
/// and well-formed reported `invalid_response` and listed nothing. `data` is
/// the roster this envelope reads first; `models` stays for hosts that serve
/// only that spelling.
#[tokio::test]
async fn roster_envelope_carrying_data_and_models_reads_the_data_roster() {
    let server = MockServer::start().await;
    mount_models_json(
        &server,
        200,
        json!({
            "object": "list",
            "data": [{"id": "relay-real-a"}, {"id": "relay-real-b"}],
            "models": [{"id": "wider-spelling-stub"}],
            "total_count": 2,
        }),
    )
    .await;
    let client = custom_mock_client_for_identity(&server, "relay-private");
    let delta = client
        .fetch_catalog_delta()
        .await
        .expect("both spellings present is a complete roster, not an invalid response");
    assert_eq!(
        delta
            .offerings
            .iter()
            .map(|row| row.wire_model_id.as_str())
            .collect::<Vec<_>>(),
        ["relay-real-a", "relay-real-b"]
    );
}

/// An empty `data` roster beside a populated `models` array is an empty
/// listing, not a reason to read the other key. A present `data` is the
/// OpenAI-compatible roster; only a host that serves no `data` at all falls
/// back to the wider spelling.
#[tokio::test]
async fn roster_envelope_with_empty_data_is_an_empty_listing_not_a_models_fallback() {
    let server = MockServer::start().await;
    mount_models_json(
        &server,
        200,
        json!({
            "object": "list",
            "data": [],
            "models": [{"id": "wider-spelling-stub"}],
        }),
    )
    .await;
    let client = custom_mock_client_for_identity(&server, "relay-private");
    assert_eq!(
        client
            .fetch_catalog_delta()
            .await
            .expect_err("an empty data roster is an empty listing"),
        CatalogRefreshError::EmptyList
    );
}

/// A custom table is reached through the OpenAI-compatible surface, so its
/// `/models` listing follows the OpenAI auth convention even when the table
/// declares `wire = "anthropic"` for Messages. A relay can answer
/// `/v1/messages` on either header and still reject `x-api-key` on
/// `/v1/models` with `401 missing_api_key`, so the listing must not inherit
/// the Messages choice.
#[tokio::test]
async fn custom_messages_wire_listing_authenticates_with_bearer() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let server = MockServer::start().await;
    mount_models_json(
        &server,
        200,
        json!({"object": "list", "data": [{"id": "relay-model"}]}),
    )
    .await;
    let mut providers = ProvidersConfig::default();
    providers.custom.insert(
        "relay-messages-wire".to_string(),
        ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            api_key: Some(KEY.to_string()),
            base_url: Some(format!("{}/v1", server.uri())),
            model: Some("relay-model".to_string()),
            wire: Some("anthropic".to_string()),
            ..ProviderConfig::default()
        },
    );
    let client = CodewhaleClient::new(&{
        let __cw_config_init_3_0 = Some("relay-messages-wire".to_string());
        let __cw_config_init_3_1 = Some(providers);
        let mut __cw_config_init_3_value: Config = Config::default();
        __cw_config_init_3_value.provider = __cw_config_init_3_0;
        __cw_config_init_3_value.providers = __cw_config_init_3_1;
        __cw_config_init_3_value
    })
    .expect("custom Messages-wire relay client");

    let delta = client
        .fetch_catalog_delta()
        .await
        .expect("the relay's listing answers on its own terms");
    assert_eq!(
        delta
            .offerings
            .iter()
            .map(|row| row.wire_model_id.as_str())
            .collect::<Vec<_>>(),
        ["relay-model"]
    );

    let request = server
        .received_requests()
        .await
        .expect("requests")
        .pop()
        .expect("one listing request");
    let expected = format!("Bearer {KEY}");
    assert_eq!(
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some(expected.as_str()),
        "the listing follows the OpenAI auth convention"
    );
    assert!(
        request.headers.get("x-api-key").is_none(),
        "the listing must not inherit the Messages x-api-key convention"
    );
}
