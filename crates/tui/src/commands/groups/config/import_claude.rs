//! Portable `/import-claude` plan rendering (FEAT-028).
//!
//! Pure formatting over the contract-owned [`CommandClaudePlan`]: no `App`, no
//! filesystem. The host builds the plan and performs the writes; the command
//! owns the flow and the messages.

use codewhale_command_contract::config::CommandClaudePlan;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Claude `permissions.defaultMode` → the Codewhale approval-posture
/// recommendation (report text only; nothing is applied).
pub(crate) fn approval_posture_recommendation(mode: Option<&str>) -> &'static str {
    match mode {
        Some("acceptEdits") => "Accept Edits posture (/permissions) matches acceptEdits",
        Some("plan") => "plan mode matches Claude's plan default",
        Some("bypassPermissions") => {
            "Full Access (yolo) matches bypassPermissions — confirm you want it"
        }
        _ => "the default Ask posture matches Claude's default",
    }
}

/// One-line migration guidance for a Claude permission rule.
pub(crate) fn permission_rule_guidance(rule: &str) -> String {
    let rule = rule.trim();
    if rule.starts_with("Bash(") {
        format!("{rule} → an execpolicy rule for that command family (/permissions)")
    } else if rule.starts_with("WebFetch(domain:") || rule.starts_with("WebSearch(") {
        format!("{rule} → a fetch/network rule for that host or surface")
    } else if rule.starts_with("mcp__") {
        format!("{rule} → enable that MCP tool in your mcp.json enabled_tools")
    } else if rule.starts_with("Read(") || rule.starts_with("Edit(") || rule.starts_with("Write(") {
        format!("{rule} → a path-scoped ask rule (reads stay workspace-bounded by default)")
    } else {
        format!("{rule} → review against /permissions before mapping")
    }
}

/// The unapplied portable bundle document for the safe env entries.
pub(crate) fn portable_bundle_json(plan: &CommandClaudePlan) -> String {
    let global: BTreeMap<&str, &str> = plan
        .env_safe
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "kind": "codewhale.portable-config",
        "metadata": {
            "name": "claude-import",
            "generator": "codewhale /import-claude",
        },
        "global": global,
    })
    .to_string()
}

/// Render the reviewable plan shown in the transcript.
pub(crate) fn render_plan(
    plan: &CommandClaudePlan,
    display: &dyn Fn(&std::path::Path) -> String,
) -> String {
    let mut out = String::new();
    out.push_str("Claude import plan (nothing is applied without its own consent):\n");
    for (path, status) in &plan.sources {
        let _ = writeln!(out, "  · {}", status.describe(&display(path)));
    }
    if !plan.mcp_candidates.is_empty() {
        out.push_str("\nMCP servers (apply each with `/mcp import <name> --approve`):\n");
        for candidate in &plan.mcp_candidates {
            let blocked = if candidate.hard_blocked {
                " [blocked]"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  · {}{blocked} — {}",
                candidate.name, candidate.summary
            );
        }
    }
    if !plan.per_project_mcp.is_empty() {
        out.push_str("\nProject-scoped MCP servers (listed for provenance only):\n");
        for name in plan.per_project_mcp.iter().take(8) {
            let _ = writeln!(out, "  · {name}");
        }
    }
    if !plan.env_safe.is_empty() {
        out.push_str("\nSafe env settings proposed for the portable bundle:\n");
        for (key, _) in &plan.env_safe {
            let _ = writeln!(out, "  · {key}");
        }
    }
    if !plan.env_skipped.is_empty() {
        out.push_str("\nEnv keys not imported (secret-shaped or unmapped; values never shown):\n");
        for key in plan.env_skipped.iter().take(12) {
            let _ = writeln!(out, "  · {key}");
        }
    }
    let _ = writeln!(
        out,
        "\nApproval posture: {}.",
        approval_posture_recommendation(plan.permissions_default_mode.as_deref())
    );
    for (label, rules) in [
        ("Allow", &plan.permissions_allow),
        ("Ask", &plan.permissions_ask),
        ("Deny", &plan.permissions_deny),
    ] {
        if !rules.is_empty() {
            let _ = writeln!(out, "\n{label} rules (map manually; not auto-applied):");
            for rule in rules.iter().take(8) {
                let _ = writeln!(out, "  · {}", permission_rule_guidance(rule));
            }
        }
    }
    if !plan.hook_events.is_empty() {
        out.push_str("\nHook events (map manually with /hooks):\n");
        for event in &plan.hook_events {
            let _ = writeln!(out, "  · {event}");
        }
    }
    if plan.has_claude_md {
        out.push_str("\nStanding instructions: ~/.claude/CLAUDE.md found — copy it to ~/.codewhale/instructions.md (or the repo's AGENTS.md) to carry it over.\n");
    }
    out
}

