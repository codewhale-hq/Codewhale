//! Transcript history-cell tests.
//!
//! Rebuilt in v0.9.11 after declaring test bankruptcy on the previous suite
//! (123 tests / 3,964 lines). About a third of that file pinned glyph choices,
//! palette tokens, span indices and English label text — `spans[1] == "⣤"`,
//! `title_span.style.fg == theme.tool_title_color`, `visible[1] == "▏ done:
//! scan repo"`. Those assertions fail on every legitimate visual refactor and
//! catch nothing a user would notice, which is the liability `d64b9429b`
//! ("remove brittle visual test mass") named.
//!
//! What survives is named for the *property* it protects. Rules for additions:
//!
//! * Assert a property, not a token. `spans[1] == "⣤"` is a token; "the frame
//!   does not change while motion is reduced" is the property, and it is
//!   strictly stronger — it also catches an animation leak the constant missed.
//! * Where the value is a design choice (color, glyph, verb), assert the
//!   *relationship* between cases instead: warning must not read as error.
//! * One test per property, with its cases in a table — not one test per case.
//! * Never assert `a || b` where `b` is trivially true of any English string.

use super::constants::{TOOL_OUTPUT_HEAD_LINES, TOOL_OUTPUT_LINE_LIMIT, TOOL_OUTPUT_TAIL_LINES};
use super::thinking::cached_color_depth;
use super::{
    ASSISTANT_GLYPH, ExecCell, ExecSource, GenericToolCell, HistoryCell, PlanUpdateCell,
    REASONING_CURSOR, REASONING_OPENER, REASONING_RAIL, RenderMode, ToolCell, ToolStatus,
    TranscriptFold, TranscriptRenderOptions, WebSearchCell, assistant_label_style_for,
    extract_reasoning_summary, render_spillover_annotation, render_thinking,
    render_thinking_with_analysis, running_status_label_with_elapsed,
};
use crate::session_manager::{SessionManager, create_saved_session};
use crate::tools::plan::{PlanSnapshot, StepStatus};
use crate::tui::motion::MotionMode;
use codewhale_models::{ContentBlock, Message, Role};
use codewhale_tui_kit::ui_text::{
    line_to_plain, slice_visible_columns, text_display_width, text_visible_width,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[test]
fn extension_prompt_snapshots_and_withdrawal_show_the_complete_model_input() {
    let instructions = format!("{}\nLast instruction.", "x".repeat(12_000));
    for block in [Some(instructions.as_str()), None] {
        let message = crate::runtime_handoff::extension_prompt_contributions_runtime_message(block);
        let cells = super::history_cells_from_message(&message);
        let [HistoryCell::System { content }] = cells.as_slice() else {
            panic!("runtime instructions must be shown as a system receipt");
        };
        match block {
            Some(text) => assert!(
                content.contains(text),
                "the full model-visible text stays auditable"
            ),
            None => assert!(content.contains("withdrawn")),
        }
    }
    let message = crate::runtime_handoff::mcp_server_instructions_runtime_message(&[(
        "guided".into(),
        "Search before fetching.".into(),
    )]);
    let cells = super::history_cells_from_message(&message);
    assert!(
        matches!(cells.as_slice(), [HistoryCell::System { content }]
            if content.contains("Search before fetching.")),
        "the transcript shows what the model saw"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn line_text(line: &ratatui::text::Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn lines_text(lines: &[ratatui::text::Line<'static>]) -> String {
    lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
}

fn generic_tool(name: &str, status: ToolStatus) -> GenericToolCell {
    GenericToolCell {
        name: name.to_string(),
        status,
        input_summary: None,
        output: None,
        prompts: None,
        spillover_path: None,
        output_summary: None,
        is_diff: false,
    }
}

fn exec_tool(command: &str, status: ToolStatus) -> ExecCell {
    ExecCell {
        command: command.to_string(),
        status,
        output: None,
        live_output: None,
        shell_task_id: None,
        owner_agent_id: None,
        owner_agent_name: None,
        started_at: None,
        duration_ms: None,
        stale_elapsed_since_output_ms: None,
        waiting: None,
        source: ExecSource::Assistant,
        description: None,
        interaction: None,
        output_summary: None,
    }
}

fn numbered_output(count: usize) -> String {
    (0..count)
        .map(|i| format!("row {i:02} plain content"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn calm_options() -> TranscriptRenderOptions {
    TranscriptRenderOptions {
        low_motion: true,
        ..TranscriptRenderOptions::default()
    }
}

#[test]
fn a_system_note_keeps_the_backslashes_in_a_windows_path() {
    // A note names paths. The markdown renderer honours CommonMark §2.4, where
    // a backslash before ASCII punctuation is an escape, so `\.` used to print
    // as `.` and `C:\Users\asus\.codewhale` rendered as
    // `C:\Users\asus.codewhale` — a directory that does not exist.
    // A width wide enough for the whole line: the point of the case is the
    // backslash, not the wrapper, and a narrower frame soft-wraps mid-path and
    // splits the string this asserts on.
    let cell = HistoryCell::System {
        content: concat!(
            "Setup provider/model state was not saved: failed to persist setup state to ",
            r"C:\Users\asus\.codewhale\setup_state.json"
        )
        .to_string(),
    };
    let text: String = cell
        .lines_with_copy_metadata(400, calm_options())
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        text.contains(r"C:\Users\asus\.codewhale\setup_state.json"),
        "{text}"
    );
    assert!(!text.contains("asus.codewhale"), "{text}");
}

#[test]
fn a_system_note_leaves_a_fenced_block_verbatim() {
    // A fence is already verbatim: doubling inside one would print the extra
    // backslash instead of protecting it.
    let cell = HistoryCell::System {
        content: concat!("env:\n```\n", r"C:\Users\asus\.codewhale", "\n```").to_string(),
    };
    let text: String = cell
        .lines_with_copy_metadata(120, calm_options())
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains(r"C:\Users\asus\.codewhale"), "{text}");
    assert!(!text.contains(r"C:\\Users"), "{text}");
}

#[test]
fn a_system_note_leaves_inline_markup_and_table_rows_verbatim() {
    // The renderer copies inline code and table cells verbatim, so doubling a
    // backslash inside one prints it doubled; bold and strikethrough read a
    // doubled backslash back as one. These are the shapes third-party guidance
    // brings into a note (MCP servers, extension prompts), and each renders a
    // Windows path correctly.
    for content in [
        r"see `C:\Users\asus\.codewhale` for the file".to_string(),
        r"**C:\Users\asus** is the home".to_string(),
        r"| C:\Users\asus | ok |".to_string(),
        r"~~C:\Users\asus~~".to_string(),
    ] {
        let cell = HistoryCell::System {
            content: content.clone(),
        };
        let text: String = cell
            .lines_with_copy_metadata(120, calm_options())
            .iter()
            .flat_map(|line| line.line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains(r"C:\Users\asus"), "{content} -> {text}");
        assert!(!text.contains(r"C:\\Users"), "{content} -> {text}");
    }
}

#[test]
fn a_fence_of_four_backticks_is_not_closed_by_three() {
    // The renderer closes only on a run at least as long as the opener, so a
    // three-backtick line inside a four-backtick fence is code content. The
    // escaper has to agree, or it starts doubling inside a code sample.
    let content = concat!(
        "sample:\n````\n",
        r"C:\Users\asus\.codewhale",
        "\n```\n",
        r"C:\Users\asus\.codewhale",
        "\n````"
    );
    let cell = HistoryCell::System {
        content: content.to_string(),
    };
    let text: String = cell
        .lines_with_copy_metadata(120, calm_options())
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains(r"C:\Users\asus"), "{text}");
    assert!(!text.contains(r"C:\\Users"), "{text}");
}

#[test]
fn ordinary_cell_fold_preserves_body_metadata_and_full_exports() {
    let content = format!(
        "[reference](https://example.com/reference)\n\n{}",
        (1..=12)
            .map(|line| format!("paragraph {line:02}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let mut tool = exec_tool("example", ToolStatus::Failed);
    tool.output = Some(content.clone());
    let cells = [
        HistoryCell::User {
            content: content.clone(),
        },
        HistoryCell::Assistant {
            content: content.clone(),
            streaming: false,
        },
        HistoryCell::Assistant {
            content: content.clone(),
            streaming: true,
        },
        HistoryCell::System {
            content: content.clone(),
        },
        HistoryCell::Error {
            message: content.clone(),
            severity: codewhale_error_taxonomy::ErrorSeverity::Error,
        },
        HistoryCell::Tool(ToolCell::Exec(tool)),
    ];
    for cell in cells {
        let options = calm_options();
        let full = cell.lines_with_copy_metadata(80, options);
        let (preview, action) =
            cell.lines_with_copy_metadata_folded(80, options, Some(TranscriptFold::Collapsed));
        assert!(
            preview.len() < full.len(),
            "a folded long body stays bounded"
        );
        assert_eq!(action, Some(super::CellFoldAction::Expand));
        assert_eq!(preview[0].copy_prefix_width, preview[0].line.width());
        assert!(preview[0].links.is_empty());
        let body = &preview[1..];
        for (index, line) in body.iter().enumerate() {
            assert_eq!(line.line, full[index].line);
            assert_eq!(line.links, full[index].links);
            assert_eq!(line.copy_prefix_width, full[index].copy_prefix_width);
            if index + 1 < body.len() {
                assert_eq!(line.copy_separator_after, full[index].copy_separator_after);
            }
        }
        assert_eq!(
            body.last().unwrap().copy_separator_after,
            codewhale_tui_kit::ui_text::CopyLineSeparator::Newline
        );
        let (plain, plain_action) =
            cell.lines_with_options_folded(80, options, Some(TranscriptFold::Collapsed));
        assert_eq!(plain_action, action);
        assert_eq!(
            plain,
            preview
                .iter()
                .map(|line| line.line.clone())
                .collect::<Vec<_>>()
        );
        let (restored, action) =
            cell.lines_with_copy_metadata_folded(80, options, Some(TranscriptFold::Expanded));
        assert!(action.is_none());
        assert_eq!(restored.len(), full.len());
        for (restored, original) in restored.iter().zip(&full) {
            assert_eq!(restored.line, original.line);
            assert_eq!(restored.links, original.links);
            assert_eq!(restored.copy_prefix_width, original.copy_prefix_width);
            assert_eq!(restored.copy_separator_after, original.copy_separator_after);
        }
        assert!(lines_text(&cell.transcript_lines(80)).contains("paragraph 12"));
    }
}

// ---------------------------------------------------------------------------
// Leaks — a rendered cell never exposes something the user was not shown
// ---------------------------------------------------------------------------

/// Spilled tool output lives in a file under the session directory. The path is
/// an internal storage detail: it names the user's home, their session id, and
/// a content hash, and it is useless to them because the affordance opens the
/// pager, not the file. No width, no render mode, and no standalone annotation
/// may print it.
///
/// Replaces five separate tests that each checked one width or one mode.
#[test]
fn no_width_or_render_mode_leaks_a_spillover_storage_path() {
    let secret = "/Users/private/.codewhale/sessions/session-a/artifacts/hash.txt";

    for width in [18_u16, 40, 80, 120] {
        for mode in [RenderMode::Live, RenderMode::Transcript] {
            let mut cell = generic_tool("read_file", ToolStatus::Success);
            cell.input_summary = Some("cmd: cargo build --release".to_string());
            cell.output = Some(numbered_output(40));
            cell.spillover_path = Some(PathBuf::from(secret));

            let rendered = lines_text(&cell.lines_with_mode(width, true, mode));
            for fragment in ["/Users", ".codewhale", "sessions/", "hash.txt"] {
                assert!(
                    !rendered.contains(fragment),
                    "storage path fragment {fragment:?} leaked at width {width} in {mode:?}: \
                     {rendered:?}"
                );
            }
        }

        // The standalone affordance carries no path either, and it fits the
        // width it was given — an affordance that overflows is a wrap artifact
        // in the transcript.
        let annotation = line_to_plain(&render_spillover_annotation(width));
        assert!(
            text_display_width(&annotation) <= usize::from(width),
            "affordance exceeds width {width}: {annotation:?}"
        );
        for fragment in ["/Users", ".codewhale", "hash.txt"] {
            assert!(
                !annotation.contains(fragment),
                "affordance leaked {fragment:?}: {annotation:?}"
            );
        }
    }

    // The common case — a result that never spilled — spends no row on an
    // affordance that would open an empty pager.
    let mut plain = generic_tool("read_file", ToolStatus::Success);
    plain.output = Some("contents".to_string());
    let hint = crate::tui::key_shortcuts::tool_details_shortcut_action_hint("output");
    let rendered = lines_text(&plain.lines_with_mode(80, true, RenderMode::Live));
    assert!(
        !rendered.contains(&hint),
        "a result that did not spill must not advertise the spillover pager: {rendered:?}"
    );
}

/// With reasoning display off, the model's chain of thought must not reach the
/// screen in any lifecycle state — not while streaming, not once complete.
/// The live case still needs a progress signal, so it renders one compact row.
#[test]
fn hidden_reasoning_never_renders_its_content_in_any_state() {
    let secret = "private chain of thought that must not be shown";
    let hidden = TranscriptRenderOptions {
        show_thinking: false,
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };

    let streaming = HistoryCell::Thinking {
        content: secret.to_string(),
        streaming: true,
        duration_secs: None,
    };
    let live = streaming.lines_with_options(80, hidden);
    let live_text = lines_text(&live);
    assert!(
        !live_text.contains(secret),
        "hidden live reasoning revealed its body: {live_text}"
    );
    assert_eq!(
        live.len(),
        1,
        "hidden reasoning is one compact progress row, not a stack of state \
         copy: {live_text}"
    );

    let complete = HistoryCell::Thinking {
        content: secret.to_string(),
        streaming: false,
        duration_secs: Some(1.0),
    };
    assert!(
        complete.lines_with_options(80, hidden).is_empty(),
        "completed hidden reasoning must leave the live transcript entirely"
    );
}

/// A live card is a summary. It must name the tool that ran (so the row is
/// attributable) and must not spend its one line echoing arguments the caller
/// never chose — `max_count: 15` is a schema default, not a user intent.
/// Transcript replay is the record, so it keeps the exact tool id.
///
/// Replaces six tests, one of which (`unknown_generic_tool_keeps_raw_name_in_
/// live_mode`) asserted only `!text.is_empty()` and so could never fail.
#[test]
fn live_cards_name_their_tool_without_echoing_control_only_arguments() {
    assert_eq!(
        super::summarize_tool_args(&serde_json::json!({
            "max_count": 15,
            "timeout_ms": 30_000
        })),
        None,
        "an argument set that is entirely control defaults summarizes to nothing"
    );
    assert_eq!(
        super::summarize_tool_args(&serde_json::json!({
            "max_count": 15,
            "branch": "main"
        }))
        .as_deref(),
        Some("branch: main"),
        "the meaningful key is what the summary is for"
    );

    for name in ["git_log", "future_private_tool"] {
        let mut cell = generic_tool(name, ToolStatus::Success);
        cell.input_summary = Some("max_count: 15".to_string());
        let lines = cell.lines_with_mode(120, true, RenderMode::Live);
        let joined = lines_text(&lines);

        assert_eq!(lines.len(), 1, "compact live row for {name}: {joined:?}");
        assert!(
            joined.contains(name),
            "the row must be attributable to {name}: {joined:?}"
        );
        assert!(
            !joined.contains("max_count"),
            "control defaults must not become the visible summary for {name}: {joined:?}"
        );
    }

    // A tool the UI has a family for is identified by that family live; the
    // raw id would be a second, redundant name. Replay keeps it.
    let mut known = generic_tool("run_verifiers", ToolStatus::Running);
    known.input_summary = Some("profile: auto, level: quick".to_string());
    let known = HistoryCell::Tool(ToolCell::Generic(known));
    let live = lines_text(&known.lines(80));
    let transcript = lines_text(&known.transcript_lines(80));
    assert!(
        !live.contains("run_verifiers"),
        "a known tool id must not take a slot in the compact live card: {live}"
    );
    assert!(
        transcript.contains("run_verifiers"),
        "transcript replay preserves the exact tool id: {transcript}"
    );
}

// ---------------------------------------------------------------------------
// Budget — the card never lies about how much it is showing
// ---------------------------------------------------------------------------

/// `selected_output_indices` fills head + tail and then tops up from lines that
/// look important (error / warning / path). Plain output — a list of names, a
/// clean build log — matches none of those, so the top-up found nothing and the
/// card silently forfeited the rest of its budget while still reporting the
/// remainder as omitted. A card that advertises N rows shows N rows.
#[test]
fn a_live_card_spends_the_whole_output_budget_it_advertises() {
    let total = 40usize;
    let cell = {
        let mut exec = exec_tool("list_things", ToolStatus::Failed);
        exec.output = Some(numbered_output(total));
        exec.duration_ms = Some(120);
        HistoryCell::Tool(ToolCell::Exec(exec))
    };

    let live_text = lines_text(&cell.lines_with_options(80, calm_options()));
    let shown = (0..total)
        .filter(|i| live_text.contains(&format!("row {i:02} plain content")))
        .count();

    assert_eq!(
        shown, TOOL_OUTPUT_LINE_LIMIT,
        "a card promising {TOOL_OUTPUT_LINE_LIMIT} rows must show \
         {TOOL_OUTPUT_LINE_LIMIT}, not stop at head+tail: {live_text}"
    );
    for i in 0..TOOL_OUTPUT_HEAD_LINES {
        assert!(
            live_text.contains(&format!("row {i:02} plain content")),
            "head row {i} missing: {live_text}"
        );
    }
    for i in (total - TOOL_OUTPUT_TAIL_LINES)..total {
        assert!(
            live_text.contains(&format!("row {i:02} plain content")),
            "tail row {i} missing: {live_text}"
        );
    }
}

/// Failure keeps the invocation plus head/tail evidence visible under every
/// density setting; the full record remains in the details view.
#[test]
fn calm1_failed_tool_keeps_context_and_result_under_every_density_setting() {
    let total = 30usize;
    let last = format!("row {:02} plain content", total - 1);

    for (label, options) in [
        ("default", TranscriptRenderOptions::default()),
        (
            "tool details hidden",
            TranscriptRenderOptions {
                show_tool_details: false,
                ..TranscriptRenderOptions::default()
            },
        ),
        (
            "calm mode",
            TranscriptRenderOptions {
                calm_mode: true,
                ..TranscriptRenderOptions::default()
            },
        ),
    ] {
        let cell = {
            let mut cell = generic_tool("read_file", ToolStatus::Failed);
            cell.input_summary = Some("command: noisy".to_string());
            cell.output = Some(numbered_output(total));
            HistoryCell::Tool(ToolCell::Generic(cell))
        };

        let text = lines_text(&cell.lines_with_options(80, options));
        assert!(
            text.contains(" lines) · "),
            "[{label}] a bounded failure must advertise omitted output: {text}"
        );
        assert!(
            text.contains(&last),
            "[{label}] failed output must stay expanded to its last row: {text}"
        );
        assert!(
            text.contains("command: noisy"),
            "[{label}] the failing invocation must stay visible: {text}"
        );
    }
}

/// The live surface is a summary and the transcript is the record. The contract
/// is directional: anything the live view drops must still be in the
/// transcript, and the live view must say so when it drops something. A success
/// gets a bounded preview; a failure gets the full budget; neither may leave
/// the transcript short.
///
/// Replaces four near-identical live/transcript comparison tests.
#[test]
fn whatever_live_truncates_the_transcript_still_holds() {
    let total = 30usize;
    let first = "row 00 plain content";
    let last = format!("row {:02} plain content", total - 1);

    // Failed exec: capped live with an honest marker, uncapped in transcript.
    let failed = {
        let mut exec = exec_tool("noisy_script.sh", ToolStatus::Failed);
        exec.output = Some(numbered_output(total));
        exec.duration_ms = Some(120);
        HistoryCell::Tool(ToolCell::Exec(exec))
    };
    let live = failed.lines_with_options(80, calm_options());
    let transcript = failed.transcript_lines(80);
    let live_text = lines_text(&live);
    let transcript_text = lines_text(&transcript);
    assert!(
        live.len() < transcript.len(),
        "live must compress (live={}, transcript={})",
        live.len(),
        transcript.len()
    );
    assert!(
        live_text.contains(" lines) · "),
        "a live view that drops rows must say so: {live_text}"
    );
    assert!(
        !transcript_text.contains(" lines) · "),
        "the transcript drops nothing, so it claims nothing: {transcript_text}"
    );
    assert!(transcript_text.contains(first) && transcript_text.contains(&last));
    assert!(
        transcript_text.contains("row 15 plain content"),
        "the transcript keeps the middle the live view skipped: {transcript_text}"
    );

    // Successful exec: a bounded head preview, never the whole body.
    let success = {
        let mut exec = exec_tool("noisy_script.sh", ToolStatus::Success);
        exec.output = Some(numbered_output(total));
        exec.duration_ms = Some(120);
        HistoryCell::Tool(ToolCell::Exec(exec))
    };
    let live_text = lines_text(&success.lines_with_options(80, calm_options()));
    let transcript_text = lines_text(&success.transcript_lines(80));
    let previewed = (0..total)
        .filter(|i| live_text.contains(&format!("row {i:02} plain content")))
        .count();
    assert_eq!(
        previewed, 3,
        "a successful exec previews only the opening and final result rows: {live_text}"
    );
    assert!(
        live_text.contains(first) && live_text.contains(&last),
        "the bounded preview retains context and the final result: {live_text}"
    );
    let output_hint = crate::tui::key_shortcuts::tool_details_shortcut_action_hint("output");
    assert!(
        live_text.contains(&output_hint),
        "a shortened success must point to its full output: {live_text}"
    );
    assert!(
        !live_text.contains("row 15 plain content"),
        "the live preview should leave routine middle output for details: {live_text}"
    );
    assert!(transcript_text.contains(first) && transcript_text.contains(&last));
    assert!(
        transcript_text.contains("row 15 plain content"),
        "the full transcript must retain output omitted from the live card: {transcript_text}"
    );

    // Successful generic tool: output collapses entirely live, and does so
    // without spending a row telling the user it collapsed.
    let quiet = {
        let mut cell = generic_tool("read_file", ToolStatus::Success);
        cell.input_summary = Some("path: crates/tui/src/main.rs".to_string());
        cell.output = Some(numbered_output(24));
        HistoryCell::Tool(ToolCell::Generic(cell))
    };
    let live_text = lines_text(&quiet.lines_with_options(80, TranscriptRenderOptions::default()));
    let transcript_text = lines_text(&quiet.transcript_lines(80));
    assert!(
        !live_text.contains(first) && !live_text.contains(" lines) · "),
        "a quiet success collapses silently: {live_text}"
    );
    assert!(transcript_text.contains(first));
    assert!(transcript_text.contains("row 23 plain content"));
}

/// A successful run shows three rows of output. They used to be the first two
/// and the very last, so `npm test` showed a blank row, the script banner and
/// a timing line, and hid whether the tests passed. The rows that say how the
/// run went must be among the three, blank rows must not take a slot, and
/// every hidden row must be announced, including rows after the last one shown.
#[test]
fn a_successful_run_preview_shows_how_the_run_ended() {
    let node_test = "\n> trip-timer@1.0.0 test\n> node --test\n\n\
                     ✔ formats minutes (0.5ms)\n\
                     ✔ trips over an hour show remainder minutes (0.1ms)\n\
                     ℹ tests 2\nℹ suites 0\nℹ pass 2\nℹ fail 0\nℹ cancelled 0\n\
                     ℹ skipped 0\nℹ todo 0\nℹ duration_ms 57.924542";
    let cargo_test = "   Compiling demo v0.1.0\n     Running unittests src/lib.rs\n\n\
                      running 2 tests\ntest a ... ok\ntest b ... ok\n\n\
                      test result: ok. 2 passed; 0 failed; finished in 0.00s\n\n";
    let plain = numbered_output(30);

    // (case, output, rows that must be shown, omission markers in order)
    let cases: [(&str, &str, &[&str], &[&str]); 3] = [
        (
            "totals followed by other lines",
            node_test,
            &["> trip-timer@1.0.0 test", "ℹ pass 2", "ℹ fail 0"],
            &["(+5 lines)", "(+4 lines)"],
        ),
        (
            "result followed by blank lines",
            cargo_test,
            &[
                "Compiling demo v0.1.0",
                "test b ... ok",
                "test result: ok. 2 passed; 0 failed",
            ],
            &["(+3 lines)"],
        ),
        (
            "nothing states an outcome",
            &plain,
            &[
                "row 00 plain content",
                "row 28 plain content",
                "row 29 plain content",
            ],
            &["(+27 lines)"],
        ),
    ];

    for (case, output, shown, markers) in cases {
        let cell = {
            let mut exec = exec_tool("npm test", ToolStatus::Success);
            exec.output = Some(output.to_string());
            exec.duration_ms = Some(120);
            HistoryCell::Tool(ToolCell::Exec(exec))
        };
        let live = cell.lines_with_options(80, calm_options());
        let live_text = lines_text(&live);

        for row in shown {
            assert!(
                live_text.contains(row),
                "[{case}] `{row}` hidden: {live_text}"
            );
        }
        let source_rows_shown = output
            .lines()
            .filter(|line| !line.trim().is_empty() && live_text.contains(line.trim()))
            .count();
        assert_eq!(
            source_rows_shown,
            shown.len(),
            "[{case}] the preview keeps its three-row budget: {live_text}"
        );
        let announced: Vec<String> = live
            .iter()
            .map(line_text)
            .filter(|line| line.contains(" lines) · "))
            .collect();
        assert_eq!(
            announced.len(),
            markers.len(),
            "[{case}] every gap is announced once: {live_text}"
        );
        for (line, marker) in announced.iter().zip(markers) {
            assert!(line.contains(marker), "[{case}] `{marker}`: {live_text}");
        }

        let transcript_text = lines_text(&cell.transcript_lines(80));
        for line in output.lines() {
            assert!(
                transcript_text.contains(line.trim()),
                "[{case}] the transcript keeps `{line}`: {transcript_text}"
            );
        }
    }
}

/// Repro for #80: a `git diff --stat`-shaped result must keep its newlines on
/// the transcript surface — one file per row, not squashed into one line.
#[test]
fn multi_line_tool_output_keeps_one_row_per_source_line() {
    let diff_stat = "Cargo.lock                |  1 +\n\
                     crates/cli/Cargo.toml     |  1 +\n\
                     crates/cli/src/main.rs    | 47 ++++++\n\
                     crates/config/src/lib.rs  | 27 ++++\n\
                     crates/tui/src/mcp.rs     | 384 +++++";

    let cell = {
        let mut cell = generic_tool("read_file", ToolStatus::Success);
        cell.input_summary = Some("command: git diff --stat".to_string());
        cell.output = Some(diff_stat.to_string());
        HistoryCell::Tool(ToolCell::Generic(cell))
    };

    let transcript_text = lines_text(&cell.transcript_lines(80));
    for needle in [
        "Cargo.lock",
        "crates/cli/Cargo.toml",
        "crates/cli/src/main.rs",
        "crates/config/src/lib.rs",
        "crates/tui/src/mcp.rs",
    ] {
        assert!(
            transcript_text.contains(needle),
            "transcript missing {needle:?}: {transcript_text}"
        );
    }
    let cargo_lock_row = transcript_text
        .lines()
        .find(|line| line.contains("Cargo.lock"))
        .expect("Cargo.lock row must exist");
    assert!(
        !cargo_lock_row.contains("crates/cli/Cargo.toml"),
        "two files were joined onto one row: {cargo_lock_row}"
    );
}

/// Reasoning folds in the live view and the fold is reversible: the collapsed
/// state must truncate a long body, the expanded state must restore every line
/// it dropped, and both must show the model's own identifiers verbatim (the
/// #4146/#4148 scrub rendered `refresh_catalog_cache` as `…` and protected
/// nothing, since the body was always one keypress away). The configured
/// default only inverts which state the toggle starts in.
///
/// Replaces four separate fold tests.
#[test]
fn reasoning_folds_in_live_and_the_fold_is_reversible() {
    let body = (1..=20)
        .map(|i| format!("step {i:02}: refresh_catalog_cache iteration"))
        .collect::<Vec<_>>()
        .join("\n");
    let cell = HistoryCell::Thinking {
        content: body,
        streaming: false,
        duration_secs: Some(1.0),
    };

    for default_expanded in [false, true] {
        let options = TranscriptRenderOptions {
            thinking_default_expanded: default_expanded,
            low_motion: true,
            ..TranscriptRenderOptions::default()
        };
        // An explicit intent says expanded or collapsed outright, so the same
        // two calls answer for either configured default. Running both proves
        // the intent is not re-read through the preference.
        let expanded = lines_text(
            &cell
                .lines_with_options_folded(80, options, Some(TranscriptFold::Expanded))
                .0,
        );
        let collapsed = lines_text(
            &cell
                .lines_with_options_folded(80, options, Some(TranscriptFold::Collapsed))
                .0,
        );

        for i in 1..=20 {
            assert!(
                expanded.contains(&format!("step {i:02}: refresh_catalog_cache iteration")),
                "[default_expanded={default_expanded}] expanded reasoning dropped line {i}: \
                 {expanded}"
            );
        }
        assert!(
            !collapsed.contains("step 20:"),
            "[default_expanded={default_expanded}] the collapsed fold must truncate: {collapsed}"
        );
        assert!(
            collapsed.contains("refresh_catalog_cache"),
            "[default_expanded={default_expanded}] the shown head keeps identifiers \
             verbatim: {collapsed}"
        );
        assert!(
            !collapsed.contains("Space:") && !collapsed.contains("Ctrl+O"),
            "[default_expanded={default_expanded}] the per-cell renderer stays \
             target-neutral; the chord belongs to whoever owns focus: {collapsed}"
        );
    }
}

/// The preference baseline (verbose session or expanded default) decides only
/// the cells nobody has touched; an explicit intent decides its own cell in
/// both directions. Verbose plus an expanded default renders expanded — the
/// old triple-XOR collapsed exactly that cell — and an explicit expand stays
/// expanded under every preference combination, which the later relative bit
/// still got wrong (#5847).
#[test]
fn explicit_thinking_fold_outranks_every_preference_baseline() {
    let body = (1..=20)
        .map(|i| format!("step {i:02}: baseline check"))
        .collect::<Vec<_>>()
        .join("\n");
    let cell = HistoryCell::Thinking {
        content: body,
        streaming: false,
        duration_secs: Some(1.0),
    };
    // (fold, verbose, default_expanded, expect_expanded)
    for (fold, verbose, default_expanded, expect_expanded) in [
        // No explicit intent: the preference baseline decides.
        (None, false, false, false),
        (None, false, true, true),
        (None, true, false, true),
        (None, true, true, true),
        // An explicit expand renders expanded whatever the preferences say.
        (Some(TranscriptFold::Expanded), false, false, true),
        (Some(TranscriptFold::Expanded), false, true, true),
        (Some(TranscriptFold::Expanded), true, false, true),
        (Some(TranscriptFold::Expanded), true, true, true),
        // And an explicit collapse renders collapsed whatever they say.
        (Some(TranscriptFold::Collapsed), false, false, false),
        (Some(TranscriptFold::Collapsed), false, true, false),
        (Some(TranscriptFold::Collapsed), true, false, false),
        (Some(TranscriptFold::Collapsed), true, true, false),
    ] {
        let options = TranscriptRenderOptions {
            verbose,
            thinking_default_expanded: default_expanded,
            low_motion: true,
            ..TranscriptRenderOptions::default()
        };
        let text = lines_text(&cell.lines_with_options_folded(80, options, fold).0);
        let expanded = text.contains("step 20: baseline check");
        assert_eq!(
            expanded, expect_expanded,
            "[fold={fold:?} verbose={verbose} default_expanded={default_expanded}]"
        );
    }
}

/// A completed reasoning cell short enough to fit needs no expand affordance,
/// and the live view must still show it — the alternative was a dead card that
/// said reasoning happened and nothing about what it was.
#[test]
fn short_completed_reasoning_is_shown_live_without_an_affordance() {
    let cell = HistoryCell::Thinking {
        content: "One brief reasoning step.".to_string(),
        streaming: false,
        duration_secs: Some(0.4),
    };

    let live_text = lines_text(&cell.lines_with_options(80, calm_options()));
    let transcript_text = lines_text(&cell.transcript_lines(80));

    assert!(
        live_text.contains("One brief reasoning step."),
        "short completed reasoning belongs inline: {live_text}"
    );
    assert!(transcript_text.contains("One brief reasoning step."));
    assert!(
        !live_text.contains("Ctrl+O") && !live_text.contains("Space:"),
        "a body that fits needs no affordance: {live_text}"
    );
}

/// A live reasoning block must show what the model is thinking right now — the
/// old behavior stalled on a `thinking...` placeholder until the block closed,
/// and a long body must keep the newest line rather than the oldest.
#[test]
fn streaming_reasoning_shows_its_newest_line_not_a_placeholder() {
    let short = render_thinking(
        "Step 1: read the code\nStep 2: trace the call\nStep 3: form a hypothesis",
        80,
        true,
        None,
        true,
        true,
    );
    let short_text = lines_text(&short);
    assert!(
        short_text.contains("Step 3: form a hypothesis"),
        "the newest reasoning line must be visible while streaming: {short_text}"
    );
    assert!(
        !short_text.contains("thinking..."),
        "real content means the placeholder must not be drawn: {short_text}"
    );

    let long = (1..=16)
        .map(|i| format!("Reasoning line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let long_text = lines_text(&render_thinking(&long, 80, true, None, true, true));
    assert!(
        long_text.contains("Reasoning line 16"),
        "the tail is what is live: {long_text}"
    );
    assert!(
        !long_text.contains("Reasoning line 1\n"),
        "the head is what gets clipped: {long_text}"
    );
}

/// A foreground shell wait blocks the turn. The card names the command it is
/// running, without a key hint, and does not duplicate the sidebar's live tail
/// in the transcript. Once the command finishes, the final output supersedes
/// any stale live tail.
///
/// Replaces three tests.
#[test]
fn a_foreground_shell_wait_names_its_command_without_a_key_hint() {
    let command = "cargo test --workspace --all-features";
    let running = {
        let mut exec = exec_tool(command, ToolStatus::Running);
        exec.live_output = Some("running line 1\nrunning line 2".to_string());
        exec.shell_task_id = Some("shell_live".to_string());
        exec
    };

    for (label, text) in [
        ("live", lines_text(&running.lines_with_motion(80, true))),
        (
            "transcript",
            lines_text(&HistoryCell::Tool(ToolCell::Exec(running.clone())).transcript_lines(80)),
        ),
    ] {
        assert!(
            text.contains(command),
            "[{label}] the card names what is running: {text}"
        );
        assert!(
            !text.contains("Ctrl+B"),
            "[{label}] a long wait moves itself to the background: {text}"
        );
        assert!(
            !text.contains("running line 1"),
            "[{label}] the live tail belongs to the sidebar and /jobs: {text}"
        );
        assert!(!text.contains("command:"), "[{label}] {text}");
    }

    let mut finished = exec_tool(command, ToolStatus::Success);
    finished.output = Some("final output".to_string());
    finished.live_output = Some("stale live tail".to_string());
    finished.shell_task_id = Some("shell_live".to_string());
    let text = lines_text(&finished.lines_with_motion(80, true));
    assert!(
        !text.contains("stale live tail"),
        "a finished command must not show the tail it already superseded: {text}"
    );
}

// ---------------------------------------------------------------------------
// Clipboard — what you copy is what was authored
// ---------------------------------------------------------------------------

/// Every rendered line carries a `copy_prefix_width`: the display columns of
/// decoration the clipboard must skip. The property is that slicing a line at
/// that width yields the payload and nothing decorative — for role markers,
/// status chrome, and the two-column continuation prefix on wrapped fenced code
/// (which must be counted in display columns, not bytes, or CJK shifts it).
///
/// Replaces three tests that each covered one cell kind.
#[test]
fn the_copy_prefix_skips_every_decoration_and_keeps_the_payload() {
    let decorations = ['╎', '▎', '●', '│', '┃', '✓', '▏'];

    let copied_line = |cell: &HistoryCell, width: u16, needle: &str| -> (String, usize) {
        let rendered = cell.lines_with_copy_metadata(width, TranscriptRenderOptions::default());
        let target = rendered
            .iter()
            .find(|entry| {
                entry
                    .line
                    .spans
                    .iter()
                    .any(|span| span.content.contains(needle))
            })
            .unwrap_or_else(|| panic!("no rendered line contains {needle:?}"));
        let text = line_to_plain(&target.line);
        (
            slice_visible_columns(&text, target.copy_prefix_width, text_visible_width(&text)),
            target.copy_prefix_width,
        )
    };

    // Fenced code: indentation survives, decoration does not.
    let rust_fence = HistoryCell::Assistant {
        content: "```rust\n    let answer = 42;\n```".to_string(),
        streaming: false,
    };
    let (copied, _) = copied_line(&rust_fence, 40, "answer");
    assert!(
        copied.contains("    let answer = 42;"),
        "code indentation was not preserved: {copied:?}"
    );
    for glyph in decorations {
        assert!(
            !copied.contains(glyph),
            "decorative glyph {glyph:?} leaked into copied code: {copied:?}"
        );
    }

    // Wrapped CJK code: the prefix is two *display* columns, not two bytes.
    let cjk_fence = HistoryCell::Assistant {
        content: "```text\n  中文 = 1\n```".to_string(),
        streaming: false,
    };
    let (copied, prefix) = copied_line(&cjk_fence, 24, "中文");
    assert_eq!(
        prefix, 2,
        "the continuation prefix is the role marker's two display columns"
    );
    assert!(
        copied.starts_with("    中文"),
        "wide-character indentation was mis-sliced: {copied:?}"
    );

    // Tool receipt: status and family chrome are prefix, the receipt text is
    // payload.
    let receipt = {
        let mut exec = exec_tool("printf 'receipt'", ToolStatus::Success);
        exec.output = Some("receipt".to_string());
        HistoryCell::Tool(ToolCell::Exec(exec))
    };
    let rendered = receipt.lines_with_copy_metadata(80, TranscriptRenderOptions::default());
    let header = rendered.first().expect("tool receipt header");
    assert!(
        header.copy_prefix_width >= 4,
        "status and family chrome should be measured as prefix, got {}",
        header.copy_prefix_width
    );
    let body = line_to_plain(&ratatui::text::Line::from(
        header
            .line
            .spans
            .iter()
            .skip(1)
            .cloned()
            .collect::<Vec<_>>(),
    ));
    let copied = slice_visible_columns(&body, header.copy_prefix_width, text_visible_width(&body));
    assert!(
        copied.contains("run done"),
        "receipt text was clipped away: {copied:?}"
    );
    for glyph in decorations {
        assert!(
            !copied.contains(glyph),
            "decorative glyph {glyph:?} leaked into the copied receipt: {copied:?}"
        );
    }
}

/// Issue #1212: the transcript rail (`▏`) marks prose continuation. Inside a
/// fence it corrupts anything the user copies, so no line of a code block may
/// carry it — not the first, not a blank line in the middle, not a wrapped
/// continuation of an over-long source line.
///
/// Replaces four tests that each covered one fence shape.
#[test]
fn no_line_inside_a_fence_carries_the_transcript_rail() {
    let long_source = "let x = ".to_string() + &"abcdef ".repeat(40);

    for (label, content, width) in [
        (
            "short fence",
            "SQL:\n```sql\nSELECT\nFROM customers\n```".to_string(),
            80u16,
        ),
        (
            "multi-line fence",
            "Here's the query:\n```sql\nSELECT\n  c.customer_id,\n  c.name,\n  \
             COUNT(o.order_id) AS order_count\nFROM customers c\nJOIN orders o ON \
             c.customer_id = o.customer_id;\n```"
                .to_string(),
            80,
        ),
        (
            "fence with a blank line",
            "```\nfn one() {}\n\nfn two() {}\n```".to_string(),
            80,
        ),
        ("wrapped fence", format!("```\n{long_source}\n```"), 40),
    ] {
        let cell = HistoryCell::Assistant {
            content,
            streaming: false,
        };
        // Line 0 is the intro paragraph (or the fence opener); every line
        // after it belongs to the code block.
        for line in cell.lines(width).iter().skip(1) {
            let text = line_text(line);
            assert!(
                !text.contains('\u{258F}'),
                "[{label}] code line took the transcript rail: {text:?}"
            );
        }
    }
}

/// Whose text gets interpreted is a trust boundary. The model's markdown is
/// rendered; the user's prompt is shown exactly as typed, including leading
/// hashes, dashes and runs of spaces. A cell holding only whitespace renders
/// nothing at all rather than an orphaned role glyph.
///
/// Replaces three tests.
#[test]
fn authored_text_keeps_its_shape_on_both_sides_of_the_turn() {
    let user = HistoryCell::User {
        content: "  # heading\n- item\n   \nhello    world".to_string(),
    };
    let visible: Vec<String> = user.lines(80).iter().map(line_text).collect();
    assert!(
        visible[0].trim_end().ends_with("# heading"),
        "a user's literal `#` must not become a rendered heading: {visible:?}"
    );
    assert!(
        visible[1].trim_end().ends_with("- item"),
        "dash-prefixed user text stays literal: {visible:?}"
    );
    assert!(
        visible[2].ends_with("   "),
        "whitespace-only user lines survive: {visible:?}"
    );
    assert!(
        visible[3].trim_end().ends_with("hello    world"),
        "internal spacing stays literal: {visible:?}"
    );
    assert!(
        !visible.iter().any(|line| line.contains('\u{2500}')),
        "user text must not gain a markdown heading rule: {visible:?}"
    );

    let assistant = HistoryCell::Assistant {
        content: "# Heading\n\n- item".to_string(),
        streaming: false,
    };
    let visible: Vec<String> = assistant.lines(80).iter().map(line_text).collect();
    assert!(
        visible[0].contains("Heading") && !visible[0].contains("# Heading"),
        "the model's markdown is still parsed: {visible:?}"
    );
    assert!(
        visible.iter().any(|line| line.contains('\u{2500}')),
        "an assistant h1 still draws its rule: {visible:?}"
    );

    // A stray newline streamed between reasoning and a tool call used to render
    // as a bare role glyph with nothing after it.
    for content in ["", "   ", "\n", "\n\n", " \t \n"] {
        for streaming in [false, true] {
            let cell = HistoryCell::Assistant {
                content: content.to_string(),
                streaming,
            };
            assert!(
                cell.lines(80).is_empty(),
                "whitespace-only assistant content {content:?} (streaming={streaming}) \
                 must render nothing"
            );
        }
    }
    let real = HistoryCell::Assistant {
        content: "hi".to_string(),
        streaming: false,
    };
    assert_eq!(
        real.lines(80)[0].spans[0].content.as_ref(),
        ASSISTANT_GLYPH,
        "real content still gets its role marker"
    );
}

/// Reasoning is neither the user's prompt nor the model's answer, and a reader
/// scanning the transcript has to be able to skip it. The markers below are
/// referenced as named constants rather than literal glyphs on purpose: this
/// protects the *distinction*, so a redesign that restyles reasoning stays
/// green while one that stops marking it at all fails.
#[test]
fn reasoning_is_marked_apart_from_both_the_prompt_and_the_answer() {
    let body_text = "concrete reasoning content";
    let reasoning = render_thinking(body_text, 80, false, Some(1.0), false, true);
    assert!(reasoning.len() >= 2, "expected a header and a body line");

    let header = line_text(&reasoning[0]);
    assert!(
        header.starts_with(REASONING_OPENER),
        "the reasoning header opens with its own marker: {header:?}"
    );
    let body = line_text(&reasoning[1]);
    assert!(
        body.starts_with(REASONING_RAIL),
        "the reasoning body carries its own rail: {body:?}"
    );

    let rail = REASONING_RAIL.trim();
    for cell in [
        HistoryCell::User {
            content: body_text.to_string(),
        },
        HistoryCell::Assistant {
            content: body_text.to_string(),
            streaming: false,
        },
    ] {
        let rendered = lines_text(&cell.lines(80));
        assert!(
            rendered.contains(body_text),
            "sanity: the cell rendered its content: {rendered}"
        );
        assert!(
            !rendered.contains(rail),
            "only reasoning may wear the reasoning rail: {rendered}"
        );
    }
}

/// A filled background behind reasoning is unreadable on a transparent or
/// light terminal, so the highlight is configurable. With it off, not one span
/// may carry a background — a single tinted span is the bug. The enabled case
/// follows the terminal's actual color depth: capable terminals tint the body,
/// while ANSI-16 intentionally stays untinted because it cannot render the
/// subtle surface faithfully.
#[test]
fn disabling_the_reasoning_highlight_leaves_no_span_with_a_background() {
    let render = |highlight: bool| {
        render_thinking_with_analysis(
            "reasoning without a filled surface",
            80,
            false,
            Some(1.0),
            false,
            true,
            highlight,
        )
        .0
    };

    assert!(
        render(false)
            .iter()
            .flat_map(|line| line.spans.iter())
            .all(|span| span.style.bg.is_none()),
        "a disabled highlight must not tint any span"
    );
    let enabled_has_background = render(true)
        .iter()
        .flat_map(|line| line.spans.iter())
        .any(|span| span.style.bg.is_some());
    assert_eq!(
        enabled_has_background,
        codewhale_palette::reasoning_surface_tint(cached_color_depth()).is_some(),
        "the enabled highlight must follow the terminal color-depth contract"
    );
}

// ---------------------------------------------------------------------------
// Motion — reduced motion is actually still
// ---------------------------------------------------------------------------

/// The deleted tests pinned the frozen glyphs (`assert_eq!(spans[1], "⣤")`).
/// That breaks on a skin change and passes on the bug that matters: a marker
/// that keeps animating for a user who asked it to stop. The property is
/// stillness — a running cell's rendered frame must not depend on how long it
/// has been running once motion is reduced — and the full-motion case is
/// asserted alongside it so a renderer that froze everything could not make
/// this test vacuously true.
///
/// Stillness is not enough on its own. Animation frame 0 is U+2800 BRAILLE
/// PATTERN BLANK, an invisible cell. Freezing there (or on the Still path)
/// looks like a missing marker, which is why reduced motion must freeze on a
/// filled, legible bubble rather than the blank the spinner starts on.
#[test]
fn reduced_and_still_motion_render_a_frame_that_does_not_move() {
    let frame_symbols = super::TOOL_RUNNING_SYMBOLS.len() as u64;
    let frame_at = |elapsed_ms: u64, low_motion: bool, motion: MotionMode| {
        let mut exec = exec_tool("echo hi", ToolStatus::Running);
        exec.started_at = Some(Instant::now() - Duration::from_millis(elapsed_ms));
        let cell = HistoryCell::Tool(ToolCell::Exec(exec));
        lines_text(&cell.lines_with_options(
            80,
            TranscriptRenderOptions {
                low_motion,
                motion_mode: motion,
                ..TranscriptRenderOptions::default()
            },
        ))
    };

    // Half a spinner cycle apart, and both well under the 3s elapsed-badge
    // threshold so the badge itself cannot be the thing that differs.
    let early = codewhale_tui_kit::spinner::LIVE_MARKER_DELAY_MS;
    let late = early + super::TOOL_STATUS_SYMBOL_MS * (frame_symbols / 2);
    assert!(
        late < 3_000,
        "both samples must stay under the elapsed badge"
    );

    // Two independent mechanisms are supposed to produce stillness — the
    // `low_motion` flag and the resolved `motion_mode`. Each is asserted on its
    // own so losing either one fails here, rather than only losing both.
    for (low_motion, motion) in [
        (true, MotionMode::Reduced),
        (true, MotionMode::Still),
        (false, MotionMode::Reduced),
        (false, MotionMode::Still),
    ] {
        let frozen = frame_at(early, low_motion, motion);
        assert_eq!(
            frozen,
            frame_at(late, low_motion, motion),
            "low_motion={low_motion} / {motion:?} must not animate the live marker"
        );
        assert!(
            !frozen.contains('\u{2800}'),
            "a frozen marker must still be visible: low_motion={low_motion} / {motion:?}: {frozen:?}"
        );
    }
    assert_ne!(
        frame_at(early, false, MotionMode::Full),
        frame_at(late, false, MotionMode::Full),
        "full motion must actually animate, or the stillness assertions above \
         prove nothing"
    );

    // The same contract for the two other animated surfaces: the streaming
    // reasoning cursor and the assistant role marker's pulse.
    let cursor_off = lines_text(&render_thinking(
        "ongoing reasoning...",
        80,
        true,
        None,
        false,
        true,
    ));
    assert!(
        !cursor_off.contains(REASONING_CURSOR),
        "low motion must suppress the streaming reasoning cursor: {cursor_off}"
    );
    assert_eq!(
        assistant_label_style_for(true, true).fg,
        assistant_label_style_for(false, false).fg,
        "a streaming assistant marker under low motion must look exactly like an \
         idle one — no pulse"
    );
}

/// Dual of the low-motion freeze above: when the cell is streaming and
/// motion is allowed, the assistant marker must actually pulse. The deleted
/// test slept up to 1s sampling `SystemTime` until the 2s sine dipped; the
/// property is that the streaming+motion color is `pulse_brightness` of the
/// idle source, which we can check without waiting on the wall clock.
///
/// Around the sine crest, `pulse_brightness` rounds back to the source
/// (~70ms of a 2s cycle). Matching the current instant would then also pass
/// a renderer that never pulsed, so we only compare once the pure function
/// itself is off the crest — a busy wait, not a sleep.
#[test]
fn assistant_marker_pulses_when_streaming_and_motion_is_allowed() {
    use codewhale_palette::{self as palette, pulse_brightness};

    let idle = assistant_label_style_for(false, false).fg;
    assert_eq!(
        idle,
        Some(palette::WHALE_ACTION),
        "the idle marker is the unpulsed source; pulsing everything would make \
         the streaming assertion vacuously true"
    );
    assert_eq!(
        assistant_label_style_for(true, true).fg,
        idle,
        "low motion must keep the streaming marker at the unpulsed source"
    );

    let epoch_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    let (t0, actual) = loop {
        assert!(
            Instant::now() < deadline,
            "pulse_brightness stayed at the source color through a 250ms spin; \
             the 2s cycle leaves the crest in ~70ms"
        );
        let t0 = epoch_ms();
        // Skip the crest and a few ms of margin so the product read of
        // SystemTime cannot land back on identity between this sample and
        // the call under test.
        let near_crest = (t0.saturating_sub(8)..=t0.saturating_add(8))
            .any(|ms| pulse_brightness(palette::WHALE_ACTION, ms) == palette::WHALE_ACTION);
        if near_crest {
            continue;
        }
        break (t0, assistant_label_style_for(true, false).fg);
    };
    let t1 = epoch_ms();
    let matches_pulse =
        (t0..=t1.max(t0)).any(|ms| actual == Some(pulse_brightness(palette::WHALE_ACTION, ms)));
    assert!(
        matches_pulse,
        "streaming + motion must apply pulse_brightness to the assistant \
         marker, got {actual:?}"
    );
    assert_ne!(
        actual, idle,
        "streaming + motion must not sit at the idle color once the pulse \
         is off its crest"
    );
}

/// The still-motion path rewrites the leading status marker in place. It once
/// rewrote any braille cell it found, which silently ate braille that was part
/// of the tool's own output.
#[test]
fn the_still_marker_rewrite_never_consumes_braille_tool_output() {
    let mut cell = generic_tool("read_file", ToolStatus::Running);
    cell.output = Some("⣿".to_string());
    let cell = HistoryCell::Tool(ToolCell::Generic(cell));

    let lines = cell.lines_with_options(
        80,
        TranscriptRenderOptions {
            low_motion: true,
            motion_mode: MotionMode::Still,
            ..TranscriptRenderOptions::default()
        },
    );

    assert!(
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content.as_ref() == "⣿"),
        "tool output must survive the typed-header marker pass: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Identity — a card never names a verb or a tool it did not run
// ---------------------------------------------------------------------------

/// #4145: a completed grep grouped under the exploration card rendered
/// `read done · Searching …` — the header verb contradicted the label directly
/// under it. The verb must agree with the work, in every locale, and a locale
/// must not fall back to the English status word.
///
/// Replaces two tests, each of which hard-coded one direction.
#[test]
fn a_card_verb_agrees_with_its_own_label_in_every_locale() {
    use codewhale_localization::Locale;

    for (label, expected_en, expected_zh, forbidden_en) in [
        (
            "Searching for `TranscriptScroll`",
            "find done",
            "搜索 完成",
            "read done",
        ),
        ("Reading src/foo.rs", "read done", "读取 完成", "find done"),
    ] {
        let cell = super::ExploringCell {
            entries: vec![super::ExploringEntry {
                label: label.to_string(),
                status: ToolStatus::Success,
                error: None,
            }],
        };

        let header_en = line_text(&cell.lines_with_motion(80, true)[0]);
        assert!(
            header_en.contains(expected_en),
            "{label:?} should read {expected_en:?}: {header_en:?}"
        );
        assert!(
            !header_en.contains(forbidden_en),
            "{label:?} must not be paired with {forbidden_en:?}: {header_en:?}"
        );
        assert!(
            header_en.contains(label),
            "the label itself must survive: {header_en:?}"
        );

        let header_zh = line_text(&cell.lines_with_motion_and_locale(80, true, Locale::ZhHans)[0]);
        assert!(
            header_zh.contains(expected_zh),
            "{label:?} should read {expected_zh:?} in zh-Hans: {header_zh:?}"
        );
        assert!(
            !header_zh.to_lowercase().contains("done"),
            "zh-Hans must not leak the English status word: {header_zh:?}"
        );
        assert!(
            header_zh.contains(label),
            "the label itself must survive localization: {header_zh:?}"
        );
    }
}

/// A read/find receipt reports a line count, so the count has to be real —
/// including the singular/plural and the localized unit. A run receipt reports
/// no count at all: inferring "3 lines" from rendered text that happens to
/// contain `stdout:` would be inventing a number the shell never reported.
#[test]
fn receipts_count_only_what_they_actually_counted() {
    use crate::tui::widgets::tool_card::ToolFamily;
    use codewhale_localization::Locale;

    for (locale, done, unit) in [(Locale::En, "done", "line"), (Locale::ZhHans, "完成", "行")] {
        let label = |family, status, output| {
            super::tool_receipt_label(family, status, Some(output), locale)
        };

        assert_eq!(label(ToolFamily::Read, ToolStatus::Success, ""), done);
        assert_eq!(
            label(ToolFamily::Read, ToolStatus::Success, "hello\n"),
            if locale == Locale::En {
                "1 line".to_string()
            } else {
                format!("1 {unit}")
            }
        );
        assert_eq!(
            label(ToolFamily::Read, ToolStatus::Success, "a\nb\nc\n"),
            if locale == Locale::En {
                "3 lines".to_string()
            } else {
                format!("3 {unit}")
            }
        );
        assert_eq!(
            label(ToolFamily::Find, ToolStatus::Success, "match 1\nmatch 2\n"),
            if locale == Locale::En {
                "2 lines".to_string()
            } else {
                format!("2 {unit}")
            }
        );

        // Run never counts, whatever the body looks like.
        for body in [
            "stdout:\nok\nmore\nstderr:\nbad\n",
            "line 1\nline 2\nline 3\n",
        ] {
            assert_eq!(
                label(ToolFamily::Run, ToolStatus::Success, body),
                done,
                "a run receipt must not infer counts from {body:?}"
            );
        }
    }

    assert_eq!(
        super::tool_receipt_label(
            ToolFamily::Read,
            ToolStatus::Running,
            Some("a\nb"),
            Locale::En
        ),
        "running",
        "an unfinished read has nothing to count yet"
    );
    for (status, text) in [
        (ToolStatus::Running, "运行中"),
        (ToolStatus::Failed, "失败"),
        (ToolStatus::Stopped, "已停止"),
        (ToolStatus::Hydrated, "已加载，未运行"),
        (ToolStatus::Warning, "存在问题"),
    ] {
        assert_eq!(
            super::tool_receipt_label(ToolFamily::Run, status, None, Locale::ZhHans),
            text
        );
    }
}

/// The same truthfulness contract through the real shell render path, where a
/// formatter has already rewritten the output: the header still reports a plain
/// localized completion and never a fabricated line count or stream name.
#[test]
fn shell_headers_stay_truthful_through_the_output_formatters() {
    use codewhale_localization::Locale;

    let cases = [
        (
            "printf redirect",
            "printf '%s\\n' 'hello' 'world' > src/main.rs",
            "printf > src/main.rs\nhello\nworld\n",
        ),
        (
            "logical-or fallback",
            "cargo build || echo fallback",
            "   Compiling pkg v0.1.0\n   Finished dev [unoptimized + debuginfo]\n",
        ),
    ];

    for (label, command, output) in cases {
        let mut cell = exec_tool(command, ToolStatus::Success);
        cell.output = Some(output.to_string());
        cell.duration_ms = Some(42);

        for (locale, done, unit) in [
            (Locale::En, "done", "lines"),
            (Locale::ZhHans, "完成", " 行"),
        ] {
            let header = line_text(&cell.render_with_locale(80, true, RenderMode::Live, locale)[0]);
            assert!(
                header.contains(done),
                "[{label}] header must carry the localized completion: {header}"
            );
            assert!(
                !header.contains(unit) && !header.contains("stdout") && !header.contains("stderr"),
                "[{label}] header must not invent counts or stream names: {header}"
            );
        }
    }
}

/// #4133 / #4148: a spawn yields its card entirely to the DelegateCard, and an
/// inspection (`peek` / `wait` / `status`) is a one-line check in every render
/// mode. It must name the child it checked, must not read as a completed
/// delegation, and must not leak the internal "unknown child" placeholder or
/// echo the verb twice when the resolved identity collapses onto it.
///
/// Replaces eight tests.
#[test]
fn agent_cards_stay_one_line_and_spawn_cards_yield_to_the_delegate_card() {
    let agent = |summary: &str, output: Option<&str>| {
        let mut cell = generic_tool("agent", ToolStatus::Success);
        cell.input_summary = Some(summary.to_string());
        cell.output = output.map(str::to_string);
        cell
    };

    for mode in [RenderMode::Live, RenderMode::Transcript] {
        let spawn = agent(
            "prompt: map the repo",
            Some(r#"{"agent_id":"agent_scout_1","status":"running"}"#),
        );
        assert!(
            spawn.lines_with_mode(120, true, mode).is_empty(),
            "a spawn must not draw a generic card beside the DelegateCard in {mode:?}"
        );

        for (summary, output, expected) in [
            (
                "action: peek agent_id: agent_scout_1",
                Some(r#"{"agent_id":"agent_scout_1","status":"running"}"#),
                "checked",
            ),
            (
                "action: wait",
                Some(r#"{"action":"wait","settled":[{"agent_id":"agent_scout_1"}]}"#),
                "waited",
            ),
            (
                "action: status agent_id: agent_scout_1",
                Some(r#"{"agent_id":"agent_scout_1","status":"running","terminal":false}"#),
                "checked",
            ),
        ] {
            let cell = agent(summary, output);
            let lines = cell.lines_with_mode(120, true, mode);
            let text = lines_text(&lines);
            assert_eq!(
                lines.len(),
                1,
                "{summary:?} must stay one line in {mode:?}: {lines:?}"
            );
            assert!(
                text.contains(expected),
                "{summary:?} should read as {expected:?}: {text:?}"
            );
            assert!(
                !text.to_lowercase().contains("delegate done"),
                "an inspection must not read as a finished delegation: {text:?}"
            );
        }
    }

    // Identity fallbacks: no raw placeholder, no doubled verb.
    let unresolved = agent("action: peek agent_type: delegate", None);
    let text = lines_text(&unresolved.lines_with_mode(80, true, RenderMode::Live));
    assert!(
        !text.contains("unknown child"),
        "the internal fallback token must not reach the transcript: {text:?}"
    );

    let collapsing = agent("action: peek role: delegate", None);
    let text = lines_text(&collapsing.lines_with_mode(80, true, RenderMode::Live));
    assert!(
        text.contains(" agent "),
        "the agent family label should appear once in the summary: {text:?}"
    );
    let mut failed = agent("action: peek agent_id: agent_scout_1", Some("failed"));
    failed.status = ToolStatus::Failed;
    let text = lines_text(&failed.lines_with_mode_and_locale(
        120,
        true,
        RenderMode::Live,
        codewhale_localization::Locale::ZhHans,
    ));
    assert!(text.contains("失败"), "{text}");
    assert!(!text.contains("已检查"), "{text}");
}

/// A tool the catalog does not have produces one useful sentence — the catalog
/// error — and nothing else. The old rendering spent a `name:` / `args:` /
/// `result:` block restating a call that never happened.
#[test]
fn an_unknown_tool_failure_shows_only_the_catalog_error() {
    let mut cell = generic_tool("item", ToolStatus::Failed);
    cell.input_summary = Some("status: pending".to_string());
    cell.output = Some(
        "Tool 'item' is not available in the current tool catalog. \
         Checklist entries are not separate tool calls."
            .to_string(),
    );

    for mode in [RenderMode::Live, RenderMode::Transcript] {
        let lines = cell.lines_with_mode(120, true, mode);
        let text = lines_text(&lines);
        assert_eq!(lines.len(), 1, "single header line in {mode:?}: {lines:?}");
        assert!(
            text.contains("Tool 'item' is not available"),
            "the catalog error is the useful part: {text:?}"
        );
        assert!(
            !text.contains("name: item"),
            "no name/args/result block for a call that did not happen: {text:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Severity — the ranks stay distinguishable
// ---------------------------------------------------------------------------

/// The deleted tests pinned each severity to a named palette constant, so a
/// theme change broke four tests and a severity collapse broke none. What has
/// to hold is the *relationship*: `Critical` reads exactly as loud as `Error`,
/// `Warning` is visibly not an error, and `Info` is quieter than both — so a
/// transient retry cannot be mistaken for a hard failure sitting next to it.
#[test]
fn error_severity_ranks_stay_visually_distinguishable() {
    use codewhale_error_taxonomy::ErrorSeverity;

    let rank = |severity| {
        let cell = HistoryCell::Error {
            message: "Authentication failed: invalid API key".to_string(),
            severity,
        };
        let lines = cell.lines(80);
        assert!(!lines.is_empty(), "{severity:?} must render a line");
        let label = &lines[0].spans[0];
        (label.content.to_string(), label.style.fg)
    };

    let (error_label, error_fg) = rank(ErrorSeverity::Error);
    let (critical_label, critical_fg) = rank(ErrorSeverity::Critical);
    let (warning_label, warning_fg) = rank(ErrorSeverity::Warning);
    let (info_label, info_fg) = rank(ErrorSeverity::Info);

    assert_eq!(
        (critical_label, critical_fg),
        (error_label.clone(), error_fg),
        "Critical and Error both flip offline mode; they must read identically"
    );
    assert_ne!(
        warning_fg, error_fg,
        "a warning that reads as an error is the whole bug this guards"
    );
    assert_ne!(warning_label, error_label, "and the labels must differ too");
    assert_ne!(info_fg, error_fg, "info must not shout");
    assert_ne!(info_fg, warning_fg, "info must not read as a warning");
    assert_ne!(info_label, warning_label);

    // The body inherits the label's rank rather than staying neutral, or the
    // colour would carry no information past the first word.
    let cell = HistoryCell::Error {
        message: "Authentication failed: invalid API key".to_string(),
        severity: ErrorSeverity::Error,
    };
    let body_fg = cell
        .lines(80)
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains("Authentication"))
        .expect("error body span")
        .style
        .fg;
    assert_eq!(body_fg, error_fg);
}

#[test]
fn error_guidance_keeps_recovery_commands_on_their_own_line() {
    let cell = HistoryCell::Error {
        message: "DeepSeek API key not found.\nSave it:\n  codewhale auth set --provider deepseek"
            .to_string(),
        severity: codewhale_error_taxonomy::ErrorSeverity::Error,
    };
    for width in [80, 140] {
        let lines = cell.lines(width);
        let command_line = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .find(|line| line.contains("codewhale auth set --provider deepseek"))
            .expect("recovery command stays intact");
        assert!(!command_line.contains('\n'), "{command_line:?}");
        assert!(!command_line.contains("Save it:"), "{command_line:?}");
    }
}

/// A multiline failure can run past the bottom of the terminal while its full
/// text stays in history. The live cell advertises the pager; the pager and the
/// transcript must carry the recovery instruction verbatim and must not
/// recursively advertise themselves.
#[test]
fn an_error_cell_advertises_the_pager_live_and_never_inside_it() {
    let recovery = "Refusing insecure base URL 'http://192.168.1.25:8000/v1'.\n\
Loopback hosts (localhost, 127.0.0.1, [::1]) are auto-allowed.\n\
Set CODEWHALE_ALLOW_INSECURE_HTTP=1 only for a trusted LAN host.";
    let cell = HistoryCell::Error {
        message: recovery.to_string(),
        severity: codewhale_error_taxonomy::ErrorSeverity::Error,
    };

    let live_text = lines_text(&cell.lines(48));
    let transcript_text = lines_text(&cell.transcript_lines(200));
    let hint = crate::tui::key_shortcuts::tool_details_shortcut_action_hint("full error");

    assert!(live_text.contains(&hint), "{live_text}");
    assert!(!transcript_text.contains(&hint), "{transcript_text}");
    assert!(
        transcript_text.contains("CODEWHALE_ALLOW_INSECURE_HTTP=1"),
        "the actionable instruction must survive verbatim: {transcript_text}"
    );
    assert!(
        transcript_text.contains("192.168.1.25"),
        "the offending host must survive verbatim: {transcript_text}"
    );
}

// ---------------------------------------------------------------------------
// Cards with a content contract
// ---------------------------------------------------------------------------

/// A search receipt has to name where the answer came from and whether the
/// provider it claimed was the provider it used.
#[test]
fn a_web_search_receipt_names_its_source_and_any_degradation() {
    let cell = WebSearchCell {
        query: "current release".to_string(),
        status: ToolStatus::Success,
        summary: Some("Found 2 results".to_string()),
        source: Some("provider-native/xai/grok-4.5".to_string()),
        degraded: Some("provider_native -> duckduckgo".to_string()),
        ref_count: 2,
    };

    let rendered = lines_text(&cell.lines_with_motion(120, true));

    for needle in [
        "source",
        "provider-native/xai/grok-4.5",
        "degraded",
        "provider_native -> duckduckgo",
        "citations",
    ] {
        assert!(rendered.contains(needle), "missing {needle:?}: {rendered}");
    }
}

/// A workflow's transcript is one row per run; live progress is the
/// workbar's. A foreground `run` card that returned its settled record says
/// only the finish — the final state replaces `started` — without repeating the header
/// in a body; the expanded card adds the goal, the child labels, the final
/// result and the error; the status card lists the runs it found.
///
/// Replaces three tests, and drops assertions of the form
/// `contains('s') || contains('m')` — true of essentially any English string.
#[test]
fn workflow_cards_report_lifecycle_children_phases_and_failures() {
    let run_output = serde_json::json!({
        "run_id": "workflow_2400c600",
        "status": "completed",
        "workflow_goal": "audit the FLEET and WORKFLOW docs",
        "child_ids": ["a1", "a2", "a3"],
        "progress": ["phase: Scan", "log: 3 findings"],
        "events": [
            {"type": "task_started", "task_id": "a1", "label": "scan-docs",
             "workflow_run_id": "workflow_2400c600", "workflow_phase_id": "Scan",
             "workflow_task_label": "scan-docs", "workflow_child_index": 0},
            {"type": "task_started", "task_id": "a2", "workflow_task_label": "check-fleet",
             "workflow_run_id": "workflow_2400c600", "workflow_child_index": 1},
            {"type": "task_started", "task_id": "a3", "label": "summarize",
             "workflow_run_id": "workflow_2400c600", "workflow_child_index": 2},
        ],
        "schema_errors": [],
    })
    .to_string();
    let mut run = generic_tool("workflow", ToolStatus::Success);
    run.input_summary = Some("action: run".to_string());
    run.output = Some(run_output);
    let text = lines_text(&run.lines_with_mode(120, true, RenderMode::Live));
    assert!(
        !text.contains("started"),
        "the settled record replaces the start line: {text:?}"
    );
    assert!(
        text.contains("finished") && text.contains("/3 done"),
        "the finish line with done/total agents: {text:?}"
    );
    assert_eq!(text.lines().count(), 1, "one row for the run: {text:?}");
    // #6503: a run with no failures does not announce `0 fail`.
    assert!(!text.contains("fail"), "no zero failure count: {text:?}");
    assert!(
        !text.contains("status:"),
        "the body must not repeat the header lifecycle: {text:?}"
    );

    let failed_output = serde_json::json!({
        "run_id": "workflow_exp",
        "status": "failed",
        "workflow_goal": "ship v0.8.68",
        "started_at_ms": 1000,
        "completed_at_ms": 5000,
        "source_path": "workflows/demo.workflow.js",
        "error": "phase Verify failed",
        "result": {"summary": "2 of 3 children ok"},
        "events": [
            {"type": "run_started", "at_ms": 1000, "run_id": "workflow_exp",
             "workflow_goal": "ship v0.8.68"},
            {"type": "phase_started", "at_ms": 1100, "title": "Verify"},
            {"type": "task_started", "at_ms": 1200, "task_id": "t1", "label": "run tests",
             "workflow_task_label": "run tests", "profile": "implementer"},
            {"type": "task_completed", "at_ms": 4000, "task_id": "t1", "status": "failed"},
            {"type": "run_completed", "at_ms": 5000, "status": "failed",
             "error": "phase Verify failed"}
        ]
    })
    .to_string();
    let mut failed = generic_tool("workflow", ToolStatus::Failed);
    failed.input_summary = Some("action: run".to_string());
    failed.output = Some(failed_output);
    failed.spillover_path = Some(PathBuf::from("/tmp/wf-artifact.json"));
    let text = lines_text(&failed.lines_with_mode(140, true, RenderMode::Transcript));
    for needle in [
        "ship v0.8.68",
        "Verify",
        "run tests",
        "2 of 3",
        "phase Verify failed",
    ] {
        assert!(
            text.contains(needle),
            "the expanded card must carry {needle:?}: {text}"
        );
    }

    let status_output = serde_json::json!({
        "action": "status",
        "count": 2,
        "runs": [
            {"run_id": "workflow_aaa", "status": "running", "child_count": 4},
            {"run_id": "workflow_bbb", "status": "completed", "child_count": 1},
        ],
    })
    .to_string();
    let mut status = generic_tool("workflow", ToolStatus::Success);
    status.input_summary = Some("action: status".to_string());
    status.output = Some(status_output);
    let text = lines_text(&status.lines_with_mode(120, true, RenderMode::Live));
    for needle in ["2 run(s)", "workflow_aaa", "running", "workflow_bbb"] {
        assert!(
            text.contains(needle),
            "the status card must list {needle:?}: {text:?}"
        );
    }
}

/// The founder's transcript (2026-09-28): a refused `start` echoed
/// `action: start` and hid its reason; the settled run's reason was cut
/// mid-word. Each is one row that says why.
#[test]
fn workflow_rows_say_why_without_raw_action_or_mid_word_cuts() {
    let mut refused = generic_tool("workflow", ToolStatus::Failed);
    refused.input_summary = Some("action: start".to_string());
    refused.output = Some(
        "Error: Invalid input for tool 'workflow': Workflow leaf 'engine-readiness': \
         task(): cwd entries must be bounded repo-relative paths\n\
         Tool validation feedback: {\"category\":\"invalid_input\"}"
            .to_string(),
    );
    let text = lines_text(&refused.lines_with_mode(100, true, RenderMode::Live));
    assert!(!text.contains("action: start"), "{text}");
    assert!(
        text.contains("cwd entries must be bounded repo-relative paths"),
        "{text}"
    );
    assert!(!text.contains("Invalid input for tool"), "{text}");

    let reason = "[auth] Authorization failed: You have run out of credits or need a Grok \
                  subscription. Add credits at https://grok.com/?_s=usage.";
    let failed = serde_json::json!({
        "run_id": "workflow_6409ebe6",
        "status": "failed",
        "workflow_goal": "Read-only release-readiness audit for Codewhale v0.10.1. Determine blockers.",
        "started_at_ms": 1_000,
        "completed_at_ms": 1_355,
        "error": "no task produced a result: all 2 task(s) failed and 1 fan-out(s) lost every slot (no work survived them); the recorded result reflects no completed work",
        "transcript_line": "finished",
        "events": [
            {"type": "run_started", "at_ms": 1_000, "workflow_goal": "Read-only release-readiness audit for Codewhale v0.10.1. Determine blockers."},
            {"type": "task_started", "at_ms": 1_080, "task_id": "a", "workflow_task_label": "engine-readiness"},
            {"type": "task_started", "at_ms": 1_117, "task_id": "b", "workflow_task_label": "desktop-readiness"},
            {"type": "task_completed", "at_ms": 1_329, "task_id": "a", "status": "failed", "reason": reason},
            {"type": "task_completed", "at_ms": 1_338, "task_id": "b", "status": "failed", "reason": reason},
            {"type": "run_completed", "at_ms": 1_355, "status": "failed"},
        ],
    })
    .to_string();
    let mut finish = generic_tool("workflow", ToolStatus::Failed);
    finish.output = Some(failed);
    let text = lines_text(&finish.lines_with_mode(60, true, RenderMode::Live));
    assert!(!text.contains("started"), "{text}");
    assert!(text.contains("0/2 done · 2 failed"), "{text}");
    assert!(text.contains("355ms"), "{text}");
    // The whole first sentence, wrapped, never cut.
    let flat = text
        .split_whitespace()
        .filter(|word| {
            !word
                .chars()
                .all(|ch| ('\u{2500}'..='\u{259F}').contains(&ch))
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        flat.contains(
            "Authorization failed: You have run out of credits or need a Grok subscription"
        ),
        "{text}"
    );
    assert!(
        !flat.contains("grok.com"),
        "only the first sentence: {text}"
    );
    assert!(!text.contains("..."), "{text}");
}

#[test]
fn degraded_workflow_receipt_is_terminal_warning_not_running_or_success() {
    let output = serde_json::json!({
        "run_id": "workflow_partial",
        "status": "degraded",
        "workflow_goal": "review the release",
        "started_at_ms": 1_000,
        "completed_at_ms": 2_000,
        "dispatch_failure_count": 1,
        "dispatch_failures": [{
            "label": "review docs",
            "message": "profile unavailable",
            "at_ms": 1_500,
        }],
    })
    .to_string();
    let mut run = generic_tool("workflow", ToolStatus::Success);
    run.output = Some(output);

    let lines = run.lines_with_mode(120, false, RenderMode::Live);
    let text = lines_text(&lines);
    assert!(
        text.contains("finished with gaps"),
        "warning receipt missing: {text:?}"
    );
    assert!(
        !text.to_lowercase().contains(" done"),
        "must not read as success: {text:?}"
    );
    assert!(
        !text.contains(" running"),
        "must not read as live: {text:?}"
    );

    let warning = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "finished with gaps")
        .expect("terminal warning status span");
    assert_eq!(
        warning.style.fg,
        Some(super::tool_rail_color(ToolStatus::Warning)),
        "degraded receipt must use the terminal warning accent"
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .all(|span| !span
                .content
                .chars()
                .any(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch))),
        "terminal receipt must not retain a spinner: {text:?}"
    );
}

/// A checklist update names one item. Showing the rest would make every
/// single-item edit cost the height of the whole list; showing none would make
/// the row unreadable. An id past the end of the list falls back to a
/// placeholder instead of panicking.
#[test]
fn a_checklist_update_shows_only_the_item_that_changed() {
    let snapshot = super::ChecklistSnapshot {
        items: vec![
            super::ChecklistItemSnapshot {
                content: "Read the spec".to_string(),
                status: "completed".to_string(),
            },
            super::ChecklistItemSnapshot {
                content: "Write the test".to_string(),
                status: "in_progress".to_string(),
            },
            super::ChecklistItemSnapshot {
                content: "Land the PR".to_string(),
                status: "pending".to_string(),
            },
        ],
        completion_pct: 33,
        completed: 1,
        total: 3,
    };
    let lines = super::render_checklist_change_card(
        "todo_update",
        ToolStatus::Success,
        &snapshot,
        &super::ChecklistChange {
            id: 2,
            status: "in_progress".to_string(),
        },
        80,
        true,
        codewhale_localization::Locale::En,
    );
    assert!(lines.len() >= 3, "header, change, summary: {}", lines.len());

    let change = line_text(&lines[1]);
    for needle in ["#2", "Write the test", "in progress"] {
        assert!(change.contains(needle), "missing {needle:?}: {change:?}");
    }
    for other in ["Land the PR", "Read the spec"] {
        assert!(
            !change.contains(other),
            "an update must not redraw the whole list: {change:?}"
        );
    }

    let summary = line_text(lines.last().expect("summary row"));
    assert!(summary.contains("3 items"), "{summary:?}");
    assert!(
        summary.contains(&crate::tui::key_shortcuts::tool_details_shortcut_action_hint("list")),
        "the full list stays one keypress away: {summary:?}"
    );

    let single = super::ChecklistSnapshot {
        items: vec![super::ChecklistItemSnapshot {
            content: "only item".to_string(),
            status: "pending".to_string(),
        }],
        completion_pct: 0,
        completed: 0,
        total: 1,
    };
    let lines = super::render_checklist_change_card(
        "todo_update",
        ToolStatus::Success,
        &single,
        &super::ChecklistChange {
            id: 99,
            status: "completed".to_string(),
        },
        80,
        true,
        codewhale_localization::Locale::En,
    );
    let change = line_text(&lines[1]);
    assert!(change.contains("#99") && change.contains("(missing title)"));
}

/// The plan card is the only place a plan's supporting artifact is visible.
/// Every populated section has to reach the surface, or the model can record
/// context the user never sees.
#[test]
fn a_plan_card_surfaces_every_populated_artifact_section() {
    let cell = PlanUpdateCell {
        snapshot: PlanSnapshot {
            objective: Some("Make Plan mode reviewable".to_string()),
            context_summary: Some("Grounded in issue #2691".to_string()),
            sources_used: vec!["gh issue view 2691".to_string()],
            critical_files: vec!["crates/tui/src/tools/plan.rs".to_string()],
            constraints: vec!["Keep To-do primary".to_string()],
            recommended_approach: Some(
                "Enrich update_plan without breaking legacy calls".to_string(),
            ),
            verification_plan: Some("Run focused renderer tests".to_string()),
            risks_and_unknowns: Some("Metadata-only plans can disappear".to_string()),
            handoff_packet: Some("Next agent should inspect relay output".to_string()),
            items: vec![crate::tools::plan::PlanItemArg {
                step: "Render artifact sections".to_string(),
                status: StepStatus::InProgress,
            }],
            ..PlanSnapshot::default()
        },
        status: ToolStatus::Success,
    };

    let visible = lines_text(&cell.lines_with_motion(120, true));

    for needle in [
        "objective:",
        "Make Plan mode reviewable",
        "source:",
        "gh issue view 2691",
        "file:",
        "verify:",
        "handoff:",
        "Render artifact sections",
    ] {
        assert!(visible.contains(needle), "missing {needle:?}: {visible}");
    }
}

/// A fan-out tool's per-child prompts get one row each so the user can read
/// what each child was asked; the inline `args:` summary that would otherwise
/// say `prompts: <3 items>` is suppressed rather than printed alongside them.
#[test]
fn fan_out_prompts_replace_the_inline_argument_summary() {
    let mut cell = generic_tool("read_file", ToolStatus::Running);
    cell.input_summary = Some("prompts: <3 items>".to_string());
    cell.prompts = Some(vec![
        "Summarize the README".to_string(),
        "List the public types in client.rs".to_string(),
        "Diff this commit against main".to_string(),
    ]);
    let text = lines_text(&HistoryCell::Tool(ToolCell::Generic(cell)).lines(80));

    assert!(text.contains("[0] Summarize the README"));
    assert!(text.contains("[1] List the public types in client.rs"));
    assert!(text.contains("[2] Diff this commit against main"));
    assert!(
        !text.contains("args: prompts:"),
        "the summary the rows replaced must not also render: {text}"
    );

    let mut plain = generic_tool("file_search", ToolStatus::Running);
    plain.input_summary = Some("query: foo, path: src".to_string());
    let text = lines_text(&HistoryCell::Tool(ToolCell::Generic(plain)).lines(80));
    assert!(
        text.contains("query: foo"),
        "a non-fan-out tool keeps its argument summary: {text}"
    );
}

/// A grouped activity row is metadata, not a tool card: exactly one line, and
/// the synthetic tool name that carries it never reaches the screen.
#[test]
fn an_activity_group_renders_as_a_single_metadata_line() {
    let mut cell = generic_tool("activity_group", ToolStatus::Success);
    cell.input_summary = Some("Explored 2 files, 1 search".to_string());

    let lines = cell.lines_with_mode(120, true, RenderMode::Live);

    assert_eq!(lines.len(), 1);
    assert_eq!(lines_text(&lines), "Explored 2 files, 1 search ›");
    assert!(!lines_text(&lines).contains("activity_group"));
    cell.input_summary = None;
    let localized = cell.lines_with_mode_and_locale(
        120,
        true,
        RenderMode::Live,
        codewhale_localization::Locale::ZhHans,
    );
    assert!(lines_text(&localized).contains("更新了元数据"));
    assert!(!lines_text(&localized).contains("Updated metadata"));
}

// ---------------------------------------------------------------------------
// Replay — wire messages project to the right typed cell
// ---------------------------------------------------------------------------

/// The wire carries a `(reasoning omitted)` placeholder for turns whose
/// reasoning the provider did not return. Replaying it as a reasoning cell
/// would put words in the model's mouth.
#[test]
fn restored_history_drops_the_wire_reasoning_placeholder() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "(reasoning omitted)".to_string(),
                signature: None,
                state: None,
                redacted_data: None,
            },
            ContentBlock::Thinking {
                thinking: "Actual model reasoning".to_string(),
                signature: None,
                state: None,
                redacted_data: None,
            },
        ],
    };

    let cells = super::history_cells_from_message(&message);
    assert_eq!(cells.len(), 1);
    assert!(matches!(
        &cells[0],
        HistoryCell::Thinking { content, .. } if content == "Actual model reasoning"
    ));
}

/// Compaction writes an `<archived_context>` envelope whose attributes are the
/// only record of what was dropped. Attribute parsing must survive spaces and
/// punctuation inside the values, or the summary reads with a mangled range.
#[test]
fn archived_context_metadata_survives_spaces_inside_attribute_values() {
    let msg = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "<archived_context level=\"1\" range=\"msg 0-128\" tokens=\"2499\" \
                   density=\"~2,500 tokens\" model=\"deepseek-v4-flash\" \
                   timestamp=\"2026-04-28T00:00:00Z\">\nSummary body\n</archived_context>"
                .to_string(),
            cache_control: None,
        }],
    };

    let cells = super::history_cells_from_message(&msg);
    assert_eq!(cells.len(), 1);
    let HistoryCell::ArchivedContext {
        level,
        range,
        tokens,
        density,
        model,
        timestamp,
        summary,
    } = &cells[0]
    else {
        panic!("expected archived context cell, got {:?}", cells[0]);
    };

    assert_eq!(*level, 1);
    assert_eq!(range, "msg 0-128");
    assert_eq!(tokens, "2499");
    assert_eq!(density, "~2,500 tokens");
    assert_eq!(model, "deepseek-v4-flash");
    assert_eq!(timestamp, "2026-04-28T00:00:00Z");
    assert_eq!(summary, "Summary body");
}

/// Two projections that must not become generic assistant prose: a repair
/// receipt is a system note, and a replayed `update_plan` call rebuilds the
/// typed plan cell with its snapshot intact.
#[test]
fn replay_routes_repair_receipts_and_plan_calls_to_typed_cells() {
    let repair = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "[tool_history_repair] Repaired 1 crashed tool call(s); quarantined 0 \
                   duplicate and 0 orphan terminal result(s)."
                .to_string(),
            cache_control: None,
        }],
    };
    assert!(matches!(
        super::history_cells_from_message(&repair).as_slice(),
        [HistoryCell::System { content }] if content.starts_with("[tool_history_repair]")
    ));

    let plan = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "plan-1".to_string(),
            name: "update_plan".to_string(),
            input: serde_json::json!({
                "objective": "Make Plan mode reviewable",
                "sources_used": ["gh issue view 2691"],
                "critical_files": ["crates/tui/src/tools/plan.rs"],
                "plan": [
                    { "step": "render replay card", "status": "completed" }
                ]
            }),
            caller: None,
            thought_signature: None,
        }],
    };
    let cells = super::history_cells_from_message(&plan);
    assert_eq!(cells.len(), 1);
    let HistoryCell::Tool(ToolCell::PlanUpdate(cell)) = &cells[0] else {
        panic!("expected update_plan replay cell");
    };
    assert_eq!(cell.status, ToolStatus::Success);
    assert_eq!(
        cell.snapshot.objective.as_deref(),
        Some("Make Plan mode reviewable")
    );
    assert_eq!(cell.snapshot.sources_used, vec!["gh issue view 2691"]);
    assert_eq!(cell.snapshot.items[0].status, StepStatus::Completed);
}

/// The runtime appends a `<turn_meta>` block to the user's message. It is
/// scaffolding and must be hidden — but only when it is the trailing block the
/// runtime appended. A user who types the same tag is quoting, not injecting,
/// and their text must survive verbatim.
#[test]
fn user_history_hides_only_the_trailing_turn_metadata_block() {
    let visible = "Explain this literal: <turn_meta>example</turn_meta>";
    let turn_meta = concat!(
        "<turn_meta>\n",
        "Current local date: 2026-07-22\n",
        "Input provenance: external_user\n",
        "Input authority: external_current_turn\n",
        "</turn_meta>",
    );
    let msg = Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: visible.to_string(),
                cache_control: None,
            },
            ContentBlock::Text {
                text: turn_meta.to_string(),
                cache_control: None,
            },
        ],
    };
    assert!(matches!(
        super::history_cells_from_message(&msg).as_slice(),
        [HistoryCell::User { content }] if content == visible
    ));

    let literal_only = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "<turn_meta>user-authored example</turn_meta>".to_string(),
            cache_control: None,
        }],
    };
    assert!(matches!(
        super::history_cells_from_message(&literal_only).as_slice(),
        [HistoryCell::User { content }]
            if content == "<turn_meta>user-authored example</turn_meta>"
    ));
}

