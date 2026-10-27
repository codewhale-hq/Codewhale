//! FEAT-029 Phase 2: host-bound baseline parity for the `debug::diagnostics`
//! command slice.
//!
//! Every assertion in this module compares the *current* public command output
//! against a fixture captured from the untouched implementation at
//! `origin/main` `922679d6c0afe4556f3e5bc59073eb8e4cf93e07`. The fixtures are
//! hand-reviewed source; they are never regenerated from the migrated
//! implementation. Volatile report stamps, cache ages and owned temporary
//! paths are normalised only at the documented comparison boundary.
//!
//! The module deliberately lives at the `commands` root, outside
//! `codewhale-commands`, which owns the portable implementations.

use std::time::Instant;

use crate::commands::debug_diagnostics_test_support::{
    DiagnosticsHarness, SealedHome, assert_fixture, normalize_cache_ages,
};
use crate::commands::{CommandResult, execute};
use crate::tui::app::{AppAction, TurnCacheRecord};
use codewhale_config::ProviderKind;
use codewhale_models::{ContentBlock, Message, Role, SystemPrompt};

fn render(result: &CommandResult) -> String {
    let mut out = String::new();
    out.push_str(&format!("is_error: {}\n", result.is_error));
    match &result.message {
        Some(message) => out.push_str(&format!("message:\n{message}\n")),
        None => out.push_str("message: <none>\n"),
    }
    match &result.action {
        Some(action) => out.push_str(&format!("action: {action:?}\n")),
        None => out.push_str("action: <none>\n"),
    }
    out
}

/// Freeze the complete `/balance` branch pair: the supported provider emits the
/// `FetchBalance` action with no message, an unsupported provider emits the
/// exact message and no action. Asserting the whole `CommandResult` also pins
/// `is_error`, which is easy to regress independently of the visible text.
#[test]
fn balance_branch_pair_matches_baseline() {
    let mut harness = DiagnosticsHarness::new();
    harness.app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(ProviderKind::Deepseek.as_str())
            .expect("captured fixture provider"),
    );
    assert_fixture(
        "balance_supported.txt",
        &render(&execute("/balance", &mut harness.app)),
    );

    let mut harness = DiagnosticsHarness::new();
    harness.app.set_provider_identity_record(
        codewhale_runtime::config::Config::default()
            .resolve_provider_identity(ProviderKind::Ollama.as_str())
            .expect("captured fixture provider"),
    );
    assert_fixture(
        "balance_unsupported.txt",
        &render(&execute("/balance", &mut harness.app)),
    );
}

/// `/preview-request` is the one `Pure` command in the slice: its registered
/// handler must never build a host envelope. The frozen fixtures pin the parsed
/// action payloads and the exact diagnostics for every grammar branch.
#[test]
fn preview_request_grammar_matches_baseline() {
    for (fixture_name, command) in [
        ("preview_default.txt", "/preview-request"),
        ("preview_json.txt", "/preview-request json"),
        ("preview_manifest.txt", "/preview-request manifest"),
        ("preview_base_prompt.txt", "/preview-request base-prompt"),
        (
            "preview_prompt.txt",
            "/preview-request --prompt refactor the parser",
        ),
        (
            "preview_prompt_json.txt",
            "/preview-request json --prompt fix it",
        ),
        (
            "preview_prompt_bytes.txt",
            "/preview-request --prompt    padded  text ",
        ),
        ("preview_unknown.txt", "/preview-request nope"),
        ("preview_bare_prompt.txt", "/preview-request --prompt"),
        ("preview_conflict.txt", "/preview-request json base-prompt"),
    ] {
        let mut harness = DiagnosticsHarness::new();
        assert_fixture(fixture_name, &render(&execute(command, &mut harness.app)));
    }
}

/// The prompt is hashed into the previewed body, so its bytes must survive the
/// dispatcher verbatim — including leading interior and trailing whitespace —
/// and the command must not touch conversation state.
#[test]
fn preview_request_preserves_prompt_bytes_and_mutates_nothing() {
    let mut harness = DiagnosticsHarness::new();
    let before_messages = harness.app.api_messages.len();
    let before_history = harness.app.history.len();

    let prompt = "   padded  text ";
    let result = execute(
        &format!("/preview-request --prompt {prompt}"),
        &mut harness.app,
    );

    let Some(AppAction::PreviewOutboundRequest {
        json,
        base_prompt_only,
        hypothetical_prompt,
    }) = result.action
    else {
        panic!("preview must emit the engine action: {result:?}");
    };
    assert!(!json);
    assert!(!base_prompt_only);
    // Exactly one whitespace codepoint delimits the flag; the rest are bytes.
    assert_eq!(hypothetical_prompt.as_deref(), Some(prompt));
    assert_eq!(harness.app.api_messages.len(), before_messages);
    assert_eq!(harness.app.history.len(), before_history);
}