/// The written follow-through report (markdown).
pub(crate) fn report_markdown(
    plan: &CommandClaudePlan,
    display: &dyn Fn(&std::path::Path) -> String,
) -> String {
    let mut out = String::from(
        "# Claude import report\n\nGenerated by `/import-claude`. Nothing was applied automatically.\n\n",
    );
    out.push_str(&render_plan(plan, display));
    if !plan.env_safe.is_empty() {
        out.push_str("\n## Apply the portable bundle\n\nReview the generated bundle, then run:\n\n    codewhale config import <bundle path>\n\n");
    }
    out.push_str("\n## Manual follow-ups\n\n- MCP: `/mcp import <name> --approve` per server (consent is recorded).\n- Permissions: map rules in /permissions and execpolicy.\n- Hooks: re-create matching hooks with /hooks.\n- Paths: copy CLAUDE.md if you want those standing instructions.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::config::{CommandMcpCandidate, CommandSourceStatus};
    use std::path::PathBuf;

    fn sample_plan() -> CommandClaudePlan {
        CommandClaudePlan {
            sources: vec![(PathBuf::from("~/.claude.json"), CommandSourceStatus::Found)],
            mcp_candidates: vec![CommandMcpCandidate {
                name: "top".into(),
                summary: "stdio server (node)".into(),
                hard_blocked: false,
            }],
            per_project_mcp: vec!["inner (project /tmp/p)".into()],
            env_safe: vec![("BASH_DEFAULT_TIMEOUT_MS".into(), "120000".into())],
            env_skipped: vec!["ANTHROPIC_API_KEY".into()],
            permissions_default_mode: Some("acceptEdits".into()),
            permissions_allow: vec!["Bash(git status:*)".into(), "[redacted]".into()],
            permissions_ask: vec![],
            permissions_deny: vec!["Read(./.env)".into()],
            hook_events: vec!["PreToolUse (1)".into()],
            has_claude_md: true,
            claude_projects: vec!["/tmp/proj-a".into()],
        }
    }

    #[test]
    fn render_plan_never_echoes_secret_values_and_maps_rules() {
        let plan = sample_plan();
        let rendered = render_plan(&plan, &|path| path.display().to_string());
        assert!(!rendered.contains("sk-"), "{rendered}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(rendered.contains("Bash(git status:*) → an execpolicy rule"));
        assert!(rendered.contains("acceptEdits"));
        assert!(rendered.contains("Standing instructions: ~/.claude/CLAUDE.md found"));
    }

    #[test]
    fn portable_bundle_is_the_portable_config_document() {
        let plan = sample_plan();
        let bundle = portable_bundle_json(&plan);
        assert!(bundle.contains("codewhale.portable-config"));
        assert!(bundle.contains("BASH_DEFAULT_TIMEOUT_MS"));
        assert!(bundle.contains("120000"));
    }

    #[test]
    fn report_markdown_includes_the_plan_and_manual_follow_ups() {
        let plan = sample_plan();
        let report = report_markdown(&plan, &|path| path.display().to_string());
        assert!(report.starts_with("# Claude import report"));
        assert!(report.contains("## Manual follow-ups"));
        assert!(report.contains("## Apply the portable bundle"));
    }

    #[test]
    fn posture_recommendations_cover_the_four_claude_modes() {
        assert!(approval_posture_recommendation(Some("acceptEdits")).contains("Accept Edits"));
        assert!(approval_posture_recommendation(Some("plan")).contains("plan"));
        assert!(approval_posture_recommendation(Some("bypassPermissions")).contains("Full Access"));
        assert!(approval_posture_recommendation(None).contains("Ask"));
    }

    #[test]
    fn source_status_wording_matches_the_baseline() {
        assert_eq!(
            CommandSourceStatus::Found.describe("~/.claude.json"),
            "found: ~/.claude.json"
        );
        assert_eq!(
            CommandSourceStatus::NotFound.describe("~/.claude.json"),
            "not found: ~/.claude.json"
        );
        assert_eq!(
            CommandSourceStatus::Oversize(5).describe("~/.claude.json"),
            "skipped, too large (5 bytes): ~/.claude.json"
        );
        assert_eq!(
            CommandSourceStatus::InvalidJson.describe("~/.claude.json"),
            "invalid JSON: ~/.claude.json"
        );
        assert_eq!(
            CommandSourceStatus::Unreadable("boom".into()).describe("~/.claude.json"),
            "unreadable: ~/.claude.json (boom)"
        );
    }

    #[test]
    fn render_plan_uses_the_host_display_projection_not_an_absolute_home_path() {
        let plan = CommandClaudePlan {
            sources: vec![(
                PathBuf::from("/home/review-user/.claude.json"),
                CommandSourceStatus::Found,
            )],
            ..Default::default()
        };
        let rendered = render_plan(&plan, &|path| {
            path.display().to_string().replace("/home/review-user", "~")
        });
        assert!(rendered.contains("found: ~/.claude.json"), "{rendered}");
        assert!(
            !rendered.contains("/home/review-user"),
            "the OS account name must not leak: {rendered}"
        );
    }
}