/// "Copy answer" must select the last completed assistant cell and serialize
/// exactly its authored text — no reasoning, no tool bodies, no runtime status,
/// no role marker, and never a half-streamed cell.
#[test]
fn the_answer_projection_copies_authored_text_and_nothing_else() {
    use crate::tui::history::text::history_cell_to_clipboard_text;

    let cells = [
        HistoryCell::User {
            content: "please summarize".to_string(),
        },
        HistoryCell::Thinking {
            content: "private reasoning trace".to_string(),
            streaming: false,
            duration_secs: Some(1.0),
        },
        HistoryCell::Tool(ToolCell::Generic({
            let mut cell = generic_tool("read_file", ToolStatus::Success);
            cell.input_summary = Some("src/lib.rs".to_string());
            cell.output = Some("raw tool result body".to_string());
            cell
        })),
        HistoryCell::System {
            content: "runtime status note".to_string(),
        },
        HistoryCell::Assistant {
            content: "still streaming partial".to_string(),
            streaming: true,
        },
        HistoryCell::Assistant {
            content: "## Final answer\nauthored markdown".to_string(),
            streaming: false,
        },
    ];

    let answer = cells
        .iter()
        .rev()
        .find(|cell| cell.is_completed_assistant_answer())
        .expect("the completed assistant cell must qualify");
    let copied = history_cell_to_clipboard_text(answer, 80);

    assert_eq!(copied, "## Final answer\nauthored markdown");
    for excluded in [
        "please summarize",
        "private reasoning trace",
        "raw tool result body",
        "runtime status note",
        "still streaming partial",
        ASSISTANT_GLYPH,
    ] {
        assert!(
            !copied.contains(excluded),
            "answer copy leaked {excluded:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Grouping and small parsers
// ---------------------------------------------------------------------------

fn tool_cell(name: &str, status: ToolStatus) -> HistoryCell {
    let mut cell = generic_tool(name, status);
    cell.input_summary = Some(format!("args for {name}"));
    cell.output = Some(format!("output for {name}"));
    HistoryCell::Tool(ToolCell::Generic(cell))
}

/// Collapsing a run of tool cards is only safe when nothing in the run needs
/// the user's eyes: a failure, an in-flight call, or a shell command must all
/// break the group and stay individually visible, and a run shorter than the
/// threshold is not a run at all.
///
/// Replaces three tests.
#[test]
fn only_contiguous_finished_safe_tool_calls_collapse_into_a_run() {
    let history = vec![
        HistoryCell::User {
            content: "go".to_string(),
        },
        tool_cell("read_file", ToolStatus::Success),
        tool_cell("list_dir", ToolStatus::Success),
        tool_cell("web_search", ToolStatus::Success),
        HistoryCell::Assistant {
            content: "done".to_string(),
            streaming: false,
        },
    ];
    let runs = super::detect_tool_runs(&history, 3);
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].start, runs[0].count), (1, 3));
    assert_eq!(
        runs[0].tool_families,
        vec!["read_file", "list_dir", "web_search"]
    );
    assert_eq!(runs[0].activity.files, 2);
    assert_eq!(runs[0].activity.searches, 1);

    assert!(
        super::detect_tool_runs(
            &[
                tool_cell("read_file", ToolStatus::Success),
                tool_cell("list_dir", ToolStatus::Success),
            ],
            3
        )
        .is_empty(),
        "a run below the threshold is not collapsed"
    );

    assert!(
        super::detect_tool_runs(
            &[
                tool_cell("read_file", ToolStatus::Success),
                HistoryCell::Assistant {
                    content: "pause".to_string(),
                    streaming: false,
                },
                tool_cell("list_dir", ToolStatus::Success),
                tool_cell("web_search", ToolStatus::Success),
            ],
            3
        )
        .is_empty(),
        "assistant prose breaks the run"
    );

    // Each of failure, in-flight and shell breaks the group; only the clean
    // trailing triple survives.
    let mut mixed = Vec::new();
    for breaker in [
        tool_cell("web_search", ToolStatus::Failed),
        tool_cell("web_search", ToolStatus::Running),
        HistoryCell::Tool(ToolCell::Exec({
            let mut exec = exec_tool("rm -rf target", ToolStatus::Success);
            exec.output = Some("ok".to_string());
            exec
        })),
    ] {
        mixed.push(tool_cell("read_file", ToolStatus::Success));
        mixed.push(tool_cell("list_dir", ToolStatus::Success));
        mixed.push(breaker);
    }
    let tail_start = mixed.len();
    mixed.push(tool_cell("read_file", ToolStatus::Success));
    mixed.push(tool_cell("list_dir", ToolStatus::Success));
    mixed.push(tool_cell("web_search", ToolStatus::Success));

    let runs = super::detect_tool_runs(&mixed, 3);
    assert_eq!(runs.len(), 1, "only the clean tail collapses: {runs:?}");
    assert_eq!((runs[0].start, runs[0].count), (tail_start, 3));
}

