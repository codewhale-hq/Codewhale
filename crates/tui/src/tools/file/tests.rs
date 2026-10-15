use super::*;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn missing_pdf_path_precedes_unavailable_helper() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let input = temporary.path().join("missing.pdf");
    let missing = temporary.path().join("definitely-not-pdftotext");
    let error = read_pdf_if_detected(
        &input,
        None,
        super::super::pdf::PdfTextCommand::test(missing.as_os_str(), Duration::from_secs(1), None),
    )
    .await
    .expect_err("missing path must fail before the missing helper is launched");

    match error {
        ToolError::ExecutionFailed { message, .. } => {
            assert!(message.contains("Failed to read"), "{message}");
            assert!(message.contains("missing.pdf"), "{message}");
        }
        other => panic!("expected ordinary read failure, got {other:?}"),
    }
}

#[tokio::test]
async fn read_file_missing_pdftotext_is_a_failed_typed_outcome() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let missing = temporary.path().join("definitely-not-pdftotext");
    let input = temporary.path().join("input.pdf");
    std::fs::write(&input, b"%PDF-1.7\n%%EOF").expect("fixture");

    let error = read_pdf_with_command(
        &input,
        None,
        super::super::pdf::PdfTextCommand::test(missing.as_os_str(), Duration::from_secs(1), None),
    )
    .await
    .expect_err("missing helper must fail the tool call");
    let payload = match &error {
        ToolError::NotAvailable { message } => {
            serde_json::from_str::<Value>(message).expect("structured unavailable payload")
        }
        other => panic!("unexpected error: {other:?}"),
    };
    assert_eq!(payload["type"], "binary_unavailable");
    assert_eq!(
        crate::tools::spec::ToolExecutionOutcome::from_legacy(Err(error)).status,
        crate::tools::spec::ToolTerminalStatus::Failed
    );
}

/// C05 regression: the reader used to stop at a fixed 2 000 lines even when
/// the byte budget had barely been touched, fragmenting an ordinary file for
/// no reason. Bytes are now the only bound.
#[tokio::test]
async fn contract_read_returns_a_file_of_more_than_two_thousand_short_lines_whole() {
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();
    let content = (0..5_000)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(
        content.len() < READ_DEFAULT_MAX_BYTES,
        "fixture fits the budget"
    );

    let window = contract_read_window(&content, READ_DEFAULT_MAX_BYTES);
    assert!(!window.truncated);
    assert_eq!(window.shown_lines, 5_000);
    assert_eq!(window.content, content);

    let temporary = tempfile::tempdir().expect("tempdir");
    std::fs::write(temporary.path().join("many.txt"), &content).expect("fixture");
    let context = ToolContext::new(temporary.path());
    let result = ReadFileTool::execute_contract_read(json!({"path": "many.txt"}), &context)
        .await
        .expect("read result");
    assert!(
        result.content.starts_with(&content),
        "whole file first, pod footer after"
    );
    assert!(
        !result.content.contains("[Showing lines"),
        "no truncation footer"
    );
}

#[tokio::test]
async fn contract_read_appends_a_pod_footer_to_a_whole_source_file() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let content = "fn main() {\n    println!(\"hi\");\n}\n";
    std::fs::write(temporary.path().join("main.rs"), content).expect("fixture");
    let context = ToolContext::new(temporary.path());
    let result = ReadFileTool::execute_contract_read(json!({"path": "main.rs"}), &context)
        .await
        .expect("read result");
    assert!(
        result.content.starts_with(content),
        "whole file first, pod footer after"
    );
    assert!(
        result.content.contains("[Pod ("),
        "pod footer present: {}",
        result.content
    );
    assert!(
        result.content.contains("[Sound: 1 symbol: fn main:1]"),
        "sounded symbol present: {}",
        result.content
    );
}

#[tokio::test]
async fn contract_read_miss_echoes_parent_mates_on_not_found() {
    let temporary = tempfile::tempdir().expect("tempdir");
    std::fs::write(temporary.path().join("alpha.rs"), "fn a() {}\n").expect("fixture");
    std::fs::write(temporary.path().join("beta.rs"), "fn b() {}\n").expect("fixture");
    let context = ToolContext::new(temporary.path());
    let error = ReadFileTool::execute_contract_read(json!({"path": "alpah.rs"}), &context)
        .await
        .expect_err("typo'd path must fail");
    match error {
        ToolError::ExecutionFailed { message, .. } => {
            assert!(message.contains("Failed to read"), "{message}");
            assert!(message.contains("[Miss echo: "), "{message}");
            assert!(message.contains("alpha.rs"), "{message}");
            assert!(message.contains("beta.rs"), "{message}");
        }
        other => panic!("expected ordinary read failure, got {other:?}"),
    }
}

#[tokio::test]
async fn contract_read_refused_probe_gets_no_miss_echo() {
    // Security gate: a denylisted path must fail WITHOUT the miss echo —
    // listing the parent would answer the refused probe. The denylist
    // check runs before the IO read, so the refusal wins whether or not
    // the file exists. Mirrors the home-dir fixture pattern in
    // file/tests/tools.rs (the guard snapshots HOME process-wide).
    let _env_lock = crate::test_support::lock_test_env();
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let temporary = tempfile::tempdir().expect("tempdir");
    let context = ToolContext::new(temporary.path());
    let probe = home.join(".ssh").join("id_ed25519_no_such_key");
    let error =
        ReadFileTool::execute_contract_read(json!({"path": probe.to_string_lossy()}), &context)
            .await
            .expect_err("denylisted path must fail");
    let message = error.to_string();
    assert!(message.contains("deny-list"), "{message}");
    assert!(
        !message.contains("[Miss echo: "),
        "refused probe must not echo: {message}"
    );
}

