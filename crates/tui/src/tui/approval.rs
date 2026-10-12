//! Tool approval system for `DeepSeek` CLI.
//!
//! Hosts the [`ApprovalRequest`] / [`ApprovalView`] pair the engine asks
//! the TUI to present whenever a tool needs human approval, plus the
//! sandbox elevation flow ([`ElevationRequest`] / [`ElevationView`]) that
//! follows a sandbox denial.
//!
//! ## v0.6.7: Codex-style takeover with stakes-based variants (#129)
//!
//! The modal renders as a compact bottom-anchored approval card that preserves
//! transcript context and routes each request to one of two
//! stakes-based variants:
//!
//! - **Benign** (`RiskLevel::Benign`) — read-only ops, MCP discovery,
//!   query-only network. A single `Enter` / `1` / `y` approves once;
//!   `2` / `a` approves for the session.
//! - **Destructive** (`RiskLevel::Destructive`) — file writes, shell
//!   commands that are not proven read-only, patches, MCP actions,
//!   unclassified tools, and any "fetch arbitrary content" surface.
//!   The approval card keeps the destructive badge and
//!   impact summary visible, then lets `Enter` commit the highlighted
//!   option or `y` / `a` / `d` commit directly.
//!
//! The decision events emitted upstream are unchanged
//! (`ViewEvent::ApprovalDecision`), so `ui.rs` and the engine handle
//! both variants without modification. Auto-approve / YOLO bypasses
//! happen *before* the view is constructed (see `tui/ui.rs`); this
//! module always assumes the user is being asked.

#[cfg(test)]
use crate::config::ApprovalDefaultSelection;
use crate::tools::approval_summary::{
    delegated_authority_fields, param_preview, truncate_string_value,
};
use crate::tools::canonical_action::canonical_action_alias;
use codewhale_config::ToolAskRule;
use codewhale_localization::{Locale, MessageId, tr};
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;

#[cfg(test)]
use crate::sandbox::SandboxPolicy;
#[cfg(test)]
use crate::tui::views::{ModalView, ViewAction, ViewEvent};
#[cfg(test)]
use crossterm::event::KeyEvent;

mod ask_rules;
mod elevation;
pub mod policy;
mod previews;
mod view;

pub use ask_rules::PermissionRuleSavePreview;
#[cfg(test)]
use ask_rules::build_save_preview as build_permission_rule_save_preview;
use ask_rules::{
    SAVE_PREVIEW_MAX_ENTRIES, build_persistent_allow_rules, build_persistent_ask_rules,
    build_save_preview,
};
pub use elevation::{ElevationOption, ElevationRequest, ElevationView};
#[cfg(test)]
use previews::apply_patch_preview_lines;
pub(crate) use previews::format_shell_command_for_approval;
use previews::{
    file_write_preview_lines, localize_detail_label, localize_preview_shell_line, param_text,
};
// Keep the existing module path even though production callers only construct
// `ApprovalView`; approval characterization tests inspect its typed options.
#[allow(unused_imports)]
pub use view::ApprovalOption;
pub use view::ApprovalView;

pub use policy::{ApprovalStakes, ToolCategory, classify_stakes, get_tool_category_for_call};

use crate::core::authority::{RiskLevel, classify_risk};

/// User's decision for a pending approval
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewDecision {
    /// Execute this tool once
    Approved,
    /// Approve and don't ask again for this tool type this session
    ApprovedForSession,
    /// Reject the tool execution
    Denied,
    /// Abort the entire turn
    Abort,
}

/// The agent a child approval card belongs to (approvals C1). `None` on the
/// parent's own cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalOwner {
    pub agent_id: String,
    /// Stable user-facing agent label (`App::ensure_agent_label`).
    pub label: String,
    /// Fleet role label, when the roster knows the agent yet.
    pub role: Option<String>,
}

