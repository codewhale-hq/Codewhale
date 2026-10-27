use std::error::Error;
use std::fmt;

use codewhale_config::{ProviderKind, opencode_go_model_id};
use serde::{Deserialize, Serialize};

/// High-level model family used for shared identity affordances across clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelFamily {
    DeepSeek,
    Anthropic,
    OpenAI,
    Google,
    Meta,
    Mistral,
    Qwen,
    Grok,
    Cohere,
    GptOss,
    Inferencer,
}

/// Metadata for a single model entry in the registry.
///
/// Each model has a canonical `id` used by the provider, a list of `aliases`
/// that users may reference, and capability flags indicating whether the model
/// supports tool use and reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// The canonical model identifier used by the provider (e.g. `"deepseek-v4-pro"`).
    pub id: String,
    /// The provider that serves this model.
    pub provider: ProviderKind,
    /// Alternative names that users can use to reference this model (case-insensitive).
    pub aliases: Vec<String>,
    /// Whether this model supports tool/function calling.
    pub supports_tools: bool,
    /// Whether this model supports extended reasoning.
    pub supports_reasoning: bool,
}

/// The result of resolving a user-requested model name to a concrete model entry.
///
/// Contains the resolved [`ModelInfo`], whether a fallback was used, and the
/// chain of resolution strategies that were attempted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResolution {
    /// The original model name requested by the user, if any.
    pub requested: Option<String>,
    /// The concrete model that was resolved.
    pub resolved: ModelInfo,
    /// Whether the provider-owned default was used because no model was requested.
    pub used_fallback: bool,
    /// The ordered list of resolution strategies that were attempted.
    pub fallback_chain: Vec<String>,
}

/// A model lookup that cannot name a provider-owned result truthfully.
///
/// The registry is metadata, not route authority. In particular, a missing
/// provider must never be interpreted as permission to select DeepSeek (or
/// any other provider), and an explicit provider with no registered models
/// must never fall through to another provider's first catalog row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelResolutionError {
    /// No provider was supplied. A model name alone is never route authority,
    /// even when it happens to match one catalog entry.
    ProviderRequired { requested: Option<String> },
    /// The caller selected a provider for which this registry has no model
    /// metadata to return.
    ProviderHasNoModels {
        provider: ProviderKind,
        requested: Option<String>,
    },
    /// The caller selected a provider, then requested a model that provider's
    /// registry rows and explicit pass-through contract do not serve.
    ModelNotAvailableForProvider {
        provider: ProviderKind,
        requested: String,
    },
    /// The provider declares a default model, but the registry cannot return a
    /// matching provider-owned row for it.
    ProviderDefaultUnavailable {
        provider: ProviderKind,
        default_model: String,
    },
}

impl fmt::Display for ModelResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderRequired {
                requested: Some(requested),
            } => write!(
                formatter,
                "model '{requested}' does not identify an unambiguous provider; select a provider explicitly"
            ),
            Self::ProviderRequired { requested: None } => {
                formatter.write_str("model resolution requires an explicit provider")
            }
            Self::ProviderHasNoModels {
                provider,
                requested: Some(requested),
            } => write!(
                formatter,
                "provider '{}' has no registered model for '{requested}'",
                provider.as_str()
            ),
            Self::ProviderHasNoModels {
                provider,
                requested: None,
            } => write!(
                formatter,
                "provider '{}' has no registered default model",
                provider.as_str()
            ),
            Self::ModelNotAvailableForProvider {
                provider,
                requested,
            } => write!(
                formatter,
                "model '{requested}' is not available from provider '{}'",
                provider.as_str()
            ),
            Self::ProviderDefaultUnavailable {
                provider,
                default_model,
            } => write!(
                formatter,
                "provider '{}' declares default model '{default_model}', but that model is not registered for the provider",
                provider.as_str()
            ),
        }
    }
}

impl Error for ModelResolutionError {}

/// A registry of supported models and their aliases, used to resolve user-facing
/// model names to concrete provider-specific model entries.
///
/// The default registry is populated with all built-in models across supported
/// providers (DeepSeek, NVIDIA NIM, OpenAI-compatible, and others).
#[derive(Debug, Clone)]
pub struct ModelRegistry {
    models: Vec<ModelInfo>,
}

/// Creates a registry pre-populated with all built-in models and their aliases.
impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new(
            codewhale_config::catalog::reviewed::bundled_reviewed()
                .selections
                .iter()
                .map(|row| ModelInfo {
                    id: row.id.clone(),
                    provider: ProviderKind::parse_config_identity(&row.provider)
                        .expect("validated bundled provider selector"),
                    aliases: row.aliases.clone(),
                    supports_tools: row.supports_tools,
                    supports_reasoning: row.supports_reasoning,
                })
                .collect(),
        )
    }
}

impl ModelRegistry {
    /// Creates a new registry from a list of [`ModelInfo`] entries.
    ///
    #[must_use]
    pub fn new(models: Vec<ModelInfo>) -> Self {
        Self { models }
    }

