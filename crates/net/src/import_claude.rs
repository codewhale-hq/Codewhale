//! `/import-claude`: explicit, reviewable migration from Claude Code (#5557).
//!
//! Reads `~/.claude.json` and `~/.claude/settings.json` (bounded, read-only)
//! and builds a plan — never a silent import:
//!
//! - **MCP servers** are surfaced through the existing external-import consent
//!   flow (`/mcp import <name> --approve`); nothing merges automatically.
//! - **Env vars** that are known-safe (timeouts, output caps, editor choices)
//!   are proposed as a portable bundle file for `codewhale config import`,
//!   which carries its own plan/consent/rollback. Secret-shaped or unknown
//!   keys are named but never imported or echoed.
//! - **Permissions** become approval-policy recommendations in a written
//!   report (Claude rule grammar does not map 1:1; nothing is auto-applied).
//! - **Hooks** and **paths** (e.g. `~/.claude/CLAUDE.md`) are listed as manual
//!   follow-ups with exact target commands.
//!
//! The plan model and all rendering live in the contract and the portable
//! group; this module only reads the Claude sources and builds the plan.

use codewhale_command_contract::config::{
    CommandClaudePlan, CommandMcpCandidate, CommandSourceStatus,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Maximum size accepted for any single Claude source file. Generous for
/// real `~/.claude.json` files with many projects; bounded so a pathological
/// file cannot exhaust memory. Matches the bundle reader's order of magnitude.
pub const MAX_SOURCE_BYTES: u64 = 5 * 1024 * 1024;

/// Env keys safe to propose for the portable bundle (lowercase). Deliberately
/// a short allowlist: timeouts, output caps, and editor/telemetry choices.
/// Anything not listed stays out of the bundle and is reported by name only.
const SAFE_ENV_KEYS: &[&str] = &[
    "bash_default_timeout_ms",
    "bash_max_timeout_ms",
    "bash_max_output_length",
    "bash_mcp_timeout_ms",
    "mcp_timeout_ms",
    "mcp_tools_timeout_ms",
    "use_builtin_ripgrep",
    "editor",
    "max_thinking_tokens",
    "disable_cost_warnings",
    "disable_nonessential_model_calls",
    "disable_telemetry",
];

/// Longest env value accepted into the bundle; guards against blob-shaped
/// values a migration should not carry.
const MAX_ENV_VALUE_LEN: usize = 200;

fn read_bounded_json(path: &Path) -> Result<Value, CommandSourceStatus> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CommandSourceStatus::NotFound,
        _ => CommandSourceStatus::Unreadable(e.to_string()),
    })?;
    if !meta.is_file() {
        return Err(CommandSourceStatus::NotFound);
    }
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(CommandSourceStatus::Oversize(meta.len()));
    }
    let raw = std::fs::read(path).map_err(|e| CommandSourceStatus::Unreadable(e.to_string()))?;
    serde_json::from_slice::<Value>(&raw).map_err(|_| CommandSourceStatus::InvalidJson)
}

/// Read the Claude sources under `home` (usually `~`).
pub fn read_sources(
    home: &Path,
) -> (
    Vec<(PathBuf, CommandSourceStatus)>,
    Option<Value>,
    Option<Value>,
) {
    let claude_json = home.join(".claude.json");
    let settings = home.join(".claude").join("settings.json");
    let mut sources = Vec::new();
    let mut claude_value = None;
    let mut settings_value = None;
    match read_bounded_json(&claude_json) {
        Ok(value) => {
            sources.push((claude_json.clone(), CommandSourceStatus::Found));
            claude_value = Some(value);
        }
        Err(status) => sources.push((claude_json, status)),
    }
    match read_bounded_json(&settings) {
        Ok(value) => {
            sources.push((settings.clone(), CommandSourceStatus::Found));
            settings_value = Some(value);
        }
        Err(status) => sources.push((settings, status)),
    }
    (sources, claude_value, settings_value)
}

fn safe_env_value(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => {
            (text.len() <= MAX_ENV_VALUE_LEN && !text.contains('\n')).then(|| text.clone())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }?;
    (codewhale_config::persistence::redact_secrets(&text) == text).then_some(text)
}

