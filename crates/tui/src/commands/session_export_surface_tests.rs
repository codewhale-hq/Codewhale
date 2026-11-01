//! FEAT-025 Phase 5: public command-surface parity coverage for `/export`.
//!
//! Phase 4 proved handler/rendering/adapter parity with fake facets and
//! relocated host regressions. This module proves the *observable command
//! surface* did not move when registration crossed the portable bridge:
//!
//! * registry metadata (name, alias, usage) and registry position,
//! * the `description_key` -> catalog bridge and its English/localized text,
//! * palette and slash-completion discovery, including the `/daochu` alias,
//! * least authority: exactly `SESSION_EXPORT` and no presentation facet,
//! * canonical-name/alias dispatch equivalence through the public `execute`
//!   seam, with exact receipts and exact visible errors.
//!
//! Like the Phase 3 host regressions, this file deliberately lives at the
//! `commands` root — outside `groups/session`, which FEAT-043 moves into
//! `codewhale-commands`. No real terminal clipboard, GUI, or device is needed:
//! the harness uses the deterministic in-process clipboard and temporary
//! filesystem fixtures.

use tempfile::TempDir;

use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};

use crate::commands::session_export_test_support::{
    assert_only_export_facet_exposed, normalize_export_time,
};
use crate::commands::{CommandResult, execute};
use crate::test_support::{EnvVarGuard, TestEnvLock};
use crate::tui::app::{App, TuiOptions};
use crate::tui::clipboard::ClipboardHandler;
use crate::tui::command_palette;
use codewhale_command_contract::CommandDiscovery;
use codewhale_config::ProviderKind;
use codewhale_localization::Locale;
use codewhale_models::{ContentBlock, Message, Role};
use codewhale_runtime::config::Config;

const EXPORT_NAME: &str = "export";
const EXPORT_ALIAS: &str = "daochu";
const EXPORT_USAGE: &str =
    "/export [clipboard|file [--force] <path>|turn [clipboard|file [--force] <path>]]";
// Pinned to the authoritative catalog values (`crates/localization/locales/{en,fr}.json`,
// key `CmdExportDescription`). The `description_key` bridge must keep resolving the
// `/export` metadata to this catalog entry; the wording itself is owned by the catalog.
const EXPORT_ENGLISH: &str = "Copy a safe export, or write it to a file";
const EXPORT_FRENCH: &str =
    "Copier un export sûr de la conversation, ou l'écrire dans un fichier explicite";

/// Shared host-isolated harness (same isolation order as the Phase 3
/// `ExportHarness`: env guards, then the lock, then the temporary directory).
struct SurfaceHarness {
    app: App,
    _home: EnvVarGuard,
    _codewhale_home: EnvVarGuard,
    _env_lock: TestEnvLock,
    temp: TempDir,
}

impl SurfaceHarness {
    fn new() -> Self {
        let env_lock = crate::test_support::lock_test_env();
        let temp = TempDir::new().expect("tempdir");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).expect("home dir");
        let _home = EnvVarGuard::set("HOME", &home);
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
        let options = TuiOptions {
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory.md"),
            notes_path: temp.path().join("notes.txt"),
            mcp_config_path: temp.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(temp.path())
        };
        let app = App::new(options, &Config::default());
        Self {
            app,
            _home,
            _codewhale_home,
            _env_lock: env_lock,
            temp,
        }
    }

    fn last_copy_path(&self) -> std::path::PathBuf {
        self.temp
            .path()
            .join("home")
            .join("exports")
            .join("last-copy.md")
    }
}

fn text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    }
}

fn export_info() -> &'static crate::commands::CommandInfo {
    crate::commands::get_command_info(EXPORT_NAME).expect("/export must be registered")
}

fn result_message(result: &CommandResult) -> &str {
    result.message.as_deref().unwrap_or_default()
}

