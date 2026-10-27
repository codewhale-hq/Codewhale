//! Shared, host-bound test support for the FEAT-029 `debug::diagnostics` slice.
//!
//! This module lives at the `commands` root — outside `codewhale-commands` — so the public-surface and
//! host-regression suites share one harness and one normalisation contract
//! instead of drifting copies.
//!
//! The fixtures under `fixtures/diagnostics/` are reviewed command captures.
//! Context fixtures reflect an App with no installed Engine instruction receipt.
//! Only clock fields and owned temporary paths are normalised. Context
//! expectations account for the host's platform/shell in the environment block
//! and its token subtotal.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use tempfile::TempDir;

use crate::tui::app::{App, TuiOptions};
use codewhale_config::ProviderKind;

/// The shared home seal, re-exported so the diagnostics suites keep one import
/// path. Take it first in any test whose output depends on the user's state
/// (global instructions, installed skills, persisted settings), before building
/// a [`DiagnosticsHarness`]: it holds the process-wide environment lock, which
/// the harness's settings loader re-enters.
pub(crate) use crate::test_support::SealedHome;

/// Host-isolated fixture for the diagnostics command surface.
///
/// The hermetic test-home wrapper isolates persisted settings. The owned
/// workspace and skills directories keep `/context` away from project data.
/// Do not hold either test-state mutex while constructing `App`: its settings
/// loader takes that mutex internally and the lock is not reentrant.
pub(crate) struct DiagnosticsHarness {
    pub(crate) app: App,
    pub(crate) workspace: PathBuf,
    pub(crate) skills: PathBuf,
    _temp: TempDir,
}

impl DiagnosticsHarness {
    pub(crate) fn new() -> Self {
        let temp = TempDir::new().expect("diagnostics tempdir");

        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let skills = temp.path().join("skills");
        std::fs::create_dir_all(&skills).expect("skills dir");

        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: workspace.clone(),
            skills_dir: skills.clone(),
            memory_path: temp.path().join("memory.md"),
            notes_path: temp.path().join("notes.txt"),
            mcp_config_path: temp.path().join("mcp.json"),
            use_memory: false,
            ..crate::tui::test_support::test_tui_options(temp.path())
        };
        let mut app = crate::tui::test_support::test_app_with_options(options);
        app.api_provider = ProviderKind::Deepseek;

        Self {
            app,
            workspace,
            skills,
            _temp: temp,
        }
    }

    /// Apply the documented volatile-field normalisation for this harness.
    pub(crate) fn normalize(&self, raw: &str) -> String {
        normalize_generated_at(&normalize_harness_paths(
            raw,
            &self.workspace,
            &self.skills,
            &self._temp.path().join("memory.md"),
        ))
    }
}

/// Read a frozen baseline fixture shipped with the slice.
pub(crate) fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/commands/fixtures/diagnostics")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "baseline fixture {} must be present and readable: {error}",
            path.display()
        )
    })
}

/// Assert the captured command output is byte-identical to its frozen baseline.
///
/// Fixture files retain a stable `command: /...` provenance header captured by
/// the temporary baseline tool. The test identity and fixture name select that
/// command; the actual runtime value starts with `is_error`, so compare the
/// entire captured `CommandResult` body without manufacturing a header at run
/// time.
#[track_caller]
pub(crate) fn assert_fixture(name: &str, actual: &str) {
    let expected = fixture(name);
    let expected = if name.starts_with("context_") {
        context_fixture_for_host(
            &expected,
            std::env::consts::OS,
            codewhale_sandbox::shell_dispatcher::global_dispatcher()
                .kind()
                .binary(),
        )
    } else {
        expected
    };
    let (_, expected) = expected
        .split_once('\n')
        .unwrap_or_else(|| panic!("baseline fixture {name} is missing its provenance header"));
    assert_eq!(
        expected, actual,
        "baseline parity drift in {name}; captured output changed"
    );
}