/// Request for user approval of a tool execution
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Unique ID for this tool use
    pub id: String,
    /// Tool being executed
    pub tool_name: String,
    /// Human-readable tool description from the engine
    pub description: String,
    /// One plain sentence naming what the call does ("Search the web for
    /// 'espresso'", "Write notes/espresso.md"), built from the tool name and
    /// its arguments only (E6). The card leads with it.
    pub summary: String,
    /// Workspace the call runs in; card paths are shown relative to it.
    pub workspace: PathBuf,
    /// Tool category
    pub category: ToolCategory,
    /// Stakes-based routing for the compact approval card
    pub risk: RiskLevel,
    /// Derived impact summary for the approval prompt
    pub impacts: Vec<String>,
    /// Tool parameters (for display)
    pub params: Value,
    /// Exact-argument fingerprint, used to scope *denials* (#1617).
    pub approval_key: String,
    /// Lossy / arity-aware fingerprint, used to scope *approvals* so an
    /// "approve for session" covers later flag variants (v0.8.37).
    pub approval_grouping_key: String,
    /// The model's explanation of intent before invoking write tools (#2381).
    /// Displayed in the approval view so users understand *why* the change
    /// is being made before reviewing *what* will change.
    pub intent_summary: Option<String>,
    /// Ask-only persistent rules that can be saved with the approval.
    pub persistent_ask_rules: Vec<ToolAskRule>,
    /// Exact repo-scoped allow rules available for safe approval requests.
    pub persistent_allow_rules: Vec<ToolAskRule>,
    /// The agent that raised this request, when it is a child's card.
    pub owner: Option<ApprovalOwner>,
    /// This file write asks only because the folder has no git repository;
    /// in one it would run without a card. The card says so in one line.
    pub asks_without_git: bool,
}

/// Key approval details rendered prominently in the approval card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDetail {
    pub label: String,
    pub value: String,
    /// Preformatted shell lines for commands that benefit from safe wrapping
    /// or a compact write-file preview. `value` remains the original command.
    pub shell_lines: Option<Vec<String>>,
    pub is_preview: bool,
}

impl ApprovalRequest {
    /// Mechanical repo-law asks are a distinct authority boundary, not an
    /// ordinary risk prompt. The engine stamps this stable prefix when a
    /// `.codewhale/constitution.json` ask rule forces review.
    #[must_use]
    pub fn is_repo_law_prompt(&self) -> bool {
        description_is_repo_law_prompt(&self.description)
    }

    /// Presentation stakes for this request (see [`ApprovalStakes`]).
    #[must_use]
    pub fn stakes(&self) -> ApprovalStakes {
        classify_stakes(&self.tool_name, self.category, self.risk, &self.params)
    }

    #[cfg(test)]
    pub fn new(
        id: &str,
        tool_name: &str,
        description: &str,
        params: &Value,
        approval_key: &str,
    ) -> Self {
        Self::new_with_intent(
            id,
            tool_name,
            description,
            params,
            approval_key,
            None,
            Path::new("/workspace"),
        )
    }

    pub fn new_with_intent(
        id: &str,
        tool_name: &str,
        description: &str,
        params: &Value,
        approval_key: &str,
        intent_summary: Option<&str>,
        workspace: &Path,
    ) -> Self {
        let semantic_tool_name = canonical_action_alias(tool_name, params);
        let category = get_tool_category_for_call(tool_name, params);
        let risk = classify_risk(tool_name, category, params);
        let approval_grouping_key =
            crate::tools::approval_cache::build_approval_grouping_key(tool_name, params).0;
        let persistent_ask_rules =
            build_persistent_ask_rules(semantic_tool_name, params, workspace);
        let persistent_allow_rules = if classify_stakes(tool_name, category, risk, params)
            == ApprovalStakes::Critical
            || description_is_repo_law_prompt(description)
        {
            Vec::new()
        } else {
            build_persistent_allow_rules(
                semantic_tool_name,
                params,
                workspace,
                &persistent_ask_rules,
            )
        };

        Self {
            id: id.to_string(),
            tool_name: tool_name.to_string(),
            description: description.to_string(),
            summary: crate::tools::approval_summary::approval_summary(
                tool_name,
                params,
                Some(workspace),
            ),
            workspace: workspace.to_path_buf(),
            category,
            risk,
            impacts: build_impact_summary(semantic_tool_name, category, params),
            params: params.clone(),
            approval_key: approval_key.to_string(),
            approval_grouping_key,
            intent_summary: intent_summary.and_then(|summary| {
                let summary = summary.trim();
                if summary.is_empty() {
                    None
                } else {
                    Some(summary.to_string())
                }
            }),
            persistent_ask_rules,
            persistent_allow_rules,
            owner: None,
            asks_without_git: false,
        }
    }

