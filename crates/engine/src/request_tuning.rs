//! Request-tuning intent carried through CodeWhale request routing (#3024).
//!
//! Request "tuning" here means the optional knobs a caller can attach to an
//! outbound model request that shape *how* the model responds without changing
//! *what* it is asked: the reasoning-effort tier and the maximum number of
//! output tokens. This module only carries that intent between routing layers.
//! Client code is still responsible for translating the intent into each
//! provider's wire format.
//!
//! ## Reasoning-effort enum reuse
//!
//! [`RequestTuning::reasoning_effort`] reuses the canonical
//! [`crate::reasoning_preference::ReasoningEffort`] enum rather than defining a local
//! `Off/Low/Medium/High` copy. That enum is the single source of truth for the
//! effort tiers across the DeepSeek and Codex effort pickers, it is already
//! imported by sibling top-level modules (`auto_reasoning`, `model_routing`),
//! and it carries the provider-normalization logic (`normalize_for_provider`,
//! `api_value_for_provider`) that a future request-tuning consumer will need.
//! Defining a parallel local enum here would duplicate that surface and risk
//! drift, so we import the existing type.
//!
use crate::reasoning_preference::ReasoningEffort;

/// Optional request-tuning knobs a caller may attach to a model request.
///
/// Both fields are `Option`: `None` means "do not tune; use the provider
/// default". This is metadata describing intent — applying it to a wire
/// request is the responsibility of the client layer, not this module.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestTuning {
    /// Desired reasoning-effort tier, or `None` for the provider default.
    ///
    /// Reuses the canonical [`ReasoningEffort`] enum (see module docs).
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Desired maximum number of output tokens, or `None` for the provider
    /// default.
    pub max_output_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_tuning_default_has_no_knobs() {
        let tuning = RequestTuning::default();
        assert_eq!(tuning.reasoning_effort, None);
        assert_eq!(tuning.max_output_tokens, None);
    }

    #[test]
    fn request_tuning_reuses_reasoning_effort_enum() {
        let tuning = RequestTuning {
            reasoning_effort: Some(ReasoningEffort::High),
            max_output_tokens: Some(4096),
        };
        assert_eq!(tuning.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(tuning.max_output_tokens, Some(4096));
    }
}