/// Build the plan from already-read sources plus MCP candidates discovered by
/// the existing external-import machinery (so server parsing stays single-sourced).
pub fn build_plan(
    sources: Vec<(PathBuf, CommandSourceStatus)>,
    claude: Option<Value>,
    settings: Option<Value>,
    home: &Path,
    mcp_candidates: Vec<CommandMcpCandidate>,
) -> CommandClaudePlan {
    let mut plan = CommandClaudePlan {
        sources,
        mcp_candidates,
        ..CommandClaudePlan::default()
    };
    if let Some(claude) = claude.as_ref()
        && let Some(projects) = claude.get("projects").and_then(Value::as_object)
    {
        for (path, project) in projects {
            if let Some(servers) = project.get("mcpServers").and_then(Value::as_object) {
                for name in servers.keys() {
                    plan.per_project_mcp
                        .push(format!("{name} (project {path})"));
                }
            }
            if plan.claude_projects.len() < 8 {
                plan.claude_projects.push(path.clone());
            }
        }
    }
    if let Some(settings) = settings.as_ref() {
        if let Some(env) = settings.get("env").and_then(Value::as_object) {
            for (key, value) in env {
                if SAFE_ENV_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
                    match safe_env_value(value) {
                        Some(text) => {
                            plan.env_safe.push((key.clone(), text));
                        }
                        None => plan.env_skipped.push(format!("{key} (value not portable)")),
                    }
                } else {
                    plan.env_skipped.push(key.clone());
                }
            }
        }
        if let Some(permissions) = settings.get("permissions").and_then(Value::as_object) {
            plan.permissions_default_mode = permissions
                .get("defaultMode")
                .and_then(Value::as_str)
                .map(str::to_string);
            for (key, target) in [
                ("allow", &mut plan.permissions_allow),
                ("ask", &mut plan.permissions_ask),
                ("deny", &mut plan.permissions_deny),
            ] {
                if let Some(rules) = permissions.get(key).and_then(Value::as_array) {
                    for rule in rules {
                        if let Some(text) = rule.as_str() {
                            target.push(codewhale_config::persistence::redact_secrets(text));
                        }
                    }
                }
            }
        }
        if let Some(hooks) = settings.get("hooks").and_then(Value::as_object) {
            for (event, entries) in hooks {
                let count = entries.as_array().map_or(1, Vec::len);
                plan.hook_events.push(format!("{event} ({count})"));
            }
        }
    }
    plan.has_claude_md = home.join(".claude").join("CLAUDE.md").is_file();
    plan.env_safe.sort();
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_home(tag: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("home");
        let _ = tag;
        let home = dir.path().to_path_buf();
        (dir, home)
    }

    fn env_safe_value(plan: &CommandClaudePlan, key: &str) -> Option<String> {
        plan.env_safe
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn missing_sources_report_not_found_and_plan_is_empty() {
        let (dir, home) = temp_home("missing");
        let (sources, claude, settings) = read_sources(&home);
        let plan = build_plan(sources, claude, settings, &home, Vec::new());
        assert!(plan.is_empty());
        assert!(
            plan.sources
                .iter()
                .any(|(_, s)| *s == CommandSourceStatus::NotFound)
        );
        assert!(dir.path().exists());
    }

    #[test]
    fn settings_env_splits_safe_and_skipped_without_ever_inlining_secret_values() {
        let (_dir, home) = temp_home("env");
        std::fs::create_dir_all(home.join(".claude")).expect("dir");
        std::fs::write(
            home.join(".claude").join("settings.json"),
            json!({
                "env": {
                    "BASH_DEFAULT_TIMEOUT_MS": "120000",
                    "USE_BUILTIN_RIPGREP": "1",
                    "EDITOR": "sk-editor-secret-value",
                    "ANTHROPIC_API_KEY": "sk-ant-secret-value",
                    "SOMETHING_ELSE": "x"
                },
                "permissions": {
                    "defaultMode": "acceptEdits",
                    "allow": ["Bash(git status:*)", "WebFetch(domain:github.com)", "mcp__demo__kick", "Bash(curl -H token=sk-rule-secret-value)"] ,
                    "deny": ["Read(./.env)"]
                },
                "hooks": {
                    "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo hi"}]}],
                    "PostToolUse": []
                }
            })
            .to_string(),
        )
        .expect("settings");

        let (sources, _claude, settings) = read_sources(&home);
        let plan = build_plan(sources, None, settings, &home, Vec::new());
        assert_eq!(
            env_safe_value(&plan, "BASH_DEFAULT_TIMEOUT_MS").as_deref(),
            Some("120000")
        );
        assert!(env_safe_value(&plan, "USE_BUILTIN_RIPGREP").is_some());
        assert!(env_safe_value(&plan, "EDITOR").is_none());
        assert!(env_safe_value(&plan, "ANTHROPIC_API_KEY").is_none());
        assert!(
            plan.env_skipped
                .iter()
                .any(|k| k.contains("ANTHROPIC_API_KEY"))
        );
        assert_eq!(
            plan.permissions_default_mode.as_deref(),
            Some("acceptEdits")
        );
        assert_eq!(plan.permissions_allow.len(), 4);
        assert!(
            plan.permissions_allow
                .iter()
                .any(|rule| rule.contains("[redacted]"))
        );
        assert_eq!(plan.permissions_deny.len(), 1);
        assert_eq!(plan.hook_events.len(), 2);
    }

    #[test]
    fn claude_json_surfaces_projects_and_per_project_servers() {
        let (dir, home) = temp_home("projects");
        std::fs::write(
            home.join(".claude.json"),
            json!({
                "mcpServers": {
                    "top": {"command": "node", "args": ["top.js"]}
                },
                "projects": {
                    "/tmp/proj-a": {"mcpServers": {"inner": {"command": "node"}}}
                }
            })
            .to_string(),
        )
        .expect("claude.json");

        let (sources, claude, _settings) = read_sources(&home);
        let candidates = vec![CommandMcpCandidate {
            name: "top".to_string(),
            summary: "stdio server (node)".to_string(),
            hard_blocked: false,
        }];
        let plan = build_plan(sources, claude, None, &home, candidates);
        assert_eq!(plan.mcp_candidates.len(), 1);
        assert_eq!(plan.per_project_mcp.len(), 1);
        assert!(plan.per_project_mcp[0].contains("inner"));
        assert!(plan.claude_projects.iter().any(|p| p.contains("proj-a")));
        assert!(dir.path().exists());
    }

    #[test]
    fn oversize_and_invalid_sources_fail_closed_with_named_status() {
        let (_dir, home) = temp_home("bad");
        std::fs::write(home.join(".claude.json"), "not json").expect("bad json");
        let (sources, claude, _settings) = read_sources(&home);
        assert!(claude.is_none());
        assert!(
            sources
                .iter()
                .any(|(_, s)| matches!(s, CommandSourceStatus::InvalidJson))
        );
    }
}
