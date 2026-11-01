//! Real pinned Builtin + actual Core captures; no external provider calls.
use super::*;
use crate::extension_host::test_support::node_for_tests;
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_runtime::features::{Feature, Features};
use std::sync::Arc;

fn host_context(root: &std::path::Path) -> ToolContext {
    let mut flags = Features::with_defaults();
    flags.enable(Feature::WebSearchHost);
    ToolContext::new(root).with_features(flags)
}
fn new_manager(node: std::path::PathBuf, root: &std::path::Path) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(root.join("host")),
        ..ExtensionHostOptions::default()
    }))
}
#[test]
fn web_flags_are_separate_and_default_to_rust() {
    let mut flags = Features::with_defaults();
    assert!(!flags.enabled(Feature::WebSearchHost));
    assert!(!flags.enabled(Feature::WebExtractHost));
    flags.enable(Feature::WebSearchHost);
    assert!(!flags.enabled(Feature::ExtensionHost));
    assert!(!flags.enabled(Feature::WebExtractHost));
    assert_eq!(
        codewhale_runtime::features::feature_from_key("web_search_host"),
        Some(Feature::WebSearchHost)
    );
    assert_eq!(
        codewhale_runtime::features::feature_from_key("web_extract_host"),
        Some(Feature::WebExtractHost)
    );
}
#[test]
fn provider_snapshot_removes_unknown_credentials_and_uses_opaque_source_handles() {
    let raw = json!({"api_key":"fixture-secret","private":{"token":"fixture-secret"},"results":[{"title":"Echo fixture-secret","url":"  https://user:password@example.com/page?token=fixture-secret  ","content":"Visible fixture-secret","credential":"fixture-secret"}]});
    let mut urls = OpaqueUrls::default();
    let captured = provider_projection(&raw, &mut urls, Some("fixture-secret"));
    let wire = captured.to_string();
    assert!(!wire.contains("fixture-secret"));
    assert!(!wire.contains("password"));
    assert!(!wire.contains("example.com"));
    assert!(!wire.contains("api_key"));
    let handle = captured["results"][0]["url"].as_str().unwrap();
    assert_eq!(
        urls.restore(handle).unwrap(),
        raw["results"][0]["url"].as_str().unwrap()
    );
    assert_eq!(
        urls.restore(handle.trim()).unwrap(),
        raw["results"][0]["url"].as_str().unwrap().trim()
    );
    assert!(!urls.restore("invented-source").unwrap_err().content());
}
#[tokio::test(flavor = "current_thread")]
async fn real_host_all_nine_provider_decoders_match_current_rust_semantics() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_web_provider_parity") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let context = host_context(root.path());
    let rows = json!([{"title":" \u{0085}A\u{0085} ","url":" https://example.com/a?private=1 ","content":null,"snippet":" fallback ","score":-0.0},
        {"title":"B","url":"https://example.com/b","content":" body ","score":"3.2"},
        {"title":"","url":"https://example.com/empty"}]);
    let cases = vec![
        (
            "tavily",
            json!({"results":rows}),
            parse_tavily_results as fn(&Value, usize) -> Vec<WebSearchEntry>,
        ),
        (
            "firecrawl",
            json!({"data":{"web":rows},"success":true}),
            parse_firecrawl_results,
        ),
        (
            "metaso",
            json!({"webpages":[{"title":" A ","link":" https://example.com/a ","snippet":null,"summary":" fallback "}],"code":0}),
            parse_metaso_results,
        ),
        (
            "bocha",
            json!({"pages":[{"name":null,"title":"unused","url":"https://example.com/unused"},{"name":" A ","link":" https://example.com/a ","summary":null,"snippet":"unused"}],"code":200}),
            parse_bocha_results,
        ),
        (
            "baidu",
            json!({"references":[{"title":" A ","link":" https://example.com/a ","content":null,"snippet":"unused"}],"error_code":0}),
            parse_baidu_results,
        ),
        ("searxng", json!({"results":rows}), parse_searxng_results),
        ("sofya", json!({"results":rows}), parse_sofya_results),
        (
            "serply",
            json!({"results":[{"title":" A ","link":" https://example.com/a ","description":" body "}]}),
            parse_serply_results,
        ),
    ];
    for (name, raw, parse) in cases {
        let expected = parse(&raw, 5);
        let actual = host_provider(name, &raw, 5, None, &context, 5_000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual, expected, "{name}");
    }
    let text = "```json\n{\"results\":[{\"title\":\" A \",\"url\":\" https://example.com/a?token=private \",\"snippet\":\" body \"}]}\n```";
    let raw = json!({"output":[{"type":"message","content":[{"text":text}]}]});
    assert_eq!(
        host_provider("volcengine", &raw, 5, None, &context, 5_000)
            .await
            .unwrap()
            .unwrap(),
        parse_volcengine_results(text, 5)
    );
    for (raw, expected) in [
        (json!({"code":2005}), Some("API key rejected")),
        (json!({"code":2005.0}), None),
        (json!({"code":"2005"}), None),
    ] {
        let result = host_provider("metaso", &raw, 5, None, &context, 5_000).await;
        if let Some(expected) = expected {
            let error = result.unwrap_err();
            assert!(error.content());
            assert!(error.to_string().contains(expected));
        } else {
            assert!(result.unwrap().unwrap().is_empty());
        }
    }
    assert!(!codewhale_plugins::activation::extension_host_policy_enabled());
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn real_host_keyless_requests_and_final_receipt_match_rust() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_web_request_parity") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let context = host_context(root.path());
    let query = SearchQuery::new(
        "authorized 漢字 query".into(),
        5,
        Some(Recency::Days(10)),
        vec!["example.com".into()],
        Some("de_DE".into()),
    );
    let filters = QueryFilters::of(&query);
    let plan = host_request("tavily", &query.query, filters, 5, &context, 5_000)
        .await
        .unwrap()
        .unwrap();
    let mut expected = tavily_search_payload("private-key", &query.query, filters, 5);
    expected.as_object_mut().unwrap().remove("api_key");
    assert_eq!(plan.payload, expected);
    assert_eq!(
        host_request("baidu", &query.query, filters, 5, &context, 5_000)
            .await
            .unwrap()
            .unwrap()
            .payload,
        baidu_search_payload(&query.query, 5)
    );
    let mut expected = volcengine_search_payload(&query.query, 5);
    for key in ["model", "stream", "tools"] {
        expected.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(
        host_request("volcengine", &query.query, filters, 5, &context, 5_000)
            .await
            .unwrap()
            .unwrap()
            .payload,
        expected
    );
    let entries = vec![
        SearchResult::new(1, "A".into(), "https://example.com/a".into(), None, None),
        SearchResult::new(
            2,
            "Other".into(),
            "https://other.example/a".into(),
            None,
            None,
        ),
    ];
    let make = || BackendSearch {
        backend: BackendId::ProviderNative,
        source: "native".into(),
        backend_detail: Some("api.example.com".into()),
        results: entries.clone(),
        degraded: vec![DegradedReason::AnswerCutByProvider],
        note: Some("private answer retained in Core".into()),
    };
    let started = Instant::now();
    let expected = finalize_search_response(
        query.clone(),
        crate::tools::web::contract::QueryCapabilities::count_only(),
        make(),
        started,
    );
    let actual = finalize_search_response_for_context(
        query,
        crate::tools::web::contract::QueryCapabilities::count_only(),
        make(),
        started,
        &context,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(actual.message, expected.message);
    assert_eq!(actual.results, expected.results);
    assert_eq!(actual.receipt.honored, expected.receipt.honored);
    assert_eq!(actual.receipt.degraded, expected.receipt.degraded);
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn real_core_http_capture_and_host_request_are_bounded_without_credentials_on_wire() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_core_web_http_capture") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let context = host_context(root.path());
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST")).and(wiremock::matchers::path("/search"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"success":true,"data":{"web":[{"title":" A ","url":" https://example.com/a ","description":" body "}]}})))
        .mount(&server).await;
    let endpoint = format!("{}/search", server.uri());
    let (entries, note) = WebSearchTool
        .run_firecrawl_search_at_for_context(
            &endpoint,
            "approved query",
            QueryFilters::default(),
            5,
            5_000,
            Some("private-test-key"),
            &context,
        )
        .await
        .unwrap();
    assert_eq!(entries[0].url, "https://example.com/a");
    assert_eq!(note, "Firecrawl authenticated");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer private-test-key"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].body).unwrap()["query"],
        "approved query"
    );
    wiremock::Mock::given(wiremock::matchers::path("/oversize"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_string("x".repeat(1024 * 1024 + 1)),
        )
        .mount(&server)
        .await;
    let response = codewhale_release::tls::reqwest_client_builder()
        .build()
        .unwrap()
        .get(format!("{}/oversize", server.uri()))
        .send()
        .await
        .unwrap();
    let error = adapter::read_response(response, &context)
        .await
        .unwrap_err();
    assert_eq!(error.origin, adapter::FailureOrigin::CaptureGuard);
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn selected_web_host_refuses_unavailable_runtime_and_pre_cancel_without_transport_fallback() {
    let _home = crate::test_support::SealedHome::new();
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(root.path().join("missing-node"), root.path());
    let _manager = TestManagerGuard::install(manager);
    let context = host_context(root.path());
    let error = host_provider("tavily", &json!({"results":[]}), 5, None, &context, 1_000)
        .await
        .unwrap_err();
    assert_eq!(error.origin, adapter::FailureOrigin::Host);
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let context = context.with_cancel_token(token);
    assert!(
        !host_request(
            "tavily",
            "query",
            QueryFilters::default(),
            5,
            &context,
            1_000
        )
        .await
        .unwrap_err()
        .content()
    );
}