/// The captured Linux environment contributes 21 tokens to the 2207 total.
/// Keep its text frozen independently of the production prompt renderer, but
/// substitute real host facts: Windows/pwsh.exe contributes 22, for example.
/// Never derive expectations from the report being tested or erase its counts.
fn context_fixture_for_host(expected: &str, platform: &str, shell: &str) -> String {
    let environment =
        format!("## Environment\n\n- lang: en\n- platform: {platform}\n- shell: {shell}");
    let tokens = environment.chars().count().div_ceil(3);
    let total = 2207 - 21 + tokens;
    expected
        .replace(
            "EnvironmentBlock: Runtime environment [<workspace>] - 21 tokens",
            &format!("EnvironmentBlock: Runtime environment [<workspace>] - {tokens} tokens"),
        )
        .replace(
            "Runtime environment (21)",
            &format!("Runtime environment ({tokens})"),
        )
        .replace(
            "\"estimated_tokens\": 21,",
            &format!("\"estimated_tokens\": {tokens},"),
        )
        .replace(
            "Source-entry total: 2207 tokens",
            &format!("Source-entry total: {total} tokens"),
        )
        .replace(
            "\"total_estimated_tokens\": 2207,",
            &format!("\"total_estimated_tokens\": {total},"),
        )
}

/// Replace the RFC-3339 `generated_at` report stamp so two captures of the same
/// source state compare byte-for-byte.
///
/// `PromptSourceMap` / `PromptContext` stamp `Utc::now()` at construction; it is
/// the only wall-clock field on the diagnostics command surface. Production
/// timestamp semantics stay untouched — tests normalise the comparison, not the
/// adapter.
pub(crate) fn normalize_generated_at(raw: &str) -> String {
    fn regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#""generated_at":\s*"[^"]*""#).expect("generated_at regex"))
    }
    regex()
        .replace_all(raw, "\"generated_at\": \"<timestamp>\"")
        .into_owned()
}

/// Replace the host-owned workspace/skills directory paths with stable
/// placeholders so the fixture does not embed a per-run temporary path.
pub(crate) fn normalize_harness_paths(
    raw: &str,
    workspace: &Path,
    skills: &Path,
    memory: &Path,
) -> String {
    fn replace_path(raw: &str, path: &str, placeholder: &str) -> String {
        let json = serde_json::to_string(path).expect("serialize fixture path");
        raw.replace(&json[1..json.len() - 1], placeholder)
            .replace(path, placeholder)
    }
    let workspace = workspace.display().to_string();
    let mut normalized = raw.to_string();
    // Path::join adds a native separator before this slash-containing suffix.
    // Replace the whole owned path first, including JSON-escaped Windows paths.
    for separator in ['/', '\\'] {
        normalized = replace_path(
            &normalized,
            &format!("{workspace}{separator}.codewhale/handoff.md"),
            "<workspace>/.codewhale/handoff.md",
        );
    }
    for (path, placeholder) in [
        (workspace, "<workspace>"),
        (skills.display().to_string(), "<skills>"),
        (memory.display().to_string(), "<memory>"),
    ] {
        normalized = replace_path(&normalized, &path, placeholder);
    }
    normalized
}

/// Replace the trailing per-turn age cell in `/cache` history rows so a slow
/// runner cannot turn `0s` into `1s` and fail an otherwise identical capture.
///
/// `humanize_age` renders `{secs}s` or `{mins}m {secs:02}s`; only that final
/// token on a telemetry row is volatile.
pub(crate) fn normalize_cache_ages(raw: &str) -> String {
    fn regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?m)(\d+m \d{2}s|\d+s)$").expect("cache-age regex"))
    }
    regex().replace_all(raw, "<age>").into_owned()
}

