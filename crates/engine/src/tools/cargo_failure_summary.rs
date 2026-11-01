//! Compact summaries for Cargo failures.
//!
//! Cargo output can be large and noisy. This module extracts stable failure
//! signals for tool metadata so context compaction can preserve the actionable
//! lines without re-running `cargo test | tail`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_ITEMS: usize = 8;
const MAX_SUMMARY_CHARS: usize = 1_200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoFailureKind {
    TestFailure,
    CompileError,
    CargoFailure,
}

impl CargoFailureKind {
    fn label(&self) -> &'static str {
        match self {
            Self::TestFailure => "test_failure",
            Self::CompileError => "compile_error",
            Self::CargoFailure => "cargo_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CargoFailureSummary {
    pub kind: CargoFailureKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing_tests: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panic_locations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_error: Option<String>,
}

impl CargoFailureSummary {
    pub fn to_metadata_value(&self) -> Value {
        json!(self)
    }
}

pub fn summarize_cargo_failure(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> Option<CargoFailureSummary> {
    if exit_code == Some(0) || !looks_like_cargo_command(command) {
        return None;
    }

    let mut failing_tests = Vec::new();
    let mut error_codes = Vec::new();
    let mut primary_errors = Vec::new();
    let mut panic_locations = Vec::new();
    let mut test_result = None;
    let mut final_error = None;

    for line in stderr.lines().chain(stdout.lines()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(test) = parse_failed_test_line(trimmed) {
            push_unique_limited(&mut failing_tests, test);
        }
        if let Some(test) = parse_failure_header(trimmed) {
            push_unique_limited(&mut failing_tests, test);
        }
        if let Some(code) = parse_error_code(trimmed) {
            push_unique_limited(&mut error_codes, code);
        }
        if is_primary_error_line(trimmed) {
            push_unique_limited(&mut primary_errors, trimmed.to_string());
        }
        if trimmed.contains("panicked at ") {
            push_unique_limited(&mut panic_locations, trimmed.to_string());
        }
        if trimmed.starts_with("test result:") {
            test_result = Some(trimmed.to_string());
        }
        if trimmed.starts_with("error: could not compile")
            || trimmed.starts_with("error: aborting due to")
            || trimmed.starts_with("error: test failed")
        {
            final_error = Some(trimmed.to_string());
        }
    }

    let kind = classify_failure(&failing_tests, &primary_errors, test_result.as_deref());
    if !has_actionable_signal(
        &failing_tests,
        &error_codes,
        &primary_errors,
        &panic_locations,
        test_result.as_deref(),
        final_error.as_deref(),
    ) {
        return None;
    }
    let summary = build_summary(
        &kind,
        &failing_tests,
        &error_codes,
        &primary_errors,
        &panic_locations,
        test_result.as_deref(),
        final_error.as_deref(),
    );

    Some(CargoFailureSummary {
        kind,
        summary,
        failing_tests,
        error_codes,
        primary_errors,
        panic_locations,
        test_result,
        final_error,
    })
}

fn looks_like_cargo_command(command: &str) -> bool {
    let Some(tokens) = shlex::split(command) else {
        return false;
    };

    let mut expect_command = true;
    for (idx, raw_token) in tokens.iter().enumerate() {
        let token = normalize_shell_token(raw_token);
        if token.is_empty() {
            continue;
        }
        if is_shell_separator(token) {
            expect_command = true;
            continue;
        }
        if !expect_command {
            continue;
        }
        if looks_like_env_assignment(token) {
            continue;
        }
        if is_cargo_binary(token) {
            return cargo_subcommand(&tokens[idx + 1..]).is_some();
        }
        expect_command = false;
    }

    false
}

fn parse_failed_test_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("test ")?;
    let (name, status) = rest.rsplit_once(" ... ")?;
    (status == "FAILED").then(|| name.trim().to_string())
}

fn parse_failure_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("---- ")?;
    let name = rest.strip_suffix(" stdout ----")?;
    Some(name.trim().to_string())
}

