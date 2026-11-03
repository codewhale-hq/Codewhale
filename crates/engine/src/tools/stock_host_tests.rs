//! Actual Core ToolSpecs, committed pinned host and local HTTP/file fixtures.
//! No provider traffic. Host selection is independent of third-party Native policy.
use super::*;
use crate::extension_host::test_support::node_for_tests;
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use crate::tools::validate_data::ValidateDataTool;
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_runtime::features::{Feature, Features};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn manager(node: std::path::PathBuf, home: &std::path::Path) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(home.join("host")),
        ..ExtensionHostOptions::default()
    }))
}
fn context(root: &std::path::Path, feature: Option<Feature>) -> ToolContext {
    let mut features = Features::with_defaults();
    if let Some(feature) = feature {
        features.enable(feature);
    }
    ToolContext::new(root).with_features(features)
}
fn equal_tool_results(rust: &ToolResult, host: &ToolResult) {
    assert_eq!(host.success, rust.success);
    assert_eq!(host.content, rust.content, "exact user/model result bytes");
    assert_eq!(host.metadata, rust.metadata);
}

#[test]
fn finance_and_data_host_flags_are_separate_and_default_to_rust() {
    let defaults = Features::with_defaults();
    assert!(!defaults.enabled(Feature::FinanceHost));
    assert!(!defaults.enabled(Feature::DataHost));
    assert_eq!(
        codewhale_runtime::features::feature_from_key("finance_host"),
        Some(Feature::FinanceHost)
    );
    assert_eq!(
        codewhale_runtime::features::feature_from_key("data_host"),
        Some(Feature::DataHost)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn real_host_data_recorded_results_match_rust_without_native_activation() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_data_recorded_results") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let rust = context(home.path(), None);
    let host = context(home.path(), Some(Feature::DataHost));
    let cases = [
        json!({"content":"{\"😀\":1,\"\":2,\"__proto__\":3,\"a\":4}"}),
        json!({"content":"[1,true,null]", "format":"json"}),
        json!({"content":"42", "format":"json"}),
        json!({"content":"-0.0", "format":"json"}),
        json!({"content":"title = \"hello\"\ndate = 2026-10-02\n[a]\nvalue = 2\n"}),
        json!({"content":"{bad 😀", "format":"json"}),
        json!({"content":"key = [", "format":"toml"}),
        json!({"content":"neither valid JSON nor TOML"}),
    ];
    for input in cases {
        let expected = ValidateDataTool
            .execute(input.clone(), &rust)
            .await
            .unwrap();
        let actual = ValidateDataTool.execute(input, &host).await.unwrap();
        equal_tool_results(&expected, &actual);
    }
    // File extension must pin a failing JSON parser even if TOML could succeed.
    std::fs::write(home.path().join("pinned.json"), "value = 1").unwrap();
    let input = json!({"path":"pinned.json"});
    equal_tool_results(
        &ValidateDataTool
            .execute(input.clone(), &rust)
            .await
            .unwrap(),
        &ValidateDataTool.execute(input, &host).await.unwrap(),
    );
    assert!(!codewhale_plugins::activation::extension_host_policy_enabled());
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_host_finance_quote_and_chart_results_match_rust_exactly() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_finance_quote_and_chart") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let server = MockServer::start().await;
    let tool = FinanceTool::with_endpoints(
        format!("{}/quote", server.uri()),
        format!("{}/chart", server.uri()),
    );
    let rust = context(home.path(), None);
    let host = context(home.path(), Some(Feature::FinanceHost));
    let fixtures = [
        (
            json!({"quoteResponse":{"result":[{"symbol":"btc-usd","regularMarketPrice":125.0,"regularMarketPreviousClose":100.0,"regularMarketTime":9223372036854775807i64,"longName":"Long","shortName":"Short","fullExchangeName":"Full","exchange":"Low"}]}}),
            json!({"chart":{"result":[]}}),
        ),
        (
            json!({"quoteResponse":{"result":[{"symbol":"BTC-USD","regularMarketPrice":-0.0,"regularMarketPreviousClose":0.0}]}}),
            json!({"chart":{"result":[]}}),
        ),
        (
            json!({"quoteResponse":{"result":[{"symbol":"BTC-USD","regularMarketPrice":1e308,"regularMarketPreviousClose":-1e308}]}}),
            json!({"chart":{"result":[]}}),
        ),
        (
            json!({"quoteResponse":{"result":[]}}),
            json!({"chart":{"result":[{"meta":{"symbol":"BTC-USD","regularMarketPrice":125.0,"chartPreviousClose":100.0,"previousClose":50.0,"regularMarketTime":-9223372036854775808i64,"instrumentType":"CRYPTOCURRENCY"}}],"error":null}}),
        ),
    ];
    for (quote, chart) in fixtures {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .respond_with(ResponseTemplate::new(200).set_body_json(quote))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/BTC-USD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chart))
            .mount(&server)
            .await;
        let input = json!({"ticker":"BTC","timeout_ms":60000});
        let expected = tool.execute(input.clone(), &rust).await.unwrap();
        let actual = tool.execute(input, &host).await.unwrap();
        equal_tool_results(&expected, &actual);
    }
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_host_finance_domain_errors_match_and_do_not_change_fallback_policy() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_finance_domain_errors") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let server = MockServer::start().await;
    let tool = FinanceTool::with_endpoints(
        format!("{}/quote", server.uri()),
        format!("{}/chart", server.uri()),
    );
    for (quote, chart) in [
        (
            json!({"quoteResponse":{"result":[]}}),
            json!({"chart":{"error":{"code":"Not Found","description":"Symbol may be delisted"}}}),
        ),
        (
            json!({"quoteResponse":{"result":[{"symbol":"BTC-USD"}]}}),
            json!({"chart":{"error":{"code":"rate limit","description":"Try later"}}}),
        ),
    ] {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .respond_with(ResponseTemplate::new(200).set_body_json(quote))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/BTC-USD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chart))
            .mount(&server)
            .await;
        let input = json!({"ticker":"BTC","timeout_ms":60000});
        let expected = tool
            .execute(input.clone(), &context(home.path(), None))
            .await
            .unwrap_err();
        let actual = tool
            .execute(input, &context(home.path(), Some(Feature::FinanceHost)))
            .await
            .unwrap_err();
        assert_eq!(actual.to_string(), expected.to_string());
    }
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn selected_host_missing_runtime_refuses_both_tools_without_rust_fallback() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let home = tempfile::tempdir().unwrap();
    let manager = manager(home.path().join("absent-node"), home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let data = ValidateDataTool
        .execute(
            json!({"content":"{}"}),
            &context(home.path(), Some(Feature::DataHost)),
        )
        .await;
    assert!(data.is_err(), "Rust validation would have succeeded");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/quote"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"quoteResponse":{"result":[{"symbol":"AAPL","regularMarketPrice":1.0}]}}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/chart/AAPL"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"chart":{"result":[{"meta":{"symbol":"AAPL","regularMarketPrice":2.0}}]}}),
        ))
        .expect(0)
        .mount(&server)
        .await;
    let tool = FinanceTool::with_endpoints(
        format!("{}/quote", server.uri()),
        format!("{}/chart", server.uri()),
    );
    assert!(
        tool.execute(
            json!({"ticker":"AAPL"}),
            &context(home.path(), Some(Feature::FinanceHost))
        )
        .await
        .is_err()
    );
    server.verify().await;
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn host_input_bound_and_cancel_refuse_before_admitting_transform() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let home = tempfile::tempdir().unwrap();
    let host = context(home.path(), Some(Feature::DataHost));
    assert!(matches!(
        ValidateDataTool
            .execute(json!({"content":"x".repeat(1024*1024+1)}), &host)
            .await,
        Err(ToolError::InvalidInput { .. })
    ));
    let manager = manager(home.path().join("absent-node"), home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let host = host.with_cancel_token(cancel);
    let error = ValidateDataTool
        .execute(json!({"content":"{}"}), &host)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    manager.shutdown().await;
}
