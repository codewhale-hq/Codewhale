use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::runtime_threads::turn_artifacts::{FileChangeKind, TurnArtifactKind, TurnArtifactRef};

const MAX_MARKDOWN_READ_BYTES: usize = 64 * 1024;
const MAX_MARKDOWN_PREVIEW_CHARS: usize = 8_000;
const MAX_MAINTENANCE_OUTPUT_CHARS: usize = 4_000;
const MAX_SUMMARY_CHARS: usize = 320;
const MAX_REASON_CHARS: usize = 280;

pub struct WhaleCardProducerInput<'a> {
    pub tool_name: &'a str,
    pub tool_input: Option<&'a Value>,
    pub detail: &'a str,
    pub is_error: bool,
    pub metadata: &'a serde_json::Map<String, Value>,
    pub artifacts: &'a [TurnArtifactRef],
    pub workspace_roots: &'a [PathBuf],
    pub secrets: &'a [String],
}

pub fn attach_whale_card_metadata(input: WhaleCardProducerInput<'_>) -> Option<Value> {
    if input.metadata.contains_key("whale_card") {
        return None;
    }
    if !input.is_error
        && let Some(card) = produce_markdown_artifact_card(&input)
    {
        return Some(card);
    }
    if input.is_error {
        if let Some(card) = produce_connection_required_card(&input) {
            return Some(card);
        }
        if let Some(card) = produce_maintenance_warning_card(&input) {
            return Some(card);
        }
    }
    None
}

pub(crate) fn redact_card_text(raw: &str, secrets: &[String]) -> String {
    let mut exact_masked = raw.to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            exact_masked = exact_masked.replace(secret, codewhale_config::persistence::REDACTED);
        }
    }
    let inline_masked = redact_inline_tokens(&exact_masked);
    codewhale_config::persistence::redact_secrets(&inline_masked)
}

fn redact_inline_tokens(input: &str) -> String {
    let mut masked_urls = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(scheme_idx) = rest.find("://") {
        let after_scheme = &rest[scheme_idx + 3..];
        let authority_end = after_scheme
            .find(|ch: char| ch == '/' || ch.is_whitespace() || ch == '\'' || ch == '"')
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        if let Some(at_idx) = authority.rfind('@') {
            masked_urls.push_str(&rest[..scheme_idx + 3]);
            masked_urls.push_str("[REDACTED]@");
            masked_urls.push_str(&authority[at_idx + 1..]);
            rest = &after_scheme[authority_end..];
        } else {
            masked_urls.push_str(&rest[..scheme_idx + 3 + authority_end]);
            rest = &after_scheme[authority_end..];
        }
    }
    masked_urls.push_str(rest);

    let bearer_masked = redact_bearer_tokens(&masked_urls);

    let mut out = String::with_capacity(bearer_masked.len());
    let mut idx = 0;
    let bytes = bearer_masked.as_bytes();
    while idx < bytes.len() {
        if let Some(token_len) = secret_token_len(&bearer_masked[idx..]) {
            let prev_ok = idx == 0 || !is_token_char(bytes[idx - 1]);
            if prev_ok {
                out.push_str("[REDACTED]");
                idx += token_len;
                continue;
            }
        }
        let ch = bearer_masked[idx..].chars().next().expect("valid utf-8");
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn redact_bearer_tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if idx + 6 <= bytes.len()
            && bytes[idx..idx + 6].eq_ignore_ascii_case(b"bearer")
            && (idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric())
        {
            let ws_start = idx + 6;
            let mut ws_end = ws_start;
            while ws_end < bytes.len() && (bytes[ws_end] == b' ' || bytes[ws_end] == b'\t') {
                ws_end += 1;
            }
            if ws_end > ws_start {
                let mut tok_end = ws_end;
                while tok_end < bytes.len() && is_bearer_value_byte(bytes[tok_end]) {
                    tok_end += 1;
                }
                while tok_end > ws_end && matches!(bytes[tok_end - 1], b'.' | b',' | b';' | b':') {
                    tok_end -= 1;
                }
                let tok = &input[ws_end..tok_end];
                if tok.len() >= 6 && tok != "[REDACTED]" && tok != "[redacted]" {
                    out.push_str(&input[idx..ws_end]);
                    out.push_str("[REDACTED]");
                    idx = tok_end;
                    continue;
                }
            }
        }
        let ch = input[idx..].chars().next().expect("valid utf-8");
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn is_bearer_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~' | b'+' | b'/' | b'=')
}

fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || byte == b'.'
}