fn parse_error_code(line: &str) -> Option<String> {
    let rest = line.strip_prefix("error[")?;
    let (code, _) = rest.split_once("]")?;
    Some(code.to_string())
}

fn is_primary_error_line(line: &str) -> bool {
    line.starts_with("error[")
        || (line.starts_with("error:") && !line.starts_with("error: test failed"))
}

fn classify_failure(
    failing_tests: &[String],
    primary_errors: &[String],
    test_result: Option<&str>,
) -> CargoFailureKind {
    if !failing_tests.is_empty()
        || test_result.is_some_and(|line| line.to_ascii_lowercase().contains("failed"))
    {
        CargoFailureKind::TestFailure
    } else if !primary_errors.is_empty() {
        CargoFailureKind::CompileError
    } else {
        CargoFailureKind::CargoFailure
    }
}

fn has_actionable_signal(
    failing_tests: &[String],
    error_codes: &[String],
    primary_errors: &[String],
    panic_locations: &[String],
    test_result: Option<&str>,
    final_error: Option<&str>,
) -> bool {
    !failing_tests.is_empty()
        || !error_codes.is_empty()
        || !primary_errors.is_empty()
        || !panic_locations.is_empty()
        || test_result.is_some()
        || final_error.is_some()
}

fn build_summary(
    kind: &CargoFailureKind,
    failing_tests: &[String],
    error_codes: &[String],
    primary_errors: &[String],
    panic_locations: &[String],
    test_result: Option<&str>,
    final_error: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Cargo failure kind: {}.", kind.label()));
    if !failing_tests.is_empty() {
        lines.push(format!("Failing tests: {}.", failing_tests.join(", ")));
    }
    if !error_codes.is_empty() {
        lines.push(format!("Rust error codes: {}.", error_codes.join(", ")));
    }
    if let Some(line) = primary_errors.first() {
        lines.push(format!("Primary error: {line}"));
    }
    if let Some(line) = panic_locations.first() {
        lines.push(format!("Panic: {line}"));
    }
    if let Some(line) = test_result {
        lines.push(line.to_string());
    }
    if let Some(line) = final_error {
        lines.push(line.to_string());
    }
    truncate_chars(&lines.join("\n"), MAX_SUMMARY_CHARS)
}

fn normalize_shell_token(token: &str) -> &str {
    token.trim_matches(|ch| matches!(ch, '(' | ')' | '{' | '}'))
}

fn is_shell_separator(token: &str) -> bool {
    matches!(token, "&&" | "||" | ";" | "|")
}

fn looks_like_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        && !name.as_bytes()[0].is_ascii_digit()
}

fn is_cargo_binary(token: &str) -> bool {
    let name = token.rsplit(['/', '\\']).next().unwrap_or(token);
    name.eq_ignore_ascii_case("cargo") || name.eq_ignore_ascii_case("cargo.exe")
}

fn cargo_subcommand(tokens: &[String]) -> Option<&str> {
    let mut idx = 0;
    while let Some(raw_token) = tokens.get(idx) {
        let token = normalize_shell_token(raw_token);
        if token.is_empty() {
            idx += 1;
            continue;
        }
        if is_shell_separator(token) {
            return None;
        }
        if token.starts_with('+') {
            idx += 1;
            continue;
        }
        if token.starts_with('-') {
            if cargo_global_flag_takes_value(token) {
                idx += 2;
            } else {
                idx += 1;
            }
            continue;
        }
        return is_supported_cargo_subcommand(token).then_some(token);
    }
    None
}

fn cargo_global_flag_takes_value(token: &str) -> bool {
    if token.contains('=') {
        return false;
    }
    matches!(
        token,
        "--color"
            | "--config"
            | "-C"
            | "--jobs"
            | "-j"
            | "--lockfile-path"
            | "--manifest-path"
            | "--message-format"
            | "--package"
            | "-p"
            | "--target"
            | "--target-dir"
            | "-Z"
    )
}

fn is_supported_cargo_subcommand(token: &str) -> bool {
    matches!(
        token,
        "test" | "check" | "build" | "clippy" | "run" | "t" | "c" | "b" | "r"
    )
}

