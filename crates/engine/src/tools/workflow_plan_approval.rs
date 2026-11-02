//! Elevated Workflow plan approval analysis (#4126).
//!
//! Builds the approval-card summary (goal, children, writes/shell/network/budget)
//! and decides whether a launch is elevated enough to require operator approval
//! beyond read-only auto-start.

use codewhale_config::WorkflowConfigToml;
use codewhale_workflow::{
    ElevationOptions, WorkflowPlanElevation, WorkflowSpec, assess_workflow_elevation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::spec::ApprovalRequirement;

/// Capability / budget summary shown on the Workflow approval card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPlanApprovalSummary {
    pub goal: String,
    pub risk: Option<String>,
    pub child_count: usize,
    pub child_labels: Vec<String>,
    pub child_summary: String,
    pub phase_count: usize,
    pub writes: bool,
    pub shell: bool,
    pub network: bool,
    pub secrets: bool,
    pub worktree: bool,
    pub high_budget: bool,
    pub broader_authority: bool,
    pub token_budget: Option<u64>,
    pub budget_label: String,
    pub elevated: bool,
    pub reasons: Vec<String>,
}

impl WorkflowPlanApprovalSummary {
    /// Card field pairs: Goal, Children, Writes, Shell, Network, Budget.
    #[must_use]
    pub fn card_fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Goal", self.goal.clone()),
            ("Children", self.child_summary.clone()),
            ("Writes", yn(self.writes).to_string()),
            ("Shell", yn(self.shell).to_string()),
            ("Network", yn(self.network).to_string()),
            ("Budget", self.budget_label.clone()),
        ]
    }

    /// One-line impacts for the shared ApprovalView card.
    #[must_use]
    pub fn approval_impacts(&self) -> Vec<String> {
        let mut impacts = Vec::new();
        if !self.goal.is_empty() {
            impacts.push(format!("Goal: {}", truncate(&self.goal, 96)));
        }
        if let Some(risk) = &self.risk {
            impacts.push(format!("Risk: {risk}"));
        }
        impacts.push(format!("Children: {}", self.child_summary));
        if self.phase_count > 0 {
            impacts.push(format!("Phases: {}", self.phase_count));
        }
        impacts.push(format!("Writes: {}", yn(self.writes)));
        impacts.push(format!("Shell: {}", yn(self.shell)));
        impacts.push(format!("Network: {}", yn(self.network)));
        if self.secrets {
            impacts.push(format!("Secrets: {}", yn(self.secrets)));
        }
        if self.worktree {
            impacts.push(format!("Worktree: {}", yn(self.worktree)));
        }
        impacts.push(format!("Budget: {}", self.budget_label));
        if self.broader_authority {
            impacts.push("Broader authority than parent mode".into());
        }
        if self.elevated {
            impacts.push(
                "Elevated plan — Approve to launch, Edit plan to revise, Cancel to abort.".into(),
            );
        } else {
            impacts.push("Read-only plan.".into());
        }
        impacts
    }

    /// Durable receipt fragment for audit after approval/launch.
    #[must_use]
    pub fn to_receipt(&self, decision: &str, approved_at_ms: u64) -> WorkflowPlanApprovalReceipt {
        WorkflowPlanApprovalReceipt {
            decision: decision.to_string(),
            approved_at_ms,
            goal: self.goal.clone(),
            child_summary: self.child_summary.clone(),
            writes: self.writes,
            shell: self.shell,
            network: self.network,
            secrets: self.secrets,
            worktree: self.worktree,
            high_budget: self.high_budget,
            broader_authority: self.broader_authority,
            budget_label: self.budget_label.clone(),
            reasons: self.reasons.clone(),
            elevated: self.elevated,
            token_budget: self.token_budget,
            risk: self.risk.clone(),
        }
    }

    #[must_use]
    pub fn is_read_only_envelope(&self) -> bool {
        !self.elevated
    }
}

/// Durable snapshot of an approved (or auto-started) plan for audit (#4126).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPlanApprovalReceipt {
    pub decision: String,
    pub approved_at_ms: u64,
    pub goal: String,
    pub child_summary: String,
    pub writes: bool,
    pub shell: bool,
    pub network: bool,
    pub secrets: bool,
    pub worktree: bool,
    pub high_budget: bool,
    pub broader_authority: bool,
    pub budget_label: String,
    pub reasons: Vec<String>,
    pub elevated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
}