/// The snapshot-availability check happens *before* format validation: a
/// missing snapshot must report the truthful unavailable state for every
/// argument spelling, not the usage error.
#[test]
fn tools_checks_snapshot_before_format_validation() {
    let mut harness = DiagnosticsHarness::new();
    for (fixture_name, command) in [
        ("tools_no_snapshot.txt", "/tools"),
        ("tools_no_snapshot.txt", "/tools yaml"),
    ] {
        assert_fixture(fixture_name, &render(&execute(command, &mut harness.app)));
    }
}

/// With a prepared snapshot present, the text, explicit-text, JSON and invalid
/// format branches are frozen byte-for-byte, and the compatibility alias
/// `/tool-studio` renders the same payload as the canonical name.
#[test]
fn tools_snapshot_branches_match_baseline() {
    for (fixture_name, command) in [
        ("tools_text.txt", "/tools"),
        ("tools_text_explicit.txt", "/tools text"),
        ("tools_json.txt", "/tools json"),
        ("tools_invalid.txt", "/tools yaml"),
    ] {
        let mut harness = DiagnosticsHarness::new();
        harness.app.session.last_tool_request_snapshot = Some(snapshot());
        assert_fixture(fixture_name, &render(&execute(command, &mut harness.app)));
    }

    let mut harness = DiagnosticsHarness::new();
    harness.app.session.last_tool_request_snapshot = Some(snapshot());
    let canonical = execute("/tools json", &mut harness.app);
    drop(harness);
    let mut alias_harness = DiagnosticsHarness::new();
    alias_harness.app.session.last_tool_request_snapshot = Some(snapshot());
    let alias = execute("/tool-studio json", &mut alias_harness.app);
    assert_eq!(
        render(&canonical),
        render(&alias),
        "the /tool-studio alias must render the canonical payload"
    );
}

/// `/system` Text/Blocks/None and the UTF-8-safe 500-byte truncation boundary
/// are frozen exactly, including the existing total-length wording.
#[test]
fn system_prompt_branches_match_baseline() {
    let mut harness = DiagnosticsHarness::new();
    harness.app.system_prompt = Some(codewhale_models::SystemPrompt::Text(
        "You are a helpful assistant.".to_string(),
    ));
    assert_fixture(
        "system_text.txt",
        &render(&execute("/system", &mut harness.app)),
    );

    let mut harness = DiagnosticsHarness::new();
    harness.app.system_prompt = Some(codewhale_models::SystemPrompt::Blocks(vec![
        codewhale_models::SystemBlock {
            block_type: "text".to_string(),
            text: "First block".to_string(),
            cache_control: None,
        },
        codewhale_models::SystemBlock {
            block_type: "text".to_string(),
            text: "Second block".to_string(),
            cache_control: None,
        },
    ]));
    assert_fixture(
        "system_blocks.txt",
        &render(&execute("/system", &mut harness.app)),
    );

    let mut harness = DiagnosticsHarness::new();
    harness.app.system_prompt = None;
    assert_fixture(
        "system_none.txt",
        &render(&execute("/system", &mut harness.app)),
    );

    let mut harness = DiagnosticsHarness::new();
    harness.app.system_prompt = Some(codewhale_models::SystemPrompt::Text(format!(
        "{}\u{e9}\u{4e2d}",
        "x".repeat(520)
    )));
    assert_fixture(
        "system_truncated.txt",
        &render(&execute("/system", &mut harness.app)),
    );
}

/// The `/system` alias `/xitong` is the same registration; the truncation
/// boundary must stay UTF-8 safe at the existing 500-byte cut.
#[test]
fn system_alias_and_truncation_boundary_are_preserved() {
    let mut harness = DiagnosticsHarness::new();
    harness.app.system_prompt = Some(codewhale_models::SystemPrompt::Text(format!(
        "{}\u{e9}\u{4e2d}",
        "x".repeat(520)
    )));
    let canonical = execute("/system", &mut harness.app);
    let alias = execute("/xitong", &mut harness.app);
    assert_eq!(render(&canonical), render(&alias));
    let message = canonical.message.expect("system message");
    assert!(
        message.contains("(truncated, 525 chars total)"),
        "{message}"
    );
    // The cut lands before the two multibyte characters, so no replacement
    // character from an invalid boundary may appear.
    assert!(!message.contains('\u{fffd}'), "{message}");
}

