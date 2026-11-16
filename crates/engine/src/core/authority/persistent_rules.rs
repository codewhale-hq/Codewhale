use std::path::Path;

use codewhale_execpolicy::ToolAskRule;
use codewhale_execpolicy::canonical_action::canonical_action_alias;
use codewhale_execpolicy::command_safety::{SafetyLevel, analyze_command};
use serde_json::Value;

use super::auto_review::{ToolActionKind, credential_upload_texts};
use super::{RiskLevel, ToolCategory, classify_risk, get_tool_category_for_call};

/// Presentation-level stakes for an approval prompt.
///
/// `RiskLevel` stays conservative ("not provably read-only" is
/// `Destructive`); stakes split it three ways for presentation and for
/// deciding whether a standing allow may be offered:
///
/// - `Routine` - provably read-only.
/// - `Elevated` - ordinary state-touching work (edits, builds, MCP actions).
/// - `Critical` - destructive, publish-like, or secret-touching per
///   `ToolActionKind`; never eligible for a standing allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalStakes {
    Routine,
    Elevated,
    Critical,
}

#[must_use]
pub fn classify_stakes(
    tool_name: &str,
    category: ToolCategory,
    risk: RiskLevel,
    params: &Value,
) -> ApprovalStakes {
    if !credential_upload_texts(tool_name, params).is_empty() {
        return ApprovalStakes::Critical;
    }
    if matches!(risk, RiskLevel::Benign) {
        return ApprovalStakes::Routine;
    }
    let semantic_name = canonical_action_alias(tool_name, params);
    match ToolActionKind::from_tool_call(semantic_name, params, category, None) {
        ToolActionKind::Publish | ToolActionKind::Destructive => ApprovalStakes::Critical,
        _ => ApprovalStakes::Elevated,
    }
}

#[must_use]
pub fn is_repo_law_prompt(description: &str) -> bool {
    description.starts_with("Repo law holds this write:")
        && description.contains(".codewhale/constitution.json")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersistentRuleCandidates {
    pub ask: Vec<ToolAskRule>,
    pub allow: Vec<ToolAskRule>,
}

#[must_use]
pub fn persistent_rule_candidates(
    tool_name: &str,
    params: &Value,
    description: &str,
    workspace: &Path,
) -> PersistentRuleCandidates {
    let semantic = canonical_action_alias(tool_name, params);
    let ask = exact_ask_rules(semantic, params, workspace);
    let category = get_tool_category_for_call(tool_name, params);
    let risk = classify_risk(tool_name, category, params);
    let allow = if classify_stakes(tool_name, category, risk, params) == ApprovalStakes::Critical
        || is_repo_law_prompt(description)
    {
        Vec::new()
    } else {
        exact_workspace_allow_rules(semantic, params, workspace, &ask)
    };
    PersistentRuleCandidates { ask, allow }
}

fn exact_ask_rules(semantic: &str, params: &Value, workspace: &Path) -> Vec<ToolAskRule> {
    match semantic {
        "exec_shell" => exec_shell_ask_rules(params),
        "write_file" | "edit_file" => file_write_ask_rules(semantic, params, workspace),
        "apply_patch" => apply_patch_ask_rules(params, workspace),
        _ => Vec::new(),
    }
}

fn exact_workspace_allow_rules(
    semantic: &str,
    params: &Value,
    workspace: &Path,
    exact_rules: &[ToolAskRule],
) -> Vec<ToolAskRule> {
    if exact_rules.is_empty() {
        return Vec::new();
    }
    if semantic == "exec_shell" {
        let Some(command) = params.get("command").and_then(Value::as_str) else {
            return Vec::new();
        };
        if !matches!(
            analyze_command(command).level,
            SafetyLevel::Safe | SafetyLevel::WorkspaceSafe
        ) {
            return Vec::new();
        }
    }
    let workspace = workspace.to_string_lossy();
    let Some(workspace) = codewhale_execpolicy::normalize_workspace_scope(workspace.as_ref())
    else {
        return Vec::new();
    };
    exact_rules
        .iter()
        .cloned()
        .map(|rule| rule.into_exact_workspace_allow(workspace.clone()))
        .collect()
}

fn exec_shell_ask_rules(params: &Value) -> Vec<ToolAskRule> {
    let Some(command) = params
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
    else {
        return Vec::new();
    };
    vec![ToolAskRule::exec_shell(command)]
}

fn file_write_ask_rules(tool_name: &str, params: &Value, workspace: &Path) -> Vec<ToolAskRule> {
    let Some(path) = params
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return Vec::new();
    };
    let workspace = workspace.to_string_lossy();
    let Some(relative) =
        codewhale_execpolicy::normalize_workspace_relative_path(path, workspace.as_ref())
            .filter(|relative| !relative.is_empty())
    else {
        return Vec::new();
    };
    vec![ToolAskRule::file_path(tool_name, relative)]
}

fn apply_patch_ask_rules(params: &Value, workspace: &Path) -> Vec<ToolAskRule> {
    let Ok(preflight) = codewhale_tools::patch_preflight::preflight_apply_patch(params) else {
        return Vec::new();
    };
    let workspace = workspace.to_string_lossy();
    let mut rules = Vec::new();
    for path in preflight.touched_files {
        let Some(relative) =
            codewhale_execpolicy::normalize_workspace_relative_path(&path, workspace.as_ref())
                .filter(|relative| !relative.is_empty())
        else {
            return Vec::new();
        };
        let rule = ToolAskRule::file_path("apply_patch", relative);
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    rules
}
