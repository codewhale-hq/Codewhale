use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a decision model answered and what it cost. Probabilities are basis
/// points (0..=10000) so the receipt stays `Eq` and never re-renders floats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoRouteDecisionEvidence {
    /// The tier the decision model chose (`fast` | `strong`).
    pub choice: String,
    pub probabilities_bp: BTreeMap<String, u16>,
    pub confidence_bp: u16,
    pub min_confidence_bp: u16,
    /// `[auto] cost_saving` kept the fast tier because the strong tier's
    /// probability was below the cost-saving floor.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cost_saving_kept_fast: bool,
    /// The reasoning effort the decision applied, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// `usage.cost` exactly as the provider reported it (USD decimal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_reported_cost_usd: Option<String>,
    pub latency_ms: u64,
    /// The dated model snapshot the provider echoed, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
}