#[test]
fn export_registration_metadata_and_registry_position_are_unchanged() {
    let info = export_info();
    assert_eq!(info.name, EXPORT_NAME);
    assert_eq!(info.aliases, &[EXPORT_ALIAS]);
    assert_eq!(info.usage, EXPORT_USAGE);

    // The alias resolves to the same registry entry and canonical metadata.
    let registry = crate::commands::registry();
    let via_alias = registry
        .get(EXPORT_ALIAS)
        .expect("/daochu must resolve to the export entry");
    assert_eq!(via_alias.info().name, EXPORT_NAME);
    assert_eq!(via_alias.info().usage, EXPORT_USAGE);
    assert!(
        registry.get(EXPORT_NAME).is_some(),
        "canonical /export must remain registered"
    );

    // Registry order is preserved: remote-env -> export -> structcopy.
    let names: Vec<&str> = crate::commands::command_infos()
        .iter()
        .map(|info| info.name)
        .collect();
    let position = |name: &str| {
        names
            .iter()
            .position(|candidate| *candidate == name)
            .unwrap_or_else(|| panic!("{name} must be registered; found {names:?}"))
    };
    assert!(
        position("remote-env") < position(EXPORT_NAME),
        "export must stay after remote-env in the session group order"
    );
    assert!(
        position(EXPORT_NAME) < position("structcopy"),
        "export must stay before structcopy in the session group order"
    );
}

#[test]
fn export_declares_exactly_session_export_without_presentation_authority() {
    let command = crate::commands::registry()
        .get(EXPORT_NAME)
        .expect("/export must be registered");
    let handler = command
        .contextual_handler()
        .expect("/export must register through the portable bridge");
    let CommandHandler::Contextual { capabilities, .. } = handler else {
        panic!("/export must be a contextual handler")
    };
    assert_eq!(
        capabilities,
        CommandCapabilities::SESSION_EXPORT,
        "/export declares exactly SESSION_EXPORT"
    );
    assert!(
        !capabilities.contains(CommandCapabilities::PRESENTATION),
        "export must not request presentation authority"
    );

    // The restricted projection exposes only the declared facet. The helper
    // destructures all sixteen `ContextParts` slots, so this is exhaustive
    // rather than a spot-check of the fields listed below by hand.
    let mut harness = SurfaceHarness::new();
    let mut bundle = harness.app.command_contexts();
    let export_only = bundle
        .contexts(CommandCapabilities::SESSION_EXPORT)
        .into_parts();
    assert_only_export_facet_exposed(export_only);
}

#[test]
fn export_description_bridge_preserves_english_localized_and_discovery_metadata() {
    let info = export_info();

    // `description_key` -> `key_to_message_id` -> catalog: English reference
    // and the shipped French pack both resolve through the same bridge.
    assert_eq!(&*info.description_for(Locale::En), EXPORT_ENGLISH);
    assert_eq!(&*info.description_for(Locale::Fr), EXPORT_FRENCH);

    // Palette text keeps the canonical description and advertises the alias.
    let palette = info.palette_description_for(Locale::En);
    assert!(
        palette.contains(EXPORT_ENGLISH),
        "palette description must keep the catalog text: {palette}"
    );
    assert!(
        palette.contains(EXPORT_ALIAS),
        "palette description must keep the alias: {palette}"
    );

    // Discovery classification and visibility are part of the surface.
    assert_eq!(info.discovery(), CommandDiscovery::Primary);
    assert!(!info.is_unlisted());
    assert!(info.show_in_empty_discovery());
    assert!(info.show_in_slash_completion("/exp"));
    assert!(info.requires_argument());
}

