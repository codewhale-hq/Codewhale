//! Reviewed local-template facts replacing the provider-wide low/medium/high
//! assumption in the vLLM Chat adapter. A server speaks a protocol; its model
//! template owns the effort ladder. Unknown models and other routes stay intact.
use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ProviderKind;

/// A reviewed model-template ladder. `high` is the on switch for boolean models;
/// `max` is the existing UI identifier for the deepest supported effort.
#[derive(Debug, Deserialize)]
pub struct LocalReasoningSpec {
    /// Canonical served-name prefixes, with a required suffix boundary.
    pub prefixes: Vec<String>,
    /// Distinct selectable tiers, in ascending order.
    pub tiers: Vec<String>,
    /// Lowest meaningful thinking tier when no preference was saved.
    pub default: String,
    aliases: HashMap<String, String>,
    wire: HashMap<String, Option<String>>,
}

impl LocalReasoningSpec {
    /// Normalize old saved choices without changing their stored value. Unknown
    /// values are not guessed: the ordinary adapter/server keeps validating them.
    #[must_use]
    pub fn normalize(&self, effort: &str) -> Option<&str> {
        let effort = effort.trim().to_ascii_lowercase();
        let effort = match effort.as_str() {
            "minimal" | "minimum" | "light" => "low",
            "mid" => "medium",
            "disabled" | "none" | "false" => "off",
            "maximum" | "ultra" | "ultracode" => "max",
            "" => return Some(&self.default),
            other => other,
        };
        self.tiers
            .iter()
            .find(|tier| tier.as_str() == effort)
            .map(String::as_str)
            .or_else(|| self.aliases.get(effort).map(String::as_str))
    }
}

/// Find a fact only for a reviewed served-name family, including repository IDs.
/// A version such as Qwen3.8-27B2 must not inherit Qwen3.8-27B's contract.
#[must_use]
pub fn local_reasoning_spec(model: &str) -> Option<&'static LocalReasoningSpec> {
    static SPECS: OnceLock<Vec<LocalReasoningSpec>> = OnceLock::new();
    let specs = SPECS.get_or_init(|| {
        serde_json::from_str(include_str!("local_reasoning.json"))
            .expect("reviewed local reasoning facts")
    });
    let normalized = model.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    let name = normalized.rsplit('/').next()?;
    // Separately named Thinking variants can be always-on and have a
    // different template; the reviewed base-family fact does not cover them.
    if name.contains("thinking") {
        return None;
    }
    specs.iter().find(|spec| {
        spec.prefixes.iter().any(|prefix| {
            name.strip_prefix(prefix.as_str())
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(['-', ':']))
        })
    })
}