/// The one-line summary that replaces a collapsed run is the user's only
/// record of it, so it must name what actually happened — the right verb, the
/// right counts, and only the tool families that belong to each clause.
///
/// Replaces four tests.
#[test]
fn a_collapsed_run_summary_names_what_actually_happened() {
    let run = |families: &[&str], activity: super::ToolRunActivitySummary| super::ToolRun {
        start: 4,
        count: families.len(),
        tool_families: families.iter().map(|f| f.to_string()).collect(),
        activity,
    };

    assert_eq!(
        super::tool_run_summary(
            &run(
                &["read_file", "list_dir"],
                super::ToolRunActivitySummary {
                    files: 4,
                    searches: 1,
                    ..Default::default()
                }
            ),
            codewhale_localization::Locale::En
        ),
        "Explored 4 files, 1 search: read file, list dir"
    );

    assert_eq!(
        super::tool_run_summary(
            &run(
                &["read_file", "run_tests", "validate_data"],
                super::ToolRunActivitySummary {
                    files: 2,
                    commands: 2,
                    ..Default::default()
                }
            ),
            codewhale_localization::Locale::En
        ),
        "Explored 2 files: read file, ran 2 commands: run tests, validate data",
        "each clause lists only its own families"
    );

    assert_eq!(
        super::tool_run_summary(
            &run(
                &["session_sync"],
                super::ToolRunActivitySummary {
                    other: 2,
                    ..Default::default()
                }
            ),
            codewhale_localization::Locale::En
        ),
        "Updated metadata",
        "a run of tools with no user-facing family falls back to a plain note"
    );

    // Classification is derived from the real cells, not hand-set counters:
    // command tools count as commands, git history tools count as files.
    let commands = super::detect_tool_runs(
        &[
            tool_cell("run_tests", ToolStatus::Success),
            tool_cell("run_verifiers", ToolStatus::Success),
            tool_cell("validate_data", ToolStatus::Success),
        ],
        3,
    );
    assert_eq!(commands[0].activity.commands, 3);
    assert_eq!(
        super::tool_run_summary(&commands[0], codewhale_localization::Locale::En),
        "Ran 3 commands: run tests, run verifiers, validate data"
    );

    let git = super::detect_tool_runs(
        &[
            tool_cell("git_log", ToolStatus::Success),
            tool_cell("git_show", ToolStatus::Success),
            tool_cell("git_blame", ToolStatus::Success),
        ],
        3,
    );
    assert_eq!(git[0].activity.files, 3);
    assert_eq!(
        super::tool_run_summary(&git[0], codewhale_localization::Locale::En),
        "Explored 3 files: git log, git show, git blame"
    );
    let sample = run(
        &["read_file", "search"],
        super::ToolRunActivitySummary {
            files: 2,
            searches: 1,
            ..Default::default()
        },
    );
    for (locale, expected) in [
        (codewhale_localization::Locale::ZhHans, "浏览了2个文件"),
        (codewhale_localization::Locale::ZhHant, "瀏覽了2個檔案"),
        (codewhale_localization::Locale::Ja, "2ファイル"),
        (codewhale_localization::Locale::Ru, "Просмотрено: файлы — 2"),
        (codewhale_localization::Locale::Uk, "Переглянуто: файли — 2"),
    ] {
        let text = super::tool_run_summary(&sample, locale);
        assert!(text.contains(expected), "{locale:?}: {text}");
        assert!(!text.contains("Explored"), "{text}");
        assert!(text.contains("read file"), "{text}");
    }
}