/// Analyze a `workflow` tool input for approval elevation (#4126).
#[must_use]
pub fn analyze_workflow_plan_approval(input: &Value) -> WorkflowPlanApprovalSummary {
    analyze_workflow_plan_approval_with_config(input, &WorkflowConfigToml::default())
}

/// Same as [`analyze_workflow_plan_approval`] with an explicit workflow config.
#[must_use]
pub fn analyze_workflow_plan_approval_with_config(
    input: &Value,
    config: &WorkflowConfigToml,
) -> WorkflowPlanApprovalSummary {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("start");
    if matches!(action, "status" | "cancel") {
        return empty_summary(format!("workflow {action}"), None);
    }

    let mut summary = analyze_workflow_launch(input, config);
    // `verify: true` runs the workspace's detected gates (package scripts,
    // `cargo check` build scripts, ...) when the run completes. That is shell
    // execution of workspace code, so it leaves the read-only envelope.
    if input.get("verify").and_then(Value::as_bool) == Some(true) {
        summary.shell = true;
        summary.elevated = true;
        if !summary.reasons.iter().any(|r| r == "verify_gates") {
            summary.reasons.push("verify_gates".into());
        }
    }
    summary
}

fn analyze_workflow_launch(
    input: &Value,
    config: &WorkflowConfigToml,
) -> WorkflowPlanApprovalSummary {
    if let Some(plan) = input.get("plan").filter(|v| v.is_object()) {
        if plan.get("nodes").is_none() {
            return analyze_plan_object(plan, optional_u64(input, "token_budget"), config);
        }
        if let Ok(spec) = crate::tools::workflow::structured_plan_to_workflow_spec(plan) {
            return analyze_workflow_spec(&spec, optional_u64(input, "token_budget"), config);
        }
    }

    // script / source_path — conservative elevated unless clearly status-only.
    let goal = input
        .get("source_path")
        .and_then(Value::as_str)
        .map(|p| format!("source_path: {p}"))
        .or_else(|| {
            input
                .get("script")
                .and_then(Value::as_str)
                .map(|s| truncate(s.lines().next().unwrap_or("inline script"), 80))
        })
        .unwrap_or_else(|| "workflow launch".into());

    let script = input
        .get("script")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let writes = script_suggests_writes(script);
    let shell = script_suggests_shell(script);
    let network = script_suggests_network(script);
    let worktree = script.contains("worktree") || script.contains("isolation");
    let token_budget = optional_u64(input, "token_budget");
    // The card reports what the run will actually get: the caller's budget,
    // else the configured default when one is set — 0 means none applies.
    let effective_budget = token_budget
        .filter(|b| *b > 0)
        .or((config.default_token_budget > 0).then_some(config.default_token_budget));
    let high_budget = is_high_budget(token_budget, config);
    // Unknown script authority: always elevated so the card is required.
    let elevated = true;
    let mut reasons = vec!["script_or_source".to_string()];
    if writes {
        reasons.push("writes".into());
    }
    if shell {
        reasons.push("shell".into());
    }
    if network {
        reasons.push("network".into());
    }
    if worktree {
        reasons.push("worktree".into());
    }
    if high_budget {
        reasons.push("high_budget".into());
    }
    let child_count = count_script_tasks(script);
    let child_summary = if child_count == 0 {
        "script/source (authority unknown until run)".into()
    } else {
        format!("{child_count} task() calls")
    };
    WorkflowPlanApprovalSummary {
        goal,
        risk: None,
        child_count,
        child_labels: Vec::new(),
        child_summary,
        phase_count: script.matches("phase(").count(),
        writes: writes || elevated,
        shell: shell || elevated,
        network: network || elevated,
        secrets: false,
        worktree,
        high_budget,
        broader_authority: false,
        token_budget,
        budget_label: budget_label(effective_budget, high_budget),
        elevated,
        reasons,
    }
}

/// Assess a compiled Workflow IR for the approval card / receipt.
#[must_use]
pub fn analyze_workflow_spec(
    spec: &WorkflowSpec,
    token_budget: Option<u64>,
    config: &WorkflowConfigToml,
) -> WorkflowPlanApprovalSummary {
    let elevation = assess_workflow_elevation(
        spec,
        ElevationOptions {
            token_budget,
            high_budget_threshold: config.default_token_budget,
            ..ElevationOptions::default()
        },
    );
    summary_from_elevation(elevation, spec.description.clone(), token_budget)
}

