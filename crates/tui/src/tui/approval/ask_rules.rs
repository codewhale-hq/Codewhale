//! Save-preview formatting for persistent permission rules.
//!
//! The rules themselves come from
//! `codewhale_engine::core::authority::persistent_rules`, shared with the
//! Runtime approval route. This module deliberately has no modal state or
//! rendering code: views consume the validated rules and their bounded text
//! preview without re-parsing tool inputs.

use codewhale_config::ToolAskRule;
use codewhale_execpolicy::PermissionAction;
use codewhale_localization::{Locale, MessageId, tr};

/// Human-readable preview of rules an approval action would append.
///
/// This is intentionally derived from the already validated persistent-rule
/// candidates; the approval UI must not re-parse tool inputs such as patches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRuleSavePreview {
    pub action: PermissionAction,
    pub rule_count: usize,
    pub entries: Vec<String>,
    pub omitted: usize,
}

impl PermissionRuleSavePreview {
    /// What saving these rules does, in the words the person reads on the
    /// card. The entries below it name each command or file, and `omitted`
    /// counts the rest, so the summary does not repeat a rule count (#6566).
    #[must_use]
    pub fn summary(&self, locale: Locale) -> String {
        let id = match self.action {
            PermissionAction::Allow => MessageId::ApprovalSaveSummaryAllow,
            PermissionAction::Ask => MessageId::ApprovalSaveSummaryAsk,
            PermissionAction::Deny => MessageId::ApprovalSaveSummaryDeny,
        };
        tr(locale, id).into_owned()
    }
}

pub(super) const SAVE_PREVIEW_MAX_ENTRIES: usize = 4;

#[must_use]
pub(super) fn build_save_preview(
    rules: &[ToolAskRule],
    max_entries: usize,
) -> Option<PermissionRuleSavePreview> {
    if rules.is_empty() {
        return None;
    }

    let entries = rules
        .iter()
        .take(max_entries)
        .map(format_save_entry)
        .collect();
    Some(PermissionRuleSavePreview {
        action: rules[0].action,
        rule_count: rules.len(),
        entries,
        omitted: rules.len().saturating_sub(max_entries),
    })
}

/// One saved rule as a plain phrase: what it covers ("run cargo test",
/// "edit src/lib.rs") and where. The card used to print the rule's config
/// syntax (`tool=exec_shell command=… command_exact=true`), which is the
/// storage format, not something a person decides on (#6566).
#[must_use]
fn format_save_entry(rule: &ToolAskRule) -> String {
    let verb = match rule.tool.as_str() {
        "exec_shell" if rule.command_exact => Some("run exactly"),
        "exec_shell" => Some("run"),
        "write_file" => Some("write"),
        "edit_file" => Some("edit"),
        "apply_patch" => Some("change"),
        _ => None,
    };
    let target = rule
        .command
        .as_deref()
        .or(rule.path.as_deref())
        .map(sanitize_preview_value);
    let mut entry = match (verb, target) {
        (Some(verb), Some(target)) => format!("{verb} {target}"),
        (Some(verb), None) => format!("{verb} ({})", sanitize_preview_value(&rule.tool)),
        (None, Some(target)) => format!("{} {target}", sanitize_preview_value(&rule.tool)),
        (None, None) => sanitize_preview_value(&rule.tool),
    };
    if let Some(workspace) = &rule.workspace {
        entry.push_str(" in ");
        entry.push_str(&sanitize_preview_value(workspace));
    }
    entry
}

#[must_use]
fn sanitize_preview_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}
