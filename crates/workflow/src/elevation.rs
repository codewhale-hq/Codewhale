//! Elevated Workflow plan assessment for approval cards (#4126).
//!
//! Pure, UI-free analysis of a [`WorkflowSpec`] (and optional planner risk
//! string) so callers can decide whether an operator approval card is required
//! and what fields that card should show.

use serde::{Deserialize, Serialize};

use crate::{
    IsolationMode, LeafSpec, PermissionSpec, TaskMode, WorkflowNode, WorkflowSpec,
    leaf_is_write_capable, leaf_wants_worktree,
};

/// Fallback high-budget flag threshold for approval display when a caller
/// does not thread `[workflow].default_token_budget` through. Display-only:
/// it never caps a run.
pub const DEFAULT_HIGH_BUDGET_THRESHOLD: u64 = 120_000;

/// Options that refine elevation assessment beyond the IR itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElevationOptions {
    /// Token budget declared on the tool call (may outrank `spec.budget`).
    pub token_budget: Option<u64>,
    /// Threshold above which a token budget is considered high. `0` disables
    /// the flag — with no configured baseline there is nothing to exceed.
    pub high_budget_threshold: u64,
    /// Whether the parent session currently allows writes.
    pub parent_allows_write: bool,
    /// Whether the parent session currently allows network.
    pub parent_allows_network: bool,
}

impl Default for ElevationOptions {
    fn default() -> Self {
        Self {
            token_budget: None,
            high_budget_threshold: DEFAULT_HIGH_BUDGET_THRESHOLD,
            // Assume Act/read-write parent unless callers narrow posture.
            parent_allows_write: true,
            parent_allows_network: true,
        }
    }
}

/// Summary of why a Workflow plan needs (or does not need) elevated approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPlanElevation {
    pub elevated: bool,
    pub goal: String,
    pub child_count: usize,
    pub child_summary: String,
    pub writes: bool,
    pub shell: bool,
    pub network: bool,
    pub secrets: bool,
    pub worktree: bool,
    pub high_budget: bool,
    pub broader_authority: bool,
    /// Human-readable budget line for the approval card.
    pub budget_label: String,
    /// Distinct elevation reasons (for audit / impact lines).
    pub reasons: Vec<String>,
}

impl WorkflowPlanElevation {
    /// Card field labels/values used by the TUI approval modal (#4126).
    #[must_use]
    pub fn card_fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Goal", self.goal.clone()),
            ("Children", self.child_summary.clone()),
            ("Writes", yes_no(self.writes)),
            ("Shell", yes_no(self.shell)),
            ("Network", yes_no(self.network)),
            ("Budget", self.budget_label.clone()),
        ]
    }

    /// True when the plan is fully inside the read-only envelope.
    #[must_use]
    pub fn is_read_only_envelope(&self) -> bool {
        !self.elevated
            && !self.writes
            && !self.shell
            && !self.network
            && !self.secrets
            && !self.worktree
            && !self.high_budget
            && !self.broader_authority
    }
}