fn secret_token_len(slice: &str) -> Option<usize> {
    for (prefix, min_suffix) in [
        ("cwc_refresh_", 8usize),
        ("cwc_key_", 8),
        ("github_pat_", 10),
        ("sk-", 10),
        ("sk_", 10),
        ("ghp_", 12),
        ("gho_", 12),
        ("ghu_", 12),
        ("ghs_", 12),
        ("ghr_", 12),
        ("xoxb-", 10),
        ("xoxa-", 10),
        ("xoxp-", 10),
        ("xoxr-", 10),
        ("xoxs-", 10),
    ] {
        if slice.starts_with(prefix) {
            let len = slice
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
                .count();
            if len >= prefix.len() + min_suffix {
                return Some(len);
            }
        }
    }
    if slice.starts_with("AKIA") {
        let len = slice
            .bytes()
            .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            .count();
        if len >= 20 {
            return Some(20);
        }
    }
    if slice.starts_with("eyJ") {
        let len = slice.bytes().take_while(|b| is_token_char(*b)).count();
        let candidate = &slice[..len];
        if let Some((first, second)) = candidate.split_once('.')
            && first.len() >= 15
            && second.len() >= 12
        {
            return Some(len);
        }
    }
    None
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

pub(crate) fn is_write_or_edit_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("write") || lower.contains("edit") || lower.contains("patch")
}

fn is_markdown_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn extract_markdown_title(markdown: &str, path: &str) -> String {
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let heading = rest.trim();
            if !heading.is_empty() {
                return truncate_chars(heading, 120);
            }
        }
    }
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, 120))
        .unwrap_or_else(|| "Report.md".to_owned())
}

fn read_confined_markdown(workspace_roots: &[PathBuf], rel_path: &str) -> Option<String> {
    for root in workspace_roots {
        let full = root.join(rel_path);
        if let Ok(file) = codewhale_util::fs_confined::open_read(root, &full) {
            let mut buf = Vec::new();
            if file
                .take((MAX_MARKDOWN_READ_BYTES + 1) as u64)
                .read_to_end(&mut buf)
                .is_ok()
            {
                if buf.len() > MAX_MARKDOWN_READ_BYTES {
                    buf.truncate(MAX_MARKDOWN_READ_BYTES);
                }
                return Some(String::from_utf8_lossy(&buf).into_owned());
            }
        }
    }
    None
}