#[test]
fn contract_read_byte_limit_keeps_only_complete_utf8_lines() {
    let first = "é".repeat(30_000);
    let second = "z".repeat(60_000);
    let window = contract_read_window(&format!("{first}\n{second}\n"), READ_DEFAULT_MAX_BYTES);
    assert!(window.truncated);
    assert_eq!(window.shown_lines, 1);
    assert_eq!(window.content, first);
    assert!(std::str::from_utf8(window.content.as_bytes()).is_ok());
}

#[test]
fn read_budget_precedence_is_request_then_workshop_then_default() {
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();

    crate::tools::large_output_router::WorkshopConfig::install_active(None);
    assert_eq!(effective_read_max_bytes(None), READ_DEFAULT_MAX_BYTES);
    assert_eq!(effective_read_max_bytes(Some(250_000)), 250_000);
    // Above the model-requestable maximum clamps down instead of erroring.
    assert_eq!(
        effective_read_max_bytes(Some(READ_REQUEST_MAX_BYTES * 10)),
        READ_REQUEST_MAX_BYTES
    );
    // A request below the active baseline leaves the baseline in place.
    assert_eq!(effective_read_max_bytes(Some(10)), READ_DEFAULT_MAX_BYTES);

    crate::tools::large_output_router::WorkshopConfig::install_active(Some(
        &crate::tools::large_output_router::WorkshopConfig {
            read_result_max_bytes: Some(700_000),
            ..Default::default()
        },
    ));
    assert_eq!(effective_read_max_bytes(None), 700_000);
    assert_eq!(effective_read_max_bytes(Some(200_000)), 700_000);
    // The workshop override keeps the 2 MiB absolute ceiling.
    crate::tools::large_output_router::WorkshopConfig::install_active(Some(
        &crate::tools::large_output_router::WorkshopConfig {
            read_result_max_bytes: Some(READ_RESULT_ABSOLUTE_MAX_BYTES * 4),
            ..Default::default()
        },
    ));
    assert_eq!(
        effective_read_max_bytes(None),
        READ_RESULT_ABSOLUTE_MAX_BYTES
    );
    crate::tools::large_output_router::WorkshopConfig::install_active(None);
}

#[tokio::test]
async fn contract_read_max_bytes_raises_the_budget_for_one_call() {
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();
    let temporary = tempfile::tempdir().expect("tempdir");
    let line = "y".repeat(199);
    let content = std::iter::repeat_n(line.as_str(), 1_500)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.len() > READ_DEFAULT_MAX_BYTES);
    assert!(content.len() < READ_REQUEST_MAX_BYTES);
    std::fs::write(temporary.path().join("wide.txt"), &content).expect("fixture");
    let context = ToolContext::new(temporary.path());

    let default_budget = ReadFileTool::execute_contract_read(json!({"path": "wide.txt"}), &context)
        .await
        .expect("default budget read");
    assert!(
        default_budget.content.contains("100000-byte output budget"),
        "{}",
        default_budget.content
    );

    let raised = ReadFileTool::execute_contract_read(
        json!({"path": "wide.txt", "max_bytes": 400_000}),
        &context,
    )
    .await
    .expect("raised budget read");
    assert!(
        raised.content.starts_with(&content),
        "whole file first, pod footer after"
    );

    // Above the hard maximum clamps down; the file still fits, so it is whole.
    let clamped = ReadFileTool::execute_contract_read(
        json!({"path": "wide.txt", "max_bytes": 9_000_000}),
        &context,
    )
    .await
    .expect("clamped budget read");
    assert!(
        clamped.content.starts_with(&content),
        "whole file first, pod footer after"
    );
}

#[tokio::test]
async fn contract_read_paginates_an_oversized_file_with_an_honest_budget_footer() {
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();
    let temporary = tempfile::tempdir().expect("tempdir");
    // 999-byte lines: 100 of them plus their 99 separators are 99 999 bytes,
    // one under the 100 000-byte budget, so page one is exactly lines 1-100.
    let line = "z".repeat(999);
    let content = std::iter::repeat_n(line.as_str(), 2_000)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(temporary.path().join("big.txt"), &content).expect("fixture");
    let context = ToolContext::new(temporary.path());

    let first = ReadFileTool::execute_contract_read(json!({"path": "big.txt"}), &context)
        .await
        .expect("first page");
    assert!(
        first.content.contains(
            "[Showing lines 1-100 of 2000 (1.9MB total, 100000-byte output budget). Use offset=101 to continue, or max_bytes up to 500000 to read more per call.]"
        ),
        "paging footer exact: {}",
        first.content
    );
    let shown = first.content.split("\n\n[").next().expect("body");
    assert_eq!(shown.lines().count(), 100);

    // The named continuation offset is exact: page two starts on line 101.
    let second =
        ReadFileTool::execute_contract_read(json!({"path": "big.txt", "offset": 101}), &context)
            .await
            .expect("second page");
    assert!(
        second.content.starts_with(&line),
        "second page starts at the named offset"
    );
    assert!(
        second.content.contains("Use offset=201 to continue"),
        "{}",
        second.content
    );
}

