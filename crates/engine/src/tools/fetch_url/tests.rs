use super::*;
use crate::tools::spec::ToolContext;
use std::path::PathBuf;

struct ArtifactRootRestore(Option<PathBuf>);

impl Drop for ArtifactRootRestore {
    fn drop(&mut self) {
        codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

#[test]
fn raw_pdf_production_path_preserves_exact_bytes_without_extractor() {
    let _lock = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temporary = tempfile::tempdir().expect("artifact root");
    let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
        temporary.path().join("sessions"),
    ));
    let _restore = ArtifactRootRestore(prior);
    let bytes = b"%PDF-1.7\nraw fixture that is intentionally not parseable\n%%EOF";
    let missing = temporary.path().join("definitely-not-pdftotext");
    let document = runtime()
        .block_on(extract_fetched_document(
            Format::Raw,
            "https://example.com/raw.pdf",
            "application/pdf",
            bytes,
            false,
            true,
            None,
            PdfTextCommand::test(missing.as_os_str(), Duration::from_millis(50), None),
        ))
        .expect("signed raw PDF must bypass the missing extractor");
    let (content, artifact) = render_extracted(
        "https://example.com/raw.pdf",
        "application/pdf",
        Format::Raw,
        document,
        bytes,
        &ToolContext::new("."),
    )
    .expect("raw PDF preservation must not require pdftotext");
    let artifact = artifact.expect("raw PDF artifact");
    assert!(content.contains("PDF response saved"), "{content}");
    assert_eq!(std::fs::read(artifact.absolute_path).unwrap(), bytes);
}

#[test]
fn raw_pdf_spoofs_and_contradictory_media_mime_create_no_artifact() {
    let _lock = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temporary = tempfile::tempdir().expect("artifact root");
    let sessions = temporary.path().join("sessions");
    let prior =
        codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(sessions.clone()));
    let _restore = ArtifactRootRestore(prior);
    let missing = temporary.path().join("definitely-not-pdftotext");
    let request = PdfTextCommand::test(missing.as_os_str(), Duration::from_millis(50), None);
    let runtime = runtime();

    for (url, content_type, bytes, expected) in [
        (
            "https://example.com/download",
            "application/pdf",
            b"plain text pretending to be a PDF".as_slice(),
            "PDF signature",
        ),
        (
            "https://example.com/spoof.pdf",
            "text/plain",
            b"plain text pretending to be a PDF".as_slice(),
            "PDF signature",
        ),
        (
            "https://example.com/signed",
            "image/png",
            b"%PDF-1.7\n%%EOF".as_slice(),
            "did not match its PDF bytes",
        ),
    ] {
        let error = runtime
            .block_on(extract_fetched_document(
                Format::Raw,
                url,
                content_type,
                bytes,
                false,
                true,
                None,
                request,
            ))
            .expect_err("invalid PDF response must fail before raw preservation");
        assert!(error.to_string().contains(expected), "{error}");
    }
    assert!(
        !sessions.exists(),
        "rejected bytes must not create artifacts"
    );
}

#[tokio::test]
async fn fetched_pdf_missing_helper_is_a_failed_typed_outcome() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let missing = temporary.path().join("definitely-not-pdftotext");
    let error = extract_fetched_document(
        Format::Text,
        "https://example.com/document.pdf",
        "application/pdf",
        b"%PDF-1.7\n%%EOF",
        false,
        true,
        None,
        PdfTextCommand::test(missing.as_os_str(), Duration::from_secs(1), None),
    )
    .await
    .expect_err("missing helper must fail the fetched PDF call");
    let payload = match &error.error {
        ToolError::NotAvailable { message } => {
            serde_json::from_str::<Value>(message).expect("structured unavailable payload")
        }
        other => panic!("unexpected error: {other:?}"),
    };
    assert_eq!(payload["type"], "binary_unavailable");
    assert_eq!(
        crate::tools::spec::ToolExecutionOutcome::from_legacy(Err(error.into())).status,
        crate::tools::spec::ToolTerminalStatus::Failed
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn fetched_pdf_host_text_matches_default_and_raw_bypasses_host() {
    use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
    use codewhale_runtime::features::{Feature, Features};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    let _home = crate::test_support::SealedHome::new();
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let Some(node) = crate::extension_host::test_support::node_for_tests("fetched_pdf_host") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("fake-pdftotext");
    std::fs::write(&binary, "#!/bin/sh\nprintf 'page one\\fpage two\\n'\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(root.path().join("host")),
        ..ExtensionHostOptions::default()
    }));
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let rust = ToolContext::new(root.path());
    let mut flags = Features::with_defaults();
    flags.enable(Feature::PdfHost);
    let host = ToolContext::new(root.path()).with_features(flags);
    let bytes = b"%PDF-1.7\nfixture\n%%EOF";
    for format in [Format::Text, Format::Markdown] {
        let expected = extract_fetched_document(
            format,
            "https://example.com/fixture.pdf",
            "application/pdf",
            bytes,
            false,
            true,
            None,
            PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), None)
                .with_context(&rust),
        )
        .await
        .unwrap();
        let actual = extract_fetched_document(
            format,
            "https://example.com/fixture.pdf",
            "application/pdf",
            bytes,
            false,
            true,
            None,
            PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), None)
                .with_context(&host),
        )
        .await
        .unwrap();
        assert_eq!(actual.text, expected.text);
        assert_eq!(actual.markdown, expected.markdown);
        assert_eq!(actual.pdf_pages, expected.pdf_pages);
    }
    manager.shutdown().await;
    drop(_manager);
    // A signed raw response deliberately has no extraction consumer, even with
    // the Host flag selected and an unusable runtime/parser.
    let unavailable = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(root.path().join("missing-node")),
        root: Some(root.path().join("absent-host")),
        ..ExtensionHostOptions::default()
    }));
    let _manager = TestManagerGuard::install(unavailable);
    let missing = root.path().join("missing-parser");
    let document = extract_fetched_document(
        Format::Raw,
        "https://example.com/fixture.pdf",
        "application/pdf",
        bytes,
        false,
        true,
        None,
        PdfTextCommand::test(missing.as_os_str(), Duration::from_secs(1), None).with_context(&host),
    )
    .await
    .unwrap();
    assert_eq!(document.kind, DocumentKind::Pdf);
    assert!(document.text.is_empty());
    assert!(document.pdf_pages.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn raw_and_non_success_catches_do_not_hide_a_selected_host_failure() {
    let _home = crate::test_support::SealedHome::new();
    let root = tempfile::tempdir().unwrap();
    let manager = std::sync::Arc::new(crate::extension_host::ExtensionHostManager::new(
        crate::extension_host::ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(root.path().join("missing-node")),
            root: Some(root.path().join("host")),
            ..crate::extension_host::ExtensionHostOptions::default()
        },
    ));
    let _manager = crate::extension_host::TestManagerGuard::install(manager);
    let mut flags = codewhale_runtime::features::Features::with_defaults();
    flags.enable(codewhale_runtime::features::Feature::WebExtractHost);
    let context = ToolContext::new(root.path()).with_features(flags);
    for (format, success) in [(Format::Raw, true), (Format::Text, false)] {
        let error=extract_fetched_document(format,"https://example.com/private", "text/html",b"<body><main>Five meaningful words survive this valid complete document content.</main></body>",false,success,None,PdfTextCommand::system(Some(&context))).await.unwrap_err();
        assert!(
            !error.content(),
            "operational Host failure cannot become raw decoded text"
        );
    }
}
