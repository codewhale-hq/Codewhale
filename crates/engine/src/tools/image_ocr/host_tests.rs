//! Real pinned harness/Execution broker and local fake parser. Native framework
//! is replaced only at its private test port; OS Vision acceptance remains separate.
use super::*;
use crate::extension_host::test_support::node_for_tests;
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_runtime::features::{Feature, Features};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

struct Overrides(Option<TestOverrides>);
impl Drop for Overrides {
    fn drop(&mut self) {
        TEST_OVERRIDES.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}
fn overrides(native: Result<Option<String>, ToolError>, binary: Option<OsString>) -> Overrides {
    let value = TestOverrides {
        native: Arc::new(move |_| native.clone()),
        tesseract: binary,
    };
    Overrides(TEST_OVERRIDES.with(|slot| slot.borrow_mut().replace(value)))
}
fn context(path: &Path, host: bool) -> ToolContext {
    let mut flags = Features::with_defaults();
    if host {
        flags.enable(Feature::OcrHost);
    }
    ToolContext::new(path).with_features(flags)
}
fn new_manager(node: PathBuf, path: &Path) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(path.join("host")),
        ..ExtensionHostOptions::default()
    }))
}
#[cfg(any(test, feature = "test-support"))]
fn binary(root: &Path, body: &str) -> PathBuf {
    let path = root.join("fake-tesseract");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn decision(code: &str, trim_end: bool, message: Option<&str>) -> ToolResult {
    ToolResult::success("").with_metadata(
        json!({"kind":"ocr_decision","code":code,"trim_end":trim_end,"message":message}),
    )
}
#[test]
fn ocr_host_is_independent_and_defaults_to_rust() {
    let mut flags = Features::with_defaults();
    assert!(!flags.enabled(Feature::OcrHost));
    assert_eq!(
        codewhale_runtime::features::feature_from_key("ocr_host"),
        Some(Feature::OcrHost)
    );
    flags.enable(Feature::OcrHost);
    assert!(!flags.enabled(Feature::ExtensionHost));
    assert!(!flags.enabled(Feature::PdfHost));
}
#[tokio::test(flavor = "current_thread")]
async fn real_host_ocr_and_read_file_preserve_native_first_and_tesseract_results() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_ocr_consumers") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let image = root.path().canonicalize().unwrap().join("image.png");
    std::fs::write(&image, b"private-image-fixture").unwrap();
    let marker = root.path().join("tesseract-launched");
    let tess = binary(
        root.path(),
        &format!(
            "printf x >> '{}'; printf '  page one\\npage two\\f\\n\\t\\302\\205'",
            marker.display()
        ),
    );
    let cases = [
        Ok(Some("  Native output stays exact \n".into())),
        Ok(None),
        Err(ToolError::execution_failed("Native framework refusal")),
    ];
    for native in cases {
        let native_ok = matches!(&native, Ok(Some(_)));
        let _override = overrides(native, Some(tess.clone().into_os_string()));
        for host in [false, true] {
            let ctx = context(root.path(), host);
            let result = ImageOcrTool
                .execute(json!({"path":"image.png"}), &ctx)
                .await
                .unwrap();
            let expected = if native_ok {
                "  Native output stays exact \n"
            } else {
                "  page one\npage two"
            };
            assert_eq!(result.content, expected);
            let read = crate::tools::file::ReadFileTool
                .execute(json!({"path":"image.png"}), &ctx)
                .await
                .unwrap();
            assert_eq!(
                read.content,
                format!("<image_ocr path=\"image.png\">\n{expected}\n</image_ocr>")
            );
        }
        if native_ok {
            assert!(
                !marker.exists(),
                "successful Native must not launch/probe fallback"
            );
        }
    }
    assert_eq!(std::fs::read(&marker).unwrap(), b"xxxxxxxx");
    assert!(!codewhale_plugins::activation::extension_host_policy_enabled());
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn real_host_ocr_no_backend_native_failure_and_parser_errors_match_default() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_ocr_faults") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let image = root.path().canonicalize().unwrap().join("image.png");
    std::fs::write(&image, b"fixture").unwrap();
    for native in [
        Ok(None),
        Err(ToolError::execution_failed("Native framework refusal")),
    ] {
        let _override = overrides(native, None);
        let rust = ocr_image_path(&image, &context(root.path(), false))
            .await
            .unwrap_err();
        let host = ocr_image_path(&image, &context(root.path(), true))
            .await
            .unwrap_err();
        assert_eq!(host.to_string(), rust.to_string());
    }
    for body in ["printf '  parser diagnostic  ' >&2; exit 2", "exit 3"] {
        let tess = binary(root.path(), body);
        let _override = overrides(Ok(None), Some(tess.into_os_string()));
        let rust = ocr_image_path(&image, &context(root.path(), false))
            .await
            .unwrap_err();
        let host = ocr_image_path(&image, &context(root.path(), true))
            .await
            .unwrap_err();
        assert_eq!(host.to_string(), rust.to_string());
    }
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn ocr_guards_run_before_host_start_and_host_refusal_has_no_fallback() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(root.path().join("missing-runtime"), root.path());
    let _manager = TestManagerGuard::install(manager);
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mark = Arc::clone(&ran);
    let value = TestOverrides {
        native: Arc::new(move |_| {
            mark.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some("must not fallback".into()))
        }),
        tesseract: None,
    };
    let _override = Overrides(TEST_OVERRIDES.with(|slot| slot.borrow_mut().replace(value)));
    let ctx = context(root.path(), true);
    let missing = ImageOcrTool
        .execute(json!({"path":"missing.png"}), &ctx)
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("does not exist"));
    std::fs::write(root.path().join(".env"), b"guarded").unwrap();
    assert!(matches!(
        ImageOcrTool
            .execute(json!({"path":".env"}), &ctx)
            .await
            .unwrap_err(),
        ToolError::PermissionDenied { .. }
    ));
    std::fs::write(
        root.path().canonicalize().unwrap().join("image.png"),
        b"fixture",
    )
    .unwrap();
    let refused = ImageOcrTool
        .execute(json!({"path":"image.png"}), &ctx)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("Builtin") || refused.to_string().contains("runtime"));
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
}
#[test]
fn ocr_private_text_and_terminal_guards_refuse_forged_host_decisions() {
    use std::os::unix::process::ExitStatusExt;
    let native = || OcrOutcome::Native(Ok(Some("private image text\n".into())));
    assert!(
        native()
            .into_host_text(decision("tesseract_success", true, None))
            .is_err()
    );
    assert_eq!(
        native()
            .into_host_text(decision("native_success", false, None))
            .unwrap(),
        "private image text\n"
    );
    let tess = |success| {
        OcrOutcome::Tesseract(Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 2 << 8 }),
            stdout: vec![b'x'; MAX_OCR_TEXT],
            stderr: b"private input path /private/customer/receipt.png\n".to_vec(),
        }))
    };
    let projection = tess(true).projection();
    assert!(serde_json::to_vec(&projection).unwrap().len() < 512);
    assert!(projection.get("stdout").is_none());
    assert!(projection.get("stderr").is_none());
    assert!(!projection.to_string().contains("receipt.png"));
    let expected =
        "tesseract failed (exit Some(2)): private input path /private/customer/receipt.png";
    assert_eq!(
        tess(false)
            .into_host_text(decision(
                "execution",
                false,
                Some("tesseract failed (exit Some(2)): ")
            ))
            .unwrap_err()
            .to_string(),
        ToolError::execution_failed(expected).to_string()
    );
    assert!(
        tess(false)
            .into_host_text(decision(
                "execution",
                false,
                Some("forged private diagnostic")
            ))
            .is_err()
    );
    assert_eq!(
        tess(true)
            .into_host_text(decision("tesseract_success", true, None))
            .unwrap()
            .len(),
        MAX_OCR_TEXT
    );
    assert!(
        tess(false)
            .into_host_text(decision("tesseract_success", true, None))
            .is_err()
    );
    assert!(
        OcrOutcome::Native(Ok(None))
            .into_host_text(decision("native_success", false, None))
            .is_err()
    );
    let cancel = OcrOutcome::Tesseract(Err(ToolError::cancelled("cancelled")))
        .into_host_text(decision("fault", false, None))
        .unwrap_err();
    assert!(matches!(cancel, ToolError::Cancelled { .. }));
}
#[tokio::test(flavor = "current_thread")]
async fn real_host_ocr_cancellation_bounds_parser_tree_and_never_replays() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_ocr_cancel") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let image = root.path().canonicalize().unwrap().join("image.png");
    std::fs::write(&image, b"fixture").unwrap();
    let warm = overrides(Ok(Some("warm".into())), None);
    ocr_image_path(&image, &context(root.path(), true))
        .await
        .unwrap();
    drop(warm);
    let marker = root.path().join("launched");
    let child = root.path().join("descendant-survived");
    let parser_pid = root.path().join("parser.pid");
    let descendant_pid = root.path().join("descendant.pid");
    let ready = root.path().join("descendant-ready");
    let release = root.path().join("release-descendant");
    let tess = binary(
        root.path(),
        &format!(
            concat!(
                "printf '%s\\n' \"$$\" > '{}'\n",
                "printf x >> '{}'\n",
                "(printf ready > '{}'; while [ ! -e '{}' ]; do /bin/sleep 0.02; done; printf alive > '{}') &\n",
                "printf '%s\\n' \"$!\" > '{}'\nwait",
            ),
            parser_pid.display(),
            marker.display(),
            ready.display(),
            release.display(),
            child.display(),
            descendant_pid.display(),
        ),
    );
    let _override = overrides(Ok(None), Some(tess.into_os_string()));
    let cancel = CancellationToken::new();
    // Backstop assertion/timeout failures without leaving a latched parser tree.
    struct ParserCleanup {
        cancel: CancellationToken,
        pid_file: PathBuf,
        armed: bool,
    }
    impl Drop for ParserCleanup {
        fn drop(&mut self) {
            self.cancel.cancel();
            if self.armed
                && let Some(pid) = std::fs::read_to_string(&self.pid_file)
                    .ok()
                    .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
                    .filter(|pid| *pid > 0)
            {
                // SAFETY: this private fixture records the contained parser's
                // process-group leader; a negative PID targets only that group.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
        }
    }
    let mut cleanup = ParserCleanup {
        cancel: cancel.clone(),
        pid_file: parser_pid.clone(),
        armed: true,
    };
    let stop = cancel.clone();
    let waiter = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() || !descendant_pid.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("OCR descendant never became ready");
        let pids = tokio::task::spawn_blocking(move || {
            (
                codewhale_runtime::process_tree::read_pid_file(&parser_pid, Duration::from_secs(5)),
                codewhale_runtime::process_tree::read_pid_file(
                    &descendant_pid,
                    Duration::from_secs(5),
                ),
            )
        })
        .await
        .unwrap();
        stop.cancel();
        pids
    });
    let ctx = context(root.path(), true).with_cancel_token(cancel);
    let err = tokio::time::timeout(Duration::from_secs(20), ocr_image_path(&image, &ctx))
        .await
        .expect("OCR cancellation did not return")
        .unwrap_err();
    assert!(matches!(err, ToolError::Cancelled { .. }));
    let (parser, descendant) = waiter.await.unwrap();
    // Host cancellation returns before its retained worker has cleaned up.
    // Observe that cleanup while allowing the current-thread runtime to run it.
    let stopped = tokio::task::spawn_blocking(move || {
        let descendant_stopped =
            codewhale_runtime::process_tree::wait_for_pid_exit(descendant, Duration::from_secs(5));
        let parser_stopped =
            codewhale_runtime::process_tree::wait_for_pid_exit(parser, Duration::from_secs(5));
        descendant_stopped && parser_stopped
    })
    .await
    .unwrap();
    if stopped {
        cleanup.armed = false;
    }
    assert!(stopped, "cancelled OCR parser tree is still running");
    // A delayed cancellation observer can no longer race a one-second writer:
    // the descendant could not write before this explicit release.
    std::fs::write(release, b"release").unwrap();
    assert_eq!(std::fs::read(marker).unwrap(), b"x");
    assert!(!child.exists());
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_host_ocr_retains_captured_bytes_after_original_path_replacement() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("ocr_captured_image") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let manager = new_manager(node, root.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let image = root.path().canonicalize().unwrap().join("receipt.png");
    let tess = binary(root.path(), "/bin/cat \"$1\"");
    for host in [false, true] {
        std::fs::write(&image, b"captured original image").unwrap();
        let original = image.clone();
        let observed = Arc::new(std::sync::Mutex::new(None));
        let record = Arc::clone(&observed);
        let value = TestOverrides {
            native: Arc::new(move |staged| {
                assert_ne!(staged, original.as_path());
                assert_eq!(std::fs::read(staged).unwrap(), b"captured original image");
                *record.lock().unwrap() = Some(staged.to_path_buf());
                std::fs::remove_file(&original).unwrap();
                std::fs::write(&original, b"replacement image must not be parsed").unwrap();
                Ok(None)
            }),
            tesseract: Some(tess.clone().into_os_string()),
        };
        let _override = Overrides(TEST_OVERRIDES.with(|slot| slot.borrow_mut().replace(value)));
        let result = ImageOcrTool
            .execute(json!({"path":"receipt.png"}), &context(root.path(), host))
            .await
            .unwrap();
        assert_eq!(result.content, "captured original image");
        assert_eq!(
            std::fs::read(&image).unwrap(),
            b"replacement image must not be parsed"
        );
        assert!(
            !observed.lock().unwrap().as_ref().unwrap().exists(),
            "captured temporary survives work and is retired afterwards"
        );
    }
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn ocr_capture_reuses_bounded_anchored_reader_and_content_digest() {
    use std::os::unix::fs::symlink;
    let _home = crate::test_support::SealedHome::new();
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let image = root_path.join("receipt.png");
    std::fs::write(&image, b"captured original image").unwrap();
    let ctx = context(&root_path, false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let first = CapturedOcr::capture(&image, &ctx, deadline).await.unwrap();
    let second = CapturedOcr::capture(&image, &ctx, deadline).await.unwrap();
    assert_ne!(first.path, second.path);
    assert_eq!(
        first.digest(),
        codewhale_runtime::hashing::sha256_hex(b"captured original image")
    );
    assert_eq!(first.digest(), second.digest());
    let staged = first.path.clone();
    drop(first);
    assert!(!staged.exists());
    let link = root_path.join("linked.png");
    symlink(&image, &link).unwrap();
    assert!(CapturedOcr::capture(&link, &ctx, deadline).await.is_err());
    let hard = root_path.join("hard.png");
    std::fs::hard_link(&image, &hard).unwrap();
    assert!(CapturedOcr::capture(&image, &ctx, deadline).await.is_err());
    std::fs::remove_file(hard).unwrap();
    let big = root_path.join("big.png");
    std::fs::File::create(&big)
        .unwrap()
        .set_len((16 * 1024 * 1024 + 1) as u64)
        .unwrap();
    assert!(
        CapturedOcr::capture(&big, &ctx, deadline)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("16 MiB")
    );
}