#[test]
fn export_is_discoverable_by_name_and_alias_in_palette_and_slash_completion() {
    // Pure discovery: a workspace path is all these surfaces need, so this test
    // avoids the shared environment lock the host fixtures require.
    let workspace_dir = TempDir::new().expect("tempdir");
    let workspace = workspace_dir.path();
    let skills_dir = workspace.join("skills");
    let mcp_config = workspace.join("mcp.json");

    let entries = command_palette::build_entries(
        Locale::En,
        &skills_dir,
        &Default::default(),
        workspace,
        &mcp_config,
        None,
    );
    let export_row = entries
        .iter()
        .find(|entry| entry.label == "/export")
        .expect("/export must appear in the command palette");
    assert!(
        export_row.description.contains(EXPORT_ENGLISH),
        "palette row must carry the catalog description: {}",
        export_row.description
    );
    assert!(
        export_row.description.contains(EXPORT_ALIAS),
        "palette row must advertise /daochu: {}",
        export_row.description
    );

    let by_prefix = crate::tui::widgets::slash_completion_hints(
        "/exp",
        64,
        &[],
        Locale::En,
        Some(workspace),
        ProviderKind::Deepseek,
    );
    assert!(
        by_prefix.iter().any(|hint| hint.name == "/export"),
        "/exp must complete to /export"
    );

    let by_alias = crate::tui::widgets::slash_completion_hints(
        "/daoc",
        64,
        &[],
        Locale::En,
        Some(workspace),
        ProviderKind::Deepseek,
    );
    let alias_row = by_alias
        .iter()
        .find(|hint| hint.name == "/export")
        .expect("/daoc must surface the export command");
    assert_eq!(
        alias_row.alias_hint.as_deref(),
        Some(EXPORT_ALIAS),
        "slash completion must explain the alias match"
    );

    // The alias token is not a second registry entry.
    assert!(
        !entries.iter().any(|entry| entry.label == "/daochu"),
        "the alias must not create a duplicate palette row"
    );
}

#[test]
fn public_dispatch_canonical_name_and_alias_are_byte_equivalent() {
    let mut harness = SurfaceHarness::new();
    let last_copy = harness.last_copy_path();
    let app = &mut harness.app;
    app.current_session_id = Some("session-987654321".to_string());
    app.api_messages = std::sync::Arc::new(vec![
        text_message(Role::User, "Please export this conversation"),
        text_message(Role::Assistant, "Exported on request."),
    ]);
    app.clipboard = ClipboardHandler::for_test(false, false);

    let canonical = execute("/export clipboard", app);
    assert!(!canonical.is_error, "{:?}", canonical.message);
    let canonical_markdown = app
        .clipboard
        .last_written_text()
        .expect("canonical clipboard payload")
        .to_string();
    let canonical_recovery = std::fs::read_to_string(&last_copy).expect("canonical recovery copy");

    // Reset the deterministic clipboard so the second dispatch records its own
    // delivery; state is otherwise identical.
    app.clipboard = ClipboardHandler::for_test(false, false);

    let alias = execute("/daochu clipboard", app);
    assert!(!alias.is_error, "{:?}", alias.message);
    let alias_markdown = app
        .clipboard
        .last_written_text()
        .expect("alias clipboard payload")
        .to_string();
    let alias_recovery = std::fs::read_to_string(&last_copy).expect("alias recovery copy");

    assert_eq!(
        result_message(&canonical),
        result_message(&alias),
        "canonical and alias receipts must be identical"
    );
    assert_eq!(
        normalize_export_time(&canonical_markdown),
        normalize_export_time(&alias_markdown),
        "canonical and alias clipboard payloads must be identical"
    );
    assert_eq!(
        normalize_export_time(&canonical_recovery),
        normalize_export_time(&alias_recovery),
        "canonical and alias recovery copies must be identical"
    );
    assert!(canonical_markdown.contains("# Codewhale conversation export"));
    assert!(canonical_markdown.contains("Please export this conversation"));
}