    /// Returns a clone of all models in the registry.
    #[must_use]
    pub fn list(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    /// Returns whether a selector is known only outside the selected provider.
    ///
    /// This is rejection metadata, never route authority: callers may use it
    /// to reject a clearly foreign model, but must not use the matching row to
    /// select a provider or credential slot.
    #[must_use]
    pub fn is_known_for_other_provider(
        &self,
        requested: &str,
        selected_provider: ProviderKind,
    ) -> bool {
        let known_here = self
            .models
            .iter()
            .any(|model| model.provider == selected_provider && model_matches(model, requested));
        !known_here
            && self
                .models
                .iter()
                .any(|model| model.provider != selected_provider && model_matches(model, requested))
    }

    /// Resolves a user-requested model name to a concrete [`ModelInfo`].
    ///
    /// Resolution follows this priority order:
    /// 1. If the provider is Ollama, the requested name is used as-is (to
    ///    support arbitrary local model tags like `qwen2.5-coder:7b`).
    /// 2. If a `provider_hint` is given, search for a model matching that
    ///    provider whose id or alias matches the request (case-insensitive).
    /// 3. Provider-specific pass-through contracts may preserve arbitrary
    ///    model ids.
    /// 4. An omitted model falls back to the explicitly selected provider's
    ///    documented default.
    /// 5. A requested model outside that provider fails closed. Model text is
    ///    metadata and never authorizes a provider or credential switch.
    pub fn resolve(
        &self,
        requested: Option<&str>,
        provider_hint: Option<ProviderKind>,
    ) -> Result<ModelResolution, ModelResolutionError> {
        let requested = requested.filter(|name| !name.trim().is_empty());
        let mut fallback_chain = Vec::new();
        let Some(provider) = provider_hint else {
            return Err(ModelResolutionError::ProviderRequired {
                requested: requested.map(ToOwned::to_owned),
            });
        };

        if let Some(name) = requested {
            fallback_chain.push(format!("requested:{name}"));
            if matches!(
                provider_hint,
                Some(ProviderKind::Ollama | ProviderKind::OllamaCloud)
            ) {
                return Ok(ModelResolution {
                    requested: Some(name.to_string()),
                    resolved: ModelInfo {
                        id: name.trim().to_string(),
                        provider: provider_hint.expect("matched provider hint"),
                        aliases: Vec::new(),
                        supports_tools: true,
                        supports_reasoning: false,
                    },
                    used_fallback: false,
                    fallback_chain,
                });
            }
            // Resolve within Go's roster without falling through to a same-named
            // model on another provider.
            if provider_hint == Some(ProviderKind::OpencodeGo)
                && let Some(canonical) = opencode_go_model_id(name)
                && let Some(model) = self
                    .models
                    .iter()
                    .find(|model| {
                        model.provider == ProviderKind::OpencodeGo
                            && model.id.eq_ignore_ascii_case(canonical)
                    })
                    .cloned()
            {
                return Ok(ModelResolution {
                    requested: Some(name.to_string()),
                    resolved: model,
                    used_fallback: false,
                    fallback_chain,
                });
            }
            if provider_hint != Some(ProviderKind::OpencodeGo)
                && let Some(provider) = provider_hint
                && let Some(model) = self
                    .models
                    .iter()
                    .find(|m| m.provider == provider && model_matches(m, name))
                    .cloned()
            {
                return Ok(ModelResolution {
                    requested: Some(name.to_string()),
                    resolved: model,
                    used_fallback: false,
                    fallback_chain,
                });
            }
            if provider_hint == Some(ProviderKind::Atlascloud)
                && let Some(model) = atlascloud_passthrough_model(name)
            {
                return Ok(ModelResolution {
                    requested: Some(name.to_string()),
                    resolved: model,
                    used_fallback: false,
                    fallback_chain,
                });
            }
            if provider_hint == Some(ProviderKind::Arcee)
                && let Some(model) = arcee_passthrough_model(name)
            {
                return Ok(ModelResolution {
                    requested: Some(name.to_string()),
                    resolved: model,
                    used_fallback: false,
                    fallback_chain,
                });
            }
            if provider_hint == Some(ProviderKind::XiaomiMimo)
                && let Some(model) = xiaomi_mimo_passthrough_model(name)
            {
                return Ok(ModelResolution {
                    requested: Some(name.to_string()),
                    resolved: model,
                    used_fallback: false,
                    fallback_chain,
                });
            }
            // A provider's own declared default is available from that
            // provider by definition — the descriptor owns that fact (#6443:
            // `deepseek-flash` is the Deepseek default and resolved nowhere).
            // Registry rows canonicalize aliases and carry capability
            // metadata; they must not gate the name the provider declares.
            let declared_default = provider.provider().default_model();
            if !declared_default.trim().is_empty()
                && name.trim().eq_ignore_ascii_case(declared_default.trim())
            {
                return Ok(ModelResolution {
                    requested: Some(name.to_string()),
                    resolved: Self::descriptor_default_model(provider, declared_default),
                    used_fallback: false,
                    fallback_chain,
                });
            }
            if !self.models.iter().any(|model| model.provider == provider) {
                return Err(ModelResolutionError::ProviderHasNoModels {
                    provider,
                    requested: Some(name.to_string()),
                });
            }
            return Err(ModelResolutionError::ModelNotAvailableForProvider {
                provider,
                requested: name.to_string(),
            });
        }

        fallback_chain.push(format!("provider_default:{}", provider.as_str()));
        let default_model = provider.provider().default_model();
        if let Some(model) = self
            .models
            .iter()
            .find(|model| model.provider == provider && model_matches(model, default_model))
            .cloned()
        {
            return Ok(ModelResolution {
                requested: None,
                resolved: model,
                used_fallback: true,
                fallback_chain,
            });
        }
        // Same rule as the explicit branch: the descriptor's declared default
        // resolves for its own provider even without a registry row (#6443).
        // Ollama is the exception: its descriptor default is the placeholder
        // `unknown`, and the real default comes from the live local catalog
        // (Y-2), so a placeholder must never resolve as a model.
        if !default_model.trim().is_empty() && provider != ProviderKind::Ollama {
            return Ok(ModelResolution {
                requested: None,
                resolved: Self::descriptor_default_model(provider, default_model),
                used_fallback: true,
                fallback_chain,
            });
        }
        if !self.models.iter().any(|model| model.provider == provider) {
            return Err(ModelResolutionError::ProviderHasNoModels {
                provider,
                requested: None,
            });
        }

        Err(ModelResolutionError::ProviderDefaultUnavailable {
            provider,
            default_model: default_model.to_string(),
        })
    }

    /// The [`ModelInfo`] a provider's declared default resolves to when the
    /// registry carries no explicit row for it. The descriptor owns the
    /// identity; capability metadata stays conservative rather than
    /// fabricating a capability the registry never recorded.
    fn descriptor_default_model(provider: ProviderKind, id: &str) -> ModelInfo {
        ModelInfo {
            id: id.trim().to_string(),
            provider,
            aliases: Vec::new(),
            supports_tools: true,
            supports_reasoning: false,
        }
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[must_use]
/// Classify a model identifier by its underlying model family.
pub fn model_family(model_id: &str) -> ModelFamily {
    let normalized = normalize(model_id);
    if normalized.is_empty() {
        return ModelFamily::Inferencer;
    }

    if normalized.contains("deepseek") {
        return ModelFamily::DeepSeek;
    }
    if normalized.contains("claude") || normalized.contains("anthropic") {
        return ModelFamily::Anthropic;
    }
    if normalized.contains("gpt-oss") || normalized.contains("gpt_oss") {
        return ModelFamily::GptOss;
    }
    if normalized.starts_with("gpt-")
        || normalized.contains("/gpt-")
        || normalized.contains("openai/")
    {
        return ModelFamily::OpenAI;
    }
    if normalized.contains("gemini")
        || normalized.contains("gemma")
        || normalized.contains("google/")
    {
        return ModelFamily::Google;
    }
    if normalized.contains("llama")
        || normalized.contains("muse-spark")
        || normalized.contains("meta-")
        || normalized.contains("meta/")
    {
        return ModelFamily::Meta;
    }
    if normalized.contains("mistral")
        || normalized.contains("mixtral")
        || normalized.contains("codestral")
    {
        return ModelFamily::Mistral;
    }
    if normalized.contains("qwen") {
        return ModelFamily::Qwen;
    }
    if normalized.contains("grok") {
        return ModelFamily::Grok;
    }
    if normalized.contains("cohere") || normalized.contains("command-r") {
        return ModelFamily::Cohere;
    }

    ModelFamily::Inferencer
}

fn model_matches(model: &ModelInfo, requested: &str) -> bool {
    let requested = normalize(requested);
    normalize(&model.id) == requested
        || model
            .aliases
            .iter()
            .any(|alias| normalize(alias) == requested)
}

fn atlascloud_passthrough_model(requested: &str) -> Option<ModelInfo> {
    let requested = requested.trim();
    if requested.is_empty() || !requested.contains('/') {
        return None;
    }

    Some(ModelInfo {
        id: requested.to_string(),
        provider: ProviderKind::Atlascloud,
        aliases: Vec::new(),
        supports_tools: true,
        supports_reasoning: true,
    })
}

fn arcee_passthrough_model(requested: &str) -> Option<ModelInfo> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    let supports_reasoning = requested.to_ascii_lowercase().contains("thinking");

    Some(ModelInfo {
        id: requested.to_string(),
        provider: ProviderKind::Arcee,
        aliases: Vec::new(),
        supports_tools: true,
        supports_reasoning,
    })
}

fn xiaomi_mimo_passthrough_model(requested: &str) -> Option<ModelInfo> {
    let requested = requested.trim();
    if requested.is_empty() || requested.chars().any(char::is_control) {
        return None;
    }

    Some(ModelInfo {
        id: requested.to_string(),
        provider: ProviderKind::XiaomiMimo,
        aliases: Vec::new(),
        supports_tools: true,
        supports_reasoning: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    trait ModelRegistryTestExt {
        fn resolve_ok(
            &self,
            requested: Option<&str>,
            provider_hint: Option<ProviderKind>,
        ) -> ModelResolution;
    }

    impl ModelRegistryTestExt for ModelRegistry {
        fn resolve_ok(
            &self,
            requested: Option<&str>,
            provider_hint: Option<ProviderKind>,
        ) -> ModelResolution {
            self.resolve(requested, provider_hint)
                .expect("test route should resolve")
        }
    }

    #[test]
    fn model_registry_new_preserves_model_rows_and_aliases() {
        let models = vec![
            ModelInfo {
                id: "Model-A".to_string(),
                provider: ProviderKind::Deepseek,
                aliases: vec!["alias-1".to_string(), " ALIAS-2 ".to_string()],
                supports_tools: true,
                supports_reasoning: false,
            },
            ModelInfo {
                id: "model-b".to_string(),
                provider: ProviderKind::Deepseek,
                aliases: vec!["alias-1".to_string()],
                supports_tools: true,
                supports_reasoning: true,
            },
        ];

        let registry = ModelRegistry::new(models);

        let rows = registry.list();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "Model-A");
        assert_eq!(rows[0].aliases, ["alias-1", " ALIAS-2 "]);
        assert_eq!(rows[1].id, "model-b");
    }

    #[test]
    fn deepseek_v4_pro_alias_stays_deepseek_when_provider_selected() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("deepseek-v4-pro"), Some(ProviderKind::Deepseek));

        assert_eq!(resolved.resolved.provider, ProviderKind::Deepseek);
        assert_eq!(resolved.resolved.id, "deepseek-v4-pro");
    }

    #[test]
    fn providerless_unknown_model_requires_explicit_route_authority() {
        let registry = ModelRegistry::default();

        for requested in [None, Some("deepseek-v4-pro"), Some("not-in-the-catalog")] {
            let error = ModelRegistry::resolve(&registry, requested, None)
                .expect_err("provider-less fallback must fail closed");
            assert_eq!(
                error,
                ModelResolutionError::ProviderRequired {
                    requested: requested.map(str::to_string),
                }
            );
            if requested.is_none() || requested == Some("not-in-the-catalog") {
                assert!(!error.to_string().to_ascii_lowercase().contains("deepseek"));
            }
        }
    }

    #[test]
    fn providerless_unknown_selectors_never_mint_provider_authority() {
        let registry = ModelRegistry::default();

        for requested in [
            "deepseek-v4-not-a-real-model",
            "gpt-not-a-real-model",
            "provider/model-that-does-not-exist",
        ] {
            assert!(matches!(
                ModelRegistry::resolve(&registry, Some(requested), None),
                Err(ModelResolutionError::ProviderRequired {
                    requested: Some(returned),
                }) if returned == requested
            ));
        }
        for requested in ["", "   "] {
            assert!(matches!(
                ModelRegistry::resolve(&registry, Some(requested), None),
                Err(ModelResolutionError::ProviderRequired { requested: None })
            ));
        }
    }

    #[test]
    fn explicit_provider_with_no_registry_rows_never_borrows_global_default() {
        let registry = ModelRegistry::new(Vec::new());

        let error = ModelRegistry::resolve(
            &registry,
            Some("provider-owned-model"),
            Some(ProviderKind::Openrouter),
        )
        .expect_err("an empty provider catalog must not borrow another route");
        assert_eq!(
            error,
            ModelResolutionError::ProviderHasNoModels {
                provider: ProviderKind::Openrouter,
                requested: Some("provider-owned-model".to_string()),
            }
        );
        assert!(!error.to_string().to_ascii_lowercase().contains("deepseek"));
    }

    #[test]
    fn explicit_deepseek_selection_retains_its_provider_owned_default() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Deepseek));

        assert_eq!(resolved.resolved.provider, ProviderKind::Deepseek);
        // The descriptor's declared default, not the first cloud row: the
        // registry's Deepseek rows start at v4-pro, and borrowing that here is
        // exactly the mismatch #6443 fixed.
        assert_eq!(resolved.resolved.id, "deepseek-flash");
        assert!(resolved.used_fallback);
        assert_eq!(resolved.fallback_chain, ["provider_default:deepseek"]);
    }

    #[test]
    fn explicit_openai_selection_uses_its_documented_default_not_first_catalog_row() {
        let registry = ModelRegistry::default();

        for requested in [None, Some(""), Some("   ")] {
            let resolved = registry.resolve_ok(requested, Some(ProviderKind::Openai));
            assert_eq!(resolved.requested, None);
            assert_eq!(resolved.resolved.provider, ProviderKind::Openai);
            assert_eq!(resolved.resolved.id, "gpt-5.6");
            assert!(resolved.used_fallback);
            assert_eq!(resolved.fallback_chain, ["provider_default:openai"]);
        }
    }

    #[test]
    fn provider_default_without_a_registry_row_resolves_to_its_own_id() {
        // SHA-6443: the descriptor owns its declared default. A registry that
        // carries unrelated rows must still resolve the provider's own
        // default — and must never borrow another provider's model.
        let registry = ModelRegistry::new(vec![ModelInfo {
            id: "not-the-openai-default".to_string(),
            provider: ProviderKind::Openai,
            aliases: Vec::new(),
            supports_tools: true,
            supports_reasoning: true,
        }]);

        let resolved = registry
            .resolve(None, Some(ProviderKind::Openai))
            .expect("the provider's declared default resolves for that provider");
        assert_eq!(resolved.resolved.id, "gpt-5.6");
        assert_eq!(resolved.resolved.provider, ProviderKind::Openai);
        assert!(resolved.used_fallback);
    }

    #[test]
    fn deepseek_vision_model_lists_and_resolves_with_aliases() {
        let registry = ModelRegistry::default();
        let listed = registry.list();

        assert!(listed.iter().any(|model| {
            model.provider == ProviderKind::Deepseek
                && model.id == "deepseek-v4-flash-vision-exp"
                && model.aliases.is_empty()
        }));

        for (selector, expected) in [
            (
                "deepseek-v4-flash-vision-exp",
                "deepseek-v4-flash-vision-exp",
            ),
            ("flash-vision", "deepseek-flash"),
            ("deepseek-v4flashvisionexp", "deepseek-flash"),
        ] {
            let resolved = registry.resolve_ok(Some(selector), Some(ProviderKind::Deepseek));
            assert_eq!(
                resolved.resolved.id, expected,
                "{selector} must resolve to {expected}"
            );
            assert_eq!(resolved.resolved.provider, ProviderKind::Deepseek);
            assert!(!resolved.used_fallback, "{selector} must not fall back");
        }
    }

    #[test]
    fn deepseek_v4_pro_alias_resolves_to_nvidia_nim_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("deepseek-v4-pro"), Some(ProviderKind::NvidiaNim));

        assert_eq!(resolved.resolved.provider, ProviderKind::NvidiaNim);
        assert_eq!(resolved.resolved.id, "deepseek-ai/deepseek-v4-pro");
    }

    #[test]
    fn nvidia_nim_default_uses_catalog_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::NvidiaNim));

        assert_eq!(resolved.resolved.provider, ProviderKind::NvidiaNim);
        assert_eq!(resolved.resolved.id, "deepseek-ai/deepseek-v4-pro");
    }

    #[test]
    fn deepseek_v4_flash_alias_resolves_to_nvidia_nim_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved =
            registry.resolve_ok(Some("deepseek-v4-flash"), Some(ProviderKind::NvidiaNim));

        assert_eq!(resolved.resolved.provider, ProviderKind::NvidiaNim);
        assert_eq!(resolved.resolved.id, "deepseek-ai/deepseek-v4-flash");
    }

    #[test]
    fn atlascloud_default_uses_namespaced_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Atlascloud));

        assert_eq!(resolved.resolved.provider, ProviderKind::Atlascloud);
        assert_eq!(resolved.resolved.id, "deepseek-ai/deepseek-v4-flash");
        assert!(resolved.resolved.supports_reasoning);
    }

    #[test]
    fn deepseek_v4_flash_alias_resolves_to_atlascloud_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved =
            registry.resolve_ok(Some("deepseek-v4-flash"), Some(ProviderKind::Atlascloud));

        assert_eq!(resolved.resolved.provider, ProviderKind::Atlascloud);
        assert_eq!(resolved.resolved.id, "deepseek-ai/deepseek-v4-flash");
    }

    #[test]
    fn deepseek_v4_pro_alias_resolves_to_atlascloud_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("deepseek-v4-pro"), Some(ProviderKind::Atlascloud));

        assert_eq!(resolved.resolved.provider, ProviderKind::Atlascloud);
        assert_eq!(resolved.resolved.id, "deepseek-ai/deepseek-v4-pro");
    }

    #[test]
    fn atlascloud_provider_hint_passes_through_explicit_model_id() {
        let registry = ModelRegistry::default();
        let resolved =
            registry.resolve_ok(Some("openai/gpt-5.2-chat"), Some(ProviderKind::Atlascloud));

        assert_eq!(resolved.resolved.provider, ProviderKind::Atlascloud);
        assert_eq!(resolved.resolved.id, "openai/gpt-5.2-chat");
        assert!(resolved.resolved.supports_tools);
        assert!(resolved.resolved.supports_reasoning);
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn atlascloud_provider_hint_preserves_explicit_model_id_case() {
        let registry = ModelRegistry::default();
        let resolved =
            registry.resolve_ok(Some("Qwen/Qwen3-Coder"), Some(ProviderKind::Atlascloud));

        assert_eq!(resolved.resolved.provider, ProviderKind::Atlascloud);
        assert_eq!(resolved.resolved.id, "Qwen/Qwen3-Coder");
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn atlascloud_plain_unknown_model_rejects_instead_of_using_default() {
        let registry = ModelRegistry::default();
        let error = registry
            .resolve(Some("not-in-atlas"), Some(ProviderKind::Atlascloud))
            .expect_err("a requested unknown model must not become the provider default");

        assert_eq!(
            error,
            ModelResolutionError::ModelNotAvailableForProvider {
                provider: ProviderKind::Atlascloud,
                requested: "not-in-atlas".to_string(),
            }
        );
    }

    #[test]
    fn openrouter_default_uses_namespaced_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Openrouter));

        assert_eq!(resolved.resolved.provider, ProviderKind::Openrouter);
        assert_eq!(resolved.resolved.id, "deepseek/deepseek-v4-pro");
    }

    #[test]
    fn xiaomi_mimo_default_uses_canonical_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::XiaomiMimo));

        assert_eq!(resolved.resolved.provider, ProviderKind::XiaomiMimo);
        assert_eq!(resolved.resolved.id, "mimo-v2.6-pro");
        assert!(resolved.resolved.supports_reasoning);
    }

    #[test]
    fn moonshot_default_and_aliases_use_kimi_k27_code() {
        let registry = ModelRegistry::default();

        for requested in [None, Some("kimi"), Some("kimi-k2.7-code")] {
            let resolved = registry.resolve_ok(requested, Some(ProviderKind::Moonshot));

            assert_eq!(resolved.resolved.provider, ProviderKind::Moonshot);
            assert_eq!(resolved.resolved.id, "kimi-k2.7-code");
            assert!(resolved.resolved.supports_tools);
            assert!(resolved.resolved.supports_reasoning);
        }
    }

    #[test]
    fn moonshot_explicit_kimi_k26_remains_available() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("kimi-k2.6"), Some(ProviderKind::Moonshot));

        assert_eq!(resolved.resolved.provider, ProviderKind::Moonshot);
        assert_eq!(resolved.resolved.id, "kimi-k2.6");
        assert!(resolved.resolved.supports_reasoning);
    }

    /// v0.9.1 dogfood report: a user ran `--provider moonshot --model kimi-k3` and was told
    /// the model was `kimi-k2.7-code`. The registry knew neither Moonshot K3
    /// product, so the explicit request fell through to the provider default.
    #[test]
    fn moonshot_resolves_both_k3_products_without_crossing_them() {
        let registry = ModelRegistry::default();

        for (requested, expected) in [("kimi-k3", "kimi-k3"), ("k3", "k3")] {
            let resolved = registry.resolve_ok(Some(requested), Some(ProviderKind::Moonshot));

            assert_eq!(resolved.resolved.provider, ProviderKind::Moonshot);
            assert_eq!(resolved.resolved.id, expected, "{resolved:?}");
            assert!(
                !resolved.used_fallback,
                "an explicit Moonshot K3 request is not a fallback: {resolved:?}"
            );
        }
    }

    /// The bare `k3` id belongs to the Kimi Code coding-plan endpoint and
    /// `kimi-k3` to the direct platform endpoint. Neither may be laundered
    /// into the other's id by alias expansion.
    #[test]
    fn moonshot_k3_ids_are_never_rewritten_into_each_other() {
        let registry = ModelRegistry::default();

        assert_eq!(
            registry
                .resolve_ok(Some("kimi-k3"), Some(ProviderKind::Moonshot))
                .resolved
                .id,
            "kimi-k3"
        );
        assert_eq!(
            registry
                .resolve_ok(Some("k3"), Some(ProviderKind::Moonshot))
                .resolved
                .id,
            "k3"
        );
    }

    /// A provider-scoped question must never be answered with another
    /// vendor's model. `kimi-k3` also exists in the OpenCode Go catalog;
    /// before this fix that entry answered `--provider moonshot` requests.
    #[test]
    fn a_provider_hint_never_resolves_to_another_providers_model() {
        let registry = ModelRegistry::default();

        let error = registry
            .resolve(Some("glm-5.2"), Some(ProviderKind::Moonshot))
            .expect_err("a Moonshot request must not be answered by Z.ai or a default");
        assert_eq!(
            error,
            ModelResolutionError::ModelNotAvailableForProvider {
                provider: ProviderKind::Moonshot,
                requested: "glm-5.2".to_string(),
            }
        );

        let go = registry.resolve_ok(Some("kimi-k3"), Some(ProviderKind::OpencodeGo));
        assert_eq!(go.resolved.provider, ProviderKind::OpencodeGo);
        assert_eq!(go.resolved.id, "kimi-k3");
    }

    #[test]
    fn xiaomi_mimo_tts_aliases_resolve_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("tts"), Some(ProviderKind::XiaomiMimo));
        assert_eq!(resolved.resolved.provider, ProviderKind::XiaomiMimo);
        assert_eq!(resolved.resolved.id, "mimo-v2.5-tts");
        assert!(!resolved.resolved.supports_tools);
        assert!(!resolved.resolved.supports_reasoning);

        let resolved = registry.resolve_ok(Some("voice-design"), Some(ProviderKind::XiaomiMimo));
        assert_eq!(resolved.resolved.id, "mimo-v2.5-tts-voicedesign");

        let resolved = registry.resolve_ok(Some("voiceclone"), Some(ProviderKind::XiaomiMimo));
        assert_eq!(resolved.resolved.id, "mimo-v2.5-tts-voiceclone");
    }

    #[test]
    fn xiaomi_mimo_chat_aliases_resolve_when_provider_hinted() {
        let registry = ModelRegistry::default();

        let resolved = registry.resolve_ok(Some("omni"), Some(ProviderKind::XiaomiMimo));
        assert_eq!(resolved.resolved.provider, ProviderKind::XiaomiMimo);
        assert_eq!(resolved.resolved.id, "mimo-v2.6-flash");
        assert!(resolved.resolved.supports_tools);
    }

    #[test]
    fn xiaomi_mimo_provider_hint_preserves_custom_model_id() {
        let registry = ModelRegistry::default();
        let resolved =
            registry.resolve_ok(Some("account-custom-mimo"), Some(ProviderKind::XiaomiMimo));

        assert_eq!(resolved.resolved.provider, ProviderKind::XiaomiMimo);
        assert_eq!(resolved.resolved.id, "account-custom-mimo");
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn xiaomi_mimo_provider_hint_does_not_reclassify_openrouter_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(
            Some("deepseek/deepseek-v4-pro"),
            Some(ProviderKind::XiaomiMimo),
        );

        assert_eq!(resolved.resolved.provider, ProviderKind::XiaomiMimo);
        assert_eq!(resolved.resolved.id, "deepseek/deepseek-v4-pro");
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn wanjie_ark_default_uses_reasoner_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::WanjieArk));

        assert_eq!(resolved.resolved.provider, ProviderKind::WanjieArk);
        assert_eq!(resolved.resolved.id, "deepseek-reasoner");
        assert!(resolved.resolved.supports_reasoning);
    }

    #[test]
    fn novita_default_uses_namespaced_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Novita));

        assert_eq!(resolved.resolved.provider, ProviderKind::Novita);
        assert_eq!(resolved.resolved.id, "deepseek/deepseek-v4-pro");
    }

    #[test]
    fn fireworks_default_uses_canonical_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Fireworks));

        assert_eq!(resolved.resolved.provider, ProviderKind::Fireworks);
        assert_eq!(
            resolved.resolved.id,
            "accounts/fireworks/models/deepseek-v4-pro"
        );
    }

    #[test]
    fn siliconflow_default_uses_canonical_pro_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Siliconflow));

        assert_eq!(resolved.resolved.provider, ProviderKind::Siliconflow);
        assert_eq!(resolved.resolved.id, "deepseek-ai/DeepSeek-V4-Pro");
        assert!(resolved.resolved.supports_reasoning);
    }

    #[test]
    fn arcee_default_uses_direct_trinity_large_thinking_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Arcee));

        assert_eq!(resolved.resolved.provider, ProviderKind::Arcee);
        assert_eq!(resolved.resolved.id, "trinity-large-thinking");
        assert!(resolved.resolved.supports_reasoning);
    }

    #[test]
    fn arcee_trinity_alias_resolves_to_direct_large_thinking_not_openrouter() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("trinity"), Some(ProviderKind::Arcee));

        assert_eq!(resolved.resolved.provider, ProviderKind::Arcee);
        assert_eq!(resolved.resolved.id, "trinity-large-thinking");
        assert!(resolved.resolved.supports_reasoning);
    }

    #[test]
    fn arcee_trinity_mini_remains_explicit_compatibility_model() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("trinity-mini"), Some(ProviderKind::Arcee));

        assert_eq!(resolved.resolved.provider, ProviderKind::Arcee);
        assert_eq!(resolved.resolved.id, "trinity-mini");
        assert!(resolved.resolved.supports_reasoning);
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn arcee_provider_hint_preserves_explicit_future_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("trinity-large-next"), Some(ProviderKind::Arcee));

        assert_eq!(resolved.resolved.provider, ProviderKind::Arcee);
        assert_eq!(resolved.resolved.id, "trinity-large-next");
        assert!(!resolved.resolved.supports_reasoning);
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn deepseek_reasoner_does_not_silently_substitute_siliconflow_pro() {
        let registry = ModelRegistry::default();
        let error = registry
            .resolve(Some("deepseek-reasoner"), Some(ProviderKind::Siliconflow))
            .expect_err("an absent alias must not become SiliconFlow's first/default row");

        assert_eq!(
            error,
            ModelResolutionError::ModelNotAvailableForProvider {
                provider: ProviderKind::Siliconflow,
                requested: "deepseek-reasoner".to_string(),
            }
        );
    }

    #[test]
    fn deepseek_v4_flash_alias_resolves_to_siliconflow_flash_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved =
            registry.resolve_ok(Some("deepseek-v4-flash"), Some(ProviderKind::Siliconflow));

        assert_eq!(resolved.resolved.provider, ProviderKind::Siliconflow);
        assert_eq!(resolved.resolved.id, "deepseek-ai/DeepSeek-V4-Flash");
    }

    #[test]
    fn sglang_default_uses_canonical_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Sglang));

        assert_eq!(resolved.resolved.provider, ProviderKind::Sglang);
        assert_eq!(resolved.resolved.id, "deepseek-ai/DeepSeek-V4-Pro");
    }

    #[test]
    fn zai_direct_models_resolve_when_provider_hinted() {
        let registry = ModelRegistry::default();

        // Keep the agent registry fallback aligned with codewhale-config's
        // DEFAULT_ZAI_MODEL.
        let default = registry.resolve_ok(None, Some(ProviderKind::Zai));
        assert_eq!(default.resolved.provider, ProviderKind::Zai);
        assert_eq!(default.resolved.id, "GLM-5.3");
        assert!(default.used_fallback);
        assert_eq!(default.fallback_chain, ["provider_default:zai"]);

        for (alias, expected) in [
            ("GLM-5.1", "GLM-5.1"),
            ("glm-5-1", "GLM-5.1"),
            ("GLM-5.2", "GLM-5.2"),
            ("glm-5.2", "GLM-5.2"),
            ("zai-glm-5-2", "GLM-5.2"),
            ("GLM-5.3", "GLM-5.3"),
            ("glm-5.3", "GLM-5.3"),
            ("glm-5-3", "GLM-5.3"),
            ("zai-glm-5-3", "GLM-5.3"),
            ("GLM-5.3-Flash", "GLM-5.3-Flash"),
            ("glm-5.3-flash", "GLM-5.3-Flash"),
            ("glm-5-3-flash", "GLM-5.3-Flash"),
            ("zai-glm-5.3-flash", "GLM-5.3-Flash"),
            ("GLM-5-Turbo", "GLM-5-Turbo"),
            ("glm-5-turbo", "GLM-5-Turbo"),
            ("zai-glm-5-turbo", "GLM-5-Turbo"),
        ] {
            let resolved = registry.resolve_ok(Some(alias), Some(ProviderKind::Zai));

            assert_eq!(resolved.resolved.provider, ProviderKind::Zai);
            assert_eq!(resolved.resolved.id, expected);
            assert!(!resolved.used_fallback);
            assert!(resolved.resolved.supports_tools);
            assert!(resolved.resolved.supports_reasoning);
        }
    }

    #[test]
    fn first_party_recent_provider_models_are_listed() {
        let registry = ModelRegistry::default();
        let models = registry.list();

        for (provider, id) in [
            (ProviderKind::Zai, "GLM-5.2"),
            (ProviderKind::Stepfun, "step-5-preview"),
            (ProviderKind::Minimax, "MiniMax-M2.1"),
            (ProviderKind::MinimaxAnthropic, "MiniMax-M3"),
            (ProviderKind::Openmodel, "deepseek-v4-flash"),
            (ProviderKind::Meta, "muse-spark-1.2"),
            (ProviderKind::Xai, "grok-4.6"),
        ] {
            assert!(
                models
                    .iter()
                    .any(|model| model.provider == provider && model.id == id),
                "expected {provider:?} model {id} in registry"
            );
        }
    }

    #[test]
    fn opencode_go_lists_documented_models_without_inventing_capabilities() {
        let registry = ModelRegistry::default();
        let listed = registry.list();
        let models: Vec<&str> = listed
            .iter()
            .filter(|model| model.provider == ProviderKind::OpencodeGo)
            .map(|model| model.id.as_str())
            .collect();

        // Literal expectations independently catch an incomplete shared roster
        // and prevent new compatibility entries from claiming capabilities.
        let expected = [
            ("deepseek-v4-pro", true),
            ("grok-4.5", true),
            ("glm-5.2", true),
            ("glm-5.1", true),
            ("kimi-k3", true),
            ("kimi-k2.7-code", true),
            ("kimi-k2.6", true),
            ("deepseek-v4-flash", true),
            ("mimo-v2.5", true),
            ("mimo-v2.5-pro", true),
            ("glm-5.3-flash", false),
            ("glm-5.3", false),
            ("longcat-2.0", false),
            ("deepseek-v4-flash-vision-exp", false),
            ("hy4-preview", false),
            ("hy3", false),
            ("omen-alpha", false),
            ("deepseek-v4.1-flash", false),
            ("grok-4.6", false),
            ("gpt-5.6-luna", false),
            ("muse-spark-1.3-contributor", false),
            ("muse-spark-1.2-contributor", false),
            ("minimax-m3", false),
            ("minimax-m2.7", false),
            ("minimax-m2.5", false),
            ("qwen3.8-max", false),
            ("qwen3.8-flash", false),
            ("qwen3.7-max", false),
            ("qwen3.7-plus", false),
            ("qwen3.6-plus", false),
        ];
        assert_eq!(
            models,
            expected.iter().map(|(id, _)| *id).collect::<Vec<_>>()
        );

        let default = registry.resolve_ok(None, Some(ProviderKind::OpencodeGo));
        assert_eq!(default.resolved.provider, ProviderKind::OpencodeGo);
        assert_eq!(default.resolved.id, "deepseek-v4-pro");

        for (model, expected_capabilities) in expected {
            for requested in [model.to_string(), format!("opencode-go/{model}")] {
                let resolved =
                    registry.resolve_ok(Some(&requested), Some(ProviderKind::OpencodeGo));
                assert_eq!(resolved.resolved.provider, ProviderKind::OpencodeGo);
                assert_eq!(resolved.resolved.id, model);
                assert!(!resolved.used_fallback);
                assert_eq!(
                    resolved.resolved.aliases,
                    vec![format!("opencode-go/{model}")],
                    "{requested}"
                );
                assert_eq!(
                    resolved.resolved.supports_tools, expected_capabilities,
                    "{requested} tool support"
                );
                assert_eq!(
                    resolved.resolved.supports_reasoning, expected_capabilities,
                    "{requested} reasoning support"
                );
            }
        }

        for non_chat in ["claude-unproven", "unknown-model", "gpt-unlisted"] {
            for requested in [non_chat.to_string(), format!("opencode-go/{non_chat}")] {
                let rejected = registry
                    .resolve(Some(&requested), Some(ProviderKind::OpencodeGo))
                    .expect_err("unknown Go id must not fall back to another provider");
                assert_eq!(
                    rejected,
                    ModelResolutionError::ModelNotAvailableForProvider {
                        provider: ProviderKind::OpencodeGo,
                        requested,
                    }
                );
            }
        }
    }

    #[test]
    fn xai_grok_models_resolve_when_provider_hinted() {
        let registry = ModelRegistry::default();

        let default = registry.resolve_ok(None, Some(ProviderKind::Xai));
        assert_eq!(default.resolved.provider, ProviderKind::Xai);
        assert_eq!(default.resolved.id, "grok-4.6");
        assert!(default.used_fallback);

        let alias = registry.resolve_ok(Some("grok"), Some(ProviderKind::Xai));
        assert_eq!(alias.resolved.provider, ProviderKind::Xai);
        assert_eq!(alias.resolved.id, "grok-4.6");
        assert!(!alias.used_fallback);

        let fast = registry.resolve_ok(
            Some("grok-4.20-0309-non-reasoning"),
            Some(ProviderKind::Xai),
        );
        assert_eq!(fast.resolved.provider, ProviderKind::Xai);
        assert_eq!(fast.resolved.id, "grok-4.20-0309-non-reasoning");
        assert!(!fast.resolved.supports_reasoning);
    }

    #[test]
    fn meta_muse_spark_resolves_when_provider_hinted() {
        let registry = ModelRegistry::default();

        let default = registry.resolve_ok(None, Some(ProviderKind::Meta));
        assert_eq!(default.resolved.provider, ProviderKind::Meta);
        assert_eq!(default.resolved.id, "muse-spark-1.2");
        assert!(default.used_fallback);

        let alias = registry.resolve_ok(Some("muse-spark"), Some(ProviderKind::Meta));
        assert_eq!(alias.resolved.provider, ProviderKind::Meta);
        assert_eq!(alias.resolved.id, "muse-spark-1.2");
        assert!(!alias.used_fallback);
        assert_eq!(model_family("muse-spark-1.2"), ModelFamily::Meta);
    }

    #[test]
    fn openai_gpt56_family_resolves_when_provider_hinted() {
        let registry = ModelRegistry::default();
        for model in ["gpt-5.6", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            let resolved = registry.resolve_ok(Some(model), Some(ProviderKind::Openai));
            assert_eq!(resolved.resolved.provider, ProviderKind::Openai, "{model}");
            assert_eq!(resolved.resolved.id, model, "{model}");
            assert!(resolved.resolved.supports_tools, "{model}");
            assert!(resolved.resolved.supports_reasoning, "{model}");
            assert!(!resolved.used_fallback, "{model}");
        }
    }

    #[test]
    fn grok_ids_stay_in_grok_family() {
        assert_eq!(model_family("grok-4.6"), ModelFamily::Grok);
        assert_eq!(model_family("grok-4.5"), ModelFamily::Grok);
        assert_eq!(
            model_family("grok-4.20-0309-non-reasoning"),
            ModelFamily::Grok
        );
    }

    #[test]
    fn stepfun_and_minimax_direct_models_resolve_when_provider_hinted() {
        let registry = ModelRegistry::default();

        let stepfun = registry.resolve_ok(None, Some(ProviderKind::Stepfun));
        assert_eq!(stepfun.resolved.provider, ProviderKind::Stepfun);
        assert_eq!(stepfun.resolved.id, "step-5-preview");

        for (alias, expected) in [
            ("minimax", "MiniMax-M3"),
            ("minimax-m3", "MiniMax-M3"),
            ("minimax-m2.7", "MiniMax-M2.7"),
            ("minimax-m2-7-highspeed", "MiniMax-M2.7-highspeed"),
            ("minimax-m2.1", "MiniMax-M2.1"),
            ("minimax-m2", "MiniMax-M2"),
        ] {
            let resolved = registry.resolve_ok(Some(alias), Some(ProviderKind::Minimax));

            assert_eq!(resolved.resolved.provider, ProviderKind::Minimax);
            assert_eq!(resolved.resolved.id, expected);
            assert!(!resolved.used_fallback);
            assert!(resolved.resolved.supports_tools);
            assert!(resolved.resolved.supports_reasoning);
        }
    }

    #[test]
    fn minimax_anthropic_models_resolve_when_provider_hinted() {
        let registry = ModelRegistry::default();

        for (alias, expected) in [
            ("minimax-anthropic", "MiniMax-M3"),
            ("minimax-m3", "MiniMax-M3"),
            ("minimax-m2.7", "MiniMax-M2.7"),
        ] {
            let resolved = registry.resolve_ok(Some(alias), Some(ProviderKind::MinimaxAnthropic));

            assert_eq!(resolved.resolved.provider, ProviderKind::MinimaxAnthropic);
            assert_eq!(resolved.resolved.id, expected);
            assert!(!resolved.used_fallback);
            assert!(resolved.resolved.supports_tools);
            assert!(resolved.resolved.supports_reasoning);
        }
    }

    #[test]
    fn deepseek_v4_flash_alias_resolves_to_openrouter_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved =
            registry.resolve_ok(Some("deepseek-v4-flash"), Some(ProviderKind::Openrouter));

        assert_eq!(resolved.resolved.provider, ProviderKind::Openrouter);
        assert_eq!(resolved.resolved.id, "deepseek/deepseek-v4-flash");
    }

    #[test]
    fn recent_openrouter_large_model_aliases_resolve_when_provider_hinted() {
        let registry = ModelRegistry::default();

        for (alias, expected) in [
            ("trinity-large-thinking", "arcee-ai/trinity-large-thinking"),
            ("qwen3.6-flash", "qwen/qwen3.6-flash"),
            ("qwen3.6-35b-a3b", "qwen/qwen3.6-35b-a3b"),
            ("qwen3.6-max-preview", "qwen/qwen3.6-max-preview"),
            ("qwen3.6-plus", "qwen/qwen3.6-plus"),
            ("gemma-4-31b-it", "google/gemma-4-31b-it"),
            ("glm-5.1", "z-ai/glm-5.1"),
            ("glm-5.2", "z-ai/glm-5.2"),
            ("glm-5.3", "z-ai/glm-5.3"),
            ("glm-5.3-flash", "z-ai/glm-5.3-flash"),
            ("minimax-m3", "minimax/minimax-m3"),
            ("minimax-2.7", "minimax/minimax-m2.7"),
            ("openrouter-mimo-v2.5-pro", "xiaomi/mimo-v2.5-pro"),
            ("openrouter-kimi-k2.7-code", "moonshotai/kimi-k2.7-code"),
            ("openrouter-kimi-k2.6", "moonshotai/kimi-k2.6"),
            ("nemotron-3-ultra", "nvidia/nemotron-3-ultra-550b-a55b"),
            (
                "nvidia/nemotron-3-ultra",
                "nvidia/nemotron-3-ultra-550b-a55b",
            ),
        ] {
            let resolved = registry.resolve_ok(Some(alias), Some(ProviderKind::Openrouter));

            assert_eq!(resolved.resolved.provider, ProviderKind::Openrouter);
            assert_eq!(resolved.resolved.id, expected);
            assert!(resolved.resolved.supports_tools);
            assert!(resolved.resolved.supports_reasoning);
        }
    }

    #[test]
    fn deepseek_v4_flash_alias_resolves_to_novita_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("deepseek-v4-flash"), Some(ProviderKind::Novita));

        assert_eq!(resolved.resolved.provider, ProviderKind::Novita);
        assert_eq!(resolved.resolved.id, "deepseek/deepseek-v4-flash");
    }

    #[test]
    fn together_inkling_keeps_published_wire_identity() {
        let registry = ModelRegistry::default();
        for requested in ["thinkingmachines/inkling", "inkling", "together-inkling"] {
            let resolved = registry.resolve_ok(Some(requested), Some(ProviderKind::Together));

            assert_eq!(resolved.resolved.provider, ProviderKind::Together);
            assert_eq!(resolved.resolved.id, "thinkingmachines/inkling");
            assert!(resolved.resolved.supports_tools);
            assert!(resolved.resolved.supports_reasoning);
            assert!(!resolved.used_fallback);
        }

        assert!(matches!(
            registry.resolve(Some("inkling"), None),
            Err(ModelResolutionError::ProviderRequired { .. })
        ));
    }

    #[test]
    fn registry_lists_and_resolves_every_v090_catalog_addition() {
        let registry = ModelRegistry::default();
        let advertised = [
            (ProviderKind::Anthropic, "claude-sonnet-5"),
            (ProviderKind::Anthropic, "claude-fable-5"),
            (ProviderKind::Openai, "gpt-5.3-codex"),
            (ProviderKind::Openai, "gpt-5.5"),
            (ProviderKind::Openai, "gpt-5.5-pro"),
            (ProviderKind::Openrouter, "qwen/qwen3.7-plus"),
            (ProviderKind::Arcee, "trinity-mini"),
        ];

        let listed = registry.list();
        for (provider, model_id) in advertised {
            assert!(
                listed
                    .iter()
                    .any(|model| model.provider == provider && model.id == model_id),
                "missing {model_id} ({}) from model list",
                provider.as_str()
            );
            let resolved = registry.resolve_ok(Some(model_id), Some(provider));
            assert_eq!(resolved.resolved.provider, provider, "{model_id}");
            assert_eq!(resolved.resolved.id, model_id, "{model_id}");
            assert!(!resolved.used_fallback, "{model_id}");
        }
    }

    #[test]
    fn gpt_55_stays_provider_scoped_between_openai_and_codex() {
        let registry = ModelRegistry::default();

        assert!(matches!(
            registry.resolve(Some("gpt-5.5"), None),
            Err(ModelResolutionError::ProviderRequired { .. })
        ));

        let codex = registry.resolve_ok(Some("gpt-5.5"), Some(ProviderKind::OpenaiCodex));
        assert_eq!(codex.resolved.provider, ProviderKind::OpenaiCodex);
        assert_eq!(codex.resolved.id, "gpt-5.5");
        assert!(!codex.used_fallback);
    }

    #[test]
    fn deepseek_v4_flash_alias_resolves_to_sglang_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("deepseek-v4-flash"), Some(ProviderKind::Sglang));

        assert_eq!(resolved.resolved.provider, ProviderKind::Sglang);
        assert_eq!(resolved.resolved.id, "deepseek-ai/DeepSeek-V4-Flash");
    }

    #[test]
    fn vllm_default_uses_canonical_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::Vllm));

        assert_eq!(resolved.resolved.provider, ProviderKind::Vllm);
        assert_eq!(resolved.resolved.id, "deepseek-ai/DeepSeek-V4-Pro");
    }

    #[test]
    fn ollama_default_is_unavailable_until_the_local_catalog_answers() {
        // Y-2: `DEFAULT_OLLAMA_MODEL` is deliberately "unknown". The real
        // default comes from the live local catalog, so the header never names
        // a model the session cannot reach; without that catalog the registry
        // must say so instead of resolving a costume.
        let registry = ModelRegistry::default();
        let error = registry
            .resolve(None, Some(ProviderKind::Ollama))
            .expect_err("the placeholder default must not resolve");

        assert!(matches!(
            error,
            ModelResolutionError::ProviderDefaultUnavailable {
                provider: ProviderKind::Ollama,
                ref default_model,
            } if default_model == "unknown"
        ));
    }

    #[test]
    fn ollama_cloud_default_uses_the_hosted_catalog_model_id() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(None, Some(ProviderKind::OllamaCloud));

        assert_eq!(resolved.resolved.provider, ProviderKind::OllamaCloud);
        assert_eq!(resolved.resolved.id, "gpt-oss:120b");
        assert!(resolved.resolved.supports_reasoning);
    }

    #[test]
    fn ollama_requested_model_tag_is_preserved() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("qwen2.5-coder:7b"), Some(ProviderKind::Ollama));

        assert_eq!(resolved.resolved.provider, ProviderKind::Ollama);
        assert_eq!(resolved.resolved.id, "qwen2.5-coder:7b");
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn deepseek_v4_flash_alias_resolves_to_vllm_when_provider_hinted() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("deepseek-v4-flash"), Some(ProviderKind::Vllm));

        assert_eq!(resolved.resolved.provider, ProviderKind::Vllm);
        assert_eq!(resolved.resolved.id, "deepseek-ai/DeepSeek-V4-Flash");
    }

    #[test]
    fn providerless_cased_model_text_does_not_authorize_deepseek() {
        let registry = ModelRegistry::default();
        assert!(matches!(
            registry.resolve(Some("DeepSeek-V4-Pro"), None),
            Err(ModelResolutionError::ProviderRequired { .. })
        ));
    }

    #[test]
    fn registry_casing_takes_priority_over_requested_casing_with_provider_hint() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("DeepSeek-V4-Pro"), Some(ProviderKind::Deepseek));

        assert_eq!(resolved.resolved.provider, ProviderKind::Deepseek);
        // Registry's canonical id is used even when user provides different casing
        assert_eq!(resolved.resolved.id, "deepseek-v4-pro");
    }

    #[test]
    fn providerless_whitespace_model_text_does_not_authorize_deepseek() {
        let registry = ModelRegistry::default();
        assert!(matches!(
            registry.resolve(Some("  DeepSeek-V4-Pro  "), None),
            Err(ModelResolutionError::ProviderRequired { .. })
        ));
    }

    #[test]
    fn alias_match_does_not_override_requested_casing() {
        let registry = ModelRegistry::default();
        let resolved = registry.resolve_ok(Some("deepseek-reasoner"), Some(ProviderKind::Deepseek));

        assert_eq!(resolved.resolved.provider, ProviderKind::Deepseek);
        assert_eq!(resolved.resolved.id, "deepseek-flash");
    }

    #[test]
    fn model_family_classifies_known_model_ids() {
        assert_eq!(model_family("deepseek-v4-pro"), ModelFamily::DeepSeek);
        assert_eq!(model_family("openai/gpt-5.4"), ModelFamily::OpenAI);
        assert_eq!(
            model_family("anthropic/claude-opus-4-7"),
            ModelFamily::Anthropic
        );
        assert_eq!(
            model_family("meta-llama/llama-3.3-70b-instruct"),
            ModelFamily::Meta
        );
        assert_eq!(model_family("Qwen/Qwen3-Coder"), ModelFamily::Qwen);
    }

    #[test]
    fn model_family_uses_underlying_model_for_router_ids() {
        assert_eq!(
            model_family("groq/llama-3.3-70b-versatile"),
            ModelFamily::Meta
        );
        assert_eq!(
            model_family("openrouter/openai/gpt-5.4"),
            ModelFamily::OpenAI
        );
        assert_eq!(
            model_family("fireworks/accounts/fireworks/models/deepseek-v4-pro"),
            ModelFamily::DeepSeek
        );
    }

    #[test]
    fn model_family_covers_prominent_google_and_mistral_model_names() {
        assert_eq!(model_family("google/gemma-3-27b-it"), ModelFamily::Google);
        assert_eq!(
            model_family("mistralai/mixtral-8x22b"),
            ModelFamily::Mistral
        );
        assert_eq!(model_family("codestral-latest"), ModelFamily::Mistral);
    }

    #[test]
    fn model_family_falls_back_to_inferencer_for_unknown_models() {
        assert_eq!(
            model_family("custom-gateway/my-private-model"),
            ModelFamily::Inferencer
        );
        assert_eq!(model_family(""), ModelFamily::Inferencer);
    }

    /// SHA-6443: a provider's declared default must be a model its own
    /// registry can resolve. A default the registry rejects fails a test
    /// here, not a founder's `model resolve`.
    #[test]
    fn every_provider_default_resolves_for_its_own_provider() {
        let registry = ModelRegistry::default();
        let mut failures = Vec::new();
        for kind in ProviderKind::all() {
            let default = kind.provider().default_model();
            if default.trim().is_empty() {
                continue;
            }
            if let Err(error) = registry.resolve(Some(default), Some(*kind)) {
                failures.push(format!("{} ({kind:?}): {error}", default));
            }
        }
        assert!(
            failures.is_empty(),
            "provider defaults must resolve for their own provider:\n{}",
            failures.join("\n")
        );
    }
}