fn summary_from_elevation(
    elevation: WorkflowPlanElevation,
    risk: Option<String>,
    token_budget: Option<u64>,
) -> WorkflowPlanApprovalSummary {
    WorkflowPlanApprovalSummary {
        goal: elevation.goal,
        risk,
        child_count: elevation.child_count,
        child_labels: Vec::new(),
        child_summary: elevation.child_summary,
        phase_count: 0,
        writes: elevation.writes,
        shell: elevation.shell,
        network: elevation.network,
        secrets: elevation.secrets,
        worktree: elevation.worktree,
        high_budget: elevation.high_budget,
        broader_authority: elevation.broader_authority,
        token_budget,
        budget_label: elevation.budget_label,
        elevated: elevation.elevated,
        reasons: elevation.reasons,
    }
}

/// Decide whether the workflow tool call requires an approval card (#4126).
#[must_use]
pub fn workflow_approval_requirement_for(
    input: &Value,
    config: &WorkflowConfigToml,
) -> ApprovalRequirement {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("start");
    match action {
        "status" => ApprovalRequirement::Auto,
        "cancel" => ApprovalRequirement::Required,
        _ => {
            let summary = analyze_workflow_plan_approval_with_config(input, config);
            if summary.is_read_only_envelope() {
                if config.auto_start_read_only {
                    ApprovalRequirement::Auto
                } else {
                    ApprovalRequirement::Required
                }
            } else if config.require_approval_for_writes {
                ApprovalRequirement::Required
            } else {
                ApprovalRequirement::Auto
            }
        }
    }
}

fn empty_summary(goal: String, token_budget: Option<u64>) -> WorkflowPlanApprovalSummary {
    WorkflowPlanApprovalSummary {
        goal,
        risk: None,
        child_count: 0,
        child_labels: Vec::new(),
        child_summary: "0 children".into(),
        phase_count: 0,
        writes: false,
        shell: false,
        network: false,
        secrets: false,
        worktree: false,
        high_budget: false,
        broader_authority: false,
        token_budget,
        budget_label: budget_label(token_budget, false),
        elevated: false,
        reasons: Vec::new(),
    }
}