#[test]
fn public_dispatch_file_receipts_and_usage_errors_are_exact() {
    let mut harness = SurfaceHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let app = &mut harness.app;
    app.workspace = workspace.clone();
    app.api_messages = std::sync::Arc::new(vec![text_message(Role::User, "file export body")]);

    let resolved = std::fs::canonicalize(&workspace)
        .expect("canonical workspace")
        .join("transcript.md");

    let first = execute("/export file transcript.md", app);
    assert!(!first.is_error, "{:?}", first.message);
    assert_eq!(
        result_message(&first),
        format!("Conversation exported to {}", resolved.display())
    );

    let refused = execute("/export file transcript.md", app);
    assert!(refused.is_error, "{:?}", refused.message);
    assert_eq!(
        result_message(&refused),
        format!(
            "Error: Failed to export Conversation to {}: destination already exists: {}. Re-run with `/export file --force <path>` to replace it",
            resolved.display(),
            resolved.display()
        )
    );

    let forced = execute("/export file --force transcript.md", app);
    assert!(!forced.is_error, "{:?}", forced.message);
    assert_eq!(
        result_message(&forced),
        format!(
            "Conversation exported to {} (overwrite explicitly allowed)",
            resolved.display()
        )
    );

    let usage = export_info().usage;
    for (arg, reason) in [
        ("file", "missing file path"),
        ("file --force", "missing file path"),
        ("clipboard extra.md", "clipboard does not accept a path"),
    ] {
        let result = execute(&format!("/export {arg}"), app);
        assert!(result.is_error, "{arg} must be rejected");
        assert_eq!(
            result_message(&result),
            format!("Error: {reason}. Usage: {usage}"),
            "{arg} must keep the baseline usage error"
        );
    }
}

// ---------------------------------------------------------------------------
// FEAT-025 Phase 7 (Task 7.2): extraction-readiness and scope audits.
//
// These audits protect the D4/D5/D8/D9 ownership contract that the portable
// source must satisfy before FEAT-043 can physically move the export slice into
// `codewhale-commands`. They are source-level checks, not runtime behavior
// tests, and they live at the `commands` root outside the movable group.
// ---------------------------------------------------------------------------

/// Production portion of the portable export source: everything before its
/// `#[cfg(test)]` module, with comments removed (full-line and trailing) so
/// rationale text (`Concrete `App`, clipboard, filesystem, ...`) is never
/// mistaken for an import or a concrete host symbol.
fn portable_production_source(source: &str) -> String {
    let mut production = String::new();
    for line in source.lines() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let code = strip_line_comment(line);
        if code.trim().is_empty() {
            continue;
        }
        production.push_str(code.trim_end());
        production.push('\n');
    }
    production
}

/// Strip a trailing `//` comment without touching `//` inside a string or
/// character literal.
///
/// A naive strip would be wrong in both directions: portable code carries URL
/// literals such as `https://…`, and a marker string could hide a real token.
/// Lifetime ticks (`&'a str`) are not treated as character literals so a
/// following comment is still removed.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => break,
                        _ => i += 1,
                    }
                }
            }
            b'\'' => {
                let opens_literal = matches!(
                    (bytes.get(i + 1), bytes.get(i + 2)),
                    (Some(b'\\'), _) | (Some(_), Some(b'\''))
                );
                if opens_literal {
                    i += 1;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'\\' => i += 2,
                            b'\'' => break,
                            _ => i += 1,
                        }
                    }
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// Word-boundary identifier match.
///
/// `production.contains("App")` fires on a legitimate `Append`, while an exact
/// equality check would miss `App::new`. `\b` matches the identifier token
/// only, so the audit fails for the right reason.
fn contains_identifier(haystack: &str, identifier: &str) -> bool {
    let pattern = format!(r"\b{}\b", regex::escape(identifier));
    regex::Regex::new(&pattern)
        .expect("identifier regex")
        .is_match(haystack)
}

