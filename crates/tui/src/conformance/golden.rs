//! Fixture discovery, golden comparison, the update mode, and the masking
//! rules every family shares. Masking is deliberately small and named: a mask
//! hides a fact that genuinely varies between hosts or runs (temp paths,
//! UUIDs, clocks), never a fact the migration could change by accident.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::test_support::{EnvVarGuard, TestEnvLock, lock_test_env};
pub(super) use codewhale_engine::test_support::{UPDATE_ENV, check_golden};
use codewhale_engine::tools::spec::ToolError;

pub(super) fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("conformance")
}

pub(super) fn family_dir(family: &str) -> PathBuf {
    fixture_root().join(family)
}

/// Case names (`<name>.case.json`) in one family, sorted. Panics on an empty
/// family: a filter that silently runs zero cases is not a pass.
pub(super) fn case_names(family: &str) -> Vec<String> {
    let dir = family_dir(family);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read fixture dir {}: {error}", dir.display()))
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.strip_suffix(".case.json").map(str::to_string)
        })
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "conformance family `{family}` has no *.case.json fixtures in {}",
        dir.display()
    );
    names
}

pub(super) fn read_case(family: &str, name: &str) -> Value {
    let path = family_dir(family).join(format!("{name}.case.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// A harness deadline is missing evidence, never a provider outcome or a
/// recordable golden. Keep this boundary shared by every asynchronous family.
pub(super) async fn complete_within<T>(
    operation: &str,
    deadline: std::time::Duration,
    future: impl std::future::Future<Output = T>,
) -> Result<T, String> {
    tokio::time::timeout(deadline, future)
        .await
        .map_err(|_| format!("harness timeout: {operation} did not complete within {deadline:?}"))
}

/// Rebuild every object with keys in sorted order. Used where map order is an
/// implementation accident (HashMap-backed metadata); the prompt family does
/// not use it, because there key order is part of the cached prefix bytes.
pub(super) fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// One compact JSON document per line, trailing newline.
pub(super) fn jsonl(lines: &[Value]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(&serde_json::to_string(line).expect("serialize golden line"));
        out.push('\n');
    }
    out
}

pub(super) fn pretty(value: &Value) -> String {
    let mut out = serde_json::to_string_pretty(value).expect("serialize golden");
    out.push('\n');
    out
}

/// Replaces host- and run-specific substrings in every string of a JSON tree.
///
/// - literal path prefixes (workspace, home) → `<WORKSPACE>` / `<HOME>`,
///   including their canonicalized spellings (`/private/var` on macOS);
/// - UUIDs → `<uuid:N>`, numbered by first appearance so that two events
///   naming the same id still visibly agree;
/// - RFC 3339 timestamps → `<timestamp>` (applied first);
/// - values of the named volatile keys (durations, clocks) → `"<masked>"`.
pub(super) struct Masker {
    literals: Vec<(String, String)>,
    volatile_keys: &'static [&'static str],
    uuids: HashMap<String, String>,
    uuid_re: regex::Regex,
    timestamp_re: regex::Regex,
}

impl Masker {
    pub(super) fn new(volatile_keys: &'static [&'static str]) -> Self {
        Self {
            literals: Vec::new(),
            volatile_keys,
            uuids: HashMap::new(),
            uuid_re: regex::Regex::new(
                r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
            )
            .expect("uuid regex"),
            timestamp_re: regex::Regex::new(
                r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})",
            )
            .expect("timestamp regex"),
        }
    }

    /// Mask `path` (and its canonical spelling) as `label`. Longer literals
    /// are applied first so a home nested in a workspace cannot half-match.
    pub(super) fn path(mut self, path: &Path, label: &str) -> Self {
        let mut spellings = vec![path.to_string_lossy().into_owned()];
        if let Ok(canonical) = path.canonicalize() {
            spellings.push(canonical.to_string_lossy().into_owned());
        }
        for spelling in spellings {
            if !spelling.is_empty() && !self.literals.iter().any(|(known, _)| *known == spelling) {
                self.literals.push((spelling, label.to_string()));
            }
        }
        self.literals
            .sort_by(|(left, _), (right, _)| right.len().cmp(&left.len()).then(left.cmp(right)));
        self
    }

    /// Mask an arbitrary literal (a random session id, say).
    pub(super) fn literal(mut self, literal: &str, label: &str) -> Self {
        if !literal.is_empty() {
            self.literals.push((literal.to_string(), label.to_string()));
            self.literals.sort_by(|(left, _), (right, _)| {
                right.len().cmp(&left.len()).then(left.cmp(right))
            });
        }
        self
    }

    pub(super) fn text(&mut self, text: &str) -> String {
        // Timestamps first: a literal (today's date, say) must not split one.
        let mut out = self
            .timestamp_re
            .replace_all(text, "<timestamp>")
            .into_owned();
        for (literal, label) in &self.literals {
            if out.contains(literal.as_str()) {
                out = out.replace(literal.as_str(), label);
            }
        }
        let uuids = &mut self.uuids;
        self.uuid_re
            .replace_all(&out, |captures: &regex::Captures<'_>| {
                let raw = captures[0].to_ascii_lowercase();
                let next = uuids.len() + 1;
                uuids
                    .entry(raw)
                    .or_insert_with(|| format!("<uuid:{next}>"))
                    .clone()
            })
            .into_owned()
    }

    pub(super) fn value(&mut self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.text(text),
            Value::Array(items) => {
                for item in items {
                    self.value(item);
                }
            }
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if self.volatile_keys.contains(&key.as_str()) && !item.is_null() {
                        *item = Value::String("<masked>".to_string());
                    } else {
                        self.value(item);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Stable snake_case name of a `ToolError` variant. The golden also pins its
/// full detail bytes; migration does not weaken either part of the contract.
pub(super) fn tool_error_kind(error: &ToolError) -> &'static str {
    match error {
        ToolError::InvalidInput { .. } => "invalid_input",
        ToolError::MissingField { .. } => "missing_field",
        ToolError::PathEscape { .. } => "path_escape",
        ToolError::ExecutionFailed { .. } => "execution_failed",
        ToolError::Timeout { .. } => "timeout",
        ToolError::Cancelled { .. } => "cancelled",
        ToolError::NotAvailable { .. } => "not_available",
        ToolError::PermissionDenied { .. } => "permission_denied",
    }
}

/// A hermetic home + workspace for one case. Holds the process test
/// env lock for its lifetime; field order is drop order (guards restore the
/// environment before the lock is released).
pub(super) struct Sandbox {
    _guards: Vec<EnvVarGuard>,
    _lock: TestEnvLock,
    pub(super) home: std::path::PathBuf,
    pub(super) workspace: std::path::PathBuf,
    root: tempfile::TempDir,
}

impl Sandbox {
    pub(super) fn new(case: &Value) -> Self {
        let lock = lock_test_env();
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let guards = vec![
            EnvVarGuard::set("HOME", &home),
            EnvVarGuard::set("USERPROFILE", &home),
            EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale")),
            // Model-visible host facts that would otherwise follow the
            // developer's shell and locale.
            EnvVarGuard::set("SHELL", "/bin/bash"),
            EnvVarGuard::set("LC_ALL", "en_US.UTF-8"),
            EnvVarGuard::set("LANG", "en_US.UTF-8"),
            EnvVarGuard::remove("LC_MESSAGES"),
        ];
        write_workspace(&workspace, case);
        if case["trusted_workspace"].as_bool() == Some(true) {
            // Repository instructions, commands and skills load only here.
            crate::test_support::trust_workspace(&workspace);
        }
        Self {
            _guards: guards,
            _lock: lock,
            home,
            workspace,
            root,
        }
    }

    pub(super) fn masker(&self, volatile_keys: &'static [&'static str]) -> Masker {
        // `<turn_meta>` states the local date; it is a clock, not a contract.
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        Masker::new(volatile_keys)
            .path(&self.workspace, "<WORKSPACE>")
            .path(&self.home, "<HOME>")
            .path(self.root.path(), "<TMP>")
            .literal(&today, "<today>")
    }
}

