//! Protocol-recovery contract tests.
//!
//! These tests exist to keep the engine hostile to fake tool-call wrappers
//! (XML/Replit/markdown pseudo-calls in assistant text). Their job is to make
//! sure that:
//!
//! 1. The known wrapper markers are still present in `core/engine.rs` so the
//!    streaming filter has something to scrub.
//! 2. The legacy text-based `tool_parser` flags fake wrappers for
//!    stripping/status bookkeeping but does NOT treat the newer
//!    `<function_calls>` wrapper as a real tool call — only the legacy
//!    `[TOOL_CALL]` and `<invoke>` shapes ever produced structured calls, and
//!    nothing should silently re-enable text-based execution.
//! 3. The closing-marker list stays the same length as the start-marker list,
//!    so filter logic cannot get stuck in tool-call mode forever.
//!
//! The point is that protocol drift in the model output should be visible (we
//! still strip it and emit a status notice), not silently turned into tool
//! execution.

use std::fs;

// `engine.rs` was decomposed into submodules under `core/engine/`. The
// protocol-scrubbing strings the tests below assert on are now spread
// across `engine.rs` and several `engine/*.rs` files. We compile-time
// include each so a contributor moving a marker into a sibling submodule
// does not silently break these regression checks.
const ENGINE_SOURCES: &[&str] = &[
    include_str!("../../../engine/src/core/engine.rs"),
    include_str!("../../../engine/src/core/engine/streaming.rs"),
    include_str!("../../../engine/src/core/engine/turn_loop.rs"),
    include_str!("../../../engine/src/core/engine/dispatch.rs"),
    include_str!("../../../engine/src/core/engine/tool_setup.rs"),
    include_str!("../../../engine/src/core/engine/tool_execution.rs"),
    include_str!("../../../engine/src/core/engine/tool_catalog.rs"),
    include_str!("../../../engine/src/core/engine/context.rs"),
    include_str!("../../../engine/src/core/engine/approval.rs"),
    include_str!("../../../engine/src/core/engine/lsp_hooks.rs"),
];

fn any_engine_source_contains(needle: &str) -> bool {
    ENGINE_SOURCES.iter().any(|src| src.contains(needle))
}

const EXPECTED_START_MARKERS: &[&str] = &[
    "[TOOL_CALL]",
    "<codewhale:tool_call",
    "<tool_call",
    "<invoke ",
    "<function_calls>",
];

const EXPECTED_END_MARKERS: &[&str] = &[
    "[/TOOL_CALL]",
    "</codewhale:tool_call>",
    "</tool_call>",
    "</invoke>",
    "</function_calls>",
];

#[test]
fn engine_keeps_known_fake_wrapper_start_markers() {
    for marker in EXPECTED_START_MARKERS {
        let needle = format!("\"{marker}\"");
        assert!(
            any_engine_source_contains(&needle),
            "no engine source file still mentions start marker `{marker}` — \
             protocol scrubbing may have regressed. Searched for {needle:?} \
             across engine.rs and engine/* submodules."
        );
    }
}

#[test]
fn engine_keeps_known_fake_wrapper_end_markers() {
    for marker in EXPECTED_END_MARKERS {
        let needle = format!("\"{marker}\"");
        assert!(
            any_engine_source_contains(&needle),
            "no engine source file still mentions end marker `{marker}` — \
             protocol scrubbing may have regressed. Searched for {needle:?} \
             across engine.rs and engine/* submodules."
        );
    }
}

#[test]
fn engine_marker_counts_stay_paired() {
    // A future contributor could quietly drop a closing marker and leave the
    // filter able to enter tool-call mode without ever leaving it. Lock the
    // count to whatever the constants currently declare.
    assert_eq!(EXPECTED_START_MARKERS.len(), EXPECTED_END_MARKERS.len());
    assert!(any_engine_source_contains("TOOL_CALL_START_MARKERS"));
    assert!(any_engine_source_contains("TOOL_CALL_END_MARKERS"));
}

#[test]
fn engine_emits_compact_fake_wrapper_notice() {
    assert!(
        any_engine_source_contains("FAKE_WRAPPER_NOTICE"),
        "no engine source file references the protocol-recovery notice constant"
    );
    assert!(
        any_engine_source_contains("API tool channel"),
        "the protocol-recovery notice should mention the API tool channel"
    );
}

#[test]
fn legacy_parser_extracts_bracket_tool_call() {
    let result = crate::tool_parser::parse_tool_calls(
        "intro [TOOL_CALL]\n{\"tool\":\"x\",\"args\":{}}\n[/TOOL_CALL]",
    );
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].name, "x");
    assert_eq!(result.clean_text, "intro");
}

#[test]
fn legacy_parser_extracts_invoke_block() {
    let result = crate::tool_parser::parse_tool_calls(
        "before <invoke name=\"do_thing\"><parameter name=\"k\">v</parameter></invoke> after",
    );
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].name, "do_thing");
}

#[test]
fn legacy_parser_does_not_execute_function_calls_wrapper() {
    // The newer `<function_calls>` wrapper is the kind of forged shape that
    // shows up in non-DeepSeek tool-call leakage. The legacy text parser must
    // NOT turn it into a structured tool call (the engine's filter still
    // strips it from visible text and the model is expected to use the API
    // tool channel instead).
    let raw = "narrative <function_calls>\n{\"name\":\"x\",\"input\":{}}\n</function_calls> end";
    let result = crate::tool_parser::parse_tool_calls(raw);
    assert!(
        result.tool_calls.is_empty(),
        "function_calls wrapper must not be parsed as a real tool call: {:?}",
        result.tool_calls
    );
}

#[test]
fn legacy_parser_marker_helper_flags_fake_wrappers_without_enabling_execution() {
    // `has_tool_call_markers` now also flags forged wrappers so the engine can
    // scrub them from visible text and keep reasoning-placeholder bookkeeping.
    // The parser still must not turn those wrappers into executable calls.
    assert!(crate::tool_parser::has_tool_call_markers(
        "noise [TOOL_CALL]x[/TOOL_CALL]"
    ));
    assert!(crate::tool_parser::has_tool_call_markers(
        "noise <invoke name=\"x\"></invoke>"
    ));
    assert!(crate::tool_parser::has_tool_call_markers(
        "noise <function_calls>{}</function_calls>"
    ));
    assert!(
        crate::tool_parser::parse_tool_calls("noise <function_calls>{}</function_calls>")
            .tool_calls
            .is_empty()
    );
}

#[test]
fn engine_source_file_still_exists_and_is_non_trivial() {
    // Sanity check so the `include_str!` above is meaningful — if the engine
    // module ever moves, this test must be updated alongside it.
    let metadata = fs::metadata(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/src/core/engine.rs"),
    )
    .expect("engine.rs must exist in the Engine crate");
    assert!(
        metadata.len() > 10_000,
        "engine.rs is unexpectedly small ({} bytes); did the file move?",
        metadata.len()
    );
}