/// The audits above are only as trustworthy as their scanners, so pin the two
/// behaviours that decide whether a finding is real: comment stripping must not
/// eat string literals, and identifier matching must not fire on a longer word.
#[test]
fn portable_source_audit_helpers_are_token_aware() {
    assert_eq!(strip_line_comment("let x = 1; // App"), "let x = 1; ");
    assert_eq!(strip_line_comment("// whole line"), "");
    assert_eq!(
        strip_line_comment("let url = \"https://example.test/a\";"),
        "let url = \"https://example.test/a\";"
    );
    assert_eq!(
        strip_line_comment("let c = '/'; let y = 1; // keep"),
        "let c = '/'; let y = 1; "
    );
    // A lifetime tick must not swallow the rest of the line.
    assert_eq!(
        strip_line_comment("fn f<'a>(x: &'a str) {} // note"),
        "fn f<'a>(x: &'a str) {} "
    );

    assert!(contains_identifier("let app = App::new();", "App"));
    assert!(!contains_identifier("values.append(item);", "App"));
    assert!(!contains_identifier("struct AppNew;", "App"));
    assert!(contains_identifier("use ratatui::text::Line;", "ratatui"));
    assert!(contains_identifier("unsafe { x }", "unsafe"));
}

#[test]
fn portable_export_source_has_no_host_dependency() {
    let source = include_str!("groups/session/export.rs");
    let production = portable_production_source(source);

    // Only the external contract, the pure sanitizer, `serde_json`, `std`, and
    // the shared session `CommandResult` may be imported.
    let allowed_use_prefixes = [
        "use std::",
        "use codewhale_command_contract",
        "use codewhale_sanitize",
        "use serde_json",
        "use super::CommandResult",
    ];
    for line in production.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("use ") {
            continue;
        }
        assert!(
            allowed_use_prefixes
                .iter()
                .any(|prefix| trimmed.starts_with(prefix)),
            "portable /export import is not allowed: {trimmed}"
        );
    }

    // No concrete host module or type may appear in portable production code.
    // Syntactic tokens are matched literally; bare identifiers are matched on
    // word boundaries so `Append` cannot false-trigger on `App`.
    for token in [
        "use crate::",
        "crate::tui",
        "codewhale_engine::client",
        "codewhale_runtime::config",
        "codewhale_snapshot",
        "codewhale_engine::session_manager",
        "std::fs",
        "std::net",
        "std::process",
    ] {
        assert!(
            !production.contains(token),
            "portable /export must not reference {token}"
        );
    }
    for identifier in [
        "App",
        "AppAction",
        "ClipboardHandler",
        "SnapshotRepo",
        "SessionManager",
        "HistoryCell",
        "ContentBlock",
        "OpenOptions",
        "ratatui",
        "crossterm",
    ] {
        assert!(
            !contains_identifier(&production, identifier),
            "portable /export must not reference {identifier}"
        );
    }

    // The group result is shared; no concrete host type may cross the boundary.
    assert!(
        !production.contains("super::App"),
        "portable export must not import a host type"
    );
}

#[test]
fn portable_export_source_carries_no_hidden_authority() {
    let source = include_str!("groups/session/export.rs");
    let production = portable_production_source(source);

    // Host authority must not hide behind a callback, boxed closure, erased
    // receipt, or unsafe escape hatch.
    for token in [
        "Box<",
        "dyn Fn",
        "impl Fn",
        "fn(&mut dyn",
        "&mut dyn",
        "transmute",
    ] {
        assert!(
            !production.contains(token),
            "portable /export must not contain {token}"
        );
    }
    assert!(
        !contains_identifier(&production, "unsafe"),
        "portable /export must not contain unsafe"
    );

    // Missing authority fails with the exact safe error and never panics: the
    // facet is destructured, not `.expect()`ed.
    for token in [".expect(", ".unwrap(", "panic!(", "unreachable!(", "todo!("] {
        assert!(
            !production.contains(token),
            "portable /export must not contain {token}"
        );
    }
    assert!(
        production.contains(
            "return CommandResult::error(\"Command capability unavailable: session_export\".to_string());"
        ),
        "portable /export must keep the exact safe missing-authority error"
    );
}