/// `/context` bare opens the inspector; the report subcommands delegate to the
/// shared renderer; an unknown subcommand is an exact error with no action.
#[test]
fn context_routing_and_report_branches_match_baseline() {
    // The report counts the user's global instructions and installed skills.
    let _home = SealedHome::new();
    let mut harness = DiagnosticsHarness::new();
    assert_fixture(
        "context_bare.txt",
        &render(&execute("/context", &mut harness.app)),
    );

    let mut harness = DiagnosticsHarness::new();
    assert_fixture(
        "context_unknown.txt",
        &render(&execute("/context bogus", &mut harness.app)),
    );

    for (fixture_name, command) in [
        ("context_report.txt", "/context report"),
        ("context_json.txt", "/context json"),
        ("context_summary.txt", "/context summary"),
        ("context_prompt_json.txt", "/context prompt-json"),
    ] {
        let mut harness = DiagnosticsHarness::new();
        let rendered = render(&execute(command, &mut harness.app));
        assert_fixture(fixture_name, &harness.normalize(&rendered));
    }
}

/// The `/context` alias `/ctx` resolves to the same entry, and the bare form
/// must return the inspector action with no message.
#[test]
fn context_alias_and_bare_action_are_preserved() {
    // The alias and canonical reports are compared byte for byte, and both
    // enumerate the user's installed skills: on a developer machine their
    // discovery can differ between two calls in one test.
    let _home = SealedHome::new();
    let mut harness = DiagnosticsHarness::new();
    let bare = execute("/context", &mut harness.app);
    assert!(bare.message.is_none());
    assert!(matches!(bare.action, Some(AppAction::OpenContextInspector)));

    let alias = execute("/ctx report", &mut harness.app);
    let canonical = execute("/context report", &mut harness.app);
    assert_eq!(
        alias.message.as_deref().unwrap_or_default(),
        canonical.message.as_deref().unwrap_or_default(),
    );
}

/// `/tokens` and `/cost` on both the empty and seeded sessions are frozen
/// exactly, including the estimate disclaimer and the coverage line that the
/// two surfaces share.
#[test]
fn tokens_and_cost_coverage_surfaces_match_baseline() {
    let mut harness = DiagnosticsHarness::new();
    assert_fixture(
        "tokens_empty.txt",
        &render(&execute("/tokens", &mut harness.app)),
    );
    assert_fixture(
        "cost_empty.txt",
        &render(&execute("/cost", &mut harness.app)),
    );

    let mut harness = DiagnosticsHarness::new();
    harness.app.session.total_tokens = 1234;
    harness.app.session.session_cost = 0.05;
    harness.app.session.cost_priced_turns = 1;
    harness.app.session.last_prompt_tokens = Some(100);
    harness.app.session.last_completion_tokens = Some(25);
    harness.app.session.last_prompt_cache_hit_tokens = Some(70);
    harness.app.session.last_prompt_cache_miss_tokens = Some(30);
    assert_fixture(
        "tokens_seeded.txt",
        &render(&execute("/tokens", &mut harness.app)),
    );
    assert_fixture(
        "cost_seeded.txt",
        &render(&execute("/cost", &mut harness.app)),
    );
}

/// `/cache` without telemetry, the unknown-argument error, the warmup action,
/// the empty stats/zones reports and the inspect conflict branch are frozen.
#[test]
fn cache_static_branches_match_baseline() {
    for (fixture_name, command) in [
        ("cache_no_data.txt", "/cache"),
        ("cache_unknown.txt", "/cache wat"),
        ("cache_warmup.txt", "/cache warmup"),
        ("cache_stats_empty.txt", "/cache stats"),
        ("cache_zones_empty.txt", "/cache zones"),
        ("cache_inspect_empty.txt", "/cache inspect"),
        (
            "cache_inspect_conflict.txt",
            "/cache inspect --json --verbose",
        ),
    ] {
        let mut harness = DiagnosticsHarness::new();
        assert_fixture(fixture_name, &render(&execute(command, &mut harness.app)));
    }

    // The conflict branch is a bounded message, not a hard error.
    let mut harness = DiagnosticsHarness::new();
    let conflict = execute("/cache inspect --json --verbose", &mut harness.app);
    assert!(!conflict.is_error);
    assert_eq!(
        conflict.message.as_deref(),
        Some("cache inspect: --json and --verbose cannot be combined")
    );
}

