use super::*;
use crate::tools::spec::{ToolExecutionOutcome, ToolTerminalStatus};

#[tokio::test]
async fn missing_binary_maps_to_bounded_failed_machine_contract() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let missing = temporary.path().join("definitely-not-pdftotext");
    let input = temporary.path().join("input.pdf");
    std::fs::write(&input, b"%PDF-1.7\n%%EOF").expect("fixture");
    let error = extract_path(
        &input,
        None,
        PdfTextCommand::test(missing.as_os_str(), Duration::from_secs(1), None),
    )
    .await
    .expect_err("missing binary");
    assert_eq!(error, PdfTextError::BinaryUnavailable);

    let error = into_tool_error(error);
    let payload = match &error {
        ToolError::NotAvailable { message } => {
            assert!(message.len() < 512, "{message}");
            serde_json::from_str::<serde_json::Value>(message).expect("JSON failure payload")
        }
        other => panic!("unexpected mapped error: {other:?}"),
    };
    assert_eq!(payload["type"], "binary_unavailable");
    assert_eq!(payload["binary"], "pdftotext");
    assert_eq!(
        ToolExecutionOutcome::from_legacy(Err(error)).status,
        ToolTerminalStatus::Failed
    );
}

#[cfg(unix)]
fn executable_script(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("tempdir");
    let binary = temporary.path().join("fake-pdftotext");
    std::fs::write(&binary, contents).expect("fake binary");
    let mut permissions = std::fs::metadata(&binary).expect("metadata").permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&binary, permissions).expect("executable");
    (temporary, binary)
}

#[cfg(unix)]
#[tokio::test]
async fn shared_adapter_forwards_page_window_and_returns_stdout() {
    let (temporary, binary) = executable_script(
        "#!/bin/sh\nprintf 'args:%s\\n' \"$*\"\nprintf 'page one\\fpage two\\n'\n",
    );
    // Success-path budget, not a tightness proof. Under a loaded cargo-test
    // process a 1s spawn of `#!/bin/sh` timed out as TimedOut (#5355). The
    // neighboring test still uses 50ms to prove the timeout path.
    let request = PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), None);
    let input = temporary.path().join("input with spaces.pdf");
    std::fs::write(&input, b"fixture bytes").expect("fixture");
    let text = extract_path(&input, Some((2, 4)), request)
        .await
        .expect("fake extraction");
    assert!(text.contains("-layout -f 2 -l 4"), "{text}");
    assert!(text.contains(input.to_string_lossy().as_ref()), "{text}");
    assert!(text.ends_with("page one\u{c}page two\n"), "{text:?}");

    let staged = extract_bytes(b"fetched fixture bytes", request)
        .await
        .expect("fake fetched extraction");
    assert!(staged.contains("-layout"), "{staged}");
    assert!(staged.ends_with("page one\u{c}page two\n"), "{staged:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn child_execution_is_timeout_and_cancellation_bounded() {
    let (temporary, binary) = executable_script("#!/bin/sh\nexec sleep 10\n");
    let input = temporary.path().join("input.pdf");
    std::fs::write(&input, b"fixture bytes").expect("fixture");
    let started = std::time::Instant::now();
    let error = extract_path(
        &input,
        None,
        PdfTextCommand::test(binary.as_os_str(), Duration::from_millis(50), None),
    )
    .await
    .expect_err("timeout");
    assert_eq!(error, PdfTextError::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(2));

    let cancel = CancellationToken::new();
    let cancel_after_spawn = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_after_spawn.cancel();
    });
    let started = std::time::Instant::now();
    let error = extract_path(
        &input,
        None,
        PdfTextCommand::test(binary.as_os_str(), Duration::from_secs(10), Some(&cancel)),
    )
    .await
    .expect_err("cancelled");
    assert_eq!(error, PdfTextError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn bounded_reader_drains_but_retains_only_the_prefix() {
    let output = read_bounded(&b"0123456789"[..], 4).await.expect("read");
    assert_eq!(output.bytes, b"0123");
    assert!(output.truncated);
}

#[test]
fn stderr_sanitizer_removes_terminal_control_bytes() {
    assert_eq!(sanitized_text(b"bad\x1b[31m\0\nnext"), "bad�[31m�\nnext");
}