    /// Format parameters for display (truncated)
    pub fn params_display(&self) -> String {
        let truncated = truncate_params_value(&self.params, 200);
        serde_json::to_string(&truncated).unwrap_or_else(|_| truncated.to_string())
    }

    /// The plain summary in `locale` (E6, experience mark 4): the same
    /// sentence the English card leads with, translated around the verbatim
    /// command, path or query.
    #[must_use]
    pub fn summary_for_locale(&self, locale: Locale) -> String {
        if locale == Locale::En {
            return self.summary.clone();
        }
        crate::tools::approval_summary::approval_summary_in(
            locale,
            &self.tool_name,
            &self.params,
            Some(&self.workspace),
        )
    }

    pub fn description_for_locale(&self, locale: Locale) -> String {
        match locale {
            Locale::En if self.category == ToolCategory::Shell => {
                "Review the command before it runs.".to_string()
            }
            Locale::En => self.description.clone(),
            _ => localized_description(self.category, locale),
        }
    }

    pub fn impacts_for_locale(&self, locale: Locale) -> Vec<String> {
        let semantic_tool_name = canonical_action_alias(&self.tool_name, &self.params);
        match locale {
            Locale::ZhHans => {
                build_impact_summary_zh_hans(semantic_tool_name, self.category, &self.params)
            }
            _ => self.impacts.clone(),
        }
    }

    #[must_use]
    pub fn can_save_ask_rule(&self) -> bool {
        !self.persistent_ask_rules.is_empty()
    }

    #[must_use]
    pub fn can_save_allow_rule(&self) -> bool {
        !self.persistent_allow_rules.is_empty()
            && self.stakes() != ApprovalStakes::Critical
            && !self.is_repo_law_prompt()
    }

    #[must_use]
    pub fn ask_rule_save_preview(&self) -> Option<PermissionRuleSavePreview> {
        build_save_preview(&self.persistent_ask_rules, SAVE_PREVIEW_MAX_ENTRIES)
    }

    #[must_use]
    pub fn allow_rule_save_preview(&self) -> Option<PermissionRuleSavePreview> {
        self.can_save_allow_rule().then(|| {
            build_save_preview(&self.persistent_allow_rules, SAVE_PREVIEW_MAX_ENTRIES)
                .expect("eligible allow rules are non-empty")
        })
    }

    #[must_use]
    #[cfg(test)]
    pub fn ask_rule_preview(&self) -> Option<String> {
        if self.persistent_ask_rules.is_empty() {
            return None;
        }
        let permissions = codewhale_config::PermissionsToml {
            rules: self.persistent_ask_rules.clone(),
        };
        toml::to_string_pretty(&permissions).ok()
    }

    /// Extract the most important params for the approval card.
    #[must_use]
    pub fn prominent_detail_items(&self, locale: Locale) -> Vec<ApprovalDetail> {
        let semantic_tool_name = canonical_action_alias(&self.tool_name, &self.params);
        build_prominent_details(semantic_tool_name, self.category, &self.params)
            .into_iter()
            .map(|mut detail| {
                if matches!(detail.label.as_str(), "File" | "Path" | "Dir") {
                    detail.value = workspace_relative(&detail.value, &self.workspace);
                }
                detail.label = localize_detail_label(&detail.label, locale).to_string();
                if detail.is_preview
                    && let Some(lines) = detail.shell_lines.as_mut()
                {
                    for line in lines.iter_mut() {
                        *line = localize_preview_shell_line(semantic_tool_name, line, locale)
                            .to_string();
                    }
                    detail.value = lines.join("\n");
                }
                detail
            })
            .collect()
    }
}