#[test]
fn shared_sanitizer_is_one_pure_acyclic_implementation() {
    // Both portable commands consume the single pure implementation in
    // codewhale-sanitize, independently of credential storage.
    let export_source = include_str!("groups/session/export.rs");
    let structcopy_source = include_str!("groups/session/structcopy.rs");
    assert!(
        export_source.contains("use codewhale_sanitize::sanitize::"),
        "portable /export must consume the shared sanitizer"
    );
    assert!(
        structcopy_source.contains("use codewhale_sanitize::sanitize::"),
        "/structcopy must consume the same shared sanitizer"
    );
    // The module prose must name the new owner: a stale `export::<helper>` seam
    // reference is exactly the comment drift inherited from PR #5525.
    for stale in ["export::redact_json", "export::sanitize_text"] {
        assert!(
            !structcopy_source.contains(stale),
            "/structcopy comments must not cite the removed {stale} seam"
        );
    }

    // Fast local tripwire only. The authoritative check is the graph scan in
    // `scripts/check-command-crate-boundaries.py`, which asserts that neither
    // the contract nor the sanitizer reaches host services; this
    // manifest read just fails sooner when someone edits the manifest by hand.
    let sanitizer_manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../sanitize/Cargo.toml"
    ))
    .expect("the shared sanitizer crate manifest must be readable");
    assert!(
        !sanitizer_manifest.contains("codewhale-tui"),
        "the shared sanitizer crate must not depend on codewhale-tui"
    );

    for forbidden in [
        "codewhale-secrets",
        "codewhale-core",
        "keyring",
        "dbus",
        "reqwest",
    ] {
        assert!(
            !sanitizer_manifest.contains(forbidden),
            "pure sanitizer must not import {forbidden}"
        );
    }

    // There is no second sanitizer implementation hiding in the portable slice.
    let production = portable_production_source(export_source);
    for duplicated in [
        "fn sanitize_text",
        "fn redact_json",
        "fn redact_url_for_display",
        "fn strip_ansi",
    ] {
        assert!(
            !production.contains(duplicated),
            "portable /export must not duplicate {duplicated}"
        );
    }
}

#[test]
fn host_bound_fixtures_stay_outside_the_movable_group() {
    // FEAT-043 moves `groups/session` into `codewhale-commands`. Real-host
    // fixtures and the shared recovery writer must therefore stay at the
    // `commands` root, and the portable slice must not embed host fixtures.
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/commands/session_export_regression_tests.rs",
        "src/commands/session_export_surface_tests.rs",
        "src/commands/session_export_test_support.rs",
        "src/commands/session_export_host.rs",
    ] {
        assert!(
            manifest_dir.join(relative).exists(),
            "host fixture {relative} must stay outside groups/session"
        );
    }

    // The baseline-captured export goldens are host-bound (they include the
    // host-derived metadata and redaction output only the real adapter can
    // produce), so they must live at the `commands` root with the suites that
    // consume them rather than inside the movable group.
    for relative in [
        "src/commands/fixtures/export_conversation_baseline.md",
        "src/commands/fixtures/export_turn_baseline.md",
        "src/commands/fixtures/export_history_fallback_recorded_baseline.md",
        "src/commands/fixtures/export_correlation_recorded_baseline.md",
    ] {
        assert!(
            manifest_dir.join(relative).exists(),
            "baseline golden {relative} must stay outside groups/session"
        );
    }

    let export_source = include_str!("groups/session/export.rs");
    for host_fixture in [
        "SessionExportAdapter",
        "ExportHarness",
        "tempfile",
        "ClipboardHandler",
        "SnapshotRepo",
        "HistoryCell",
    ] {
        assert!(
            !export_source.contains(host_fixture),
            "portable /export must not embed the host fixture {host_fixture}"
        );
    }
}
