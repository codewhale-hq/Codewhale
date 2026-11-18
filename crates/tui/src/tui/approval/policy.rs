//! Approval stakes for the takeover modal.
//!
//! The risk decision itself lives in `codewhale_engine::core::authority::risk`,
//! and the stakes split lives beside the persistent-rule eligibility it gates in
//! `codewhale_engine::core::authority::persistent_rules`.

// Tool categorization is runtime policy (the extension host and auto-review
// both consult it), so it lives in `core::authority`; re-exported here for the
// approval views.
pub use codewhale_engine::core::authority::persistent_rules::{ApprovalStakes, classify_stakes};
pub use codewhale_engine::core::authority::{ToolCategory, get_tool_category_for_call};