/// #6283 AC1: a >10 MiB file read without paging params returns page one
/// plus the file's size, line count, and truncated flag — never the whole
/// file.
#[tokio::test]
async fn contract_read_reports_size_and_truncation_for_huge_files() {
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();
    let temporary = tempfile::tempdir().expect("tempdir");
    let line = "x".repeat(99);
    let content = std::iter::repeat_n(line.as_str(), 110_000)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        content.len() > 10 * 1024 * 1024,
        "fixture exceeds 10 MiB: {}",
        content.len()
    );
    std::fs::write(temporary.path().join("huge.bin.txt"), &content).expect("fixture");
    let context = ToolContext::new(temporary.path());

    let first = ReadFileTool::execute_contract_read(json!({"path": "huge.bin.txt"}), &context)
        .await
        .expect("first page");
    let metadata = first.metadata.clone().expect("paging metadata");
    assert_eq!(metadata["size"], content.len() as u64);
    assert_eq!(metadata["truncated"], true);
    assert_eq!(metadata["line_count"], 110_000);
    assert!(
        first.content.len() < content.len(),
        "page one must never be the whole file"
    );
    assert!(
        first.content.len() <= READ_DEFAULT_MAX_BYTES + 1_024,
        "page one stays within the default budget plus footer slack: {}",
        first.content.len()
    );
    assert!(
        first.content.contains("total") && first.content.contains("Use offset="),
        "footer names the size and the continuation: {}",
        first
            .content
            .rsplit_once("\n\n")
            .map(|(_, f)| f)
            .unwrap_or("")
    );
}

/// #6283 AC2: paging through a file keeps every response bounded and
/// terminates with an untruncated page whose union is the whole file.
#[tokio::test]
async fn contract_read_pages_stay_bounded_and_cover_the_whole_file() {
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();
    let temporary = tempfile::tempdir().expect("tempdir");
    let content = (0..3_000)
        .map(|index| format!("paged-line-{index:05}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(temporary.path().join("paged.txt"), &content).expect("fixture");
    let context = ToolContext::new(temporary.path());

    let mut seen: Vec<String> = Vec::new();
    let mut offset = 1usize;
    for page in 0..100 {
        let result = ReadFileTool::execute_contract_read(
            json!({"path": "paged.txt", "offset": offset, "limit": 500}),
            &context,
        )
        .await
        .expect("page read");
        let metadata = result.metadata.clone().expect("paging metadata");
        assert_eq!(metadata["size"], content.len() as u64);
        assert!(
            result.content.len() <= READ_DEFAULT_MAX_BYTES + 1_024,
            "page {page} bounded: {}",
            result.content.len()
        );
        let body = result
            .content
            .split("\n\n[")
            .next()
            .unwrap_or(&result.content);
        seen.extend(body.lines().map(str::to_string));
        let truncated = metadata["truncated"].as_bool().expect("truncated flag");
        if !truncated {
            break;
        }
        offset += 500;
        assert!(page < 99, "paging must terminate");
    }
    assert_eq!(seen.len(), 3_000);
    assert_eq!(seen.join("\n"), content);
}

/// #6283: ordinary whole reads carry the same paging metadata (with
/// truncated=false); the pod footer follows the whole file.
#[tokio::test]
async fn contract_read_metadata_for_ordinary_whole_read() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let content = "alpha\nbeta\ngamma\n";
    std::fs::write(temporary.path().join("small.txt"), content).expect("fixture");
    let context = ToolContext::new(temporary.path());

    let result = ReadFileTool::execute_contract_read(json!({"path": "small.txt"}), &context)
        .await
        .expect("read result");
    assert!(
        result.content.starts_with(content),
        "whole file first, pod footer after"
    );
    assert!(
        result.content.contains("[Pod ("),
        "pod footer present: {}",
        result.content
    );
    let metadata = result.metadata.clone().expect("paging metadata");
    assert_eq!(metadata["size"], content.len() as u64);
    assert_eq!(metadata["truncated"], false);
    assert_eq!(metadata["line_count"], 4);
}

/// #6283 AC3: grep-then-read flow — locate a marker with `grep_files`,
/// then read exactly that line range. (`grep_files` in the child surface
/// is pinned by `an_explicit_parent_tool_scope_is_enforced_by_the_child_registry`.)
#[tokio::test]
async fn grep_then_read_flow_targets_matched_lines() {
    use crate::tools::spec::ToolSpec;

    let temporary = tempfile::tempdir().expect("tempdir");
    let mut lines: Vec<String> = (0..200)
        .map(|index| format!("filler line {index}"))
        .collect();
    lines[150] = "the needle marker lives here".to_string();
    std::fs::write(temporary.path().join("haystack.txt"), lines.join("\n")).expect("fixture");
    let context = ToolContext::new(temporary.path());

    let grep = crate::tools::search::GrepFilesTool
        .execute(
            json!({"pattern": "needle marker", "path": ".", "context_lines": 1}),
            &context,
        )
        .await
        .expect("grep result");
    let payload: serde_json::Value =
        serde_json::from_str(&grep.content).expect("grep JSON envelope");
    assert_eq!(payload["total_matches"], 1);
    let matched = &payload["matches"][0];
    assert_eq!(matched["line_number"], 151);

    let read = ReadFileTool::execute_contract_read(
        json!({
            "path": matched["file"].as_str().expect("match file"),
            "offset": matched["line_number"].as_u64().expect("match line"),
            "limit": 1,
        }),
        &context,
    )
    .await
    .expect("targeted read");
    assert!(
        read.content.contains("the needle marker lives here"),
        "{}",
        read.content
    );
}

#[tokio::test]
async fn contract_read_reports_huge_first_line_with_exact_bash_fallback() {
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();
    let temporary = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temporary.path().join("huge.txt"),
        "x".repeat(READ_DEFAULT_MAX_BYTES + 1),
    )
    .expect("fixture");
    let context = ToolContext::new(temporary.path());
    let result = ReadFileTool::execute_contract_read(json!({"path": "huge.txt"}), &context)
        .await
        .expect("read result");
    assert!(
        result.content.starts_with(
            "[Line 1 is 97.7KB, exceeds the 100000-byte output budget for this call. Use bash: sed -n '1p' huge.txt | head -c 100000]"
        ),
        "exact bash fallback first, pod footer after: {}",
        result.content
    );
}