fn analyze_plan_object(
    plan: &Value,
    token_budget_override: Option<u64>,
    config: &WorkflowConfigToml,
) -> WorkflowPlanApprovalSummary {
    let goal = plan
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let risk = plan
        .get("risk")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // Mirror structured-plan lowering's `plan_risk_to_mode` aliases exactly:
    // child role identity never elevates an omitted/read-only plan mode.
    let default_mode_is_write = matches!(
        risk.as_deref(),
        Some(
            "writes"
                | "write"
                | "read_write"
                | "readwrite"
                | "medium"
                | "elevated"
                | "high"
                | "shell"
                | "network"
        )
    );
    let token_budget = token_budget_override.or_else(|| {
        plan.get("token_budget")
            .and_then(Value::as_u64)
            .or_else(|| {
                plan.get("budget")
                    .and_then(|b| b.get("max_tokens"))
                    .and_then(Value::as_u64)
            })
    });

    let mut child_labels = Vec::new();
    let mut child_count = 0usize;
    let mut phase_count = 0usize;
    let mut writes = false;
    let mut shell = false;
    let mut network = false;
    let mut secrets = false;
    let mut worktree = false;

    if let Some(phases) = plan.get("phases").and_then(Value::as_array) {
        phase_count = phases.len();
        for phase in phases {
            collect_children(
                phase.get("children").and_then(Value::as_array),
                &mut child_labels,
                &mut child_count,
                &mut writes,
                &mut shell,
                &mut network,
                &mut secrets,
                &mut worktree,
                default_mode_is_write,
            );
        }
    }
    collect_children(
        plan.get("children").and_then(Value::as_array),
        &mut child_labels,
        &mut child_count,
        &mut writes,
        &mut shell,
        &mut network,
        &mut secrets,
        &mut worktree,
        default_mode_is_write,
    );

    if default_mode_is_write {
        writes = true;
        shell = shell || matches!(risk.as_deref(), Some("elevated" | "high" | "shell"));
        network = network || matches!(risk.as_deref(), Some("elevated" | "high" | "network"));
    }

    // Parallel write children default to worktree isolation (#4120).
    if writes && child_count > 1 {
        worktree = true;
    }

    let high_budget = is_high_budget(token_budget, config);
    let mut reasons = Vec::new();
    if writes {
        reasons.push("writes".into());
    }
    if shell {
        reasons.push("shell".into());
    }
    if network {
        reasons.push("network".into());
    }
    if secrets {
        reasons.push("secrets".into());
    }
    if worktree {
        reasons.push("worktree".into());
    }
    if high_budget {
        reasons.push("high_budget".into());
    }
    let elevated = !reasons.is_empty();

    let child_summary = if child_labels.is_empty() {
        format!(
            "{child_count} child{}",
            if child_count == 1 { "" } else { "ren" }
        )
    } else {
        let shown: Vec<_> = child_labels.iter().take(4).map(String::as_str).collect();
        let mut line = format!(
            "{child_count} child{}: {}",
            if child_count == 1 { "" } else { "ren" },
            shown.join(", ")
        );
        if child_labels.len() > 4 {
            line.push_str(", …");
        }
        line
    };

    WorkflowPlanApprovalSummary {
        goal,
        risk,
        child_count,
        child_labels,
        child_summary,
        phase_count,
        writes,
        shell,
        network,
        secrets,
        worktree,
        high_budget,
        broader_authority: false,
        token_budget,
        budget_label: budget_label(token_budget, high_budget),
        elevated,
        reasons,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_children(
    children: Option<&Vec<Value>>,
    labels: &mut Vec<String>,
    count: &mut usize,
    writes: &mut bool,
    shell: &mut bool,
    network: &mut bool,
    secrets: &mut bool,
    worktree: &mut bool,
    default_mode_is_write: bool,
) {
    let Some(children) = children else {
        return;
    };
    for child in children {
        *count += 1;
        if let Some(label) = child
            .get("label")
            .or_else(|| child.get("id"))
            .and_then(Value::as_str)
        {
            labels.push(label.to_string());
        }
        let mode = child
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let agent_type = child
            .get("type")
            .or_else(|| child.get("agent_type"))
            .and_then(Value::as_str)
            .unwrap_or("general")
            .trim()
            .to_ascii_lowercase();
        let effective_read_write = match mode.as_str() {
            "read_only" | "readonly" => false,
            "read_write" | "readwrite" | "writes" | "write" => true,
            "" => default_mode_is_write,
            other => other.contains("write") && !other.contains("read_only"),
        };
        if effective_read_write {
            *writes = true;
            // Write-capable builders/workers may run shell beyond read-only.
            // Keep the legacy runtime names for stored Workflow plans.
            if matches!(
                agent_type.as_str(),
                "builder" | "implement" | "implementer" | "worker" | "general"
            ) {
                *shell = true;
            }
        }
        if child
            .get("permissions")
            .and_then(|p| p.get("allow_write"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            *writes = true;
        }
        if child
            .get("permissions")
            .and_then(|p| p.get("allow_network"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            *network = true;
        }
        let isolation = child
            .get("isolation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if isolation == "worktree" {
            *worktree = true;
        }
        if let Some(tools) = child
            .get("permissions")
            .and_then(|p| p.get("allowed_tools"))
            .and_then(Value::as_array)
        {
            for tool in tools {
                let authority = tool_name_authority(tool.as_str().unwrap_or_default());
                *shell |= authority.shell;
                *network |= authority.network;
                *writes |= authority.writes;
                *secrets |= authority.secrets;
            }
        }
    }
}

/// What an allowed-tool entry lets a child do.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ToolAuthority {
    shell: bool,
    network: bool,
    writes: bool,
    secrets: bool,
}

/// Classify one allowed-tool name, as a plan's `allowed_tools` or a script's
/// quoted tool name spells it.
///
/// Names match case-insensitively (the model-facing families are spelled
/// `Bash`, `Web`, `File`, `Run`, `Git`), and a pattern suffix such as
/// `Web(*)` or `Bash(git status)` names the same family.
fn tool_name_authority(raw: &str) -> ToolAuthority {
    let name = raw
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let name = name.as_str();
    let mut authority = ToolAuthority::default();
    if name.contains("shell")
        || name.contains("exec")
        || name.contains("bash")
        || name == "run"
        || name.starts_with("run_")
        // `rlm` evaluates code as well as fetching.
        || name == "rlm"
    {
        authority.shell = true;
    }
    if name.contains("secret") || name.contains("credential") || name == "read_env" {
        authority.secrets = true;
    }
    if matches!(
        name,
        "web" | "web_search" | "web_run" | "web.run" | "fetch_url" | "rlm" | "git_fetch"
    ) || name.starts_with("mcp_")
    {
        authority.network = true;
    }
    // `File` carries write/edit/patch actions as well as reads; an
    // allow-list entry for the family grants all of them.
    if matches!(
        name,
        "file" | "write" | "edit" | "write_file" | "edit_file" | "apply_patch"
    ) {
        authority.writes = true;
    }
    // Git fetches and commits; the GitHub tools comment, close and publish.
    if matches!(name, "git" | "github" | "gh") || name.starts_with("github_") {
        authority.network = true;
        authority.writes = true;
    }
    // Computer and browser control act on apps and pages directly.
    if name.starts_with("computer") || name.starts_with("browser") {
        authority.network = true;
        authority.writes = true;
    }
    if name.starts_with("computer") {
        authority.shell = true;
    }
    // Tasks and automations start delegated work that runs on its own.
    if matches!(
        name,
        "task" | "tasks" | "task_create" | "automation" | "automations" | "automation_create"
    ) {
        authority.shell = true;
        authority.writes = true;
    }
    authority
}

/// Tool names a script quotes, in any quote style (`"Web"`, `'Web'`,
/// `` `Web` ``), combined. Only identifier-like literals count, so prompt
/// text in the script is not read as a tool name.
fn script_tool_authority(script: &str) -> ToolAuthority {
    let mut authority = ToolAuthority::default();
    let mut chars = script.chars();
    while let Some(c) = chars.next() {
        if !matches!(c, '"' | '\'' | '`') {
            continue;
        }
        let mut literal = String::new();
        let mut escaped = false;
        for next in chars.by_ref() {
            if escaped {
                escaped = false;
            } else if next == '\\' {
                escaped = true;
            } else if next == c {
                break;
            }
            literal.push(next);
        }
        let looks_like_tool = !literal.is_empty()
            && literal.len() <= 64
            && literal
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
            && literal.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | '(' | ')' | '*' | ' ')
            });
        if looks_like_tool {
            let found = tool_name_authority(&literal);
            authority.shell |= found.shell;
            authority.network |= found.network;
            authority.writes |= found.writes;
            authority.secrets |= found.secrets;
        }
    }
    authority
}