fn produce_markdown_artifact_card(input: &WhaleCardProducerInput<'_>) -> Option<Value> {
    if !is_write_or_edit_tool(input.tool_name) {
        return None;
    }
    let artifact_path = input
        .artifacts
        .iter()
        .find(|art| {
            art.kind == TurnArtifactKind::File
                && art.change != Some(FileChangeKind::Deleted)
                && is_markdown_path(&art.path)
        })
        .map(|art| art.path.clone())
        .or_else(|| {
            let raw_path = input.tool_input.and_then(|val| {
                val.get("path")
                    .or_else(|| val.get("file_path"))
                    .or_else(|| val.get("target_file"))
                    .and_then(Value::as_str)
            })?;
            if !is_markdown_path(raw_path) {
                return None;
            }
            if input.workspace_roots.is_empty() {
                return (!raw_path.contains("..") && !Path::new(raw_path).is_absolute())
                    .then(|| raw_path.to_owned());
            }
            crate::runtime_threads::turn_artifacts::confined_workspace_path(
                input.workspace_roots,
                raw_path,
            )
        })?;

    let raw_markdown =
        read_confined_markdown(input.workspace_roots, &artifact_path).or_else(|| {
            input.tool_input.and_then(|val| {
                val.get("content")
                    .or_else(|| val.get("markdown"))
                    .or_else(|| val.get("replacement"))
                    .or_else(|| val.get("new_string"))
                    .or_else(|| val.get("new_str"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        })?;

    let redacted = redact_card_text(raw_markdown.trim(), input.secrets);
    if redacted.is_empty() {
        return None;
    }
    let word_count = redacted.split_whitespace().count();
    let bounded = truncate_chars(&redacted, MAX_MARKDOWN_PREVIEW_CHARS);
    let title = redact_card_text(
        &extract_markdown_title(&bounded, &artifact_path),
        input.secrets,
    );
    let clean_path = truncate_chars(&redact_card_text(&artifact_path, input.secrets), 200);
    Some(json!({
        "kind": "markdown_artifact",
        "title": title,
        "path": clean_path,
        "markdown": bounded,
        "wordCount": word_count,
    }))
}

fn extract_shell_command(input: &WhaleCardProducerInput<'_>) -> Option<String> {
    if let Some(cmd) = input.metadata.get("command").and_then(Value::as_str)
        && !cmd.trim().is_empty()
    {
        return Some(cmd.trim().to_owned());
    }
    if let Some(tool_input) = input.tool_input {
        for key in ["command", "cmd", "script"] {
            if let Some(cmd) = tool_input.get(key).and_then(Value::as_str)
                && !cmd.trim().is_empty()
            {
                return Some(cmd.trim().to_owned());
            }
        }
        if input.tool_name.eq_ignore_ascii_case("git")
            && let Some(sub) = tool_input
                .get("subcommand")
                .or_else(|| tool_input.get("operation"))
                .or_else(|| tool_input.get("action"))
                .and_then(Value::as_str)
        {
            return Some(format!("git {}", sub.trim()));
        }
    }
    None
}

fn is_repo_maintenance_command(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    for segment in lower.split([';', '&', '|', '\n']) {
        let trimmed = segment.trim();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let Some(&first) = tokens.first() else {
            continue;
        };
        let bin = first.rsplit('/').next().unwrap_or(first);
        let is_maintenance = matches!(
            (bin, tokens.get(1).copied(), tokens.get(2).copied()),
            (
                "git",
                Some("pull" | "fetch" | "clone" | "submodule" | "worktree"),
                _
            ) | (
                "npm" | "pnpm" | "yarn" | "bun",
                Some("ci" | "install" | "i" | "frozen-lockfile"),
                _
            ) | ("cargo", Some("fetch"), _)
                | ("pip" | "pip3" | "poetry" | "bundle", Some("install"), _)
                | ("uv", Some("sync"), _)
                | ("uv", Some("pip"), Some("install"))
        );
        if is_maintenance {
            return true;
        }
    }
    false
}

fn detect_missing_tools(output: &str) -> Vec<String> {
    let mut missing = Vec::new();
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(pos) = lower.find("command not found") {
            let prefix = &line[..pos];
            let candidate = prefix
                .trim_end_matches([':', ' '])
                .rsplit([':', ' '])
                .next()
                .unwrap_or("")
                .trim();
            if !candidate.is_empty()
                && candidate.len() <= 64
                && candidate
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && !missing.iter().any(|existing| existing == candidate)
            {
                missing.push(candidate.to_owned());
            }
        }
    }
    missing
}

fn detect_stale_lockfiles(output: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = output.to_ascii_lowercase();
    for lockfile in [
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lockb",
        "Cargo.lock",
        "poetry.lock",
        "uv.lock",
    ] {
        if lower.contains(&lockfile.to_ascii_lowercase())
            && (lower.contains("frozen")
                || lower.contains("out of date")
                || lower.contains("outdated")
                || lower.contains("lockfile")
                || lower.contains("in sync"))
        {
            out.push(lockfile.to_owned());
        }
    }
    out
}

fn produce_maintenance_warning_card(input: &WhaleCardProducerInput<'_>) -> Option<Value> {
    let lower_tool = input.tool_name.to_ascii_lowercase();
    let is_shell_or_git = lower_tool.contains("shell")
        || lower_tool == "bash"
        || lower_tool == "git"
        || lower_tool.starts_with("git_");
    if !is_shell_or_git {
        return None;
    }
    let exit_code = input
        .metadata
        .get("exit_code")
        .and_then(Value::as_i64)
        .filter(|code| *code != 0)?;
    let command = extract_shell_command(input)?;
    if !is_repo_maintenance_command(&command) {
        return None;
    }

    let env_name = input
        .workspace_roots
        .first()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Workspace");
    let redacted_env = truncate_chars(&redact_card_text(env_name, input.secrets), 80);
    let redacted_cmd = truncate_chars(&redact_card_text(&command, input.secrets), 240);
    let redacted_output = truncate_chars(
        &redact_card_text(input.detail.trim(), input.secrets),
        MAX_MAINTENANCE_OUTPUT_CHARS,
    );
    let first_line = redacted_output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Repository maintenance command failed");
    let summary = truncate_chars(
        &format!("`{redacted_cmd}` exited with code {exit_code}: {first_line}"),
        MAX_SUMMARY_CHARS,
    );
    let missing_tools = detect_missing_tools(&redacted_output);
    let stale_lockfiles = detect_stale_lockfiles(&redacted_output);

    Some(json!({
        "kind": "maintenance_warning",
        "environmentName": redacted_env,
        "command": redacted_cmd,
        "exitCode": exit_code,
        "output": redacted_output,
        "summary": summary,
        "missingTools": missing_tools,
        "staleLockfiles": stale_lockfiles,
    }))
}

fn is_builtin_mcp_helper_tool(name: &str) -> bool {
    matches!(
        name,
        "mcp_read_resource"
            | "mcp_get_prompt"
            | "list_mcp_resources"
            | "list_mcp_resource_templates"
            | "read_mcp_resource"
    )
}

fn extract_connector_name(input: &WhaleCardProducerInput<'_>) -> Option<String> {
    for key in [
        "connector",
        "mcp_server",
        "server",
        "server_name",
        "provider",
    ] {
        if let Some(val) = input.metadata.get(key).and_then(Value::as_str)
            && !val.trim().is_empty()
        {
            return Some(val.trim().to_owned());
        }
    }
    if (is_builtin_mcp_helper_tool(input.tool_name) || input.tool_name.starts_with("mcp_"))
        && let Some(tool_input) = input.tool_input
    {
        for key in ["server", "server_name", "connector"] {
            if let Some(val) = tool_input.get(key).and_then(Value::as_str)
                && !val.trim().is_empty()
            {
                return Some(val.trim().to_owned());
            }
        }
    }
    if is_builtin_mcp_helper_tool(input.tool_name) {
        return None;
    }
    if let Some(rest) = input.tool_name.strip_prefix("mcp__") {
        if let Some((server, _)) = rest.split_once("__")
            && !server.trim().is_empty()
        {
            return Some(server.trim().to_owned());
        }
        if let Some((server, _)) = rest.split_once(':')
            && !server.trim().is_empty()
        {
            return Some(server.trim().to_owned());
        }
    }
    if let Some(rest) = input.tool_name.strip_prefix("mcp_")
        && let Some((server, tool)) = rest.split_once('_')
        && !server.trim().is_empty()
        && !tool.trim().is_empty()
    {
        return Some(server.trim().to_owned());
    }
    None
}

fn extract_scopes(input: &WhaleCardProducerInput<'_>, detail: &str) -> Vec<String> {
    let mut scopes = Vec::new();
    for key in ["scopes", "required_scopes"] {
        if let Some(arr) = input.metadata.get(key).and_then(Value::as_array) {
            for entry in arr.iter().filter_map(Value::as_str) {
                let cleaned = entry.trim();
                if !cleaned.is_empty() && scopes.len() < 8 {
                    scopes.push(truncate_chars(
                        &redact_card_text(cleaned, input.secrets),
                        160,
                    ));
                }
            }
        }
    }
    if scopes.is_empty()
        && let Some(idx) = detail.find("scope=")
    {
        let after = &detail[idx + 6..];
        let raw = if let Some(stripped) = after.strip_prefix('"') {
            stripped.split('"').next().unwrap_or("")
        } else {
            after.split_whitespace().next().unwrap_or("")
        };
        for scope in raw
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if scopes.len() >= 8 {
                break;
            }
            scopes.push(truncate_chars(&redact_card_text(scope, input.secrets), 160));
        }
    }
    scopes
}

fn produce_connection_required_card(input: &WhaleCardProducerInput<'_>) -> Option<Value> {
    let connector = extract_connector_name(input)?;
    let status_is_401 = input
        .metadata
        .get("status_code")
        .or_else(|| input.metadata.get("http_status"))
        .and_then(Value::as_u64)
        == Some(401)
        || input.metadata.get("auth_required").and_then(Value::as_bool) == Some(true);
    if !status_is_401 && !crate::mcp::oauth::error_text_looks_auth_required(input.detail) {
        return None;
    }
    let clean_connector = truncate_chars(&redact_card_text(&connector, input.secrets), 80);
    if clean_connector.is_empty() {
        return None;
    }
    let redacted_detail = redact_card_text(input.detail.trim(), input.secrets);
    let reason = if redacted_detail.is_empty() {
        format!("Sign in to {clean_connector} to continue.")
    } else {
        truncate_chars(&redacted_detail, MAX_REASON_CHARS)
    };
    let scopes = extract_scopes(input, &redacted_detail);
    Some(json!({
        "kind": "connection_required",
        "connector": clean_connector,
        "scopes": scopes,
        "reason": reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_threads::turn_artifacts::TurnArtifactSource;
    use chrono::Utc;

    #[test]
    fn produces_bounded_markdown_artifact_card_from_written_md_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let report_path = temp.path().join("docs/audit.md");
        std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
        let long_body = format!(
            "# Quarterly Security Audit\n\nToken sk-1234567890abcdefSECRET and {}",
            "word ".repeat(2_000)
        );
        std::fs::write(&report_path, &long_body).unwrap();

        let artifact = TurnArtifactRef {
            id: "file_1234".into(),
            kind: TurnArtifactKind::File,
            path: "docs/audit.md".into(),
            change: Some(FileChangeKind::Created),
            previous_path: None,
            size: Some(long_body.len() as u64),
            revision: None,
            content_type: None,
            session_id: None,
            item_id: Some("item_1".into()),
            tool_call_id: Some("call_1".into()),
            tool_name: Some("write_file".into()),
            source: TurnArtifactSource::ToolMutation,
            restore_snapshot_id: None,
            recorded_at: Utc::now(),
        };
        let meta = serde_json::Map::new();
        let roots = vec![temp.path().to_path_buf()];
        let card = attach_whale_card_metadata(WhaleCardProducerInput {
            tool_name: "write_file",
            tool_input: None,
            detail: "Wrote docs/audit.md",
            is_error: false,
            metadata: &meta,
            artifacts: &[artifact],
            workspace_roots: &roots,
            secrets: &[],
        })
        .expect("markdown_artifact card");

        assert_eq!(card["kind"], "markdown_artifact");
        assert_eq!(card["title"], "Quarterly Security Audit");
        assert_eq!(card["path"], "docs/audit.md");
        let md = card["markdown"].as_str().unwrap();
        assert!(md.chars().count() <= 8_000);
        assert!(!md.contains("sk-1234567890abcdefSECRET"));
        assert!(
            md.contains("[REDACTED]") || md.contains(codewhale_config::persistence::REDACTED),
            "expected redaction marker in {md}"
        );
        assert!(card["wordCount"].as_u64().unwrap() >= 2_000);
    }

    #[test]
    fn produces_maintenance_warning_card_on_git_pull_exit_128_with_redaction() {
        let mut meta = serde_json::Map::new();
        meta.insert("exit_code".into(), json!(128));
        let tool_input = json!({ "command": "git pull --ff-only" });
        let roots = vec![PathBuf::from("/tmp/codewhale-repo")];
        let stderr = "fatal: Authentication failed for 'https://x-access-token:ghp_1234567890abcdefghij@github.com/org/repo.git/'\nzsh: pnpm: Command not found";

        let card = attach_whale_card_metadata(WhaleCardProducerInput {
            tool_name: "exec_shell",
            tool_input: Some(&tool_input),
            detail: stderr,
            is_error: true,
            metadata: &meta,
            artifacts: &[],
            workspace_roots: &roots,
            secrets: &[],
        })
        .expect("maintenance_warning card");

        assert_eq!(card["kind"], "maintenance_warning");
        assert_eq!(card["environmentName"], "codewhale-repo");
        assert_eq!(card["command"], "git pull --ff-only");
        assert_eq!(card["exitCode"], 128);
        assert_eq!(card["missingTools"], json!(["pnpm"]));
        let out = card["output"].as_str().unwrap();
        assert!(!out.contains("ghp_1234567890abcdefghij"));
        assert!(out.contains("[REDACTED]"));
        assert!(!card["summary"].as_str().unwrap().is_empty());
    }

    #[test]
    fn redacts_bearer_sk_underscore_cwc_and_github_pat_tokens_in_card_text() {
        let raw = [
            "abcdeé — Authorization: Bearer short_secret",
            "ééé bare bearer opaqueNoDigitsToken — 失败 … 🐋",
            "key1=sk-proj-abcdefABCDEFabcdef",
            "key2=sk_live_abcdefABCDEFabcdef",
            "pat=github_pat_abcdefABCDEFabcdef",
            "cw1=cwc_key_abcdefABCDEF",
            "cw2=cwc_refresh_abcdefABCDEF",
            "url=https://deploy_token_abc@git.example.com/org/repo.git",
        ]
        .join("\n");
        let scrubbed = redact_card_text(&raw, &["custom_exact_secret".into()]);
        assert!(scrubbed.contains("abcdeé"));
        assert!(scrubbed.contains("失败"));
        for multibyte in ["abcdeé", "a🐋b", "—…失败", "12345é"] {
            assert_eq!(redact_card_text(multibyte, &[]), multibyte);
        }
        for forbidden in [
            "short_secret",
            "opaqueNoDigitsToken",
            "sk-proj-abcdefABCDEFabcdef",
            "sk_live_abcdefABCDEFabcdef",
            "github_pat_abcdefABCDEFabcdef",
            "cwc_key_abcdefABCDEF",
            "cwc_refresh_abcdefABCDEF",
            "deploy_token_abc",
        ] {
            assert!(
                !scrubbed.contains(forbidden),
                "leaked {forbidden} in {scrubbed}"
            );
        }
    }

    #[test]
    fn maintenance_warning_ignores_git_checkout_yarn_test_and_uv_pip_list() {
        let mut meta = serde_json::Map::new();
        meta.insert("exit_code".into(), json!(1));
        for cmd in [
            "git checkout non-existent-branch",
            "yarn test",
            "uv pip list",
        ] {
            let input = json!({ "command": cmd });
            assert!(
                attach_whale_card_metadata(WhaleCardProducerInput {
                    tool_name: "exec_shell",
                    tool_input: Some(&input),
                    detail: "error: command failed with status 1",
                    is_error: true,
                    metadata: &meta,
                    artifacts: &[],
                    workspace_roots: &[],
                    secrets: &[],
                })
                .is_none(),
                "must not emit maintenance_warning for `{cmd}`"
            );
        }
    }

    #[test]
    fn produces_connection_required_card_on_mcp_401_failure_and_ignores_shell_401() {
        let mut meta = serde_json::Map::new();
        meta.insert("scopes".into(), json!(["repo:read", "issues:write"]));
        for tool_name in ["mcp__github__list_issues", "mcp_github_list_issues"] {
            let card = attach_whale_card_metadata(WhaleCardProducerInput {
                tool_name,
                tool_input: None,
                detail: "MCP request failed with HTTP 401 Unauthorized: OAuth authorization required",
                is_error: true,
                metadata: &meta,
                artifacts: &[],
                workspace_roots: &[],
                secrets: &[],
            })
            .unwrap_or_else(|| panic!("expected connection_required card for {tool_name}"));

            assert_eq!(card["kind"], "connection_required");
            assert_eq!(card["connector"], "github");
            assert_eq!(card["scopes"], json!(["repo:read", "issues:write"]));
            assert!(card["reason"].as_str().unwrap().contains("401"));
        }

        let shell_input = json!({ "command": "cargo test" });
        assert!(
            attach_whale_card_metadata(WhaleCardProducerInput {
                tool_name: "exec_shell",
                tool_input: Some(&shell_input),
                detail: "test auth_401_unauthorized ... FAILED: HTTP 401 Unauthorized",
                is_error: true,
                metadata: &serde_json::Map::new(),
                artifacts: &[],
                workspace_roots: &[],
                secrets: &[],
            })
            .is_none(),
            "non-MCP exec_shell 401 stderr must not emit connection_required"
        );
    }
}