fn yes_no(flag: bool) -> String {
    if flag {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

/// Assess elevation for a compiled [`WorkflowSpec`].
#[must_use]
pub fn assess_workflow_elevation(
    spec: &WorkflowSpec,
    options: ElevationOptions,
) -> WorkflowPlanElevation {
    let mut child_ids = Vec::new();
    let mut writes = false;
    let mut shell = false;
    let mut network = false;
    let mut secrets = false;
    let mut worktree = false;

    walk_nodes(
        &spec.nodes,
        /* parallel */ false,
        &mut child_ids,
        &mut writes,
        &mut shell,
        &mut network,
        &mut secrets,
        &mut worktree,
    );

    // Spec-level permissions also elevate.
    merge_permissions(
        &spec.permissions,
        &mut writes,
        &mut shell,
        &mut network,
        &mut secrets,
    );

    // The structured-plan lowerer stores its validated risk enum on
    // `description`, while authored Workflow specs use that field for ordinary
    // prose. Only consume recognized enum values here: treating free-form
    // descriptions as unknown risk would falsely report writes, shell, and
    // network in the approval receipt. Unknown planner risk remains fail-closed
    // in `assess_plan_risk_string` and is rejected before structured lowering.
    if let Some(risk) = embedded_plan_risk_hint(spec.description.as_deref()) {
        apply_plan_risk_hint(Some(risk), &mut writes, &mut shell, &mut network);
    }

    let effective_tokens = options
        .token_budget
        .or(spec.budget.max_tokens)
        .filter(|n| *n > 0);
    let high_budget = options.high_budget_threshold > 0
        && effective_tokens.is_some_and(|n| n > options.high_budget_threshold);

    let broader_authority =
        (!options.parent_allows_write && writes) || (!options.parent_allows_network && network);

    let mut reasons = Vec::new();
    if writes {
        reasons.push("writes".to_string());
    }
    if shell {
        reasons.push("shell".to_string());
    }
    if network {
        reasons.push("network".to_string());
    }
    if secrets {
        reasons.push("secrets".to_string());
    }
    if worktree {
        reasons.push("worktree".to_string());
    }
    if high_budget {
        reasons.push("high_budget".to_string());
    }
    if broader_authority {
        reasons.push("broader_authority".to_string());
    }

    let elevated = !reasons.is_empty();
    let child_count = child_ids.len();
    let child_summary = if child_ids.is_empty() {
        "0 children".to_string()
    } else if child_ids.len() <= 4 {
        format!(
            "{} child{}: {}",
            child_ids.len(),
            if child_ids.len() == 1 { "" } else { "ren" },
            child_ids.join(", ")
        )
    } else {
        format!(
            "{} children: {}, {}… (+{})",
            child_ids.len(),
            child_ids[0],
            child_ids[1],
            child_ids.len() - 2
        )
    };

    let budget_label = format_budget_label(effective_tokens, &spec.budget, high_budget);

    WorkflowPlanElevation {
        elevated,
        goal: spec.goal.clone(),
        child_count,
        child_summary,
        writes,
        shell,
        network,
        secrets,
        worktree,
        high_budget,
        broader_authority,
        budget_label,
        reasons,
    }
}

/// Lightweight assessment from a planner `risk` string alone (before IR lower).
#[must_use]
pub fn assess_plan_risk_string(risk: Option<&str>) -> PlanRiskHint {
    match risk.map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("read_only") | Some("readonly") | Some("low") | Some("safe") => {
            PlanRiskHint::ReadOnly
        }
        Some("writes") | Some("write") | Some("read_write") | Some("readwrite")
        | Some("medium") => PlanRiskHint::Writes,
        Some("shell") => PlanRiskHint::Shell,
        Some("network") => PlanRiskHint::Network,
        Some("elevated") | Some("high") => PlanRiskHint::Elevated,
        Some(_) => PlanRiskHint::Elevated,
    }
}

/// Coarse risk classification from the structured plan `risk` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanRiskHint {
    ReadOnly,
    Writes,
    Shell,
    Network,
    Elevated,
}

impl PlanRiskHint {
    #[must_use]
    pub fn elevates(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }
}

fn apply_plan_risk_hint(
    risk: Option<&str>,
    writes: &mut bool,
    shell: &mut bool,
    network: &mut bool,
) {
    match assess_plan_risk_string(risk) {
        PlanRiskHint::ReadOnly => {}
        PlanRiskHint::Writes => *writes = true,
        PlanRiskHint::Shell => {
            *shell = true;
            *writes = true;
        }
        PlanRiskHint::Network => {
            *network = true;
        }
        PlanRiskHint::Elevated => {
            *writes = true;
            *shell = true;
            *network = true;
        }
    }
}

fn embedded_plan_risk_hint(description: Option<&str>) -> Option<&str> {
    let value = description
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    matches!(
        value,
        "read_only"
            | "readonly"
            | "low"
            | "safe"
            | "writes"
            | "write"
            | "read_write"
            | "readwrite"
            | "medium"
            | "shell"
            | "network"
            | "elevated"
            | "high"
    )
    .then_some(value)
}