fn script_suggests_writes(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    lower.contains("implementer")
        || lower.contains("read_write")
        || lower.contains("allow_write")
        || lower.contains("write_file")
        || lower.contains("\"write\"")
        || lower.contains("\"edit\"")
        || lower.contains("\"file\"")
        || lower.contains("apply_patch")
        || script_tool_authority(script).writes
}

fn script_suggests_shell(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    lower.contains("exec_shell")
        || lower.contains("\"bash\"")
        || lower.contains("bash(")
        || ((lower.contains("allowedtools") || lower.contains("allowed_tools"))
            && (lower.contains("shell") || lower.contains("bash")))
        || script_tool_authority(script).shell
}

fn script_suggests_network(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    lower.contains("allow_network")
        || lower.contains("web_search")
        || lower.contains("fetch_url")
        || lower.contains("\"web\"")
        || script_tool_authority(script).network
}

fn count_script_tasks(script: &str) -> usize {
    script.matches("task(").count()
}

fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

/// A budget is "high" only against a configured baseline; when
/// `[workflow].default_token_budget` is 0 (the default) nothing is high.
fn is_high_budget(token_budget: Option<u64>, config: &WorkflowConfigToml) -> bool {
    config.default_token_budget > 0 && token_budget.is_some_and(|b| b > config.default_token_budget)
}

fn budget_label(token_budget: Option<u64>, high_budget: bool) -> String {
    match token_budget {
        Some(n) if high_budget => format!("{n} tokens (high)"),
        Some(n) => format!("{n} tokens"),
        None => "unbounded".to_string(),
    }
}