/// Shape only thinking fields after the generic Chat controls. Route identity,
/// authentication, message history, budgets and sampling remain caller-owned.
/// SGLang endpoints using the local vLLM wire consume the nested spelling too.
pub fn apply_local_reasoning_controls(
    body: &mut Value,
    provider: ProviderKind,
    model: &str,
    effort: Option<&str>,
) {
    if provider != ProviderKind::Vllm {
        return;
    }
    let Some((spec, tier)) =
        local_reasoning_spec(model).and_then(|spec| Some((spec, spec.normalize(effort?)?)))
    else {
        return;
    };
    let Some(object) = body.as_object_mut() else {
        return;
    };
    let kwargs = object
        .entry("chat_template_kwargs")
        .or_insert_with(|| json!({}));
    let Some(kwargs) = kwargs.as_object_mut() else {
        return;
    };
    kwargs.insert("enable_thinking".into(), json!(tier != "off"));
    match &spec.wire[tier] {
        Some(wire) => {
            kwargs.insert("reasoning_effort".into(), json!(wire));
        }
        None => {
            kwargs.remove("reasoning_effort");
        }
    }
    // Template effort is an extension, not a promise about the server's
    // top-level OpenAI enum. Older vLLM schemas reject max/xhigh before
    // rendering. Both reviewed vLLM/SGLang routes accept template kwargs.
    object.remove("reasoning_effort");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_tiers_aliases_and_defaults_all_have_wire_contracts() {
        let specs: Vec<LocalReasoningSpec> =
            serde_json::from_str(include_str!("local_reasoning.json")).unwrap();
        assert!(!specs.is_empty());
        for spec in specs {
            assert!(!spec.prefixes.is_empty());
            assert_eq!(spec.tiers.len(), spec.wire.len());
            for tier in spec.tiers.iter().chain(spec.aliases.values()) {
                assert!(spec.wire.contains_key(tier));
            }
            assert!(spec.tiers.contains(&spec.default));
        }
    }

    #[test]
    fn qwen38_high_uses_xhigh_and_preserves_request_fields() {
        for model in ["Qwen3.8-27B-NVFP4", "nvidia/Qwen3.8-Flash-Next-NVFP4"] {
            let mut body = json!({"model":model,"messages":[{"role":"user","content":"hello"}],"max_tokens":123,"stream":true,"tools":[{"type":"function"}],"chat_template_kwargs":{"custom":42},"reasoning_effort":"high"});
            let before = body.clone();
            apply_local_reasoning_controls(&mut body, ProviderKind::Vllm, model, Some("high"));
            assert!(body.get("reasoning_effort").is_none());
            assert_eq!(body["chat_template_kwargs"]["reasoning_effort"], "xhigh");
            assert_eq!(body["chat_template_kwargs"]["custom"], 42);
            for field in ["model", "messages", "max_tokens", "stream", "tools"] {
                assert_eq!(body[field], before[field]);
            }
        }
    }

    #[test]
    fn legacy_preference_spellings_keep_their_effective_tier() {
        let spec = local_reasoning_spec("Qwen3.8-27B-NVFP4").unwrap();
        for (saved, expected) in [
            ("maximum", "max"),
            ("ultra", "max"),
            ("ultracode", "max"),
            ("xhigh", "max"),
            ("minimum", "low"),
            ("minimal", "low"),
            ("light", "low"),
            ("mid", "medium"),
            ("disabled", "off"),
            ("none", "off"),
            ("false", "off"),
            ("", "low"),
            (" HIGH ", "max"),
        ] {
            assert_eq!(spec.normalize(saved), Some(expected), "{saved}");
        }
    }

    #[test]
    fn every_reviewed_tier_and_legacy_alias_has_a_valid_wire_value() {
        let fixtures = [
            (
                "Qwen3.8-27B-NVFP4",
                [
                    None,
                    Some("low"),
                    Some("medium"),
                    Some("xhigh"),
                    Some("xhigh"),
                    Some("xhigh"),
                ],
            ),
            (
                "GLM-5.3-Flash-NVFP4",
                [
                    Some("low"),
                    Some("low"),
                    Some("high"),
                    Some("high"),
                    Some("max"),
                    Some("max"),
                ],
            ),
            (
                "DeepSeek-V4-Flash-Vision-Exp",
                [
                    None,
                    Some("low"),
                    Some("high"),
                    Some("high"),
                    Some("max"),
                    Some("max"),
                ],
            ),
        ];
        for (model, expected) in fixtures {
            for (effort, wire) in ["off", "low", "medium", "high", "max", "xhigh"]
                .into_iter()
                .zip(expected)
            {
                let mut body = json!({});
                apply_local_reasoning_controls(&mut body, ProviderKind::Vllm, model, Some(effort));
                assert!(body.get("reasoning_effort").is_none());
                assert_eq!(
                    body["chat_template_kwargs"]["reasoning_effort"].as_str(),
                    wire,
                    "{model}: {effort}"
                );
                assert_eq!(
                    body["chat_template_kwargs"]["enable_thinking"],
                    wire.is_some(),
                    "{model}: {effort}"
                );
            }
        }
    }

    #[test]
    fn boolean_templates_only_get_the_thinking_switch() {
        for model in ["Ornith-1.5-35B-A3B-NVFP4", "Qwen3.6-35B-A3B-NVFP4-Fast"] {
            for effort in ["off", "low", "medium", "high", "max"] {
                let mut body = json!({"reasoning_effort":"high","chat_template_kwargs":{"reasoning_effort":"medium"}});
                apply_local_reasoning_controls(&mut body, ProviderKind::Vllm, model, Some(effort));
                assert!(body.get("reasoning_effort").is_none());
                assert!(
                    body["chat_template_kwargs"]
                        .get("reasoning_effort")
                        .is_none()
                );
                assert_eq!(
                    body["chat_template_kwargs"]["enable_thinking"],
                    effort != "off"
                );
            }
        }
    }

    #[test]
    fn unreviewed_models_non_vllm_routes_and_unspecified_effort_are_untouched() {
        for model in [
            "Qwen3.8-27B2",
            "Qwen3.8-235B",
            "Qwen3.8-27B-Thinking",
            "Qwen3.6-35B-A3B-Thinking",
            "DeepSeek-V4.1-Flash-1017",
            "custom-model",
        ] {
            assert!(local_reasoning_spec(model).is_none());
        }
        for (provider, model, effort) in [
            (ProviderKind::Openai, "Qwen3.8-27B-NVFP4", Some("high")),
            (ProviderKind::Vllm, "custom-model", Some("high")),
            (ProviderKind::Vllm, "Qwen3.8-27B-NVFP4", None),
            (ProviderKind::Vllm, "Qwen3.8-27B-NVFP4", Some("unknown")),
        ] {
            let mut body = json!({"reasoning_effort":"high","chat_template_kwargs":{"custom":42}});
            let before = body.clone();
            apply_local_reasoning_controls(&mut body, provider, model, effort);
            assert_eq!(body, before);
        }
    }
}