fn push_unique_limited(target: &mut Vec<String>, value: String) {
    if target.len() >= MAX_ITEMS || target.iter().any(|existing| existing == &value) {
        return;
    }
    target.push(value);
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if let Some((idx, _)) = text.char_indices().nth(max_chars) {
        if max_chars < 3 {
            return text[..idx].to_string();
        }
        let truncate_at = text
            .char_indices()
            .nth(max_chars - 3)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        format!("{}...", &text[..truncate_at])
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_failed_libtest_output() {
        let stdout = r"
running 1 test
test tests::fails ... FAILED

failures:

---- tests::fails stdout ----
thread 'tests::fails' panicked at src/lib.rs:7:9:
assertion `left == right` failed

test result: FAILED. 0 passed; 1 failed; 0 ignored; finished in 0.00s
";
        let summary =
            summarize_cargo_failure("cargo test", stdout, "", Some(101)).expect("summary");

        assert_eq!(summary.kind, CargoFailureKind::TestFailure);
        assert_eq!(summary.failing_tests, vec!["tests::fails"]);
        assert!(summary.summary.contains("Failing tests: tests::fails"));
        assert!(summary.test_result.unwrap().contains("1 failed"));
    }

    #[test]
    fn summarizes_rustc_compile_error() {
        let stderr = r#"
error[E0308]: mismatched types
  --> src/lib.rs:2:5
   |
2  |     "" 
   |     ^^ expected `i32`, found `&str`
error: could not compile `demo` (lib) due to 1 previous error
"#;
        let summary =
            summarize_cargo_failure("cargo check", "", stderr, Some(101)).expect("summary");

        assert_eq!(summary.kind, CargoFailureKind::CompileError);
        assert_eq!(summary.error_codes, vec!["E0308"]);
        assert!(summary.primary_errors[0].contains("mismatched types"));
        assert!(summary.final_error.unwrap().contains("could not compile"));
    }

    #[test]
    fn recognizes_cargo_aliases_and_uncoded_errors() {
        let stderr = "error: cannot find value `missing` in this scope\n";
        let summary = summarize_cargo_failure("cargo c", "", stderr, Some(101)).expect("summary");

        assert_eq!(summary.kind, CargoFailureKind::CompileError);
        assert_eq!(
            summary.primary_errors,
            vec!["error: cannot find value `missing` in this scope"]
        );
    }

    #[test]
    fn recognizes_tokenized_cargo_invocations() {
        assert!(
            summarize_cargo_failure(
                "cargo +nightly --manifest-path demo/Cargo.toml test",
                "test tests::fails ... FAILED\n",
                "",
                Some(101),
            )
            .is_some()
        );
        assert!(
            summarize_cargo_failure(
                "DEMO=1 cargo --locked run",
                "",
                "error: process didn't exit successfully\n",
                Some(101),
            )
            .is_some()
        );
        assert!(
            summarize_cargo_failure(
                "echo cargo test && false",
                "test tests::fails ... FAILED\n",
                "",
                Some(1),
            )
            .is_none()
        );
    }

    #[test]
    fn skips_generic_cargo_failure_without_actionable_signal() {
        assert!(
            summarize_cargo_failure("cargo test", "build failed", "command failed", Some(1))
                .is_none()
        );
    }

    #[test]
    fn truncate_chars_respects_tiny_limits() {
        assert_eq!(truncate_chars("abcdef", 0), "");
        assert_eq!(truncate_chars("abcdef", 1), "a");
        assert_eq!(truncate_chars("abcdef", 2), "ab");
        assert_eq!(truncate_chars("abcdef", 3), "...");
        assert_eq!(truncate_chars("abcdef", 4), "a...");
    }

    #[test]
    fn ignores_successful_or_non_cargo_commands() {
        assert!(summarize_cargo_failure("cargo test", "", "", Some(0)).is_none());
        assert!(summarize_cargo_failure("npm test", "failed", "", Some(1)).is_none());
    }
}