fn yn(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> WorkflowConfigToml {
        WorkflowConfigToml::default()
    }

    #[test]
    fn read_only_plan_is_not_elevated_and_auto_starts() {
        let input = json!({
            "action": "start",
            "plan": {
                "goal": "scout crates",
                "risk": "read_only",
                "token_budget": 50000,
                "phases": [{
                    "id": "scout",
                    "children": [
                        { "id": "a", "prompt": "look left", "type": "explore" },
                        { "id": "b", "prompt": "look right", "type": "explore" }
                    ]
                }]
            }
        });
        let summary = analyze_workflow_plan_approval(&input);
        assert!(!summary.elevated, "{summary:?}");
        assert_eq!(summary.child_count, 2);
        assert_eq!(summary.phase_count, 1);
        assert!(!summary.writes);
        assert_eq!(
            workflow_approval_requirement_for(&input, &config()),
            ApprovalRequirement::Auto
        );
        let fields = summary.card_fields();
        assert_eq!(fields.len(), 6);
        assert!(
            fields
                .iter()
                .any(|(k, v)| *k == "Goal" && v.contains("scout"))
        );
        assert!(fields.iter().any(|(k, v)| *k == "Writes" && v == "no"));
        assert!(fields.iter().any(|(k, v)| *k == "Shell" && v == "no"));
        assert!(fields.iter().any(|(k, v)| *k == "Network" && v == "no"));
        assert!(fields.iter().any(|(k, _)| *k == "Children"));
        assert!(fields.iter().any(|(k, _)| *k == "Budget"));
        let impacts = summary.approval_impacts();
        assert!(impacts.iter().any(|i| i.contains("Goal: scout")));
        assert!(impacts.iter().any(|i| i.contains("Writes: no")));
    }

    #[test]
    fn write_plan_is_elevated_with_card_fields_and_requires_approval() {
        let input = json!({
            "action": "start",
            "plan": {
                "goal": "land the fix",
                "risk": "writes",
                "token_budget": 120000,
                "children": [
                    {
                        "id": "builder",
                        "label": "impl",
                        "prompt": "patch it",
                        "type": "implementer",
                        "mode": "read_write"
                    }
                ]
            }
        });
        let summary = analyze_workflow_plan_approval(&input);
        assert!(summary.elevated);
        assert!(summary.writes);
        assert!(summary.shell);
        assert_eq!(
            workflow_approval_requirement_for(&input, &config()),
            ApprovalRequirement::Required
        );
        let fields = summary.card_fields();
        assert!(fields.iter().any(|(k, v)| *k == "Writes" && v == "yes"));
        assert!(fields.iter().any(|(k, v)| *k == "Shell" && v == "yes"));
        assert!(
            fields
                .iter()
                .any(|(k, v)| *k == "Budget" && v.contains("120000"))
        );
        let impacts = summary.approval_impacts();
        assert!(impacts.iter().any(|i| i.contains("Writes: yes")));
        assert!(impacts.iter().any(|i| i.contains("Approve to launch")));
        let receipt = summary.to_receipt("approved", 99);
        assert_eq!(receipt.decision, "approved");
        assert_eq!(receipt.approved_at_ms, 99);
        assert_eq!(receipt.goal, "land the fix");
        assert!(receipt.elevated);
        assert!(receipt.writes);
    }

    #[test]
    fn capability_flags_match_tool_family_names_case_insensitively() {
        let input = json!({
            "action": "start",
            "plan": {
                "goal": "scout",
                "children": [{
                    "prompt": "look",
                    "type": "explore",
                    "permissions": { "allowed_tools": ["Bash", "Web", "File"] }
                }]
            }
        });
        let summary = analyze_workflow_plan_approval(&input);
        assert!(summary.shell, "{summary:?}");
        assert!(summary.network, "{summary:?}");
        assert!(summary.writes, "{summary:?}");

        let script = r#"task({ allowedTools: ["Web", "File"] })"#;
        assert!(script_suggests_network(script));
        assert!(script_suggests_writes(script));
    }

    #[test]
    fn capability_flags_cover_quote_styles_patterns_and_more_families() {
        for script in [
            "task({ allowedTools: ['Web', 'File'] })",
            "task({ allowedTools: [`Web`, `File`] })",
            "task({ allowedTools: ['Web(*)', 'File(*)'] })",
        ] {
            assert!(script_suggests_network(script), "{script}");
            assert!(script_suggests_writes(script), "{script}");
        }
        assert!(script_suggests_shell("task({ allowedTools: ['Bash'] })"));
        // Prompt text that mentions a tool is not a tool name.
        let prose = "task({ prompt: 'read the file and summarise the web page' })";
        assert!(!script_suggests_writes(prose));
        assert!(!script_suggests_network(prose));

        let flags = |tools: Value| {
            analyze_workflow_plan_approval(&json!({
                "action": "start",
                "plan": {
                    "goal": "scout",
                    "children": [{
                        "prompt": "look",
                        "type": "explore",
                        "permissions": { "allowed_tools": tools }
                    }]
                }
            }))
        };
        let summary = flags(json!([" Web(*) ", "File(src/**)"]));
        assert!(summary.network && summary.writes, "{summary:?}");
        let summary = flags(json!(["rlm"]));
        assert!(summary.shell && summary.network, "{summary:?}");
        for family in ["github", "Git", "computer", "browser"] {
            let summary = flags(json!([family]));
            assert!(summary.network && summary.writes, "{family}: {summary:?}");
        }
        for family in ["tasks", "automation", "Run"] {
            let summary = flags(json!([family]));
            assert!(summary.shell, "{family}: {summary:?}");
        }
        // Read-only Git tools stay read-only.
        let summary = flags(json!(["git_status", "git_diff"]));
        assert!(!summary.network && !summary.writes, "{summary:?}");
    }

    #[test]
    fn canonical_worker_role_flags_shell_for_write_plan() {
        let input = json!({
            "action": "start",
            "plan": {
                "goal": "land the fix",
                "children": [{
                    "prompt": "patch it",
                    "type": "worker",
                    "mode": "read_write"
                }]
            }
        });

        let summary = analyze_workflow_plan_approval(&input);
        assert!(summary.writes);
        assert!(summary.shell);
    }

    #[test]
    fn lowercase_bash_is_classified_as_shell_authority() {
        for script in [
            r#"{"allowed_tools":["bash"]}"#,
            r#"bash({"command":"pwd"})"#,
        ] {
            assert!(script_suggests_shell(script), "{script}");
        }
    }

    #[test]
    fn read_only_implementer_does_not_request_write_or_shell_authority() {
        for child in [
            json!({
                "prompt": "review an implementation",
                "type": "implementer",
                "mode": "read_only"
            }),
            json!({
                "prompt": "review under the plan envelope",
                "type": "implementer"
            }),
        ] {
            let summary = analyze_workflow_plan_approval(&json!({
                "plan": {
                    "goal": "read-only implementation review",
                    "risk": "read_only",
                    "children": [child]
                }
            }));
            assert!(!summary.writes, "{summary:?}");
            assert!(!summary.shell, "{summary:?}");
            assert!(!summary.elevated, "{summary:?}");
        }

        let omitted_risk = analyze_workflow_plan_approval(&json!({
            "plan": {
                "goal": "default-safe implementation review",
                "children": [{ "prompt": "inspect", "type": "implementer" }]
            }
        }));
        assert!(!omitted_risk.writes, "{omitted_risk:?}");
        assert!(!omitted_risk.shell, "{omitted_risk:?}");

        for risk in ["medium", "readwrite"] {
            let write_default = analyze_workflow_plan_approval(&json!({
                "plan": {
                    "goal": "default writer",
                    "risk": risk,
                    "children": [{ "prompt": "patch" }]
                }
            }));
            assert!(write_default.writes, "{risk}: {write_default:?}");
            assert!(write_default.shell, "{risk}: {write_default:?}");
        }
    }

    #[test]
    fn elevated_risk_flags_shell_and_network() {
        let summary = analyze_workflow_plan_approval(&json!({
            "plan": {
                "goal": "full authority",
                "risk": "elevated",
                "children": [{ "prompt": "go", "type": "implementer" }]
            }
        }));
        assert!(summary.elevated);
        assert!(summary.writes);
        assert!(summary.shell);
        assert!(summary.network);
        let fields = summary.card_fields();
        assert!(fields.iter().any(|(k, v)| *k == "Network" && v == "yes"));
    }

    #[test]
    fn high_budget_elevates_read_only_plan() {
        let input = json!({
            "action": "start",
            "plan": {
                "goal": "huge scout",
                "risk": "read_only",
                "token_budget": 250_000,
                "children": [{ "prompt": "scan", "type": "explore" }]
            }
        });
        // With no configured baseline (the default) nothing is "high" — the
        // flag only exists relative to an operator-set cap (#6189).
        let uncapped = analyze_workflow_plan_approval(&input);
        assert!(!uncapped.high_budget, "{uncapped:?}");
        let config = WorkflowConfigToml {
            default_token_budget: 120_000,
            ..WorkflowConfigToml::default()
        };
        let summary = analyze_workflow_plan_approval_with_config(&input, &config);
        assert!(summary.high_budget, "{summary:?}");
        assert!(summary.elevated);
        assert!(summary.budget_label.contains("high"));
        assert_eq!(
            workflow_approval_requirement_for(&input, &config),
            ApprovalRequirement::Required
        );
    }

    #[test]
    fn secrets_and_network_tools_elevate() {
        let summary = analyze_workflow_plan_approval(&json!({
            "plan": {
                "goal": "creds",
                "risk": "read_only",
                "children": [{
                    "id": "s",
                    "prompt": "read secrets",
                    "type": "explore",
                    "permissions": {
                        "allow_network": true,
                        "allowed_tools": ["read_secret", "fetch_url"]
                    }
                }]
            }
        }));
        assert!(summary.elevated);
        assert!(summary.secrets);
        assert!(summary.network);
    }

    #[test]
    fn status_is_auto_cancel_is_required() {
        assert_eq!(
            workflow_approval_requirement_for(&json!({"action": "status"}), &config()),
            ApprovalRequirement::Auto
        );
        assert_eq!(
            workflow_approval_requirement_for(
                &json!({"action": "cancel", "run_id": "x"}),
                &config()
            ),
            ApprovalRequirement::Required
        );
    }

    #[test]
    fn require_approval_for_writes_true_blocks_write_start_read_only_stays_auto() {
        let mut cfg = config();
        cfg.require_approval_for_writes = true;
        cfg.auto_start_read_only = true;
        let write_plan = json!({
            "action": "start",
            "plan": {
                "goal": "land the fix",
                "risk": "writes",
                "children": [{
                    "prompt": "patch it",
                    "type": "implementer",
                    "mode": "read_write"
                }]
            }
        });
        let read_only = json!({
            "action": "start",
            "plan": {
                "goal": "scout crates",
                "risk": "read_only",
                "children": [{ "prompt": "look", "type": "explore" }]
            }
        });
        assert_eq!(
            workflow_approval_requirement_for(&write_plan, &cfg),
            ApprovalRequirement::Required,
            "require_approval_for_writes = true must require the card for a write start"
        );
        assert_eq!(
            workflow_approval_requirement_for(&read_only, &cfg),
            ApprovalRequirement::Auto,
            "auto_start_read_only = true must still auto-start a read-only plan"
        );
    }

    #[test]
    fn require_approval_for_writes_false_allows_elevated_auto() {
        let mut cfg = config();
        cfg.require_approval_for_writes = false;
        let input = json!({
            "action": "start",
            "plan": {
                "goal": "write freely",
                "risk": "writes",
                "children": [{ "prompt": "edit", "type": "implementer" }]
            }
        });
        assert_eq!(
            workflow_approval_requirement_for(&input, &cfg),
            ApprovalRequirement::Auto
        );
    }

    #[test]
    fn verify_gates_leave_the_read_only_envelope() {
        let plan = json!({
            "goal": "scout crates",
            "risk": "read_only",
            "children": [{ "prompt": "look", "type": "explore" }]
        });
        let without = json!({ "action": "start", "plan": plan.clone() });
        let with_verify = json!({ "action": "start", "plan": plan, "verify": true });
        assert_eq!(
            workflow_approval_requirement_for(&without, &config()),
            ApprovalRequirement::Auto
        );
        let summary = analyze_workflow_plan_approval(&with_verify);
        assert!(summary.elevated && summary.shell, "{summary:?}");
        assert!(summary.reasons.iter().any(|r| r == "verify_gates"));
        assert_eq!(
            workflow_approval_requirement_for(&with_verify, &config()),
            ApprovalRequirement::Required,
            "completion gates run workspace code and need the approval card"
        );
    }

    #[test]
    fn script_launch_requires_approval() {
        assert_eq!(
            workflow_approval_requirement_for(
                &json!({"action": "start", "script": "return 1;"}),
                &config()
            ),
            ApprovalRequirement::Required
        );
    }

    #[test]
    fn card_fields_always_six_required_labels() {
        let summary = analyze_workflow_plan_approval(&json!({
            "plan": {
                "goal": "x",
                "risk": "read_only",
                "children": [{ "prompt": "y", "type": "explore" }]
            }
        }));
        let labels: Vec<_> = summary.card_fields().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            labels,
            vec!["Goal", "Children", "Writes", "Shell", "Network", "Budget"]
        );
    }
}