/// Show `value` relative to `workspace` when it is an absolute path inside
/// it, so the card never spends a row on the workspace prefix.
fn workspace_relative(value: &str, workspace: &Path) -> String {
    let path = Path::new(value);
    if workspace.as_os_str().is_empty() || !path.is_absolute() {
        return value.to_string();
    }
    match path.strip_prefix(workspace) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_string(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => value.to_string(),
    }
}

pub use crate::mcp::connected_app_server;

fn description_is_repo_law_prompt(description: &str) -> bool {
    description.starts_with("Repo law holds this write:")
        && description.contains(".codewhale/constitution.json")
}

fn mcp_target_hint(tool_name: &str) -> Option<String> {
    let remainder = tool_name.strip_prefix("mcp_")?;
    if remainder.is_empty() {
        None
    } else {
        Some(remainder.to_string())
    }
}

fn build_impact_summary(tool_name: &str, category: ToolCategory, params: &Value) -> Vec<String> {
    let mut impacts = match category {
        ToolCategory::Safe => {
            let mut impacts = vec!["Read-only operation.".to_string()];
            if let Some(path) = param_preview(params, &["path", "ref_id", "uri"], 72) {
                impacts.push(format!("Reads: {path}"));
            }
            impacts
        }
        ToolCategory::FileWrite => {
            let mut impacts =
                vec!["Writes files in the workspace or an approved write scope.".to_string()];
            if let Some(path) = param_preview(params, &["path", "target", "destination"], 72) {
                impacts.push(format!("Writes: {path}"));
            }
            impacts
        }
        ToolCategory::Shell => {
            vec!["Runs a shell command in your workspace.".to_string()]
        }
        ToolCategory::Network => {
            let mut impacts = vec!["May reach network services or remote content.".to_string()];
            if let Some(target) =
                param_preview(params, &["url", "q", "query", "location", "repo"], 96)
            {
                impacts.push(format!("Target: {target}"));
            }
            impacts
        }
        ToolCategory::McpRead => {
            let mut impacts =
                vec!["Reads from a connected app without an obvious local write.".to_string()];
            if let Some(target) = mcp_target_hint(tool_name) {
                impacts.push(format!("Connected app: {target}"));
            }
            impacts
        }
        ToolCategory::McpAction => {
            let mut impacts =
                vec!["Uses a connected app action that may have side effects.".to_string()];
            if let Some(target) = mcp_target_hint(tool_name) {
                impacts.push(format!("Connected app: {target}"));
            }
            impacts
        }
        ToolCategory::Agent if tool_name == "workflow" => {
            // #4126: elevated Workflow plan card — goal, children, capability flags, budget.
            crate::tools::workflow_plan_approval::analyze_workflow_plan_approval(params)
                .approval_impacts()
        }
        ToolCategory::Agent => {
            let mut impacts = vec![
                "Starts or checks on an agent; the agent still asks for its own approvals."
                    .to_string(),
            ];
            if let Some(kind) = param_preview(params, &["type"], 40) {
                impacts.push(format!("Agent type: {kind}"));
            }
            impacts
        }
        ToolCategory::Unknown => {
            let mut impacts = vec![
                "Tool is not classified. Review params carefully before approving.".to_string(),
            ];
            if let Some(target) = param_preview(
                params,
                &["path", "cmd", "command", "url", "q", "query", "ref_id"],
                96,
            ) {
                impacts.push(format!("Primary input: {target}"));
            }
            impacts
        }
    };
    impacts.extend(delegated_authority_impacts(tool_name, params, false));
    impacts
}