#[tokio::test]
async fn contract_read_offset_oob_and_limit_continuation_match_contract() {
    let temporary = tempfile::tempdir().expect("tempdir");
    std::fs::write(temporary.path().join("lines.txt"), "one\ntwo\nthree").expect("fixture");
    let context = ToolContext::new(temporary.path());

    let limited = ReadFileTool::execute_contract_read(
        json!({"path": "lines.txt", "offset": 2, "limit": 1}),
        &context,
    )
    .await
    .expect("limited read");
    assert!(
        limited
            .content
            .starts_with("two\n\n[1 more lines in file (13B total). Use offset=3 to continue.]"),
        "page plus paging footer first, pod footer after: {}",
        limited.content
    );

    let error =
        ReadFileTool::execute_contract_read(json!({"path": "lines.txt", "offset": 4}), &context)
            .await
            .expect_err("offset beyond EOF");
    assert_eq!(
        error.to_string(),
        "Failed to execute tool: Offset 4 is beyond end of file (3 lines total)"
    );
}

/// Regression: grok-4.7 serializes every JSON number as a float, so its
/// ranged reads arrive as `{"offset": 2.0, "limit": 1.0}`. Those used to
/// fail with "offset must be a non-negative integer" on every call.
#[tokio::test]
async fn contract_read_accepts_whole_number_floats_and_still_refuses_fractions() {
    let temporary = tempfile::tempdir().expect("tempdir");
    std::fs::write(temporary.path().join("lines.txt"), "one\ntwo\nthree").expect("fixture");
    let context = ToolContext::new(temporary.path());

    let ranged = ReadFileTool::execute_contract_read(
        json!({"path": "lines.txt", "offset": 2.0, "limit": 1.0, "max_bytes": 200.0}),
        &context,
    )
    .await
    .expect("float-typed ranged read");
    assert_eq!(
        ranged.content,
        "two\n\n[1 more lines in file (13B total). Use offset=3 to continue.]\n\n[Pod (1): lines.txt (this file)]"
    );

    for (key, bad) in [
        ("offset", json!(-1.0)),
        ("offset", json!(2.5)),
        ("limit", json!("2")),
        ("limit", json!([2])),
    ] {
        let error =
            ReadFileTool::execute_contract_read(json!({"path": "lines.txt", key: bad}), &context)
                .await
                .expect_err("non-integer must be refused");
        assert!(
            error
                .to_string()
                .contains(&format!("{key} must be a non-negative integer")),
            "{error}"
        );
    }
}

#[tokio::test]
async fn contract_read_uses_magic_not_extension_for_images() {
    let temporary = tempfile::tempdir().expect("tempdir");
    std::fs::write(temporary.path().join("plain.png"), "ordinary text").expect("text fixture");
    std::fs::write(
        temporary.path().join("renamed.data"),
        crate::image_attach::tests::PNG_1X1,
    )
    .expect("image fixture");
    std::fs::write(
        temporary.path().join("truncated.png"),
        [b"\x89PNG\r\n\x1a\n".as_slice(), b"\0\0\0\rIHDR".as_slice()].concat(),
    )
    .expect("truncated image fixture");
    let context = ToolContext::new(temporary.path());

    let text = ReadFileTool::execute_contract_read(json!({"path": "plain.png"}), &context)
        .await
        .expect("fake extension remains text");
    assert!(
        text.content.starts_with("ordinary text"),
        "text first, pod footer after: {}",
        text.content
    );
    let image = ReadFileTool::execute_contract_read(json!({"path": "renamed.data"}), &context)
        .await
        .expect("real image uses typed transport");
    assert_eq!(image.content_blocks.len(), 1);
    assert!(matches!(
        &image.content_blocks[0],
        codewhale_tools::ToolResultContentBlock::Image { mime_type, .. }
            if mime_type == "image/png"
    ));
    let truncated = ReadFileTool::execute_contract_read(json!({"path": "truncated.png"}), &context)
        .await
        .expect("invalid image retains an omission receipt");
    assert!(truncated.content_blocks.is_empty());
    assert!(truncated.content.contains("Image omitted"));
}

#[test]
fn contract_edit_preparation_accepts_string_and_legacy_recovery_forms() {
    let encoded = prepare_contract_edit_input(json!({
        "path": "doc.txt",
        "edits": "[{\"oldText\":\"a\",\"newText\":\"b\"}]"
    }))
    .expect("encoded edits");
    assert_eq!(encoded["edits"][0], json!({"oldText": "a", "newText": "b"}));

    let recovered = prepare_contract_edit_input(json!({
        "path": "doc.txt",
        "edits": {"malformed": true},
        "oldText": "a",
        "newText": "b"
    }))
    .expect("legacy recovery");
    assert_eq!(
        recovered["edits"],
        json!([{"oldText": "a", "newText": "b"}])
    );
    assert!(recovered.get("oldText").is_none());
    assert!(recovered.get("newText").is_none());
}

