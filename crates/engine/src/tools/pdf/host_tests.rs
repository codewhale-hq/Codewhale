//! Real pinned Host plus local fake parser. These are author acceptance cases;
//! the parent must regenerate the canonical bundle/pin and run them after apply.
use super::*;
use crate::extension_host::test_support::node_for_tests;
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use crate::tools::spec::{ToolContext, ToolResult, ToolSpec};
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_runtime::features::{Feature, Features};
use std::sync::Arc;

fn context(root: &Path, host: bool) -> ToolContext {
    let mut flags = Features::with_defaults();
    if host {
        flags.enable(Feature::PdfHost);
    }
    ToolContext::new(root).with_features(flags)
}
fn new_manager(node: PathBuf, root: &Path) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(root.join("host")),
        ..ExtensionHostOptions::default()
    }))
}
#[cfg(any(test, feature = "test-support"))]
#[cfg(unix)]
fn parser(root: &Path, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("pdftotext");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn decision(code: &str, message: Option<&str>) -> ToolResult {
    ToolResult {
        content: String::new(),
        success: true,
        metadata: Some(json!({"kind":"pdf_decision","code":code,"message":message})),
    }
}

#[test]
fn pdf_host_is_independent_and_defaults_to_rust() {
    let mut flags = Features::with_defaults();
    assert!(!flags.enabled(Feature::PdfHost));
    assert_eq!(
        codewhale_runtime::features::feature_from_key("pdf_host"),
        Some(Feature::PdfHost)
    );
    flags.enable(Feature::PdfHost);
    assert!(!flags.enabled(Feature::ExtensionHost));
    assert!(!flags.enabled(Feature::SpeechHost));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn real_host_pdf_matches_default_stdout_page_arguments_and_faults() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_pdf_parity") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let _secret =
        crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "test-only-parent-secret");
    let input = root.path().join("input with spaces.pdf");
    std::fs::write(&input, b"%PDF-1.7\nfixture\n%%EOF").unwrap();
    for (index, script) in [
        "printf 'args:%s\\n' \"$*\"; printf 'page one\\fpage two\\n'",
        "printf 'bad\\033[31m\\000\\nnext' >&2; exit 2",
        "exit 3",
        "[ -z \"${DEEPSEEK_API_KEY+x}\" ] || exit 9; printf clean",
    ]
    .into_iter()
    .enumerate()
    {
        let binary = parser(root.path(), script);
        let rust = context(root.path(), false);
        let host = context(root.path(), true);
        let expected = extract_path(
            &input,
            Some((2, 4)),
            PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), None)
                .with_context(&rust),
        )
        .await;
        let actual = extract_path(
            &input,
            Some((2, 4)),
            PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), None)
                .with_context(&host),
        )
        .await;
        assert_eq!(actual, expected);
        if let Ok(text) = actual
            && index == 0
        {
            assert!(text.contains("-layout -f 2 -l 4"));
            assert!(text.contains(input.to_string_lossy().as_ref()));
        }
    }
    let missing = root.path().join("missing-parser");
    assert_eq!(
        extract_path(
            &input,
            None,
            PdfTextCommand::test(missing.as_os_str(), Duration::from_secs(10), None)
                .with_context(&context(root.path(), true))
        )
        .await,
        Err(PdfTextError::BinaryUnavailable)
    );
    assert!(!codewhale_plugins::activation::extension_host_policy_enabled());
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn real_host_read_file_and_web_document_use_the_shared_pdf_job() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_pdf_consumers") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    parser(root.path(), "printf 'page one\\fpage two\\n'");
    let search = std::env::join_paths(std::iter::once(root.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let _path = crate::test_support::EnvVarGuard::set("PATH", search);
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let bytes = b"%PDF-1.7\nfixture\n%%EOF";
    std::fs::write(root.path().join("input.pdf"), bytes).unwrap();
    let rust = context(root.path(), false);
    let host = context(root.path(), true);
    let tool = crate::tools::file::ReadFileTool;
    let input = json!({"path":"input.pdf","pages":"1-2"});
    let expected = tool.execute(input.clone(), &rust).await.unwrap();
    let actual = tool.execute(input, &host).await.unwrap();
    assert_eq!(actual.content, expected.content);
    assert_eq!(actual.metadata, expected.metadata);
    assert_eq!(actual.success, expected.success);
    let expected = crate::tools::web::extract::extract_document(
        "https://example.com/input.pdf",
        Some("application/pdf"),
        bytes,
        false,
        Some(&rust),
    )
    .await
    .unwrap();
    let actual = crate::tools::web::extract::extract_document(
        "https://example.com/input.pdf",
        Some("application/pdf"),
        bytes,
        false,
        Some(&host),
    )
    .await
    .unwrap();
    assert_eq!(actual.text, expected.text);
    assert_eq!(actual.markdown, expected.markdown);
    assert_eq!(actual.pdf_pages, expected.pdf_pages);
    let invalid = tool
        .execute(json!({"path":"missing.pdf","pages":"bad"}), &host)
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("Failed to read"));
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn pdf_private_output_guards_cannot_be_overridden_by_host_decisions() {
    use std::os::unix::process::ExitStatusExt;
    let output = |overflow, success| PdfProcessOutcome {
        output: Ok((
            BoundedOutput {
                bytes: b"private extracted document".to_vec(),
                truncated: overflow,
            },
            BoundedOutput {
                bytes: vec![],
                truncated: false,
            },
            std::process::ExitStatus::from_raw(if success { 0 } else { 2 << 8 }),
        )),
    };
    assert!(
        output(true, true)
            .into_host_text(decision("success", None))
            .is_err()
    );
    assert!(
        output(false, false)
            .into_host_text(decision("success", None))
            .is_err()
    );
    assert!(
        output(false, true)
            .into_host_text(decision("execution", Some("fabricated error")))
            .is_err()
    );
    assert!(
        PdfProcessOutcome {
            output: Err(PdfTextError::Cancelled)
        }
        .into_host_text(decision("success", None))
        .is_err()
    );
    assert_eq!(
        PdfProcessOutcome {
            output: Err(PdfTextError::Cancelled)
        }
        .into_host_text(decision("cancelled", Some("ignored"))),
        Err(PdfTextError::Cancelled)
    );
    let mut result = decision("success", None);
    result.metadata.as_mut().unwrap()["extra"] = json!(true);
    assert!(output(false, true).into_host_text(result).is_err());
    let full = PdfProcessOutcome {
        output: Ok((
            BoundedOutput {
                bytes: vec![b'x'; MAX_PDF_STDOUT_BYTES],
                truncated: false,
            },
            BoundedOutput {
                bytes: vec![],
                truncated: false,
            },
            std::process::ExitStatus::from_raw(0),
        )),
    };
    let wire = serde_json::to_vec(&full.projection()).unwrap();
    assert!(wire.len() < 512);
    assert!(!String::from_utf8(wire).unwrap().contains("xxxxx"));
    assert_eq!(
        full.into_host_text(decision("success", None))
            .unwrap()
            .len(),
        MAX_PDF_STDOUT_BYTES
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn real_host_pdf_cancel_timeout_and_host_refusal_never_replay() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_pdf_cancel") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("launched");
    let binary = parser(
        root.path(),
        &format!("printf x >> '{}'; exec /bin/sleep 30", marker.display()),
    );
    let input = root.path().join("input.pdf");
    std::fs::write(&input, b"%PDF-1.7\n%%EOF").unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    // Warm only the already admitted bounded Host; timeout is measured after startup.
    let fast = parser(root.path(), "printf warm");
    extract_path(
        &input,
        None,
        PdfTextCommand::test(fast.as_os_str(), Duration::from_secs(10), None)
            .with_context(&context(root.path(), true)),
    )
    .await
    .unwrap();
    parser(
        root.path(),
        &format!("printf x >> '{}'; exec /bin/sleep 30", marker.display()),
    );
    let cancel = CancellationToken::new();
    let ctx = context(root.path(), true).with_cancel_token(cancel.clone());
    let stop = cancel.clone();
    let marker_clone = marker.clone();
    tokio::spawn(async move {
        for _ in 0..1000 {
            if marker_clone.exists() {
                stop.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("parser never launched");
    });
    let started = std::time::Instant::now();
    assert_eq!(
        extract_path(
            &input,
            None,
            PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), None)
                .with_context(&ctx)
        )
        .await,
        Err(PdfTextError::Cancelled)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(std::fs::read(&marker).unwrap(), b"x");
    let error = extract_path(
        &input,
        None,
        PdfTextCommand::test(binary.as_os_str(), Duration::from_millis(100), None)
            .with_context(&context(root.path(), true)),
    )
    .await
    .unwrap_err();
    assert_eq!(error, PdfTextError::TimedOut);
    assert_eq!(std::fs::read(&marker).unwrap(), b"xx");
    manager.shutdown().await;
    drop(_manager);
    let unavailable = new_manager(root.path().join("no-runtime"), root.path());
    let _manager = TestManagerGuard::install(unavailable);
    let error = extract_path(
        &input,
        None,
        PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(2), None)
            .with_context(&context(root.path(), true)),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("no Rust fallback"));
    assert_eq!(std::fs::read(&marker).unwrap(), b"xx");
}

#[cfg(unix)]
#[tokio::test]
async fn pdf_driver_cancellation_kills_descendants_and_keeps_staged_input_alive() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("child-alive");
    let binary = parser(
        root.path(),
        &format!(
            "(/bin/sleep 1; printf alive > '{}') &\n/bin/sleep 30",
            marker.display()
        ),
    );
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.cancel();
    });
    assert_eq!(
        extract_bytes(
            b"%PDF-1.7\n%%EOF",
            PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), Some(&cancel))
        )
        .await,
        Err(PdfTextError::Cancelled)
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        !marker.exists(),
        "parser descendant must be terminated with its parent"
    );
    parser(
        root.path(),
        "/bin/sleep 0.1; cat \"$2\"; printf '\\fpage two\\n'",
    );
    let text = extract_bytes(
        b"%PDF-1.7\nstaged stays present",
        PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), None),
    )
    .await
    .unwrap();
    assert!(text.starts_with("%PDF-1.7\nstaged stays present"));
}

#[tokio::test]
async fn pdf_core_terminal_status_precedes_a_generic_host_refusal() {
    let refusal =
        || ToolError::execution_failed("Builtin runner failed; no Rust fallback was attempted");
    let now = tokio::time::Instant::now();
    assert_eq!(
        host_terminal_error(refusal(), now, None),
        PdfTextError::TimedOut
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        host_terminal_error(refusal(), now, Some(&cancel)),
        PdfTextError::Cancelled
    );
    assert_eq!(
        host_terminal_error(
            ToolError::cancelled("broker cancelled"),
            now + Duration::from_secs(1),
            None
        ),
        PdfTextError::Cancelled
    );
    let error = host_terminal_error(refusal(), now + Duration::from_secs(1), None);
    assert!(matches!(error, PdfTextError::Execution(_)));
    assert!(error.to_string().contains("no Rust fallback"));
}