fn localized_description(category: ToolCategory, locale: Locale) -> String {
    match category {
        ToolCategory::Safe => tr(locale, MessageId::ApprovalDescSafe).to_string(),
        ToolCategory::FileWrite => tr(locale, MessageId::ApprovalDescFileWrite).to_string(),
        ToolCategory::Shell => tr(locale, MessageId::ApprovalDescShell).to_string(),
        ToolCategory::Network => tr(locale, MessageId::ApprovalDescNetwork).to_string(),
        ToolCategory::McpRead => tr(locale, MessageId::ApprovalDescMcpRead).to_string(),
        ToolCategory::McpAction => tr(locale, MessageId::ApprovalDescMcpAction).to_string(),
        ToolCategory::Agent => tr(locale, MessageId::ApprovalDescAgent).to_string(),
        ToolCategory::Unknown => tr(locale, MessageId::ApprovalDescUnknown).to_string(),
    }
}

fn build_impact_summary_zh_hans(
    tool_name: &str,
    category: ToolCategory,
    params: &Value,
) -> Vec<String> {
    let locale = Locale::ZhHans;
    let mut impacts = match category {
        ToolCategory::Safe => {
            let mut impacts = vec![tr(locale, MessageId::ApprovalImpactSafe).to_string()];
            if let Some(path) = param_preview(params, &["path", "ref_id", "uri"], 72) {
                impacts.push(format!("读取：{path}"));
            }
            impacts
        }
        ToolCategory::FileWrite => {
            let mut impacts = vec![tr(locale, MessageId::ApprovalImpactFileWrite).to_string()];
            if let Some(path) = param_preview(params, &["path", "target", "destination"], 72) {
                impacts.push(format!("写入：{path}"));
            }
            impacts
        }
        ToolCategory::Shell => {
            vec![tr(locale, MessageId::ApprovalImpactShell).to_string()]
        }
        ToolCategory::Network => {
            let mut impacts = vec![tr(locale, MessageId::ApprovalImpactNetwork).to_string()];
            if let Some(target) =
                param_preview(params, &["url", "q", "query", "location", "repo"], 96)
            {
                impacts.push(format!("目标：{target}"));
            }
            impacts
        }
        ToolCategory::McpRead => {
            let mut impacts = vec![tr(locale, MessageId::ApprovalImpactMcpRead).to_string()];
            if let Some(target) = mcp_target_hint(tool_name) {
                impacts.push(format!("已连接应用：{target}"));
            }
            impacts
        }
        ToolCategory::McpAction => {
            let mut impacts = vec![tr(locale, MessageId::ApprovalImpactMcpAction).to_string()];
            if let Some(target) = mcp_target_hint(tool_name) {
                impacts.push(format!("已连接应用：{target}"));
            }
            impacts
        }
        ToolCategory::Agent => {
            let mut impacts = vec![tr(locale, MessageId::ApprovalImpactAgent).to_string()];
            if let Some(kind) = param_preview(params, &["type"], 40) {
                impacts.push(format!("子代理类型：{kind}"));
            }
            impacts
        }
        ToolCategory::Unknown => {
            let mut impacts = vec![tr(locale, MessageId::ApprovalImpactUnknown).to_string()];
            if let Some(target) = param_preview(
                params,
                &["path", "cmd", "command", "url", "q", "query", "ref_id"],
                96,
            ) {
                impacts.push(format!("主要输入：{target}"));
            }
            impacts
        }
    };
    impacts.extend(delegated_authority_impacts(tool_name, params, true));
    impacts
}

/// Tools that hand work to a later, unattended run (a durable task or a
/// scheduled automation) and can ask for authority of their own.
fn delegated_authority_impacts(tool_name: &str, params: &Value, zh: bool) -> Vec<String> {
    let separator = if zh { "：" } else { ": " };
    delegated_authority_fields(tool_name, params, zh)
        .into_iter()
        .map(|(label, value)| format!("{label}{separator}{value}"))
        .collect()
}