#[test]
fn contract_edit_fuzzy_normalization_preserves_untouched_lines() {
    let original = "untouched line  \nShe said “hello”—today.   \ntail  \n";
    let updated = apply_contract_edits(
        original,
        &[ContractEdit {
            index: 0,
            old_text: "She said \"hello\"-today.".to_string(),
            new_text: "She said hello.".to_string(),
        }],
        "doc.txt",
        false,
    )
    .expect("fuzzy edit");
    assert_eq!(updated, "untouched line  \nShe said hello.\ntail  \n");
}

#[tokio::test]
async fn contract_edit_preserves_bom_and_crlf_without_prior_read() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("doc.txt");
    std::fs::write(&path, "\u{FEFF}alpha\r\nbeta\r\n").expect("fixture");
    let context = ToolContext::new(temporary.path());
    let result = EditFileTool::execute_contract_edits(
        json!({
            "path": "doc.txt",
            "edits": [{"oldText": "alpha\nbeta", "newText": "one\ntwo"}]
        }),
        &context,
    )
    .await
    .expect("edit");
    assert_eq!(
        result.content,
        "Successfully replaced 1 block(s) in doc.txt."
    );
    assert_eq!(
        std::fs::read(&path).expect("updated"),
        "\u{FEFF}one\r\ntwo\r\n".as_bytes()
    );
    // The receipt describes the bytes written (BOM and CRLF included), not
    // the edited text, so a turn artifact's revision matches the file read.
    let on_disk = std::fs::read(&path).expect("updated");
    assert_eq!(
        result.metadata.as_ref().expect("metadata")["mutation"]["files"],
        json!([{
            "path": "doc.txt",
            "outcome": "updated",
            "size": on_disk.len(),
            "sha256": crate::hashing::sha256_hex(&on_disk),
        }])
    );
}

#[tokio::test]
async fn contract_write_receipt_carries_written_size_and_sha256() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let context = ToolContext::new(temporary.path());
    let result = WriteFileTool::execute_contract_write(
        json!({"path": "notes/out.md", "content": "# Title\n"}),
        &context,
    )
    .await
    .expect("write");
    let on_disk = std::fs::read(temporary.path().join("notes/out.md")).expect("written");
    assert_eq!(
        result.metadata.as_ref().expect("metadata")["mutation"]["files"],
        json!([{
            "path": "notes/out.md",
            "outcome": "created",
            "size": on_disk.len(),
            "sha256": crate::hashing::sha256_hex(&on_disk),
        }])
    );
}

/// B6: bytes that are not UTF-8 survive an edit elsewhere in the file.
#[tokio::test]
async fn contract_edit_keeps_non_utf8_bytes() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("latin1.txt");
    let mut original = b"caf\xe9 \xff\xfe tail\n".to_vec();
    original.extend_from_slice(b"change me\n");
    std::fs::write(&path, &original).expect("fixture");
    let context = ToolContext::new(temporary.path());
    EditFileTool::execute_contract_edits(
        json!({"path": "latin1.txt", "edits": [{"oldText": "change me", "newText": "changed"}]}),
        &context,
    )
    .await
    .expect("edit");
    assert_eq!(
        std::fs::read(&path).expect("updated"),
        b"caf\xe9 \xff\xfe tail\nchanged\n".to_vec()
    );
}

/// A valid UTF-8 file may use the placeholder range itself (Nerd Font
/// Material Design icons are U+F0000..U+F00FF). Those characters are text,
/// not raw bytes, and an edit elsewhere must keep them.
#[tokio::test]
async fn contract_edit_keeps_placeholder_range_characters_in_utf8_files() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("starship.toml");
    let original = "icon = \"\u{F0026}\"\nwide = \"\u{F00A0}\"\ncolor = \"red\"\n";
    std::fs::write(&path, original).expect("fixture");
    let context = ToolContext::new(temporary.path());
    EditFileTool::execute_contract_edits(
        json!({"path": "starship.toml", "edits": [{"oldText": "red", "newText": "blue"}]}),
        &context,
    )
    .await
    .expect("edit");
    assert_eq!(
        std::fs::read_to_string(&path).expect("still UTF-8"),
        original.replace("red", "blue")
    );
}

/// B6: in a file with mixed line endings, only the lines an edit wrote take
/// the dominant ending; every untouched line keeps its own.
#[tokio::test]
async fn contract_edit_keeps_untouched_line_endings() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("mixed.txt");
    std::fs::write(&path, "one\r\ntwo\nthree\r\nfour\rfive\n").expect("fixture");
    let context = ToolContext::new(temporary.path());
    EditFileTool::execute_contract_edits(
        json!({"path": "mixed.txt", "edits": [{"oldText": "three", "newText": "THREE\nand more"}]}),
        &context,
    )
    .await
    .expect("edit");
    assert_eq!(
        std::fs::read_to_string(&path).expect("updated"),
        "one\r\ntwo\nTHREE\r\nand more\r\nfour\rfive\n"
    );
}