pub(super) fn write_workspace(workspace: &Path, case: &Value) {
    if let Some(files) = case.get("workspace_files").and_then(Value::as_object) {
        for (relative, content) in files {
            let path = workspace.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture dir");
            }
            std::fs::write(
                &path,
                content.as_str().expect("workspace file content is text"),
            )
            .expect("write fixture file");
        }
    }
}

/// Collects per-case failures so one run reports every drifted case.
#[derive(Default)]
pub(super) struct Failures(Vec<String>);

impl Failures {
    pub(super) fn contains(&self, message: &str) -> bool {
        self.0.iter().any(|failure| failure.contains(message))
    }
    pub(super) fn record(&mut self, case: &str, result: Result<(), String>) {
        if let Err(message) = result {
            self.0.push(format!("[{case}] {message}"));
        }
    }

    pub(super) fn push(&mut self, case: &str, message: impl Into<String>) {
        self.0.push(format!("[{case}] {}", message.into()));
    }

    pub(super) fn finish(self, family: &str, cases: usize) {
        assert!(cases > 0, "conformance family `{family}` ran no cases");
        assert!(
            self.0.is_empty(),
            "conformance family `{family}`: {} failure(s) across {cases} case(s)\n\n{}",
            self.0.len(),
            self.0.join("\n\n")
        );
        eprintln!("conformance family `{family}`: {cases} case(s) matched");
    }
}