/// The small pure helpers behind the cards, as one table each. Every row is a
/// documented input shape or a documented rejection; a helper that guesses on
/// malformed input is worse than one that declines.
///
/// Replaces ten single-case tests.
#[test]
fn the_card_helpers_accept_their_documented_forms_and_decline_the_rest() {
    // Agent ids come out of a JSON body that the renderer must not fully parse.
    for (input, expected) in [
        (
            r#"{"agent_id": "agent-abc12", "nickname": "Beluga"}"#,
            Some("agent-abc12"),
        ),
        (
            "{\n    \"agent_id\"   :    \"agent-xyz\",\n    \"model\": \"x\"\n}",
            Some("agent-xyz"),
        ),
        (r#"{"nickname": "Orca", "model": "x"}"#, None),
        (r#"{"agent_id": "", "model": "x"}"#, None),
        ("(not json)", None),
        ("", None),
    ] {
        assert_eq!(
            super::extract_agent_id(input),
            expected,
            "extract_agent_id({input:?})"
        );
    }

    // Checklist update prefixes: both vocabularies, and no guessing.
    for (input, expected) in [
        (
            "Updated todo #3 to in_progress\n{ \"items\": [...] }",
            Some(super::ChecklistChange {
                id: 3,
                status: "in_progress".to_string(),
            }),
        ),
        (
            "Updated checklist #7 to completed\n{ \"items\": [] }",
            Some(super::ChecklistChange {
                id: 7,
                status: "completed".to_string(),
            }),
        ),
        ("{ \"items\": [] }", None),
        ("Wrote 5 todos\n{}", None),
        ("Updated todo #3\n", None),
        ("Updated todo #foo to done\n", None),
    ] {
        assert_eq!(
            super::parse_update_prefix(input),
            expected,
            "parse_update_prefix({input:?})"
        );
    }

    // The elapsed badge appears at three seconds and not before, so quick
    // reads and greps do not visually churn.
    for secs in [0, 1, 2] {
        assert_eq!(
            running_status_label_with_elapsed(secs, codewhale_localization::Locale::En),
            "running"
        );
    }
    for secs in [3u64, 7, 120] {
        assert_eq!(
            running_status_label_with_elapsed(secs, codewhale_localization::Locale::En),
            format!("running ({secs}s)")
        );
    }

    // A reasoning summary prefers an explicit Summary block, and otherwise is
    // the reasoning itself rather than nothing.
    assert_eq!(
        extract_reasoning_summary("Thinking...\nSummary: First line\nSecond line\n\nTail")
            .expect("summary"),
        "First line\nSecond line"
    );
    assert_eq!(
        extract_reasoning_summary("Line one\nLine two").expect("summary"),
        "Line one\nLine two"
    );
}

/// The card rail is the block's border, so it carries the cell's own state.
/// Property, not token: every status paints a distinct rail, the rail is never
/// left unstyled — an unstyled rail is what shipped before, and it made a
/// failed card look exactly like a finished one from the border in — and it
/// agrees with the glyph beside it on everything that needs attention while
/// receding on a settled card, which is the whole point of the split.
#[test]
fn card_rail_carries_the_cell_status() {
    let mut rails = Vec::new();
    for status in [
        ToolStatus::Running,
        ToolStatus::Success,
        ToolStatus::Hydrated,
        ToolStatus::Warning,
        ToolStatus::Failed,
    ] {
        let cell = exec_tool("cargo test", status);
        let lines = cell.render(80, /*low_motion*/ true, RenderMode::Live);
        let first = &lines[0];

        let rail = first.spans.first().expect("card rail span");
        assert!(
            matches!(rail.content.as_ref(), "─ " | "╭ "),
            "{status:?} lost its card rail: {:?}",
            rail.content
        );
        let rail_color = rail.style.fg.unwrap_or_else(|| {
            panic!("{status:?} left the card rail unstyled — the border must follow the state")
        });

        let glyph_color = first.spans[1]
            .style
            .fg
            .expect("header status glyph must be styled");
        assert_eq!(
            rail_color, glyph_color,
            "{status:?} must read the same on the rail and the glyph"
        );

        rails.push((status, rail_color));
    }

    for (i, (status, color)) in rails.iter().enumerate() {
        for (other_status, other_color) in &rails[i + 1..] {
            assert_ne!(
                color, other_color,
                "{status:?} and {other_status:?} draw the same rail"
            );
        }
    }
}

/// Finished work shares quiet ink while its glyph shape preserves identity:
/// a passed verify and a completed read must still be distinguishable.
#[test]
fn a_settled_verify_glyph_does_not_read_as_a_settled_read() {
    let verify = generic_tool("run_tests", ToolStatus::Success);
    let read = generic_tool("read_file", ToolStatus::Success);

    let glyph = |cell: &GenericToolCell| {
        cell.lines_with_mode_and_locale(
            80,
            /*low_motion*/ true,
            RenderMode::Live,
            codewhale_localization::Locale::En,
        )[0]
        // Rail, shared status mark, then the tool-family identity glyph.
        .spans[2]
            .clone()
    };
    let verify = glyph(&verify);
    let read = glyph(&read);
    assert_ne!(verify.content, read.content, "tool identity stays visible");
    assert_eq!(
        verify.style.fg, read.style.fg,
        "settled work shares quiet ink"
    );
}

/// An exploring cell rolls its parallel entries up into one state, and a
/// single failure is never averaged away by its successful siblings. This is
/// the fold that used to exist in three places and could not produce `Failed`
/// in the one that painted the header.
#[test]
fn exploring_cell_status_keeps_the_loudest_terminal_state() {
    use super::{ExploringCell, ExploringEntry};

    let cell = |statuses: &[ToolStatus]| ExploringCell {
        entries: statuses
            .iter()
            .map(|status| ExploringEntry {
                label: "Reading src/foo.rs".to_string(),
                status: *status,
                error: None,
            })
            .collect(),
    };

    for (statuses, expected) in [
        (
            vec![ToolStatus::Success, ToolStatus::Success],
            ToolStatus::Success,
        ),
        (
            vec![ToolStatus::Success, ToolStatus::Running],
            ToolStatus::Running,
        ),
        (
            vec![ToolStatus::Failed, ToolStatus::Running],
            ToolStatus::Running,
        ),
        (
            vec![ToolStatus::Success, ToolStatus::Failed],
            ToolStatus::Failed,
        ),
        (
            vec![ToolStatus::Warning, ToolStatus::Failed],
            ToolStatus::Failed,
        ),
        (
            vec![ToolStatus::Success, ToolStatus::Warning],
            ToolStatus::Warning,
        ),
        (
            vec![ToolStatus::Success, ToolStatus::Hydrated],
            ToolStatus::Hydrated,
        ),
    ] {
        assert_eq!(
            cell(&statuses).status(),
            expected,
            "{statuses:?} should roll up to {expected:?}"
        );
    }

    // And the cell reports that state through the ToolCell it lives in.
    assert_eq!(
        ToolCell::Exploring(cell(&[ToolStatus::Success, ToolStatus::Failed])).status(),
        Some(ToolStatus::Failed)
    );
}

// ---------------------------------------------------------------------------
// Tool-card ink: rail vs glyph
// ---------------------------------------------------------------------------

/// The rail is the card border and follows OMP's rule: every status draws a
/// distinct border. Two statuses sharing a rail is the failure this mapping
/// exists to prevent — it is how `Hydrated` once sat on the running accent and
/// read as live work.
#[test]
fn tool_rail_is_distinct_for_every_status() {
    let statuses = [
        ToolStatus::Running,
        ToolStatus::Success,
        ToolStatus::Hydrated,
        ToolStatus::Warning,
        ToolStatus::Failed,
    ];
    for (i, status) in statuses.iter().enumerate() {
        for other in &statuses[i + 1..] {
            assert_ne!(
                super::tool_rail_color(*status),
                super::tool_rail_color(*other),
                "{status:?} and {other:?} draw the same rail"
            );
        }
    }
}

/// Finished rows recede while failures and warnings retain attention ink.
#[test]
fn calm1_settled_headers_are_muted_without_hiding_failures() {
    use crate::tui::widgets::tool_card::ToolFamily;
    use ratatui::style::Modifier;
    for family in [ToolFamily::Read, ToolFamily::Verify] {
        for status in [
            ToolStatus::Running,
            ToolStatus::Success,
            ToolStatus::Hydrated,
            ToolStatus::Warning,
            ToolStatus::Failed,
        ] {
            assert_eq!(
                super::tool_rail_color(status),
                super::tool_glyph_color(status, family)
            );
        }
        assert_ne!(
            super::tool_glyph_color(ToolStatus::Success, family),
            super::tool_glyph_color(ToolStatus::Failed, family)
        );
    }
    assert!(
        !super::tool_title_style(ToolStatus::Success)
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(
        super::tool_title_style(ToolStatus::Failed)
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

/// Issue #5871: `todo_write` replaces the whole list on every call, so a long
/// session stacked full checklist cards that could only be cleared by `/clear`
/// or `/new` — and both of those also drop `api_messages` and the compaction
/// summary. Only the newest snapshot keeps its card; the ones it replaced keep
/// their header (the progress reading) and a details affordance.
#[test]
fn superseded_todo_snapshots_collapse_to_their_header() {
    let snapshot = |done: usize| {
        let items: Vec<String> = (0..3)
            .map(|i| {
                let status = if i < done { "completed" } else { "pending" };
                format!(r#"{{"content":"step {i}","status":"{status}"}}"#)
            })
            .collect();
        let mut cell = generic_tool("todo_write", ToolStatus::Success);
        cell.output = Some(format!(r#"{{"items":[{}]}}"#, items.join(",")));
        HistoryCell::Tool(ToolCell::Generic(cell))
    };

    let mut options = TranscriptRenderOptions {
        show_tool_details: true,
        ..Default::default()
    };
    let older = snapshot(1);

    let expanded = older.lines_with_options(120, options);
    assert!(
        expanded.len() > 2,
        "the newest snapshot renders its full card: {expanded:?}"
    );

    options.superseded_work_receipt = true;
    let collapsed = older.lines_with_options(120, options);
    assert_eq!(
        collapsed.len(),
        2,
        "a replaced snapshot keeps its header plus the details affordance"
    );
    let header: String = collapsed[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        header.contains("1/3"),
        "the collapsed row keeps the progress reading: {header}"
    );
}

/// One click is one request to open one file.
///
/// The old `try_open_file_at_line` looped over every line of the cell and
/// spawned a detached editor per match, so a stack trace or a grep result could
/// launch several at once, all fighting the still-raw-mode TUI for the tty
/// (#6235). The parser now returns the first resolvable reference and nothing
/// else; spawning belongs to `external_editor`.
#[test]
fn first_file_line_reference_returns_one_match_and_resolves_it() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(workspace.join("src/first.rs"), "fn a() {}\n").unwrap();
    std::fs::write(workspace.join("src/second.rs"), "fn b() {}\n").unwrap();

    let text = "note: two frames below\n  src/first.rs:12\n  src/second.rs:34\n";
    let (path, line) = super::first_file_line_reference(text, workspace)
        .expect("the first resolvable reference is returned");
    assert_eq!(path, workspace.join("src/first.rs"));
    assert_eq!(line, 12);
}

/// The forms tools and models print: rustc's `-->` locator with a column,
/// a backticked reference, and one inside a sentence with punctuation.
#[test]
fn file_line_reference_reads_common_forms_on_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(workspace.join("src/a.rs"), "fn a() {}\n").unwrap();
    let expected = Some((workspace.join("src/a.rs"), 12));

    for line in [
        "  --> src/a.rs:12:5",
        "see `src/a.rs:12` for the loop",
        "the bug is in (src/a.rs:12), again.",
        "src/a.rs:12: error: mismatched types",
        "./src/a.rs:12",
    ] {
        assert_eq!(
            super::file_line_reference(line, workspace),
            expected,
            "{line:?}"
        );
    }
    assert_eq!(super::file_line_reference("src/a.rs:0", workspace), None);
}

/// Model output is not trusted to name a file: an absolute path outside the
/// workspace and a `../` escape both used to open in `$EDITOR`.
#[test]
fn file_line_reference_refuses_paths_outside_the_workspace() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("ws");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(workspace.join("src/in.rs"), "fn a() {}\n").unwrap();
    let outside = root.path().join("outside.rs");
    std::fs::write(&outside, "secret\n").unwrap();

    let absolute_outside = format!("{}:3", outside.display());
    assert_eq!(
        super::file_line_reference(&absolute_outside, &workspace),
        None
    );
    assert_eq!(
        super::file_line_reference("../outside.rs:3", &workspace),
        None
    );
    assert_eq!(
        super::first_file_line_reference(
            &format!("{absolute_outside}\n../outside.rs:1\n"),
            &workspace
        ),
        None
    );

    let absolute_inside = format!("{}:4", workspace.join("src/in.rs").display());
    assert_eq!(
        super::file_line_reference(&absolute_inside, &workspace),
        Some((workspace.join("src/in.rs"), 4)),
        "an absolute path inside the workspace still opens"
    );
}

/// A link inside the workspace passed the text-only check and `is_file()`
/// followed it, so `vendor -> <outside>` or `notes.md -> <outside file>` in
/// model output offered "Open in editor" on a file outside the workspace.
#[cfg(unix)]
#[test]
fn file_line_reference_refuses_links_out_of_the_workspace() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("ws");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.rs"), "secret\n").unwrap();
    std::fs::write(workspace.join("src/in.rs"), "fn a() {}\n").unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("vendor")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.rs"), workspace.join("notes.rs")).unwrap();
    // A link that stays inside is still a link: refused, not followed.
    std::os::unix::fs::symlink(workspace.join("src"), workspace.join("alias")).unwrap();

    for line in ["vendor/secret.rs:1", "notes.rs:1", "alias/in.rs:1"] {
        assert_eq!(
            super::file_line_reference(line, &workspace),
            None,
            "{line:?}"
        );
    }
    assert_eq!(
        super::workspace_file(&workspace, "src"),
        None,
        "a directory"
    );
    assert_eq!(
        super::file_line_reference("src/in.rs:2", &workspace),
        Some((workspace.join("src/in.rs"), 2))
    );
}

/// The two escapes the review named: a directory link to `/` and links into
/// an `.ssh` directory. The `.ssh` here is one the test creates outside the
/// workspace with real files in it, so a follow-the-link check would find
/// them and resolve; the test does not depend on the host's own keys.
#[cfg(unix)]
#[test]
fn file_line_reference_refuses_links_to_root_and_ssh() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = &dir.path().join("ws");
    std::fs::create_dir_all(workspace).unwrap();
    std::os::unix::fs::symlink("/", workspace.join("rootfs")).unwrap();
    let ssh = dir.path().join("home/.ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("id_ed25519"), "PRIVATE KEY\n").unwrap();
    std::fs::write(ssh.join("config"), "Host *\n").unwrap();
    std::os::unix::fs::symlink(ssh.join("id_ed25519"), workspace.join("key.rs")).unwrap();
    std::os::unix::fs::symlink(&ssh, workspace.join("ssh")).unwrap();
    assert!(workspace.join("key.rs").is_file(), "the file link resolves");
    assert!(
        workspace.join("ssh/config").is_file(),
        "the dir link resolves"
    );

    for line in [
        "rootfs/etc/hosts:1",
        "./rootfs/etc/hosts:1",
        "key.rs:1",
        "ssh/config:1",
        "ssh/id_ed25519:1",
    ] {
        assert_eq!(
            super::file_line_reference(line, workspace),
            None,
            "{line:?}"
        );
    }
    let absolute = workspace.join("rootfs/etc/hosts");
    assert_eq!(
        super::workspace_file(workspace, absolute.to_str().unwrap()),
        None,
        "an absolute path through the link"
    );
}

#[test]
fn first_file_line_reference_skips_unresolvable_and_malformed_rows() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(workspace.join("src/real.rs"), "fn a() {}\n").unwrap();

    // A path that does not exist, a bare word, a non-numeric suffix and an
    // empty suffix all fall through to the one row that resolves.
    let text = concat!(
        "  src/missing.rs:9\n",
        "  notafile:12\n",
        "  src/real.rs:abc\n",
        "  src/real.rs:\n",
        "  src/real.rs:7\n",
    );
    let (path, line) =
        super::first_file_line_reference(text, workspace).expect("the only resolvable row wins");
    assert_eq!(path, workspace.join("src/real.rs"));
    assert_eq!(line, 7);

    assert!(
        super::first_file_line_reference("no references here\n", workspace).is_none(),
        "a cell with nothing to open must report nothing, not a default"
    );
}

/// #6601: the project-trust warning is a runtime-owned internal message; the
/// model reads it, the transcript never shows it as the user's words.
#[test]
fn workspace_trust_warning_renders_no_transcript_cell() {
    for warning in [Some("untrusted project skills were skipped"), None] {
        let message = crate::runtime_handoff::workspace_trust_runtime_message(warning);
        assert!(crate::runtime_handoff::is_internal_runtime_handoff(
            &message
        ));
        assert!(
            super::history_cells_from_message(&message).is_empty(),
            "{warning:?}"
        );
    }
}

#[test]
fn calm1_failed_tool_preview_matrix_preserves_full_details_and_mcp_identity() {
    for width in [40, 60, 80, 140] {
        for status in [ToolStatus::Running, ToolStatus::Success, ToolStatus::Failed] {
            let output = (0..30)
                .map(|i| format!("row {i:02}"))
                .collect::<Vec<_>>()
                .join("\n");
            let mcp = HistoryCell::Tool(ToolCell::Mcp(super::McpToolCell {
                tool: "linear_get_issue".into(),
                status,
                content: Some(output.clone()),
                is_image: false,
            }));
            let mut generic = generic_tool("read_file", status);
            generic.output = Some(output);
            for (cell, is_mcp) in [
                (mcp, true),
                (HistoryCell::Tool(ToolCell::Generic(generic)), false),
            ] {
                let options = TranscriptRenderOptions {
                    calm_mode: true,
                    show_tool_details: false,
                    low_motion: true,
                    ..Default::default()
                };
                let live = cell.lines_with_options(width, options);
                let text = lines_text(&live);
                let full = lines_text(&cell.transcript_lines(width));
                assert!(
                    full.contains("row 15"),
                    "full details must preserve omitted content"
                );
                if is_mcp {
                    assert_eq!(text.matches("linear_get_issue").count(), 1, "{text}");
                }
                if status == ToolStatus::Failed {
                    assert!(text.contains("row 00") && text.contains("row 29"), "{text}");
                    assert!(!text.contains("row 15"), "{text}");
                    assert!(text.contains(" lines) · "), "{text}");
                    assert_eq!(
                        (0..30)
                            .filter(|i| text.contains(&format!("row {i:02}")))
                            .count(),
                        6
                    );
                    if is_mcp {
                        assert!(live.len() <= 8, "{text}");
                    }
                }
            }
        }
    }
}

#[test]
fn calm1_settled_reasoning_is_one_localized_row_and_stays_expandable() {
    for (locale, expected) in [
        (codewhale_localization::Locale::En, "Thought for 12s"),
        (codewhale_localization::Locale::ZhHans, "思考用时 12s"),
    ] {
        let cell = HistoryCell::Thinking {
            content: "private reasoning body\nlast step".into(),
            streaming: false,
            duration_secs: Some(12.0),
        };
        let options = TranscriptRenderOptions {
            calm_mode: true,
            locale,
            ..Default::default()
        };
        let (lines, action) = cell.lines_with_options_folded(80, options, None);
        assert_eq!(lines.len(), 1);
        assert!(
            lines_text(&lines).contains(expected),
            "{}",
            lines_text(&lines)
        );
        assert_eq!(action, Some(super::CellFoldAction::Expand));
        let expanded = cell
            .lines_with_options_folded(80, options, Some(TranscriptFold::Expanded))
            .0;
        assert!(lines_text(&expanded).contains("private reasoning body"));
    }
}

#[test]
fn calm1_calm_and_hidden_details_share_one_card_budget() {
    let mut exec = exec_tool("command", ToolStatus::Success);
    exec.output = Some(numbered_output(40));
    let cell = HistoryCell::Tool(ToolCell::Exec(exec));
    let mut rendered = Vec::new();
    for (calm_mode, show_tool_details) in [(true, false), (true, true), (false, false)] {
        let options = TranscriptRenderOptions {
            calm_mode,
            show_tool_details,
            low_motion: true,
            ..Default::default()
        };
        let lines = cell.lines_with_options(80, options);
        assert!(lines.len() <= super::constants::TOOL_SUMMARY_CARD_LINES);
        rendered.push(lines_text(&lines));
    }
    assert!(rendered.windows(2).all(|pair| pair[0] == pair[1]));
}

/// #4681: reopening a session must not surface `<turn_meta>` machine
/// blocks in the transcript. Covers the current trailing shape and the
/// legacy leading shape (sessions saved before the turn-meta tail move),
/// while the loaded API history keeps both envelopes intact for replay.
#[test]
fn rehydrated_turn_meta_blocks_never_render_in_history_cells() {
    let tmp = tempdir().expect("tempdir");
    let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");

    let turn_meta = "<turn_meta>\nCurrent local date: 2026-08-01\n</turn_meta>";
    let trailing_shape = Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: "Fix the flaky test".to_string(),
                cache_control: None,
            },
            ContentBlock::Text {
                text: turn_meta.to_string(),
                cache_control: None,
            },
        ],
    };
    let legacy_leading_shape = Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: turn_meta.to_string(),
                cache_control: None,
            },
            ContentBlock::Text {
                text: "Now add the docs".to_string(),
                cache_control: None,
            },
        ],
    };
    let messages = vec![
        trailing_shape,
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Done.".to_string(),
                cache_control: None,
            }],
        },
        legacy_leading_shape,
    ];
    let session = create_saved_session(&messages, "test-model", tmp.path(), 100, None);
    let session_id = session.metadata.id.clone();
    manager.save_session(&session).expect("save");

    let loaded = manager.load_session(&session_id).expect("load");

    // Display path: no rendered cell may carry turn_meta markup.
    let rendered: Vec<HistoryCell> = loaded
        .messages
        .iter()
        .flat_map(super::history_cells_from_message)
        .collect();
    let user_texts: Vec<&str> = rendered
        .iter()
        .filter_map(|cell| match cell {
            HistoryCell::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(user_texts, vec!["Fix the flaky test", "Now add the docs"]);
    assert!(
        !user_texts.iter().any(|text| text.contains("<turn_meta")),
        "rendered cells must not contain turn_meta markup: {user_texts:?}"
    );

    // Model-facing replay: the persisted envelopes survive the round trip.
    let replayed_envelopes = loaded
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| {
            matches!(block, ContentBlock::Text { text, .. } if text.contains("<turn_meta>"))
        })
        .count();
    assert_eq!(replayed_envelopes, 2);
}

