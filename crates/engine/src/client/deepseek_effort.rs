//! The single source of truth for DeepSeek's reasoning-effort mapping.
//!
//! DeepSeek exposes the same four thinking tiers on both wires it speaks to
//! us, but spells them differently, and until #5055 each spelling lived inline
//! at its call site — so a documented mapping change meant code archaeology
//! across two files instead of one edit here.
//!
//! Source: <https://api-docs.deepseek.com/api/create-chat-completion> for the
//! Chat Completions `reasoning_effort` values, and DeepSeek's Thinking Mode
//! guide <https://api-docs.deepseek.com/guides/reasoning_model> for the
//! requested-to-actual mapping (verified 2026-09-17).
//!
//! What the wires document, and what this table encodes:
//!
//! | Tier | Chat Completions            | Responses API      |
//! |------|-----------------------------|--------------------|
//! | Off  | `thinking: {type: disabled}` | `effort: "none"`   |
//! | Low  | `reasoning_effort: "low"`   | `effort: "low"`    |
//! | High | `reasoning_effort: "high"`  | `effort: "high"`   |
//! | Max  | `reasoning_effort: "max"`   | `effort: "max"`    |
//!
//! DeepSeek's Thinking Mode guide documents the requested-to-actual effort
//! mapping; thinking is enabled by default with an actual effort of `high`:
//!
//! | Requested | Actual |
//! |-----------|--------|
//! | minimal   | low    |
//! | low       | low    |
//! | medium    | high   |
//! | high      | high   |
//! | xhigh     | high   |
//! | max       | max    |
//! | ultra     | max    |
//!
//! `medium`, `xhigh`, and `ultra` are client-side requests that collapse onto
//! the documented wire tiers — they are not new wire values. When DeepSeek
//! changes the mapping, edit [`DeepseekEffortTier`]'s two wire accessors and
//! [`DEEPSEEK_EFFORT_ALIASES`] below, bump the verified date in this comment,
//! and both call sites move together — that is the whole point of this module.

/// A documented DeepSeek thinking tier, independent of which wire carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeepseekEffortTier {
    /// Thinking disabled.
    Off,
    /// The cheaper real reasoning tier.
    Low,
    /// The default reasoning tier; also where an undocumented `medium` lands.
    High,
    /// The deepest documented tier.
    Max,
}