#[test]
fn golden_comparison_rejects_changed_bytes_and_missing_output() {
    let _lock = lock_test_env();
    let _update = EnvVarGuard::set(UPDATE_ENV, "0");
    let dir = tempfile::tempdir().expect("temporary golden");
    let path = dir.path().join("case.golden.txt");
    std::fs::write(&path, "expected\n").expect("write golden");
    assert!(check_golden(&path, "expected\n").is_ok());
    assert!(
        check_golden(&path, "different\n")
            .unwrap_err()
            .contains("golden drift")
    );
    assert!(check_golden(&path, "expected\r\n").is_err());
    assert!(check_golden(&dir.path().join("missing"), "").is_err());
    // A CRLF golden must not be silently normalized either.
    std::fs::write(&path, "expected\r\n").expect("write CRLF golden");
    assert!(check_golden(&path, "expected\n").is_err());

    // A large snapshot's common prefix must not hide its actual differing
    // field; the comparison still rejects the entire changed document.
    let prefix = "x".repeat(700);
    let expected = jsonl(&[serde_json::json!({"prefix": prefix, "tail": "old"})]);
    let actual = jsonl(&[serde_json::json!({"prefix": prefix, "tail": "new"})]);
    std::fs::write(&path, expected).expect("write long JSON golden");
    let diagnostic = check_golden(&path, &actual).unwrap_err();
    assert!(diagnostic.contains("\"tail\":\"old\""));
    assert!(diagnostic.contains("\"tail\":\"new\""));
}

#[test]
#[should_panic(expected = "ran no cases")]
fn empty_family_cannot_pass() {
    Failures::default().finish("empty_control", 0);
}

#[test]
#[should_panic(expected = "is refused under CI")]
fn update_mode_is_refused_under_ci_even_for_matching_bytes() {
    let _lock = lock_test_env();
    let _update = EnvVarGuard::set(UPDATE_ENV, "1");
    let _ci = EnvVarGuard::set("CI", "1");
    let dir = tempfile::tempdir().expect("temporary golden");
    let path = dir.path().join("matching.golden.txt");
    std::fs::write(&path, "matching\n").expect("write golden");
    let _ = check_golden(&path, "matching\n");
}