fn build_prominent_details(
    tool_name: &str,
    category: ToolCategory,
    params: &Value,
) -> Vec<ApprovalDetail> {
    let mut details: Vec<ApprovalDetail> = delegated_authority_fields(tool_name, params, false)
        .into_iter()
        .map(|(label, value)| ApprovalDetail {
            label,
            value,
            shell_lines: None,
            is_preview: false,
        })
        .collect();
    match category {
        ToolCategory::Shell => {
            if let Some(command) = param_text(params, &["command", "cmd"]) {
                details.push(ApprovalDetail {
                    label: "Command".to_string(),
                    shell_lines: Some(format_shell_command_for_approval(&command)),
                    value: command,
                    is_preview: false,
                });
            }
            if let Some(workdir) = param_preview(params, &["workdir", "cwd"], 96) {
                details.push(ApprovalDetail {
                    label: "Dir".to_string(),
                    value: workdir,
                    shell_lines: None,
                    is_preview: false,
                });
            }
        }
        ToolCategory::FileWrite => {
            if let Some(path) = param_preview(params, &["path", "target", "destination"], 200) {
                details.push(ApprovalDetail {
                    label: "File".to_string(),
                    value: path,
                    shell_lines: None,
                    is_preview: false,
                });
            }
            if let Some(preview_lines) = file_write_preview_lines(tool_name, params) {
                details.push(ApprovalDetail {
                    label: "Preview".to_string(),
                    value: preview_lines.join("\n"),
                    shell_lines: Some(preview_lines),
                    is_preview: true,
                });
            }
        }
        ToolCategory::Safe => {
            if let Some(path) = param_preview(params, &["path", "ref_id", "uri"], 200) {
                details.push(ApprovalDetail {
                    label: "Path".to_string(),
                    value: path,
                    shell_lines: None,
                    is_preview: false,
                });
            }
        }
        ToolCategory::Network => {
            if let Some(target) =
                param_preview(params, &["url", "q", "query", "location", "repo"], 200)
            {
                details.push(ApprovalDetail {
                    label: "Target".to_string(),
                    value: target,
                    shell_lines: None,
                    is_preview: false,
                });
            }
        }
        ToolCategory::Agent if tool_name == "workflow" => {
            // #4126: elevated Workflow plan card fields.
            let summary =
                crate::tools::workflow_plan_approval::analyze_workflow_plan_approval(params);
            for (label, value) in summary.card_fields() {
                details.push(ApprovalDetail {
                    label: label.to_string(),
                    value,
                    shell_lines: None,
                    is_preview: false,
                });
            }
        }
        ToolCategory::Agent => {
            if let Some(action) = param_preview(params, &["action"], 40) {
                details.push(ApprovalDetail {
                    label: "Action".to_string(),
                    value: action,
                    shell_lines: None,
                    is_preview: false,
                });
            }
            if let Some(kind) = param_preview(params, &["type"], 40) {
                details.push(ApprovalDetail {
                    label: "Type".to_string(),
                    value: kind,
                    shell_lines: None,
                    is_preview: false,
                });
            }
            if let Some(prompt) = param_preview(params, &["prompt", "task", "message"], 200) {
                details.push(ApprovalDetail {
                    label: "Prompt".to_string(),
                    value: prompt,
                    shell_lines: None,
                    is_preview: false,
                });
            }
        }
        ToolCategory::McpRead | ToolCategory::McpAction | ToolCategory::Unknown => {
            if let Some(input) = param_preview(
                params,
                &["command", "cmd", "path", "url", "q", "query", "ref_id"],
                200,
            ) {
                details.push(ApprovalDetail {
                    label: "Input".to_string(),
                    value: input,
                    shell_lines: None,
                    is_preview: false,
                });
            }
        }
    }
    details
}

fn truncate_params_value(value: &Value, max_len: usize) -> Value {
    match value {
        Value::Object(map) => {
            let truncated = map
                .iter()
                .map(|(key, val)| (key.clone(), truncate_params_value(val, max_len)))
                .collect();
            Value::Object(truncated)
        }
        Value::Array(items) => {
            let truncated_items = items
                .iter()
                .map(|val| truncate_params_value(val, max_len))
                .collect();
            Value::Array(truncated_items)
        }
        Value::String(text) => Value::String(truncate_string_value(text, max_len)),
        other => {
            let rendered = other.to_string();
            if rendered.chars().count() > max_len {
                Value::String(truncate_string_value(&rendered, max_len))
            } else {
                other.clone()
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;