fn format_budget_label(
    effective_tokens: Option<u64>,
    budget: &crate::BudgetSpec,
    high_budget: bool,
) -> String {
    let mut parts = Vec::new();
    if let Some(tokens) = effective_tokens {
        parts.push(format!("{tokens} tokens"));
    }
    if let Some(steps) = budget.max_steps {
        parts.push(format!("max_steps={steps}"));
    }
    if let Some(timeout) = budget.timeout_secs {
        parts.push(format!("timeout={timeout}s"));
    }
    if let Some(parallel) = budget.max_parallel {
        parts.push(format!("max_parallel={parallel}"));
    }
    if parts.is_empty() {
        "default".to_string()
    } else if high_budget {
        format!("{} (high)", parts.join(", "))
    } else {
        parts.join(", ")
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_nodes(
    nodes: &[WorkflowNode],
    parallel: bool,
    child_ids: &mut Vec<String>,
    writes: &mut bool,
    shell: &mut bool,
    network: &mut bool,
    secrets: &mut bool,
    worktree: &mut bool,
) {
    for node in nodes {
        match node {
            WorkflowNode::Leaf(leaf) => {
                inspect_leaf(
                    leaf, parallel, child_ids, writes, shell, network, secrets, worktree,
                );
            }
            WorkflowNode::BranchSet(branch) => {
                merge_permissions(&branch.permissions, writes, shell, network, secrets);
                walk_nodes(
                    &branch.children,
                    branch.parallel || parallel,
                    child_ids,
                    writes,
                    shell,
                    network,
                    secrets,
                    worktree,
                );
            }
            WorkflowNode::Sequence(seq) => {
                walk_nodes(
                    &seq.children,
                    parallel,
                    child_ids,
                    writes,
                    shell,
                    network,
                    secrets,
                    worktree,
                );
            }
            WorkflowNode::LoopUntil(loop_spec) => {
                walk_nodes(
                    &loop_spec.children,
                    parallel,
                    child_ids,
                    writes,
                    shell,
                    network,
                    secrets,
                    worktree,
                );
            }
            WorkflowNode::Cond(cond) => {
                walk_nodes(
                    &cond.then_nodes,
                    parallel,
                    child_ids,
                    writes,
                    shell,
                    network,
                    secrets,
                    worktree,
                );
                walk_nodes(
                    &cond.else_nodes,
                    parallel,
                    child_ids,
                    writes,
                    shell,
                    network,
                    secrets,
                    worktree,
                );
            }
            WorkflowNode::Expand(expand) => {
                if let Some(template) = expand.template.as_deref() {
                    walk_nodes(
                        std::slice::from_ref(template),
                        parallel,
                        child_ids,
                        writes,
                        shell,
                        network,
                        secrets,
                        worktree,
                    );
                }
            }
            WorkflowNode::Reduce(_) | WorkflowNode::TeacherReview(_) => {
                // Control/reduce nodes do not spawn write-capable leaves themselves.
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn inspect_leaf(
    leaf: &LeafSpec,
    parallel: bool,
    child_ids: &mut Vec<String>,
    writes: &mut bool,
    shell: &mut bool,
    network: &mut bool,
    secrets: &mut bool,
    worktree: &mut bool,
) {
    child_ids.push(leaf.id.clone());
    if leaf_is_write_capable(leaf) {
        *writes = true;
    }
    merge_permissions(&leaf.permissions, writes, shell, network, secrets);
    if leaf_wants_worktree(leaf, parallel) || matches!(leaf.isolation, IsolationMode::Worktree) {
        *worktree = true;
    }
    // Explicit read_write mode with shell tools already handled; implementer
    // without a tool denylist can run shell.
    if leaf.mode == TaskMode::ReadWrite
        && leaf.permissions.allowed_tools.is_empty()
        && matches!(
            leaf.agent_type,
            crate::AgentType::Implementer | crate::AgentType::General
        )
    {
        // Write-capable implementers/general agents may run shell beyond
        // read-only — flag shell as elevated for the approval card.
        *shell = true;
    }
}

fn merge_permissions(
    permissions: &PermissionSpec,
    writes: &mut bool,
    shell: &mut bool,
    network: &mut bool,
    secrets: &mut bool,
) {
    if permissions.allow_write {
        *writes = true;
    }
    if permissions.allow_network {
        *network = true;
    }
    for tool in &permissions.allowed_tools {
        let name = tool.trim();
        let authority = tool_name_authority(name);
        if is_write_tool(name) || authority.writes {
            *writes = true;
        }
        if is_shell_tool(name) || authority.shell {
            *shell = true;
        }
        if is_network_tool(name) || authority.network {
            *network = true;
        }
        if is_secret_tool(name) || authority.secrets {
            *secrets = true;
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ToolAuthority {
    pub shell: bool,
    pub network: bool,
    pub writes: bool,
    pub secrets: bool,
}

#[must_use]
pub fn tool_name_authority(raw: &str) -> ToolAuthority {
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
    if matches!(
        name,
        "file" | "write" | "edit" | "write_file" | "edit_file" | "apply_patch"
    ) {
        authority.writes = true;
    }
    if matches!(name, "git" | "github" | "gh") || name.starts_with("github_") {
        authority.network = true;
        authority.writes = true;
    }
    if name.starts_with("computer") || name.starts_with("browser") {
        authority.network = true;
        authority.writes = true;
    }
    if name.starts_with("computer") {
        authority.shell = true;
    }
    if matches!(
        name,
        "task" | "tasks" | "task_create" | "automation" | "automations" | "automation_create"
    ) {
        authority.shell = true;
        authority.writes = true;
    }
    authority
}

/// True for a tool that can modify files.
///
/// This is the one list. It previously existed twice — here and as half of
/// the TUI's `is_write_or_shell_tool` — and the two drifted: `Edit`, the
/// model-visible canonical name for the write tool, was in the TUI copy and
/// missing here, so a branch or sequence whose `allowed_tools` was `["Edit"]`
/// produced an approval card reporting `writes: false` for a spec that could
/// in fact write.
pub fn is_write_tool(tool: &str) -> bool {
    matches!(
        tool.trim(),
        "Edit" | "write_file" | "edit_file" | "apply_patch" | "checklist_write" | "todo_write"
    )
}

/// True for a tool that can run a shell command.
pub fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool.trim(),
        "exec_shell"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "exec_wait"
            | "exec_interact"
            | "task_shell_start"
            | "task_shell_wait"
    )
}

fn is_network_tool(tool: &str) -> bool {
    matches!(
        tool,
        "web_search" | "web_run" | "fetch_url" | "wait_for_dev_server"
    ) || tool.starts_with("mcp_")
}

fn is_secret_tool(tool: &str) -> bool {
    let lower = tool.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("credential")
        || lower.contains("password")
        || lower == "read_env"
        || lower == "env"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentType, BranchSpec, BudgetSpec, LeafSpec, ModelPolicy, PermissionSpec, PromotionPolicy,
        SequenceSpec, TaskMode,
    };

    fn leaf(id: &str, mode: TaskMode) -> LeafSpec {
        LeafSpec {
            id: id.to_string(),
            prompt: format!("do {id}"),
            agent_type: if mode == TaskMode::ReadWrite {
                AgentType::Implementer
            } else {
                AgentType::Explore
            },
            profile: None,
            role: None,
            mode,
            isolation: IsolationMode::Auto,
            file_scope: Vec::new(),
            cwd: None,
            depends_on_results: Vec::new(),
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
        }
    }

    #[test]
    fn edit_is_recognized_as_a_write_tool() {
        // #4730: `Edit` is the model-visible canonical write-tool name. It
        // lived only in the TUI's copy of this list, so the risk assessor
        // didn't know it was a write.
        assert!(is_write_tool("Edit"));
        assert!(is_write_tool(" Edit "));
        for tool in [
            "write_file",
            "edit_file",
            "apply_patch",
            "checklist_write",
            "todo_write",
        ] {
            assert!(is_write_tool(tool), "{tool} must count as a write");
        }
        assert!(!is_write_tool("read_file"));
        assert!(!is_write_tool("Editor"));
    }

    #[test]
    fn branch_allowing_edit_reports_writes_in_its_risk_summary() {
        // The tool-allowlist path is what produces branch/sequence-level
        // permission summaries; a spec that can write must not present an
        // approval card saying it cannot.
        let spec = spec_with(
            vec![WorkflowNode::BranchSet(BranchSpec {
                id: "edits".to_string(),
                description: None,
                parallel: false,
                budget: BudgetSpec::default(),
                permissions: PermissionSpec {
                    allowed_tools: vec!["Edit".to_string()],
                    ..PermissionSpec::default()
                },
                model_policy: ModelPolicy::default(),
                children: vec![WorkflowNode::Leaf(leaf("child", TaskMode::ReadOnly))],
            })],
            None,
        );

        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(
            elevation.writes,
            "branch allowing Edit must report writes: {elevation:?}"
        );
    }

    fn spec_with(nodes: Vec<WorkflowNode>, risk: Option<&str>) -> WorkflowSpec {
        WorkflowSpec {
            id: Some("test".to_string()),
            goal: "ship feature".to_string(),
            description: risk.map(str::to_string),
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            promotion_policy: PromotionPolicy::default(),
            gates: Vec::new(),
            nodes,
        }
    }

    #[test]
    fn read_only_plan_is_not_elevated() {
        let spec = spec_with(
            vec![WorkflowNode::Leaf(leaf("scan", TaskMode::ReadOnly))],
            Some("read_only"),
        );
        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(!elevation.elevated, "{elevation:?}");
        assert!(elevation.is_read_only_envelope());
        assert_eq!(elevation.goal, "ship feature");
        assert!(elevation.child_summary.contains("scan"));
        assert!(!elevation.writes);
        assert!(!elevation.shell);
        assert!(!elevation.network);
        let fields = elevation.card_fields();
        assert_eq!(fields.len(), 6);
        assert!(
            fields
                .iter()
                .any(|(k, v)| *k == "Goal" && v == "ship feature")
        );
        assert!(fields.iter().any(|(k, v)| *k == "Writes" && v == "no"));
    }

    #[test]
    fn free_form_description_is_not_treated_as_plan_risk() {
        let spec = spec_with(
            vec![WorkflowNode::Leaf(leaf("scan", TaskMode::ReadOnly))],
            Some(
                "Read-only release acceptance fixture; no step edits files or accesses the network.",
            ),
        );

        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(elevation.is_read_only_envelope(), "{elevation:?}");
        assert!(!elevation.writes, "{elevation:?}");
        assert!(!elevation.shell, "{elevation:?}");
        assert!(!elevation.network, "{elevation:?}");
        assert!(elevation.reasons.is_empty(), "{elevation:?}");
    }

    #[test]
    fn read_only_implementer_role_is_not_write_capable_or_elevated() {
        let mut implementer = leaf("verify-only", TaskMode::ReadOnly);
        implementer.agent_type = AgentType::Implementer;
        implementer.role = Some("implementer".to_string());
        let spec = spec_with(
            vec![WorkflowNode::BranchSet(BranchSpec {
                id: "parallel-read-only".to_string(),
                description: None,
                parallel: true,
                budget: BudgetSpec::default(),
                permissions: PermissionSpec::default(),
                model_policy: ModelPolicy::default(),
                children: vec![WorkflowNode::Leaf(implementer)],
            })],
            Some("read_only"),
        );

        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(elevation.is_read_only_envelope(), "{elevation:?}");
        assert!(!elevation.elevated, "{elevation:?}");
        assert!(!elevation.writes, "{elevation:?}");
        assert!(!elevation.shell, "{elevation:?}");
        assert!(!elevation.worktree, "{elevation:?}");
    }

    #[test]
    fn write_plan_elevates_and_flags_shell_for_implementer() {
        let spec = spec_with(
            vec![WorkflowNode::Leaf(leaf("impl", TaskMode::ReadWrite))],
            Some("writes"),
        );
        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(elevation.elevated);
        assert!(elevation.writes);
        assert!(elevation.shell);
        assert!(elevation.reasons.iter().any(|r| r == "writes"));
    }

    #[test]
    fn network_and_secrets_tools_elevate() {
        let mut network_leaf = leaf("fetch", TaskMode::ReadOnly);
        network_leaf.permissions.allow_network = true;
        network_leaf.permissions.allowed_tools = vec!["fetch_url".to_string()];

        let mut secret_leaf = leaf("creds", TaskMode::ReadOnly);
        secret_leaf.permissions.allowed_tools = vec!["read_secret".to_string()];

        let spec = spec_with(
            vec![WorkflowNode::Sequence(SequenceSpec {
                id: "seq".to_string(),
                children: vec![
                    WorkflowNode::Leaf(network_leaf),
                    WorkflowNode::Leaf(secret_leaf),
                ],
            })],
            None,
        );
        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(elevation.elevated);
        assert!(elevation.network);
        assert!(elevation.secrets);
        assert!(elevation.reasons.iter().any(|r| r == "network"));
        assert!(elevation.reasons.iter().any(|r| r == "secrets"));
    }

    #[test]
    fn parallel_write_children_flag_worktree() {
        let left = leaf("left", TaskMode::ReadWrite);
        let right = leaf("right", TaskMode::ReadWrite);
        let spec = spec_with(
            vec![WorkflowNode::BranchSet(BranchSpec {
                id: "parallel".to_string(),
                description: None,
                parallel: true,
                budget: BudgetSpec::default(),
                permissions: PermissionSpec::default(),
                model_policy: ModelPolicy::default(),
                children: vec![WorkflowNode::Leaf(left), WorkflowNode::Leaf(right)],
            })],
            Some("writes"),
        );
        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(elevation.worktree, "{elevation:?}");
        assert!(elevation.writes);
    }

    #[test]
    fn high_budget_elevates() {
        let mut spec = spec_with(
            vec![WorkflowNode::Leaf(leaf("scan", TaskMode::ReadOnly))],
            Some("read_only"),
        );
        spec.budget.max_tokens = Some(250_000);
        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(elevation.high_budget);
        assert!(elevation.elevated);
        assert!(elevation.budget_label.contains("high"));
    }

    #[test]
    fn broader_authority_when_parent_is_read_only() {
        let spec = spec_with(
            vec![WorkflowNode::Leaf(leaf("impl", TaskMode::ReadWrite))],
            Some("writes"),
        );
        let elevation = assess_workflow_elevation(
            &spec,
            ElevationOptions {
                parent_allows_write: false,
                parent_allows_network: false,
                ..ElevationOptions::default()
            },
        );
        assert!(elevation.broader_authority);
        assert!(elevation.reasons.iter().any(|r| r == "broader_authority"));
    }

    #[test]
    fn plan_risk_string_classifies_elevated_variants() {
        assert_eq!(
            assess_plan_risk_string(Some("read_only")),
            PlanRiskHint::ReadOnly
        );
        assert_eq!(
            assess_plan_risk_string(Some("writes")),
            PlanRiskHint::Writes
        );
        assert_eq!(assess_plan_risk_string(Some("shell")), PlanRiskHint::Shell);
        assert_eq!(
            assess_plan_risk_string(Some("network")),
            PlanRiskHint::Network
        );
        assert_eq!(
            assess_plan_risk_string(Some("elevated")),
            PlanRiskHint::Elevated
        );
        assert_eq!(
            assess_plan_risk_string(Some("unknown-risk")),
            PlanRiskHint::Elevated,
            "unknown planner risk must remain fail-closed"
        );
        assert!(assess_plan_risk_string(Some("elevated")).elevates());
        assert!(!assess_plan_risk_string(Some("read_only")).elevates());
    }

    #[test]
    fn card_fields_always_include_required_labels() {
        let spec = spec_with(
            vec![WorkflowNode::Leaf(leaf("a", TaskMode::ReadOnly))],
            None,
        );
        let fields = assess_workflow_elevation(&spec, ElevationOptions::default()).card_fields();
        let labels: Vec<_> = fields.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            labels,
            vec!["Goal", "Children", "Writes", "Shell", "Network", "Budget"]
        );
    }

    #[test]
    fn read_only_leaf_with_canonical_tool_families_elevates() {
        let mut inspect = leaf("inspect", TaskMode::ReadOnly);
        inspect.permissions.allowed_tools =
            vec!["Bash".to_string(), "Web".to_string(), "File".to_string()];
        let spec = spec_with(vec![WorkflowNode::Leaf(inspect)], Some("read_only"));
        let elevation = assess_workflow_elevation(&spec, ElevationOptions::default());
        assert!(elevation.elevated, "{elevation:?}");
        assert!(elevation.writes, "{elevation:?}");
        assert!(elevation.shell, "{elevation:?}");
        assert!(elevation.network, "{elevation:?}");
        assert_eq!(
            elevation.reasons,
            vec![
                "writes".to_string(),
                "shell".to_string(),
                "network".to_string()
            ]
        );
    }
}
