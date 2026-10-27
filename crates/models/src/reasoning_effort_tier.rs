use serde::{Deserialize, Serialize};

/// Canonical reasoning-effort tiers recorded as configuration facts. This is
/// deliberately an enum rather than free-form text so Work Graph activity can
/// never become a side channel for model reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffortTier {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Ultra,
    Auto,
    Max,
    /// Thinking is enabled, but the provider route exposes no supported
    /// effort tiers. This is an effective receipt, never a requested setting.
    ThinkingEnabledGranularityUnavailable,
    /// The configured route exposes no verified reasoning-control contract.
    /// This is an effective receipt, never a requested setting.
    Unavailable,
}
