//! Approval stakes for the takeover modal.
//!
//! The risk decision itself lives in `crate::core::authority::risk`. This maps
//! it onto the three visual weights the approval views render.

use crate::core::authority::RiskLevel;
use codewhale_execpolicy::canonical_action::canonical_action_alias;
use serde_json::Value;

// Tool categorization is runtime policy (the extension host and auto-review
// both consult it), so it lives in `core::authority`; re-exported here for the
// approval views.
pub use crate::core::authority::{ToolCategory, get_tool_category_for_call};

/// Presentation-level stakes for the approval prompt (#3883 follow-up).
///
/// `RiskLevel` drives keymaps and stays conservative ("not provably
/// read-only" is `Destructive`), but rendering everything in that bucket
/// as a red DESTRUCTIVE takeover made routine file edits and build
/// commands read like emergencies. Stakes split presentation three ways:
///
/// - `Routine` - provably read-only; minimal chrome.
/// - `Elevated` - ordinary state-touching work (edits, builds, MCP
///   actions); a calm approval, not a warning.
/// - `Critical` - genuinely destructive, publish-like, or
///   secret-touching per `ToolActionKind`; keeps the strong styling and
///   the policy semantics lines.
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
    if !crate::core::authority::auto_review::credential_upload_texts(tool_name, params).is_empty() {
        return ApprovalStakes::Critical;
    }
    if matches!(risk, RiskLevel::Benign) {
        return ApprovalStakes::Routine;
    }
    let semantic_name = canonical_action_alias(tool_name, params);
    match crate::core::authority::auto_review::ToolActionKind::from_tool_call(
        semantic_name,
        params,
        category,
        None,
    ) {
        crate::core::authority::auto_review::ToolActionKind::Publish
        | crate::core::authority::auto_review::ToolActionKind::Destructive => {
            ApprovalStakes::Critical
        }
        _ => ApprovalStakes::Elevated,
    }
}