#[test]
fn save_and_load_session_preserves_rich_update_plan_tool_payload() {
    let tmp = tempdir().expect("tempdir");
    let manager = SessionManager::new(tmp.path().join("sessions")).expect("new");
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "plan this carefully".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "plan-1".to_string(),
                name: "update_plan".to_string(),
                input: serde_json::json!({
                    "objective": "Make Plan mode reviewable",
                    "sources_used": ["gh issue view 2691"],
                    "critical_files": ["crates/tui/src/tools/plan.rs"],
                    "constraints": ["Preserve legacy update_plan payloads"],
                    "verification_plan": "Run focused plan tests",
                    "handoff_packet": "Next agent should inspect replay",
                    "plan": [
                        { "step": "render replay card", "status": "completed" }
                    ]
                }),
                caller: None,
                thought_signature: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "plan-1".to_string(),
                content: "Plan updated".to_string(),
                is_error: None,
                content_blocks: None,
            }],
        },
    ];
    let session = create_saved_session(&messages, "deepseek-v4-flash", tmp.path(), 42, None);
    let session_id = session.metadata.id.clone();

    manager.save_session(&session).expect("save");
    let loaded = manager.load_session(&session_id).expect("load");

    assert_eq!(loaded.messages.len(), 3);
    let cells = super::history_cells_from_message(&loaded.messages[1]);
    let Some(HistoryCell::Tool(ToolCell::PlanUpdate(cell))) = cells.first() else {
        panic!("expected loaded update_plan to replay as a PlanUpdate cell");
    };
    assert_eq!(
        cell.snapshot.objective.as_deref(),
        Some("Make Plan mode reviewable")
    );
    assert_eq!(
        cell.snapshot.critical_files,
        vec!["crates/tui/src/tools/plan.rs"]
    );
    assert_eq!(cell.snapshot.items[0].status, StepStatus::Completed);
}