/// Recorded turn telemetry is frozen for the history table (full and
/// count-clamped) and for the stats/zones aggregations; only the per-turn age
/// cell is normalised.
#[test]
fn cache_recorded_telemetry_matches_baseline() {
    for (fixture_name, command) in [
        ("cache_history.txt", "/cache"),
        ("cache_history_count.txt", "/cache 2"),
        ("cache_stats_seeded.txt", "/cache stats"),
        ("cache_zones_seeded.txt", "/cache zones"),
    ] {
        let mut harness = DiagnosticsHarness::new();
        seed_turns(&mut harness);
        let rendered = render(&execute(command, &mut harness.app));
        assert_fixture(fixture_name, &normalize_cache_ages(&rendered));
    }
}

/// Freeze the ordered JSON bytes and the observation commit after a real
/// successful inspection. A second inspection compares with the stored first
/// request after the system prefix changes.
#[test]
fn cache_inspect_json_and_previous_state_match_baseline() {
    let mut harness = DiagnosticsHarness::new();
    harness.app.system_prompt = Some(SystemPrompt::Text("Base policy".into()));
    harness.app.session.last_tool_catalog = Some(vec![tool("read_file")]);
    harness.app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Current task".into(),
            cache_control: None,
        }],
    });
    assert!(harness.app.session.last_cache_inspection.is_none());
    let first = execute("/cache inspect --json", &mut harness.app);
    assert!(harness.app.session.last_cache_inspection.is_some());
    let previous = harness.app.session.last_cache_inspection.clone();
    assert_fixture("cache_inspect_json.txt", &render(&first));

    harness.app.system_prompt = Some(SystemPrompt::Text("Changed policy".into()));
    let second = execute("/cache inspect --json", &mut harness.app);
    assert_ne!(harness.app.session.last_cache_inspection, previous);
    assert_fixture("cache_inspect_json_changed.txt", &render(&second));
}

/// The slice inventory is exactly the eight declared commands plus the five
/// mutation commands now adopted by the same debug group; no diagnostics command may
/// lose its registration.
#[test]
fn diagnostics_slice_inventory_is_exactly_the_declared_eight() {
    for name in [
        "tokens",
        "cost",
        "balance",
        "cache",
        "preview-request",
        "tools",
        "system",
        "context",
    ] {
        assert!(
            crate::commands::get_command_info(name).is_some(),
            "/{name} must remain registered"
        );
    }
    for name in ["change", "edit", "diff", "undo", "retry"] {
        assert!(
            crate::commands::get_command_info(name).is_some(),
            "/{name} is the FEAT-030 mutation slice and must stay registered"
        );
    }
}

fn snapshot() -> crate::tool_inspection::ToolInspectionSnapshot {
    crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
        "turn-1",
        2,
        Some(&[tool("read_file"), tool("write_file")]),
    )
}

fn seed_turns(harness: &mut DiagnosticsHarness) {
    let now = Instant::now();
    for record in [
        turn_record(4_000, 200, Some(3_000), Some(1_000), None, now),
        turn_record(6_000, 250, Some(3_000), Some(3_000), Some(150), now),
        turn_record(5_000, 100, Some(2_500), None, None, now),
        turn_record(1_000, 50, None, None, None, now),
    ] {
        harness.app.push_turn_cache_record(record);
    }
}

fn turn_record(
    input_tokens: u32,
    output_tokens: u32,
    cache_hit_tokens: Option<u32>,
    cache_miss_tokens: Option<u32>,
    reasoning_replay_tokens: Option<u32>,
    recorded_at: Instant,
) -> TurnCacheRecord {
    TurnCacheRecord {
        provider: Some(ProviderKind::Deepseek),
        provider_identity: Some("deepseek".to_string()),
        model: Some("deepseek-v4-pro".to_string()),
        auto_model: false,
        input_tokens,
        output_tokens,
        cache_hit_tokens,
        cache_miss_tokens,
        reasoning_replay_tokens,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at,
    }
}

fn tool(name: &str) -> codewhale_models::Tool {
    codewhale_models::Tool {
        tool_type: Some("function".to_string()),
        name: name.to_string(),
        description: format!("{name} test tool"),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}}
        }),
        allowed_callers: None,
        defer_loading: Some(false),
        input_examples: None,
        strict: Some(true),
        cache_control: None,
    }
}