impl DeepseekEffortTier {
    /// Chat Completions `reasoning_effort` value, or `None` for the off tier —
    /// which that wire expresses with `thinking: {"type": "disabled"}` rather
    /// than an effort string.
    pub(super) fn chat_reasoning_effort(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Low => Some("low"),
            Self::High => Some("high"),
            Self::Max => Some("max"),
        }
    }

    /// Whether this tier enables thinking on the Chat Completions wire.
    pub(super) fn chat_thinking_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Responses API `reasoning.effort` value. Every tier has a documented
    /// label here, including off (`"none"`), so the picker's Off entry stays
    /// off instead of collapsing into a still-thinking low.
    pub(super) fn responses_effort(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

/// Every CodeWhale-side effort spelling that maps onto a documented DeepSeek
/// tier. The empty string is CodeWhale's "unset" effort, which means the
/// default tier.
///
/// The Responses wire normalizes unknown spellings to the default tier, except
/// for `auto`, which is omitted. The Chat wire omits unknown spellings.
pub(super) const DEEPSEEK_EFFORT_ALIASES: &[(&str, DeepseekEffortTier)] = &[
    ("off", DeepseekEffortTier::Off),
    ("disabled", DeepseekEffortTier::Off),
    ("none", DeepseekEffortTier::Off),
    ("false", DeepseekEffortTier::Off),
    ("minimal", DeepseekEffortTier::Low),
    ("low", DeepseekEffortTier::Low),
    ("medium", DeepseekEffortTier::High),
    ("mid", DeepseekEffortTier::High),
    ("high", DeepseekEffortTier::High),
    ("", DeepseekEffortTier::High),
    ("xhigh", DeepseekEffortTier::High),
    ("max", DeepseekEffortTier::Max),
    ("maximum", DeepseekEffortTier::Max),
    ("highest", DeepseekEffortTier::Max),
    ("ultra", DeepseekEffortTier::Max),
    ("ultracode", DeepseekEffortTier::Max),
];

/// The tier CodeWhale falls back to when a caller asks for something this
/// table does not name (a future provider label or a typo).
pub(super) const DEEPSEEK_DEFAULT_EFFORT_TIER: DeepseekEffortTier = DeepseekEffortTier::High;

/// Look up a raw effort string. Case- and whitespace-insensitive.
pub(super) fn deepseek_effort_tier(raw: &str) -> Option<DeepseekEffortTier> {
    let normalized = raw.trim().to_ascii_lowercase();
    DEEPSEEK_EFFORT_ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map(|(_, tier)| *tier)
}

/// Look up a raw effort string, falling back to
/// [`DEEPSEEK_DEFAULT_EFFORT_TIER`] for unknown spellings.
pub(super) fn deepseek_effort_tier_or_default(raw: &str) -> DeepseekEffortTier {
    deepseek_effort_tier(raw).unwrap_or(DEEPSEEK_DEFAULT_EFFORT_TIER)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DeepSeek's documented requested-to-actual mapping (Thinking Mode
    /// guide, verified 2026-09-17). Pin every row so a future edit to the
    /// table cannot silently disagree with the vendor.
    #[test]
    fn vendor_requested_to_actual_mapping_is_pinned() {
        for (requested, tier) in [
            ("minimal", DeepseekEffortTier::Low),
            ("low", DeepseekEffortTier::Low),
            ("medium", DeepseekEffortTier::High),
            ("high", DeepseekEffortTier::High),
            ("xhigh", DeepseekEffortTier::High),
            ("max", DeepseekEffortTier::Max),
            ("ultra", DeepseekEffortTier::Max),
        ] {
            assert_eq!(deepseek_effort_tier(requested), Some(tier), "{requested}");
        }
    }

    #[test]
    fn aliases_are_unique_and_normalized() {
        for (index, (alias, _)) in DEEPSEEK_EFFORT_ALIASES.iter().enumerate() {
            assert_eq!(
                *alias,
                alias.trim().to_ascii_lowercase(),
                "alias {alias:?} must be stored pre-normalized"
            );
            assert!(
                !DEEPSEEK_EFFORT_ALIASES[..index]
                    .iter()
                    .any(|(seen, _)| seen == alias),
                "duplicate alias {alias:?}"
            );
        }
    }

    #[test]
    fn lookup_is_case_and_whitespace_insensitive() {
        assert_eq!(
            deepseek_effort_tier("  MAX "),
            Some(DeepseekEffortTier::Max)
        );
        assert_eq!(deepseek_effort_tier("auto"), None);
        assert_eq!(
            deepseek_effort_tier_or_default("auto"),
            DeepseekEffortTier::High
        );
    }

    #[test]
    fn ultra_and_legacy_ultracode_alias_resolve_to_max() {
        assert_eq!(deepseek_effort_tier("ultra"), Some(DeepseekEffortTier::Max));
        assert_eq!(
            deepseek_effort_tier("ultracode"),
            Some(DeepseekEffortTier::Max)
        );
        assert_eq!(
            deepseek_effort_tier_or_default("ultra").responses_effort(),
            "max"
        );
    }

    #[test]
    fn every_tier_has_both_wire_spellings() {
        for tier in [
            DeepseekEffortTier::Off,
            DeepseekEffortTier::Low,
            DeepseekEffortTier::High,
            DeepseekEffortTier::Max,
        ] {
            let responses = tier.responses_effort();
            assert!(!responses.is_empty(), "{tier:?} needs a Responses label");
            match tier {
                DeepseekEffortTier::Off => {
                    assert_eq!(tier.chat_reasoning_effort(), None);
                    assert!(!tier.chat_thinking_enabled());
                }
                _ => {
                    assert_eq!(tier.chat_reasoning_effort(), Some(responses));
                    assert!(tier.chat_thinking_enabled());
                }
            }
        }
    }
}