#[tokio::test]
async fn queued_parallel_contract_edits_preserve_both_changes() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("doc.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").expect("fixture");
    let context = ToolContext::new(temporary.path());
    let first_context = context.clone();
    let second_context = context.clone();

    let first = tokio::spawn(async move {
        EditFileTool::execute_contract_edits(
            json!({"path": "doc.txt", "edits": [{"oldText": "alpha", "newText": "A"}]}),
            &first_context,
        )
        .await
    });
    let second = tokio::spawn(async move {
        EditFileTool::execute_contract_edits(
            json!({"path": "doc.txt", "edits": [{"oldText": "gamma", "newText": "G"}]}),
            &second_context,
        )
        .await
    });
    first.await.expect("first task").expect("first edit");
    second.await.expect("second task").expect("second edit");
    assert_eq!(
        std::fs::read_to_string(path).expect("updated"),
        "A\nbeta\nG\n"
    );
}

#[tokio::test]
async fn contract_edit_echoes_touched_symbols_and_callers() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let context = ToolContext::new(temporary.path());
    std::fs::write(temporary.path().join("a.py"), "import b\nprint(b.swim())\n").expect("fixture");
    std::fs::write(temporary.path().join("b.py"), "def swim():\n    return 1\n").expect("fixture");
    let result = EditFileTool::execute_contract_edits(
        json!({
            "path": "b.py",
            "edits": [{"oldText": "return 1", "newText": "return 2"}]
        }),
        &context,
    )
    .await
    .expect("edit runs");
    assert!(
        result.content.contains("Successfully replaced"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("[Edit echo: touched def swim:1"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("heard by a.py"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn contract_write_receipt_counts_bytes_not_utf16_units() {
    // "héllo\n" is 7 bytes but 6 UTF-16 units; the receipt must say 7.
    let temporary = tempfile::tempdir().expect("tempdir");
    let context = ToolContext::new(temporary.path());
    let result = WriteFileTool::execute_contract_write(
        json!({"path": "note.txt", "content": "héllo\n"}),
        &context,
    )
    .await
    .expect("write runs");
    assert!(
        result.content.contains("Successfully wrote 7 bytes"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn cancelled_queued_pi_write_never_starts() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let context = ToolContext::new(temporary.path());
    let path = context.resolve_path("queued.txt").expect("resolved path");
    let held = file_mutation_lock(&path).expect("queue").lock_owned().await;
    let cancellation = CancellationToken::new();
    let queued_context = context.clone().with_cancel_token(cancellation.clone());
    let queued = tokio::spawn(async move {
        WriteFileTool::execute_contract_write(
            json!({"path": "queued.txt", "content": "must-not-land"}),
            &queued_context,
        )
        .await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();
    let error = queued
        .await
        .expect("queued task")
        .expect_err("queued write must cancel");
    assert!(matches!(error, ToolError::Cancelled { .. }));
    assert!(!path.exists());
    drop(held);
}

#[cfg(unix)]
#[tokio::test]
async fn contract_write_preserves_unreadable_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("write-only.txt");
    std::fs::write(&path, "original\n").expect("fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200))
        .expect("make unreadable");
    let context = ToolContext::new(temporary.path());
    let result = WriteFileTool::execute_contract_write(
        json!({"path": "write-only.txt", "content": "replacement\n"}),
        &context,
    )
    .await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("restore permissions");

    let error = result.expect_err("cannot overwrite without the prior contents");
    assert!(error.to_string().contains("Failed to read"), "{error}");
    assert_eq!(
        std::fs::read_to_string(path).expect("unchanged"),
        "original\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn compatibility_write_preserves_unreadable_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("write-only.txt");
    let context = ToolContext::new(temporary.path());
    let file_tool = crate::tools::file_tool::FileTool::new("File");
    for tool in [&WriteFileTool as &dyn ToolSpec, &file_tool] {
        std::fs::write(&path, "original\n").expect("fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200))
            .expect("make unreadable");
        let mut input = json!({"path": "write-only.txt", "content": "replacement\n"});
        if tool.name() == "File" {
            input["action"] = json!("write");
        }
        let result = tool.execute(input, &context).await;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restore permissions");

        let error = result.expect_err("cannot overwrite without the prior contents");
        assert!(error.to_string().contains("Failed to read"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged"),
            "original\n"
        );
    }
}

#[tokio::test]
async fn compatibility_write_preserves_non_utf8_existing_file() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("latin1.txt");
    let original = b"caf\xe9\n";
    std::fs::write(&path, original).expect("fixture");
    let context = ToolContext::new(temporary.path());
    let file_tool = crate::tools::file_tool::FileTool::new("File");
    for tool in [&WriteFileTool as &dyn ToolSpec, &file_tool] {
        let mut input = json!({"path": "latin1.txt", "content": "replacement\n"});
        if tool.name() == "File" {
            input["action"] = json!("write");
        }
        let error = tool
            .execute(input, &context)
            .await
            .expect_err("must decode original");
        assert!(error.to_string().contains("Failed to read"), "{error}");
        assert_eq!(std::fs::read(&path).expect("unchanged"), original);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn contract_edit_rejects_read_only_target_before_atomic_replace() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("readonly.txt");
    std::fs::write(&path, "alpha\n").expect("fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("readonly");
    let context = ToolContext::new(temporary.path());
    let result = EditFileTool::execute_contract_edits(
        json!({"path": "readonly.txt", "edits": [{"oldText": "alpha", "newText": "beta"}]}),
        &context,
    )
    .await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("restore permissions");
    let error = result.expect_err("read-only target must fail");
    assert!(error.to_string().contains("readable and writable"));
    assert_eq!(std::fs::read_to_string(path).expect("unchanged"), "alpha\n");
}

async fn run_contract_bounds_operation(
    operation: &str,
    path: &str,
    context: &ToolContext,
) -> Result<(), ToolError> {
    match operation {
        "read" => ReadFileTool::execute_contract_read(
            json!({"path": path, "offset": 1, "limit": 1}),
            context,
        )
        .await
        .map(|_| ()),
        "write" => WriteFileTool::execute_contract_write(
            json!({"path": path, "content": "replacement"}),
            context,
        )
        .await
        .map(|_| ()),
        "edit" => EditFileTool::execute_contract_edits(
            json!({"path": path, "edits": [{"oldText": "seed", "newText": "replacement"}]}),
            context,
        )
        .await
        .map(|_| ()),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn contract_source_cap_refuses_sparse_files_without_paging_or_mutation() {
    use std::io::{Read as _, Write as _};
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("large.txt");
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(b"seed").unwrap();
    file.set_len(CONTRACT_FILE_MAX_BYTES as u64 + 1).unwrap();
    drop(file);
    let context = ToolContext::new(temporary.path());
    for operation in ["read", "write", "edit"] {
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            run_contract_bounds_operation(operation, "large.txt", &context),
        )
        .await
        .expect("bounded refusal")
        .expect_err("source cap");
        let message = error.to_string();
        assert!(
            message.contains("16 MiB processing cap"),
            "{operation}: {message}"
        );
        assert!(
            !message.contains("offset=") && !message.contains("max_bytes"),
            "{message}"
        );
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            CONTRACT_FILE_MAX_BYTES as u64 + 1
        );
        let mut prefix = [0; 4];
        fs::File::open(&path)
            .unwrap()
            .read_exact(&mut prefix)
            .unwrap();
        assert_eq!(&prefix, b"seed");
        assert!(context.require_fresh_file_read(&path, "large.txt").is_err());
    }
}

#[test]
fn contract_source_actual_reads_stop_at_cap_plus_one_and_poll_cancellation() {
    use std::io::Read as _;
    let mut exact = std::io::repeat(b'x').take(CONTRACT_FILE_MAX_BYTES as u64);
    assert_eq!(
        read_contract_source(&mut exact, None).unwrap().len(),
        CONTRACT_FILE_MAX_BYTES
    );

    struct Reader {
        consumed: usize,
        cancel: Option<CancellationToken>,
    }
    impl std::io::Read for Reader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            bytes.fill(b'x');
            self.consumed += bytes.len();
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
            Ok(bytes.len())
        }
    }
    // No metadata shortcut: this represents a growing/virtual regular file.
    let mut growing = Reader {
        consumed: 0,
        cancel: None,
    };
    assert!(read_contract_source(&mut growing, None).is_err());
    assert_eq!(growing.consumed, CONTRACT_FILE_MAX_BYTES + 1);

    let token = CancellationToken::new();
    let mut interrupted = Reader {
        consumed: 0,
        cancel: Some(token.clone()),
    };
    assert!(matches!(
        read_contract_source(&mut interrupted, Some(&token)),
        Err(ToolError::Cancelled { .. })
    ));
    assert_eq!(interrupted.consumed, 64 * 1024);
    interrupted.consumed = 0;
    assert!(matches!(
        read_contract_source(&mut interrupted, Some(&token)),
        Err(ToolError::Cancelled { .. })
    ));
    assert_eq!(interrupted.consumed, 0);
}

#[tokio::test]
async fn contract_source_directory_and_cancelled_calls_leave_targets_untouched() {
    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir(temporary.path().join("directory")).unwrap();
    let path = temporary.path().join("plain.txt");
    fs::write(&path, b"seed").unwrap();
    let context = ToolContext::new(temporary.path());
    for operation in ["read", "write", "edit"] {
        assert!(
            run_contract_bounds_operation(operation, "directory", &context)
                .await
                .is_err()
        );
    }
    let token = CancellationToken::new();
    token.cancel();
    let cancelled = context.with_cancel_token(token);
    for operation in ["read", "write", "edit"] {
        let error = run_contract_bounds_operation(operation, "plain.txt", &cancelled)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ToolError::Cancelled { .. }),
            "{operation}: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"seed");
        assert!(
            cancelled
                .require_fresh_file_read(&path, "plain.txt")
                .is_err()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn contract_source_fifo_without_writer_and_hard_links_are_refused_promptly() {
    use std::os::unix::ffi::OsStrExt as _;
    let temporary = tempfile::tempdir().unwrap();
    let fifo = temporary.path().join("pipe");
    let cpath = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let plain = temporary.path().join("plain.txt");
    fs::write(&plain, b"seed").unwrap();
    fs::hard_link(&plain, temporary.path().join("linked.txt")).unwrap();
    let context = ToolContext::new(temporary.path());
    for path in ["pipe", "linked.txt"] {
        for operation in ["read", "write", "edit"] {
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                run_contract_bounds_operation(operation, path, &context),
            )
            .await
            .expect("must not block opening a FIFO")
            .expect_err("regular single-link target only");
            assert!(
                error.to_string().contains("regular"),
                "{operation}: {error}"
            );
        }
    }
    assert_eq!(fs::read(&plain).unwrap(), b"seed");
}

#[tokio::test]
async fn contract_read_newline_dense_ranges_preserve_trailing_and_empty_lines() {
    let temporary = tempfile::tempdir().unwrap();
    let text = "\n".repeat(2 * 1024 * 1024);
    fs::write(temporary.path().join("dense.txt"), &text).unwrap();
    let context = ToolContext::new(temporary.path());
    let result = ReadFileTool::execute_contract_read(
        json!({"path": "dense.txt", "offset": text.len(), "limit": 2}),
        &context,
    )
    .await
    .unwrap();
    assert_eq!(result.content, "\n\n\n[Pod (1): dense.txt (this file)]");
    let metadata = result.metadata.as_ref().unwrap();
    assert_eq!(metadata["size"], text.len());
    assert_eq!(metadata["line_count"], text.len() + 1);
    assert_eq!(metadata["truncated"], false);
    let empty = ReadFileTool::execute_contract_read(
        json!({"path": "dense.txt", "offset": text.len() + 1, "limit": 0}),
        &context,
    )
    .await
    .unwrap();
    assert_eq!(
        empty.content,
        "\n\n[1 more lines in file (2.0MB total). Use offset=2097153 to continue.]\n\n[Pod (1): dense.txt (this file)]"
    );
}

#[tokio::test]
async fn contract_write_and_edit_reject_payload_or_line_ending_growth_before_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plain.txt");
    let context = ToolContext::new(temporary.path());
    fs::write(&path, b"seed\r\n").unwrap();
    let error = WriteFileTool::execute_contract_write(
        json!({"path": "plain.txt", "content": "x".repeat(CONTRACT_FILE_MAX_BYTES + 1)}),
        &context,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("16 MiB processing cap"));
    for operation in ["write", "edit"] {
        let newlines = "\n".repeat(CONTRACT_FILE_MAX_BYTES / 2 + 1);
        let result = if operation == "write" {
            WriteFileTool::execute_contract_write(
                json!({"path": "plain.txt", "content": newlines}),
                &context,
            )
            .await
        } else {
            EditFileTool::execute_contract_edits(
                json!({"path": "plain.txt", "edits": [{"oldText": "seed", "newText": newlines}]}),
                &context,
            )
            .await
        };
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("16 MiB processing cap")
        );
        assert_eq!(fs::read(&path).unwrap(), b"seed\r\n");
        assert!(context.require_fresh_file_read(&path, "plain.txt").is_err());
    }
}

#[tokio::test]
async fn contract_edit_rejects_oversized_intermediate_replacement_even_if_later_edit_shrinks() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plain.txt");
    fs::write(&path, b"ab").unwrap();
    let context = ToolContext::new(temporary.path());
    let error = EditFileTool::execute_contract_edits(
        json!({"path": "plain.txt", "edits": [
            {"oldText": "a", "newText": ""},
            {"oldText": "b", "newText": "x".repeat(CONTRACT_FILE_MAX_BYTES)}
        ]}),
        &context,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("16 MiB processing cap"));
    assert_eq!(fs::read(&path).unwrap(), b"ab");
}

#[tokio::test]
async fn contract_read_admits_only_an_image_the_user_attached_from_outside_the_workspace() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let shot = outside.path().join("Screenshot 2026-10-04 at 22.25.47.png");
    std::fs::write(&shot, crate::image_attach::tests::PNG_1X1).expect("fixture");
    let stray = outside.path().join("stray.png");
    std::fs::write(&stray, crate::image_attach::tests::PNG_1X1).expect("fixture");
    let prompt = codewhale_models::Message {
        role: codewhale_models::Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: format!("what is this?\n[Attached image: {}]", shot.display()),
            cache_control: None,
        }],
    };
    let context = ToolContext::new(workspace.path()).with_session_objects(
        crate::rlm::session::SessionObjectSnapshot::new(
            "session".to_string(),
            "model".to_string(),
            workspace.path().to_path_buf(),
            None,
            vec![prompt],
        ),
    );

    let image =
        ReadFileTool::execute_contract_read(json!({"path": shot.display().to_string()}), &context)
            .await
            .expect("the attached screenshot is readable");
    assert_eq!(image.content_blocks.len(), 1);

    let error =
        ReadFileTool::execute_contract_read(json!({"path": stray.display().to_string()}), &context)
            .await
            .expect_err("an outside image the user never attached stays refused");
    assert!(matches!(error, ToolError::PathEscape { .. }), "{error:?}");
}

#[tokio::test]
async fn contract_read_enters_terminal_buzz_on_third_pod_visit() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = temporary.path();
    std::fs::create_dir_all(root.join("src/net")).expect("mkdir");
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("fixture");
    std::fs::write(root.join("src/net/mod.rs"), "pub mod server;\n").expect("fixture");
    std::fs::write(root.join("src/net/server.rs"), "fn serve() {}\n").expect("fixture");
    std::fs::write(root.join("src/net/client.rs"), "fn connect() {}\n").expect("fixture");
    let context = ToolContext::new(root);

    // First two pod reads: full footer, no callers yet.
    for path in ["src/net/mod.rs", "src/net/client.rs"] {
        let result = ReadFileTool::execute_contract_read(json!({"path": path}), &context)
            .await
            .expect("read result");
        assert!(!result.content.contains("heard by"), "{path}");
    }
    // Third distinct pod read: the buzz adds who links here.
    let result =
        ReadFileTool::execute_contract_read(json!({"path": "src/net/server.rs"}), &context)
            .await
            .expect("read result");
    assert!(
        result.content.contains("heard by: mod.rs"),
        "{}",
        result.content
    );
    // Another pod stays sparse: the buzz is per-pod, not global.
    std::fs::write(root.join("src/other.rs"), "fn other() {}\n").expect("fixture");
    let result = ReadFileTool::execute_contract_read(json!({"path": "src/other.rs"}), &context)
        .await
        .expect("read result");
    assert!(!result.content.contains("heard by"), "{}", result.content);
}
