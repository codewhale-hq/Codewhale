//! Actual complete HTML bytes stay private through the real Builtin selection.
use super::*;
use crate::extension_host::tests::node_for_tests;
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use crate::plugins::activation::TestPolicyGuard;
use crate::tools::spec::ToolContext;
use codewhale_runtime::features::{Feature, Features};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
async fn real_host_complete_html_region_selection_preserves_all_document_bytes() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_web_html_parity") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(root.path().join("host")),
        ..ExtensionHostOptions::default()
    }));
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let rust = ToolContext::new(root.path());
    let mut flags = Features::with_defaults();
    flags.enable(Feature::WebExtractHost);
    let host = rust.clone().with_features(flags);
    let large = format!(
        "<html><head><title>Whole document</title></head><body><nav>private navigation</nav><main><a href='../relative'>link</a><p>{}END_SENTINEL</p></main></body></html>",
        "Meaningful 漢字 words remain complete. ".repeat(40_000)
    );
    for html in ["<body><article>short</article><main>Five meaningful words survive this complete main-region extraction path.</main></body>".to_owned(),"<body><article>Five meaningful words stay in first article region.</article><main>Other valid words live in the second main region.</main></body>".into(),large] {
        let expected=extract_document("https://example.com/docs/page",Some("text/html"),html.as_bytes(),false,Some(&rust)).await.unwrap();
        let actual=extract_document("https://example.com/docs/page",Some("text/html"),html.as_bytes(),false,Some(&host)).await.unwrap();
        assert_eq!(actual.text,expected.text);assert_eq!(actual.markdown,expected.markdown);assert_eq!(actual.cleaned_html,expected.cleaned_html);assert_eq!(actual.title,expected.title);
        if html.contains("END_SENTINEL"){assert!(actual.text.contains("END_SENTINEL"));assert!(actual.markdown.len()>1024*1024);}
    }
    let error = extract_document(
        "https://example.com/shell",
        Some("text/html"),
        b"<body><div id='root'></div></body>",
        false,
        Some(&host),
    )
    .await
    .unwrap_err();
    assert!(error.content());
    assert!(is_js_shell_error(&error.error));
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn host_failure_with_shell_marker_never_earns_revalidation() {
    use super::super::fetch::{FetchOptions, fetch_readable_with_initial_pin};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    for origin in [
        super::super::adapter::FailureOrigin::Host,
        super::super::adapter::FailureOrigin::CaptureGuard,
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<body>fixture</body>"),
            )
            .mount(&server)
            .await;
        let context = ToolContext::new(".").with_state_namespace(format!("web-origin-{origin:?}"));
        let url = format!("http://public.example:{}/origin", server.address().port());
        let result: Result<super::super::fetch::ReadableFetch<()>, ToolError> =
            fetch_readable_with_initial_pin(
                &url,
                &FetchOptions::new(std::time::Duration::from_secs(5), 1024, "text/html"),
                &context,
                "web-origin",
                Some((
                    "public.example".into(),
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                )),
                move |_| {
                    Box::pin(async move {
                        Err(AdapterFailure {
                            origin,
                            error: ToolError::execution_failed(JS_SHELL_MARKER),
                        })
                    })
                },
            )
            .await;
        assert!(result.unwrap_err().to_string().contains(JS_SHELL_MARKER));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