/// The normalisers must replace *only* the documented volatile fields. A
/// normaliser that over-matched would turn the golden comparison into a
/// tautology, so pin its exact behaviour and prove an unrelated byte change
/// still fails equality.
#[test]
fn normalisers_replace_only_documented_volatile_fields() {
    let raw = concat!(
        "{\n",
        "  \"budget_used_percent\": 0.0048,\n",
        "  \"generated_at\": \"2026-09-23T19:02:15Z\",\n",
        "  \"note\": \"stable\"\n",
        "}\n",
    );
    let normalized = normalize_generated_at(raw);
    assert!(
        normalized.contains("\"generated_at\": \"<timestamp>\""),
        "{normalized}"
    );
    assert!(!normalized.contains("2026-09-23T19:02:15Z"), "{normalized}");
    // Every other byte survives exactly.
    assert!(
        normalized.contains("\"budget_used_percent\": 0.0048"),
        "{normalized}"
    );
    assert!(normalized.contains("\"note\": \"stable\""), "{normalized}");

    // An unrelated byte change is not normalised away: it must still differ.
    let mutated = raw.replace("\"stable\"", "\"drifted\"");
    assert_ne!(
        normalize_generated_at(raw),
        normalize_generated_at(&mutated),
        "a non-clock change must still fail the golden comparison"
    );

    // Path normalisation is exact-substring and cannot swallow unrelated text.
    let paths = normalize_harness_paths(
        "pack [/tmp/abc/workspace] skills [/tmp/abc/skills] memory [/tmp/abc/memory.md]",
        Path::new("/tmp/abc/workspace"),
        Path::new("/tmp/abc/skills"),
        Path::new("/tmp/abc/memory.md"),
    );
    assert_eq!(
        paths,
        "pack [<workspace>] skills [<skills>] memory [<memory>]"
    );

    // The age normaliser touches only the trailing age cell.
    let rows = "   1  deepseek/x   4000    200   3000   1000      —       —    75.0%           —   0s\n\
                footer: sum_write: 0\n";
    let aged = normalize_cache_ages(rows);
    assert!(aged.contains("75.0%           —   <age>\n"), "{aged}");
    assert!(aged.contains("footer: sum_write: 0"), "{aged}");
}

#[test]
fn context_expectations_preserve_counts_for_each_host() {
    for name in [
        "context_report.txt",
        "context_json.txt",
        "context_summary.txt",
        "context_prompt_json.txt",
    ] {
        let baseline = fixture(name);
        assert_eq!(
            context_fixture_for_host(&baseline, "linux", "/bin/bash"),
            baseline
        );
        // The Windows environment adds one token to the captured Linux total.
        let windows = baseline
            .replace("- 21 tokens", "- 22 tokens")
            .replace("environment (21)", "environment (22)")
            .replace("\"estimated_tokens\": 21,", "\"estimated_tokens\": 22,")
            .replace("2207", "2208");
        let expected = context_fixture_for_host(&baseline, "windows", "pwsh.exe");
        assert_eq!(expected, windows, "{name}");
        // A fixture that carries no host-dependent number (the top-source list
        // can stop naming the environment block) is identical on both hosts, so
        // only the host-carrying fixtures can prove the difference is kept.
        let host_dependent = baseline.contains("- 21 tokens")
            || baseline.contains("environment (21)")
            || baseline.contains("\"estimated_tokens\": 21,");
        if host_dependent {
            assert_ne!(expected, baseline, "{name} must retain the host difference");
            assert_ne!(expected, windows.replace("22", "23"));
        }
        // A regression in another source still fails on every fixture.
        assert_ne!(expected, windows.replace("1803", "1804"));
    }
}

#[test]
fn windows_paths_normalize_in_text_and_json_without_changing_other_data() {
    let workspace = Path::new(r"C:\Users\runner\Temp\fixture\workspace");
    let skills = Path::new(r"C:\Users\runner\Temp\fixture\skills");
    let memory = Path::new(r"C:\Users\runner\Temp\fixture\memory.md");
    let handoff = format!("{}\\.codewhale/handoff.md", workspace.display());
    let raw = format!(
        "handoff [{handoff}]\n{{\"handoff\":{},\"skills\":{},\"memory\":{},\"tokens\":22,\"other\":\"C:\\\\unrelated\"}}",
        serde_json::to_string(&handoff).unwrap(),
        serde_json::to_string(&skills.display().to_string()).unwrap(),
        serde_json::to_string(&memory.display().to_string()).unwrap(),
    );
    assert_eq!(
        normalize_harness_paths(&raw, workspace, skills, memory),
        "handoff [<workspace>/.codewhale/handoff.md]\n{\"handoff\":\"<workspace>/.codewhale/handoff.md\",\"skills\":\"<skills>\",\"memory\":\"<memory>\",\"tokens\":22,\"other\":\"C:\\\\unrelated\"}"
    );
}
