//! Configuration loading and defaults for codewhale.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context, Result};
use codewhale_execpolicy::ExecPolicyEngine;
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::audit::log_sensitive_event;
use crate::credentials::CredentialStore;
use crate::features::{Feature, Features, FeaturesToml, is_known_feature_key};
use crate::hooks::HooksConfig;

// Sub-agent concurrency/timeout limit constants and their clamp resolvers live
// in the `subagent_limits` leaf module. The constants are re-exported (keeping
// each item's visibility) so `crate::config::<CONST>` paths resolve unchanged;
// the private resolvers are pulled back in without widening external surface
// (#3311).
#[cfg(test)]
mod scope_tests;
// The single place provider credential precedence is decided. Lives inside
// `config` so it can walk the private probe helpers without widening their
// visibility; `has_api_key_for` is a thin wrapper over it (#pi-auth-port).
mod credential_resolve;
pub(crate) use credential_resolve::resolve_credential_source;
mod subagent_limits;
pub use subagent_limits::*;
use subagent_limits::{resolve_subagent_api_timeout_secs, resolve_subagent_heartbeat_timeout_secs};

// Provider model-name and base-URL constants live in the `models` leaf module
// and are re-exported below so every `crate::config::<CONST>` path is unchanged
// (#3311).
mod models;
pub use models::*;

#[cfg(test)]
pub(crate) use codewhale_config::API_KEYRING_SENTINEL;
pub(crate) use codewhale_config::{ConfigApiKeyValueKind, classify_config_api_key_value};

pub const DEFAULT_ZAI_PROVIDER_MAX_CONCURRENCY: usize = 3;
pub const MAX_PROVIDER_REQUEST_CONCURRENCY: usize = 64;

/// Default maximum number of automatic re-requests when a reasoning model
/// returns only hidden thinking without any answer text or tool call.
pub const DEFAULT_REASONING_ONLY_REPROMPTS: u32 = 2;

/// Nudge sent with a reasoning-only re-request once a bare retry has already
/// come back answerless. Overridable with `[reasoning_only] reprompt_message`.
///
/// It is never written to the session: it rides one outbound request and is
/// discarded, so the transcript never gains a message the user did not send.
pub const DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE: &str =
    "Continue: give your answer, or make the next tool call.";

pub fn default_stop_words() -> Vec<String> {
    ["stop", "wait", "pause"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub use codewhale_config::ProviderKind;
use codewhale_config::descriptors::{compatibility_for_id, compatibility_for_selector};
use codewhale_config::route::ProviderId;

/// Exact, non-secret provider identity resolved from live configuration.
///
/// Built-ins use their canonical slug (for example `openrouter`). Dynamic
/// custom providers keep the user-owned `[providers.<name>]` key so session
/// persistence never collapses `lm-studio` into the generic `custom` kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentity {
    pub(crate) provider: ProviderKind,
    pub(crate) key: ProviderId,
    /// Additive exact configured provider id written by current persistence
    /// schemas. `None` is meaningful: it identifies the released legacy
    /// root-level `provider = "custom"` route and must never be upgraded to an
    /// exact `[providers.custom]` table merely because one exists later.
    pub(crate) exact_id: Option<ProviderId>,
    /// Runtime provenance for the released `ollama` + exact Cloud route.
    /// Persistence writes the canonical Cloud kind plus the original `ollama`
    /// id, then reconstructs this flag on resume; the flag itself is not
    /// serialized.
    pub(crate) migrated_legacy_ollama_cloud_route: bool,
    /// Parse-scoped, redacted proof for the id-less migrated root route.
    pub(crate) legacy_root_custom_generation: Option<crate::route_receipt::CredentialGeneration>,
}

pub(crate) fn is_legacy_antigravity_identity(value: &str) -> bool {
    codewhale_config::ProviderKind::parse_config_identity(value)
        == Some(codewhale_config::ProviderKind::Antigravity)
}

impl ProviderIdentity {
    #[must_use]
    pub(crate) fn persisted_id(&self) -> Option<&str> {
        self.exact_id.as_ref().map(ProviderId::as_str)
    }

    /// Released kind spelling, retaining the China table's explicit provenance.
    pub(crate) fn persisted_kind(&self) -> &str {
        if self.provider == ProviderKind::Deepseek
            && self.key.as_str() == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id
        {
            self.key.as_str()
        } else {
            self.provider.as_str()
        }
    }

    /// Exact presentation/config leaf after admission. Auth storage policy is
    /// separate and remains intrinsic Rust authority.
    pub(crate) fn config_table_key(&self) -> Result<&str> {
        if self.provider == ProviderKind::Custom {
            return Ok(self.key.as_str());
        }
        if self.migrated_legacy_ollama_cloud_route {
            return Ok(ProviderKind::Ollama.as_str());
        }
        self.compatibility()
            .map(|row| row.config_key)
            .context("provider config metadata")
    }

    /// Pure descriptor view after admission; a custom key never borrows a brand.
    pub(crate) fn compatibility(
        &self,
    ) -> Option<&'static codewhale_config::descriptors::ProviderCompatibility> {
        if self.provider == ProviderKind::Custom {
            return None;
        }
        compatibility_for_id(self.key.as_str()).filter(|row| row.kind == self.provider)
    }
}

fn normalize_subagent_provider_key(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| match ch {
            '-' | '_' | '.' | ' ' => '_',
            other => other,
        })
        .collect()
}

// ============================================================================
// Provider Capability Matrix
// ============================================================================

/// Known capabilities for a provider + resolved-model combination.
///
/// Returned by [`provider_capability`] to describe what a given provider
/// supports for the resolved model string.  All fields are derived from
/// static knowledge (release docs, API guides) rather than live API probes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ProviderCapability {
    /// Canonical provider identifier.
    pub provider: ProviderKind,
    /// Resolved model identifier that will be sent in the API payload.
    pub resolved_model: String,
    /// Context window in tokens (the maximum input the model can accept).
    pub context_window: u32,
    /// Known output ceiling for this provider/model metadata path, when one is
    /// actually known.
    ///
    /// `None` means "this route publishes no output maximum we can stand
    /// behind" — for example the Kimi Code membership ids, whose limits live in
    /// the membership catalog rather than the static model catalogue. Unknown
    /// must stay unknown: callers may **not** substitute a placeholder ceiling,
    /// and in particular [`crate::route_budget`] does not clamp a requested
    /// `max_tokens` against an unknown compatibility cap.
    ///
    /// When `Some`, the value is a documented exact-route maximum or a
    /// deliberately conservative provider ceiling (Anthropic's 64K floor, the
    /// Codex OAuth route). It is metadata for diagnostics and CI policy; normal
    /// turns use a separate, more conservative request cap in the engine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u32>,
    /// Whether the provider+model supports thinking/reasoning mode.
    pub thinking_supported: bool,
    /// Whether the provider returns prompt-cache telemetry fields.
    pub cache_telemetry_supported: bool,
    /// Which request-payload dialect the provider uses.
    pub request_payload_mode: RequestPayloadMode,
    /// Deprecation metadata for compatibility aliases that are still accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_deprecation: Option<ModelAliasDeprecation>,
}

pub const DEEPSEEK_ALIAS_RETIREMENT_DATE: &str = "2026-07-24";
pub const DEEPSEEK_ALIAS_RETIREMENT_UTC: &str = "2026-07-24T15:59:00Z";
pub use codewhale_config::catalog::reviewed::constants::DEEPSEEK_ALIAS_REPLACEMENT;

/// Upstream retirement metadata for a model alias that remains compatible.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ModelAliasDeprecation {
    pub alias: String,
    pub replacement: String,
    pub retirement_date: String,
    pub retirement_utc: String,
    pub notice: String,
}

/// Which request-payload dialect the provider speaks.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum RequestPayloadMode {
    /// Standard OpenAI-compatible `/v1/chat/completions` payload.
    ChatCompletions,
    /// OpenAI Responses API payload.
    Responses,
    /// Native Anthropic Messages API `/v1/messages` payload (#3014).
    AnthropicMessages,
}

/// Resolve the provider capability for a given [`ProviderKind`] and resolved
/// model string.
///
/// The `resolved_model` should be the final model identifier that will appear
/// in the API payload (after normalization / provider-specific mapping).
#[must_use]
pub fn provider_capability(provider: ProviderKind, resolved_model: &str) -> ProviderCapability {
    provider_capability_with_wire(provider, resolved_model, None)
}

/// Wire-aware variant of [`provider_capability`] that respects
/// `wire = "responses" | "anthropic" | "chat"` for `Custom` providers.
///
/// Built-ins keep their fixed policy; `Custom` defaults to `Chat` when `wire`
/// is absent so existing configs stay compatible. Mirrors
/// `crates/tui/src/client.rs::provider_wire_format_for_config` and the
/// `Custom` comment in `crates/config/src/provider.rs`.
#[must_use]
pub fn provider_capability_with_wire(
    provider: ProviderKind,
    resolved_model: &str,
    wire: Option<&str>,
) -> ProviderCapability {
    // Custom wire overrides must be checked before the generic fallback so
    // `[providers.<name>] wire = "responses"` / `"anthropic"` is honored.
    if provider == ProviderKind::Custom {
        if wire_config_prefers_anthropic(wire) {
            return ProviderCapability {
                provider,
                resolved_model: resolved_model.to_string(),
                context_window: codewhale_models::context_window_for_model(resolved_model)
                    .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
                max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
                thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
                cache_telemetry_supported: false,
                request_payload_mode: RequestPayloadMode::AnthropicMessages,
                alias_deprecation: None,
            };
        }
        if wire_config_prefers_responses(wire) {
            return ProviderCapability {
                provider,
                resolved_model: resolved_model.to_string(),
                context_window: codewhale_models::context_window_for_model(resolved_model)
                    .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
                max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
                thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
                cache_telemetry_supported: false,
                request_payload_mode: RequestPayloadMode::Responses,
                alias_deprecation: None,
            };
        }
    }

    if matches!(
        provider,
        ProviderKind::Anthropic | ProviderKind::MinimaxAnthropic | ProviderKind::Openmodel
    ) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            // 200K is the conservative Anthropic floor; 4.6+ models resolve
            // their 1M windows from models.rs rows (#3014).
            context_window: codewhale_models::context_window_for_model(resolved_model)
                .unwrap_or(200_000),
            // 64K is the documented Anthropic Messages floor. For a model
            // the catalogue describes this carries its documented ceiling;
            // for an unknown one it is an *assumed* floor, and
            // `route_budget::output_ceiling_source` labels it unverified so
            // no receipt renders it as "documented" (#5440).
            max_output: Some(
                codewhale_models::max_output_tokens_for_model(resolved_model).unwrap_or(64_000),
            ),
            thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: matches!(provider, ProviderKind::Anthropic),
            request_payload_mode: RequestPayloadMode::AnthropicMessages,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ProviderKind::OpenaiCodex) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: OPENAI_CODEX_EFFECTIVE_CONTEXT_WINDOW_TOKENS,
            // The OAuth cache does not publish an output ceiling. This 4K is a
            // deliberate, long-standing product decision for the Codex route
            // (not a fallback): keep the compatibility capability conservative
            // instead of inheriting the public API model's output limit. It is
            // an assumption, not a documented fact — receipts label it
            // unverified (#5440).
            max_output: Some(4096),
            thinking_supported: true,
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::Responses,
            alias_deprecation: None,
        };
    }

    // #3023: Delete the Openai/Atlascloud/Moonshot early-return so these
    // providers use the generic model-based path below, which correctly
    // resolves context windows, output limits, and thinking support from
    // models.rs lookups.  Ollama also falls through to model-based lookups
    // with 8192 as the last-resort fallback instead of a hardcoded floor.
    if matches!(provider, ProviderKind::XiaomiMimo) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: codewhale_models::context_window_for_model(resolved_model)
                .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
            // No documented output maximum for these routes: stay unknown so
            // no compatibility clamp is applied downstream.
            max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
            thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ProviderKind::Arcee) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: codewhale_models::context_window_for_model(resolved_model)
                .unwrap_or(codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
            // No documented output maximum for these routes: stay unknown so
            // no compatibility clamp is applied downstream.
            max_output: codewhale_models::max_output_tokens_for_model(resolved_model),
            thinking_supported: codewhale_models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    let model_lower = resolved_model.to_ascii_lowercase();
    let alias_deprecation = if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        deepseek_alias_deprecation(&model_lower)
    } else {
        None
    };
    let exact_deepseek = canonical_official_deepseek_model_id(&model_lower);
    let is_v4_pro = exact_deepseek == Some("deepseek-v4-pro");
    let is_v4_flash =
        exact_deepseek == Some(DEEPSEEK_ALIAS_REPLACEMENT) || alias_deprecation.is_some();
    let is_reasoner = matches!(provider, ProviderKind::WanjieArk)
        && (model_lower.contains("reasoner") || model_lower.contains("r1"));

    // Provider-owned wire IDs can have exact catalog facts without a legacy
    // model-only row. Reuse those facts before conservative fallback budgets.
    let offering =
        crate::provider_lake::bundled_catalog_offering_for_model(provider, resolved_model);
    let context_window = if is_v4_pro || is_v4_flash {
        codewhale_models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
    } else if let Some(window) = codewhale_models::context_window_for_model(resolved_model) {
        window
    } else if let Some(window) = offering
        .as_ref()
        .and_then(|row| row.limit.as_ref())
        .and_then(|limit| limit.context)
        .and_then(|window| u32::try_from(window).ok())
    {
        window
    } else if matches!(provider, ProviderKind::Ollama) {
        8192
    } else {
        codewhale_models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS
    };

    // Output limits require an exact catalog row or an explicitly recognized
    // compatibility alias. A family-name match cannot document a new model.
    // The catalog answers
    // `None` when the catalogue has no row. That is the truthful state for
    // membership routes such as the `kimi-for-coding` family, whose ceilings
    // are owned by the membership catalog. It must not become a placeholder
    // number: a fabricated 4K here silently clamped offline membership routes
    // to 4K output via `route_budget`.
    let max_output = codewhale_models::max_output_tokens_for_model(resolved_model)
        .or_else(|| {
            // Provider-owned wire IDs need not exist in the legacy model-only
            // catalog (for example Fireworks' accounts/... slug). Reuse the
            // exact bundled offering instead of inferring a family ceiling.
            offering
                .as_ref()
                .and_then(|offering| offering.limit.as_ref())
                .and_then(|limit| limit.output)
                .and_then(|limit| u32::try_from(limit).ok())
        })
        .or_else(|| {
            canonical_official_deepseek_model_id(resolved_model)
                .or_else(|| {
                    alias_deprecation
                        .as_ref()
                        .map(|_| DEEPSEEK_ALIAS_REPLACEMENT)
                })
                .and_then(codewhale_models::max_output_tokens_for_model)
        });

    // Exact catalog reasoning facts and recognized compatibility aliases only.
    let thinking_supported = is_v4_pro
        || is_v4_flash
        || is_reasoner
        || offering
            .as_ref()
            .is_some_and(|row| row.reasoning == Some(true))
        || codewhale_models::model_supports_reasoning(resolved_model);

    // Cache telemetry: returned only by DeepSeek-native and NVIDIA NIM endpoints.
    let cache_telemetry_supported = matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::NvidiaNim | ProviderKind::Volcengine
    );

    let request_payload_mode = if matches!(
        provider,
        ProviderKind::DeepseekAnthropic | ProviderKind::MinimaxAnthropic | ProviderKind::Openmodel
    ) {
        RequestPayloadMode::AnthropicMessages
    } else {
        RequestPayloadMode::ChatCompletions
    };

    ProviderCapability {
        provider,
        resolved_model: resolved_model.to_string(),
        context_window,
        max_output,
        thinking_supported,
        cache_telemetry_supported,
        request_payload_mode,
        alias_deprecation,
    }
}

fn deepseek_alias_deprecation(model_lower: &str) -> Option<ModelAliasDeprecation> {
    match model_lower {
        "deepseek-chat" | "deepseek-reasoner" => Some(ModelAliasDeprecation {
            alias: model_lower.to_string(),
            replacement: DEEPSEEK_ALIAS_REPLACEMENT.to_string(),
            retirement_date: DEEPSEEK_ALIAS_RETIREMENT_DATE.to_string(),
            retirement_utc: DEEPSEEK_ALIAS_RETIREMENT_UTC.to_string(),
            notice: format!(
                "{model_lower} is a compatibility alias for {DEEPSEEK_ALIAS_REPLACEMENT} and is scheduled to retire on {DEEPSEEK_ALIAS_RETIREMENT_DATE}."
            ),
        }),
        _ => None,
    }
}

/// Canonicalize compact DeepSeek model aliases to stable IDs.
///
/// Already-valid model IDs pass through unchanged. Only the compact
/// `v4pro`/`v4flash` spellings and the experimental vision shorthand are
/// rewritten to their hyphenated forms.
#[must_use]
pub fn canonical_model_name(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_model_name",
        &model.trim().to_ascii_lowercase(),
    )
}

/// Normalize a configured/runtime model name.
///
/// Trims whitespace, preserves caller-provided case for already-valid model
/// IDs, and only canonicalizes compact aliases like `deepseek-v4pro`.
/// Non-DeepSeek or malformed names return `None`; DeepSeek's `/v1/models`
/// endpoint is the authority on valid model IDs.
#[must_use]
pub fn normalize_model_name(model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(canonical) = canonical_model_name(trimmed) {
        return Some(canonical.to_string());
    }

    let normalized = trimmed.to_ascii_lowercase();
    if !normalized.starts_with("deepseek") && !normalized.contains("/deepseek") {
        return None;
    }

    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
    {
        return Some(trimmed.to_string());
    }

    None
}

#[must_use]
pub(crate) fn normalize_custom_model_id(model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Validate a user-requested model id against the active provider (#3018).
///
/// DeepSeek providers use the strict `normalize_model_name` gate (the official
/// API only accepts DeepSeek IDs). OpenCode Go uses its documented model-scoped
/// protocol roster. Other providers pass any non-empty,
/// non-control-character string through — the provider API is the authority.
#[must_use]
pub fn requested_model_for_provider(provider: ProviderKind, model: &str) -> Option<String> {
    match provider {
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic => normalize_model_name(model),
        ProviderKind::OpencodeGo => opencode_go_model_id(model).map(str::to_string),
        _ => normalize_custom_model_id(model),
    }
}

/// Reject a provider/model tuple that we can be confident is invalid *before*
/// it reaches the network (#3227).
///
/// The route-isolation bug paired a model picked under one provider with a
/// different provider's route (model chip `deepseek-v4-pro`, provider badge
/// `Z.ai`), producing a `400 Unknown Model` from the upstream. This guard
/// catches that locally and names the incompatible pair instead.
///
/// We only reject tuples that are *known* to be wrong so legitimate custom
/// routing (self-hosted endpoints, OpenAI-compatible aggregators that proxy
/// DeepSeek weights, etc.) keeps working:
///
/// 1. A DeepSeek-native provider (`deepseek` / `deepseek-cn`) accepts only
///    DeepSeek model IDs or `auto` — same gate as [`normalize_model_name`].
/// 2. A non-DeepSeek *native* provider (e.g. Z.ai, which serves GLM) must not
///    be handed a DeepSeek-only model ID. This reuses the same
///    "foreign to a direct provider" classification the model resolver uses,
///    so DeepSeek aggregators (NVIDIA NIM, OpenRouter, Fireworks, …) stay
///    permissive.
/// 3. OpenCode Go accepts models with a documented Chat, Responses, or
///    Messages protocol; unknown wire contracts are rejected.
///
/// Returns `Ok(())` for any tuple we cannot confidently reject (the provider
/// API remains the final authority for those).
pub fn validate_route(provider: ProviderKind, model: &str) -> Result<(), String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "No model selected for provider '{}'.",
            provider.as_str()
        ));
    }
    if trimmed.eq_ignore_ascii_case("auto") {
        return Ok(());
    }

    if provider == ProviderKind::OpencodeGo {
        return if opencode_go_model_id(trimmed).is_some() {
            Ok(())
        } else {
            Err(format!(
                "Model '{trimmed}' is not in OpenCode Go's documented protocol roster. \
                 Choose one of: {}.",
                opencode_go_models().join(", ")
            ))
        };
    }

    // Providers whose model id is passed through verbatim (OpenAI-compatible,
    // Ollama tags, custom base URLs, …) are validated by the upstream service.
    if provider_passes_model_through(provider) {
        return Ok(());
    }

    if matches!(provider, ProviderKind::Deepseek) {
        if normalize_model_name(trimmed).is_some() {
            return Ok(());
        }
        return Err(format!(
            "Model '{trimmed}' is not a DeepSeek model, but the active provider is '{}'. \
             Use a DeepSeek model id (for example {}) or switch providers together with the model.",
            provider.as_str(),
            COMMON_DEEPSEEK_MODELS.join(", ")
        ));
    }

    // A non-DeepSeek native provider was handed a DeepSeek-only model id: this
    // is the exact contamination from #3227 (Z.ai + deepseek-v4-pro).
    if root_deepseek_model_is_foreign_to_direct_provider(provider, trimmed) {
        return Err(format!(
            "Model '{trimmed}' is a DeepSeek model and is not compatible with provider '{}'. \
             Switch the provider and model together, or pick a model this provider serves.",
            provider.as_str()
        ));
    }

    Ok(())
}

use codewhale_models::canonical_official_deepseek_model_id;

/// Resolve model names accepted by DeepSeek's first-party endpoints.
///
/// The legacy aliases are intentionally handled only in this direct-provider
/// layer. Aggregators and custom endpoints own their model namespaces; for
/// example, Wanjie Ark still documents `deepseek-reasoner` as its native id.
fn canonical_direct_deepseek_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_direct_deepseek_model_id",
        &model.trim().to_ascii_lowercase(),
    )
    .or_else(|| canonical_official_deepseek_model_id(model))
}

fn legacy_deepseek_alias_reasoning_effort(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "legacy_deepseek_alias_reasoning_effort",
        &model.trim().to_ascii_lowercase(),
    )
}

fn canonical_openrouter_recent_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_openrouter_recent_model_id",
        &model.trim().to_ascii_lowercase().replace(['_', ' '], "-"),
    )
}

pub(crate) fn opencode_go_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::opencode_go_model_id(model)
}

fn canonical_xiaomi_mimo_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_xiaomi_mimo_model_id",
        &model.trim().to_ascii_lowercase().replace(['_', ' '], "-"),
    )
}

fn canonical_arcee_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_arcee_model_id",
        &model.trim().to_ascii_lowercase().replace(['_', ' '], "-"),
    )
}

fn canonical_moonshot_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_moonshot_model_id",
        &model.trim().to_ascii_lowercase().replace(['_', ' '], "-"),
    )
}

fn canonical_zai_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_zai_model_id",
        &model.trim().to_ascii_lowercase().replace(['_', ' '], "-"),
    )
}

fn canonical_minimax_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::compatibility_alias(
        "canonical_minimax_model_id",
        &model.trim().to_ascii_lowercase().replace(['_', ' '], "-"),
    )
}

/// Resolve a user-entered model id to the canonical family id a provider
/// understands, without any wire-id translation.
///
/// Most provider-owned families (GLM via Z.ai/Zhipu, Kimi, Xiaomi MiMo,
/// MiniMax, Arcee, OpenRouter slugs, …) resolve through the same "apply the
/// family's canonical map, else pass the input through" path. OpenCode Go is
/// deliberately stricter because one provider roster spans two incompatible
/// wire protocols; only its Chat Completions rows may resolve here.
///
/// This is the canonicalization half of what [`normalize_model_name_for_provider`]
/// used to fuse together. Wire-id translation (e.g. `deepseek-v4-pro` → an
/// aggregator's `accounts/…/deepseek-v4-pro` slug) belongs to the route
/// resolver at request time, not to a name typed into `/provider`, so it is
/// deliberately kept out of here.
///
/// Returns `None` for empty or control-character input and for ids outside the
/// OpenCode Go documented protocol roster. Other provider ids pass through so a
/// custom/self-hosted endpoint is never wrongly rejected.
#[must_use]
pub fn canonical_model_id_for_provider(provider: ProviderKind, model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }

    // Go resolves aliases only within its documented protocol roster.
    if provider == ProviderKind::OpencodeGo {
        return opencode_go_model_id(trimmed).map(str::to_string);
    }

    // Provider-owned model families resolve through their own canonical map,
    // which defines the authoritative casing (`glm-5.1` → `GLM-5.1`,
    // `minimax-m2.7` → `MiniMax-M2.7`). Each map recognizes only *its own*
    // aliases, so an unknown id falls through to passthrough — no family acts
    // as a gate against any other.
    let family_canonical: Option<&'static str> = match provider {
        ProviderKind::Openrouter => canonical_openrouter_recent_model_id(trimmed),
        ProviderKind::XiaomiMimo => canonical_xiaomi_mimo_model_id(trimmed),
        ProviderKind::Arcee => canonical_arcee_model_id(trimmed),
        ProviderKind::Moonshot => canonical_moonshot_model_id(trimmed),
        ProviderKind::Zai => canonical_zai_model_id(trimmed),
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => {
            canonical_minimax_model_id(trimmed)
        }
        _ => None,
    };
    if let Some(canonical) = family_canonical {
        return Some(canonical.to_string());
    }

    // The official DeepSeek API is the one legitimate per-family gate: it serves
    // only its own ids (and 400s anything else), so reject an id it does not
    // recognize. Compact aliases are rewritten (deepseek-v4pro → deepseek-v4-pro)
    // and the caller's casing is kept for an already-valid id (`DeepSeek-V4-Flash`
    // stays as-is). Custom/self-hosted DeepSeek endpoints take the
    // accepts-custom-model-ids path, so they never reach this gate.
    if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        let normalized = normalize_model_name(trimmed)?;
        if let Some(canonical) = canonical_direct_deepseek_model_id(&normalized) {
            if canonical.eq_ignore_ascii_case(&normalized)
                || normalized.to_ascii_lowercase() == canonical
            {
                return Some(normalized);
            }
            return Some(canonical.to_string());
        }
        return Some(normalized);
    }

    // Aggregators that host DeepSeek (NIM, Novita, Fireworks, SiliconFlow, SGLang,
    // vLLM, DeepInfra, Wanjie Ark, Volcengine) canonicalize recognized DeepSeek
    // ids but pass everything else through — they serve more than DeepSeek, so
    // the upstream API stays the authority. A name is never rejected here.
    if matches!(
        provider,
        ProviderKind::NvidiaNim
            | ProviderKind::Novita
            | ProviderKind::Fireworks
            | ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Sglang
            | ProviderKind::Vllm
            | ProviderKind::Deepinfra
            | ProviderKind::WanjieArk
            | ProviderKind::Volcengine
    ) && let Some(canonical) = canonical_official_deepseek_model_id(
        &normalize_model_name(trimmed).unwrap_or_else(|| trimmed.to_string()),
    ) {
        return Some(canonical.to_string());
    }

    // Everything else (HuggingFace, OpenAI-compatible, Qianfan, StepFun, Codex,
    // Anthropic) owns no canonical map — the id the user typed is authoritative.
    Some(trimmed.to_string())
}

/// Normalize a model selected through the TUI for the active provider, applying
/// the provider's wire-slug translation on top of the canonical family id.
///
/// This is the wire-id half of the split (canonicalization lives in
/// [`canonical_model_id_for_provider`]). Used by config-file normalization,
/// where vendor-prefixed ids (e.g. `deepseek-ai/DeepSeek-V4-Pro` on SiliconFlow)
/// are the stored form. `/provider` deliberately uses the canonical half instead.
#[must_use]
pub fn normalize_model_name_for_provider(provider: ProviderKind, model: &str) -> Option<String> {
    let canonical = canonical_model_id_for_provider(provider, model)?;
    // Translate the canonical family id to the provider's wire slug when the
    // provider's API uses vendor-prefixed ids (Together, Siliconflow, NIM, …).
    // `model_for_provider` is a no-op for providers without a wire-slug map, so
    // this is one uniform layer over the equal-treatment canonical resolver.
    Some(model_for_provider(provider, canonical))
}

#[must_use]
pub fn wire_model_for_provider(provider: ProviderKind, model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    if provider == ProviderKind::OpencodeGo {
        // Keep an unknown ID unchanged so validation can reject it by name.
        return opencode_go_model_id(trimmed)
            .map(str::to_string)
            .unwrap_or_else(|| trimmed.to_string());
    }
    if matches!(provider, ProviderKind::XiaomiMimo) {
        return normalize_model_name_for_provider(provider, trimmed)
            .unwrap_or_else(|| trimmed.to_string());
    }
    if provider_passes_model_through(provider) {
        return trimmed.to_string();
    }
    normalize_model_name_for_provider(provider, trimmed).unwrap_or_else(|| trimmed.to_string())
}

/// Resolve the final request model while respecting custom endpoint
/// namespaces. Provider-only normalization cannot distinguish DeepSeek's
/// first-party API from a self-hosted OpenAI-compatible endpoint configured
/// under the legacy `deepseek` provider name, so actual HTTP clients use this
/// route-aware boundary.
#[must_use]
pub fn wire_model_for_provider_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    // A custom endpoint still uses the documented Go model and wire contract.
    if provider == ProviderKind::OpencodeGo {
        return wire_model_for_provider(provider, trimmed);
    }
    if base_url_is_custom_for_provider(provider, base_url) {
        return trimmed.to_string();
    }
    wire_model_for_provider(provider, trimmed)
}

/// Reconcile a remembered `/model` pick with the model the config file names.
///
/// `provider_models` in `settings.toml` remembers the last `/model` (or model
/// picker) selection and outranks `config.toml` on the next launch. The picker
/// offers catalog spellings, which are lowercase, so a user whose config names
/// `DeepSeek-V4-Flash` can end up relaunching into `deepseek-v4-flash` — the
/// wrong id for a self-hosted OpenAI-compatible gateway whose model names are
/// case-sensitive, and the wrong id in the header.
///
/// When the two strings name the *same* model in a different ASCII case, the
/// config file owns the spelling. A remembered pick that names a genuinely
/// different model still wins, so `/model` persistence is unchanged: only the
/// spelling defers, never the selection.
#[must_use]
pub(crate) fn prefer_configured_model_spelling(configured: &str, remembered: String) -> String {
    let configured = configured.trim();
    if remembered != configured && remembered.eq_ignore_ascii_case(configured) {
        return configured.to_string();
    }
    remembered
}

/// Recover the behavioral intent of a retiring alias only when the selected
/// route is a first-party DeepSeek endpoint. Custom endpoints own both the id
/// and its semantics, so they deliberately return `None` here.
pub(crate) fn legacy_deepseek_alias_effort_for_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> Option<&'static str> {
    if !matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        return None;
    }
    let effort = legacy_deepseek_alias_reasoning_effort(model)?;
    (wire_model_for_provider_route(provider, base_url, model) != model.trim()).then_some(effort)
}

/// Reviewed per-provider model id projection used **only as a compatibility
/// fallback** (#4188).
///
/// Preferred sources are the live Models.dev catalog and the offline bundled
/// snapshot via [`crate::provider_lake`]. Call this directly only for
/// Codewhale-only / local providers Models.dev does not represent, or when
/// probing the fallback table in tests. Picker, inventory, and subagent
/// surfaces must go through the provider lake.
#[must_use]
pub fn model_completion_names_for_provider(provider: ProviderKind) -> Vec<&'static str> {
    codewhale_config::catalog::reviewed::constants::completion_names(provider.as_str()).to_vec()
}

// === Types ===

/// `[extension_host]`: settings for the experimental TypeScript extension
/// host (`[features] extension_host`). User config only: project-scope config
/// is applied by an explicit allowlist that does not include this table.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHostConfig {
    /// Explicit MCP protocol backend. Rust remains default; Host supports stdio,
    /// HTTP and SSE independently of optional Native extension activation.
    #[serde(default)]
    pub mcp_backend: crate::mcp::McpBackend,
    /// Which runtime runs the host: `node` (the default), `bun`, or `auto`
    /// (Bun when a supported one is found and starts, else Node). `bun` and
    /// `auto` are opt-ins: Bun is not the default until it is qualified on
    /// every platform. An explicit `bun` or `node` never falls back to the
    /// other runtime. Unset means `node`, except that a table setting only
    /// `bun` means `bun` ([`Self::effective_runtime`]).
    #[serde(default)]
    pub runtime: Option<ExtensionHostRuntime>,
    /// Path to a Node.js runtime (`^22.19 || >=24`). When set it is the only
    /// Node candidate: if it does not run or is below the floor, Node
    /// resolution fails with that reason instead of searching `PATH`.
    /// Unset, every `node` on `PATH` is tried in order, skipping any inside
    /// a `node_modules` directory or the working directory.
    #[serde(default)]
    pub node: Option<String>,
    /// Path to a Bun runtime (>= 1.4.0). When set it is the only Bun
    /// candidate, as for `node`. Unset, `bun` on `PATH` and then
    /// `$BUN_INSTALL/bin` (default `~/.bun/bin`) are tried, with the same
    /// skips.
    #[serde(default)]
    pub bun: Option<String>,
}

impl ExtensionHostConfig {
    /// `runtime` as configured, else `bun` when only a Bun path is set (the
    /// table names no other runtime), else `node`.
    #[must_use]
    pub fn effective_runtime(&self) -> ExtensionHostRuntime {
        match (self.runtime, &self.node, &self.bun) {
            (Some(runtime), _, _) => runtime,
            (None, None, Some(_)) => ExtensionHostRuntime::Bun,
            (None, _, _) => ExtensionHostRuntime::Node,
        }
    }
}

/// `[extension_host] runtime`. Node is the default; Bun (`bun`, or `auto`,
/// which prefers it) stays an opt-in until an explicit, recorded cutover.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionHostRuntime {
    Auto,
    Bun,
    #[default]
    Node,
}

impl ExtensionHostRuntime {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Bun => "bun",
            Self::Node => "node",
        }
    }
}

/// `[plugins."<name>"]`: per-plugin settings, keyed by the plugin's manifest
/// name. User config only (like `[extension_host]`): project-scope config is
/// applied by an explicit allowlist that does not include this table, so a
/// repository cannot configure the plugins it asks you to trust.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginSettings {
    /// `[plugins."<name>".config]`: a TOML table delivered as the `config`
    /// argument of the plugin's `apply(ctx, config)` when the extension host
    /// activates it, validated against the plugin's own `Config` schema when it
    /// declares one. A change takes effect at `/plugin reload` (the plugin is
    /// re-activated). Values are not secrets storage: they are shown to the
    /// plugin's code, and `/plugin show` lists their keys.
    #[serde(default)]
    pub config: Option<toml::Table>,
}

/// The `[plugins]` table of the user config file at `path`, read on its own so
/// an edit reaches the extension host at `/plugin reload` without reloading
/// the whole configuration. A missing file has no settings. Blocking.
pub(crate) fn read_plugin_settings(
    path: &Path,
) -> std::result::Result<BTreeMap<String, PluginSettings>, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    // The same parse the whole configuration goes through; its error text
    // omits file contents, which can hold secrets.
    parse_config_file(&contents)
        .map(|file| file.base.plugins.unwrap_or_default())
        .map_err(|_| format!("cannot parse {}", path.display()))
}

/// Raw retry configuration loaded from config files.
#[derive(Debug, Clone, Deserialize)]
pub struct RetryConfig {
    pub enabled: Option<bool>,
    pub max_retries: Option<u32>,
    pub initial_delay: Option<f64>,
    pub max_delay: Option<f64>,
    pub exponential_base: Option<f64>,
    /// #6700: randomize each backoff delay by `jitter_factor`. Default `true`.
    #[serde(default)]
    pub jitter: Option<bool>,
    /// #6700: jitter spread as a fraction of the delay (`0.1` = ±10%).
    /// Default `0.1`; values clamp to `0.0..=1.0`, non-finite values use the
    /// default.
    #[serde(default)]
    pub jitter_factor: Option<f64>,
    /// #6700: honor a server `Retry-After` header instead of the computed
    /// backoff. Default `true`.
    #[serde(default)]
    pub respect_retry_after: Option<bool>,
}

/// Deserialize `status_items` tolerantly: skip keys unknown to this build
/// instead of erroring with "unknown variant".  This lets a dev build write
/// `"balance"` (or any future item) while the stable build still parses the
/// config file successfully.
fn deser_status_items<'de, D>(deserializer: D) -> Result<Option<Vec<StatusItem>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<Vec<String>> = Option::deserialize(deserializer)?;
    Ok(raw.map(|strings| {
        strings
            .into_iter()
            .filter_map(|s| {
                StatusItem::from_key(&s).or_else(|| {
                    tracing::warn!("ignoring unknown status item {s:?} in config");
                    None
                })
            })
            .collect()
    }))
}

/// Canonical model-stream and transport settings. The existing `Config`
/// accessors own resolution; `[tui]` spellings remain read-only compatibility.
/// Transport changes apply to newly constructed clients, not active requests.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StreamConfig {
    pub open_timeout_secs: Option<u64>,
    pub chunk_timeout_secs: Option<u64>,
    pub force_http1: Option<bool>,
    pub max_resumes: Option<u32>,
    pub max_transparent_retries: Option<u32>,
    pub max_stream_errors: Option<u32>,
    pub max_duration_secs: Option<u64>,
    pub max_content_mb: Option<u64>,
    pub connect_timeout_secs: Option<u64>,
    /// Omitted keeps 30 seconds; zero disables TCP keepalive.
    pub tcp_keepalive_secs: Option<u64>,
    /// Omitted keeps 15 seconds; zero disables HTTP/2 PINGs.
    pub http2_keep_alive_interval_secs: Option<u64>,
    /// Omitted or zero keeps the 20-second PING acknowledgement deadline.
    pub http2_keep_alive_timeout_secs: Option<u64>,
}

/// UI configuration loaded from config files.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TuiConfig {
    pub alternate_screen: Option<String>,
    pub mouse_capture: Option<bool>,
    /// Copy a transcript drag selection as Markdown source (`true`, the
    /// default) instead of rendered terminal text. The payload projects every
    /// intersected cell through the same canonical serialization Ctrl-Y and
    /// `/copy` use. Set `false` to restore the rendered-text payload (#6156).
    /// PRIMARY selection on Linux always keeps rendered text.
    pub selection_copy_markdown: Option<bool>,
    /// Per-SSE-chunk idle timeout in seconds. Defaults to 900 seconds when
    /// omitted. `0` maps to the default; values clamp to `1..=3600`.
    pub stream_chunk_timeout_secs: Option<u64>,
    /// Optional ceiling on model steps in a single turn. Omitted or `0`
    /// leaves model steps uncapped; explicit positive values clamp to
    /// `1..=100_000`. Wall-clock and stream budgets remain independent.
    pub max_model_steps: Option<u32>,
    /// R1: cumulative wall-clock budget for a single turn, in seconds.
    /// Omitted or `0` resolve to the finite default (3600); explicit values
    /// clamp to `30..=86_400`. Time blocked on a human approval decision is
    /// excluded from the measurement.
    pub turn_wall_clock_secs: Option<u64>,
    /// R1: per-step cap on accumulated streamed content, in megabytes.
    /// Omitted or `0` resolve to the default (10 MB); explicit values clamp
    /// to `64 KiB..=512 MiB`.
    pub stream_max_content_mb: Option<u64>,
    /// R1: per-step cap on a single stream's wall-clock duration, in
    /// seconds. Omitted or `0` resolve to the default (1800); explicit
    /// values clamp to `10..=86_400`.
    pub stream_max_duration_secs: Option<u64>,
    /// #6700: whole-request re-issues after a failed stream — a stream that
    /// never opened (#6699), died before content, or dropped mid-stream.
    /// Omitted resolves to the default (3); `0` disables them; values clamp
    /// to `0..=10`.
    pub stream_max_resumes: Option<u32>,
    /// #6700: in-stream re-requests while nothing has streamed yet (#103).
    /// Omitted resolves to the default (2); `0` disables them; values clamp
    /// to `0..=10`.
    pub stream_max_transparent_retries: Option<u32>,
    /// #6700: recoverable errors tolerated within one stream before it
    /// ends. Omitted or `0` resolves to the default (5); other values clamp
    /// to `1..=50`.
    pub stream_max_errors: Option<u32>,
    /// #6700: wait for SSE response headers, in seconds. Omitted or `0`
    /// fall back to `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`, then 45; values
    /// clamp to `5..=300`.
    pub stream_open_timeout_secs: Option<u64>,
    /// #6700: TCP/TLS connect timeout for the model HTTP client, in seconds.
    /// Omitted or `0` resolve to the default (30); values clamp to `1..=300`.
    pub connect_timeout_secs: Option<u64>,
    /// #6700: pin the model HTTP client to HTTP/1.1 (config form of
    /// `CODEWHALE_FORCE_HTTP1`). Omitted or `false` leaves HTTP/2 on unless
    /// the env var is truthy; either one pins.
    pub force_http1: Option<bool>,
    /// Ordered list of footer items the user wants visible. `None` (the field
    /// missing from `config.toml`) means "use the built-in default order"; an
    /// empty `Some(vec![])` means "show nothing in the footer".
    ///
    /// Edited interactively via `/statusline`; persisted to `tui.status_items`
    /// in `~/.deepseek/config.toml`.
    #[serde(default, deserialize_with = "deser_status_items")]
    pub status_items: Option<Vec<StatusItem>>,
    /// How much of the posture bar — the first row under the composer — to
    /// paint: `full` (default), `compact`, or `hidden`. `hidden` gives the
    /// row back to the transcript; `compact` keeps the row and starts its
    /// shed ladder past the clocks, counts and hints (#5950).
    ///
    /// `status_items` still composes what is *in* the row; this only decides
    /// the row's size. Absent from an older `config.toml` means `full`.
    #[serde(default)]
    pub posture_bar: Option<ChromeRowPreset>,
    /// The same three settings for the metrics line under the posture bar.
    /// `compact` is the default: it keeps the route, the context reading, the cost and the
    /// balance, the cache rate, and drops the other telemetry and the help hint (#5950, #6565).
    #[serde(default)]
    pub metrics_line: Option<ChromeRowPreset>,
    /// Emit OSC 8 hyperlink escape sequences around URLs in the transcript so
    /// supporting terminals (iTerm2, Terminal.app 13+, Ghostty, Kitty,
    /// WezTerm, Alacritty, recent gnome-terminal/konsole) make them clickable
    /// with the terminal's link gesture (usually Cmd-click on macOS and
    /// Ctrl-click on Linux/Windows). Terminals without OSC 8 support render the
    /// plain label and ignore the escape. Defaults to on for macOS/Linux and
    /// off for Windows legacy consoles; set `false` to suppress everywhere
    /// (e.g. for a terminal that misrenders the sequence). OSC 8 escapes are
    /// emitted out-of-band, so buffer-column corruption is not a concern.
    pub osc8_links: Option<bool>,
    /// High-level notification trigger condition. When set, controls whether
    /// operator notifications may interrupt the current terminal and, for
    /// `Always`, overrides the `[notifications].threshold_secs` gate from the
    /// lower-level `[notifications]` block:
    ///
    /// - `Always` — allow configured operator notifications even while the
    ///   terminal is focused. Successful turn completion also ignores the
    ///   duration threshold. Method, category, and quiet gates still apply.
    /// - `Unfocused` — notify only after the terminal has remained unfocused
    ///   for the built-in attention grace period. The normal duration
    ///   threshold still applies.
    /// - `Never` — suppress all operator notifications.
    /// - Unset (default) — behave like `Unfocused` and fall back to the
    ///   `[notifications]` duration threshold.
    pub notification_condition: Option<NotificationCondition>,
    /// When `true`, plain Up/Down on an empty composer scroll the
    /// transcript instead of recalling input history. Useful for
    /// terminals that map mouse-wheel gestures to arrow keys. Default:
    /// `true` only when mouse capture is off; otherwise `false`.
    #[serde(default)]
    pub composer_arrows_scroll: Option<bool>,
}

/// How much of one bottom-chrome row to paint (#5950). One value for each
/// of the two rows under the composer — [`TuiConfig::posture_bar`] and
/// [`TuiConfig::metrics_line`] — so a small tmux pane can give one or both
/// rows back to the transcript without touching `status_items`.
///
/// `compact` is not a second renderer: it starts the row's existing shed
/// ladder at a fixed rung and lets width shed the rest, so what it keeps is
/// exactly what a narrow row keeps.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChromeRowPreset {
    /// Every fact the row owns, shed only by width.
    #[default]
    Full,
    /// The row's shed ladder started past its most expendable rungs.
    Compact,
    /// No row: the transcript takes the line.
    Hidden,
}

impl ChromeRowPreset {
    /// Every setting value, in the order `/config` names them.
    pub const SETTINGS: [&'static str; 3] = ["full", "compact", "hidden"];

    /// Stable name used in `config.toml` and `/config`.
    #[must_use]
    pub const fn as_setting(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
            Self::Hidden => "hidden",
        }
    }

    /// Reverse of [`Self::as_setting`]; `None` for anything else.
    #[must_use]
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "compact" => Some(Self::Compact),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }
}

#[cfg(test)]
pub use codewhale_config::notifications::{EventSoundConfig, NotificationEventsConfig};

pub use codewhale_config::notifications::{
    CompletionSound, NotificationCondition, NotificationConfigUpdate, NotificationMethod,
    NotificationSetting, NotificationsConfig, SubagentCompletionNotification,
};

fn default_snapshots_enabled() -> bool {
    true
}

fn default_snapshot_max_age_days() -> u64 {
    crate::snapshot::DEFAULT_MAX_AGE.as_secs() / (24 * 60 * 60)
}

fn default_snapshot_max_workspace_gb() -> u64 {
    crate::snapshot::DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT / (1024 * 1024 * 1024)
}

/// Workspace side-git snapshot configuration (#137).
#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotsConfig {
    /// Snapshot the workspace before and after each interactive agent turn.
    #[serde(default = "default_snapshots_enabled")]
    pub enabled: bool,
    /// Prune side-git snapshots older than this many days at session boot.
    #[serde(default = "default_snapshot_max_age_days")]
    pub max_age_days: u64,
    /// Maximum non-excluded workspace size (in GB) before the snapshot
    /// feature self-disables on first use. Set to `0` to disable the cap
    /// and snapshot regardless of size (the v0.8.31 behavior). The walk
    /// honors `.gitignore` and the snapshot module's built-in excludes
    /// (`node_modules/`, `target/`, ...) so the measured size reflects
    /// what would actually land in a snapshot commit.
    #[serde(default = "default_snapshot_max_workspace_gb")]
    pub max_workspace_gb: u64,
}

impl Default for SnapshotsConfig {
    fn default() -> Self {
        Self {
            enabled: default_snapshots_enabled(),
            max_age_days: default_snapshot_max_age_days(),
            max_workspace_gb: default_snapshot_max_workspace_gb(),
        }
    }
}

/// User-level memory configuration (#489).
///
/// Default is opt-in: when this table is absent or `enabled = false`, the
/// memory file is neither read nor written, and `# foo` quick-adds in the
/// composer fall through to the normal turn-submission path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryBackend {
    Native,
    #[default]
    Off,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MemoryConfig {
    /// When `true`, load the user memory file at `Config::memory_path()`
    /// into the system prompt as a `<user_memory>` block, and intercept
    /// `# foo` typed in the composer to append to that file. Default `false`.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Explicit backend selection for the v0.9.2 memory lifecycle.
    /// `None` preserves the pre-native opt-in behavior for old configs.
    #[serde(default)]
    pub backend: Option<MemoryBackend>,
}

/// Xiaomi MiMo speech/TTS output configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SpeechConfig {
    /// Default directory for generated speech/TTS files when no explicit
    /// output path is provided.
    #[serde(default)]
    pub output_dir: Option<String>,
}

impl SnapshotsConfig {
    #[must_use]
    pub fn max_age(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.max_age_days.saturating_mul(24 * 60 * 60))
    }
}

// Web-search `[search]` table types live in the `search` leaf module and are
// re-exported below so `crate::config::SearchProvider` (and siblings) resolve
// unchanged (#3311).
mod search;
pub use search::*;

/// Model-visible tool catalog controls (`[tools]` table in config.toml).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolsConfig {
    /// Native tool names to keep loaded even when they are outside the small
    /// default core catalog. Unknown names are harmless and simply never match.
    #[serde(default)]
    pub always_load: Vec<String>,

    /// Optional directory to scan for plugin tool scripts. Scripts with a
    /// frontmatter header (`# name:`, `# description:`, `# schema:`) are
    /// auto-discovered and registered as tools.
    ///
    /// Defaults to `~/.codewhale/tools/` when `None`.
    #[serde(default)]
    pub plugin_dir: Option<String>,

    /// Per-tool overrides keyed by tool name. `disabled` turns any tool off,
    /// built-ins included; `script` / `command` adds a tool under a name no
    /// built-in owns (or replaces a drop-in script of that name). A `script` /
    /// `command` entry keyed by a built-in is refused and the built-in stays
    /// active (D4; see `ToolRegistry::apply_overrides`).
    #[serde(default)]
    pub overrides: Option<HashMap<String, ToolOverride>>,

    /// Ceiling on how many questions one `request_user_input` call may ask
    /// (#5949). `None` uses
    /// [`crate::tools::user_input::DEFAULT_MAX_QUESTIONS`] (6). Values outside
    /// `1..=10` are clamped with a warning rather than failing the load.
    #[serde(default)]
    pub user_input_max_questions: Option<u32>,

    /// Ceiling on how many options each `request_user_input` question may
    /// offer. `None` uses [`crate::tools::user_input::DEFAULT_MAX_OPTIONS`]
    /// (4). Values outside `2..=10` are clamped with a warning.
    #[serde(default)]
    pub user_input_max_options: Option<u32>,

    /// Seconds Codewhale waits for a `request_user_input` answer before
    /// cancelling it (#6003). Absent, or an explicit `0`, waits until the
    /// person answers or cancels — the same as an approval. A positive
    /// value bounds that one wait. Values above 86,400 (24h) are clamped
    /// with a warning.
    #[serde(default)]
    pub user_input_timeout_seconds: Option<u64>,
}

/// Persistent-goal loop controls (`[goal]` table in config.toml, #5052).
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
pub struct GoalConfig {
    /// Optional safety backstop on automatic goal continuation passes.
    /// Goals are unlimited by default; token/time budgets are telemetry only.
    ///
    /// `None` uses the built-in default
    /// ([`crate::goal_loop::DEFAULT_MAX_GOAL_CONTINUATIONS`], currently `0`);
    /// `0` disables the backstop entirely so only terminal status or user
    /// control ends the run.
    #[serde(default)]
    pub max_continuations: Option<u32>,

    /// Optional per-engine-turn step ceiling while a goal is active (#6512).
    /// Like ordinary turns, `None` or `0` leaves model steps uncapped;
    /// positive values clamp to `1..=100,000`. This bounds each turn, never
    /// the number of continuation passes; explicit per-invocation ceilings
    /// (`exec --max-turns`, child-worker caps) still win.
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Optional quiet period between successful cross-turn continuations.
    /// `0` preserves immediate continuation. Positive values make long-lived
    /// coordinator goals yield visibly between turns instead of sleeping
    /// inside a provider turn.
    #[serde(default)]
    pub continuation_delay_seconds: Option<u64>,

    /// Make a goal's `token_budget` a hard stop instead of advisory telemetry
    /// (#6013). `false`/`None` preserves current behavior: crossing the budget
    /// logs and continues. `true` stops the run with `BudgetLimit` once
    /// `tokens_used >= token_budget`; goals created without a token budget
    /// stay unbounded either way.
    #[serde(default)]
    pub enforce_token_budget: Option<bool>,
}

/// Reasoning-only recovery controls (`[reasoning_only]` table in config.toml).
/// When the model returns only hidden reasoning (thinking) without any answer
/// text or tool call, the engine can re-request the answer automatically.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct ReasoningOnlyConfig {
    /// Maximum number of automatic re-requests when the model returns only
    /// reasoning without any answer or tool call.
    /// Defaults to 2. Set to 0 to disable automatic recovery.
    pub max_reprompts: Option<u32>,
    /// Nudge attached to a reasoning-only re-request after the first bare
    /// retry has already come back answerless. Unset uses the built-in text.
    ///
    /// The nudge rides a single outbound request and is never added to the
    /// session, so it does not persist, does not appear in the transcript or
    /// exports, and is not re-sent on later turns.
    pub reprompt_message: Option<String>,
}

/// One configurable bottom-chrome item.
///
/// Every variant owns exactly one thing on screen, and `/statusline` turns
/// that thing on or off:
///
/// | Variant | What it paints |
/// | --- | --- |
/// | `Mode` | the posture bar's `plan`/`act`/`operate` chip |
/// | `Model` | the metrics line's route segment |
/// | `ContextPercent` | the metrics line's `ctx NN%` reading |
/// | `Cost` | the metrics line's session price |
/// | `SessionMetrics` | the metrics line's `ttft` and `tok/s` |
/// | `Cache` | the metrics line's `cache NN%` |
/// | `Tokens` | the metrics line's `↓ NNN` output tokens |
/// | `Balance` | the metrics line's prepaid-credit reading (also gates the fetch) |
/// | `Workspace` | the metrics line's workspace leaf-directory chip |
/// | `GitBranch` | the metrics line's current-branch chip (short SHA when detached) |
///
/// A variant that paints nothing does not belong here. Eight variants were
/// retired in #5950 because the 0.9.12 shell gave their facts to a surface
/// `/statusline` does not own — the posture bar's clock and live counts
/// (`Status`, `Agents`), the launch header and git dock (`GitBranch`) — or
/// because they were never wired at all (`ReasoningReplay`,
/// `PrefixStability`, `LastToolElapsed`, `RateLimit`). #6112 revived
/// `GitBranch` (and added `Workspace`) as opt-in metrics-line chips fed by
/// the cached workspace context, not by a per-frame git call. The other
/// retired keys still parse out of an old `config.toml`:
/// [`StatusItem::from_key`] returns `None` and `deser_status_items` skips
/// them with a warning, so an upgrader's file keeps loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StatusItem {
    /// "act" / "plan" / "operate" chip.
    Mode,
    /// Model identifier (e.g. `deepseek-v4-pro`).
    Model,
    /// Session cost in the configured display currency.
    Cost,
    /// Cache hit rate ("cache 73%").
    Cache,
    /// Context-window utilisation percent ("ctx 48%").
    ContextPercent,
    /// Output tokens of the live or last turn ("↓ 1.2K").
    Tokens,
    /// Prepaid remaining credit, refreshed once per turn completion.
    Balance,
    /// Legacy configuration alias enabling both TTFT and output rate.
    /// The picker expands it into independently editable readings.
    SessionMetrics,
    /// Mean measured time from request dispatch to the first token.
    Ttft,
    /// Provider output tokens divided by measured request time.
    OutputRate,
    /// Leaf directory of the session workspace, left-truncated when long.
    /// Opt-in (#6112); off the default footer.
    Workspace,
    /// Current git branch from the cached workspace context — the short SHA
    /// when HEAD is detached, absent outside a repository. Opt-in (#6112);
    /// off the default footer.
    GitBranch,
}

impl StatusItem {
    /// Default footer composition for the always-on status line. Used when
    /// `tui.status_items` is missing from `config.toml` so upgraders see a
    /// concise footer by default; diagnostic chips remain available via
    /// `/statusline` without crowding the main UI.
    #[must_use]
    pub fn default_footer() -> Vec<StatusItem> {
        vec![
            StatusItem::Mode,
            StatusItem::Model,
            StatusItem::ContextPercent,
            StatusItem::Cost,
            StatusItem::Cache,
            StatusItem::Tokens,
            StatusItem::Ttft,
            StatusItem::OutputRate,
        ]
    }

    /// Stable canonical name used in TOML and the picker label.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            StatusItem::Mode => "mode",
            StatusItem::Model => "model",
            StatusItem::Cost => "cost",
            StatusItem::Cache => "cache",
            StatusItem::ContextPercent => "context_percent",
            StatusItem::Tokens => "tokens",
            StatusItem::Balance => "balance",
            StatusItem::SessionMetrics => "session_metrics",
            StatusItem::Ttft => "ttft",
            StatusItem::OutputRate => "output_rate",
            StatusItem::Workspace => "workspace",
            StatusItem::GitBranch => "git_branch",
        }
    }

    /// Reverse of [`key`](Self::key): parse a config string back to a variant.
    /// Returns `None` for unknown keys so the config parser can silently skip
    /// items added by newer versions rather than crashing with "unknown variant".
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "mode" => Some(Self::Mode),
            "model" => Some(Self::Model),
            "cost" => Some(Self::Cost),
            "cache" => Some(Self::Cache),
            "context_percent" => Some(Self::ContextPercent),
            "tokens" => Some(Self::Tokens),
            // Retired in #5950; skipped rather than rejected so an old
            // `config.toml` still parses. See the type's doc comment.
            "status" | "agents" | "reasoning_replay" | "prefix_stability" | "last_tool_elapsed"
            | "rate_limit" => None,
            "balance" => Some(Self::Balance),
            "session_metrics" => Some(Self::SessionMetrics),
            "ttft" => Some(Self::Ttft),
            "output_rate" => Some(Self::OutputRate),
            "workspace" => Some(Self::Workspace),
            // Revived in #6112 as an opt-in metrics-line chip; it parses
            // again, so a config written between its #5950 retirement and
            // the revival simply gets the chip back.
            "git_branch" => Some(Self::GitBranch),
            _ => None,
        }
    }

    /// Human-readable label for the picker.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            StatusItem::Mode => "Mode",
            StatusItem::Model => "Model",
            StatusItem::Cost => "Session cost",
            StatusItem::Cache => "Prompt cache hit rate",
            StatusItem::ContextPercent => "Context window %",
            StatusItem::Tokens => "Output tokens",
            StatusItem::Balance => "Account balance",
            StatusItem::SessionMetrics => "Session metrics",
            StatusItem::Ttft => "Time to first token",
            StatusItem::OutputRate => "Output rate",
            StatusItem::Workspace => "Workspace",
            StatusItem::GitBranch => "Git branch",
        }
    }

    /// One-line hint shown beside the label so the user knows what each item
    /// surfaces without having to toggle it on first.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            StatusItem::Mode => "plan · act · operate",
            StatusItem::Model => "the model id you'll send to",
            StatusItem::Cost => "running total for this session",
            StatusItem::Cache => "% of prompt served from cache",
            StatusItem::ContextPercent => "tokens used / model context window",
            StatusItem::Tokens => "output tokens of the live or last turn",
            StatusItem::Balance => "remaining prepaid credit from the active provider",
            StatusItem::SessionMetrics => "time to first token and output rate",
            StatusItem::Ttft => "average wait for the first token",
            StatusItem::OutputRate => "average tok/s, including first-token wait",
            StatusItem::Workspace => "directory this session writes to",
            StatusItem::GitBranch => "branch the next commit lands on",
        }
    }

    /// Editable items in display order. Legacy combined metrics parse but
    /// expand to the two individual controls instead of appearing twice.
    #[must_use]
    pub fn all() -> &'static [StatusItem] {
        &[
            StatusItem::Mode,
            StatusItem::Model,
            StatusItem::ContextPercent,
            StatusItem::Cost,
            StatusItem::Balance,
            StatusItem::Cache,
            StatusItem::Tokens,
            StatusItem::Ttft,
            StatusItem::OutputRate,
            StatusItem::Workspace,
            StatusItem::GitBranch,
        ]
    }

    /// Whether this item is relevant for `provider`.  Provider-specific
    /// items return `false` for unsupported providers so the picker doesn't
    /// offer toggles that can never show useful data.
    #[must_use]
    pub fn is_available_for(self, provider: ProviderKind) -> bool {
        match self {
            StatusItem::Balance => provider_has_balance_api(provider),
            _ => true,
        }
    }
}

/// Prepaid providers that publish a remaining-credit endpoint Codewhale
/// can fetch. Local runtimes and invoice-only vendors stay out.
#[must_use]
pub fn provider_has_balance_api(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::Deepseek
            | ProviderKind::Openrouter
            | ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
    )
}

/// Resolved retry policy with defaults applied.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub initial_delay: f64,
    pub max_delay: f64,
    pub exponential_base: f64,
    pub jitter: bool,
    pub jitter_factor: f64,
    pub respect_retry_after: bool,
}

/// Context management configuration.
///
/// `project_pack` is the only setting. The removed "Flash seam" layered-context
/// keys (`enabled`, `verbatim_window_turns`, `l1/l2/l3_threshold`,
/// `seam_model`, #159) are no longer fields: this table does not deny unknown
/// fields, so an older config that still carries them keeps loading and they
/// are simply ignored (#6516).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ContextConfig {
    /// Include a deterministic project context pack in the stable prompt
    /// prefix. Default: false — the pack is a large pretty-printed directory
    /// listing the model can rebuild with one `File` call (#4781). Set
    /// `[context] project_pack = true` to opt in (useful for weak tool-calling
    /// models).
    #[serde(default)]
    pub project_pack: Option<bool>,
}

/// Maximum characters of `[compaction] summary_instructions` that are
/// appended to the summarizer prompt. Longer values are truncated (with a
/// warning) rather than rejected: a long standing instruction should not fail
/// the compaction pass that keeps the session alive.
pub const COMPACTION_SUMMARY_INSTRUCTIONS_MAX_CHARS: usize = 4_000;
/// Default verbatim retention budget for recent plain user messages in the
/// compaction replacement history (Codex parity). Unset config keeps this.
pub const DEFAULT_COMPACTION_RETAINED_USER_MESSAGE_TOKENS: usize = 20_000;
/// Lower clamp: below this the replacement history stops carrying a usable
/// amount of the user's own words.
pub const MIN_COMPACTION_RETAINED_USER_MESSAGE_TOKENS: usize = 2_000;
/// Upper clamp: above this the "compacted" history is large enough to
/// re-trigger compaction on the next turn.
pub const MAX_COMPACTION_RETAINED_USER_MESSAGE_TOKENS: usize = 200_000;

/// Compaction summarizer tuning (`[compaction]`, #5956).
///
/// `auto_compact` / `auto_compact_threshold_percent` (settings.toml) decide
/// *when* compaction runs; these two keys decide *how* the pass behaves. Both
/// are absent by default and absent means today's built-in behavior, byte for
/// byte.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CompactionSettings {
    /// Standing operator instructions appended to the built-in summarizer
    /// prompt on every pass, manual and automatic — the effort-free
    /// counterpart to a one-off `/compact <focus>`, which still composes
    /// after this text. Empty or whitespace-only is treated as unset;
    /// longer than [`COMPACTION_SUMMARY_INSTRUCTIONS_MAX_CHARS`] is
    /// truncated where it is applied.
    #[serde(default)]
    pub summary_instructions: Option<String>,
    /// Token budget for the recent plain user messages kept verbatim in the
    /// replacement history. Clamped to
    /// [`MIN_COMPACTION_RETAINED_USER_MESSAGE_TOKENS`]..=
    /// [`MAX_COMPACTION_RETAINED_USER_MESSAGE_TOKENS`]; unset keeps
    /// [`DEFAULT_COMPACTION_RETAINED_USER_MESSAGE_TOKENS`].
    #[serde(default, alias = "retained_user_message_max_tokens")]
    pub retained_user_message_tokens: Option<usize>,
}

/// Fleet-role model overrides for delegated workers. Canonical keys in
/// `models` are `worker`, `scout`, `planner`, `reviewer`, `builder`,
/// `verifier`, and `custom`. Legacy sub-agent type names remain accepted for
/// v0.9.x compatibility. Explicit manual pins are authoritative at admission.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SubagentsConfig {
    /// Top-level switch for the model-facing `agent` tool. `None` preserves
    /// the feature-flag default; `false` hides/refuses sub-agent spawning
    /// without changing the numeric queue/depth knobs.
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub worker_model: Option<String>,
    #[serde(default, rename = "scout_model", alias = "explorer_model")]
    pub explorer_model: Option<String>,
    #[serde(default, rename = "planner_model", alias = "awaiter_model")]
    pub awaiter_model: Option<String>,
    #[serde(default, rename = "reviewer_model", alias = "review_model")]
    pub review_model: Option<String>,
    #[serde(default)]
    pub custom_model: Option<String>,
    #[serde(default)]
    pub models: Option<HashMap<String, String>>,
    /// Structured role pins, folded into the same override map as the legacy
    /// scalar and `models` inputs. These entries take precedence over both.
    #[serde(default)]
    pub roles: Option<HashMap<String, SubagentRoleConfig>>,
    /// Maximum concurrent sub-agents. Overrides the top-level max_subagents
    /// setting. Clamped to [1, MAX_SUBAGENTS].
    #[serde(default)]
    pub max_concurrent: Option<usize>,
    /// How many levels of nested sub-agents the interactive `agent` tool may
    /// spawn. `0` blocks the model-facing `agent` tool at this runtime depth;
    /// use `[subagents] enabled = false` for the clearer durable off switch.
    /// `1` allows one level, `2` two, and so on. When unset, defaults to
    /// [`codewhale_config::DEFAULT_SPAWN_DEPTH`]; any value is clamped to
    /// [`codewhale_config::MAX_SPAWN_DEPTH_CEILING`]. Fleet workers are
    /// governed separately by `[fleet.exec] max_spawn_depth`; both share the
    /// same default and ceiling so the limit cannot drift.
    #[serde(default)]
    pub max_depth: Option<u32>,
    /// Number of direct (depth-1) sub-agents that may execute concurrently
    /// before further launches queue for a launch slot (#3095). When unset,
    /// defaults to the full resolved `max_subagents()` (no artificial
    /// throttle); explicit values are clamped to [1, max_subagents].
    #[serde(default)]
    pub launch_concurrency: Option<usize>,
    /// Maximum queued + running sub-agents admitted for one session. Defaults
    /// to a large bounded queue while `launch_concurrency` keeps instantaneous
    /// execution bounded.
    #[serde(default, alias = "max_total", alias = "admission_limit")]
    pub max_admitted: Option<usize>,
    /// Deprecated pre-v0.8.61 alias for `launch_concurrency`. Honored only
    /// when `launch_concurrency` is unset, so the new key always wins.
    #[serde(default, rename = "interactive_max_launch")]
    pub interactive_max_launch_legacy: Option<usize>,
    /// Per-step DeepSeek API timeout for sub-agent requests, in seconds. The
    /// timeout wraps `client.create_message` so a stuck single step cannot
    /// pin the parent's parent-completion wakeup channel indefinitely.
    /// Defaults to `DEFAULT_SUBAGENT_API_TIMEOUT_SECS` (600) and is clamped
    /// to `MIN_SUBAGENT_API_TIMEOUT_SECS..=MAX_SUBAGENT_API_TIMEOUT_SECS`
    /// (1..=3600). Zero or unset uses the 600s default (#1806, #1808).
    #[serde(default)]
    pub api_timeout_secs: Option<u64>,
    /// Wall-clock timeout for a running sub-agent that stops making
    /// manager-visible progress. Defaults to 5 minutes and is kept above the
    /// per-step API timeout so slow but legitimate model calls are not
    /// cancelled before their request timeout can fire (#2614).
    #[serde(default)]
    pub heartbeat_timeout_secs: Option<u64>,
    /// Default per-child model-turn budget applied when an `agent` start
    /// carries no explicit `max_steps` (#5324). Unset or zero remains
    /// unbounded for every Fleet role; positive values are clamped to the
    /// runtime ceiling (2000) at resolution.
    #[serde(default)]
    pub default_max_steps: Option<u32>,
    /// Default per-child wall-clock budget in seconds applied when an
    /// `agent` start carries no explicit `wall_time_secs` (#5324). When
    /// unset, children get 1800s; values are clamped to 1..=86400 at
    /// resolution.
    #[serde(default)]
    pub default_wall_time_secs: Option<u64>,
    /// Per-provider overrides for sub-agent fanout and budget knobs. Keys are
    /// provider names such as `deepseek`, `zai`, `openrouter`, or `anthropic`.
    #[serde(default)]
    pub providers: Option<HashMap<String, SubagentProviderConfig>>,
}

/// One authored role pin. This new field accepts an explicit `provider/model`
/// declaration, or a bare model on the session provider. Legacy model inputs
/// retain their opaque provider-owned ids, including any slashes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentRoleConfig {
    pub model: String,
    /// Operator-approved `provider/model` routes, tried in order only when
    /// this pin's first request is refused before the agent has done any
    /// work (exhausted quota, rejected credentials or authorization, or an
    /// unavailable model). Listing a route authorizes sending the agent's
    /// task to that provider. Empty keeps the pin exact.
    #[serde(default)]
    pub replacements: Vec<String>,
}

fn parse_subagent_role_pin(value: &str) -> SubagentModelOverride {
    let value = value.trim();
    match value.split_once('/') {
        Some((provider, model)) => SubagentModelOverride {
            provider: Some(provider.trim().to_string()),
            model: model.trim().to_string(),
        },
        None => value.into(),
    }
}

/// One role override carried through Config, Engine, and child admission.
/// Provider identity is retained until the existing route resolver binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentModelOverride {
    pub provider: Option<String>,
    pub model: String,
}

impl From<String> for SubagentModelOverride {
    fn from(model: String) -> Self {
        Self {
            provider: None,
            model,
        }
    }
}

impl From<&str> for SubagentModelOverride {
    fn from(model: &str) -> Self {
        model.to_string().into()
    }
}

/// Provider-specific sub-agent limit overrides.
///
/// Every field inherits from `[subagents]` when unset, so a provider profile
/// can tighten only the knobs that matter for that API's rate limits.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SubagentProviderConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub max_concurrent: Option<usize>,
    #[serde(default)]
    pub max_depth: Option<u32>,
    #[serde(default)]
    pub launch_concurrency: Option<usize>,
    #[serde(default, alias = "max_total", alias = "admission_limit")]
    pub max_admitted: Option<usize>,
    #[serde(default)]
    pub api_timeout_secs: Option<u64>,
    #[serde(default)]
    pub heartbeat_timeout_secs: Option<u64>,
}

/// `[auto]` table — knobs for the `--model auto` / `/model auto` router.
///
/// `cost_saving` (#1207): when `true`, the auto-mode router prefers the
/// active provider's known fast sibling for ambiguous requests, only using
/// its strong tier when the task clearly benefits from deeper reasoning.
/// Providers without a validated sibling stay on the active model. Default
/// is `false` (balanced — match the existing routing voice).
///
/// `cross_provider` (#4411): Auto routing is scoped to the active provider
/// unless this persisted opt-in is set to `true`. Without it, neither the
/// classifier inventory nor the local heuristic may leave the provider the
/// session is actually configured to use.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AutoConfig {
    #[serde(default)]
    pub cost_saving: Option<bool>,
    /// Persisted opt-in for cross-provider Auto routing (`[auto]
    /// cross_provider = true`). Default `false`: active provider only.
    #[serde(default)]
    pub cross_provider: Option<bool>,
    /// Optional explicit auto-router classifier route (`[auto.router]`).
    #[serde(default)]
    pub router: Option<AutoRouterConfig>,
}

/// Default classifier call timeout for `[auto.router]` (seconds).
pub(crate) const DEFAULT_AUTO_ROUTER_TIMEOUT_SECS: u64 = 4;
/// Upper clamp for a configured classifier timeout: a hung local router must
/// not stall an Auto turn forever.
pub(crate) const MAX_AUTO_ROUTER_TIMEOUT_SECS: u64 = 300;

/// Explicit classifier route for Auto model mode (`[auto.router]`).
///
/// When `provider` + `model` are set, Auto mode's classifier call goes to that
/// route. When unset, Auto stays local and free: it uses the heuristic and
/// makes no classifier call at all.
///
/// There is deliberately no implicit default. Holding a DeepSeek key used to
/// elect `deepseek-v4-flash` as the classifier for every Auto turn, which spent
/// a user's tokens on a route they never chose and privileged one provider over
/// the rest. Electing a network classifier is now something the operator writes
/// down.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoRouterConfig {
    /// Provider id for the classifier route (e.g. `"deepseek"`, `"zai"`).
    #[serde(default)]
    pub provider: Option<String>,
    /// Model id on that provider (e.g. `"deepseek-v4-flash"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Thinking tier for the classifier call (e.g. `"off"`). Defaults to off.
    #[serde(default)]
    pub thinking: Option<String>,
    /// Classifier call timeout in seconds. Defaults to
    /// [`DEFAULT_AUTO_ROUTER_TIMEOUT_SECS`] (4); `0` means "use the default".
    /// Values above [`MAX_AUTO_ROUTER_TIMEOUT_SECS`] (300) are clamped so a
    /// hung local router cannot stall a turn indefinitely.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Router kind (#6525): `"chat"` (default) asks a chat model for JSON;
    /// `"decision"` asks a System One decision model (Jev) a typed Choice
    /// between the active provider's fast and strong tiers. Any other value
    /// leaves the router unconfigured (shown as failing, never guessed).
    #[serde(default)]
    pub kind: Option<String>,
    /// Decision routers only: below this answer confidence (0..=1, default
    /// [`DEFAULT_AUTO_ROUTER_MIN_CONFIDENCE`]) the turn takes the local
    /// fallback instead of the decision.
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Decision routers only: endpoint override for `provider = "typesafe"`
    /// (default `https://api.typesafe.ai/v1`). OpenRouter decision routers use
    /// the configured OpenRouter base URL.
    #[serde(default)]
    pub base_url: Option<String>,
}

/// `[auto.router] kind` (#6525).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutoRouterKind {
    /// A chat model returns `{provider, model, thinking}` JSON.
    Chat,
    /// A System One decision model answers a typed Choice over tiers.
    Decision,
}

impl AutoRouterKind {
    /// `None` (absent) is the chat default; an unknown value is `None` so the
    /// caller reports the router as not configured instead of guessing.
    #[must_use]
    pub(crate) fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).filter(|kind| !kind.is_empty()) {
            None => Some(Self::Chat),
            Some(kind) if kind.eq_ignore_ascii_case("chat") => Some(Self::Chat),
            Some(kind) if kind.eq_ignore_ascii_case("decision") => Some(Self::Decision),
            Some(_) => None,
        }
    }
}

/// Default `[auto.router] min_confidence` for decision routers: TypeSafe's
/// "below 0.5, don't act" band (for two options, the chosen tier's
/// probability must reach 0.75).
pub(crate) const DEFAULT_AUTO_ROUTER_MIN_CONFIDENCE: f64 = 0.5;

fn default_update_check_for_updates() -> bool {
    true
}

fn default_update_check_interval_hours() -> u64 {
    codewhale_release::check::DEFAULT_CHECK_INTERVAL_HOURS
}

/// Startup update-check configuration (`[update]` table in config.toml).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct UpdateConfig {
    /// When false, skip the TUI startup background update check entirely.
    #[serde(default = "default_update_check_for_updates")]
    pub check_for_updates: bool,
    /// Hours between network checks. The answer is cached on disk in between,
    /// so the notice still appears on every launch — only the request is
    /// throttled. `0` disables caching and checks on every launch.
    #[serde(default = "default_update_check_interval_hours")]
    pub check_interval_hours: u64,
    /// Optional GitHub-compatible latest-release JSON endpoint.
    #[serde(default)]
    pub update_uri: Option<String>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            check_for_updates: true,
            check_interval_hours: default_update_check_interval_hours(),
            update_uri: None,
        }
    }
}

impl UpdateConfig {
    #[must_use]
    pub fn update_uri(&self) -> Option<&str> {
        self.update_uri
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

fn default_cloud_facts_channel() -> String {
    "stable".to_string()
}

fn default_cloud_facts_ttl_hours() -> u64 {
    codewhale_cloud_facts::DEFAULT_TTL_SECS / 3600
}

/// Cloud facts overlay (`[cloud_facts]` table in config.toml). Off by default;
/// see `docs/CLOUD_FACTS.md`. Env: `CODEWHALE_CLOUD_FACTS=1|0` overrides
/// `enabled`, `CODEWHALE_DISABLE_CLOUD_FACTS=1` is a hard kill switch.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CloudFactsConfig {
    /// When false (default), no cloud facts are read or fetched; the binary
    /// behaves exactly as it ships.
    #[serde(default)]
    pub enabled: bool,
    /// Channel slug (`stable` / `beta`).
    #[serde(default = "default_cloud_facts_channel")]
    pub channel: String,
    /// Optional endpoint override (`{channel}` placeholder allowed).
    #[serde(default)]
    pub url: Option<String>,
    /// Hours between refreshes of a verified payload.
    #[serde(default = "default_cloud_facts_ttl_hours")]
    pub ttl_hours: u64,
}

impl Default for CloudFactsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: default_cloud_facts_channel(),
            url: None,
            ttl_hours: default_cloud_facts_ttl_hours(),
        }
    }
}

impl CloudFactsConfig {
    /// Runtime settings with env overrides applied.
    #[must_use]
    pub fn settings(&self) -> codewhale_cloud_facts::Settings {
        let channel = self.channel.trim();
        codewhale_cloud_facts::Settings {
            enabled: self.enabled,
            channel: if codewhale_cloud_facts::valid_channel(channel) {
                channel.to_string()
            } else {
                default_cloud_facts_channel()
            },
            url: self
                .url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_string),
            ttl_secs: self.ttl_hours.max(1).saturating_mul(3600),
            ..codewhale_cloud_facts::Settings::default()
        }
        .resolve()
    }
}

/// Which approval option a freshly rendered approval card highlights.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDefaultSelection {
    /// Highlight the deny option, so a reflexive Enter refuses the call.
    #[default]
    Deny,
    /// Highlight "allow once", restoring the pre-v0.9.6 Enter-to-approve flow.
    AllowOnce,
}

/// Approval-card presentation (`[approval]` table in config.toml). Approval
/// *policy* stays the top-level `approval_policy` key; this table only governs
/// how the card is presented once a prompt is already required.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
pub struct ApprovalConfig {
    /// Option highlighted when an approval card first appears (#5293).
    /// Default: `deny`.
    #[serde(default)]
    pub default_selection: ApprovalDefaultSelection,
    /// Seconds an interactive approval card may wait before it resolves
    /// **deny** on its own (#6101). Absent or an explicit `0` waits
    /// indefinitely — the operator is at the terminal, so the card stays
    /// unbounded by default. Values above 86,400 (24h) are clamped with a
    /// warning.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// `transcript.prose_measure` exactly as written in `config.toml`.
///
/// Parsed permissively (any scalar shape) so an invalid value can surface as
/// a targeted `transcript.prose_measure` config error via [`Config::validate`]
/// instead of a generic whole-file parse failure that names no key.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawProseMeasure {
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Text(String),
}

impl RawProseMeasure {
    /// Render the raw file value for config diagnostics.
    fn describe(&self) -> String {
        match self {
            Self::Integer(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::Boolean(value) => value.to_string(),
            Self::Text(value) => format!("'{value}'"),
        }
    }
}

/// Transcript rendering controls (`[transcript]` table in config.toml).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptConfig {
    /// Wrap cap, in terminal columns, for prose cells — user messages,
    /// assistant answers, and reasoning/thinking blocks — in the live
    /// transcript (#5436). Absent or `0` spends the full content width,
    /// matching tool/status cells and the #5322 wide-frame decision. A
    /// positive integer caps prose at that many columns for owners who
    /// want a bounded reading measure on ultrawide displays. Tool, diff,
    /// and status cells never inherit this cap.
    #[serde(default)]
    pub(crate) prose_measure: Option<RawProseMeasure>,
}

impl TranscriptConfig {
    /// Resolve the raw file value into a prose wrap cap.
    ///
    /// `Ok(None)` means full content width. Errors describe the raw value so
    /// [`Config::validate`] can name the offending key and setting.
    fn prose_measure_columns(&self) -> Result<Option<u16>, String> {
        match &self.prose_measure {
            None | Some(RawProseMeasure::Integer(0)) => Ok(None),
            Some(RawProseMeasure::Integer(columns)) if *columns > 0 => {
                Ok(Some((*columns).min(i64::from(u16::MAX)) as u16))
            }
            Some(raw) => Err(format!(
                "expected a positive whole number of columns \
                 (0 or absent = full width), got {}",
                raw.describe()
            )),
        }
    }
}

/// Process-only account auth transform. Shared by Config clones so logout and
/// expiry affect future request resolution without rewriting provider config.
/// Running turns retain their materialized client; immediate revocation of
/// that credential remains the account service's responsibility.
#[derive(Debug, Clone)]
pub(crate) struct AccountModelAccess {
    pub(crate) session_id: String,
    pub(crate) credential: crate::credentials::Credential,
    pub(crate) expires_at: i64,
    pub(crate) profile: Option<String>,
}

/// Resolved CLI configuration, including defaults and environment overrides.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub(crate) account_profile: Option<String>,
    /// Diagnostic clones must never refresh or mutate plugin OAuth credentials.
    #[serde(skip)]
    pub(crate) plugin_oauth_read_only: bool,
    /// Never deserialized from disk or exposed as provider configuration.
    #[serde(skip)]
    pub(crate) account_model_access:
        std::sync::Arc<parking_lot::RwLock<Option<AccountModelAccess>>>,
    #[serde(skip)]
    pub(crate) required_account_model_owner: Option<String>,
    /// Persisted exact-route declarations, separate from provider credentials.
    #[serde(
        default,
        deserialize_with = "codewhale_config::catalog::configured::deserialize_configured_models"
    )]
    pub custom_models: Option<Vec<codewhale_config::catalog::configured::ConfiguredModel>>,
    /// Single-token inputs that cancel the active turn before dispatch.
    #[serde(default)]
    pub stop_words: Option<Vec<String>>,
    pub provider: Option<String>,
    // No top-level `api_key` / `base_url`: `parse_config_file` moves the
    // legacy keys into `[providers.<name>]` before this struct is built
    // (#6394).
    /// Optional extra HTTP headers sent to model API requests.
    #[serde(alias = "httpHeaders")]
    pub http_headers: Option<HashMap<String, String>>,
    /// Optional user-facing tab/window title shown as `[title] …` in front of
    /// the terminal window title (the `Codewhale` / `reasoning…` / `done.`
    /// states). This is the default for every session in this config scope;
    /// the `/title` command overrides it per session, and `/config title …
    /// --save` persists a new default here. Multi-window setups can point each
    /// workspace at its own `--config` file (or profile) so alt-tabbed
    /// sessions are identifiable at a glance.
    pub title: Option<String>,
    #[serde(alias = "defaultTextModel")]
    pub default_text_model: Option<String>,
    /// Read-only compatibility for the dispatcher's historical root `model`.
    /// New durable choices are written to the provider model slot.
    #[serde(rename = "model", skip_serializing)]
    pub(crate) legacy_model: Option<String>,
    #[serde(alias = "authMode")]
    pub auth_mode: Option<String>,
    /// DeepSeek reasoning-effort tier: `"off" | "low" | "medium" | "high" | "max"`.
    /// Defaults to `"max"` at runtime if unset.
    pub reasoning_effort: Option<String>,
    /// True only when compatibility migration inferred `reasoning_effort`
    /// from a retiring DeepSeek alias. This distinguishes that inferred value
    /// from a user override during an in-session provider switch.
    #[serde(skip)]
    pub(crate) reasoning_effort_inferred_from_legacy_alias: bool,
    /// Runtime-only receipt that a fresh launch adopted the selected Fleet's
    /// operator provider/model pair. App initialization uses it to prevent
    /// generic remembered `/model` preferences from replacing that selected
    /// Fleet route later in the same launch.
    #[serde(skip)]
    pub(crate) fleet_operator_route_applied: bool,
    /// Runtime-only receipt that the selected Fleet also supplied a reasoning
    /// tier. Kept separate because an operator with no tier deliberately
    /// inherits the ordinary session/settings reasoning preference.
    #[serde(skip)]
    pub(crate) fleet_operator_reasoning_applied: bool,
    /// Original first-party DeepSeek alias captured before model normalization.
    /// This runtime-only receipt lets diagnostics explain why the resolved
    /// model changed without persisting compatibility state back to config.
    #[serde(skip)]
    pub(crate) migrated_deepseek_model_alias: Option<String>,
    /// Runtime-only receipt that the released `ollama` + exact
    /// `https://ollama.com/v1` tuple was upgraded to `ollama-cloud` in memory.
    ///
    /// This survives route-scoped config clones so old provider-table and
    /// secret-slot reads remain available to that exact migrated route. It is
    /// never serialized and is never set for an explicit `ollama-cloud`
    /// selection.
    #[serde(skip)]
    pub(crate) migrated_legacy_ollama_cloud_route: bool,
    /// Runtime-only isolation boundary for account-owned managed Chat.
    ///
    /// This is never user-configurable or serialized. The Runtime Chat relay
    /// sets it on its private config clone so the Engine suppresses all host
    /// workspace metadata and constructs no model-visible MCP, sub-agent, or
    /// native tool surface for those turns.
    #[serde(skip)]
    pub(crate) runtime_chat_isolated: bool,
    /// Runtime-only marker for an independent RuntimeThreadManager store.
    ///
    /// Those threads are not projected into the interactive TUI's attached
    /// CWC run, so their provider requests do not participate in that run's
    /// exclusive Chat ownership gate. RuntimeThreadManager sets this marker on
    /// its private config clone; it is never user-configurable or serialized.
    #[serde(skip)]
    pub(crate) runtime_thread_inference_unrelated: bool,
    /// Native tool catalog controls. This table controls built-in
    /// tool loading policy.
    #[serde(default)]
    pub tools: Option<ToolsConfig>,
    pub skills_dir: Option<String>,
    pub mcp_config_path: Option<String>,
    pub mcp_oauth_callback_port: Option<u16>,
    pub mcp_oauth_callback_url: Option<String>,
    pub notes_path: Option<String>,
    pub memory_path: Option<String>,
    /// When true, set `tool_choice: "required"` and opt compatible function
    /// schemas into DeepSeek beta strict mode. Schemas with root alternatives
    /// stay non-strict to avoid changing optional/one-of tool semantics.
    pub strict_tool_mode: Option<bool>,
    /// Additional user-owned system-prompt sources concatenated in declared
    /// order (#454). Paths are expanded via `expand_path` so `~` and env vars
    /// work. Project-scope config is not allowed to set this field; the TUI
    /// project overlay ignores `instructions` so a cloned repo cannot choose
    /// arbitrary local files to place into the prompt. Each configured file is
    /// loaded, capped at 100 KiB, and skipped (with a warning) on read errors so
    /// a missing optional file doesn't fail the launch.
    pub instructions: Option<Vec<String>>,
    pub allow_shell: Option<bool>,
    /// Opt-in ghost-text follow-up prompt suggestion after each completed turn.
    /// Default: false — the user must explicitly set this to true to enable.
    pub prompt_suggestion: Option<bool>,
    #[serde(alias = "approvalPolicy")]
    pub approval_policy: Option<String>,
    #[serde(alias = "sandboxMode")]
    pub sandbox_mode: Option<String>,
    /// Whether a workspace-write sandbox also grants the shell outbound
    /// network access. Defaults to `false`: editing the workspace is not a
    /// reason to be able to reach the internet. Network comes from an explicit
    /// opt-in here, from a `danger-full-access` posture, or from the
    /// post-denial elevation prompt. `yolo`/`Bypass` is unaffected — it
    /// resolves to `danger-full-access`, which is unsandboxed by definition.
    #[serde(alias = "sandboxNetworkAccess")]
    pub sandbox_network_access: Option<bool>,
    /// Foreign-agent instruction formats to import as project instructions.
    /// Empty by default: a `CLAUDE.md`, `.cursorrules`, or
    /// `.github/copilot-instructions.md` written as law for another tool is
    /// not silently treated as law for this one. Accepts `claude`, `cursor`,
    /// `cline`, `windsurf`, `gemini`, `copilot`, `muse`, or `all`.
    #[serde(default, alias = "projectInstructionImports")]
    pub project_instruction_imports: Vec<String>,
    /// `telemetry` as written to the config file, before environment and
    /// default resolution. Kept so doctor and config displays can state the
    /// *resolved* consent with its source (default | env | config) instead of
    /// reading "unset" while batches ship (#5441).
    #[serde(default)]
    pub telemetry: Option<bool>,
    #[serde(default, alias = "fallbackProviders")]
    pub fallback_providers: Vec<codewhale_config::ProviderKind>,
    pub yolo: Option<bool>,
    pub verbosity: Option<String>,
    /// External sandbox backend: `"none"` or `"opensandbox"`.
    /// When set, exec_shell routes commands through the backend's HTTP API
    /// instead of spawning a local process.
    #[serde(alias = "sandboxBackend")]
    pub sandbox_backend: Option<String>,
    /// Base URL for the external sandbox backend (default: `"http://localhost:8080"`).
    #[serde(alias = "sandboxUrl")]
    pub sandbox_url: Option<String>,
    /// Optional API key for the external sandbox backend (sent as Bearer token).
    #[serde(alias = "sandboxApiKey")]
    pub sandbox_api_key: Option<String>,
    /// When true and bubblewrap actually works on this Linux host, route
    /// sandboxed exec_shell commands through it (#2184).
    /// Defaults to true — an unset key means sandboxed commands run under
    /// bwrap whenever `/usr/bin/bwrap` is installed and can create its
    /// namespaces. An explicit `prefer_bwrap = false` opts out and leaves
    /// Linux commands unwrapped (the posture then reports policy-only).
    /// Requires the `bubblewrap` package to be installed separately — we do
    /// NOT vendor bwrap.
    #[serde(alias = "preferBwrap")]
    pub prefer_bwrap: Option<bool>,
    /// Additional host paths to bind read-only inside the bubblewrap sandbox
    /// (Linux, `prefer_bwrap = true`, #5410). The default root bind already
    /// exposes the host filesystem read-only; these cover setups where a
    /// policy or future default narrows it. Non-existent paths are skipped.
    #[serde(default, alias = "bwrapRoRoots")]
    pub bwrap_ro_roots: Vec<std::path::PathBuf>,
    /// Host device nodes to bind read-write inside the bubblewrap sandbox
    /// (#5410), e.g. `/dev/null` for shell redirection against the host node.
    /// Only character/block devices are honored — never directories — so
    /// this key cannot become a writable-root escape hatch. Non-existent or
    /// non-device paths are skipped. The default private `/dev` already
    /// provides fresh device nodes, so most users never need this.
    #[serde(default, alias = "bwrapDevRoots")]
    pub bwrap_dev_roots: Vec<std::path::PathBuf>,
    /// Opt-in sandbox read deny-list (S1, #5568). Listed subpaths are
    /// unreadable inside sandboxed shell commands even though the sandbox
    /// otherwise grants full-disk read: Seatbelt appends last-match-wins
    /// deny rules; bubblewrap masks each existing path with an empty tmpfs
    /// (directories) or a /dev/null bind (files). A leading `~` expands to
    /// the user's home. Empty (the default) preserves current behavior.
    /// Example: `sandbox_denied_read_paths = ["~/.ssh", "~/.aws"]`.
    #[serde(default, alias = "sandboxDeniedReadPaths")]
    pub sandbox_denied_read_paths: Vec<std::path::PathBuf>,
    /// Apply the built-in credential-store read deny-list (S1). Defaults to
    /// `true`: `~/.ssh`, cloud credential dirs, keychains, browser profiles,
    /// `.env` files, and Codewhale's own secret stores are unreadable by the
    /// file-reading tools and by sandboxed shell commands. Set to `false` to
    /// restore the pre-S1 full-disk-read behavior. See
    /// `sandbox::read_guard` for the exact list and its honest limits — it is
    /// defense-in-depth, not a security boundary.
    #[serde(default, alias = "sandboxReadDenylistDefaults")]
    pub sandbox_read_denylist_defaults: Option<bool>,
    /// Subtract paths from the *built-in* deny-list defaults when a project
    /// genuinely needs one of them. Has no effect on
    /// `sandbox_denied_read_paths`: an explicit deny always wins.
    #[serde(default, alias = "sandboxReadDenylistExempt")]
    pub sandbox_read_denylist_exempt: Vec<std::path::PathBuf>,
    #[serde(alias = "managedConfigPath")]
    pub managed_config_path: Option<String>,
    #[serde(alias = "requirementsPath")]
    pub requirements_path: Option<String>,
    #[serde(alias = "maxSubagents")]
    pub max_subagents: Option<usize>,
    pub retry: Option<RetryConfig>,
    pub stream: Option<StreamConfig>,
    pub features: Option<FeaturesToml>,
    /// Experimental TypeScript extension host settings.
    #[serde(default)]
    pub extension_host: Option<ExtensionHostConfig>,
    /// Per-plugin settings (`[plugins."<name>".config]`). User config only.
    #[serde(default)]
    pub plugins: Option<BTreeMap<String, PluginSettings>>,

    /// Deterministic user-level auto-review policy for tool calls. The engine
    /// applies these rules after built-in safety floors, so config cannot
    /// bypass publish/destructive-background holds.
    #[serde(default)]
    pub auto_review: Option<AutoReviewConfig>,

    /// TUI configuration (alternate screen, etc.)
    pub tui: Option<TuiConfig>,

    /// Transcript rendering controls (`[transcript]` table). Absent means
    /// prose uses the full content width (#5436).
    #[serde(default)]
    pub transcript: Option<TranscriptConfig>,

    /// Lifecycle hooks configuration
    #[serde(default)]
    pub hooks: Option<HooksConfig>,

    /// Lifecycle event outbox (`[lifecycle_outbox]`). Opt-in: an unset or
    /// empty `path` disables the feature and leaves behavior unchanged.
    /// Fires for interactive TUI sessions and headless `codewhale exec` runs.
    #[serde(default)]
    pub lifecycle_outbox: Option<codewhale_config::LifecycleOutboxToml>,

    /// Per-session control socket (`[control_socket]`). Opt-in: an absent
    /// table or `enabled = false` (the default) leaves the feature off.
    /// When enabled, the interactive TUI binds a unix socket per running
    /// session (see `crate::tui::control_socket`).
    #[serde(default)]
    pub control_socket: Option<codewhale_config::ControlSocketToml>,

    /// Provider-specific credentials and defaults shared with the `codewhale` facade.
    #[serde(default)]
    pub providers: Option<ProvidersConfig>,

    /// Desktop notification settings (OSC 9 / BEL on long turn completion).
    #[serde(default)]
    pub notifications: Option<NotificationsConfig>,

    /// Approval-card presentation (`[approval]`). Absent means deny-by-default
    /// preselection.
    #[serde(default)]
    pub approval: Option<ApprovalConfig>,

    /// Per-domain network policy (#135). When absent, network tools fall back
    /// to a permissive default that mirrors pre-v0.7.0 behavior.
    #[serde(default)]
    pub network: Option<NetworkPolicyToml>,

    /// Verifier-preview behavior (#2093). When absent, automatic verifier
    /// preview stays off and verifier verdicts use the hunt policy.
    #[serde(default)]
    pub verifier: Option<codewhale_config::VerifierConfigToml>,

    /// Background advisor watcher (#3982). When absent, the advisor is off
    /// by default. Enable with `[advisor] enabled = true` or `/advisor on`.
    #[serde(default)]
    pub advisor: Option<codewhale_config::AdvisorConfigToml>,

    /// Community skill installer settings (#140). When absent, installer
    /// commands fall back to the bundled defaults
    /// ([`crate::skills::install::DEFAULT_REGISTRY_URL`] +
    /// [`crate::skills::install::DEFAULT_MAX_SIZE_BYTES`]).
    #[serde(default)]
    pub skills: Option<SkillsConfig>,

    /// Workspace side-git snapshots (#137). Defaults to enabled with 7-day
    /// retention when the table is absent.
    #[serde(default)]
    pub snapshots: Option<SnapshotsConfig>,

    /// Web search provider configuration. When absent, defaults to keyless
    /// Firecrawl. Other API services require credentials; SearXNG requires a
    /// trusted `base_url`.
    #[serde(default)]
    pub search: Option<SearchConfig>,

    /// Persistent-goal loop controls (#5052). When absent, goals have no
    /// continuation ceiling. Users can opt into one with
    /// `[goal] max_continuations`.
    #[serde(default)]
    pub goal: Option<GoalConfig>,

    /// Reasoning-only recovery controls. When absent, the engine uses the
    /// built-in default (2 retries, no custom message). Configure with
    /// `[reasoning_only] max_reprompts` and/or `[reasoning_only] reprompt_message`.
    #[serde(default)]
    pub reasoning_only: Option<ReasoningOnlyConfig>,

    /// User-level memory (#489). Default behaviour is **opt-in**:
    /// loading + injection happens only when `[memory] enabled = true` or
    /// `DEEPSEEK_MEMORY=on` is set. The surviving store is the native
    /// Markdown + SQLite FTS5 system (`memory/global/MEMORY.md`).
    #[serde(default)]
    pub memory: Option<MemoryConfig>,

    /// Xiaomi MiMo speech/TTS defaults.
    #[serde(default)]
    pub speech: Option<SpeechConfig>,

    /// Tunables for `--model auto` (#1207). When absent, the auto router
    /// keeps its existing balanced behaviour.
    #[serde(default)]
    pub auto: Option<AutoConfig>,

    /// Optional 1-8 hotbar slot bindings (#2064). When absent, hotbar UI and
    /// dispatch layers use the built-in defaults from `codewhale_config`.
    #[serde(default)]
    pub hotbar: Option<Vec<codewhale_config::HotbarBindingToml>>,

    /// Startup update-check behavior. When absent, the TUI keeps the default
    /// fire-and-forget latest-release check.
    #[serde(default)]
    pub update: Option<UpdateConfig>,

    /// Cloud facts overlay (`[cloud_facts]`). Absent/false = off.
    #[serde(default)]
    pub cloud_facts: Option<CloudFactsConfig>,

    /// Post-edit LSP diagnostics injection (#136). When absent, the engine
    /// applies the defaults documented in [`LspConfigToml`].
    #[serde(default)]
    pub lsp: Option<LspConfigToml>,

    /// Context configuration (project context pack; legacy seam keys are
    /// parsed but ignored since the 2026-07-23 removal).
    #[serde(default)]
    pub context: ContextConfig,

    /// Compaction summarizer tuning (#5956). Absent keeps the built-in
    /// summarizer prompt and the 20 000-token verbatim retention budget.
    #[serde(default)]
    pub compaction: Option<CompactionSettings>,

    /// Agent Fleet trust/security/role/exec config.
    #[serde(default)]
    pub fleet: Option<codewhale_config::FleetConfigToml>,

    /// Workflow automatic-launch, approval, isolation, and activity
    /// persistence knobs (#4128). When absent, consumers use
    /// [`codewhale_config::WorkflowConfigToml::default`] via
    /// [`Self::workflow_config`].
    #[serde(default)]
    pub workflow: Option<codewhale_config::WorkflowConfigToml>,

    /// Sub-agent model overrides.
    #[serde(default)]
    pub subagents: Option<SubagentsConfig>,

    /// Runtime API server tuning (`codewhale serve --http`). Currently only
    /// hosts the CORS allow-list extension (whalescale#255 / #561). When the
    /// table is absent, the daemon ships with localhost:3000 / localhost:1420
    /// / tauri://localhost as the only allowed dev origins.
    #[serde(default)]
    pub runtime_api: Option<RuntimeApiConfig>,

    /// Workshop / large-tool-output routing (#548). When absent, the global
    /// default threshold of 4 096 tokens applies and routing is active.
    #[serde(default)]
    pub workshop: Option<crate::tools::large_output_router::WorkshopConfig>,

    /// Vision model configuration for the `image_analyze` tool.
    #[serde(default)]
    pub vision_model: Option<VisionModelConfig>,

    /// Model-bound credential redaction policy (`[redaction]`). When absent,
    /// masking is enabled — the shipped security default. A `"disabled"`
    /// request only takes effect after a TUI restart and an explicit
    /// confirmation on the startup gate; see
    /// [`codewhale_config::redaction`].
    #[serde(default)]
    pub redaction: Option<codewhale_config::redaction::RedactionToml>,

    /// Local provenance of the config actually loaded, including --config and
    /// CODEWHALE_CONFIG_PATH. Consent must not borrow another config's receipt.
    #[serde(skip)]
    pub loaded_config_path: Option<PathBuf>,

    /// Runtime-only receipt that a higher-precedence layer supplied `[network]`
    /// — a managed overlay. The resolved policy is then not the user
    /// document's to replace: a mid-session re-read of that document must not
    /// widen what the higher layer set.
    #[serde(skip)]
    pub(crate) network_layer_is_managed: bool,

    /// A resolved startup snapshot never reads remembered route choices again.
    /// False means an explicit config/profile owns the route instead.
    #[serde(skip)]
    pub(crate) remembered_selection_scope: Option<bool>,

    /// An explicit model applied by the environment/CLI layer outranks project
    /// defaults, including when it equals the previously saved model.
    #[serde(skip)]
    pub(crate) environment_model_applied: bool,

    /// Atomic migration receipt: provider/model selections now belong to this
    /// config document. Settings route fields are legacy inputs only.
    pub(crate) route_preferences_version: Option<u32>,

    /// Sibling `permissions.toml` ask-rules compiled for runtime checks.
    ///
    /// This is deliberately not part of `config.toml`; it is loaded from the
    /// companion permissions file after profile/env/managed config resolution.
    #[serde(skip)]
    pub exec_policy_engine: ExecPolicyEngine,

    /// Receipt describing what the environment layer did to this config's
    /// effective base URL.
    ///
    /// This provenance cannot be reconstructed from the merged provider table:
    /// environment overrides are written into the same `base_url` field as
    /// file-owned routes. Keep the receipt so a saved provider/root key (or a
    /// configured `api_key_env`) cannot silently follow an env-selected custom
    /// host, and so a cross-provider child cannot borrow an ambient generic
    /// host that was never addressed to it.
    #[serde(skip)]
    pub(crate) base_url_env_receipt: BaseUrlEnvReceipt,

    /// What loading moved in memory from legacy top-level `base_url` /
    /// `api_key` keys (#6394). Reported by doctor; never rewrites the file.
    #[serde(skip)]
    pub(crate) legacy_root: codewhale_config::legacy_root::LegacyRootMigration,

    /// Bound at parse time, never reconstructed from later diagnostic notes.
    #[cfg(not(test))]
    #[serde(skip)]
    legacy_root_custom_generation: Option<crate::route_receipt::CredentialGeneration>,
    // Test fixtures construct Config through public-field struct updates. The
    // production receipt remains private; both builds run identical admission.
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) legacy_root_custom_generation: Option<crate::route_receipt::CredentialGeneration>,

    /// Mini-window (pinned, always-on-top) mode layout preferences
    /// (`[mini_window]` in config.toml). When the host terminal window is
    /// pinned into its small always-on-top form, the TUI switches to a
    /// compact layout that keeps only the elements listed here.
    #[serde(default)]
    pub mini_window: Option<MiniWindowConfig>,
}

/// Layout preferences for the pinned (always-on-top) mini-window mode.
///
/// When the host window is shrunk into its mini form (the right-click
/// "弹出置顶小窗" action), the TUI hides the shell chrome and keeps only the
/// message stream plus whatever the user opted to keep here.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MiniWindowConfig {
    /// Keep the composer (message input box) visible in mini mode.
    /// Default: true — the mini window stays interactive.
    #[serde(default = "mini_default_keep_input")]
    pub keep_input: bool,
    /// Keep the Tasks + To-do strip visible in mini mode. Default: false.
    #[serde(default)]
    pub keep_todo: bool,
    /// Keep the side work rail (work surface side panel) visible in mini
    /// mode. Default: false.
    #[serde(default)]
    pub keep_sidebar: bool,
    /// Keep the bottom phase strip visible in mini mode. Default: false.
    #[serde(default)]
    pub keep_footer: bool,
    /// Keep the top status bar (route/mode/effort/permission header) visible
    /// in mini mode. Default: false.
    #[serde(default)]
    pub keep_header: bool,
}

impl Default for MiniWindowConfig {
    fn default() -> Self {
        Self {
            keep_input: true,
            keep_todo: false,
            keep_sidebar: false,
            keep_footer: false,
            keep_header: false,
        }
    }
}

fn mini_default_keep_input() -> bool {
    true
}

/// What the environment layer decided about the generic
/// `CODEWHALE_BASE_URL` / `DEEPSEEK_BASE_URL` override.
///
/// The distinction that matters is between "no receipt" and "a receipt saying
/// nobody owns it". They are not the same state and must not collapse: a
/// missing receipt is a config that never passed through the environment
/// layer, while [`BaseUrlEnvReceipt::NoOwner`] is a positive statement that a
/// higher-precedence layer took the endpoint away from the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum BaseUrlEnvReceipt {
    /// The environment layer never ran for this config — directly constructed
    /// configs, embedded profiles, and unit-test fixtures. These keep the
    /// established global fallback: the generic override applies to whatever
    /// route is asked about.
    #[default]
    Unrecorded,
    /// The environment layer ran and no route owns the generic override —
    /// either it was absent, or a higher-precedence file layer (a managed
    /// overlay) supplied/reselected the effective route's endpoint. No route,
    /// active or pinned, may borrow the ambient generic host.
    NoOwner,
    /// The environment layer ran and addressed the override to exactly this
    /// `(provider, identity)`. Only that route resolves it; every other route
    /// falls through to its own default.
    Route(ProviderKind, String),
}

impl BaseUrlEnvReceipt {
    /// Whether `(provider, identity)` is the route this receipt names.
    fn owns(&self, provider: ProviderKind, identity: &str) -> bool {
        match self {
            Self::Route(owner, owner_identity) => *owner == provider && owner_identity == identity,
            Self::Unrecorded | Self::NoOwner => false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoReviewConfig {
    #[serde(default)]
    pub allow: Vec<AutoReviewRuleConfig>,
    #[serde(default)]
    pub block: Vec<AutoReviewRuleConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoReviewRuleConfig {
    pub id: Option<String>,
    #[serde(default, alias = "toolName", alias = "tool_name")]
    pub tool: Option<String>,
    #[serde(default, alias = "actionKind", alias = "action_kind")]
    pub action_kind: Option<String>,
    #[serde(default, alias = "textContains")]
    pub(crate) text_contains: Option<String>,
    pub reason: Option<String>,
}

impl AutoReviewConfig {
    fn to_runtime_policy(&self) -> crate::core::authority::auto_review::AutoReviewPolicy {
        crate::core::authority::auto_review::AutoReviewPolicy {
            allow_rules: self
                .allow
                .iter()
                .enumerate()
                .map(|(index, rule)| {
                    rule.to_runtime_rule(
                        index,
                        crate::core::authority::auto_review::AutoReviewAction::Allow,
                    )
                })
                .collect(),
            block_rules: self
                .block
                .iter()
                .enumerate()
                .map(|(index, rule)| {
                    rule.to_runtime_rule(
                        index,
                        crate::core::authority::auto_review::AutoReviewAction::Block,
                    )
                })
                .collect(),
        }
    }

    fn validate(&self) -> Result<()> {
        validate_auto_review_rules("allow", &self.allow)?;
        validate_auto_review_rules("block", &self.block)?;
        Ok(())
    }
}

impl AutoReviewRuleConfig {
    fn to_runtime_rule(
        &self,
        index: usize,
        action: crate::core::authority::auto_review::AutoReviewAction,
    ) -> crate::core::authority::auto_review::AutoReviewRule {
        let id_prefix = match action {
            crate::core::authority::auto_review::AutoReviewAction::Allow => "allow",
            crate::core::authority::auto_review::AutoReviewAction::Block => "block",
            crate::core::authority::auto_review::AutoReviewAction::AskUser => "ask",
        };
        let id = self
            .id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("config-{id_prefix}-{index}"));
        let reason = self
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("configured auto-review {id_prefix} rule"));
        let mut rule = match action {
            crate::core::authority::auto_review::AutoReviewAction::Allow => {
                crate::core::authority::auto_review::AutoReviewRule::allow(id, reason)
            }
            crate::core::authority::auto_review::AutoReviewAction::Block => {
                crate::core::authority::auto_review::AutoReviewRule::block(id, reason)
            }
            crate::core::authority::auto_review::AutoReviewAction::AskUser => {
                crate::core::authority::auto_review::AutoReviewRule::block(id, reason)
            }
        };

        if let Some(tool) = self
            .tool
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            rule = rule.tool_name(tool.to_string());
        }
        if let Some(action_kind) = self
            .action_kind
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(parse_auto_review_action_kind)
        {
            rule = rule.action_kind(action_kind);
        }
        rule
    }

    fn has_matcher(&self) -> bool {
        self.tool
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || self
                .action_kind
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }
}

fn validate_auto_review_rules(kind: &str, rules: &[AutoReviewRuleConfig]) -> Result<()> {
    for (index, rule) in rules.iter().enumerate() {
        if rule
            .text_contains
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            anyhow::bail!(
                "Invalid auto_review.{kind}[{index}].text_contains: user-intent matching was retired; scope the rule with tool and/or action_kind."
            );
        }
        if !rule.has_matcher() {
            anyhow::bail!(
                "Invalid auto_review.{kind}[{index}]: set at least one of tool or action_kind."
            );
        }
        if let Some(action_kind) = rule.action_kind.as_deref() {
            let normalized = action_kind.trim().to_ascii_lowercase().replace('-', "_");
            if parse_auto_review_action_kind(&normalized).is_none() {
                anyhow::bail!(
                    "Invalid auto_review.{kind}[{index}].action_kind '{action_kind}': expected read, write, shell, external, publish, or destructive."
                );
            }
            if kind == "allow"
                && !matches!(
                    normalized.as_str(),
                    "read" | "write" | "shell" | "external" | "publish" | "destructive"
                )
            {
                anyhow::bail!(
                    "Invalid auto_review.allow[{index}].action_kind '{action_kind}': this retired narrow kind cannot safely widen to a v0.9.8 decision class; replace it with an exact tool rule or a current action_kind."
                );
            }
        }
    }
    Ok(())
}

fn parse_auto_review_action_kind(
    raw: &str,
) -> Option<crate::core::authority::auto_review::ToolActionKind> {
    match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "read" | "mcp_read" => Some(crate::core::authority::auto_review::ToolActionKind::Read),
        "write" => Some(crate::core::authority::auto_review::ToolActionKind::Write),
        "shell" => Some(crate::core::authority::auto_review::ToolActionKind::Shell),
        "external" | "network" | "git" | "mcp_action" | "browser" | "unknown" => {
            Some(crate::core::authority::auto_review::ToolActionKind::External)
        }
        "publish" => Some(crate::core::authority::auto_review::ToolActionKind::Publish),
        "destructive" | "secret" => {
            Some(crate::core::authority::auto_review::ToolActionKind::Destructive)
        }
        _ => None,
    }
}

/// How a user wants to disable a tool or supply a script / command tool.
/// Only `Disabled` may target a built-in; see `ToolsConfig::overrides`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOverride {
    /// Run a local script file. The script receives the tool's JSON input
    /// on stdin and must return a JSON `ToolResult` on stdout.
    Script {
        /// Path to the script (absolute, or relative to `~/.codewhale/tools/`).
        path: String,
        /// Optional static arguments prepended before the tool's JSON input.
        #[serde(default)]
        args: Option<Vec<String>>,
    },
    /// Run an external command. The command receives the tool's JSON input
    /// on stdin and must return a JSON `ToolResult` on stdout.
    Command {
        /// The command to run (binary name or absolute path).
        command: String,
        /// Optional static arguments prepended before the tool's JSON input.
        #[serde(default)]
        args: Option<Vec<String>>,
    },
    /// Completely disable a tool, built-in or not. The tool will not appear in
    /// the model-visible catalog and cannot be called.
    Disabled,
}

/// Vision model configuration for the `image_analyze` tool.
/// Uses an OpenAI-compatible vision model API.
#[derive(Debug, Clone, Deserialize)]
pub struct VisionModelConfig {
    /// Model identifier (e.g., "gemini-3.1-flash-lite-preview").
    pub model: String,
    /// API key for the vision model. Inherits from main config if not specified.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL for the vision model API. Defaults to OpenAI.
    #[serde(default)]
    pub base_url: Option<String>,
}

/// `[runtime_api]` table — knobs for the local HTTP/SSE daemon.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuntimeApiConfig {
    /// Additional CORS origins to allow on top of the built-in defaults
    /// (`http://localhost:{3000,1420}`, `http://127.0.0.1:{3000,1420}`,
    /// `tauri://localhost`). Useful when developing a UI against a non-default
    /// dev server port (e.g. Vite's default `:5173`).
    ///
    /// Resolution order (highest priority first): `--cors-origin` CLI flag,
    /// `DEEPSEEK_CORS_ORIGINS` env var (comma-separated), this field. Whalescale#255 / #561.
    #[serde(default)]
    pub cors_origins: Option<Vec<String>>,
}

/// `[skills]` table — knobs for the community-skill installer.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SkillsConfig {
    /// Curated registry index. `/skill install <name>` looks up the spec here.
    /// Defaults to [`crate::skills::install::DEFAULT_REGISTRY_URL`].
    #[serde(default)]
    pub registry_url: Option<String>,
    /// Per-skill maximum *uncompressed* size in bytes. Tarballs that exceed
    /// this limit are rejected during validation. Defaults to 5 MiB.
    #[serde(default)]
    pub max_install_size_bytes: Option<u64>,
    /// When true, skill discovery scans only Codewhale-owned skill roots
    /// (plus any explicit `skills_dir`) instead of importing compatible
    /// directories from other AI tools such as Claude, OpenCode, or Cursor.
    #[serde(default, alias = "scanCodewhaleOnly")]
    pub scan_codewhale_only: Option<bool>,
    /// Opt in to discovery from `<workspace>/skills` after workspace trust.
    /// Otherwise the flat root is visible only to compatible audit.
    #[serde(default)]
    pub flat_workspace_root: Option<bool>,
}

impl SkillsConfig {
    #[must_use]
    pub fn flat_workspace_root(&self) -> bool {
        self.flat_workspace_root.unwrap_or(false)
    }

    /// Resolve whether session-time discovery should ignore cross-tool skill
    /// directories. Defaults to the compatibility-preserving broad scan.
    #[must_use]
    pub fn scan_codewhale_only(&self) -> bool {
        self.scan_codewhale_only.unwrap_or(false)
    }
}

/// `[network]` table — mirrors `codewhale_config::NetworkPolicyToml` so the live
/// TUI runtime can construct a [`crate::network_policy::NetworkPolicy`]
/// without reaching into the workspace config crate. See `config.example.toml`
/// for documentation.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkPolicyToml {
    /// Decision for hosts that are not in `allow` or `deny`. One of
    /// `"allow" | "deny" | "prompt"`. Defaults to `"prompt"`.
    #[serde(default = "default_network_decision")]
    pub default: String,
    /// Hosts that are always allowed. Subdomain rules: a leading dot
    /// (`.example.com`) matches subdomains but not the apex.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Hosts that are always denied. Deny entries win over allow entries.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Hostnames whose DNS may resolve to fake-IP/private proxy ranges in an
    /// explicitly trusted proxy setup. Literal IP URLs remain blocked.
    #[serde(default)]
    pub proxy: Vec<String>,
    /// Explicit fake-IP placeholder CIDRs for those proxy hosts. Only subnets
    /// within `198.18.0.0/15` are accepted by the runtime SSRF guard.
    #[serde(default)]
    pub proxy_fake_ip_cidrs: Vec<String>,
    /// Whether to record one audit-log line per outbound network call.
    #[serde(default = "default_network_audit")]
    pub audit: bool,
}

fn default_network_decision() -> String {
    "prompt".to_string()
}

fn default_network_audit() -> bool {
    true
}

impl Default for NetworkPolicyToml {
    fn default() -> Self {
        Self {
            default: default_network_decision(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: default_network_audit(),
        }
    }
}

impl NetworkPolicyToml {
    /// Build a runtime [`crate::network_policy::NetworkPolicy`] from the
    /// on-disk schema.
    #[must_use]
    pub fn into_runtime(self) -> crate::network_policy::NetworkPolicy {
        crate::network_policy::NetworkPolicy {
            default: crate::network_policy::Decision::parse(&self.default).into(),
            allow: self.allow,
            deny: self.deny,
            proxy: self.proxy,
            proxy_fake_ip_cidrs: self.proxy_fake_ip_cidrs,
            audit: self.audit,
        }
    }
}

/// Re-read the `[network]` table from a configuration document.
///
/// Deliberately narrower than [`Config::load`]: this runs on the tool-context
/// build path, where re-applying the environment, managed, and credential
/// layers would be both wasteful and wrong. Only the document's own table comes
/// back — the same table `/network allow <host>` edits.
///
/// `None` means there is nothing usable to adopt: the document is missing,
/// unreadable, unparseable, or carries no `[network]` table. Callers keep the
/// policy they already hold, which is the conservative direction for an
/// allow/deny gate.
///
/// Known limitation: `[profiles.<name>.network]` is not consulted. `/network`
/// does not write it either, so a live session and the command agree on this
/// base table.
#[must_use]
pub fn network_policy_from_document(path: &Path) -> Option<crate::network_policy::NetworkPolicy> {
    #[derive(Deserialize)]
    struct NetworkTable {
        network: Option<NetworkPolicyToml>,
    }

    let contents = fs::read_to_string(path).ok()?;
    let parsed: NetworkTable = toml::from_str(&contents).ok()?;
    parsed.network.map(NetworkPolicyToml::into_runtime)
}

/// `[lsp]` table — mirrors [`crate::lsp::LspConfig`]. Documented in
/// `config.example.toml`. When omitted, defaults from `LspConfig::default()`
/// apply (enabled, 5 s poll, 20 diagnostics/file, errors only, no overrides).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LspConfigToml {
    /// Master switch. Defaults to `true`.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// How long to wait for the LSP server to publish diagnostics after a
    /// `didOpen`/`didChange`. Defaults to 5000 ms.
    #[serde(default)]
    pub poll_after_edit_ms: Option<u64>,
    /// Cap on diagnostics surfaced per file. Defaults to 20.
    #[serde(default)]
    pub max_diagnostics_per_file: Option<usize>,
    /// Whether to surface warnings in addition to errors. Defaults to `false`.
    #[serde(default)]
    pub include_warnings: Option<bool>,
    /// Optional override for the `Language -> [cmd, ...args]` table. Keys
    /// are language slugs (`"rust"`, `"go"`, etc.).
    #[serde(default)]
    pub servers: Option<HashMap<String, Vec<String>>>,
    /// User-defined LSP servers for file extensions not in the built-in
    /// registry. Keyed by extension (e.g. `"php"`, `"rb"`).
    #[serde(default)]
    pub custom: Option<HashMap<String, crate::lsp::CustomLspDef>>,
}

impl LspConfigToml {
    /// Build a runtime [`crate::lsp::LspConfig`] from the on-disk schema,
    /// falling back to defaults for any unset fields.
    #[must_use]
    pub fn into_runtime(self) -> crate::lsp::LspConfig {
        let defaults = crate::lsp::LspConfig::default();
        crate::lsp::LspConfig {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            poll_after_edit_ms: self
                .poll_after_edit_ms
                .unwrap_or(defaults.poll_after_edit_ms),
            max_diagnostics_per_file: self
                .max_diagnostics_per_file
                .unwrap_or(defaults.max_diagnostics_per_file),
            include_warnings: self.include_warnings.unwrap_or(defaults.include_warnings),
            servers: self.servers.unwrap_or_default(),
            custom: self.custom.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// OpenRouter upstream slug; disables upstream fallbacks when set.
    pub vendor: Option<String>,
    #[serde(alias = "apiKey")]
    pub api_key: Option<String>,
    #[serde(alias = "baseUrl")]
    pub base_url: Option<String>,
    pub model: Option<String>,
    #[serde(
        default,
        alias = "contextWindow",
        alias = "context_window_tokens",
        alias = "contextWindowTokens",
        alias = "context_length",
        alias = "contextLength"
    )]
    pub context_window: Option<u32>,
    /// Per-model context-window overrides keyed by exact wire model id
    /// (`[providers.<name>.model_context_windows]`, #6108). A matching entry
    /// wins over this provider's `context_window` for that model only, so one
    /// gateway can front models with heterogeneous windows.
    #[serde(default, alias = "modelContextWindows")]
    pub model_context_windows: Option<std::collections::BTreeMap<String, u32>>,
    pub mode: Option<String>,
    /// Dual-wire dialect toggle: `openai` (default) or `anthropic`.
    /// Not a separate catalog provider — config only (DeepSeek / MiniMax /
    /// Model Studio).
    #[serde(
        default,
        alias = "apiStyle",
        alias = "api_style",
        alias = "protocol",
        alias = "wire_format",
        alias = "wireFormat",
        alias = "dialect"
    )]
    pub wire: Option<String>,
    #[serde(alias = "authMode")]
    pub auth_mode: Option<String>,
    /// Core-owned public-client OAuth descriptor for a named plugin provider.
    #[serde(default)]
    pub oauth: Option<crate::oauth::PluginOAuthConfig>,
    /// Validated basename of the active Codewhale-owned xAI OAuth generation.
    /// The file always lives below Codewhale's private credentials directory.
    #[serde(default, alias = "oauthCredentialGeneration")]
    pub oauth_credential_generation: Option<String>,
    #[serde(alias = "insecureSkipTlsVerify")]
    pub insecure_skip_tls_verify: Option<bool>,
    /// Per-provider consent to a plain-HTTP `base_url` (#5991). Loopback is
    /// always allowed without it.
    #[serde(default, alias = "allowInsecureHttp")]
    pub allow_insecure_http: Option<bool>,
    #[serde(alias = "httpHeaders")]
    pub http_headers: Option<HashMap<String, String>>,
    #[serde(alias = "pathSuffix")]
    pub path_suffix: Option<String>,
    #[serde(alias = "reasoningStyle", alias = "reasoningStreamStyle")]
    pub reasoning_stream_style: Option<String>,
    #[serde(
        default,
        alias = "max-concurrency",
        alias = "maxConcurrency",
        alias = "concurrency"
    )]
    pub max_concurrency: Option<usize>,
    pub auth: Option<codewhale_config::ProviderAuthSourceToml>,
    /// Explicit, provider-scoped consent for one credential file owned by
    /// another CLI. Absence is the disabled default.
    #[serde(default, alias = "externalCredentials")]
    pub external_credentials: Option<codewhale_config::ExternalCredentialConsentToml>,
    /// Wire-protocol selector for a custom `[providers.<name>]` entry (#1519).
    ///
    /// Only `"openai-compatible"` is accepted for now; any other value is
    /// rejected at selection time so unsupported wire formats fail loudly rather
    /// than silently routing as OpenAI. Built-in providers leave this unset.
    #[serde(default)]
    pub kind: Option<String>,
    /// Runtime-only receipt; a config file cannot manufacture plugin authority.
    #[serde(skip)]
    pub plugin_authority: Option<crate::plugins::types::PluginAuthority>,
    /// Name of the environment variable holding this custom provider's API key
    /// (#1519), e.g. `api_key_env = "EXAMPLE_API_KEY"`. The key value itself is
    /// never stored in config; only the env var name is.
    #[serde(default, alias = "apiKeyEnv")]
    pub api_key_env: Option<String>,
}

impl ProviderConfig {
    /// True when this entry selects the OpenAI-compatible custom wire protocol.
    ///
    /// `kind` is matched case-insensitively against `openai-compatible` (and the
    /// `openai_compatible` underscore spelling). Returns `false` when `kind` is
    /// unset (built-in providers) or names any other value.
    #[must_use]
    pub fn is_openai_compatible_custom(&self) -> bool {
        self.kind.as_deref().is_some_and(|kind| {
            let normalized = kind.trim().to_ascii_lowercase().replace('_', "-");
            normalized == "openai-compatible"
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub deepseek: ProviderConfig,
    #[serde(default, alias = "deepseekCn")]
    pub deepseek_cn: ProviderConfig,
    #[serde(
        default,
        alias = "deepseek-anthropic",
        alias = "deepseekAnthropic",
        alias = "deepseek-claude",
        alias = "deepseek_claude"
    )]
    pub deepseek_anthropic: ProviderConfig,
    #[serde(default, alias = "nvidiaNim")]
    pub nvidia_nim: ProviderConfig,
    #[serde(default)]
    pub openai: ProviderConfig,
    #[serde(default)]
    pub atlascloud: ProviderConfig,
    #[serde(default, alias = "wanjieArk")]
    pub wanjie_ark: ProviderConfig,
    #[serde(default)]
    pub volcengine: ProviderConfig,
    #[serde(default)]
    pub openrouter: ProviderConfig,
    #[serde(default, alias = "orca_router", alias = "orca")]
    pub orcarouter: ProviderConfig,
    #[serde(
        default,
        alias = "xiaomi",
        alias = "mimo",
        alias = "xiaomimimo",
        alias = "xiaomiMimo"
    )]
    pub xiaomi_mimo: ProviderConfig,
    #[serde(default)]
    pub novita: ProviderConfig,
    #[serde(default)]
    pub fireworks: ProviderConfig,
    #[serde(default)]
    pub siliconflow: ProviderConfig,
    #[serde(
        default,
        alias = "siliconflow-CN",
        alias = "siliconflow-cn",
        alias = "siliconflowCn"
    )]
    pub siliconflow_cn: ProviderConfig,
    #[serde(default)]
    pub arcee: ProviderConfig,
    #[serde(default)]
    pub moonshot: ProviderConfig,
    #[serde(default)]
    pub sglang: ProviderConfig,
    #[serde(default)]
    pub vllm: ProviderConfig,
    #[serde(default)]
    pub ollama: ProviderConfig,
    #[serde(default, alias = "ollama-cloud", alias = "ollamaCloud")]
    pub ollama_cloud: ProviderConfig,
    #[serde(default, alias = "hugging-face", alias = "hf")]
    pub huggingface: ProviderConfig,
    #[serde(default, alias = "model-scope", alias = "model_scope")]
    pub modelscope: ProviderConfig,
    #[serde(default, alias = "deep-infra", alias = "deep_infra")]
    pub deepinfra: ProviderConfig,
    #[serde(default, alias = "together-ai")]
    pub together: ProviderConfig,
    #[serde(
        default,
        alias = "baidu-qianfan",
        alias = "baidu_qianfan",
        alias = "baidu"
    )]
    pub qianfan: ProviderConfig,
    #[serde(
        default,
        alias = "openai-codex",
        alias = "openaiCodex",
        alias = "codex",
        alias = "chatgpt"
    )]
    pub openai_codex: ProviderConfig,
    #[serde(default, alias = "claude")]
    pub anthropic: ProviderConfig,
    #[serde(default, alias = "open-model", alias = "open_model")]
    pub openmodel: ProviderConfig,
    #[serde(
        default,
        alias = "zhipu",
        alias = "zhipuai",
        alias = "bigmodel",
        alias = "big-model"
    )]
    pub zai: ProviderConfig,
    #[serde(default)]
    pub stepfun: ProviderConfig,
    #[serde(default)]
    pub minimax: ProviderConfig,
    #[serde(
        default,
        alias = "minimax-anthropic",
        alias = "minimaxAnthropic",
        alias = "mini-max-anthropic",
        alias = "mini_max_anthropic"
    )]
    pub minimax_anthropic: ProviderConfig,
    #[serde(default, alias = "sakana-ai", alias = "sakana_ai", alias = "fugu")]
    pub sakana: ProviderConfig,
    #[serde(
        default,
        alias = "long-cat",
        alias = "meituan-longcat",
        alias = "meituan"
    )]
    pub longcat: ProviderConfig,
    #[serde(default, alias = "opencode-go", alias = "opencodego")]
    pub opencode_go: ProviderConfig,
    #[serde(
        default,
        alias = "opencode-zen",
        alias = "opencodezen",
        alias = "zen",
        alias = "opencode"
    )]
    pub opencode_zen: ProviderConfig,
    #[serde(
        default,
        alias = "meta-ai",
        alias = "meta_ai",
        alias = "meta-model-api",
        alias = "meta_model_api",
        alias = "muse",
        alias = "muse-spark"
    )]
    pub meta: ProviderConfig,
    #[serde(default, alias = "x-ai", alias = "x_ai", alias = "grok")]
    pub xai: ProviderConfig,
    #[serde(
        default,
        alias = "mistral-ai",
        alias = "mistral_ai",
        alias = "mistralai",
        alias = "la-plateforme",
        alias = "la_plateforme"
    )]
    pub mistral: ProviderConfig,
    #[serde(
        default,
        alias = "google-gemini",
        alias = "google_gemini",
        alias = "gemini"
    )]
    pub google: ProviderConfig,
    #[serde(default, alias = "agy")]
    pub antigravity: ProviderConfig,
    #[serde(
        default,
        alias = "telecom-js",
        alias = "telecom_js",
        alias = "telecomjs-cn",
        alias = "tokenhub"
    )]
    pub telecomjs: ProviderConfig,
    /// Eden AI — OpenAI-compatible AI gateway (aggregator).
    #[serde(default, alias = "eden-ai", alias = "eden_ai")]
    pub edenai: ProviderConfig,
    /// ZenMux — OpenAI-compatible AI gateway (aggregator).
    #[serde(default, alias = "zen-mux", alias = "zen_mux")]
    pub zenmux: ProviderConfig,
    /// CSDN 星图 — OpenAI-compatible hosted platform and Coding Plan.
    #[serde(
        default,
        alias = "csdn-ai",
        alias = "csdn_ai",
        alias = "csdn-coding-plan",
        alias = "csdn_coding_plan",
        alias = "starmap"
    )]
    pub csdn: ProviderConfig,
    /// Concentrate — OpenAI Responses-compatible AI gateway (aggregator).
    #[serde(
        default,
        alias = "concentrate-ai",
        alias = "concentrate_ai",
        alias = "concentrateai"
    )]
    pub concentrate: ProviderConfig,
    /// Codewhale API — account-backed model access over connected provider keys.
    #[serde(
        default,
        alias = "codewhale-api",
        alias = "codewhale_api",
        alias = "cw-api",
        alias = "codewhale-cloud"
    )]
    pub codewhale: ProviderConfig,
    /// Alibaba Cloud Model Studio — Token Plan (OpenAI-compatible Chat Completions).
    #[serde(default, alias = "modelstudio-token-plan")]
    pub modelstudio_token_plan: ProviderConfig,
    /// Alibaba Cloud Model Studio — Token Plan Anthropic-compatible endpoint.
    #[serde(default, alias = "modelstudio-token-plan-anthropic")]
    pub modelstudio_token_plan_anthropic: ProviderConfig,
    /// Alibaba Cloud Model Studio — Coding Plan (OpenAI-compatible Chat Completions).
    #[serde(default, alias = "modelstudio-coding-plan")]
    pub modelstudio_coding_plan: ProviderConfig,
    /// Alibaba Cloud Model Studio — Coding Plan Anthropic-compatible endpoint.
    #[serde(default, alias = "modelstudio-coding-plan-anthropic")]
    pub modelstudio_coding_plan_anthropic: ProviderConfig,
    /// Arbitrary user-named custom providers (#1519).
    ///
    /// Captures every `[providers.<name>]` table whose key is not one of the
    /// built-in providers above. Each entry is an OpenAI-compatible custom
    /// endpoint selected via `provider = "<name>"`; routing reads its
    /// `base_url` / `model` / `api_key_env` through [`ProviderKind::Custom`].
    #[serde(flatten, default)]
    pub custom: HashMap<String, ProviderConfig>,
}

impl ProvidersConfig {
    /// Look up a user-defined custom provider table by its `[providers.<name>]`
    /// key (#1519). Returns `None` when no entry with that exact name exists.
    #[must_use]
    pub fn custom_provider_config(&self, name: &str) -> Option<&ProviderConfig> {
        self.custom.get(name)
    }

    fn validate(&self) -> Result<()> {
        let builtins = [
            ("providers.deepseek", &self.deepseek),
            ("providers.deepseek_cn", &self.deepseek_cn),
            ("providers.deepseek_anthropic", &self.deepseek_anthropic),
            ("providers.nvidia_nim", &self.nvidia_nim),
            ("providers.openai", &self.openai),
            ("providers.atlascloud", &self.atlascloud),
            ("providers.wanjie_ark", &self.wanjie_ark),
            ("providers.volcengine", &self.volcengine),
            ("providers.openrouter", &self.openrouter),
            ("providers.xiaomi_mimo", &self.xiaomi_mimo),
            ("providers.novita", &self.novita),
            ("providers.fireworks", &self.fireworks),
            ("providers.siliconflow", &self.siliconflow),
            ("providers.siliconflow_cn", &self.siliconflow_cn),
            ("providers.arcee", &self.arcee),
            ("providers.moonshot", &self.moonshot),
            ("providers.sglang", &self.sglang),
            ("providers.vllm", &self.vllm),
            ("providers.ollama", &self.ollama),
            ("providers.ollama_cloud", &self.ollama_cloud),
            ("providers.huggingface", &self.huggingface),
            ("providers.deepinfra", &self.deepinfra),
            ("providers.together", &self.together),
            ("providers.qianfan", &self.qianfan),
            ("providers.openai_codex", &self.openai_codex),
            ("providers.anthropic", &self.anthropic),
            ("providers.openmodel", &self.openmodel),
            ("providers.zai", &self.zai),
            ("providers.stepfun", &self.stepfun),
            ("providers.minimax", &self.minimax),
            ("providers.minimax_anthropic", &self.minimax_anthropic),
            ("providers.sakana", &self.sakana),
            ("providers.opencode_go", &self.opencode_go),
            ("providers.opencode_zen", &self.opencode_zen),
            ("providers.meta", &self.meta),
            ("providers.xai", &self.xai),
        ];
        for (name, config) in builtins {
            validate_provider_context_window(name, config.context_window)?;
            validate_model_context_windows(name, config.model_context_windows.as_ref())?;
        }
        for (name, config) in &self.custom {
            let name = format!("providers.{name}");
            validate_provider_context_window(&name, config.context_window)?;
            validate_model_context_windows(&name, config.model_context_windows.as_ref())?;
        }
        Ok(())
    }
}

fn validate_provider_context_window(name: &str, value: Option<u32>) -> Result<()> {
    if value == Some(0) {
        anyhow::bail!("{name}.context_window must be greater than 0");
    }
    Ok(())
}

fn validate_model_context_windows(
    name: &str,
    table: Option<&std::collections::BTreeMap<String, u32>>,
) -> Result<()> {
    if let Some(table) = table {
        for (model, window) in table {
            if *window == 0 {
                anyhow::bail!("{name}.model_context_windows.{model} must be greater than 0");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ConfigFile {
    /// Boxed so the parsed document never carries the multi-kilobyte
    /// `Config` by value through `toml::de` and `apply_profile` frames. A
    /// `#[tokio::test]` runs those frames on libtest's default 2 MiB stack,
    /// which the by-value copies overflowed (#6362).
    #[serde(flatten)]
    base: Box<Config>,
    profiles: Option<HashMap<String, Config>>,
    #[serde(skip)]
    legacy_root: codewhale_config::legacy_root::LegacyRootMigration,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RequirementsFile {
    #[serde(default)]
    allowed_approval_policies: Vec<String>,
    #[serde(default)]
    allowed_sandbox_modes: Vec<String>,
}

/// The highest-precedence source that can currently own approval policy.
///
/// The resolved [`Config`] historically retained only the final string, which
/// made an in-session editor unable to distinguish a user-owned root key from
/// a profile, environment, managed, requirements, or project constraint. The
/// destructive Full Access preset uses this classification to fail closed
/// unless it can prove that removing the root key is the operation requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalPolicyControl {
    Unset,
    RootConfig,
    Profile,
    Environment,
    ManagedConfig,
    ProjectConfig,
    Requirements,
    Ambiguous,
}

impl ApprovalPolicyControl {
    #[must_use]
    pub(crate) fn editable_root(self) -> bool {
        matches!(self, Self::Unset | Self::RootConfig)
    }

    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unset => "saved TUI posture",
            Self::RootConfig => "the root config.toml approval_policy",
            Self::Profile => "the active config profile",
            Self::Environment => {
                set_env_var_name(APPROVAL_POLICY_ENV).unwrap_or(APPROVAL_POLICY_ENV[0])
            }
            Self::ManagedConfig => "managed configuration",
            Self::ProjectConfig => "project configuration",
            Self::Requirements => "managed approval requirements",
            Self::Ambiguous => "an unresolved configuration source",
        }
    }
}

/// Highest-precedence source that owns the interactive shell availability
/// switch. Project/profile/environment/managed constraints are intentionally
/// read-only from the root settings editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellAccessControl {
    Unset,
    RootConfig,
    Profile,
    Environment,
    ManagedConfig,
    ProjectConfig,
    Ambiguous,
}

impl ShellAccessControl {
    #[must_use]
    pub(crate) fn editable_root(self) -> bool {
        matches!(self, Self::Unset | Self::RootConfig)
    }

    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unset => "the session default",
            Self::RootConfig => "the root config.toml allow_shell",
            Self::Profile => "the active config profile",
            Self::Environment => set_env_var_name(ALLOW_SHELL_ENV).unwrap_or(ALLOW_SHELL_ENV[0]),
            Self::ManagedConfig => "managed configuration",
            Self::ProjectConfig => "project configuration",
            Self::Ambiguous => "an unresolved configuration source",
        }
    }
}

const APPROVAL_POLICY_ENV: [&str; 2] = ["CODEWHALE_APPROVAL_POLICY", "DEEPSEEK_APPROVAL_POLICY"];
const ALLOW_SHELL_ENV: [&str; 2] = ["CODEWHALE_ALLOW_SHELL", "DEEPSEEK_ALLOW_SHELL"];

/// The variable of a `[CODEWHALE_*, legacy DEEPSEEK_*]` pair that is set,
/// in the precedence the config readers use (the `CODEWHALE_*` name wins).
/// The dispatcher only exports `CODEWHALE_*` (#6516), so a `DEEPSEEK_*` hit
/// means the user set the legacy name themselves; naming it keeps the
/// settings editor pointing at the variable that actually owns the value.
fn set_env_var_name(names: [&'static str; 2]) -> Option<&'static str> {
    let read = || {
        names
            .into_iter()
            .find(|name| std::env::var_os(name).is_some())
    };
    #[cfg(test)]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(test))]
    {
        read()
    }
}

fn approval_policy_env_is_set() -> bool {
    set_env_var_name(APPROVAL_POLICY_ENV).is_some()
}

fn allow_shell_env_is_set() -> bool {
    set_env_var_name(ALLOW_SHELL_ENV).is_some()
}

fn project_config_root_bool(workspace: &Path, key: &str) -> Option<bool> {
    [
        workspace
            .join(codewhale_config::CODEWHALE_APP_DIR)
            .join("config.toml"),
        workspace
            .join(codewhale_config::LEGACY_APP_DIR)
            .join("config.toml"),
    ]
    .into_iter()
    .find(|path| path.exists())
    .and_then(|path| std::fs::read_to_string(path).ok())
    .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
    .and_then(|document| document.get(key).and_then(toml::Value::as_bool))
}

/// Map the saved TUI permission posture onto the approval-policy ordering used
/// by project config. Full Access is looser than every project policy, so its
/// baseline is the loosest ranked policy (`auto`).
#[must_use]
pub(crate) fn approval_policy_baseline_from_permission_posture(
    posture: Option<&str>,
) -> Option<&'static str> {
    posture.and_then(
        |posture| match posture.trim().to_ascii_lowercase().as_str() {
            "ask" | "suggest" | "on-request" | "untrusted" => Some("on-request"),
            "auto" | "auto-review" | "auto_review" => Some("auto"),
            "full" | "full-access" | "full_access" | "bypass" => Some("auto"),
            _ => None,
        },
    )
}

// === Config Loading ===

impl Config {
    /// The read deny-list in force for this config (S1).
    ///
    /// Unions the built-in credential-store defaults (unless
    /// `sandbox_read_denylist_defaults = false`) with
    /// `sandbox_denied_read_paths`, minus `sandbox_read_denylist_exempt`.
    /// Feeds both the in-process file-reading tools and the OS sandbox
    /// wrappers, so a shell `cat` and a `read_file` call refuse the same paths.
    #[must_use]
    pub fn read_denylist(&self) -> crate::sandbox::read_guard::ReadDenylist {
        crate::sandbox::read_guard::ReadDenylist::build(
            self.sandbox_read_denylist_defaults.unwrap_or(true),
            &self.sandbox_denied_read_paths,
            &self.sandbox_read_denylist_exempt,
        )
    }

    /// Every path the OS sandbox wrappers should deny reads under — the
    /// deny-list's subtree rules, home-expanded and normalized.
    #[must_use]
    pub fn effective_sandbox_denied_read_paths(&self) -> Vec<std::path::PathBuf> {
        self.read_denylist().subtree_paths()
    }

    /// Whether Linux shell commands prefer bubblewrap confinement.
    ///
    /// On by default: an unset `prefer_bwrap` means sandboxed commands use
    /// the OS wrapper whenever `/usr/bin/bwrap` works on this host, matching
    /// the Seatbelt behavior macOS already has. An explicit `false` opts out
    /// and leaves Linux commands unwrapped.
    #[must_use]
    pub fn prefers_bwrap(&self) -> bool {
        self.prefer_bwrap.unwrap_or(true)
    }

    #[must_use]
    pub fn stop_words(&self) -> Vec<String> {
        self.stop_words.clone().unwrap_or_else(default_stop_words)
    }

    /// Structural external-credential status for user-facing inventory. This
    /// resolves only environment/config strings and performs no filesystem or
    /// network access.
    pub(crate) fn external_credential_consent_status(
        &self,
        identity: &ProviderIdentity,
    ) -> Option<codewhale_config::ExternalCredentialConsentStatus> {
        if identity.key.as_str() == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id {
            return None;
        }
        let provider = identity.provider;
        let (kind, source, path) = match provider {
            ProviderKind::OpenaiCodex => (
                codewhale_config::ProviderKind::OpenaiCodex,
                codewhale_config::ExternalCredentialSource::CodexCli,
                crate::oauth::auth_file_path(),
            ),
            ProviderKind::Xai => (
                codewhale_config::ProviderKind::Xai,
                codewhale_config::ExternalCredentialSource::GrokCli,
                crate::oauth::grok_auth_file_path(),
            ),
            ProviderKind::Deepseek => (
                codewhale_config::ProviderKind::Deepseek,
                codewhale_config::ExternalCredentialSource::DshCli,
                codewhale_config::default_dsh_credentials_path(),
            ),
            ProviderKind::DeepseekAnthropic => (
                codewhale_config::ProviderKind::DeepseekAnthropic,
                codewhale_config::ExternalCredentialSource::DshCli,
                codewhale_config::default_dsh_credentials_path(),
            ),
            _ => return None,
        };
        self.verify_provider_identity(identity).ok()?;
        let active = self.active_provider_identity().ok()?;
        let consent = self
            .provider_config_for(identity)
            .and_then(|entry| entry.external_credentials.as_ref());
        let mut status = codewhale_config::external_credential_consent_status(
            consent,
            kind,
            source,
            &path,
            active.provider,
        );
        if active != *identity {
            status.route_state = "dormant";
        }
        Some(status)
    }

    /// Return the non-root source that prevents an interactive runtime preset
    /// from safely rewriting approval, shell, and sandbox posture. Presets may
    /// edit user-owned root keys, but must never overwrite a profile, env,
    /// managed, requirements, or project constraint in the live merged Config.
    #[must_use]
    pub(crate) fn runtime_preset_blocker(
        &self,
        config_path: Option<&Path>,
        profile: Option<&str>,
        workspace: &Path,
    ) -> Option<&'static str> {
        let requirements_path = self
            .requirements_path
            .as_deref()
            .map(expand_path)
            .or_else(default_requirements_path);
        if let Some(path) = requirements_path
            && path.exists()
        {
            let controlled = std::fs::read_to_string(path)
                .ok()
                .and_then(|raw| toml::from_str::<RequirementsFile>(&raw).ok())
                .is_none_or(|requirements| {
                    !requirements.allowed_approval_policies.is_empty()
                        || !requirements.allowed_sandbox_modes.is_empty()
                });
            if controlled {
                return Some("managed runtime requirements");
            }
        }

        let workspace_is_home = effective_home_dir().is_some_and(|home| {
            let workspace = workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf());
            let home = home.canonicalize().unwrap_or(home);
            workspace == home
        });
        let project_controls_runtime = || {
            let saved_approval_baseline = crate::settings::Settings::load_persisted()
                .ok()
                .and_then(|settings| settings.permission_posture)
                .and_then(|posture| {
                    approval_policy_baseline_from_permission_posture(Some(&posture))
                });
            let approval_baseline = self.approval_policy.as_deref().or(saved_approval_baseline);
            let parsed_controls =
                codewhale_config::load_project_config(workspace).is_some_and(|project| {
                    project.approval_policy.as_deref().is_some_and(|policy| {
                        codewhale_config::project_approval_policy_is_allowed(
                            approval_baseline,
                            policy,
                        )
                    }) || project.sandbox_mode.as_deref().is_some_and(|sandbox| {
                        codewhale_config::project_sandbox_mode_is_allowed(
                            self.sandbox_mode.as_deref(),
                            sandbox,
                        )
                    })
                });
            parsed_controls || project_config_root_bool(workspace, "allow_shell") == Some(false)
        };
        if !workspace_is_home && project_controls_runtime() {
            return Some("project runtime configuration");
        }

        let managed_path = self
            .managed_config_path
            .as_deref()
            .map(expand_path)
            .or_else(default_managed_config_path);
        if let Some(path) = managed_path
            && path.exists()
        {
            match load_single_config_file(&path) {
                Ok(managed)
                    if managed.approval_policy.is_some()
                        || managed.sandbox_mode.is_some()
                        || managed.allow_shell.is_some() =>
                {
                    return Some("managed runtime configuration");
                }
                Err(_) => return Some("an unreadable managed runtime configuration"),
                Ok(_) => {}
            }
        }

        let env_controls_runtime = || {
            approval_policy_env_is_set()
                || allow_shell_env_is_set()
                || std::env::var_os("CODEWHALE_SANDBOX_MODE").is_some()
                || std::env::var_os("DEEPSEEK_SANDBOX_MODE").is_some()
        };
        #[cfg(test)]
        let env_controls_runtime = crate::test_support::with_test_env_lock(env_controls_runtime);
        #[cfg(not(test))]
        let env_controls_runtime = env_controls_runtime();
        if env_controls_runtime {
            return Some("environment-controlled runtime posture");
        }

        if let Some(profile) = profile {
            let path = match resolve_load_config_path(config_path.map(Path::to_path_buf)) {
                Ok(Some(path)) => path,
                Ok(None) => return Some("an unresolved active config profile"),
                Err(_) => return Some("an invalid active config path override"),
            };
            let Some(parsed) = std::fs::read_to_string(path)
                .ok()
                .and_then(|raw| parse_config_file(&raw).ok())
            else {
                return Some("an unreadable active config profile");
            };
            if parsed
                .profiles
                .as_ref()
                .and_then(|profiles| profiles.get(profile))
                .is_some_and(|profile| {
                    profile.approval_policy.is_some()
                        || profile.sandbox_mode.is_some()
                        || profile.allow_shell.is_some()
                })
            {
                return Some("the active config profile");
            }
        }

        None
    }

    /// Identify whether the effective approval policy can safely be edited by
    /// changing the root user config. Sources applied later in the load chain
    /// are deliberately treated as controlling even when their value happens
    /// to equal the root value; equality is not provenance.
    #[must_use]
    pub(crate) fn approval_policy_control(
        &self,
        config_path: Option<&Path>,
        profile: Option<&str>,
        workspace: &Path,
    ) -> ApprovalPolicyControl {
        if self.approval_policy_is_requirements_managed() {
            return ApprovalPolicyControl::Requirements;
        }

        let workspace_is_home = effective_home_dir().is_some_and(|home| {
            let workspace = workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf());
            let home = home.canonicalize().unwrap_or(home);
            workspace == home
        });
        if !workspace_is_home {
            let saved_approval_baseline = crate::settings::Settings::load_persisted()
                .ok()
                .and_then(|settings| settings.permission_posture)
                .and_then(|posture| {
                    approval_policy_baseline_from_permission_posture(Some(&posture))
                });
            let approval_baseline = self.approval_policy.as_deref().or(saved_approval_baseline);
            if codewhale_config::load_project_config(workspace)
                .and_then(|project| project.approval_policy)
                .is_some_and(|policy| {
                    codewhale_config::project_approval_policy_is_allowed(approval_baseline, &policy)
                })
            {
                return ApprovalPolicyControl::ProjectConfig;
            }
        }

        let managed_path = self
            .managed_config_path
            .as_deref()
            .map(expand_path)
            .or_else(default_managed_config_path);
        if let Some(path) = managed_path
            && path.exists()
        {
            match load_single_config_file(&path) {
                Ok(managed) if managed.approval_policy.is_some() => {
                    return ApprovalPolicyControl::ManagedConfig;
                }
                Err(_) => return ApprovalPolicyControl::Ambiguous,
                Ok(_) => {}
            }
        }

        if approval_policy_env_is_set() {
            return ApprovalPolicyControl::Environment;
        }

        let path = match resolve_load_config_path(config_path.map(Path::to_path_buf)) {
            Ok(Some(path)) => path,
            Ok(None) | Err(_) => {
                return if self.approval_policy.is_some() {
                    ApprovalPolicyControl::Ambiguous
                } else {
                    ApprovalPolicyControl::Unset
                };
            }
        };
        let parsed = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| parse_config_file(&raw).ok());
        let Some(parsed) = parsed else {
            return if self.approval_policy.is_some() {
                ApprovalPolicyControl::Ambiguous
            } else {
                ApprovalPolicyControl::Unset
            };
        };
        if let Some(profile) = profile
            && parsed
                .profiles
                .as_ref()
                .and_then(|profiles| profiles.get(profile))
                .is_some_and(|profile| profile.approval_policy.is_some())
        {
            return ApprovalPolicyControl::Profile;
        }
        if parsed.base.approval_policy.is_some() {
            ApprovalPolicyControl::RootConfig
        } else if self.approval_policy.is_some() {
            ApprovalPolicyControl::Ambiguous
        } else {
            ApprovalPolicyControl::Unset
        }
    }

    /// Identify whether shell availability can safely be edited through the
    /// user-owned root config. Later sources are controlling even when their
    /// effective value happens to match the root value.
    #[must_use]
    pub(crate) fn allow_shell_control(
        &self,
        config_path: Option<&Path>,
        profile: Option<&str>,
        workspace: &Path,
    ) -> ShellAccessControl {
        let workspace_is_home = effective_home_dir().is_some_and(|home| {
            let workspace = workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf());
            let home = home.canonicalize().unwrap_or(home);
            workspace == home
        });
        if !workspace_is_home && project_config_root_bool(workspace, "allow_shell") == Some(false) {
            return ShellAccessControl::ProjectConfig;
        }

        let managed_path = self
            .managed_config_path
            .as_deref()
            .map(expand_path)
            .or_else(default_managed_config_path);
        if let Some(path) = managed_path
            && path.exists()
        {
            match load_single_config_file(&path) {
                Ok(managed) if managed.allow_shell.is_some() => {
                    return ShellAccessControl::ManagedConfig;
                }
                Err(_) => return ShellAccessControl::Ambiguous,
                Ok(_) => {}
            }
        }

        if allow_shell_env_is_set() {
            return ShellAccessControl::Environment;
        }

        let path = match resolve_load_config_path(config_path.map(Path::to_path_buf)) {
            Ok(Some(path)) => path,
            Ok(None) | Err(_) => {
                return if self.allow_shell.is_some() {
                    ShellAccessControl::Ambiguous
                } else {
                    ShellAccessControl::Unset
                };
            }
        };
        let parsed = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| parse_config_file(&raw).ok());
        let Some(parsed) = parsed else {
            return if self.allow_shell.is_some() {
                ShellAccessControl::Ambiguous
            } else {
                ShellAccessControl::Unset
            };
        };
        if let Some(profile) = profile
            && parsed
                .profiles
                .as_ref()
                .and_then(|profiles| profiles.get(profile))
                .is_some_and(|profile| profile.allow_shell.is_some())
        {
            return ShellAccessControl::Profile;
        }
        if parsed.base.allow_shell.is_some() {
            ShellAccessControl::RootConfig
        } else if self.allow_shell.is_some() {
            ShellAccessControl::Ambiguous
        } else {
            ShellAccessControl::Unset
        }
    }

    /// Whether an explicit config or requirements file owns approval posture.
    /// TUI preferences may supply a default only when this is false.
    #[must_use]
    pub fn approval_policy_is_managed(&self) -> bool {
        if self.approval_policy.is_some() {
            return true;
        }
        self.approval_policy_is_requirements_managed()
    }

    /// Whether organization requirements, rather than a user-editable config
    /// key, own approval posture. User config still outranks TUI settings, but
    /// `/config approval_mode ... --save` may edit that user-owned key.
    /// Sandbox requirements also lock posture: Full Access changes the implicit
    /// sandbox. This conservatively locks even posture changes that would fit.
    #[must_use]
    pub fn approval_policy_is_requirements_managed(&self) -> bool {
        let path = self
            .requirements_path
            .as_deref()
            .map(expand_path)
            .or_else(default_requirements_path);
        let Some(path) = path else {
            return false;
        };
        if !path.exists() {
            return false;
        }
        // Fail closed if a present requirements file becomes unreadable or
        // malformed between Config::load and App::new.
        std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| toml::from_str::<RequirementsFile>(&contents).ok())
            .is_none_or(|requirements| {
                !requirements.allowed_approval_policies.is_empty()
                    || !requirements.allowed_sandbox_modes.is_empty()
            })
    }

    #[must_use]
    pub fn search_provider_resolution(&self) -> SearchProviderResolution {
        if let Ok(raw) = std::env::var("CODEWHALE_SEARCH_PROVIDER")
            .or_else(|_| std::env::var("DEEPSEEK_SEARCH_PROVIDER"))
            && let Some(provider) = SearchProvider::parse(&raw)
        {
            return SearchProviderResolution {
                provider,
                source: SearchProviderSource::EnvOverride,
            };
        }

        if let Some(provider) = self.search.as_ref().and_then(|search| search.provider) {
            return SearchProviderResolution {
                provider,
                source: SearchProviderSource::Config,
            };
        }

        // Tavily autodetect: a dedicated `TAVILY_API_KEY`, or a generic
        // `[search] api_key` in the `tvly-` family. Runtime-only — never write
        // `[search] provider` from here, and never merge the env key into
        // `search.api_key`.
        let generic_key = self
            .search
            .as_ref()
            .and_then(|search| search.api_key.as_deref());
        if tavily_key_from(generic_key).is_some() {
            return SearchProviderResolution {
                provider: SearchProvider::Tavily,
                source: SearchProviderSource::TavilyKey,
            };
        }

        SearchProviderResolution {
            provider: SearchProvider::default(),
            source: SearchProviderSource::Default,
        }
    }

    #[must_use]
    pub fn search_provider(&self) -> SearchProvider {
        self.search_provider_resolution().provider
    }

    /// Whether provider-native search may lead the search chain.
    ///
    /// `[search] native = true|false` is explicit. Unset, a user-chosen
    /// provider (config, env, or a Tavily key) wins over provider-native
    /// search (`Some(false)`); with no provider configured it stays `None`,
    /// which keeps native search first on routes that offer it.
    #[must_use]
    pub fn search_native(&self) -> Option<bool> {
        self.search
            .as_ref()
            .and_then(|search| search.native)
            .or_else(|| {
                (self.search_provider_resolution().source != SearchProviderSource::Default)
                    .then_some(false)
            })
    }

    /// Store a session/config provider choice and return the effective runtime
    /// provider after applying the documented environment precedence.
    pub fn set_search_provider(&mut self, provider: SearchProvider) -> SearchProvider {
        self.search
            .get_or_insert_with(SearchConfig::default)
            .provider = Some(provider);
        self.search_provider()
    }

    /// Return `true` if the `[auto] cost_saving = true` opt-in is set
    /// (#1207). When true, the auto-mode router biases toward the active
    /// provider's validated fast sibling for ambiguous requests instead of
    /// its strong tier. Providers without a known sibling stay on the active
    /// model. Default: `false` (balanced behaviour).
    #[must_use]
    pub fn auto_cost_saving(&self) -> bool {
        self.auto
            .as_ref()
            .and_then(|a| a.cost_saving)
            .unwrap_or(false)
    }

    /// Return `true` only when `[auto] cross_provider = true` is persisted in
    /// config (#4411). Auto mode otherwise stays on the active provider: the
    /// classifier never sees other providers' routes, and the local fallback
    /// never selects one. There is no interactive toggle — enabling
    /// cross-provider Auto is an explicit, durable config edit.
    #[must_use]
    pub fn auto_cross_provider(&self) -> bool {
        self.auto
            .as_ref()
            .and_then(|a| a.cross_provider)
            .unwrap_or(false)
    }

    /// Classifier call timeout for `[auto.router]` in seconds. Defaults to
    /// [`DEFAULT_AUTO_ROUTER_TIMEOUT_SECS`] (4); `0` means "use the default".
    /// Values above [`MAX_AUTO_ROUTER_TIMEOUT_SECS`] (300) are clamped so a
    /// hung local router cannot stall a turn indefinitely.
    #[must_use]
    pub fn auto_router_timeout_secs(&self) -> u64 {
        self.auto
            .as_ref()
            .and_then(|a| a.router.as_ref())
            .and_then(|r| r.timeout_secs)
            .filter(|secs| *secs > 0)
            .unwrap_or(DEFAULT_AUTO_ROUTER_TIMEOUT_SECS)
            .min(MAX_AUTO_ROUTER_TIMEOUT_SECS)
    }

    /// Decision-router confidence floor, clamped to `0..=1`; absent or
    /// non-finite values use [`DEFAULT_AUTO_ROUTER_MIN_CONFIDENCE`].
    #[must_use]
    pub(crate) fn auto_router_min_confidence(&self) -> f64 {
        self.auto
            .as_ref()
            .and_then(|a| a.router.as_ref())
            .and_then(|r| r.min_confidence)
            .filter(|value| value.is_finite())
            .unwrap_or(DEFAULT_AUTO_ROUTER_MIN_CONFIDENCE)
            .clamp(0.0, 1.0)
    }

    #[must_use]
    pub fn tools_always_load(&self) -> std::collections::HashSet<String> {
        self.tools
            .as_ref()
            .map(|tools| {
                tools
                    .always_load
                    .iter()
                    .map(|name| name.trim())
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Effective `request_user_input` payload ceilings for this session
    /// (#5949). Resolved once per engine build; out-of-range `[tools]` values
    /// clamp with a single warning naming the key and the value used.
    #[must_use]
    pub fn user_input_limits(&self) -> crate::tools::user_input::UserInputLimits {
        let tools = self.tools.as_ref();
        crate::tools::user_input::UserInputLimits::from_config_values(
            tools.and_then(|t| t.user_input_max_questions),
            tools.and_then(|t| t.user_input_max_options),
        )
    }

    /// Effective wait for a user-input answer or an approval decision
    /// (#6003). `None` or `0` waits until the person answers or cancels.
    /// A positive value bounds that one wait; values above 24h clamp.
    #[must_use]
    pub fn user_input_timeout(&self) -> Option<std::time::Duration> {
        const MAX_SECONDS: u64 = 86_400;
        let seconds = self
            .tools
            .as_ref()
            .and_then(|tools| tools.user_input_timeout_seconds)?;
        if seconds > MAX_SECONDS {
            tracing::warn!(
                "[tools] user_input_timeout_seconds={seconds} exceeds 24h; clamping to {MAX_SECONDS}"
            );
        }
        Some(std::time::Duration::from_secs(seconds.min(MAX_SECONDS)))
    }

    #[must_use]
    pub fn auto_review_policy(&self) -> crate::core::authority::auto_review::AutoReviewPolicy {
        self.auto_review
            .as_ref()
            .map(AutoReviewConfig::to_runtime_policy)
            .unwrap_or_default()
    }

    /// Load configuration from disk and merge with environment overrides.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// # use crate::config::Config;
    /// let config = Config::load(None, None)?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn load(path: Option<PathBuf>, profile: Option<&str>) -> Result<Self> {
        Self::load_with_environment_policy(path, profile, ConfigEnvironmentPolicy::Runtime)
    }

    /// Load configuration for a structural diagnostic without materializing
    /// secret-bearing environment values into the returned configuration.
    ///
    /// This still applies the ordinary safe routing, model, and policy
    /// overrides so doctor describes the runtime the user selected. Provider
    /// credentials are resolved only inside an explicit live-probe boundary.
    pub(crate) fn load_structural(path: Option<PathBuf>, profile: Option<&str>) -> Result<Self> {
        Self::load_with_environment_policy(
            path,
            profile,
            ConfigEnvironmentPolicy::StructuralDiagnostic,
        )
    }

    /// Parse persisted configuration through the same profile precedence used
    /// at startup, without environment, credentials, or filesystem writes.
    pub(crate) fn from_saved_document(contents: &str, profile: Option<&str>) -> Result<Self> {
        let parsed = parse_config_file(contents).map_err(|_| {
            anyhow::anyhow!("Failed to parse configuration; file contents were omitted")
        })?;
        let legacy_root = parsed.legacy_root.clone();
        let mut config = apply_profile(parsed, profile)?;
        config.account_profile = profile.map(str::to_owned);
        config.legacy_root = legacy_root;
        Ok(config)
    }

    fn load_with_environment_policy(
        path: Option<PathBuf>,
        profile: Option<&str>,
        environment_policy: ConfigEnvironmentPolicy,
    ) -> Result<Self> {
        let path = resolve_load_config_path(path)?;
        let mut config = if let Some(path) = path.as_ref() {
            if path.exists() {
                let contents = fs::read_to_string(path)
                    .with_context(|| format!("Failed to read config file: {}", path.display()))?;
                let parsed = Self::from_saved_document(&contents, profile).with_context(|| {
                    format!(
                        "Failed to load config file {}",
                        codewhale_config::quote_os_path(path)
                    )
                })?;
                if let Some(msg) = warn_on_misplaced_top_level_keys(&contents) {
                    tracing::warn!("{msg}");
                }
                parsed
            } else {
                apply_profile(ConfigFile::default(), profile)?;
                Config::default()
            }
        } else {
            apply_profile(ConfigFile::default(), profile)?;
            Config::default()
        };

        // Scope and profile choices outrank device startup memory. Environment
        config.account_profile = profile.map(str::to_owned);
        // and managed values are applied afterwards, so their models win too.
        if profile.is_none() && path.as_deref().is_some_and(is_home_config_path) {
            if let Ok(settings) =
                crate::settings::Settings::load_legacy_route_preferences_read_only()
            {
                config.apply_saved_selection(&settings);
            }
            config.remembered_selection_scope = Some(true);
        } else {
            config.remembered_selection_scope = Some(false);
        }
        apply_env_overrides(&mut config, environment_policy);
        apply_managed_overrides(&mut config)?;
        apply_requirements(&mut config)?;
        crate::plugins::providers::apply_startup_providers(&mut config)?;
        normalize_model_config(&mut config);
        config.exec_policy_engine = load_sibling_exec_policy_engine(path.as_deref())?;
        config.loaded_config_path = path.as_deref().map(std::path::absolute).transpose()?;
        config.validate()?;
        Ok(config)
    }

    #[cfg(test)]
    pub(crate) fn remembered_selection_is_applicable(&self) -> bool {
        self.remembered_selection_scope.unwrap_or(true)
    }

    /// Old chooser memory used the public Cloud key even when the active
    /// hosted Ollama route still owns the legacy `ollama` table.
    pub(crate) fn legacy_selection_identity(
        &self,
        key: &str,
    ) -> std::result::Result<ProviderIdentity, String> {
        if let Ok(active) = self.active_provider_identity()
            && active.key.as_str() == key
        {
            Ok(active)
        } else {
            self.resolve_provider_pin_identity(key)
        }
    }

    /// Resolve the existing startup preferences into the Config snapshot shared
    /// by the TUI, inventory and Runtime. Never touch persisted preferences or
    /// reread them while binding a saved thread's route.
    pub(crate) fn apply_saved_selection(&mut self, settings: &crate::settings::Settings) -> bool {
        if self.remembered_selection_scope.is_some() {
            return false;
        }
        self.remembered_selection_scope = Some(true);
        if self.default_text_model.is_none() {
            self.default_text_model.clone_from(&self.legacy_model);
        }
        if self.fleet_operator_route_applied || self.route_preferences_version.is_some() {
            return false;
        }
        let mut active_changed = false;
        if let Some(provider) = settings.default_provider.as_deref()
            && let Ok(identity) = self.resolve_provider_identity(provider)
        {
            active_changed = self
                .active_provider_identity()
                .is_ok_and(|active| active != identity);
            if self.scope_to_provider_identity(&identity).is_err() {
                return false;
            }
        }
        let mut choices = settings.provider_models.clone().unwrap_or_default();
        if let Some(model) = settings.default_model.as_ref() {
            for key in [
                ProviderKind::Deepseek.as_str(),
                codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id,
            ] {
                choices.entry(key.into()).or_insert_with(|| model.clone());
            }
        }
        let mut choices: Vec<_> = choices.into_iter().collect();
        // Old TUI reads used the exact public identity. If a hand-edited
        // archive contains aliases too, process that canonical entry last.
        choices.sort_by_key(|(key, _)| {
            (
                self.legacy_selection_identity(key)
                    .is_ok_and(|identity| identity.key.as_str() == key),
                key.clone(),
            )
        });
        for (key, remembered) in choices {
            let remembered = remembered.trim();
            if remembered.is_empty() || remembered.chars().any(char::is_control) {
                continue;
            }
            let Ok(identity) = self.legacy_selection_identity(&key) else {
                continue;
            };
            let provider = identity.provider;
            let mut scoped = self.clone();
            if scoped.scope_to_provider_identity(&identity).is_err() {
                continue;
            }
            let configured = scoped
                .provider_config_for(&identity)
                .and_then(|entry| entry.model.as_deref())
                .or_else(|| {
                    self.active_provider_identity()
                        .is_ok_and(|active| active == identity)
                        .then_some(self.default_text_model.as_deref())
                        .flatten()
                })
                .unwrap_or_default();
            let declared = crate::provider_lake::configured_model_for_route(
                &scoped,
                provider,
                identity.key.as_str(),
                &scoped.base_url_for_route(&identity),
                remembered,
            )
            .is_some();
            let model = if declared {
                remembered.to_string()
            } else {
                prefer_configured_model_spelling(configured, remembered.to_string())
            };
            if provider == ProviderKind::Custom && identity.persisted_id().is_some() {
                if let Some(entry) = self
                    .providers
                    .as_mut()
                    .and_then(|providers| providers.custom.get_mut(identity.key.as_str()))
                {
                    entry.model = Some(model);
                }
            } else if identity.migrated_legacy_ollama_cloud_route {
                self.providers
                    .get_or_insert_with(ProvidersConfig::default)
                    .ollama
                    .model = Some(model);
            } else {
                self.set_provider_model_override(&identity, Some(model))
                    .unwrap();
            }
            active_changed |= self
                .active_provider_identity()
                .is_ok_and(|active| active == identity);
        }
        active_changed
    }

    /// Validate that critical config fields are present.
    pub fn validate(&self) -> Result<()> {
        if let Some(provider) = self.provider.as_deref()
            && !is_legacy_antigravity_identity(provider)
            && compatibility_for_selector(provider).is_none()
            && self
                .providers
                .as_ref()
                .and_then(|providers| providers.custom_provider_config(provider))
                .is_none()
        {
            return Err(invalid_provider_diagnostic(provider).into());
        }
        let identity = self
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        let active_provider = identity.provider;
        codewhale_config::catalog::configured::validate_configured_models(
            self.custom_models.as_deref().unwrap_or_default(),
        )?;
        self.openrouter_vendor()?;
        if self
            .provider
            .as_deref()
            .is_some_and(is_legacy_antigravity_identity)
        {
            anyhow::bail!(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
        }
        match validate_kimi_code_api_model_id(
            active_provider,
            &self.active_route_base_url(),
            &self.default_model(),
        ) {
            Err(error) if error == KIMI_CODE_CLAUDE_ALIAS_GUIDANCE => {
                return Err(SafeConfigDiagnostic::KimiCodeClaudeAlias.into());
            }
            result => result.map_err(anyhow::Error::msg)?,
        }
        if let Some(features) = &self.features {
            for key in features.entries.keys() {
                if !is_known_feature_key(key) {
                    anyhow::bail!("Unknown feature flag: {key}");
                }
            }
        }
        // Validate the model against the *active provider's* name space, not
        // against DeepSeek's. `canonical_model_id_for_provider` is the
        // equal-treatment resolver: it applies each family's own canonical map
        // (GLM via Z.ai, Kimi, MiniMax, …) and passes unknown ids through, so
        // it rejects only what the provider genuinely cannot serve. Validating
        // with the DeepSeek-only `normalize_model_name` bricked every config
        // whose provider owns a non-DeepSeek family — including ones our own
        // setup wizard writes (`provider = "zai"`, `GLM-5.2`). (#4829)
        // Provider-scoped choices own the active route. A retained root
        // fallback can belong to a different provider after a saved switch.
        let configured_model = self
            .provider_config_string_with_runtime_fallback(&identity, |entry| entry.model.clone())
            .or_else(|| self.default_text_model.clone());
        if let Some(model) = configured_model.as_deref()
            && !model.trim().eq_ignore_ascii_case("auto")
            && !provider_passes_model_through(active_provider)
            && !self.active_provider_preserves_custom_base_url_model()
            && crate::provider_lake::configured_model_for_route(
                self,
                active_provider,
                identity.key.as_str(),
                &self.base_url_for_route(&identity),
                model,
            )
            .is_none()
            && canonical_model_id_for_provider(active_provider, model).is_none()
        {
            let provider = active_provider;
            let known = model_completion_names_for_provider(provider);
            let hint = if known.is_empty() {
                String::new()
            } else {
                format!(" (for example: {})", known.join(", "))
            };
            return Err(SafeConfigDiagnostic::invalid_value(
                "model",
                model,
                &format!(
                    "auto or a model ID provider '{}' serves{hint}",
                    provider.as_str()
                ),
                user_config_fix("model", "auto", Some("a *_MODEL environment variable")),
            )
            .into());
        }
        // One vocabulary with `codewhale config set`, which refuses the same
        // values before writing them (`codewhale_config::config_toml_choices`).
        for (key, value, replacement, env_var) in [
            (
                "approval_policy",
                self.approval_policy.as_deref(),
                "on-request",
                Some("CODEWHALE_APPROVAL_POLICY"),
            ),
            ("verbosity", self.verbosity.as_deref(), "normal", None),
            (
                "sandbox_mode",
                self.sandbox_mode.as_deref(),
                "workspace-write",
                Some("CODEWHALE_SANDBOX_MODE"),
            ),
        ] {
            let (Some(value), Some(choices)) = (value, codewhale_config::config_toml_choices(key))
            else {
                continue;
            };
            if !choices.contains(&value.trim().to_ascii_lowercase().as_str()) {
                let expected = match choices.split_last() {
                    Some((last, [])) => (*last).to_string(),
                    Some((last, [only])) => format!("{only} or {last}"),
                    Some((last, rest)) => format!("{}, or {last}", rest.join(", ")),
                    None => String::new(),
                };
                let displayed_value = codewhale_secrets::redact::redact_secrets(value);
                return Err(SafeConfigDiagnostic::invalid_value(
                    key,
                    &displayed_value,
                    &expected,
                    user_config_fix(key, replacement, env_var),
                )
                .into());
            }
        }
        if let Some(tui) = &self.tui
            && let Some(mode) = tui.alternate_screen.as_deref()
        {
            let mode = mode.to_ascii_lowercase();
            if !matches!(mode.as_str(), "auto" | "always" | "never") {
                return Err(SafeConfigDiagnostic::invalid_value(
                    "tui.alternate_screen",
                    &mode,
                    "auto, always, or never",
                    user_config_fix("tui.alternate_screen", "auto", None),
                )
                .into());
            }
        }
        if let Some(transcript) = &self.transcript
            && let Err(detail) = transcript.prose_measure_columns()
        {
            anyhow::bail!("Invalid transcript.prose_measure: {detail}.");
        }
        if let Some(auto_review) = &self.auto_review {
            auto_review.validate()?;
        }
        if let Some(providers) = &self.providers {
            providers.validate()?;
        }
        Ok(())
    }

    /// Resolved prose wrap cap from `[transcript] prose_measure` (#5436).
    ///
    /// `None` (absent or `0`) means prose uses the full content width,
    /// consistent with tool/status cells. Invalid values are rejected by
    /// [`Config::validate`], which every load path runs, so this resolver
    /// cannot fail here.
    #[must_use]
    pub fn prose_measure(&self) -> Option<u16> {
        self.transcript
            .as_ref()
            .and_then(|transcript| transcript.prose_measure_columns().ok().flatten())
    }

    #[must_use]
    /// Whether the live config uses the released route-sensitive Ollama Cloud
    /// shape. This is a pure in-memory compatibility check: no config or
    /// secret state is rewritten, and only the exact official `/v1` endpoint
    /// upgrades from `ollama` to `ollama-cloud`.
    fn selects_legacy_ollama_cloud_route(&self) -> bool {
        if self.migrated_legacy_ollama_cloud_route {
            return true;
        }
        if self.provider.as_deref().and_then(ProviderKind::parse) != Some(ProviderKind::Ollama) {
            return false;
        }
        self.legacy_ollama_cloud_route_configured()
    }

    /// Whether the legacy Ollama table itself names the exact hosted route,
    /// independent of which provider the parent session currently selects.
    /// Fleet and subagent pins need this route-scoped form.
    fn legacy_ollama_cloud_route_configured(&self) -> bool {
        let base_url = self
            .providers
            .as_ref()
            .and_then(|providers| providers.ollama.base_url.as_deref())
            .map(str::to_string)
            .or_else(|| first_nonempty_env(&["OLLAMA_BASE_URL"]));
        base_url.is_some_and(|base_url| {
            codewhale_config::provider::migrates_legacy_ollama_cloud_route(
                codewhale_config::ProviderKind::Ollama,
                &base_url,
            )
        })
    }

    /// Resolve the currently selected live route, keeping its exact id.
    pub(crate) fn active_provider_identity(&self) -> std::result::Result<ProviderIdentity, String> {
        let selected = self
            .provider
            .as_deref()
            .unwrap_or(ProviderKind::Deepseek.as_str());
        if self.migrated_legacy_ollama_cloud_route && selected == ProviderKind::OllamaCloud.as_str()
        {
            return self.resolve_persisted_provider_identity(
                Some(ProviderKind::OllamaCloud.as_str()),
                Some(ProviderKind::Ollama.as_str()),
            );
        }
        // Tombstones remain visible to inspection/clear; execution preflight
        // refuses the intrinsic retired kind before obtaining credentials.
        if is_legacy_antigravity_identity(selected) {
            return Ok(ProviderIdentity {
                provider: ProviderKind::Antigravity,
                key: ProviderId::from(ProviderKind::Antigravity.as_str()),
                exact_id: Some(ProviderId::from(ProviderKind::Antigravity.as_str())),
                migrated_legacy_ollama_cloud_route: false,
                legacy_root_custom_generation: None,
            });
        }
        if self.selects_literal_custom_provider()
            && self.current_legacy_root_custom_generation().is_some()
        {
            return self.resolve_persisted_provider_identity(Some("custom"), None);
        }
        self.resolve_provider_identity(selected)
    }

    /// Test fixtures use the same canonical resolver. This convenience never
    /// supplies an absent custom ID or reconstructs production admission.
    #[cfg(test)]
    pub(crate) fn test_identity_for_kind(&self, kind: ProviderKind) -> ProviderIdentity {
        if let Ok(active) = self.active_provider_identity()
            && active.provider == kind
        {
            return active;
        }
        self.builtin_provider_identity(kind)
            .expect("test fixture must declare an admitted provider identity")
    }

    /// Snapshot admitted identities in descriptor order, then exact custom-key order.
    /// This lists routes through the same resolver used by final dispatch; it
    /// neither activates them nor reads credentials.
    pub(crate) fn provider_identities(&self) -> Vec<ProviderIdentity> {
        let active = self.active_provider_identity().ok();
        let mut identities = Vec::new();
        for row in codewhale_config::descriptors::provider_compatibility() {
            if row.kind == ProviderKind::Custom {
                continue;
            }
            let identity = active
                .as_ref()
                .filter(|identity| identity.key.as_str() == row.id)
                .cloned()
                .or_else(|| self.resolve_provider_pin_identity(row.id).ok());
            if let Some(identity) = identity
                && identity.provider == row.kind
                && !identities
                    .iter()
                    .any(|existing: &ProviderIdentity| existing == &identity)
            {
                identities.push(identity);
            }
        }
        if let Some(active) = active
            && !identities.contains(&active)
        {
            identities.push(active);
        }
        if let Some(providers) = &self.providers {
            for key in providers.custom.keys() {
                if let Ok(identity) = self.resolve_provider_pin_identity(key)
                    && !identities
                        .iter()
                        .any(|existing| existing.key == identity.key)
                {
                    identities.push(identity);
                }
            }
        }
        identities
    }

    /// Preserve broken/manual selections in diagnostic rosters without admitting them.
    pub(crate) fn unadmitted_provider_keys(&self) -> Vec<&str> {
        let admitted = self.provider_identities();
        let mut keys = Vec::new();
        if let Some(providers) = &self.providers {
            keys.extend(providers.custom.keys().map(String::as_str).filter(|key| {
                !admitted
                    .iter()
                    .any(|identity| identity.key.as_str() == *key)
            }));
        }
        if let Some(key) = self.provider.as_deref()
            && self.active_provider_identity().is_err()
            && !keys.contains(&key)
        {
            keys.push(key);
        }
        keys.sort_unstable();
        keys
    }

    pub(crate) fn builtin_provider_identity(
        &self,
        kind: ProviderKind,
    ) -> std::result::Result<ProviderIdentity, String> {
        self.resolve_persisted_provider_identity(Some(kind.as_str()), Some(kind.as_str()))
    }

    /// Resolve a persisted provider key against the current live config.
    ///
    /// Named custom providers are exact and fail closed: a removed, renamed,
    /// or malformed table can never fall through to DeepSeek or whichever
    /// provider happens to be selected now. The literal legacy value `custom`
    /// remains loadable only for the old root-field config shape where the live
    /// provider is also literally `custom` and both `base_url` and
    /// `default_text_model` identify one valid route.
    pub(crate) fn resolve_provider_identity(
        &self,
        persisted: &str,
    ) -> std::result::Result<ProviderIdentity, String> {
        let key = persisted.trim();
        if key.is_empty() {
            return Err(
                "saved session has an empty provider identity; choose a valid session or repair its `metadata.model_provider` field"
                    .to_string(),
            );
        }

        let has_exact_custom_table = self
            .providers
            .as_ref()
            .and_then(|providers| providers.custom_provider_config(key))
            .is_some();

        if !has_exact_custom_table
            && let Some(row) = compatibility_for_selector(key)
            && row.kind != ProviderKind::Antigravity
            && row.kind != ProviderKind::Custom
        {
            let mut provider = row.kind;
            let migrated_legacy_ollama_cloud_route =
                provider == ProviderKind::Ollama && self.legacy_ollama_cloud_route_configured();
            if provider == ProviderKind::Ollama && migrated_legacy_ollama_cloud_route {
                provider = ProviderKind::OllamaCloud;
            }
            return Ok(ProviderIdentity {
                provider,
                key: ProviderId::from(if migrated_legacy_ollama_cloud_route {
                    provider.as_str()
                } else {
                    row.id
                }),
                exact_id: Some(if migrated_legacy_ollama_cloud_route {
                    ProviderId::from(ProviderKind::Ollama.as_str())
                } else {
                    ProviderId::from(row.id)
                }),
                migrated_legacy_ollama_cloud_route,
                legacy_root_custom_generation: None,
            });
        }

        if !has_exact_custom_table && key.eq_ignore_ascii_case(ProviderKind::Custom.as_str()) {
            if self.selects_literal_custom_provider() {
                // The literal `provider = "custom"` route lives in
                // `[providers.custom]`; parsing moved an older top-level
                // endpoint there (#6394). Without that table there is no
                // route to resume.
                return Err(
                    "`provider = \"custom\"` requires a `[providers.custom]` table with a `base_url`; Codewhale will not use the custom-provider placeholder or fall back"
                        .to_string(),
                );
            }

            // Pre-exact releases persisted every named custom route as the
            // generic literal `custom`. Migrate that record only when the live
            // config selects the sole valid named custom table; otherwise the
            // old value is genuinely ambiguous and must fail closed.
            if !self.selects_literal_custom_provider() {
                let selected = self.provider.as_deref().map(str::trim).unwrap_or_default();
                let valid_named = self
                    .providers
                    .as_ref()
                    .map(|providers| {
                        providers
                            .custom
                            .keys()
                            .filter(|name| {
                                !name.eq_ignore_ascii_case(ProviderKind::Custom.as_str())
                                    && ProviderKind::parse(name).is_none()
                                    && self.resolve_provider_identity(name).is_ok()
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if let [name] = valid_named.as_slice()
                    && selected == name
                {
                    return self.resolve_provider_identity(name);
                }
                return Err(format!(
                    "legacy session records only the generic `custom` provider kind, but the live config does not select exactly one valid named custom route (selected '{}', valid named routes: {}). Restore the original single `[providers.<name>]` route or repair the saved provider identity; Codewhale will not guess or fall back",
                    if selected.is_empty() {
                        "<unset>"
                    } else {
                        selected
                    },
                    valid_named.len()
                ));
            }
        }

        let exact_key = key;

        let entry = self
            .providers
            .as_ref()
            .and_then(|providers| providers.custom_provider_config(exact_key))
            .ok_or_else(|| {
                format!(
                    "saved session requires custom provider '{exact_key}', but `[providers.{exact_key}]` is missing from the live config. Restore that exact table and retry; Codewhale will not fall back"
                )
            })?;
        if !entry.is_openai_compatible_custom() {
            return Err(format!(
                "saved session requires custom provider '{exact_key}', but `[providers.{exact_key}]` must set `kind = \"openai-compatible\"`. Fix the live config and retry; Codewhale will not fall back"
            ));
        }
        let base_url = entry
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|base_url| !base_url.is_empty())
            .ok_or_else(|| {
                format!(
                    "saved session requires custom provider '{exact_key}', but `[providers.{exact_key}]` has no `base_url`. Fix the live config and retry; Codewhale will not fall back"
                )
            })?;
        let parsed = reqwest::Url::parse(base_url).map_err(|err| {
            format!(
                "saved session requires custom provider '{exact_key}', but `[providers.{exact_key}].base_url` is invalid: {err}. Fix the live config and retry; Codewhale will not fall back"
            )
        })?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(format!(
                "saved session requires custom provider '{exact_key}', but `[providers.{exact_key}].base_url` must be an http(s) URL with a host. Fix the live config and retry; Codewhale will not fall back"
            ));
        }

        Ok(ProviderIdentity {
            provider: ProviderKind::Custom,
            key: ProviderId::from(exact_key),
            exact_id: Some(ProviderId::from(exact_key)),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        })
    }

    /// Resolve a provider explicitly pinned by a current Fleet/subagent
    /// declaration.
    ///
    /// A scoped legacy Ollama Cloud config retains its migration marker so the
    /// active client can keep reading `[providers.ollama]` and the old secret
    /// slot. That marker is provenance for the active route, not an alias for a
    /// newly declared `ollama-cloud` pin: the explicit pin must bind the
    /// first-class table and credential slot even when it is declared by a
    /// child of the migrated route.
    pub(crate) fn resolve_provider_pin_identity(
        &self,
        provider_id: &str,
    ) -> std::result::Result<ProviderIdentity, String> {
        let mut identity = self.resolve_provider_identity(provider_id)?;
        if provider_id
            .trim()
            .eq_ignore_ascii_case(ProviderKind::Custom.as_str())
            && !identity
                .key
                .as_str()
                .eq_ignore_ascii_case(ProviderKind::Custom.as_str())
        {
            return Err(format!(
                "an explicit provider pin must name the configured provider '{}'; `custom` is not a wildcard for a named provider",
                identity.key
            ));
        }
        if identity.provider == ProviderKind::OllamaCloud
            && ProviderKind::parse(provider_id.trim()) == Some(ProviderKind::OllamaCloud)
        {
            identity.migrated_legacy_ollama_cloud_route = false;
        }
        Ok(identity)
    }

    /// Resolve a provider a user is selecting now (`codewhale config set
    /// provider <name>`). A name that is neither a built-in provider nor a
    /// configured table is a typo, not a saved session missing its route, so
    /// it gets config validation's wording instead of the resume wording.
    pub(crate) fn resolve_provider_selection_identity(
        &self,
        provider_id: &str,
    ) -> std::result::Result<ProviderIdentity, String> {
        let requested = provider_id.trim();
        if !requested.is_empty()
            && compatibility_for_selector(requested).is_none()
            && !requested.eq_ignore_ascii_case(ProviderKind::Custom.as_str())
            && self
                .providers
                .as_ref()
                .and_then(|providers| providers.custom_provider_config(requested))
                .is_none()
        {
            return Err(invalid_provider_message(requested));
        }
        if is_legacy_antigravity_identity(requested) {
            return Err("Antigravity is retired and cannot be selected".to_string());
        }
        self.resolve_provider_pin_identity(provider_id)
    }

    /// Resolve an additive exact provider id. Unlike raw selector resolution,
    /// an id means the record requires that exact `[providers.<id>]` table.
    fn resolve_exact_provider_identity(
        &self,
        persisted: &str,
    ) -> std::result::Result<ProviderIdentity, String> {
        let id = persisted.trim();
        if id.is_empty() {
            return Err(
                "persisted provider route has an empty exact provider id; Codewhale will not guess or fall back"
                    .to_string(),
            );
        }
        let has_exact_custom_table = self
            .providers
            .as_ref()
            .and_then(|providers| providers.custom_provider_config(id))
            .is_some();
        if id.eq_ignore_ascii_case(ProviderKind::Custom.as_str()) && !has_exact_custom_table {
            return Err(format!(
                "persisted provider route requires exact custom provider '{id}', but `[providers.{id}]` is missing from the live config. Restore that exact table and retry; Codewhale will not fall back"
            ));
        }

        let identity = self.resolve_provider_identity(id)?;
        if identity.provider == ProviderKind::Custom && identity.persisted_id() != Some(id) {
            return Err(format!(
                "persisted provider route requires exact custom provider '{id}', but the live config does not provide that exact table. Restore `[providers.{id}]` and retry; Codewhale will not fall back"
            ));
        }
        Ok(identity)
    }

    /// Resolve the two-field provider route written by current session/thread
    /// schemas without erasing which field supplied the identity.
    ///
    /// `provider_kind` is the generic wire/provider class (`custom` for every
    /// named OpenAI-compatible endpoint); `provider_id` is the additive exact
    /// configured key. Older records have no id and may have overloaded the
    /// kind field with an exact custom name. Keeping those cases distinct is
    /// security-sensitive: a legacy built-in record must never be captured by
    /// a later same-key custom table, while a current `custom` + exact-id pair
    /// must retain that user-owned table identity.
    pub(crate) fn resolve_persisted_provider_identity(
        &self,
        provider_kind: Option<&str>,
        provider_id: Option<&str>,
    ) -> std::result::Result<ProviderIdentity, String> {
        let kind = provider_kind
            .map(str::trim)
            .filter(|value| !value.is_empty());
        // Missing and malformed are different security states. An explicitly
        // persisted empty id must reach `resolve_exact_provider_identity` so
        // it fails closed instead of being reinterpreted as an id-less legacy
        // root route.
        let id = provider_id.map(str::trim);

        let Some(kind) = kind else {
            return id.map_or_else(
                || {
                    Err(
                        "persisted provider route has neither a provider kind nor an exact provider id; Codewhale will not guess or fall back"
                            .to_string(),
                    )
                },
                |id| self.resolve_exact_provider_identity(id),
            );
        };

        let source =
            compatibility_for_selector(kind).filter(|row| row.kind != ProviderKind::Antigravity);
        let Some(mut provider) = source.map(|row| row.kind) else {
            // Pre-additive releases sometimes wrote an exact named custom key
            // into `model_provider`. Preserve that shape, but reject a
            // contradictory additive id instead of silently choosing one.
            if let Some(id) = id
                && id != kind
            {
                return Err(format!(
                    "persisted provider route has legacy identity '{kind}' but exact provider id '{id}'; repair the mismatched fields because Codewhale will not guess or fall back"
                ));
            }
            return match id {
                Some(id) => self.resolve_exact_provider_identity(id),
                None => self.resolve_provider_identity(kind),
            };
        };
        let migrated_legacy_ollama_cloud = (provider == ProviderKind::Ollama
            && self.legacy_ollama_cloud_route_configured())
            || (provider == ProviderKind::OllamaCloud
                && id.and_then(ProviderKind::parse) == Some(ProviderKind::Ollama)
                && self.legacy_ollama_cloud_route_configured());
        if migrated_legacy_ollama_cloud {
            provider = ProviderKind::OllamaCloud;
        }

        if provider == ProviderKind::Custom {
            if let Some(id) = id {
                let identity = self.resolve_exact_provider_identity(id)?;
                if identity.provider != ProviderKind::Custom {
                    return Err(format!(
                        "persisted provider route declares generic kind 'custom' but exact provider id '{id}' resolves as built-in '{}'; use the matching built-in kind or restore `[providers.{id}]`. Codewhale will not guess or fall back",
                        identity.provider.as_str()
                    ));
                }
                return Ok(identity);
            }

            // A table is not evidence for a released id-less root record.
            // Only the root-scope canonicalizer's parse-bound receipt admits it.
            let generation = self.current_legacy_root_custom_generation().filter(|_| self.selects_literal_custom_provider()).ok_or_else(|| {
                "legacy id-less `custom` route requires the unchanged root-scope migrated `base_url`; a table-only, conflicting, changed, or profile-only route cannot supply that provenance. Repair the saved exact provider id; Codewhale will not guess or fall back".to_string()
            })?;
            let mut identity =
                self.resolve_exact_provider_identity(ProviderKind::Custom.as_str())?;
            identity.exact_id = None;
            identity.legacy_root_custom_generation = Some(generation);
            return Ok(identity);
        }

        let exact = id.and_then(compatibility_for_selector);
        if let Some(id) = id
            && (exact.is_none_or(|row| row.kind != provider)
                || source.is_some_and(|row| {
                    row.id != row.kind.as_str() && exact.is_some_and(|exact| exact.id != row.id)
                }))
            && !(migrated_legacy_ollama_cloud
                && ProviderKind::parse(id) == Some(ProviderKind::Ollama))
        {
            return Err(format!(
                "persisted provider route declares built-in kind '{}' but exact provider id '{id}' names a different route; repair the mismatched fields because Codewhale will not guess or fall back",
                provider.as_str()
            ));
        }

        let key = if migrated_legacy_ollama_cloud {
            provider.as_str()
        } else {
            exact.or(source).expect("parsed built-in compatibility").id
        };

        // Exact custom keys normally win raw string resolution. A persisted
        // built-in kind is stronger evidence than that raw key, but Config's
        // single selector cannot represent both routes simultaneously. Fail
        // closed instead of constructing a descriptor whose client would read
        // credentials/settings from the shadowing custom table.
        if self
            .providers
            .as_ref()
            .and_then(|providers| providers.custom_provider_config(key))
            .is_some()
        {
            return Err(format!(
                "persisted provider route requires built-in '{}', but an exact `[providers.{}]` custom route shadows the same selector. Rename the custom route or update the saved provider kind/id pair; Codewhale will not guess or fall back",
                provider.as_str(),
                provider.as_str()
            ));
        }

        Ok(ProviderIdentity {
            provider,
            key: ProviderId::from(key),
            exact_id: Some(if migrated_legacy_ollama_cloud {
                ProviderId::from(ProviderKind::Ollama.as_str())
            } else {
                ProviderId::from(key)
            }),
            migrated_legacy_ollama_cloud_route: migrated_legacy_ollama_cloud,
            legacy_root_custom_generation: None,
        })
    }

    /// Scope a runtime clone to one admitted identity; the parse-bound table
    /// remains present and every later table projection revalidates its receipt.
    pub(crate) fn scope_to_provider_identity(
        &mut self,
        identity: &ProviderIdentity,
    ) -> std::result::Result<(), String> {
        self.verify_provider_identity(identity)?;
        self.migrated_legacy_ollama_cloud_route = identity.migrated_legacy_ollama_cloud_route;
        self.provider = Some(identity.key.to_string());
        Ok(())
    }

    fn legacy_root_custom_table_generation(
        &self,
    ) -> Option<crate::route_receipt::CredentialGeneration> {
        let entry = self.providers.as_ref()?.custom.get("custom")?;
        let value = serde_json::to_value(entry).ok()?;
        Some(crate::route_receipt::CredentialGeneration::derive(
            "codewhale/legacy-root-custom-table/v1",
            &crate::client::canonical_json(&value),
        ))
    }

    fn bind_legacy_root_custom_generation(&mut self) {
        use codewhale_config::legacy_root::{LegacyRootField, LegacyRootNote};
        let moved = self.legacy_root.notes.iter().any(|note| {
            matches!(note,
                LegacyRootNote::Moved { scope: None, field: LegacyRootField::BaseUrl, to }
                | LegacyRootNote::Merged { scope: None, field: LegacyRootField::BaseUrl, to }
                if to == "providers.custom"
            )
        });
        let conflicting = self.legacy_root.notes.iter().any(|note| {
            matches!(
                note,
                LegacyRootNote::Conflict {
                    scope: None,
                    field: LegacyRootField::BaseUrl,
                    ..
                } | LegacyRootNote::DroppedEmpty {
                    scope: None,
                    field: LegacyRootField::BaseUrl
                }
            )
        });
        self.legacy_root_custom_generation =
            (moved && !conflicting && self.selects_literal_custom_provider())
                .then(|| self.legacy_root_custom_table_generation())
                .flatten();
    }

    fn current_legacy_root_custom_generation(
        &self,
    ) -> Option<crate::route_receipt::CredentialGeneration> {
        let captured = self.legacy_root_custom_generation.as_ref()?;
        let current = self.legacy_root_custom_table_generation()?;
        (captured == &current).then_some(current)
    }

    pub(crate) fn selects_literal_custom_provider(&self) -> bool {
        self.provider
            .as_deref()
            .map(str::trim)
            .is_some_and(|name| name.eq_ignore_ascii_case(ProviderKind::Custom.as_str()))
    }

    /// Whether `identity` names a custom route that this config can resolve.
    ///
    /// Only an exact `[providers.<name>]` custom table (the literal `custom`
    /// route included, since #6394). Anything else — an empty key, a
    /// removed table, a built-in provider name — is an unresolvable custom
    /// identity and endpoint resolution must fail closed on it.
    ///
    /// The predicate that pins that contract for the regression suite; the
    /// resolver itself fails closed without consulting it.
    #[cfg(test)]
    pub(crate) fn custom_identity_is_resolvable(&self, identity: &str) -> bool {
        self.custom_provider_entry_for_identity(identity).is_some()
    }

    /// Trimmed, non-empty `wire` dialect preference for `provider`'s config
    /// table (`[providers.<name>] wire = "responses" | "anthropic" | "chat"`).
    ///
    /// Single source for the client wire resolver and the capability reporter
    /// so the two cannot drift. `None` means "no preference" — the provider's
    /// static policy applies.
    pub(crate) fn provider_wire_dialect(&self, identity: &ProviderIdentity) -> Option<&str> {
        self.provider_config_for(identity)?
            .wire
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub(crate) fn verify_provider_identity(
        &self,
        identity: &ProviderIdentity,
    ) -> std::result::Result<(), String> {
        let admitted = self.resolve_persisted_provider_identity(
            Some(identity.persisted_kind()),
            identity.persisted_id(),
        )?;
        if admitted == *identity {
            Ok(())
        } else {
            Err("provider identity changed before projection".into())
        }
    }

    pub(crate) fn provider_config_for(
        &self,
        identity: &ProviderIdentity,
    ) -> Option<&ProviderConfig> {
        let providers = self.providers.as_ref()?;
        self.verify_provider_identity(identity).ok()?;
        if identity.provider == ProviderKind::Custom {
            return providers.custom_provider_config(identity.key.as_str());
        }
        let table = if identity.migrated_legacy_ollama_cloud_route {
            ProviderKind::Ollama.as_str()
        } else {
            identity.key.as_str()
        };
        codewhale_config::provider_config_table!(@read providers, table)
    }

    /// Resolve the pin from the selected provider only, before any request.
    pub(crate) fn openrouter_vendor(&self) -> Result<Option<String>> {
        let identity = self
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        let provider = identity.provider;

        let Some(vendor) = self
            .provider_config_for(&identity)
            .and_then(|entry| entry.vendor.as_deref())
        else {
            return Ok(None);
        };
        let vendor = codewhale_config::validate_openrouter_vendor(vendor)?;
        if vendor.is_some() && provider != ProviderKind::Openrouter {
            anyhow::bail!("vendor is only supported by providers.openrouter");
        }
        Ok(vendor.map(str::to_string))
    }

    pub(crate) fn subagent_provider_config(
        &self,
        identity: &ProviderIdentity,
    ) -> Option<&SubagentProviderConfig> {
        let providers = self.subagents.as_ref()?.providers.as_ref()?;
        providers.iter().find_map(|(key, config)| {
            let matches = if identity.provider == ProviderKind::Custom {
                key == identity.key.as_str()
            } else if let Some(row) = identity.compatibility() {
                let normalized = normalize_subagent_provider_key(key);
                compatibility_for_selector(key).is_some_and(|candidate| candidate.id == row.id)
                    || normalized == normalize_subagent_provider_key(row.id)
                    || row.subagent_aliases.contains(&normalized.as_str())
            } else {
                false
            };
            matches.then_some(config)
        })
    }

    pub(crate) fn provider_config_for_mut(
        &mut self,
        identity: &ProviderIdentity,
    ) -> Result<&mut ProviderConfig> {
        self.verify_provider_identity(identity)
            .map_err(anyhow::Error::msg)?;
        let providers = self.providers.get_or_insert_with(ProvidersConfig::default);
        if identity.provider == ProviderKind::Custom {
            return providers
                .custom
                .get_mut(identity.key.as_str())
                .context("exact custom provider table missing before mutation");
        }
        let table = if identity.migrated_legacy_ollama_cloud_route {
            ProviderKind::Ollama.as_str()
        } else {
            identity.key.as_str()
        };
        codewhale_config::provider_config_table!(@write providers, table)
            .context("provider has no generated typed config table")
    }

    /// Apply a runtime model override to the route's own table.
    pub(crate) fn set_provider_model_override(
        &mut self,
        identity: &ProviderIdentity,
        model: Option<String>,
    ) -> Result<()> {
        self.provider_config_for_mut(identity)?.model = model;
        // A deliberate model-only preparation keeps the canonicalizer's root
        // origin, but yields a new captured generation. Old snapshots refuse
        // the changed content. Endpoint/auth mutations never rebind this proof.
        if identity.legacy_root_custom_generation.is_some() {
            self.legacy_root_custom_generation = self.legacy_root_custom_table_generation();
        }
        Ok(())
    }

    /// Apply a runtime endpoint override to the route's own table.
    pub(crate) fn set_provider_base_url_override(
        &mut self,
        identity: &ProviderIdentity,
        base_url: Option<String>,
    ) -> Result<()> {
        self.provider_config_for_mut(identity)?.base_url = base_url;
        Ok(())
    }

    /// Apply an in-memory credential update to the route's own table.
    pub(crate) fn set_provider_api_key_override(
        &mut self,
        identity: &ProviderIdentity,
        api_key: Option<String>,
    ) -> Result<()> {
        self.provider_config_for_mut(identity)?.api_key = api_key;
        Ok(())
    }

    /// Mirror a successful native xAI login into the live route config.
    /// Codewhale-owned OAuth storage supersedes any dormant Grok CLI consent.
    pub(crate) fn mark_codewhale_owned_xai_oauth(&mut self, generation: String) -> Result<()> {
        let identity = self
            .builtin_provider_identity(ProviderKind::Xai)
            .map_err(anyhow::Error::msg)?;
        let entry = self.provider_config_for_mut(&identity)?;
        entry.auth_mode = Some("oauth".to_string());
        entry.oauth_credential_generation = Some(generation);
        entry.external_credentials = None;
        Ok(())
    }

    /// Mirror a successful native ChatGPT PKCE login into the live Codex route.
    /// Codewhale-owned OAuth storage supersedes any dormant Codex CLI consent.
    pub(crate) fn mark_codewhale_owned_chatgpt_oauth(&mut self, generation: String) -> Result<()> {
        let identity = self
            .builtin_provider_identity(ProviderKind::OpenaiCodex)
            .map_err(anyhow::Error::msg)?;
        let entry = self.provider_config_for_mut(&identity)?;
        entry.auth_mode = Some("oauth".to_string());
        entry.oauth_credential_generation = Some(generation);
        entry.external_credentials = None;
        Ok(())
    }

    pub(crate) fn clear_codewhale_owned_chatgpt_oauth(&mut self) -> Result<()> {
        let identity = self
            .builtin_provider_identity(ProviderKind::OpenaiCodex)
            .map_err(anyhow::Error::msg)?;
        let entry = self.provider_config_for_mut(&identity)?;
        if entry
            .oauth_credential_generation
            .as_deref()
            .is_some_and(codewhale_config::is_valid_chatgpt_oauth_generation)
        {
            entry.oauth_credential_generation = None;
            if entry.auth_mode.as_deref() == Some("oauth") {
                entry.auth_mode = None;
            }
        }
        Ok(())
    }

    /// Refresh only model-provider route material from a newly loaded disk
    /// snapshot. The receiver is the already-effective interactive Config,
    /// including CLI feature toggles and workspace/project permission overlays;
    /// replacing it wholesale during `/load` could silently loosen those
    /// controls. Provider tables carry their endpoint, auth, headers, TLS,
    /// model-passthrough, and per-route limits as one atomic registry.
    pub(crate) fn refresh_provider_routes_from(&mut self, fresh: &Self) {
        self.custom_models.clone_from(&fresh.custom_models);
        self.provider.clone_from(&fresh.provider);
        self.http_headers.clone_from(&fresh.http_headers);
        self.default_text_model
            .clone_from(&fresh.default_text_model);
        self.legacy_model.clone_from(&fresh.legacy_model);
        self.remembered_selection_scope = fresh.remembered_selection_scope;
        self.route_preferences_version = fresh.route_preferences_version;
        self.environment_model_applied = fresh.environment_model_applied;
        self.auth_mode.clone_from(&fresh.auth_mode);
        self.fallback_providers
            .clone_from(&fresh.fallback_providers);
        self.retry.clone_from(&fresh.retry);
        self.providers.clone_from(&fresh.providers);
        self.base_url_env_receipt
            .clone_from(&fresh.base_url_env_receipt);
        self.legacy_root.clone_from(&fresh.legacy_root);
        self.legacy_root_custom_generation
            .clone_from(&fresh.legacy_root_custom_generation);
        self.reasoning_effort_inferred_from_legacy_alias =
            fresh.reasoning_effort_inferred_from_legacy_alias;
        self.migrated_deepseek_model_alias
            .clone_from(&fresh.migrated_deepseek_model_alias);
    }

    /// Return the configured provider request concurrency cap.
    ///
    /// `None` means the client does not apply an extra in-flight request
    /// semaphore. Z.ai/GLM gets a conservative default because its SSE endpoint
    /// times out under sustained parallel stream opens well below the advertised
    /// service concurrency (#3496). Operators can raise it with
    /// `[providers.zai] max_concurrency = N`; `0` explicitly disables the
    /// client-side cap for that provider.
    #[must_use]
    pub fn provider_max_concurrency(&self, identity: &ProviderIdentity) -> Option<usize> {
        let provider = identity.provider;
        let configured = self
            .provider_config_for(identity)
            .and_then(|entry| entry.max_concurrency);
        match configured {
            Some(0) => None,
            Some(limit) => Some(limit.clamp(1, MAX_PROVIDER_REQUEST_CONCURRENCY)),
            None if provider == ProviderKind::Zai => Some(DEFAULT_ZAI_PROVIDER_MAX_CONCURRENCY),
            None => None,
        }
    }

    pub(crate) fn provider_config(&self) -> Option<&ProviderConfig> {
        let identity = self.active_provider_identity().ok()?;
        self.provider_config_for(&identity)
    }

    fn provider_config_string_with_runtime_fallback<F>(
        &self,
        identity: &ProviderIdentity,
        get: F,
    ) -> Option<String>
    where
        F: Fn(&ProviderConfig) -> Option<String>,
    {
        let provider = identity.provider;
        if let Some(value) = self.provider_config_for(identity).and_then(&get) {
            return Some(value);
        }
        if provider == ProviderKind::SiliconflowCN {
            return self
                .builtin_provider_identity(ProviderKind::Siliconflow)
                .ok()
                .as_ref()
                .and_then(|sibling| self.provider_config_for(sibling))
                .and_then(get);
        }
        None
    }

    /// [`Config::provider_config_string_with_runtime_fallback`] for a route's
    /// endpoint or key. DeepSeek-CN also reads `[providers.deepseek]`: the two
    /// identities used to share the top-level `base_url` / `api_key`, which
    /// now live there (#6394). Only these two fields are shared — never the
    /// model — and never a DeepSeek value the environment addressed to the
    /// DeepSeek identity alone.
    pub(crate) fn provider_route_string_with_deepseek_fallback<F>(
        &self,
        identity: &ProviderIdentity,
        get: F,
    ) -> Option<String>
    where
        F: Fn(&ProviderConfig) -> Option<String>,
    {
        if let Some(value) = self.provider_config_string_with_runtime_fallback(identity, &get) {
            return Some(value);
        }
        if identity.key.as_str() == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id
            && !matches!(
                &self.base_url_env_receipt,
                BaseUrlEnvReceipt::Route(ProviderKind::Deepseek, key) if key == ProviderKind::Deepseek.as_str()
            )
        {
            return self
                .builtin_provider_identity(ProviderKind::Deepseek)
                .ok()
                .as_ref()
                .and_then(|sibling| self.provider_config_for(sibling))
                .and_then(get)
                .filter(|value| !value.trim().is_empty());
        }
        None
    }

    #[must_use]
    pub fn insecure_skip_tls_verify(&self) -> bool {
        self.provider_config()
            .and_then(|provider| provider.insecure_skip_tls_verify)
            .unwrap_or(false)
    }

    /// Per-provider consent to a plain-HTTP `base_url` (#5991). Loopback is
    /// always allowed without it; this covers named LAN/internal hosts the
    /// user explicitly trusts. Narrower than the env-var override, which
    /// applies to every provider in the process.
    #[must_use]
    pub fn allow_insecure_http(&self) -> bool {
        self.provider_config()
            .and_then(|provider| provider.allow_insecure_http)
            .unwrap_or(false)
    }

    #[must_use]
    pub(crate) fn context_window_for_provider_config(
        &self,
        identity: &ProviderIdentity,
    ) -> Option<u32> {
        let provider = identity.provider;
        if let Some(window) = self
            .provider_config_for(identity)
            .and_then(|entry| entry.context_window)
            .filter(|window| *window > 0)
        {
            return Some(window);
        }
        if provider == ProviderKind::SiliconflowCN {
            return self
                .builtin_provider_identity(ProviderKind::Siliconflow)
                .ok()
                .as_ref()
                .and_then(|sibling| self.provider_config_for(sibling))
                .and_then(|entry| entry.context_window)
                .filter(|window| *window > 0);
        }
        None
    }

    /// `[providers.<id>.model_context_windows]` for this provider, keyed by
    /// exact wire model id (#6108). Follows the same SiliconFlow CN sibling
    /// fallback as [`Self::context_window_for_provider_config`].
    #[must_use]
    pub(crate) fn model_context_windows_for(
        &self,
        identity: &ProviderIdentity,
    ) -> Option<&std::collections::BTreeMap<String, u32>> {
        let provider = identity.provider;
        let table = self
            .provider_config_for(identity)
            .and_then(|entry| entry.model_context_windows.as_ref())
            .filter(|table| !table.is_empty());
        if table.is_some() {
            return table;
        }
        if provider == ProviderKind::SiliconflowCN {
            return self
                .builtin_provider_identity(ProviderKind::Siliconflow)
                .ok()
                .as_ref()
                .and_then(|sibling| self.provider_config_for(sibling))
                .and_then(|entry| entry.model_context_windows.as_ref())
                .filter(|table| !table.is_empty());
        }
        None
    }

    #[must_use]
    pub fn http_headers(&self) -> HashMap<String, String> {
        let Ok(identity) = self.active_provider_identity() else {
            return HashMap::new();
        };
        let mut headers = self.http_headers.clone().unwrap_or_default();
        if let Some(provider_headers) = self
            .provider_config_for(&identity)
            .and_then(|provider| provider.http_headers.as_ref())
        {
            headers.extend(provider_headers.clone());
        }
        headers.retain(|name, value| !name.trim().is_empty() && !value.trim().is_empty());
        if auth_mode_disables_api_key(self.auth_mode_for_provider(&identity).as_deref()) {
            headers.retain(|name, _| !codewhale_config::is_upstream_auth_header(name));
        }
        headers
    }

    fn active_configured_model_id(&self) -> Option<&str> {
        let identity = self.active_provider_identity().ok()?;
        self.provider_config_for(&identity)
            .and_then(|entry| entry.model.as_deref())
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .or_else(|| {
                self.default_text_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
            })
    }

    /// Describe a first-party DeepSeek alias that was migrated for the active
    /// route. Custom endpoints retain ownership of the same model strings and
    /// must not receive DeepSeek's deprecation claim.
    pub(crate) fn active_deepseek_alias_deprecation(&self) -> Option<ModelAliasDeprecation> {
        let identity = self.active_provider_identity().ok()?;
        let provider = identity.provider;

        if !matches!(
            provider,
            ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
        ) {
            return None;
        }

        let alias = self
            .migrated_deepseek_model_alias
            .as_deref()
            .or_else(|| self.active_configured_model_id())?
            .trim()
            .to_ascii_lowercase();
        // A custom endpoint owns the same model strings, so DeepSeek's
        // retirement claim must not travel to it. Ask that question directly:
        // the previous proxy for it — "the wire id was rewritten" — also
        // excluded every id DeepSeek retires while still accepting, which is
        // exactly the V4 Pro case.
        let base_url = self.active_route_base_url();
        if base_url_is_custom_for_provider(provider, &base_url) {
            return None;
        }

        deepseek_alias_deprecation(&alias)
    }

    /// The root `default_text_model` alias is the *active* route's fallback.
    /// When a switch to `incoming` leaves a route that has no model leaf of its
    /// own and was actually resolving the alias, return that outgoing identity
    /// and value: the choice belongs to the outgoing route and must follow it
    /// onto its own leaf, rather than stay at the root where the incoming route
    /// would inherit it and the outgoing route would forget it (falling back to
    /// its catalog default on the way back). The alias is `default_text_model`
    /// or, when that is unset, the legacy root `model` it falls back to. A
    /// persisted writer that moves it clears both roots
    /// (`config_persistence::unset_root_model_aliases`); clearing only the
    /// first would resurrect the second on the incoming route after reload.
    pub(crate) fn root_model_alias_owned_by_outgoing(
        &self,
        incoming: &ProviderIdentity,
    ) -> Option<(ProviderIdentity, String)> {
        // `default_model` reads the legacy root `model` whenever
        // `default_text_model` is unset, so the effective alias is either one.
        let value = self
            .default_text_model
            .as_deref()
            .or(self.legacy_model.as_deref())?
            .trim();
        if value.is_empty()
            || value.eq_ignore_ascii_case("auto")
            || value.chars().any(char::is_control)
        {
            return None;
        }
        let outgoing = self.active_provider_identity().ok()?;
        // An unnamed custom route stores its model in the alias itself.
        if outgoing == *incoming
            || (outgoing.provider == ProviderKind::Custom && outgoing.persisted_id().is_none())
        {
            return None;
        }
        let mut scoped = self.clone();
        scoped.scope_to_provider_identity(&outgoing).ok()?;
        if scoped
            .provider_config_for(&outgoing)
            .and_then(|entry| entry.model.as_deref())
            .is_some()
        {
            return None;
        }
        (scoped.default_model() == value).then(|| (outgoing, value.to_string()))
    }

    #[must_use]
    pub fn default_model(&self) -> String {
        let Ok(identity) = self.active_provider_identity() else {
            return String::new();
        };
        let provider = identity.provider;
        if self.default_text_model.is_none() && self.legacy_model.is_some() {
            let mut config = self.clone();
            config.default_text_model.clone_from(&self.legacy_model);
            return config.default_model();
        }

        let declared = |model: &str| {
            crate::provider_lake::configured_model_for_route(
                self,
                provider,
                identity.key.as_str(),
                &self.active_route_base_url(),
                model,
            )
            .is_some()
        };
        if let Some(model) = self
            .provider_config_string_with_runtime_fallback(&identity, |entry| entry.model.clone())
        {
            let model = model.trim();
            // Automatic selection is a saved choice on every route, including
            // DeepSeek. Resolve it before provider model-name normalization.
            if model.eq_ignore_ascii_case("auto") {
                return "auto".to_string();
            }
            if declared(model)
                || provider_passes_model_through(provider)
                || self.active_provider_preserves_custom_base_url_model()
            {
                return model.to_string();
            }
            if let Some(normalized) = normalize_model_for_provider(provider, model) {
                return normalized;
            }
            // An explicit provider-scoped model that is not a recognized
            // DeepSeek alias is a deliberate custom choice for a non-DeepSeek
            // provider (e.g. `MiniMax-M2.7` on an OpenAI-compatible endpoint).
            // It must pass through verbatim rather than fall back to a
            // DeepSeek/provider default (issue #1714).
            if !matches!(provider, ProviderKind::Deepseek) && !model.is_empty() {
                return model.to_string();
            }
        }
        if let Some(model) = self
            .default_text_model
            .as_deref()
            .filter(|model| declared(model))
        {
            return model.to_string();
        }
        let moonshot_config = (provider == ProviderKind::Moonshot)
            .then(|| self.provider_config())
            .flatten();
        let moonshot_uses_kimi_code = moonshot_config.is_some_and(|config| {
            provider_config_uses_kimi_imported_token(config)
                || config
                    .base_url
                    .as_deref()
                    .is_some_and(moonshot_base_url_uses_kimi_code)
        });
        if moonshot_uses_kimi_code {
            return DEFAULT_KIMI_CODE_MODEL.to_string();
        }
        if let Some(model) = self.default_text_model.as_deref()
            && model.trim().eq_ignore_ascii_case("auto")
        {
            return "auto".to_string();
        }
        // A root DeepSeek-family default must not leak onto a vendor-locked
        // official endpoint that can never serve it (the provider then
        // rejects every request, e.g. `deepseek-v4-pro` on api.x.ai). Custom
        // base URLs keep full pass-through: a compatible proxy may
        // legitimately serve any model id.
        let foreign_root_default = |model: &str| {
            !self.active_provider_preserves_custom_base_url_model()
                && matches!(
                    provider,
                    ProviderKind::Xai | ProviderKind::Openai | ProviderKind::Moonshot
                )
                && normalize_model_name(model).is_some()
        };
        // Xiaomi MiMo: honour a root `default_text_model` that names a MiMo id
        // (canonical aliases or a custom account id). Do not silently drop it
        // for the provider seed default.
        if provider == ProviderKind::XiaomiMimo
            && let Some(model) = self.default_text_model.as_deref()
        {
            if let Some(canonical) = canonical_xiaomi_mimo_model_id(model) {
                return canonical.to_string();
            }
            // Non-empty root value that is not a known foreign DeepSeek id is
            // a deliberate custom MiMo choice — apply it. A stale DeepSeek id
            // still falls through to the provider default below rather than
            // being forwarded to Xiaomi's endpoint.
            let trimmed = model.trim();
            if !trimmed.is_empty() && normalize_model_name(trimmed).is_none() {
                return trimmed.to_string();
            }
        }
        if let Some(model) = self.default_text_model.as_deref()
            && (provider_passes_model_through(provider)
                || self.active_provider_preserves_custom_base_url_model())
            && !foreign_root_default(model)
            // Xiaomi was handled above so a stale DeepSeek root id does not
            // pass through merely because the provider is pass-through.
            && provider != ProviderKind::XiaomiMimo
        {
            return model.trim().to_string();
        }
        if let Some(model) = self.default_text_model.as_deref()
            && provider != ProviderKind::XiaomiMimo
            && !root_deepseek_model_is_foreign_to_direct_provider(provider, model)
            && let Some(normalized) = normalize_model_name_for_provider(provider, model)
            // A wire-slug translation (e.g. the Moonshot map) resolves the
            // foreign default to a native model; an identity result does not.
            && (!foreign_root_default(model) || !normalized.eq_ignore_ascii_case(model.trim()))
        {
            return normalized;
        }

        if let Some((model, _)) = codewhale_config::cloud_facts::cloud_default_model_for_route(
            provider,
            &self.base_url_for_route(&identity),
        ) {
            return model;
        }

        // The account roster still owns a live Codex preference. All static
        // seeds come from the shared descriptor; no provider default table here.
        if provider == ProviderKind::OpenaiCodex
            && let Some(preferred) =
                crate::codex_model_cache::model_roster_for(self).preferred_model_id()
        {
            return preferred.to_string();
        }
        identity
            .compatibility()
            .map_or("", |row| row.default_model)
            .to_string()
    }

    /// Return the configured API base URL (normalized) for the selected route.
    #[must_use]
    pub fn active_route_base_url(&self) -> String {
        self.active_provider_identity()
            .map(|identity| self.base_url_for_route(&identity))
            .unwrap_or_default()
    }

    /// Resolve `provider`'s endpoint from the layers that provider actually
    /// owns, in precedence order:
    ///
    /// 1. its own `[providers.<table>]` entry (including in-memory runtime
    ///    overrides; DeepSeek-CN also reads `[providers.deepseek]`). A legacy
    ///    top-level `base_url` was moved into its owner's table on parse
    ///    (#6394);
    /// 2. its provider-specific environment contract (`MOONSHOT_BASE_URL`,
    ///    `OPENAI_BASE_URL`, ...), which names exactly one provider and is
    ///    therefore sound to read for a route that is not the session's;
    /// 3. the generic `CODEWHALE_BASE_URL` / `DEEPSEEK_BASE_URL` override, but
    ///    only when this config is still the route that override selected;
    /// 4. the provider's canonical default endpoint.
    ///
    /// Step 3 is why this is identity-aware instead of a bare env read.
    /// `CODEWHALE_BASE_URL` is documented as "base URL for the active
    /// provider", and [`apply_env_overrides`] writes it onto exactly one
    /// provider entry. Every cross-provider construction seam — a pinned
    /// subagent/fleet child, the per-turn auto-router, tool routing, a picker
    /// preview — works by cloning the session config and re-pointing
    /// `provider`, so without the ownership check a Moonshot/Z.ai/MiniMax
    /// child in a DeepSeek session would silently inherit the DeepSeek host
    /// and dispatch a pinned model to the wrong vendor.
    pub(crate) fn base_url_for_route(&self, identity: &ProviderIdentity) -> String {
        if self.verify_provider_identity(identity).is_err() {
            return String::new();
        }
        let provider = identity.provider;
        let provider_base = if provider == ProviderKind::Custom {
            self.provider_config_for(identity)
                .and_then(|entry| entry.base_url.clone())
        } else {
            self.provider_route_string_with_deepseek_fallback(identity, |entry| {
                entry.base_url.clone()
            })
        };
        // A provider-scoped endpoint variable names exactly one provider, so it
        // resolves for the selected identity whether or not that identity is
        // the session route. `apply_env_overrides` only merges these into the
        // active provider's table, which is why a non-active route has to read
        // them here instead of relying on the merged config.
        let configured_base_url =
            provider_base.or_else(|| provider_env_base_url_override(provider));
        let entry = self.provider_config_for(identity);
        let mode = entry.and_then(|e| e.mode.as_deref());
        let wire = entry.and_then(|e| e.wire.as_deref());
        let base = if provider == ProviderKind::XiaomiMimo {
            let config_api_key = entry.and_then(|e| e.api_key.as_deref()).filter(|value| {
                classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal
            });
            let env_api_key =
                xiaomi_mimo_env_api_key_for_runtime(mode, configured_base_url.as_deref());
            let api_key = config_api_key.or(env_api_key.as_deref());
            resolve_xiaomi_mimo_base_url(configured_base_url, api_key, mode)
        } else if matches!(
            provider,
            ProviderKind::ModelstudioTokenPlan
                | ProviderKind::ModelstudioTokenPlanAnthropic
                | ProviderKind::ModelstudioCodingPlan
                | ProviderKind::ModelstudioCodingPlanAnthropic
        ) {
            resolve_modelstudio_base_url_for_tui(configured_base_url, provider, mode, wire)
        } else if matches!(
            provider,
            ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
        ) {
            resolve_minimax_base_url_for_tui(configured_base_url, provider, wire)
        } else if matches!(
            provider,
            ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
        ) {
            resolve_deepseek_base_url_for_tui(configured_base_url, provider, wire)
        } else {
            configured_base_url
                .or_else(|| self.route_owned_generic_env_base_url(identity))
                .unwrap_or_else(|| {
                    // Membership-token routing is an execution fact, not a
                    // provider metadata default. All other defaults use the
                    // same descriptor-backed facade as every caller.
                    if provider == ProviderKind::Moonshot
                        && self
                            .provider_config_for(identity)
                            .is_some_and(provider_config_uses_kimi_imported_token)
                    {
                        DEFAULT_KIMI_CODE_BASE_URL
                    } else {
                        identity.compatibility().map_or("", |row| row.base_url)
                    }
                    .to_string()
                })
        };
        normalize_base_url(&base)
    }

    /// The generic `CODEWHALE_BASE_URL` / `DEEPSEEK_BASE_URL` override, but
    /// only for the route that override actually selected.
    ///
    /// [`apply_env_overrides`] records the owning `(provider, identity)` in
    /// [`Config::base_url_env_receipt`] at load time and writes the value onto
    /// that provider's own entry. A config later re-pointed at another identity
    /// is a different route: it must fall through to that provider's own
    /// default rather than borrow the session host.
    fn route_owned_generic_env_base_url(&self, identity: &ProviderIdentity) -> Option<String> {
        match &self.base_url_env_receipt {
            BaseUrlEnvReceipt::Unrecorded => env_base_url_override(),
            BaseUrlEnvReceipt::NoOwner => None,
            BaseUrlEnvReceipt::Route(..) => self
                .base_url_env_receipt
                .owns(identity.provider, identity.key.as_str())
                .then(env_base_url_override)
                .flatten(),
        }
    }

    /// Resolve a named custom provider's table by explicit identity.
    ///
    /// Fails closed: an empty identity, or one that names no
    /// `[providers.<name>]` custom table, resolves to nothing instead of
    /// falling back to whichever custom route the session is currently on.
    #[cfg(test)]
    fn custom_provider_entry_for_identity(&self, identity: &str) -> Option<&ProviderConfig> {
        let key = identity.trim();
        if key.is_empty() {
            return None;
        }
        self.providers.as_ref()?.custom_provider_config(key)
    }

    fn active_provider_preserves_custom_base_url_model(&self) -> bool {
        self.active_provider_identity()
            .is_ok_and(|identity| self.provider_uses_custom_endpoint(&identity))
    }

    /// Whether `provider`'s effective endpoint is a custom host rather than its
    /// shipped one. Resolved through the same identity-aware resolver the
    /// client is built from, so this predicate cannot disagree with the URL the
    /// request will actually be sent to.
    pub(crate) fn provider_uses_custom_endpoint(&self, identity: &ProviderIdentity) -> bool {
        let provider = identity.provider;
        provider_preserves_custom_base_url_model(provider, &self.base_url_for_route(identity))
    }

    /// Whether file-owned credential slots are bound to `provider`'s
    /// effective endpoint.
    ///
    /// The environment can replace the active route's base URL after config
    /// parsing. In that case, a root/provider `api_key` or configured
    /// `api_key_env` still belongs to the file-owned endpoint and must not
    /// follow a newly selected custom host. An explicit source-marked CLI key
    /// remains a deliberate endpoint override and is handled before this
    /// predicate by the runtime resolver.
    pub(crate) fn config_credentials_are_bound_to_provider_endpoint(
        &self,
        identity: &ProviderIdentity,
    ) -> bool {
        !self
            .active_provider_identity()
            .is_ok_and(|active| active == *identity)
            || !self.active_base_url_is_environment_owned(identity)
            || !self.provider_uses_custom_endpoint(identity)
    }

    fn active_base_url_is_environment_owned(&self, identity: &ProviderIdentity) -> bool {
        let provider = identity.provider;
        if !self
            .active_provider_identity()
            .is_ok_and(|active| active == *identity)
        {
            return false;
        }
        let route_key = identity.key.as_str();
        if self.base_url_env_receipt.owns(provider, route_key) {
            return true;
        }

        // Below the receipt, the environment can still supply the endpoint for
        // a route that has none of its own. A provider-scoped variable names
        // exactly one provider, so it always owns that route's endpoint. The
        // generic variable only does so while no receipt has said otherwise —
        // once a receipt exists and does not name this route,
        // `route_owned_generic_env_base_url` refuses it, so claiming env
        // ownership here would contradict the URL actually resolved.
        if self.configured_base_url_for_provider(identity).is_some() {
            return false;
        }
        provider_env_base_url_override(provider).is_some()
            || (matches!(self.base_url_env_receipt, BaseUrlEnvReceipt::Unrecorded)
                && env_base_url_override().is_some())
    }

    /// The active route names its own endpoint: its `[providers.<name>]`
    /// `base_url` (where a legacy top-level `base_url` lands too) or an
    /// environment endpoint override. An endpoint is a configured route even
    /// without a model or a working key.
    pub(crate) fn active_route_endpoint_configured(&self) -> bool {
        self.active_provider_identity().is_ok_and(|identity| {
            self.configured_base_url_for_provider(&identity).is_some()
                || self.active_base_url_is_environment_owned(&identity)
        })
    }

    /// The endpoint `provider` owns through a file or in-memory layer, before
    /// the environment layer is consulted: its own `[providers.<name>]` table
    /// (DeepSeek-CN also reading `[providers.deepseek]`). There is no
    /// top-level endpoint any more (#6394).
    fn configured_base_url_for_provider(&self, identity: &ProviderIdentity) -> Option<String> {
        self.provider_route_string_with_deepseek_fallback(identity, |entry| entry.base_url.clone())
            .filter(|base| !base.trim().is_empty())
    }

    /// Whether model ids for `provider` belong to the configured endpoint.
    ///
    /// Every route — active or pinned — is judged on the endpoint it will
    /// actually be dispatched to, so a pinned child cannot canonicalize model
    /// ids for a host that owns its own namespace (or pass through ids on a
    /// route that resolves to a canonical endpoint). The resolver behind
    /// [`Config::provider_uses_custom_endpoint`] is identity-aware, so this no
    /// longer risks attributing the session's endpoint to another provider.
    pub(crate) fn model_ids_pass_through_for_provider(&self, identity: &ProviderIdentity) -> bool {
        let provider = identity.provider;
        provider_passes_model_through(provider) || self.provider_uses_custom_endpoint(identity)
    }

    pub(crate) fn model_ids_pass_through(&self) -> bool {
        self.active_provider_identity()
            .is_ok_and(|identity| self.model_ids_pass_through_for_provider(&identity))
    }

    pub(crate) fn auth_mode_for_provider(&self, identity: &ProviderIdentity) -> Option<String> {
        self.provider_config_string_with_runtime_fallback(identity, |entry| entry.auth_mode.clone())
            .or_else(|| {
                (self
                    .active_provider_identity()
                    .is_ok_and(|active| active == *identity))
                .then(|| self.auth_mode.clone())
                .flatten()
            })
    }

    /// Mint a read capability for the exact external credential path selected
    /// when consent was granted.
    ///
    /// Path resolution itself is side-effect free. The returned capability is
    /// required by every external credential adapter before it may stat or
    /// read the selected file. `suggested_path` is used only in disabled-mode
    /// guidance; an existing grant remains pinned to its persisted path even
    /// if ambient CLI-home environment variables change later.
    pub(crate) fn external_credential_read_grant(
        &self,
        identity: &ProviderIdentity,
        source: codewhale_config::ExternalCredentialSource,
        suggested_path: &Path,
    ) -> Result<codewhale_config::ExternalCredentialReadGrant> {
        anyhow::ensure!(
            identity.key.as_str() != codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id,
            "external credentials are unsupported for the legacy DeepSeek China route"
        );
        let provider = identity.provider;
        if !self
            .active_provider_identity()
            .is_ok_and(|active| active == *identity)
        {
            anyhow::bail!(
                "external credential access for {} is dormant until that provider is explicitly selected",
                provider.provider().display_name()
            );
        }
        let kind = provider;
        let consent = self
            .provider_config_for(identity)
            .and_then(|entry| entry.external_credentials.as_ref())
            .with_context(|| {
                format!(
                    "External credentials owned by {} are disabled for {}. To allow read-only access to this exact file, run:\n  codewhale auth external-consent --provider {} --mode read-only --path {}",
                    source.as_str(),
                    provider.provider().display_name(),
                    kind.as_str(),
                    codewhale_config::quote_os_path(suggested_path)
                )
            })?;
        consent
            .read_grant(kind, source, &consent.path)
            .map_err(|error| {
                anyhow::anyhow!(
                    "external credential consent for {}: {error}",
                    provider.provider().display_name()
                )
            })
    }

    /// Whether a structurally valid read-only consent record exists for an
    /// external credential source. This never stats or reads the selected
    /// file and never mints the capability required to do so.
    pub(crate) fn external_credential_read_consent_configured(
        &self,
        identity: &ProviderIdentity,
        source: codewhale_config::ExternalCredentialSource,
    ) -> bool {
        if identity.key.as_str() == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id {
            return false;
        }
        let provider = identity.provider;
        let kind = provider;
        let Some(consent) = self
            .provider_config_for(identity)
            .and_then(|entry| entry.external_credentials.as_ref())
        else {
            return false;
        };
        consent
            .validate_read_scope(kind, source, &consent.path)
            .is_ok()
    }

    pub(crate) fn should_skip_secret_store_for_provider(
        &self,
        identity: &ProviderIdentity,
    ) -> bool {
        if self.verify_provider_identity(identity).is_err() {
            return true;
        }
        let provider = identity.provider;
        // The CLI's durable credential namespace has one compatibility slot
        // named `custom`; it cannot identify an arbitrary named custom route.
        // Reusing that slot for `[providers.<name>]` could send endpoint A's
        // bearer token to endpoint B. Named routes therefore resolve only
        // their own config/auth/api_key_env sources. The generic slot remains
        // valid solely for the literal `custom` route (whose older top-level
        // endpoint now lives in `[providers.custom]`, #6394).
        if provider == ProviderKind::Custom
            && !identity
                .key
                .as_str()
                .eq_ignore_ascii_case(ProviderKind::Custom.as_str())
        {
            return true;
        }

        let auth_mode = self.auth_mode_for_provider(identity);
        if auth_mode_disables_api_key(auth_mode.as_deref()) {
            return true;
        }
        if self.provider_uses_custom_endpoint(identity) {
            // An explicitly authenticated loopback runtime may intentionally
            // use the durable provider slot (for example a protected local
            // vLLM server). Remote custom endpoints must never inherit an
            // official provider's saved credential.
            let explicitly_authenticated_loopback = self
                .active_provider_identity()
                .is_ok_and(|active| active == *identity)
                && auth_mode_requires_api_key(auth_mode.as_deref())
                && base_url_uses_local_host(&self.active_route_base_url());
            if !explicitly_authenticated_loopback {
                return true;
            }
        }
        if auth_mode_requires_api_key(auth_mode.as_deref()) {
            return false;
        }

        // The Codewhale API is authenticated on every origin it is allowed to
        // reach. A loopback `CODEWHALE_API_BASE` is a test origin for that
        // same contract, not a keyless local runtime, so it must not suppress
        // the route's saved or exported key.
        if provider == ProviderKind::Codewhale {
            return false;
        }

        provider_route_is_keyless_self_hosted(provider, &self.base_url_for_route(identity))
            || (self
                .active_provider_identity()
                .is_ok_and(|active| active == *identity)
                && base_url_uses_local_host(&self.active_route_base_url()))
    }

    pub(crate) fn bind_account_model_owner(&mut self, owner: &str) -> Result<()> {
        anyhow::ensure!(
            !owner.is_empty()
                && owner.len() <= 240
                && !owner.chars().any(|c| c.is_control() || c.is_whitespace()),
            "Invalid account model owner"
        );
        let access = self
            .account_model_access
            .read()
            .clone()
            .context("Connect account model access before running the Codewhale agent")?;
        self.account_model_access = std::sync::Arc::new(parking_lot::RwLock::new(Some(access)));
        self.required_account_model_owner = Some(owner.to_string());
        let identity = self
            .resolve_persisted_provider_identity(Some("codewhale"), Some("codewhale"))
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            self.account_model_api_key(&identity).is_some(),
            "Codewhale agent model access must belong to the signed-in account"
        );
        Ok(())
    }

    pub(crate) fn account_model_api_key(&self, identity: &ProviderIdentity) -> Option<String> {
        let provider = identity.provider;
        // Exact endpoint binding, including path. A custom Codewhale route
        // must never inherit the account's credential, even on the same host.
        if provider != ProviderKind::Codewhale
            || self.base_url_for_route(identity).trim_end_matches('/') != DEFAULT_CODEWHALE_BASE_URL
            || auth_mode_disables_api_key(self.auth_mode_for_provider(identity).as_deref())
            || self
                .provider_config_for(identity)
                .is_some_and(|entry| entry.auth.is_some() || entry.api_key_env.is_some())
        {
            return None;
        }
        let access = self.account_model_access.read().clone()?;
        if access.expires_at <= chrono::Utc::now().timestamp() {
            return None;
        }
        // Check the shared session again at use, so a late install cannot
        // resurrect a session removed by another local process.
        let secrets = codewhale_secrets::account::secure_account_session_secrets().ok()?;
        let account = codewhale_secrets::account::AccountSessionStore::new(
            secrets,
            access.profile.as_deref(),
            codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE,
        )
        .runtime_info_at(chrono::Utc::now())
        .ok()?;
        (account.state == codewhale_secrets::account::AccountSessionState::Authenticated
            && account.session_id.as_deref() == Some(access.session_id.as_str())
            && self
                .required_account_model_owner
                .as_ref()
                .is_none_or(|owner| account.account_id.as_ref() == Some(owner)))
        .then(|| access.credential.expose_secret().to_string())
    }

    /// Prove a current health-cache credential generation without commands,
    /// refresh, secret-store reads, or external credential files. Opaque sources
    /// deliberately remain unchecked; this is not a credential resolver.
    pub(crate) fn readonly_health_credential_generation(
        &self,
        identity: &ProviderIdentity,
    ) -> Option<crate::route_receipt::CredentialGeneration> {
        if self.required_account_model_owner.is_some() {
            return None;
        }
        self.verify_provider_identity(identity).ok()?;
        let mut scoped = self.clone();
        scoped.scope_to_provider_identity(identity).ok()?;
        let endpoint = scoped.base_url_for_route(identity);
        let mode = scoped.auth_mode_for_provider(identity);
        let generation = |value: &str| {
            crate::route_receipt::CredentialGeneration::derive(
                &endpoint,
                &codewhale_secrets::normalize_api_key(value),
            )
        };
        if auth_mode_disables_api_key(mode.as_deref()) {
            return Some(generation(""));
        }
        if provider_uses_oauth_credentials(&scoped, identity)
            || scoped
                .provider_config_for(identity)
                .is_some_and(|entry| entry.auth.is_some())
        {
            return None;
        }
        if let Some(key) = explicit_cli_api_key_override() {
            return Some(generation(&key));
        }
        // The legacy CLI source marker can make DeepSeek prefer an ambient
        // provider key before a saved literal. Without the captured CLI key,
        // this read-only cache projection cannot prove that precedence.
        if identity.provider == ProviderKind::Deepseek
            && cli_api_key_source().as_deref() == Some("cli")
        {
            return None;
        }
        if scoped.config_credentials_are_bound_to_provider_endpoint(identity)
            && let Some(key) = scoped
                .provider_route_string_with_deepseek_fallback(identity, |entry| {
                    entry.api_key.clone()
                })
            && classify_config_api_key_value(&key) == ConfigApiKeyValueKind::Literal
        {
            return Some(generation(&key));
        }
        if let Some(key) = provider_config_env_api_key(&scoped, identity) {
            return Some(generation(&key));
        }
        // An unresolved named binding cannot fall through to unauthenticated local.
        if bound_provider_api_key_env_name(&scoped, identity).is_some() {
            return None;
        }
        if !auth_mode_requires_api_key(mode.as_deref())
            && scoped.should_skip_secret_store_for_provider(identity)
            && identity.provider != ProviderKind::Codewhale
            && scoped
                .provider_config_for(identity)
                .is_none_or(|entry| entry.external_credentials.is_none())
            && (provider_route_is_keyless_self_hosted(identity.provider, &endpoint)
                || base_url_uses_local_host(&endpoint))
        {
            return Some(generation(""));
        }
        None
    }

    /// Read the API key.
    ///
    /// Precedence: **route-specific explicitly consented OAuth token → source-marked explicit CLI key →
    /// provider/root config → configured custom-provider environment →
    /// secret store → ambient provider environment**.
    ///
    /// An in-memory provider-table key is only honored when the user
    /// explicitly set the field (not the legacy `API_KEYRING_SENTINEL`
    /// placeholder, not empty whitespace).
    pub fn active_route_api_key(&self) -> Result<String> {
        self.active_route_api_key_with_source().map(|(key, _)| key)
    }

    /// The active route's key with a display label naming its source
    /// (secret store slot, config file, env var name, CLI, OAuth, …) for
    /// authentication diagnostics. The key is normalized with
    /// [`codewhale_secrets::normalize_api_key`] (#6528).
    pub fn active_route_api_key_with_source(&self) -> Result<(String, String)> {
        self.active_route_api_key_with_secret_store_mode(false)
    }

    /// [`Self::active_route_api_key_with_source`], plus — only when the
    /// resolver's xAI OAuth step produced the key — the account label of
    /// that same credential (`Some(None)`: its ID token names no email).
    /// Plan-limit guidance must name the account that sends the request, and
    /// this reads it without opening the credential a second time.
    pub(crate) fn active_route_api_key_with_xai_sign_in(
        &self,
    ) -> Result<(ResolvedApiKey, Option<XaiSignInLabel>)> {
        if let Some(credentials) = self.xai_oauth_route_credentials() {
            let credentials = credentials?;
            return Ok((
                (
                    codewhale_secrets::normalize_api_key(&credentials.access_token),
                    XAI_OAUTH_KEY_SOURCE.to_string(),
                ),
                Some(credentials.account_label),
            ));
        }
        Ok((self.active_route_api_key_with_source()?, None))
    }

    /// The resolver's xAI OAuth step: `Some` when the active route is xAI on
    /// the official endpoint with `auth_mode = "oauth"` and a usable sign-in
    /// exists (configured owned generation, legacy owned file, or consented
    /// Grok CLI import), holding that sign-in's credentials.
    fn xai_oauth_route_credentials(&self) -> Option<Result<crate::oauth::OwnedOAuthCredentials>> {
        let identity = self.active_provider_identity().ok()?;
        let provider = identity.provider;

        let selected = provider == ProviderKind::Xai
            && !self.provider_uses_custom_endpoint(&identity)
            && self
                .provider_config_for(&identity)
                .is_some_and(provider_config_uses_xai_oauth)
            && crate::oauth::credentials_present(crate::oauth::OAuthProvider::Xai, self);
        selected.then(|| crate::oauth::get_xai_credentials(self))
    }

    /// Resolve an API key for a diagnostic without migrating a legacy secret
    /// store or opening a write-capable secret backend.
    ///
    /// This retains ordinary credential precedence, including a legacy
    /// file-backed secret as a fallback, but it must only be used by static
    /// diagnostic/reporting paths. Normal runtime and authentication paths use
    /// [`Self::active_route_api_key`] and preserve their existing migration
    /// behavior.
    pub(crate) fn active_route_api_key_read_only(&self) -> Result<String> {
        self.active_route_api_key_with_secret_store_mode(true)
            .map(|(key, _)| key)
    }

    fn active_route_api_key_with_secret_store_mode(
        &self,
        read_only: bool,
    ) -> Result<(String, String)> {
        self.resolve_active_route_api_key(read_only)
            .map(|(key, source)| (codewhale_secrets::normalize_api_key(&key), source))
    }

    /// Clone this route with a diagnostic-only credential in its in-memory
    /// provider slot.
    ///
    /// A live `doctor` probe still needs to construct the ordinary client. By
    /// materializing the credential on an isolated clone first, that client
    /// never reaches the normal migrating secret-store resolver while it is
    /// only checking connectivity. The clone is process-local and is never
    /// persisted.
    pub(crate) fn with_read_only_api_key_for_diagnostic(&self) -> Result<Self> {
        let identity = self
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;

        let mut diagnostic = self.clone();
        if identity.provider == ProviderKind::Anthropic
            && self.auth_mode_for_provider(&identity).as_deref() == Some("oauth")
        {
            diagnostic.plugin_oauth_read_only = true;
            return Ok(diagnostic);
        }
        if identity.provider == ProviderKind::Custom
            && self
                .provider_config_for(&identity)
                .is_some_and(|entry| entry.oauth.is_some())
        {
            diagnostic.plugin_oauth_read_only = true;
            return Ok(diagnostic);
        }
        let api_key = self.active_route_api_key_read_only()?;
        diagnostic.set_provider_api_key_override(&identity, Some(api_key))?;
        Ok(diagnostic)
    }

    /// The active route's key and a display label for where it came from
    /// (#6528). Callers go through [`Self::active_route_api_key_with_source`],
    /// which normalizes the key.
    fn resolve_active_route_api_key(&self, read_only: bool) -> Result<(String, String)> {
        let identity = self
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        self.verify_provider_identity(&identity)
            .map_err(anyhow::Error::msg)?;
        if self.required_account_model_owner.is_some() {
            return self.account_model_api_key(&identity)
                .map(|key| (key, "Codewhale account".to_string()))
                .context("The selected account model access is unavailable; no credential fallback is allowed");
        }
        let provider = identity.provider;
        let keyless = || (String::new(), "none (keyless route)".to_string());

        if provider == ProviderKind::Antigravity {
            anyhow::bail!(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
        }
        let auth_mode = self.auth_mode_for_provider(&identity);
        if provider == ProviderKind::Custom
            && let Some(entry) = self.provider_config_for(&identity)
            && let Some(oauth) = entry.oauth.as_ref()
        {
            entry
                .plugin_authority
                .as_ref()
                .context("Plugin OAuth route lacks an approved plugin authority")?;
            anyhow::ensure!(
                auth_mode.as_deref() == Some("oauth"),
                "Plugin OAuth route requires auth_mode = oauth"
            );
            self.provider
                .as_deref()
                .context("Plugin OAuth route has no provider name")?;
            oauth.validate()?;
            // Generic config/client construction must never read secure storage,
            // hash plugin files or refresh OAuth on an async caller's thread.
            // The request worker verifies the receipt, resolves the bound token
            // and checks revocation again immediately before each actual send.
            return Ok((String::new(), "host-managed plugin OAuth".to_string()));
        }

        if auth_mode_disables_api_key(auth_mode.as_deref()) {
            return Ok(keyless());
        }
        if provider == ProviderKind::Anthropic && auth_mode.as_deref() == Some("oauth") {
            anyhow::ensure!(
                matches!(
                    self.base_url_for_route(&identity).trim_end_matches('/'),
                    "https://api.anthropic.com" | "https://api.anthropic.com/v1"
                ),
                "Claude subscription credentials require the native Anthropic endpoint"
            );
            if read_only {
                return Ok((
                    crate::oauth::get_owned_credentials_read_only(
                        crate::oauth::OAuthProvider::Claude,
                        self,
                    )?
                    .access_token,
                    "Claude sign-in".to_string(),
                ));
            }
            return Ok((String::new(), "Claude sign-in".to_string()));
        }
        let custom_endpoint = self.provider_uses_custom_endpoint(&identity);
        let explicit_cli_key = explicit_cli_api_key_override();

        // 0. When the CLI dispatcher forwards an explicit `--api-key`
        // through the provider-neutral CLI bridge with its source marker, that
        // intentional override must win over the saved root key. This is
        // essential for DeepSeek-compatible subscription endpoints where the
        // user runs something like:
        //   codewhale --provider deepseek --api-key ark-... --base-url ... --model auto
        if matches!(provider, ProviderKind::Deepseek)
            && cli_api_key_source().as_deref() == Some("cli")
            && let Some((env_key, source)) = explicit_cli_key
                .as_ref()
                .cloned()
                .map(|key| (key, "--api-key (CLI)".to_string()))
                .or_else(|| {
                    provider_env_api_key_named(provider)
                        .map(|(name, key)| (key, format!("env var {name}")))
                })
            && !env_key.trim().is_empty()
        {
            return Ok((env_key, source));
        }

        if provider == ProviderKind::Moonshot
            && !custom_endpoint
            && self
                .provider_config_for(&identity)
                .is_some_and(provider_config_uses_kimi_imported_token)
        {
            let credential_help =
                credential_help_for_provider_route(provider, &self.active_route_base_url());
            anyhow::bail!(
                "Kimi CLI credential import is unsupported. Codewhale does not impersonate or reuse Kimi OAuth clients; configure an API key from {} instead.",
                credential_help
                    .credential_url
                    .unwrap_or("the selected provider's API-key console")
            );
        }

        // xAI OAuth prefers Codewhale-owned device-login storage. An existing
        // Grok CLI file is considered only with provider/path-scoped read-only
        // consent. Activated by [providers.xai] auth_mode = "oauth". No
        // earlier step applies to xAI, which
        // `active_route_api_key_with_xai_sign_in` relies on.
        if let Some(credentials) = self.xai_oauth_route_credentials() {
            return credentials
                .map(|credentials| (credentials.access_token, XAI_OAUTH_KEY_SOURCE.to_string()));
        }

        if provider == ProviderKind::OpenaiCodex && !custom_endpoint {
            let access_token = if read_only {
                crate::oauth::get_owned_credentials_read_only(
                    crate::oauth::OAuthProvider::Chatgpt,
                    self,
                )?
                .access_token
            } else {
                self.codex_credentials()?.access_token
            };
            return Ok((access_token, "ChatGPT sign-in".to_string()));
        }

        // The dispatcher cannot know the effective provider until the TUI
        // applies `--profile`. A provider-neutral, source-marked CLI override
        // therefore wins over saved API-key slots here, after OAuth routes
        // have made their own credential decision.
        if let Some(value) = explicit_cli_key {
            return Ok((value, "--api-key (CLI)".to_string()));
        }

        // 1. Config file (provider-scoped slot). This intentionally wins
        // over ambient env so `codewhale auth set` fixes stale shell exports.
        if self.config_credentials_are_bound_to_provider_endpoint(&identity)
            && let Some(configured) = self
                .provider_route_string_with_deepseek_fallback(&identity, |entry| {
                    entry.api_key.clone()
                })
            && classify_config_api_key_value(&configured) == ConfigApiKeyValueKind::Literal
        {
            let config_source = match provider_config_table_name(&identity) {
                Ok(table) => format!("`{table}` api_key"),
                Err(_) => "the provider config-table api_key".to_string(),
            };
            warn_on_config_api_key_shadowing(self, &identity, &config_source);
            return Ok((configured, format!("config file ({config_source})")));
        }

        // 1b. A route can explicitly bind an environment variable by name via
        // `[providers.<name>] api_key_env = "..."`. This remains safe for a
        // custom endpoint because the binding belongs to that route; ambient
        // provider variables below do not.
        //
        // For a custom provider, a binding that names an unset (or empty)
        // variable is a broken credential contract, not a keyless route: fail
        // loudly with the route-scoped fix instead of silently degrading to
        // the self-hosted loopback keyless fallback below (#5104). Without
        // this, an `api_key_env` route on a loopback host dispatched
        // unauthenticated while the operator believed credentials were wired,
        // and the composer-side preflight recovery never saw an error.
        if provider == ProviderKind::Custom
            && let Some(env_name) = bound_provider_api_key_env_name(self, &identity)
        {
            return match std::env::var(&env_name) {
                Ok(value) if !value.trim().is_empty() => {
                    Ok((value, format!("env var {env_name} (api_key_env)")))
                }
                _ => {
                    let route_name = self.provider.as_deref().unwrap_or("<name>");
                    Err(anyhow::anyhow!(
                        "Custom provider '{route_name}' API key not found: the route binds \
                         api_key_env = \"{env_name}\" but that environment variable is not set. \
                         Set {env_name} to your key, or remove api_key_env from \
                         [providers.{route_name}] to run the endpoint without credentials."
                    ))
                }
            };
        }
        if let Some(value) = provider_config_env_api_key(self, &identity) {
            let name = bound_provider_api_key_env_name(self, &identity).unwrap_or_default();
            return Ok((value, format!("env var {name} (api_key_env)")));
        }

        // 2. The dispatcher resolves this same provider slot before launching
        // the TUI. Standalone `codewhale-tui` launches must see the identical
        // durable credential. Auto-detection is file-backed and prompt-free by
        // default; the OS keyring is queried only when the user explicitly
        // selects the system backend.
        if !self.should_skip_secret_store_for_provider(&identity)
            && let Some(value) = provider_secret_store_api_key_with_mode(self, &identity, read_only)
        {
            return Ok((
                value,
                format!(
                    "secret store slot `{}`",
                    provider_secret_store_slot(provider)
                ),
            ));
        }

        // 3. Ambient provider environment variables are scoped to official
        // endpoints. Never send an official-provider export to a custom host.
        if !self.should_skip_secret_store_for_provider(&identity)
            && provider == ProviderKind::XiaomiMimo
        {
            let mode = self
                .provider_config_for(&identity)
                .and_then(|provider| provider.mode.as_deref());
            if let Some(value) =
                xiaomi_mimo_env_api_key_for_runtime(mode, Some(&self.active_route_base_url()))
                && !value.trim().is_empty()
            {
                return Ok((
                    value,
                    format!("env var ({})", provider.provider().env_vars().join(" / ")),
                ));
            }
        }
        if !self.should_skip_secret_store_for_provider(&identity)
            && let Some((name, value)) = provider_env_api_key_named(provider)
        {
            return Ok((value, format!("env var {name}")));
        }

        // Official DeepSeek Harness credentials, only after explicit
        // read-only consent to one exact `$DSH_HOME/.credentials.yaml`.
        if matches!(
            provider,
            ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
        ) && identity.key.as_str() != codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id
            && !custom_endpoint
        {
            let path = codewhale_config::default_dsh_credentials_path();
            if let Ok(grant) = self.external_credential_read_grant(
                &identity,
                codewhale_config::ExternalCredentialSource::DshCli,
                &path,
            ) && let Some(value) = crate::dsh_credentials::deepseek_api_key_from_grant(&grant)?
            {
                return Ok((
                    value,
                    "DeepSeek Harness credentials (consented)".to_string(),
                ));
            }
        }

        // Account auth is a reversible fallback, never an overwrite of an
        // environment, config-file, or durable provider credential.
        if let Some(key) = self.account_model_api_key(&identity) {
            return Ok((key, "Codewhale account".to_string()));
        }

        // The Codewhale API always authenticates. It is not a self-hosted
        // runtime, and a loopback `CODEWHALE_API_BASE` is a test origin for
        // the same authenticated contract — never a keyless one. Without this
        // the loopback arm below returned an empty key and the request went
        // out with no `Authorization` header at all.
        if provider != ProviderKind::Codewhale
            && !auth_mode_requires_api_key(auth_mode.as_deref())
            && (provider_route_is_keyless_self_hosted(provider, &self.active_route_base_url())
                || base_url_uses_local_host(&self.active_route_base_url()))
        {
            return Ok(keyless());
        }

        if custom_endpoint {
            let route_name = self
                .provider
                .as_deref()
                .unwrap_or_else(|| provider.as_str());
            anyhow::bail!(
                "Custom endpoint credentials for {route_name} must be bound explicitly. Ambient provider credentials are not sent to {}. Add api_key or api_key_env to this provider route, or pass --api-key with --base-url.",
                self.active_route_base_url()
            );
        }

        match provider {
            ProviderKind::Codewhale => anyhow::bail!(
                "Codewhale API key not found, so no request was sent.\n\
                 \n\
                 The Codewhale API authenticates every model with one account \
                 API key carrying the `models:infer` scope.\n\
                 \n\
                 1. Create one and save it on this machine:\n\
                        codewhale account api-keys create --name <name> --scope models:infer --use\n\
                 2. Or export it for this shell:\n\
                        export CODEWHALE_API_KEY=cwc_key_...\n\
                 \n\
                 You can also create a key at {} and put it in \
                 [providers.codewhale] api_key (or api_key_env).",
                provider
                    .provider()
                    .credential_help()
                    .credential_url
                    .unwrap_or("https://app.codewhale.net/settings?section=api")
            ),
            ProviderKind::Deepseek => {
                anyhow::bail!(deepseek_missing_key_message())
            }
            ProviderKind::SiliconflowCN => anyhow::bail!(
                "SiliconFlow China API key not found. Get a key: {}. Run 'codewhale auth set --provider siliconflow-CN', \
                 set {}, or add [{}] api_key in ~/.codewhale/config.toml. \
                 [providers.siliconflow] remains a fallback when the CN table omits api_key.",
                provider
                    .provider()
                    .credential_help()
                    .credential_url
                    .unwrap_or("https://cloud.siliconflow.com/account/ak"),
                provider.provider().env_vars().join(" / "),
                provider_config_table_name(&identity)?
            ),
            ProviderKind::Moonshot => {
                let credential_help =
                    credential_help_for_provider_route(provider, &self.active_route_base_url());
                if moonshot_base_url_is_exact_kimi_code(&self.active_route_base_url()) {
                    anyhow::bail!(
                        "Kimi Code membership-plan API key not found. Get a plan key: {}. This route uses api.kimi.com/coding/v1 and does not import Kimi CLI credentials. Run 'codewhale auth set --provider moonshot', set {}, or add [{}] api_key.",
                        credential_help
                            .credential_url
                            .unwrap_or(KIMI_CODE_MEMBERSHIP_PLAN_CONSOLE_URL),
                        provider.provider().env_vars().join(" / "),
                        provider_config_table_name(&identity)?
                    );
                }
                anyhow::bail!(
                    "Moonshot/Kimi API key not found. Get a key: {}. Run 'codewhale auth set --provider moonshot', \
                     set {}, or add [{}] api_key. \
                     For a Kimi Code plan key, set [providers.moonshot] base_url = \
                     \"https://api.kimi.com/coding/v1\" and model = \"kimi-for-coding\".",
                    credential_help
                        .credential_url
                        .unwrap_or("https://platform.kimi.ai/console/api-keys"),
                    provider.provider().env_vars().join(" / "),
                    provider_config_table_name(&identity)?
                );
            }
            ProviderKind::Anthropic | ProviderKind::Openmodel => {
                anyhow::bail!("{}", missing_provider_api_key_message(&identity)?)
            }
            ProviderKind::OpencodeZen => {
                anyhow::bail!("{}", missing_provider_api_key_message(&identity)?)
            }
            ProviderKind::OpenaiCodex => anyhow::bail!(
                "{}",
                crate::oauth::missing_auth_message(crate::oauth::OAuthProvider::Chatgpt)
            ),
            ProviderKind::Xai => {
                // Prefer OAuth guidance when auth_mode requests it or Grok CLI
                // tokens already exist; otherwise show both API-key and OAuth.
                if self
                    .provider_config_for(&identity)
                    .is_some_and(provider_config_uses_xai_oauth)
                    || crate::oauth::credentials_present(crate::oauth::OAuthProvider::Xai, self)
                {
                    anyhow::bail!(
                        "{}",
                        crate::oauth::missing_auth_message(crate::oauth::OAuthProvider::Xai)
                    );
                }
                anyhow::bail!(
                    "xAI API key not found. Get a key: https://console.x.ai/\n\
                     Run 'codewhale auth set --provider xai', set XAI_API_KEY, or add \
                     [providers.xai] api_key.\n\
                     OAuth alternative: run `codewhale auth xai-device` for \
                     Codewhale-owned storage and set [providers.xai] auth_mode = \"oauth\"."
                );
            }
            // Self-hosted deployments commonly run without auth on localhost.
            // Return an empty key and let the client omit the Authorization header.
            ProviderKind::Sglang | ProviderKind::Vllm => Ok(keyless()),
            ProviderKind::Ollama
                if provider_route_is_keyless_self_hosted(
                    provider,
                    &self.active_route_base_url(),
                ) =>
            {
                Ok(keyless())
            }
            ProviderKind::Ollama => {
                let help =
                    credential_help_for_provider_route(provider, &self.active_route_base_url());
                anyhow::bail!(
                    "Ollama Cloud API key not found. Get a key: {}. Run 'codewhale auth set --provider ollama', set OLLAMA_API_KEY, or add [providers.ollama] api_key in ~/.codewhale/config.toml.",
                    help.credential_url
                        .unwrap_or(codewhale_config::provider::OLLAMA_CLOUD_API_KEY_URL)
                )
            }
            // Custom OpenAI-compatible endpoints (#1519): the key comes from the
            // env var named by `[providers.<name>] api_key_env`. If we reached
            // here it is unset/empty (and the endpoint is not loopback).
            ProviderKind::Custom => {
                let provider_name = self.provider.as_deref().unwrap_or("<name>");
                match self
                    .provider_config_for(&identity)
                    .and_then(|entry| entry.api_key_env.as_deref())
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    Some(env_name) => anyhow::bail!(
                        "Custom provider '{provider_name}' API key not found.\n\
                         Set the environment variable {env_name} to your key, \
                         or add api_key to [providers.{provider_name}]."
                    ),
                    None => anyhow::bail!(
                        "Custom provider '{provider_name}' has no auth configured.\n\
                         Add api_key_env = \"YOUR_ENV_VAR\" (or api_key) to \
                         [providers.{provider_name}] in ~/.codewhale/config.toml."
                    ),
                }
            }
            _ => anyhow::bail!("{}", missing_provider_api_key_message(&identity)?),
        }
    }

    /// Resolve the skills directory path.
    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.skills_dir
            .as_deref()
            .map(expand_path)
            .or_else(default_skills_dir)
            .unwrap_or_else(|| PathBuf::from("./skills"))
    }

    /// Resolve the MCP config path.
    #[must_use]
    pub fn mcp_config_path(&self) -> PathBuf {
        let configured = self.mcp_config_path.as_deref().map(expand_path);
        match configured {
            Some(path) if path.is_absolute() => path,
            Some(path) => {
                tracing::warn!(
                    configured_path = %path.display(),
                    "relative mcp_config_path is not stable across launch directories; using the user-global MCP config"
                );
                default_mcp_config_path().unwrap_or_else(|| PathBuf::from("./mcp.json"))
            }
            None => default_mcp_config_path().unwrap_or_else(|| PathBuf::from("./mcp.json")),
        }
    }

    /// Resolve the notes file path.
    #[must_use]
    pub fn notes_path(&self) -> PathBuf {
        self.notes_path
            .as_deref()
            .map(expand_path)
            .or_else(default_notes_path)
            .unwrap_or_else(|| PathBuf::from("./notes.txt"))
    }

    /// Resolve the memory file path.
    #[must_use]
    pub fn memory_path(&self) -> PathBuf {
        let legacy_path = self
            .memory_path
            .as_deref()
            .map(expand_path)
            .or_else(default_memory_path)
            .unwrap_or_else(|| PathBuf::from("./memory.md"));
        if self.memory_backend() == MemoryBackend::Native {
            // The configured value is historically a *legacy single-file*
            // path (`$CODEWHALE_HOME/memory.md`), and the native store lives
            // beside it. Deriving from the parent is therefore right for the
            // default and for anyone still carrying the old setting.
            //
            // But someone who points `memory_path` at a native store — the
            // obvious reading of the name — used to get a second one nested
            // inside it (`…/memory/global/memory/global/MEMORY.md`), silently
            // writing somewhere other than the file they named. Honour an
            // already-native path as itself.
            if crate::native_memory::NativeMemoryStore::from_global_path(&legacy_path).is_some() {
                return legacy_path;
            }
            return legacy_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("memory")
                .join("global")
                .join("MEMORY.md");
        }
        legacy_path
    }

    /// Resolve the default speech/TTS output directory, if configured.
    #[must_use]
    pub fn speech_output_dir(&self) -> Option<PathBuf> {
        std::env::var("XIAOMI_MIMO_SPEECH_OUTPUT_DIR")
            .or_else(|_| std::env::var("MIMO_SPEECH_OUTPUT_DIR"))
            .or_else(|_| std::env::var("XIAOMIMIMO_SPEECH_OUTPUT_DIR"))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| expand_path(&value))
            .or_else(|| {
                self.speech
                    .as_ref()
                    .and_then(|speech| speech.output_dir.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(expand_path)
            })
    }

    /// Resolve the configured `instructions = [...]` array (#454)
    /// to absolute paths, in declared order. Empty when unset or
    /// when every entry is empty after trimming. Each entry runs
    /// through `expand_path` so `~` and env vars are honoured.
    #[must_use]
    pub fn instructions_paths(&self) -> Vec<PathBuf> {
        self.instructions
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(expand_path)
            .collect()
    }

    /// Whether the user-memory feature is enabled. The default is **off**
    /// to preserve zero-overhead behavior for users who haven't opted in.
    /// Flips to `true` when `[memory] enabled = true` in `config.toml` or
    /// `DEEPSEEK_MEMORY=on` is set in the environment.
    #[must_use]
    pub fn memory_enabled(&self) -> bool {
        if let Some(backend) = self.memory.as_ref().and_then(|memory| memory.backend) {
            return backend != MemoryBackend::Off;
        }
        self.memory
            .as_ref()
            .and_then(|m| m.enabled)
            .unwrap_or(false)
    }

    /// Effective safety backstop on automatic goal continuation passes
    /// (#5052). Goals are unlimited by default. `[goal] max_continuations`
    /// opts into a ceiling; `0` disables it so only terminal status or user
    /// control stops an operate-mode goal run.
    #[must_use]
    pub fn goal_max_continuations(&self) -> u32 {
        self.goal
            .as_ref()
            .and_then(|goal| goal.max_continuations)
            .unwrap_or(crate::goal_loop::DEFAULT_MAX_GOAL_CONTINUATIONS)
    }

    /// Per-engine-turn model-step budget while a goal is active (#6512).
    /// Uses the ordinary turn resolver: omitted or zero is uncapped, and
    /// explicit positive ceilings are clamped to its supported range.
    #[must_use]
    pub fn goal_max_steps(&self) -> u32 {
        let configured = self.goal.as_ref().and_then(|goal| goal.max_steps);
        let resolved = crate::core::engine::turn_budget::resolve_max_model_steps(configured);
        if let Some(steps) = configured
            && steps > 0
            && resolved != steps
        {
            tracing::warn!("[goal] max_steps={steps} out of range; clamping to {resolved}");
        }
        resolved
    }

    /// Whether a goal's `token_budget` is a hard stop (#6013). Default `false`
    /// keeps the advisory/telemetry behavior.
    #[must_use]
    pub fn goal_enforce_token_budget(&self) -> bool {
        self.goal
            .as_ref()
            .and_then(|goal| goal.enforce_token_budget)
            .unwrap_or(false)
    }

    /// Quiet period between successful interactive goal turns (#5508).
    /// Absent/zero keeps the existing immediate-continuation behavior.
    #[must_use]
    pub fn goal_continuation_delay_seconds(&self) -> u64 {
        self.goal
            .as_ref()
            .and_then(|goal| goal.continuation_delay_seconds)
            .unwrap_or(0)
            .min(crate::goal_loop::MAX_GOAL_CONTINUATION_DELAY_SECONDS)
    }

    /// Maximum number of automatic re-requests when the model returns only
    /// reasoning without any answer or tool call. Defaults to 2.
    /// Set via `[reasoning_only] max_reprompts` in config.toml.
    #[must_use]
    pub fn reasoning_only_max_reprompts(&self) -> u32 {
        self.reasoning_only
            .as_ref()
            .and_then(|cfg| cfg.max_reprompts)
            .unwrap_or(DEFAULT_REASONING_ONLY_REPROMPTS)
    }

    /// Optional custom message sent to the model on each re-request when the
    /// model returns only reasoning without any answer or tool call.
    /// Set via `[reasoning_only] reprompt_message` in config.toml.
    /// When unset, the built-in default is used.
    #[must_use]
    pub fn reasoning_only_reprompt_message(&self) -> &str {
        self.reasoning_only
            .as_ref()
            .and_then(|cfg| cfg.reprompt_message.as_deref())
            .unwrap_or(DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE)
    }

    /// Resolve the explicit local-memory backend.
    #[must_use]
    pub fn memory_backend(&self) -> MemoryBackend {
        self.memory
            .as_ref()
            .and_then(|memory| memory.backend)
            .unwrap_or_else(|| {
                let Some(memory) = self.memory.as_ref() else {
                    return MemoryBackend::Off;
                };
                if memory.enabled.unwrap_or(false) {
                    MemoryBackend::Native
                } else {
                    MemoryBackend::Off
                }
            })
    }

    /// Return the configured vision model config. A `[vision_model]` that
    /// used to inherit the top-level `api_key` carries its own copy since
    /// parsing moved that key (#6394); nothing else is inherited, so a
    /// DeepSeek key never follows `vision_model.base_url` to another host.
    #[must_use]
    pub fn vision_model_config(&self) -> Option<VisionModelConfig> {
        self.vision_model.clone()
    }

    #[must_use]
    pub fn project_context_pack_enabled(&self) -> bool {
        self.context.project_pack.unwrap_or(false)
    }

    /// Return whether shell execution is allowed for noninteractive and
    /// durable-task profiles. Defaults to `false`: in headless, app-server, and
    /// background-task contexts there is no human to approve commands, so shell
    /// access must be opted into explicitly (GHSA-72w5-pf8h-xfp4).
    #[must_use]
    pub fn allow_shell(&self) -> bool {
        self.allow_shell.unwrap_or(false)
    }

    /// Return whether shell execution is allowed for an *interactive* TUI Agent
    /// session. Defaults to `true`: the interactive composer always gates each
    /// shell command behind an approval prompt, so the catalog can expose shell
    /// by default while still preserving consent (GHSA-72w5-pf8h-xfp4). An
    /// explicit `allow_shell = false` still hides shell tools. This is the
    /// single source of truth for the interactive default; both startup
    /// (`run_interactive`) and the durable Agent permission baseline read it so
    /// the default cannot drift between them.
    #[must_use]
    pub fn interactive_allow_shell(&self) -> bool {
        self.allow_shell.unwrap_or(true)
    }

    /// Whether ghost-text prompt suggestion is enabled (opt-in, default off).
    pub fn prompt_suggestion_enabled(&self) -> bool {
        self.prompt_suggestion.unwrap_or(false)
    }

    /// Standing operator instructions for the compaction summarizer
    /// (`[compaction] summary_instructions`, #5956).
    ///
    /// Trimmed; empty or whitespace-only reads as unset so an accidentally
    /// blank key cannot append an empty delimited section to every prompt.
    /// The character cap is applied where the suffix is built, so the warning
    /// fires once per compaction pass rather than once per turn.
    #[must_use]
    pub fn compaction_summary_instructions(&self) -> Option<String> {
        self.compaction
            .as_ref()
            .and_then(|compaction| compaction.summary_instructions.as_deref())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    }

    /// Verbatim retention budget for recent plain user messages in the
    /// compaction replacement history (`[compaction]
    /// retained_user_message_tokens`, #5956).
    ///
    /// Unset returns the historical hard-coded 20 000. Explicit values clamp
    /// to `2_000..=200_000`: below the floor the replacement history stops
    /// carrying a usable amount of the user's own words, above the ceiling
    /// the "compacted" history is large enough to re-trigger compaction on
    /// the next turn.
    #[must_use]
    pub fn compaction_retained_user_message_tokens(&self) -> usize {
        let Some(requested) = self
            .compaction
            .as_ref()
            .and_then(|compaction| compaction.retained_user_message_tokens)
        else {
            return DEFAULT_COMPACTION_RETAINED_USER_MESSAGE_TOKENS;
        };
        let clamped = requested.clamp(
            MIN_COMPACTION_RETAINED_USER_MESSAGE_TOKENS,
            MAX_COMPACTION_RETAINED_USER_MESSAGE_TOKENS,
        );
        if clamped != requested {
            tracing::warn!(
                "[compaction] retained_user_message_tokens = {requested} is outside {}..={}; using {clamped}",
                MIN_COMPACTION_RETAINED_USER_MESSAGE_TOKENS,
                MAX_COMPACTION_RETAINED_USER_MESSAGE_TOKENS
            );
        }
        clamped
    }

    /// Return the maximum number of concurrent sub-agents.
    /// Checks `[subagents] max_concurrent` first, then top-level `max_subagents`,
    /// then falls back to `DEFAULT_MAX_SUBAGENTS`.
    #[must_use]
    pub fn max_subagents(&self) -> usize {
        // Check [subagents] max_concurrent first
        if let Some(subagents_cfg) = self.subagents.as_ref()
            && let Some(max) = subagents_cfg.max_concurrent
        {
            return max.clamp(1, MAX_SUBAGENTS);
        }
        // Fall back to top-level max_subagents
        self.max_subagents
            .unwrap_or(DEFAULT_MAX_SUBAGENTS)
            .clamp(1, MAX_SUBAGENTS)
    }

    /// Return the provider-specific maximum number of concurrent sub-agents.
    /// `[subagents.providers.<provider>] max_concurrent` inherits from the
    /// global `[subagents]` value when unset.
    #[must_use]
    pub fn max_subagents_for_provider(&self, identity: &ProviderIdentity) -> usize {
        self.subagent_provider_config(identity)
            .and_then(|cfg| cfg.max_concurrent)
            .map(|max| max.clamp(1, MAX_SUBAGENTS))
            .unwrap_or_else(|| self.max_subagents())
    }

    /// Whether the model-facing `agent` tool is available after applying the
    /// feature flag, explicit `[subagents] enabled` switch, and legacy
    /// zero-valued opt-outs.
    #[must_use]
    pub fn subagents_enabled(&self) -> bool {
        self.subagents_disabled_reason().is_none()
    }

    /// Whether the model-facing `agent` tool is available for this provider
    /// after applying global and provider-specific sub-agent controls.
    #[must_use]
    pub fn subagents_enabled_for_provider(&self, identity: &ProviderIdentity) -> bool {
        if !self.subagents_enabled() {
            return false;
        }
        let Some(provider_cfg) = self.subagent_provider_config(identity) else {
            return true;
        };
        provider_cfg.enabled != Some(false)
            && provider_cfg.max_concurrent != Some(0)
            && provider_cfg.max_depth != Some(0)
    }

    /// Machine-readable reason sub-agents are disabled, in precedence order.
    #[must_use]
    pub fn subagents_disabled_reason(&self) -> Option<&'static str> {
        if !self.features().enabled(Feature::Subagents) {
            return Some("features.subagents=false");
        }
        let subagents_cfg = self.subagents.as_ref()?;
        if subagents_cfg.enabled == Some(false) {
            return Some("subagents.enabled=false");
        }
        if subagents_cfg.max_concurrent == Some(0) {
            return Some("subagents.max_concurrent=0");
        }
        if subagents_cfg.max_depth == Some(0) {
            return Some("subagents.max_depth=0");
        }
        None
    }

    /// How many levels of nested sub-agents the interactive `agent` tool may
    /// spawn. Reads `[subagents] max_depth`; when unset it defaults to
    /// [`codewhale_config::DEFAULT_SPAWN_DEPTH`]. `0` is a valid value that
    /// blocks the `agent` tool at this runtime depth. Any value is clamped to
    /// [`codewhale_config::MAX_SPAWN_DEPTH_CEILING`] so the operator's choice
    /// can never exceed the hard recursion ceiling.
    #[must_use]
    pub fn subagent_max_spawn_depth(&self) -> u32 {
        self.subagents
            .as_ref()
            .and_then(|cfg| cfg.max_depth)
            .unwrap_or(codewhale_config::DEFAULT_SPAWN_DEPTH)
            .min(codewhale_config::MAX_SPAWN_DEPTH_CEILING)
    }

    /// Return the provider-specific maximum sub-agent recursion depth.
    #[must_use]
    pub fn subagent_max_spawn_depth_for_provider(&self, identity: &ProviderIdentity) -> u32 {
        self.subagent_provider_config(identity)
            .and_then(|cfg| cfg.max_depth)
            .unwrap_or_else(|| self.subagent_max_spawn_depth())
            .min(codewhale_config::MAX_SPAWN_DEPTH_CEILING)
    }

    /// Number of direct (depth-1) sub-agents that may execute concurrently
    /// before further launches queue for a launch slot (#3095). Reads
    /// `[subagents] launch_concurrency` (or the deprecated
    /// `interactive_max_launch` alias); when unset it defaults to the full
    /// resolved `max_subagents()` (no artificial throttle), and any explicit
    /// value is clamped to `[1, max_subagents]`.
    #[must_use]
    pub fn launch_concurrency(&self) -> usize {
        let max = self.max_subagents();
        self.subagents
            .as_ref()
            .and_then(|cfg| cfg.launch_concurrency.or(cfg.interactive_max_launch_legacy))
            .unwrap_or(max)
            .clamp(1, max)
    }

    /// Return the provider-specific direct launch throttle. Children above
    /// this limit queue for a launch slot instead of starting immediately.
    #[must_use]
    pub fn launch_concurrency_for_provider(&self, identity: &ProviderIdentity) -> usize {
        let max = self.max_subagents_for_provider(identity);
        self.subagent_provider_config(identity)
            .and_then(|cfg| cfg.launch_concurrency)
            .or_else(|| {
                self.subagents
                    .as_ref()
                    .and_then(|cfg| cfg.launch_concurrency.or(cfg.interactive_max_launch_legacy))
            })
            .unwrap_or(max)
            .clamp(1, max)
    }

    /// Maximum queued + running sub-agents admitted for the session.
    ///
    /// Defaults to [`MAX_SUBAGENT_ADMISSION`] so distinct `agent` calls can
    /// queue and drain through `launch_concurrency` instead of being rejected
    /// at the instantaneous concurrency cap. Explicit values are clamped to
    /// `[max_subagents, MAX_SUBAGENT_ADMISSION]`.
    #[must_use]
    pub fn max_admitted_subagents(&self) -> usize {
        let max_concurrent = self.max_subagents();
        self.subagents
            .as_ref()
            .and_then(|cfg| cfg.max_admitted)
            .unwrap_or(MAX_SUBAGENT_ADMISSION)
            .clamp(max_concurrent, MAX_SUBAGENT_ADMISSION)
    }

    /// Return the provider-specific queued + running admission cap.
    #[must_use]
    pub fn max_admitted_subagents_for_provider(&self, identity: &ProviderIdentity) -> usize {
        let max_concurrent = self.max_subagents_for_provider(identity);
        self.subagent_provider_config(identity)
            .and_then(|cfg| cfg.max_admitted)
            .or_else(|| self.subagents.as_ref().and_then(|cfg| cfg.max_admitted))
            .unwrap_or(MAX_SUBAGENT_ADMISSION)
            .clamp(max_concurrent, MAX_SUBAGENT_ADMISSION)
    }

    /// Default per-child model-turn budget from `[subagents]
    /// default_max_steps`, applied when an `agent` start carries no explicit
    /// `max_steps` (#5324). `None` or `0` mean unbounded; a positive value is
    /// clamped to the runtime ceiling when applied.
    #[must_use]
    pub fn subagent_default_max_steps(&self) -> Option<u32> {
        self.subagents
            .as_ref()
            .and_then(|cfg| cfg.default_max_steps)
            .filter(|steps| *steps > 0)
    }

    /// Default per-child wall-clock budget in seconds from `[subagents]
    /// default_wall_time_secs`, applied when an `agent` start carries no
    /// explicit `wall_time_secs` (#5324). `None` or `0` keep the 1800s
    /// default; the resolved value is clamped to 1..=86400 when applied.
    #[must_use]
    pub fn subagent_default_wall_time_secs(&self) -> Option<u64> {
        self.subagents
            .as_ref()
            .and_then(|cfg| cfg.default_wall_time_secs)
            .filter(|secs| *secs > 0)
    }

    /// Resolved per-step DeepSeek API timeout for sub-agents, in seconds.
    ///
    /// Reads `[subagents] api_timeout_secs` and clamps to
    /// `[MIN_SUBAGENT_API_TIMEOUT_SECS, MAX_SUBAGENT_API_TIMEOUT_SECS]`
    /// (1..=3600). `None` or `0` resolve to
    /// `DEFAULT_SUBAGENT_API_TIMEOUT_SECS` (600); explicit `1` is honored,
    /// useful only in fast fail-fast tests, not production (#1806, #1808).
    #[must_use]
    pub fn subagent_api_timeout_secs(&self) -> u64 {
        resolve_subagent_api_timeout_secs(
            self.subagents.as_ref().and_then(|cfg| cfg.api_timeout_secs),
        )
    }

    /// Return the provider-specific per-step API timeout for sub-agents.
    #[must_use]
    pub fn subagent_api_timeout_secs_for_provider(&self, identity: &ProviderIdentity) -> u64 {
        resolve_subagent_api_timeout_secs(
            self.subagent_provider_config(identity)
                .and_then(|cfg| cfg.api_timeout_secs)
                .or_else(|| self.subagents.as_ref().and_then(|cfg| cfg.api_timeout_secs)),
        )
    }

    /// Resolved no-progress heartbeat timeout for running sub-agents.
    ///
    /// Reads `[subagents] heartbeat_timeout_secs` and clamps to
    /// `[MIN_SUBAGENT_HEARTBEAT_TIMEOUT_SECS, MAX_SUBAGENT_HEARTBEAT_TIMEOUT_SECS]`.
    /// `None` or `0` resolve to the default 300 seconds. The final value is
    /// also kept at least 30 seconds above `subagent_api_timeout_secs()` so a
    /// configured long model request is not pre-empted by heartbeat cleanup,
    /// and at least 30 seconds above the sub-agent tool timeout so a single
    /// long tool execution is not cancelled as "no progress" (2026-08-04
    /// sub-agent hunt, finding 4).
    #[must_use]
    pub fn subagent_heartbeat_timeout_secs(&self) -> u64 {
        resolve_subagent_heartbeat_timeout_secs(
            self.subagents
                .as_ref()
                .and_then(|cfg| cfg.heartbeat_timeout_secs),
            self.subagent_api_timeout_secs(),
            DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS,
        )
    }

    /// Return the provider-specific no-progress heartbeat timeout.
    #[must_use]
    pub fn subagent_heartbeat_timeout_secs_for_provider(&self, identity: &ProviderIdentity) -> u64 {
        let api_timeout = self.subagent_api_timeout_secs_for_provider(identity);
        resolve_subagent_heartbeat_timeout_secs(
            self.subagent_provider_config(identity)
                .and_then(|cfg| cfg.heartbeat_timeout_secs)
                .or_else(|| {
                    self.subagents
                        .as_ref()
                        .and_then(|cfg| cfg.heartbeat_timeout_secs)
                }),
            api_timeout,
            DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS,
        )
    }

    /// Resolved per-SSE-chunk idle timeout in seconds.
    ///
    /// Reads `[stream].chunk_timeout_secs`, then legacy `[tui]`, then the
    /// `CODEWHALE_STREAM_IDLE_TIMEOUT_SECS` env var (legacy alias:
    /// `DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS`) when the config key is
    /// omitted. `None` or `0` resolve to the default 900 seconds; explicit
    /// values are clamped to `1..=3600`.
    #[must_use]
    pub fn stream_chunk_timeout_secs(&self) -> u64 {
        let raw = self
            .stream
            .as_ref()
            .and_then(|cfg| cfg.chunk_timeout_secs)
            .or_else(|| {
                self.tui
                    .as_ref()
                    .and_then(|cfg| cfg.stream_chunk_timeout_secs)
            })
            .or_else(|| {
                std::env::var(STREAM_CHUNK_TIMEOUT_ENV)
                    .or_else(|_| std::env::var(LEGACY_STREAM_CHUNK_TIMEOUT_ENV))
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .unwrap_or(DEFAULT_STREAM_CHUNK_TIMEOUT_SECS);
        if raw == 0 {
            return DEFAULT_STREAM_CHUNK_TIMEOUT_SECS;
        }
        raw.clamp(MIN_STREAM_CHUNK_TIMEOUT_SECS, MAX_STREAM_CHUNK_TIMEOUT_SECS)
    }

    /// Resolved optional ceiling on model steps in a single turn.
    ///
    /// Reads `[tui].max_model_steps`, falling back to the
    /// `CODEWHALE_MAX_MODEL_STEPS` env var, then to the uncapped default.
    /// `0` from either source also selects the uncapped default.
    #[must_use]
    pub fn max_model_steps(&self) -> u32 {
        let raw = self
            .tui
            .as_ref()
            .and_then(|cfg| cfg.max_model_steps)
            .or_else(|| {
                std::env::var(MAX_MODEL_STEPS_ENV)
                    .ok()
                    .and_then(|value| value.trim().parse::<u32>().ok())
            });
        crate::core::engine::turn_budget::resolve_max_model_steps(raw)
    }

    /// R1: resolved cumulative per-turn wall-clock budget.
    ///
    /// Reads `[tui].turn_wall_clock_secs`, falling back to the
    /// `CODEWHALE_TURN_WALL_CLOCK_SECS` env var, then to no limit. `0`
    /// also means no limit.
    #[must_use]
    pub fn turn_wall_clock(&self) -> std::time::Duration {
        let raw = self
            .tui
            .as_ref()
            .and_then(|cfg| cfg.turn_wall_clock_secs)
            .or_else(|| {
                std::env::var(TURN_WALL_CLOCK_ENV)
                    .ok()
                    .and_then(|value| value.trim().parse::<u64>().ok())
            });
        crate::core::engine::turn_budget::resolve_turn_wall_clock(raw)
    }

    /// R1: resolved per-step cap on accumulated streamed content, in bytes.
    #[must_use]
    pub fn stream_max_content_bytes(&self) -> usize {
        crate::core::engine::turn_budget::resolve_stream_max_content_bytes(
            self.stream
                .as_ref()
                .and_then(|cfg| cfg.max_content_mb)
                .or_else(|| self.tui.as_ref().and_then(|cfg| cfg.stream_max_content_mb)),
        )
    }

    /// R1: resolved per-step cap on a single stream's wall-clock duration.
    #[must_use]
    pub fn stream_max_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            crate::core::engine::turn_budget::resolve_stream_max_duration_secs(
                self.stream
                    .as_ref()
                    .and_then(|cfg| cfg.max_duration_secs)
                    .or_else(|| {
                        self.tui
                            .as_ref()
                            .and_then(|cfg| cfg.stream_max_duration_secs)
                    }),
            ),
        )
    }

    /// Resolved `[stream]` retry budgets, falling back to legacy `[tui]` keys.
    #[must_use]
    pub fn stream_retry_limits(&self) -> crate::core::engine::turn_budget::StreamRetryLimits {
        let tui = self.tui.as_ref();
        let stream = self.stream.as_ref();
        crate::core::engine::turn_budget::resolve_stream_retry_limits(
            stream
                .and_then(|cfg| cfg.max_resumes)
                .or_else(|| tui.and_then(|cfg| cfg.stream_max_resumes)),
            stream
                .and_then(|cfg| cfg.max_transparent_retries)
                .or_else(|| tui.and_then(|cfg| cfg.stream_max_transparent_retries)),
            stream
                .and_then(|cfg| cfg.max_stream_errors)
                .or_else(|| tui.and_then(|cfg| cfg.stream_max_errors)),
        )
    }

    /// #6700: resolved wait for SSE response headers.
    #[must_use]
    pub fn stream_open_timeout(&self) -> std::time::Duration {
        crate::client::resolve_stream_open_timeout(
            self.stream
                .as_ref()
                .and_then(|cfg| cfg.open_timeout_secs)
                .or_else(|| {
                    self.tui
                        .as_ref()
                        .and_then(|cfg| cfg.stream_open_timeout_secs)
                }),
        )
    }

    /// #6700: whether the model HTTP client is pinned to HTTP/1.1 —
    /// `[stream].force_http1` (legacy `[tui]` fallback), OR the environment pin.
    #[must_use]
    pub fn force_http1(&self) -> bool {
        self.stream
            .as_ref()
            .and_then(|cfg| cfg.force_http1)
            .or_else(|| self.tui.as_ref().and_then(|cfg| cfg.force_http1))
            .unwrap_or(false)
            || crate::client::force_http1_from_env()
    }

    /// #6700: resolved TCP/TLS connect timeout for the model HTTP client.
    #[must_use]
    pub fn connect_timeout(&self) -> std::time::Duration {
        let secs = match self
            .stream
            .as_ref()
            .and_then(|cfg| cfg.connect_timeout_secs)
            .or_else(|| self.tui.as_ref().and_then(|cfg| cfg.connect_timeout_secs))
        {
            None | Some(0) => DEFAULT_CONNECT_TIMEOUT_SECS,
            Some(secs) => secs.clamp(MIN_CONNECT_TIMEOUT_SECS, MAX_CONNECT_TIMEOUT_SECS),
        };
        std::time::Duration::from_secs(secs)
    }

    /// TCP keepalive idle time. Zero disables; positive values clamp to 1..=3600.
    pub fn tcp_keepalive(&self) -> Option<std::time::Duration> {
        let secs = self
            .stream
            .as_ref()
            .and_then(|cfg| cfg.tcp_keepalive_secs)
            .unwrap_or(30);
        (secs > 0).then(|| std::time::Duration::from_secs(secs.min(3600)))
    }

    /// HTTP/2 PING interval for active connections (not idle pooled connections).
    pub fn http2_keep_alive_interval(&self) -> Option<std::time::Duration> {
        let secs = self
            .stream
            .as_ref()
            .and_then(|cfg| cfg.http2_keep_alive_interval_secs)
            .unwrap_or(15);
        (secs > 0).then(|| std::time::Duration::from_secs(secs.min(3600)))
    }

    pub fn http2_keep_alive_timeout(&self) -> std::time::Duration {
        let secs = match self
            .stream
            .as_ref()
            .and_then(|cfg| cfg.http2_keep_alive_timeout_secs)
        {
            None | Some(0) => 20,
            Some(secs) => secs.min(3600),
        };
        std::time::Duration::from_secs(secs)
    }

    /// Non-secret effective settings for `config dump`/`get`. This projection
    /// calls the runtime accessors, so aliases, environment and clamps cannot
    /// acquire a second policy in the dispatcher. It does not save defaults.
    pub fn resolved_stream_settings(&self) -> StreamConfig {
        let limits = self.stream_retry_limits();
        StreamConfig {
            open_timeout_secs: Some(self.stream_open_timeout().as_secs()),
            chunk_timeout_secs: Some(self.stream_chunk_timeout_secs()),
            force_http1: Some(self.force_http1()),
            max_resumes: Some(limits.max_resumes),
            max_transparent_retries: Some(limits.max_transparent_retries),
            max_stream_errors: Some(limits.max_errors),
            max_duration_secs: Some(self.stream_max_duration().as_secs()),
            max_content_mb: Some((self.stream_max_content_bytes() / (1024 * 1024)) as u64),
            connect_timeout_secs: Some(self.connect_timeout().as_secs()),
            tcp_keepalive_secs: Some(self.tcp_keepalive().map_or(0, |value| value.as_secs())),
            http2_keep_alive_interval_secs: Some(
                self.http2_keep_alive_interval()
                    .map_or(0, |value| value.as_secs()),
            ),
            http2_keep_alive_timeout_secs: Some(self.http2_keep_alive_timeout().as_secs()),
        }
    }

    /// Raw sub-agent model override map. Values are validated at spawn time
    /// so an invalid role/type model fails before any partial agent spawn.
    #[must_use]
    pub fn subagent_model_overrides(&self) -> HashMap<String, SubagentModelOverride> {
        let mut overrides = HashMap::new();
        let Some(cfg) = self.subagents.as_ref() else {
            return overrides;
        };

        let mut insert = |key: &str, value: &Option<String>| {
            if let Some(model) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                overrides.insert(key.to_string(), model.into());
            }
        };
        insert("default", &cfg.default_model);
        insert("worker", &cfg.worker_model);
        insert("general", &cfg.worker_model);
        insert("scout", &cfg.explorer_model);
        insert("explorer", &cfg.explorer_model);
        insert("explore", &cfg.explorer_model);
        insert("planner", &cfg.awaiter_model);
        insert("awaiter", &cfg.awaiter_model);
        insert("plan", &cfg.awaiter_model);
        insert("reviewer", &cfg.review_model);
        insert("review", &cfg.review_model);
        insert("custom", &cfg.custom_model);

        if let Some(models) = cfg.models.as_ref() {
            for (key, model) in models {
                let key = key.trim();
                let model = model.trim();
                if !key.is_empty() && !model.is_empty() {
                    overrides.insert(key.to_ascii_lowercase(), model.into());
                }
            }
        }

        if let Some(roles) = cfg.roles.as_ref() {
            let mut entries: Vec<_> = roles.iter().collect();
            // Apply legacy aliases first, then canonical keys, deterministically.
            // `default` is the all-role fallback, not the legacy general alias.
            let canonical = |key: &str| {
                let key = key.trim().to_ascii_lowercase();
                if key == "default" {
                    key
                } else {
                    crate::fleet::role::migrate_legacy_role_token(&key)
                        .unwrap_or(&key)
                        .to_string()
                }
            };
            entries
                .sort_by_key(|(key, _)| (canonical(key) == key.trim().to_ascii_lowercase(), *key));
            for (key, pin) in entries {
                // Keep blank explicit pins so admission rejects them rather
                // than silently inheriting a different route.
                overrides.insert(canonical(key), parse_subagent_role_pin(&pin.model));
            }
        }

        overrides
    }

    /// The operator-approved replacement routes declared beside the role pin
    /// that [`Self::subagent_model_overrides`] resolved under `key`. A
    /// canonical role key wins over a legacy alias, matching pin precedence.
    pub fn subagent_route_replacements(&self, key: &str) -> Vec<SubagentModelOverride> {
        let Some(roles) = self.subagents.as_ref().and_then(|cfg| cfg.roles.as_ref()) else {
            return Vec::new();
        };
        let canonical = |raw: &str| {
            let raw = raw.trim().to_ascii_lowercase();
            if raw == "default" {
                raw
            } else {
                crate::fleet::role::migrate_legacy_role_token(&raw)
                    .unwrap_or(&raw)
                    .to_string()
            }
        };
        roles
            .iter()
            .filter(|(raw, _)| canonical(raw) == key)
            .max_by_key(|(raw, _)| (canonical(raw) == raw.trim().to_ascii_lowercase(), *raw))
            .map(|(_, pin)| {
                pin.replacements
                    .iter()
                    .map(|route| parse_subagent_role_pin(route))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Parsed `[fleet]` table, or defaults when the table is absent
    /// (#fleet-roster cutover (v0.8.67)).
    #[must_use]
    pub fn fleet_config(&self) -> codewhale_config::FleetConfigToml {
        self.fleet.clone().unwrap_or_default()
    }

    /// The person's fleet as models (design MODEL-ROUTING-CATALOG §10 F1):
    /// every exact provider + model route in the selected fleet for
    /// `workspace`, with the roles each fills. `Ok(empty)` = the session
    /// model only; `Err` = a selected fleet that cannot be read. This is the
    /// seam the operator-awareness slice (F2) reads.
    pub fn fleet_members(
        &self,
        workspace: &Path,
    ) -> Result<Vec<crate::fleet::members::FleetModel>, crate::fleet::store::FleetStoreError> {
        crate::fleet::members::fleet_models(workspace)
    }

    /// Parsed `[workflow]` table, or product defaults when the table is absent
    /// (#4128 / Section 2.11). Automatic launch, approval, isolation, and
    /// activity-persistence consumers should read through this accessor so
    /// omitted keys share one model.
    #[must_use]
    pub fn workflow_config(&self) -> codewhale_config::WorkflowConfigToml {
        self.workflow.clone().unwrap_or_default()
    }

    /// The requested model-bound masking mode (`[redaction] model_bound`),
    /// defaulting to enabled. This is the user's *request*; the effective mode
    /// also depends on the startup-gate confirmation receipt, see
    /// [`codewhale_config::redaction::effective_masking`].
    #[must_use]
    pub fn model_bound_redaction(&self) -> codewhale_config::redaction::ModelBoundMasking {
        self.redaction
            .as_ref()
            .map(codewhale_config::redaction::RedactionToml::model_bound_masking)
            .unwrap_or_default()
    }

    /// Return the configured DeepSeek reasoning-effort tier, if any.
    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    pub(crate) fn reasoning_effort_is_explicit(&self) -> bool {
        self.reasoning_effort.is_some() && !self.reasoning_effort_inferred_from_legacy_alias
    }

    /// Get hooks configuration, returning default if not configured.
    pub fn hooks_config(&self) -> HooksConfig {
        self.hooks.clone().unwrap_or_default()
    }

    /// Resolve the notifications configuration with defaults applied.
    #[must_use]
    pub fn notifications_config(&self) -> NotificationsConfig {
        let mut notifications = self.notifications.clone().unwrap_or_default();
        notifications.condition = Some(
            notifications
                .condition
                .or_else(|| self.tui.as_ref().and_then(|tui| tui.notification_condition))
                .unwrap_or(NotificationCondition::Unfocused),
        );
        notifications
    }

    /// Resolve which approval option a fresh card highlights (#5293).
    #[must_use]
    pub fn approval_default_selection(&self) -> ApprovalDefaultSelection {
        self.approval.unwrap_or_default().default_selection
    }

    /// Effective expiry for the Engine-held approval request (#6101).
    /// `None` (absent or an explicit `0`) waits indefinitely; a positive
    /// value bounds the wait, including a hidden card, and expiry blocks the call.
    /// Values above 24h clamp with a warning.
    #[must_use]
    pub fn approval_timeout(&self) -> Option<std::time::Duration> {
        const MAX_SECONDS: u64 = 86_400;
        let seconds = self.approval.unwrap_or_default().timeout_seconds?;
        if seconds == 0 {
            return None;
        }
        if seconds > MAX_SECONDS {
            tracing::warn!(
                "[approval] timeout_seconds={seconds} exceeds 24h; clamping to {MAX_SECONDS}"
            );
        }
        Some(std::time::Duration::from_secs(seconds.min(MAX_SECONDS)))
    }

    /// Resolve workspace side-git snapshot settings with defaults applied.
    #[must_use]
    pub fn snapshots_config(&self) -> SnapshotsConfig {
        self.snapshots.clone().unwrap_or_default()
    }

    /// Resolve community skill settings with defaults applied.
    #[must_use]
    pub fn skills_config(&self) -> SkillsConfig {
        self.skills.clone().unwrap_or_default()
    }

    /// Resolve startup update-check settings with defaults applied.
    #[must_use]
    pub fn update_config(&self) -> UpdateConfig {
        self.update.clone().unwrap_or_default()
    }

    /// Resolve cloud facts settings with defaults applied (off by default).
    #[must_use]
    pub fn cloud_facts_config(&self) -> CloudFactsConfig {
        self.cloud_facts.clone().unwrap_or_default()
    }

    /// Resolve durable hotbar bindings for render/dispatch layers.
    #[must_use]
    pub fn resolve_hotbar_bindings(
        &self,
        known_action_ids: &[&str],
    ) -> codewhale_config::HotbarConfigResolution {
        codewhale_config::resolve_hotbar_bindings(self.hotbar.as_deref(), known_action_ids)
    }

    /// Resolve enabled features from defaults and config entries.
    #[must_use]
    pub fn features(&self) -> Features {
        let mut features = Features::with_defaults();
        if let Some(table) = &self.features {
            features.apply_map(&table.entries);
        }
        features
    }

    /// Override a feature flag in memory (used by CLI overrides).
    pub fn set_feature(&mut self, key: &str, enabled: bool) -> Result<()> {
        if !is_known_feature_key(key) {
            anyhow::bail!("Unknown feature flag: {key}");
        }
        let table = self.features.get_or_insert_with(FeaturesToml::default);
        table.entries.insert(key.to_string(), enabled);
        Ok(())
    }

    /// Resolve the effective retry policy with defaults applied.
    #[must_use]
    pub fn retry_policy(&self) -> RetryPolicy {
        let defaults = RetryPolicy {
            enabled: true,
            max_retries: 3,
            initial_delay: 1.0,
            max_delay: 60.0,
            exponential_base: 2.0,
            jitter: true,
            jitter_factor: 0.1,
            respect_retry_after: true,
        };

        let Some(cfg) = &self.retry else {
            return defaults;
        };

        RetryPolicy {
            enabled: cfg.enabled.unwrap_or(defaults.enabled),
            max_retries: cfg.max_retries.unwrap_or(defaults.max_retries),
            initial_delay: cfg.initial_delay.unwrap_or(defaults.initial_delay),
            max_delay: cfg.max_delay.unwrap_or(defaults.max_delay),
            exponential_base: cfg.exponential_base.unwrap_or(defaults.exponential_base),
            jitter: cfg.jitter.unwrap_or(defaults.jitter),
            jitter_factor: cfg
                .jitter_factor
                .filter(|factor| factor.is_finite())
                .map_or(defaults.jitter_factor, |factor| factor.clamp(0.0, 1.0)),
            respect_retry_after: cfg
                .respect_retry_after
                .unwrap_or(defaults.respect_retry_after),
        }
    }
}

/// Controls whether configuration loading may copy secret-bearing environment
/// values into the in-memory configuration.
///
/// Structural diagnostics intentionally retain safe environment routing and
/// policy fields while refusing values that could be secrets when later
/// rendered or included in an error path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigEnvironmentPolicy {
    Runtime,
    StructuralDiagnostic,
}

impl ConfigEnvironmentPolicy {
    const fn permits_secret_bearing_values(self) -> bool {
        matches!(self, Self::Runtime)
    }
}

fn root_deepseek_model_is_foreign_to_direct_provider(provider: ProviderKind, model: &str) -> bool {
    if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) || provider_passes_model_through(provider)
    {
        return false;
    }
    if matches!(
        provider,
        ProviderKind::NvidiaNim
            | ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::Novita
            | ProviderKind::Fireworks
            | ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Deepinfra
            | ProviderKind::Together
            | ProviderKind::Sglang
            | ProviderKind::Vllm
            | ProviderKind::Volcengine
            | ProviderKind::Atlascloud
            | ProviderKind::OpencodeGo
            | ProviderKind::WanjieArk
    ) {
        return false;
    }
    normalize_model_name(model).is_some()
}

// === Defaults ===

// Pure filesystem path helpers live in the `paths` leaf module. Shared
// entry points are re-exported so external `crate::config::`
// callers resolve unchanged; the remaining helpers are imported privately for
// the workspace-trust/config-load logic that stays in this file (#3311).
mod home;
mod paths;
#[cfg(test)]
pub(crate) use paths::clipboard_images_dir_for_home;
use paths::{
    canonicalize_or_keep, codewhale_home_dir, default_config_path, default_managed_config_path,
    default_mcp_config_path, default_memory_path, default_notes_path, default_requirements_path,
    default_skills_dir, env_config_path, expand_pathbuf, try_default_config_path,
    workspace_config_key,
};
pub(crate) use paths::{
    clipboard_images_dir, effective_home_dir, expand_path, home_config_path, is_home_config_path,
};

/// Activate facts only at accepted inference runtime settings boundaries.
/// Identical settings preserve the shared ticket; readers track its generation.
pub(crate) fn initialize_cloud_facts(config: &Config) {
    let settings = config.cloud_facts_config().settings();
    codewhale_cloud_facts::configure(&settings);
    codewhale_cloud_facts::maybe_load_persisted_cache(&settings);
    if tokio::runtime::Handle::try_current().is_ok() {
        codewhale_cloud_facts::spawn_background_refresh(settings, None);
    }
}

pub(crate) fn workspace_trust_config_candidate_paths() -> Vec<PathBuf> {
    #[cfg(test)]
    {
        if !crate::test_support::guarded_environment_provides_state_paths() {
            return vec![
                crate::test_support::unsealed_test_state_root()
                    .join(codewhale_config::CONFIG_FILE_NAME),
            ];
        }
    }

    match env_config_path() {
        Ok(Some(path)) => return vec![path],
        Ok(None) => {}
        Err(error) => {
            tracing::error!(
                error = %error,
                "invalid config path override; refusing workspace-trust fallback"
            );
            return Vec::new();
        }
    }

    match codewhale_home_dir() {
        Ok(Some(codewhale_home)) => return vec![codewhale_home.join("config.toml")],
        Ok(None) => {}
        Err(error) => {
            tracing::error!(
                error = %error,
                "invalid Codewhale home override; refusing workspace-trust fallback"
            );
            return Vec::new();
        }
    }

    let Some(home) = effective_home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".codewhale").join("config.toml"),
        home.join(".deepseek").join("config.toml"),
    ]
}

#[must_use]
pub(crate) fn is_workspace_trusted(workspace: &Path) -> bool {
    let config_path = match default_config_path() {
        Ok(path) => path,
        Err(error) => {
            tracing::error!(
                error = %error,
                "failed to resolve workspace-trust config; treating workspace as untrusted"
            );
            return false;
        }
    };
    let Ok(raw) = fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return false;
    };
    workspace_trust_level_from_doc(&doc, workspace).is_some_and(is_trusted_level)
}

pub(crate) fn save_workspace_trust(workspace: &Path) -> Result<PathBuf> {
    set_workspace_trust(workspace, true)
}

pub(crate) fn set_workspace_trust(workspace: &Path, trusted: bool) -> Result<PathBuf> {
    let config_path =
        try_default_config_path().context("Failed to resolve config path for workspace trust.")?;
    ensure_parent_dir(&config_path)?;

    let project_key = workspace_config_key(workspace);
    crate::config_persistence::mutate_config_document(&config_path, |doc| {
        crate::config_persistence::set_document_value(
            doc,
            &["projects", project_key.as_str(), "trust_level"],
            if trusted { "trusted" } else { "untrusted" },
        )
    })
    .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    Ok(config_path)
}

/// Project hook approval lives only in user-owned config, never in the repo.
pub(crate) fn hook_receipt_for_workspace(workspace: &Path) -> Option<String> {
    let raw = fs::read_to_string(default_config_path().ok()?).ok()?;
    let doc = toml::from_str::<toml::Value>(&raw).ok()?;
    doc.get("projects")?
        .get(workspace_config_key(workspace))?
        .get("hooks_sha256")?
        .as_str()
        .map(str::to_owned)
}

pub(crate) fn save_workspace_hook_receipt(workspace: &Path, digest: &str) -> Result<PathBuf> {
    let config_path = try_default_config_path()?;
    ensure_parent_dir(&config_path)?;
    let project_key = workspace_config_key(workspace);
    crate::config_persistence::mutate_config_document(&config_path, |doc| {
        crate::config_persistence::set_document_value(
            doc,
            &["projects", project_key.as_str(), "hooks_sha256"],
            digest,
        )
    })?;
    Ok(config_path)
}

fn workspace_trust_level_from_doc<'a>(doc: &'a toml::Value, workspace: &Path) -> Option<&'a str> {
    let workspace = canonicalize_or_keep(workspace);
    // Trust records may sit at the top level or — from the historic
    // extras-nesting write bug (healed on mutation since 2026-07-23) — under
    // one or more literal `extras` tables. Read tolerantly so a not-yet-
    // healed config file still recognizes its trusted workspaces.
    let mut scope = Some(doc);
    while let Some(current) = scope {
        if let Some(projects) = current.get("projects").and_then(toml::Value::as_table) {
            for (raw_path, project) in projects {
                let project_path = canonicalize_or_keep(&expand_path(raw_path));
                if project_path == workspace {
                    return project.get("trust_level").and_then(toml::Value::as_str);
                }
            }
        }
        scope = current.get("extras");
    }
    None
}

fn is_trusted_level(level: &str) -> bool {
    level.trim().eq_ignore_ascii_case("trusted")
}

pub(crate) fn resolve_load_config_path(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    if let Some(path) = path {
        return Ok(Some(expand_pathbuf(path)));
    }

    try_default_config_path().map(Some)
}

/// Create an inspectable config file on first interactive launch.
///
/// The file intentionally omits `api_key`; onboarding or `codewhale auth set`
/// writes that field after the user supplies a key.
pub fn ensure_config_file_exists(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let config_path = match path {
        Some(path) => expand_pathbuf(path),
        None => default_config_path().context("Failed to resolve config path.")?,
    };
    if config_path.exists() {
        return Ok(None);
    }

    ensure_parent_dir(&config_path)?;
    let content = format!(
        r#"# codewhale Configuration
# Get your API key from https://platform.deepseek.com
# Save it with: codewhale auth set --provider deepseek

# Base URL (default: https://api.deepseek.com/beta)
# Set https://api.deepseek.com to opt out of beta features.
# base_url = "https://api.deepseek.com/beta"

# Default model
default_text_model = "{DEFAULT_TEXT_MODEL}"

# Thinking mode (DeepSeek V4 reasoning effort):
# "auto" | "off" | "low" | "medium" | "high" | "max"
# Ctrl+T in the TUI (or /effort) cycles the active model's effort levels.
reasoning_effort = "auto"

# Startup update check
[update]
check_for_updates = true
# check_interval_hours = 1
# update_uri = "https://internal.mirror.example/codewhale/releases/latest"
"#
    );
    write_config_file_secure(&config_path, &content)
        .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    Ok(Some(config_path))
}

// === Environment Overrides ===

/// Read the `CODEWHALE_BASE_URL` env var that the CLI dispatcher forwards from
/// `--base-url`, or the user-set legacy `DEEPSEEK_BASE_URL` alias.  Returns `None` when the var is
/// absent or empty so that provider-specific defaults still apply.
fn env_base_url_override() -> Option<String> {
    codewhale_env_var("CODEWHALE_BASE_URL", "DEEPSEEK_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn first_nonempty_env(names: &[&str]) -> Option<String> {
    let read = || {
        names.iter().find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
    };
    #[cfg(test)]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(test))]
    {
        read()
    }
}

/// Return the provider-scoped endpoint override that `apply_env_overrides`
/// will apply to the active route. This is intentionally kept beside the
/// mutation code: after the write, a provider-table `base_url` no longer
/// carries enough information to distinguish a file-owned route from an
/// environment-selected host.
fn provider_env_base_url_override(provider: ProviderKind) -> Option<String> {
    let names: &[&str] = match provider {
        ProviderKind::NvidiaNim => &["NVIDIA_NIM_BASE_URL", "NIM_BASE_URL", "NVIDIA_BASE_URL"],
        ProviderKind::Openai => &["OPENAI_BASE_URL"],
        ProviderKind::Atlascloud => &["ATLASCLOUD_BASE_URL"],
        ProviderKind::Openrouter => &["OPENROUTER_BASE_URL"],
        // The inference/catalog origin. OrcaRouter's **auth** origin is a
        // different host and is resolved by
        // `crate::oauth::resolve_orcarouter_auth_base`; the two never derive
        // from each other.
        ProviderKind::Orcarouter => &["ORCA_API_BASE_URL", "ORCA_BASE_URL", "ORCAROUTER_BASE_URL"],
        ProviderKind::XiaomiMimo => &["XIAOMI_MIMO_BASE_URL", "MIMO_BASE_URL"],
        ProviderKind::WanjieArk => &[
            "WANJIE_ARK_BASE_URL",
            "WANJIE_BASE_URL",
            "WANJIE_MAAS_BASE_URL",
        ],
        ProviderKind::Volcengine => &[
            "VOLCENGINE_BASE_URL",
            "VOLCENGINE_ARK_BASE_URL",
            "ARK_BASE_URL",
        ],
        ProviderKind::Novita => &["NOVITA_BASE_URL"],
        ProviderKind::Fireworks => &["FIREWORKS_BASE_URL"],
        ProviderKind::Siliconflow | ProviderKind::SiliconflowCN => &["SILICONFLOW_BASE_URL"],
        ProviderKind::Arcee => &["ARCEE_BASE_URL"],
        ProviderKind::Moonshot => &["MOONSHOT_BASE_URL", "KIMI_BASE_URL"],
        ProviderKind::Sglang => &["SGLANG_BASE_URL"],
        ProviderKind::Vllm => &["VLLM_BASE_URL"],
        ProviderKind::Ollama => &["OLLAMA_BASE_URL"],
        ProviderKind::OllamaCloud => &["OLLAMA_CLOUD_BASE_URL"],
        ProviderKind::Huggingface => &["HUGGINGFACE_BASE_URL", "HF_BASE_URL"],
        ProviderKind::Modelscope => &["MODELSCOPE_BASE_URL"],
        ProviderKind::Meta => &["META_MODEL_API_BASE_URL", "MODEL_API_BASE_URL"],
        ProviderKind::Xai => &["XAI_BASE_URL"],
        ProviderKind::Mistral => &["MISTRAL_BASE_URL"],
        ProviderKind::Google => &["GOOGLE_BASE_URL", "GEMINI_BASE_URL"],
        ProviderKind::Antigravity => &[],
        ProviderKind::Telecomjs => &["TELECOMJS_BASE_URL"],
        ProviderKind::Edenai => &["EDENAI_BASE_URL"],
        ProviderKind::Zenmux => &["ZENMUX_BASE_URL"],
        ProviderKind::Csdn => &["CSDN_BASE_URL"],
        ProviderKind::Concentrate => &["CONCENTRATE_BASE_URL"],
        ProviderKind::Codewhale => &["CODEWHALE_API_BASE"],
        ProviderKind::ModelstudioTokenPlan | ProviderKind::ModelstudioTokenPlanAnthropic => {
            &["MODELSTUDIO_TOKEN_PLAN_BASE_URL"]
        }
        ProviderKind::ModelstudioCodingPlan | ProviderKind::ModelstudioCodingPlanAnthropic => {
            &["MODELSTUDIO_CODING_PLAN_BASE_URL"]
        }
        ProviderKind::OpencodeGo => &["OPENCODE_GO_BASE_URL"],
        ProviderKind::OpencodeZen => &["OPENCODE_ZEN_BASE_URL"],
        ProviderKind::Deepseek
        | ProviderKind::DeepseekAnthropic
        | ProviderKind::Anthropic
        | ProviderKind::Openmodel
        | ProviderKind::Deepinfra
        | ProviderKind::Together
        | ProviderKind::Qianfan
        | ProviderKind::OpenaiCodex
        | ProviderKind::Zai
        | ProviderKind::Stepfun
        | ProviderKind::Minimax
        | ProviderKind::MinimaxAnthropic
        | ProviderKind::Sakana
        | ProviderKind::LongCat
        | ProviderKind::Custom => &[],
    };
    // `CODEWHALE_API_BASE` carries a `cwc_key_…` bearer, which has no replay
    // protection, so it is a trust boundary rather than a plain string: an
    // origin the account surface would refuse is dropped here instead of
    // becoming a route, and the workspace-wide insecure-HTTP escape hatch
    // deliberately does not reopen it.
    if provider == ProviderKind::Codewhale {
        return first_nonempty_env(names)
            .as_deref()
            .and_then(codewhale_config::provider::codewhale_api_base);
    }
    first_nonempty_env(names)
}

/// Resolve an env var, preferring the `CODEWHALE_*` form over the
/// legacy `DEEPSEEK_*` form. Empty values are ignored so a blank shell export
/// does not erase configured provider settings.
fn codewhale_env_var(
    codewhale_name: &str,
    legacy_name: &str,
) -> Result<String, std::env::VarError> {
    let read = || {
        std::env::var(codewhale_name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var(legacy_name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .ok_or(std::env::VarError::NotPresent)
    };
    #[cfg(test)]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(test))]
    {
        read()
    }
}

fn apply_env_overrides(config: &mut Config, policy: ConfigEnvironmentPolicy) {
    #[cfg(test)]
    {
        crate::test_support::with_test_env_lock(|| {
            apply_env_overrides_unlocked(config, policy);
        })
    }
    #[cfg(not(test))]
    {
        apply_env_overrides_unlocked(config, policy);
    }
}

fn apply_env_overrides_unlocked(config: &mut Config, policy: ConfigEnvironmentPolicy) {
    if let Ok(value) = codewhale_env_var("CODEWHALE_PROVIDER", "DEEPSEEK_PROVIDER") {
        config.provider = Some(value);
    }
    let Ok(identity) = config.active_provider_identity() else {
        return;
    };
    let active_provider = identity.provider;
    let active_base_url_from_env = env_base_url_override().is_some()
        || provider_env_base_url_override(active_provider).is_some()
        || (config.selects_legacy_ollama_cloud_route()
            && first_nonempty_env(&["OLLAMA_BASE_URL"]).is_some());
    let changes_root_authority = active_base_url_from_env
        || (policy.permits_secret_bearing_values()
            && std::env::var("CODEWHALE_HTTP_HEADERS")
                .or_else(|_| std::env::var("DEEPSEEK_HTTP_HEADERS"))
                .is_ok());
    let identity = if identity.legacy_root_custom_generation.is_some() && changes_root_authority {
        let Ok(exact) = config.resolve_exact_provider_identity(identity.key.as_str()) else {
            return;
        };
        config.legacy_root_custom_generation = None;
        exact
    } else {
        identity
    };
    if let Ok(value) = codewhale_env_var("CODEWHALE_BASE_URL", "DEEPSEEK_BASE_URL")
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::NvidiaNim)
        && let Ok(value) = std::env::var("NVIDIA_NIM_BASE_URL")
            .or_else(|_| std::env::var("NIM_BASE_URL"))
            .or_else(|_| std::env::var("NVIDIA_BASE_URL"))
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    // OpenAI-compatible and non-DeepSeek hosted providers are scoped only on
    // their own provider entry.
    if matches!(active_provider, ProviderKind::Openai)
        && let Ok(value) = std::env::var("OPENAI_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Atlascloud)
        && let Ok(value) = std::env::var("ATLASCLOUD_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Openrouter)
        && let Ok(value) = std::env::var("OPENROUTER_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::XiaomiMimo)
        && let Ok(value) =
            std::env::var("XIAOMI_MIMO_BASE_URL").or_else(|_| std::env::var("MIMO_BASE_URL"))
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::XiaomiMimo)
        && let Ok(value) = std::env::var("XIAOMI_MIMO_MODE").or_else(|_| std::env::var("MIMO_MODE"))
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.mode = Some(value);
    }
    if matches!(active_provider, ProviderKind::WanjieArk)
        && let Ok(value) = std::env::var("WANJIE_ARK_BASE_URL")
            .or_else(|_| std::env::var("WANJIE_BASE_URL"))
            .or_else(|_| std::env::var("WANJIE_MAAS_BASE_URL"))
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Volcengine)
        && let Ok(value) = std::env::var("VOLCENGINE_BASE_URL")
            .or_else(|_| std::env::var("VOLCENGINE_ARK_BASE_URL"))
            .or_else(|_| std::env::var("ARK_BASE_URL"))
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Novita)
        && let Ok(value) = std::env::var("NOVITA_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Fireworks)
        && let Ok(value) = std::env::var("FIREWORKS_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(
        active_provider,
        ProviderKind::Siliconflow | ProviderKind::SiliconflowCN
    ) && let Ok(value) = std::env::var("SILICONFLOW_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Arcee)
        && let Ok(value) = std::env::var("ARCEE_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Huggingface)
        && let Ok(value) =
            std::env::var("HUGGINGFACE_BASE_URL").or_else(|_| std::env::var("HF_BASE_URL"))
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Modelscope)
        && let Ok(value) = std::env::var("MODELSCOPE_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Moonshot)
        && let Ok(value) =
            std::env::var("MOONSHOT_BASE_URL").or_else(|_| std::env::var("KIMI_BASE_URL"))
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Sglang)
        && let Ok(value) = std::env::var("SGLANG_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Vllm)
        && let Ok(value) = std::env::var("VLLM_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Meta)
        && let Ok(value) = std::env::var("META_MODEL_API_BASE_URL")
            .or_else(|_| std::env::var("MODEL_API_BASE_URL"))
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Xai)
        && let Ok(value) = std::env::var("XAI_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Mistral)
        && let Ok(value) = std::env::var("MISTRAL_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Telecomjs)
        && let Ok(value) = std::env::var("TELECOMJS_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Edenai)
        && let Ok(value) = std::env::var("EDENAI_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Zenmux)
        && let Ok(value) = std::env::var("ZENMUX_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Csdn)
        && let Ok(value) = std::env::var("CSDN_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    // Concentrate has no inline block here on purpose: CONCENTRATE_BASE_URL
    // is already served by `provider_env_base_url_override`, which the route
    // resolver consults — a second inline assignment was a duplicate.
    if matches!(
        active_provider,
        ProviderKind::ModelstudioTokenPlan | ProviderKind::ModelstudioTokenPlanAnthropic
    ) && let Ok(value) = std::env::var("MODELSTUDIO_TOKEN_PLAN_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(
        active_provider,
        ProviderKind::ModelstudioCodingPlan | ProviderKind::ModelstudioCodingPlanAnthropic
    ) && let Ok(value) = std::env::var("MODELSTUDIO_CODING_PLAN_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if policy.permits_secret_bearing_values()
        && let Ok(value) = std::env::var("CODEWHALE_HTTP_HEADERS")
            .or_else(|_| std::env::var("DEEPSEEK_HTTP_HEADERS"))
        && let Ok(headers) = parse_http_headers(&value)
        && !headers.is_empty()
    {
        let mut root_headers = config.http_headers.clone().unwrap_or_default();
        root_headers.extend(headers.clone());
        config.http_headers = Some(root_headers);

        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            let mut provider_headers = entry.http_headers.clone().unwrap_or_default();
            provider_headers.extend(headers);
            entry.http_headers = Some(provider_headers);
        }
    }
    if config.provider.as_deref().and_then(ProviderKind::parse) == Some(ProviderKind::Ollama)
        && let Ok(value) = std::env::var("OLLAMA_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::OllamaCloud)
        && config.provider.as_deref().and_then(ProviderKind::parse)
            == Some(ProviderKind::OllamaCloud)
        && let Ok(value) = std::env::var("OLLAMA_CLOUD_BASE_URL")
        && !value.trim().is_empty()
        && let Ok(entry) = config.provider_config_for_mut(&identity)
    {
        entry.base_url = Some(value);
    }
    if matches!(active_provider, ProviderKind::Sglang)
        && let Ok(value) = std::env::var("SGLANG_MODEL")
    {
        config.default_text_model = Some(value.clone());
        config
            .set_provider_model_override(&identity, Some(value))
            .unwrap();
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Vllm)
        && let Ok(value) = std::env::var("VLLM_MODEL")
    {
        config.default_text_model = Some(value.clone());
        config
            .set_provider_model_override(&identity, Some(value))
            .unwrap();
        config.environment_model_applied = true;
    }
    if matches!(
        active_provider,
        ProviderKind::Ollama | ProviderKind::OllamaCloud
    ) && let Ok(value) = std::env::var("OLLAMA_MODEL")
    {
        config.default_text_model = Some(value.clone());
        config
            .set_provider_model_override(&identity, Some(value))
            .unwrap();
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::OllamaCloud)
        && let Ok(value) = std::env::var("OLLAMA_CLOUD_MODEL")
    {
        config.default_text_model = Some(value.clone());
        config
            .set_provider_model_override(&identity, Some(value))
            .unwrap();
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Openai)
        && let Ok(value) = std::env::var("OPENAI_MODEL")
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::XiaomiMimo)
        && let Ok(value) =
            std::env::var("XIAOMI_MIMO_MODEL").or_else(|_| std::env::var("MIMO_MODEL"))
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Atlascloud)
        && let Ok(value) = std::env::var("ATLASCLOUD_MODEL")
    {
        config.default_text_model = Some(value.clone());
        config
            .set_provider_model_override(&identity, Some(value))
            .unwrap();
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::WanjieArk)
        && let Ok(value) = std::env::var("WANJIE_ARK_MODEL")
            .or_else(|_| std::env::var("WANJIE_MODEL"))
            .or_else(|_| std::env::var("WANJIE_MAAS_MODEL"))
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Openrouter)
        && let Ok(value) = std::env::var("OPENROUTER_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Volcengine)
        && let Ok(value) =
            std::env::var("VOLCENGINE_MODEL").or_else(|_| std::env::var("VOLCENGINE_ARK_MODEL"))
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Novita)
        && let Ok(value) = std::env::var("NOVITA_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Fireworks)
        && let Ok(value) = std::env::var("FIREWORKS_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Moonshot)
        && let Ok(value) = std::env::var("MOONSHOT_MODEL")
            .or_else(|_| std::env::var("KIMI_MODEL_NAME"))
            .or_else(|_| std::env::var("KIMI_MODEL"))
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(
        active_provider,
        ProviderKind::Siliconflow | ProviderKind::SiliconflowCN
    ) && let Ok(value) = std::env::var("SILICONFLOW_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Arcee)
        && let Ok(value) = std::env::var("ARCEE_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Huggingface)
        && let Ok(value) = std::env::var("HUGGINGFACE_MODEL").or_else(|_| std::env::var("HF_MODEL"))
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Modelscope)
        && let Ok(value) = std::env::var("MODELSCOPE_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Meta)
        && let Ok(value) =
            std::env::var("META_MODEL_API_MODEL").or_else(|_| std::env::var("MODEL_API_MODEL"))
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Xai)
        && let Ok(value) = std::env::var("XAI_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Mistral)
        && let Ok(value) = std::env::var("MISTRAL_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::OpencodeGo)
        && let Ok(value) = std::env::var("OPENCODE_GO_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Telecomjs)
        && let Ok(value) = std::env::var("TELECOMJS_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Concentrate)
        && let Ok(value) = std::env::var("CONCENTRATE_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Edenai)
        && let Ok(value) = std::env::var("EDENAI_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Zenmux)
        && let Ok(value) = std::env::var("ZENMUX_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::Csdn)
        && let Ok(value) = std::env::var("CSDN_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(
        active_provider,
        ProviderKind::ModelstudioTokenPlan | ProviderKind::ModelstudioTokenPlanAnthropic
    ) && let Ok(value) = std::env::var("MODELSTUDIO_TOKEN_PLAN_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(
        active_provider,
        ProviderKind::ModelstudioCodingPlan | ProviderKind::ModelstudioCodingPlanAnthropic
    ) && let Ok(value) = std::env::var("MODELSTUDIO_CODING_PLAN_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::OpencodeZen)
        && let Ok(value) = std::env::var("OPENCODE_ZEN_MODEL")
        && !value.trim().is_empty()
    {
        if let Ok(entry) = config.provider_config_for_mut(&identity) {
            entry.model = Some(value);
        }
        config.environment_model_applied = true;
    }
    if matches!(active_provider, ProviderKind::NvidiaNim)
        && let Ok(value) = std::env::var("NVIDIA_NIM_MODEL")
    {
        config.default_text_model = Some(value.clone());
        config
            .set_provider_model_override(&identity, Some(value))
            .unwrap();
        config.environment_model_applied = true;
    }
    if let Some(value) = codewhale_env_var("CODEWHALE_MODEL", "DEEPSEEK_MODEL")
        .ok()
        .or_else(|| {
            std::env::var("DEEPSEEK_DEFAULT_TEXT_MODEL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
    {
        if matches!(
            active_provider,
            ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
        ) {
            config.default_text_model = Some(value.clone());
        }
        config
            .set_provider_model_override(&identity, Some(value))
            .unwrap();
        config.environment_model_applied = true;
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_SKILLS_DIR").or_else(|_| std::env::var("DEEPSEEK_SKILLS_DIR"))
    {
        config.skills_dir = Some(value);
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_MCP_CONFIG").or_else(|_| std::env::var("DEEPSEEK_MCP_CONFIG"))
    {
        config.mcp_config_path = Some(value);
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_NOTES_PATH").or_else(|_| std::env::var("DEEPSEEK_NOTES_PATH"))
    {
        config.notes_path = Some(value);
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_MEMORY_PATH").or_else(|_| std::env::var("DEEPSEEK_MEMORY_PATH"))
    {
        config.memory_path = Some(value);
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_MEMORY").or_else(|_| std::env::var("DEEPSEEK_MEMORY"))
    {
        let on = matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "on" | "true" | "yes" | "y" | "enabled"
        );
        config
            .memory
            .get_or_insert_with(MemoryConfig::default)
            .enabled = Some(on);
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_ALLOW_SHELL").or_else(|_| std::env::var("DEEPSEEK_ALLOW_SHELL"))
    {
        config.allow_shell = Some(value == "1" || value.eq_ignore_ascii_case("true"));
    }
    if let Ok(value) = std::env::var("CODEWHALE_APPROVAL_POLICY")
        .or_else(|_| std::env::var("DEEPSEEK_APPROVAL_POLICY"))
    {
        config.approval_policy = Some(value);
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_SANDBOX_MODE").or_else(|_| std::env::var("DEEPSEEK_SANDBOX_MODE"))
    {
        config.sandbox_mode = Some(value);
    }
    if let Ok(value) = std::env::var("CODEWHALE_SANDBOX_NETWORK_ACCESS")
        .or_else(|_| std::env::var("DEEPSEEK_SANDBOX_NETWORK_ACCESS"))
    {
        config.sandbox_network_access = Some(value == "1" || value.eq_ignore_ascii_case("true"));
    }
    if let Ok(value) = std::env::var("CODEWHALE_PROJECT_INSTRUCTION_IMPORTS") {
        config.project_instruction_imports = value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
    }
    // `DEEPSEEK_YOLO` is a read-only deprecated alias of `CODEWHALE_YOLO`
    // (removable in 0.10 per issue #5443); `CODEWHALE_YOLO` wins when both
    // are set.
    if let Ok(value) = std::env::var("CODEWHALE_YOLO").or_else(|_| std::env::var("DEEPSEEK_YOLO")) {
        config.yolo = Some(value == "1" || value.eq_ignore_ascii_case("true"));
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_VERBOSITY").or_else(|_| std::env::var("DEEPSEEK_VERBOSITY"))
    {
        config.verbosity = Some(value);
    }
    if let Ok(value) = std::env::var("CODEWHALE_SANDBOX_BACKEND")
        .or_else(|_| std::env::var("DEEPSEEK_SANDBOX_BACKEND"))
    {
        config.sandbox_backend = Some(value);
    }
    if let Ok(value) = codewhale_env_var("CODEWHALE_PREFER_BWRAP", "DEEPSEEK_PREFER_BWRAP") {
        let primary_is_set = std::env::var("CODEWHALE_PREFER_BWRAP")
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
        let legacy_is_set = std::env::var("DEEPSEEK_PREFER_BWRAP")
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
        if !primary_is_set && legacy_is_set {
            tracing::warn!(
                "DEEPSEEK_PREFER_BWRAP is deprecated; use CODEWHALE_PREFER_BWRAP (the legacy alias is removed in 0.10.0)"
            );
        }
        config.prefer_bwrap = Some(value == "1" || value.eq_ignore_ascii_case("true"));
    }
    if let Ok(value) =
        std::env::var("CODEWHALE_SANDBOX_URL").or_else(|_| std::env::var("DEEPSEEK_SANDBOX_URL"))
    {
        config.sandbox_url = Some(value);
    }
    if policy.permits_secret_bearing_values()
        && let Ok(value) = std::env::var("CODEWHALE_SANDBOX_API_KEY")
            .or_else(|_| std::env::var("DEEPSEEK_SANDBOX_API_KEY"))
    {
        config.sandbox_api_key = Some(value);
    }
    if let Ok(value) = std::env::var("CODEWHALE_MANAGED_CONFIG_PATH")
        .or_else(|_| std::env::var("DEEPSEEK_MANAGED_CONFIG_PATH"))
    {
        config.managed_config_path = Some(value);
    }
    if policy.permits_secret_bearing_values()
        && let Ok(value) = std::env::var("CODEWHALE_SEARCH_API_KEY")
            .or_else(|_| std::env::var("DEEPSEEK_SEARCH_API_KEY"))
        && !value.trim().is_empty()
    {
        config
            .search
            .get_or_insert_with(SearchConfig::default)
            .api_key = Some(value);
    }
    if let Ok(value) = codewhale_env_var("CODEWHALE_SEARCH_BASE_URL", "DEEPSEEK_SEARCH_BASE_URL") {
        config
            .search
            .get_or_insert_with(SearchConfig::default)
            .base_url = Some(value);
    }
    if let Ok(value) = std::env::var("CODEWHALE_REQUIREMENTS_PATH")
        .or_else(|_| std::env::var("DEEPSEEK_REQUIREMENTS_PATH"))
    {
        config.requirements_path = Some(value);
    }
    if let Ok(value) = std::env::var("CODEWHALE_MAX_SUBAGENTS")
        .or_else(|_| std::env::var("DEEPSEEK_MAX_SUBAGENTS"))
        && let Ok(parsed) = value.parse::<usize>()
    {
        config.max_subagents = Some(parsed.clamp(1, MAX_SUBAGENTS));
    }
    // Always leave a receipt: "the environment layer ran and nobody owns the
    // base URL" is a different, stronger statement than "no receipt", and only
    // the explicit form stops a pinned cross-provider child from treating the
    // ambient generic host as a global fallback.
    config.base_url_env_receipt = if active_base_url_from_env {
        BaseUrlEnvReceipt::Route(active_provider, identity.key.to_string())
    } else {
        BaseUrlEnvReceipt::NoOwner
    };
}

fn normalize_model_config(config: &mut Config) {
    if config.default_text_model.is_none() {
        config.default_text_model.clone_from(&config.legacy_model);
    }
    let Ok(identity) = config.active_provider_identity() else {
        return;
    };
    let provider = identity.provider;
    let base_url = config.active_route_base_url();
    let mut declared = Vec::new();
    for row in codewhale_config::descriptors::provider_compatibility() {
        let Ok(candidate) = config.resolve_persisted_provider_identity(Some(row.id), Some(row.id))
        else {
            continue;
        };
        for model in config.custom_models.as_deref().unwrap_or_default() {
            if crate::provider_lake::configured_model_for_route(
                config,
                candidate.provider,
                candidate.key.as_str(),
                &config.base_url_for_route(&candidate),
                &model.id,
            )
            .is_some()
            {
                declared.push((candidate.key.clone(), model.id.clone()));
            }
        }
    }
    let is_declared = |key: &str, model: &str| {
        declared
            .iter()
            .any(|(id, value)| id.as_str() == key && value == model)
    };
    config.migrated_deepseek_model_alias = if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        config
            .active_configured_model_id()
            .filter(|model| !is_declared(identity.key.as_str(), model))
            .map(str::to_ascii_lowercase)
            .filter(|model| deepseek_alias_deprecation(model).is_some())
            .filter(|model| {
                wire_model_for_provider_route(provider, &base_url, model) != model.as_str()
            })
    } else {
        None
    };

    // Preserve the behavioral half of DeepSeek's retired aliases while
    // migrating their model id to V4 Flash. An explicit reasoning setting is
    // authoritative; this compatibility default only fills an omitted value.
    // Custom endpoints retain both their model id and their own semantics.
    if config.reasoning_effort.is_none() {
        let alias_effort = config
            .migrated_deepseek_model_alias
            .as_deref()
            .and_then(legacy_deepseek_alias_reasoning_effort);
        if let Some(effort) = alias_effort {
            config.reasoning_effort = Some(effort.to_string());
            config.reasoning_effort_inferred_from_legacy_alias = true;
        }
    }

    if let Some(model) = config.default_text_model.as_deref()
        && !is_declared(identity.key.as_str(), model)
        && !provider_passes_model_through(provider)
        && !config.active_provider_preserves_custom_base_url_model()
        && let Some(normalized) = normalize_model_for_provider(provider, model)
    {
        config.default_text_model = Some(normalized);
    }

    if let Some(providers) = config.providers.as_mut() {
        if let Some(model) = providers.deepseek.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Deepseek, &providers.deepseek)
            && !is_declared(ProviderKind::Deepseek.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Deepseek, model)
        {
            providers.deepseek.model = Some(normalized);
        }
        if let Some(model) = providers.deepseek_cn.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Deepseek, &providers.deepseek_cn)
            && !is_declared(codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id, model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Deepseek, model)
        {
            providers.deepseek_cn.model = Some(normalized);
        }
        if let Some(model) = providers.deepseek_anthropic.model.as_deref()
            && !provider_entry_uses_custom_base_url(
                ProviderKind::DeepseekAnthropic,
                &providers.deepseek_anthropic,
            )
            && !is_declared(ProviderKind::DeepseekAnthropic.as_str(), model)
            && let Some(normalized) =
                normalize_model_for_provider(ProviderKind::DeepseekAnthropic, model)
        {
            providers.deepseek_anthropic.model = Some(normalized);
        }
        if let Some(model) = providers.nvidia_nim.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::NvidiaNim, &providers.nvidia_nim)
            && !is_declared(ProviderKind::NvidiaNim.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::NvidiaNim, model)
        {
            providers.nvidia_nim.model = Some(normalized);
        }
        if let Some(model) = providers.openrouter.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Openrouter, &providers.openrouter)
            && !is_declared(ProviderKind::Openrouter.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Openrouter, model)
        {
            providers.openrouter.model = Some(normalized);
        }
        if let Some(model) = providers.novita.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Novita, &providers.novita)
            && !is_declared(ProviderKind::Novita.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Novita, model)
        {
            providers.novita.model = Some(normalized);
        }
        if let Some(model) = providers.fireworks.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Fireworks, &providers.fireworks)
            && !is_declared(ProviderKind::Fireworks.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Fireworks, model)
        {
            providers.fireworks.model = Some(normalized);
        }
        if let Some(model) = providers.siliconflow.model.as_deref()
            && !provider_entry_uses_custom_base_url(
                ProviderKind::Siliconflow,
                &providers.siliconflow,
            )
            && !is_declared(ProviderKind::Siliconflow.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Siliconflow, model)
        {
            providers.siliconflow.model = Some(normalized);
        }
        if let Some(model) = providers.siliconflow_cn.model.as_deref()
            && !provider_entry_uses_custom_base_url(
                ProviderKind::SiliconflowCN,
                &providers.siliconflow_cn,
            )
            && !is_declared(ProviderKind::SiliconflowCN.as_str(), model)
            && let Some(normalized) =
                normalize_model_for_provider(ProviderKind::SiliconflowCN, model)
        {
            providers.siliconflow_cn.model = Some(normalized);
        }
        if let Some(model) = providers.moonshot.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Moonshot, &providers.moonshot)
            && !is_declared(ProviderKind::Moonshot.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Moonshot, model)
        {
            providers.moonshot.model = Some(normalized);
        }
        if let Some(model) = providers.sglang.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Sglang, &providers.sglang)
            && !is_declared(ProviderKind::Sglang.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Sglang, model)
        {
            providers.sglang.model = Some(normalized);
        }
        if let Some(model) = providers.vllm.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Vllm, &providers.vllm)
            && !is_declared(ProviderKind::Vllm.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Vllm, model)
        {
            providers.vllm.model = Some(normalized);
        }
        if let Some(model) = providers.deepinfra.model.as_deref()
            && !provider_entry_uses_custom_base_url(ProviderKind::Deepinfra, &providers.deepinfra)
            && !is_declared(ProviderKind::Deepinfra.as_str(), model)
            && let Some(normalized) = normalize_model_for_provider(ProviderKind::Deepinfra, model)
        {
            providers.deepinfra.model = Some(normalized);
        }
    }
}

#[cfg(test)]
pub(crate) fn normalize_model_config_for_test(config: &mut Config) {
    normalize_model_config(config);
}

fn normalize_model_for_provider(provider: ProviderKind, model: &str) -> Option<String> {
    if matches!(provider, ProviderKind::XiaomiMimo)
        && let Some(canonical) = canonical_xiaomi_mimo_model_id(model)
    {
        return Some(canonical.to_string());
    }
    if provider_passes_model_through(provider) {
        return None;
    }
    normalize_model_name_for_provider(provider, model)
}

pub(crate) fn provider_passes_model_through(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::Openai
            | ProviderKind::Atlascloud
            | ProviderKind::WanjieArk
            | ProviderKind::Volcengine
            | ProviderKind::XiaomiMimo
            | ProviderKind::Moonshot
            | ProviderKind::Qianfan
            | ProviderKind::Openmodel
            | ProviderKind::Ollama
            | ProviderKind::OllamaCloud
            | ProviderKind::Huggingface
            | ProviderKind::Modelscope
            | ProviderKind::Meta
            | ProviderKind::Xai
            | ProviderKind::Telecomjs
            | ProviderKind::Edenai
            | ProviderKind::Zenmux
            | ProviderKind::Csdn
            // Concentrate ids are gateway-owned (plain, `provider/model`, or the
            // gateway's own `auto`); the resolver strips only `concentrate/`.
            | ProviderKind::Concentrate
            // Codewhale API ids are `provider/model` exactly as the account
            // catalog returns them; never normalize or rewrite them.
            | ProviderKind::Codewhale
            | ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic
            // Custom OpenAI-compatible endpoints preserve user-supplied model
            // ids verbatim (#1519); never normalize/rewrite them.
            | ProviderKind::Custom
    )
}

fn provider_entry_uses_custom_base_url(provider: ProviderKind, entry: &ProviderConfig) -> bool {
    entry
        .base_url
        .as_deref()
        .is_some_and(|base_url| provider_preserves_custom_base_url_model(provider, base_url))
}

fn xiaomi_mimo_base_url_for_mode(mode: &str) -> Option<&'static str> {
    let normalized = mode.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    if normalized.is_empty() || xiaomi_mimo_mode_uses_standard_endpoint(&normalized) {
        return None;
    }
    Some(match normalized.as_str() {
        "token-plan" | "tokenplan" | "subscription" | "subscribed" | "plan" => {
            DEFAULT_XIAOMI_MIMO_BASE_URL
        }
        "token-plan-cn"
        | "token-plan-china"
        | "token-plan-mainland"
        | "token-plan-mainland-china"
        | "cn"
        | "china" => XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL,
        "token-plan-sgp"
        | "token-plan-sg"
        | "token-plan-singapore"
        | "sgp"
        | "sg"
        | "singapore" => XIAOMI_MIMO_TOKEN_PLAN_SGP_BASE_URL,
        "token-plan-ams"
        | "token-plan-eu"
        | "token-plan-europe"
        | "token-plan-amsterdam"
        | "ams"
        | "eu"
        | "europe"
        | "amsterdam" => XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL,
        _ => DEFAULT_XIAOMI_MIMO_BASE_URL,
    })
}

fn xiaomi_mimo_mode_uses_standard_endpoint(normalized_mode: &str) -> bool {
    matches!(
        normalized_mode,
        "standard" | "default" | "payg" | "paygo" | "pay-as-you-go" | "pay-as-go"
    )
}

fn xiaomi_mimo_base_url_uses_token_plan(base_url: &str) -> bool {
    let normalized = normalize_base_url(base_url).to_ascii_lowercase();
    normalized == XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL
        || normalized == XIAOMI_MIMO_TOKEN_PLAN_SGP_BASE_URL
        || normalized == XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL
}

fn xiaomi_mimo_env_var(candidates: &[&str]) -> Option<String> {
    candidates.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

fn xiaomi_mimo_env_api_key_for_runtime(
    mode: Option<&str>,
    base_url: Option<&str>,
) -> Option<String> {
    const TOKEN_PLAN_ENV_VARS: &[&str] =
        &["XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "MIMO_TOKEN_PLAN_API_KEY"];
    const STANDARD_ENV_VARS: &[&str] = &["XIAOMI_MIMO_API_KEY", "XIAOMI_API_KEY", "MIMO_API_KEY"];

    let normalized_mode =
        mode.map(|value| value.trim().to_ascii_lowercase().replace(['_', ' '], "-"));
    let standard_selected = normalized_mode
        .as_deref()
        .is_some_and(xiaomi_mimo_mode_uses_standard_endpoint)
        || base_url.is_some_and(xiaomi_mimo_base_url_is_pay_as_you_go);
    if standard_selected {
        return xiaomi_mimo_env_var(STANDARD_ENV_VARS);
    }

    let token_plan_selected = normalized_mode
        .as_deref()
        .and_then(xiaomi_mimo_base_url_for_mode)
        .is_some()
        || base_url.is_some_and(xiaomi_mimo_base_url_uses_token_plan);
    if token_plan_selected {
        return xiaomi_mimo_env_var(TOKEN_PLAN_ENV_VARS);
    }

    xiaomi_mimo_env_var(TOKEN_PLAN_ENV_VARS).or_else(|| xiaomi_mimo_env_var(STANDARD_ENV_VARS))
}

fn wire_config_prefers_anthropic(wire: Option<&str>) -> bool {
    let Some(raw) = wire.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
    matches!(
        normalized.as_str(),
        "anthropic"
            | "anthropic-messages"
            | "messages"
            | "claude"
            | "anthropic-compatible"
            | "anthropic-compat"
    )
}

fn wire_config_prefers_responses(wire: Option<&str>) -> bool {
    let Some(raw) = wire.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
    matches!(
        normalized.as_str(),
        "responses"
            | "responses-api"
            | "openai-responses"
            | "openai-responses-api"
            | "response"
            | "response-api"
            | "openai-responses-compat"
            | "responses-compat"
    )
}

fn modelstudio_mode_is_coding_plan(provider: ProviderKind, mode: Option<&str>) -> bool {
    if matches!(
        provider,
        ProviderKind::ModelstudioCodingPlan | ProviderKind::ModelstudioCodingPlanAnthropic
    ) {
        return true;
    }
    let Some(raw) = mode.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
    matches!(
        normalized.as_str(),
        "coding-plan" | "coding" | "codingplan" | "dashscope-coding" | "code"
    )
}

fn resolve_modelstudio_base_url_for_tui(
    configured: Option<String>,
    provider: ProviderKind,
    mode: Option<&str>,
    wire: Option<&str>,
) -> String {
    if let Some(url) = configured.filter(|value| !value.trim().is_empty()) {
        return url;
    }
    let coding = modelstudio_mode_is_coding_plan(provider, mode);
    let anthropic = matches!(
        provider,
        ProviderKind::ModelstudioTokenPlanAnthropic | ProviderKind::ModelstudioCodingPlanAnthropic
    ) || wire_config_prefers_anthropic(wire);
    match (coding, anthropic) {
        (true, true) => MODELSTUDIO_CODING_PLAN_ANTHROPIC_BASE_URL.to_string(),
        (true, false) => DEFAULT_MODELSTUDIO_CODING_PLAN_BASE_URL.to_string(),
        (false, true) => MODELSTUDIO_TOKEN_PLAN_ANTHROPIC_BASE_URL.to_string(),
        (false, false) => DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL.to_string(),
    }
}

fn resolve_minimax_base_url_for_tui(
    configured: Option<String>,
    provider: ProviderKind,
    wire: Option<&str>,
) -> String {
    if let Some(url) = configured.filter(|value| !value.trim().is_empty()) {
        return url;
    }
    if matches!(provider, ProviderKind::MinimaxAnthropic) || wire_config_prefers_anthropic(wire) {
        DEFAULT_MINIMAX_ANTHROPIC_BASE_URL.to_string()
    } else {
        DEFAULT_MINIMAX_BASE_URL.to_string()
    }
}

fn resolve_deepseek_base_url_for_tui(
    configured: Option<String>,
    provider: ProviderKind,
    wire: Option<&str>,
) -> String {
    if let Some(url) = configured.filter(|value| !value.trim().is_empty()) {
        return url;
    }
    if matches!(provider, ProviderKind::DeepseekAnthropic) || wire_config_prefers_anthropic(wire) {
        DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL.to_string()
    } else {
        DEFAULT_DEEPSEEK_BASE_URL.to_string()
    }
}

fn resolve_xiaomi_mimo_base_url(
    configured: Option<String>,
    api_key: Option<&str>,
    mode: Option<&str>,
) -> String {
    let normalized_mode =
        mode.map(|value| value.trim().to_ascii_lowercase().replace(['_', ' '], "-"));
    let uses_standard_mode = normalized_mode
        .as_deref()
        .is_some_and(xiaomi_mimo_mode_uses_standard_endpoint);
    let mode_base_url = normalized_mode
        .as_deref()
        .and_then(xiaomi_mimo_base_url_for_mode);
    let uses_token_plan = xiaomi_mimo_api_key_uses_token_plan(api_key);
    match configured {
        Some(base_url) if uses_standard_mode => base_url,
        Some(base_url) if uses_token_plan && xiaomi_mimo_base_url_is_pay_as_you_go(&base_url) => {
            mode_base_url
                .unwrap_or(DEFAULT_XIAOMI_MIMO_BASE_URL)
                .to_string()
        }
        Some(base_url) => base_url,
        None => {
            if let Some(base_url) = mode_base_url {
                base_url.to_string()
            } else if uses_standard_mode {
                XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL.to_string()
            } else if uses_token_plan || api_key.is_none() {
                DEFAULT_XIAOMI_MIMO_BASE_URL.to_string()
            } else {
                XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL.to_string()
            }
        }
    }
}

fn xiaomi_mimo_api_key_uses_token_plan(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| key.trim_start().starts_with("tp-"))
}

fn xiaomi_mimo_base_url_is_pay_as_you_go(base_url: &str) -> bool {
    matches!(
        normalize_base_url(base_url).to_ascii_lowercase().as_str(),
        "https://api.xiaomimimo.com" | "https://api.xiaomimimo.com/v1"
    )
}

fn base_url_is_custom_for_provider(provider: ProviderKind, base_url: &str) -> bool {
    codewhale_config::provider_preserves_custom_base_url_model(provider, base_url)
}

/// Whether this concrete route is a self-hosted endpoint whose credentials
/// are optional by default.
///
/// Ollama is local; the released exact `ollama` + `https://ollama.com/v1`
/// tuple is upgraded to `OllamaCloud` before this helper runs. Cloud is never
/// self-hosted, while neighboring remote Ollama URLs remain custom and are
/// rejected before they can inherit ambient or saved credentials.
pub(crate) fn provider_route_is_keyless_self_hosted(
    provider: ProviderKind,
    base_url: &str,
) -> bool {
    if provider == ProviderKind::Ollama {
        return base_url_uses_local_host(base_url);
    }
    matches!(
        provider,
        ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm
    )
}

fn provider_preserves_custom_base_url_model(provider: ProviderKind, base_url: &str) -> bool {
    base_url_is_custom_for_provider(provider, base_url)
}

fn moonshot_base_url_uses_kimi_code(base_url: &str) -> bool {
    let normalized = normalize_base_url(base_url).to_ascii_lowercase();
    normalized == DEFAULT_KIMI_CODE_BASE_URL
        || normalized == "https://api.kimi.com/coding"
        || normalized.starts_with("https://api.kimi.com/coding/")
}

/// The Kimi Code API endpoint, normalized only for insignificant trailing
/// slashes. This must stay stricter than `moonshot_base_url_uses_kimi_code`:
/// route-specific K3 capability and request shaping are not safe for arbitrary
/// Kimi-hosted paths.
pub(crate) fn moonshot_base_url_is_exact_kimi_code(base_url: &str) -> bool {
    codewhale_config::provider::is_exact_kimi_code_route(
        codewhale_config::ProviderKind::Moonshot,
        base_url,
    )
}

/// The exact Moonshot direct-API endpoint, normalized only for an
/// insignificant trailing slash. Custom gateways must retain their own wire
/// contract even when they expose a `kimi-k3` model id.
pub(crate) fn moonshot_base_url_is_exact_direct_platform(base_url: &str) -> bool {
    codewhale_config::provider::is_exact_moonshot_platform_route(
        codewhale_config::ProviderKind::Moonshot,
        base_url,
    )
}

/// Whether a route is exactly Moonshot's direct pay-as-you-go K3 route.
pub(crate) fn is_exact_direct_moonshot_k3_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    provider == ProviderKind::Moonshot
        && moonshot_base_url_is_exact_direct_platform(base_url)
        && model.trim().eq_ignore_ascii_case(MOONSHOT_KIMI_K3_MODEL)
}

/// Whether a route uses either official Kimi Code K3 membership model.
pub(crate) fn is_exact_kimi_code_k3_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    provider == ProviderKind::Moonshot
        && moonshot_base_url_is_exact_kimi_code(base_url)
        && [KIMI_CODE_K3_MODEL, KIMI_CODE_K3_256K_MODEL]
            .iter()
            .any(|id| model.trim().eq_ignore_ascii_case(id))
}

/// Whether a route uses plan-tier-dependent bare `k3`.
///
/// Keep entitlement handling separate from `k3-256k`, whose window is fixed.
#[must_use]
pub(crate) fn is_exact_kimi_code_bare_k3_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    provider == ProviderKind::Moonshot
        && moonshot_base_url_is_exact_kimi_code(base_url)
        && model.trim().eq_ignore_ascii_case(KIMI_CODE_K3_MODEL)
}

/// Whether a route is one of Z.ai's exact first-party Chat endpoints.
#[must_use]
pub(crate) fn is_exact_zai_chat_route(provider: ProviderKind, base_url: &str) -> bool {
    provider == ProviderKind::Zai
        && codewhale_config::provider::is_exact_zai_chat_route(
            codewhale_config::ProviderKind::Zai,
            base_url,
        )
}

/// Whether a route is an exact first-party Z.ai model that exposes **tiered**
/// reasoning effort (`reasoning_effort: high | max`) rather than only the
/// generic thinking toggle.
///
/// GLM-5.2 is the verified member. GLM-5.3 and GLM-5.3-Flash inherit it
/// because their catalog rows inherit GLM-5.2's `reasoning_options`
/// wholesale. Where the 5.3 family does diverge — forced thinking — is
/// captured by [`is_exact_zai_forced_thinking_route`].
#[must_use]
pub(crate) fn is_exact_zai_tiered_effort_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    is_exact_zai_chat_route(provider, base_url)
        && (model.trim().eq_ignore_ascii_case(ZAI_GLM_5_2_MODEL)
            || is_exact_zai_forced_thinking_route(provider, base_url, model))
}

/// Whether a route is exactly first-party Z.ai GLM-5.3 or GLM-5.3-Flash.
///
/// Both BigModel (`docs.bigmodel.cn/cn/guide/capabilities/thinking`) and
/// Z.ai (`docs.z.ai/guides/capabilities/thinking`) document the 5.3 family
/// as forced-thinking: `thinking.type: "disabled"` is rejected with an error
/// and `reasoning_effort` accepts only `low` / `high` / `max`. The migration
/// note for a former `disabled` payload is `enabled` + `reasoning_effort:
/// "low"`. GLM-5.2 stays outside this predicate because it still honours
/// the generic disabled toggle.
#[must_use]
pub(crate) fn is_exact_zai_forced_thinking_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    is_exact_zai_chat_route(provider, base_url)
        && (model.trim().eq_ignore_ascii_case(ZAI_GLM_5_3_MODEL)
            || model.trim().eq_ignore_ascii_case(ZAI_GLM_5_3_FLASH_MODEL))
}

/// Whether a route is exactly first-party Z.ai GLM-5-Turbo.
#[must_use]
pub(crate) fn is_exact_zai_glm_5_turbo_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    is_exact_zai_chat_route(provider, base_url)
        && model.trim().eq_ignore_ascii_case(ZAI_GLM_5_TURBO_MODEL)
}

/// Whether a route is an exact first-party Z.ai model with a verified
/// reasoning control. GLM-5.2, GLM-5.3, and GLM-5.3-Flash have tiered
/// effort; GLM-5.1 and GLM-5-Turbo only expose the generic thinking toggle.
#[must_use]
pub(crate) fn is_exact_known_zai_reasoning_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    is_exact_zai_tiered_effort_route(provider, base_url, model)
        || is_exact_zai_glm_5_turbo_route(provider, base_url, model)
        || (is_exact_zai_chat_route(provider, base_url)
            && model.trim().eq_ignore_ascii_case(ZAI_GLM_5_1_MODEL))
}

/// MiniMax's own hosted routes, for both wire dialects.
///
/// Kept as a pure string predicate so a dispatch receipt can be judged without
/// a `Config`, and shared with billing classification so a MiniMax-compatible
/// gateway cannot inherit the first-party PAYG/Token Plan duality. Both the
/// `.io` and `.com` hosts are first-party; anything else is a gateway.
#[must_use]
pub(crate) fn minimax_base_url_is_supported_direct(base_url: &str) -> bool {
    codewhale_config::provider::is_exact_minimax_chat_route(
        codewhale_config::ProviderKind::Minimax,
        base_url,
    ) || codewhale_config::provider::is_exact_minimax_anthropic_route(
        codewhale_config::ProviderKind::MinimaxAnthropic,
        base_url,
    )
}

/// Whether a route is exactly MiniMax-M3 on the first-party OpenAI-compatible
/// Chat API. Compatible gateways and the Anthropic Messages route retain
/// their own token-limit dialects.
#[must_use]
pub(crate) fn is_exact_minimax_m3_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    provider == ProviderKind::Minimax
        && codewhale_config::provider::is_exact_minimax_chat_route(
            codewhale_config::ProviderKind::Minimax,
            base_url,
        )
        && model.trim().eq_ignore_ascii_case(DEFAULT_MINIMAX_MODEL)
}

/// Whether a route is exactly MiniMax-M3 on a first-party Anthropic-compatible
/// Messages endpoint. The wire supports adaptive/disabled thinking, but no
/// distinct effort tier.
#[must_use]
pub(crate) fn is_exact_minimax_anthropic_m3_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    provider == ProviderKind::MinimaxAnthropic
        && codewhale_config::provider::is_exact_minimax_anthropic_route(
            codewhale_config::ProviderKind::MinimaxAnthropic,
            base_url,
        )
        && model.trim().eq_ignore_ascii_case(DEFAULT_MINIMAX_MODEL)
}

#[must_use]
pub(crate) fn minimax_m3_route_uses_max_completion_tokens(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    is_exact_minimax_m3_route(provider, base_url, model)
}

/// The Kimi Code membership roster, as one fact.
///
/// The picker offers these ids, `validate_kimi_code_api_model_id` accepts them
/// on the membership endpoint and rejects them on the direct platform, and the
/// model picker labels them as plan routes. Those sites previously kept
/// independent literal lists and had already drifted (`kimi-for-coding` was
/// missing from the picker label), so the roster lives here and nowhere else.
pub(crate) const KIMI_CODE_MEMBERSHIP_MODELS: [&str; 4] = [
    KIMI_CODE_K3_MODEL,
    KIMI_CODE_K3_256K_MODEL,
    DEFAULT_KIMI_CODE_MODEL,
    KIMI_CODE_HIGHSPEED_MODEL,
];

/// Whether `model` is a Kimi Code membership model id.
///
/// The single membership-roster predicate. Callers that need to name the
/// product — output-ceiling provenance, picker rosters, setup validation, and
/// the model picker's route label — must use this rather than re-listing ids.
#[must_use]
pub(crate) fn is_kimi_code_membership_model(model: &str) -> bool {
    let model = model.trim();
    KIMI_CODE_MEMBERSHIP_MODELS
        .iter()
        .any(|id| model.eq_ignore_ascii_case(id))
}

/// Keep the recovery command visible in small terminals. The optional DSH
/// advice is conditional prose: producing an error must not inspect PATH or
/// another application's credential file on the runtime thread.
fn deepseek_missing_key_message() -> &'static str {
    concat!(
        "DeepSeek API key not found.\n",
        "Save a key for every folder:\n",
        "  codewhale auth set --provider deepseek\n",
        "Get a key: https://platform.deepseek.com/api_keys\n",
        "Or export DEEPSEEK_API_KEY=<your-key> (this shell only).\n",
        "zsh: ~/.zshrc is interactive only; use ~/.zshenv.\n",
        "Or set api_key in ~/.codewhale/config.toml.\n",
        "If you already use DeepSeek Harness, grant read-only access:\n",
        "  codewhale auth external-consent --provider deepseek --mode read-only"
    )
}

/// The Moonshot direct-platform roster, as one fact. Mirror of
/// [`KIMI_CODE_MEMBERSHIP_MODELS`] for the pay-as-you-go product.
pub(crate) const MOONSHOT_DIRECT_PLATFORM_MODELS: [&str; 3] = [
    MOONSHOT_KIMI_K3_MODEL,
    DEFAULT_MOONSHOT_MODEL,
    MOONSHOT_KIMI_K2_6_MODEL,
];

pub(crate) const KIMI_CODE_CLAUDE_ALIAS_GUIDANCE: &str = "Kimi Code model `k3[1m]` is a Claude Code environment convention, not an API model id. Use model = \"k3\". If your Kimi Code plan includes 1M context, also set context_window = 1048576; otherwise keep the 262144 safe default.";

/// Configuration errors whose text is safe to show in diagnostics such as
/// `codewhale doctor`. Everything else stays suppressed there because parse
/// errors and credential fields can echo secret material.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SafeConfigDiagnostic {
    #[error("{}", KIMI_CODE_CLAUDE_ALIAS_GUIDANCE)]
    KimiCodeClaudeAlias,
    /// A plain enum/value validation failure on a non-credential key.
    /// `message` quotes the rejected value for the local error; `shareable`
    /// omits it, because a mistyped value can still be a pasted secret.
    #[error("{message}")]
    InvalidValue {
        message: String,
        shareable: String,
        /// A `codewhale config set <key> <valid>` command, when one fixes it.
        fix: Option<String>,
    },
}

impl SafeConfigDiagnostic {
    fn invalid_value(key: &str, value: &str, expected: &str, fix: String) -> Self {
        Self::InvalidValue {
            message: format!("Invalid {key} '{value}': expected {expected}."),
            shareable: format!("Invalid {key} (value not shown): expected {expected}."),
            fix: Some(fix),
        }
    }

    /// The diagnostic text for reports such as `codewhale doctor`, without
    /// the rejected value and redacted defensively.
    pub(crate) fn display_message(&self) -> String {
        let text = match self {
            Self::KimiCodeClaudeAlias => self.to_string(),
            Self::InvalidValue { shareable, .. } => shareable.clone(),
        };
        codewhale_secrets::redact::redact_secrets(&text)
    }

    pub(crate) fn fix(&self) -> Option<&str> {
        match self {
            Self::KimiCodeClaudeAlias => None,
            Self::InvalidValue { fix, .. } => fix.as_deref(),
        }
    }

    /// Find a safe diagnostic anywhere in an error chain (loaders wrap
    /// validation errors in file-path context).
    pub(crate) fn find_in(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|cause| cause.downcast_ref::<Self>())
    }
}

/// How to correct a rejected value in the user config. `config set` does not
/// take dotted keys, so a table field names the table to edit. Validation runs
/// after the environment, profile and managed layers are applied, and any of
/// them outranks the user config, so the fix names those layers too.
fn user_config_fix(key: &str, valid: &str, env_var: Option<&str>) -> String {
    let edit = match key.split_once('.') {
        Some((table, field)) => {
            format!("set {field} = \"{valid}\" in the [{table}] table of config.toml")
        }
        None => format!("codewhale config set {key} {valid}"),
    };
    let layers = match env_var {
        Some(env_var) => format!("{env_var}, a profile, or managed config"),
        None => "a profile or managed config".to_string(),
    };
    format!("{edit} (if {layers} sets it, correct it there)")
}

fn invalid_provider_diagnostic(provider: &str) -> SafeConfigDiagnostic {
    SafeConfigDiagnostic::invalid_value(
        "provider",
        provider,
        &ProviderKind::names_hint(),
        user_config_fix(
            "provider",
            ProviderKind::Deepseek.as_str(),
            Some("CODEWHALE_PROVIDER"),
        ),
    )
}

/// The one wording for an unknown provider name, shared by config validation
/// and `config set provider`.
pub(crate) fn invalid_provider_message(provider: &str) -> String {
    invalid_provider_diagnostic(provider).to_string()
}

/// Fail closed on known-bad model/endpoint pairings (#4687).
///
/// Canonical endpoints reject `k3[1m]` and known membership/direct cross-pairings.
/// Unknown IDs and custom Moonshot-compatible gateways remain pass-through.
pub(crate) fn validate_kimi_code_api_model_id(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> std::result::Result<(), String> {
    if provider != ProviderKind::Moonshot {
        return Ok(());
    }
    let model = model.trim();
    if model.is_empty() {
        return Ok(());
    }

    if moonshot_base_url_is_exact_kimi_code(base_url) {
        if model.eq_ignore_ascii_case("k3[1m]") {
            return Err(KIMI_CODE_CLAUDE_ALIAS_GUIDANCE.to_string());
        }
        for direct_id in MOONSHOT_DIRECT_PLATFORM_MODELS {
            if model.eq_ignore_ascii_case(direct_id) {
                return Err(format!(
                    "Kimi Code membership route (api.kimi.com/coding/v1) does not accept model = \"{model}\": it is a direct Moonshot platform id. Use a Kimi Code membership model (\"k3\", \"k3-256k\", \"kimi-for-coding\", or \"kimi-for-coding-highspeed\") for this base_url. Direct Moonshot pay-as-you-go uses base_url = \"https://api.moonshot.ai/v1\" with model = \"{direct_id}\"."
                ));
            }
        }
        return Ok(());
    }

    if moonshot_base_url_is_exact_direct_platform(base_url) {
        for membership_id in KIMI_CODE_MEMBERSHIP_MODELS {
            if model.eq_ignore_ascii_case(membership_id) {
                return Err(format!(
                    "Moonshot direct route (api.moonshot.ai/v1) does not accept model = \"{model}\": it is a Kimi Code membership model id, not a direct-platform catalog model. Kimi Code membership uses base_url = \"https://api.kimi.com/coding/v1\" with model = \"{membership_id}\"; direct Moonshot pay-as-you-go K3 uses model = \"kimi-k3\"."
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod kimi_code_pairing_tests {
    use super::*;

    #[test]
    fn membership_roster_passes_on_kimi_code_endpoint() {
        for model in [
            KIMI_CODE_K3_MODEL,
            KIMI_CODE_K3_256K_MODEL,
            DEFAULT_KIMI_CODE_MODEL,
            KIMI_CODE_HIGHSPEED_MODEL,
        ] {
            assert!(
                validate_kimi_code_api_model_id(
                    ProviderKind::Moonshot,
                    DEFAULT_KIMI_CODE_BASE_URL,
                    model,
                )
                .is_ok(),
                "{model} must be accepted on the exact Kimi Code membership endpoint"
            );
        }
    }

    #[test]
    fn direct_platform_ids_fail_on_kimi_code_endpoint() {
        for model in [
            MOONSHOT_KIMI_K3_MODEL,
            DEFAULT_MOONSHOT_MODEL,
            MOONSHOT_KIMI_K2_6_MODEL,
        ] {
            let err = validate_kimi_code_api_model_id(
                ProviderKind::Moonshot,
                DEFAULT_KIMI_CODE_BASE_URL,
                model,
            )
            .expect_err("direct-platform ids are not Kimi Code membership roster models");
            assert!(err.contains(model), "{err}");
            assert!(err.contains("api.moonshot.ai/v1"), "{err}");
        }
    }

    #[test]
    fn membership_ids_fail_on_direct_moonshot_endpoint() {
        for model in [
            KIMI_CODE_K3_MODEL,
            KIMI_CODE_K3_256K_MODEL,
            DEFAULT_KIMI_CODE_MODEL,
            KIMI_CODE_HIGHSPEED_MODEL,
        ] {
            let err = validate_kimi_code_api_model_id(
                ProviderKind::Moonshot,
                DEFAULT_MOONSHOT_BASE_URL,
                model,
            )
            .expect_err("membership ids are not direct-platform catalog models");
            assert!(err.contains(model), "{err}");
            assert!(err.contains("api.kimi.com/coding/v1"), "{err}");
        }
    }

    #[test]
    fn canonical_pairs_pass_and_custom_gateways_are_untouched() {
        // Canonical pairs pass on both endpoints.
        for (base_url, model) in [
            (DEFAULT_KIMI_CODE_BASE_URL, KIMI_CODE_K3_MODEL),
            (DEFAULT_KIMI_CODE_BASE_URL, KIMI_CODE_K3_256K_MODEL),
            (DEFAULT_KIMI_CODE_BASE_URL, DEFAULT_KIMI_CODE_MODEL),
            (DEFAULT_KIMI_CODE_BASE_URL, KIMI_CODE_HIGHSPEED_MODEL),
            (DEFAULT_MOONSHOT_BASE_URL, MOONSHOT_KIMI_K3_MODEL),
            (DEFAULT_MOONSHOT_BASE_URL, DEFAULT_MOONSHOT_MODEL),
            (DEFAULT_MOONSHOT_BASE_URL, MOONSHOT_KIMI_K2_6_MODEL),
        ] {
            assert!(
                validate_kimi_code_api_model_id(ProviderKind::Moonshot, base_url, model).is_ok(),
                "{base_url} / {model}"
            );
        }
        // The pre-existing cross-pairings still fail closed.
        assert!(
            validate_kimi_code_api_model_id(
                ProviderKind::Moonshot,
                DEFAULT_KIMI_CODE_BASE_URL,
                MOONSHOT_KIMI_K3_MODEL,
            )
            .is_err()
        );
        assert!(
            validate_kimi_code_api_model_id(
                ProviderKind::Moonshot,
                DEFAULT_MOONSHOT_BASE_URL,
                KIMI_CODE_K3_MODEL,
            )
            .is_err()
        );
        // Custom gateways keep their own wire contract, membership ids
        // included: only the two canonical endpoints enforce pairings.
        for model in [
            KIMI_CODE_K3_MODEL,
            KIMI_CODE_K3_256K_MODEL,
            DEFAULT_KIMI_CODE_MODEL,
            KIMI_CODE_HIGHSPEED_MODEL,
            MOONSHOT_KIMI_K3_MODEL,
        ] {
            assert!(
                validate_kimi_code_api_model_id(
                    ProviderKind::Moonshot,
                    "https://proxy.example/v1",
                    model,
                )
                .is_ok(),
                "{model} on a custom gateway"
            );
        }
    }
}

/// Short route label for header/diagnostics without credentials (#4687).
pub(crate) fn moonshot_k3_route_display_name(base_url: &str, model: &str) -> Option<&'static str> {
    if is_exact_kimi_code_bare_k3_route(ProviderKind::Moonshot, base_url, model) {
        return Some("Kimi Code membership / k3");
    }
    if is_exact_kimi_code_k3_route(ProviderKind::Moonshot, base_url, model) {
        return Some("Kimi Code membership / k3-256k");
    }
    if is_exact_direct_moonshot_k3_route(ProviderKind::Moonshot, base_url, model) {
        return Some("Moonshot direct / kimi-k3");
    }
    None
}

/// Credential help for a concrete provider route.
///
/// `ProviderKind::Moonshot` intentionally retains its generic direct-API
/// metadata in `codewhale-config`: that remains correct for Moonshot's own
/// platform route. The Kimi Code membership endpoint is a distinct route and
/// must not send its users to the generic API console or imply CLI credential
/// import support.
pub(crate) fn credential_help_for_provider_route(
    provider: ProviderKind,
    base_url: &str,
) -> codewhale_config::provider::CredentialHelp {
    codewhale_config::provider::credential_help_for_route(provider, base_url)
}

pub(crate) fn provider_config_uses_kimi_imported_token(config: &ProviderConfig) -> bool {
    config
        .auth_mode
        .as_deref()
        .is_some_and(auth_mode_uses_kimi_imported_token)
}

pub(crate) use codewhale_config::{
    auth_mode_disables_api_key, auth_mode_requires_api_key, auth_mode_uses_kimi_imported_token,
};

fn provider_config_uses_xai_oauth(config: &ProviderConfig) -> bool {
    config
        .auth_mode
        .as_deref()
        .is_some_and(crate::oauth::auth_mode_uses_xai_oauth)
}

/// Whether a base URL points at a loopback/unspecified host, i.e. a local
/// runtime rather than a hosted endpoint. Shared by the active-provider
/// local-base-url check above and the `/provider` picker's custom-provider
/// auth-optionality heuristic (#3830).
pub(crate) fn base_url_uses_local_host(base_url: &str) -> bool {
    let Some(host) = base_url_host(base_url) else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if matches!(host.as_str(), "localhost" | "0.0.0.0") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|addr| addr.is_loopback() || addr.is_unspecified())
}

fn base_url_host(base_url: &str) -> Option<&str> {
    let without_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = without_scheme.split('/').next()?.rsplit('@').next()?;
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host);
    }
    authority.split(':').next().filter(|host| !host.is_empty())
}

fn model_for_provider(provider: ProviderKind, normalized: String) -> String {
    let lowered = normalized.to_ascii_lowercase();
    match (provider, lowered.as_str()) {
        (ProviderKind::NvidiaNim, "deepseek-v4-pro") => DEFAULT_NVIDIA_NIM_MODEL.to_string(),
        (ProviderKind::NvidiaNim, "deepseek-v4-flash") => {
            DEFAULT_NVIDIA_NIM_FLASH_MODEL.to_string()
        }
        (ProviderKind::Openrouter, "deepseek-v4-pro") => DEFAULT_OPENROUTER_MODEL.to_string(),
        (ProviderKind::Openrouter, "deepseek-v4-flash") => {
            DEFAULT_OPENROUTER_FLASH_MODEL.to_string()
        }
        (ProviderKind::Novita, "deepseek-v4-pro") => DEFAULT_NOVITA_MODEL.to_string(),
        (ProviderKind::Novita, "deepseek-v4-flash") => DEFAULT_NOVITA_FLASH_MODEL.to_string(),
        (ProviderKind::Fireworks, "deepseek-v4-pro") => DEFAULT_FIREWORKS_MODEL.to_string(),
        (
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN,
            "deepseek-v4-pro" | "deepseek-reasoner" | "deepseek-r1",
        ) => DEFAULT_SILICONFLOW_MODEL.to_string(),
        (
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN,
            "deepseek-v4-flash" | "deepseek-chat" | "deepseek-v3",
        ) => DEFAULT_SILICONFLOW_FLASH_MODEL.to_string(),
        (ProviderKind::Sglang, "deepseek-v4-pro") => DEFAULT_SGLANG_MODEL.to_string(),
        (ProviderKind::Sglang, "deepseek-v4-flash") => DEFAULT_SGLANG_FLASH_MODEL.to_string(),
        (ProviderKind::Vllm, "deepseek-v4-pro") => DEFAULT_VLLM_MODEL.to_string(),
        (ProviderKind::Vllm, "deepseek-v4-flash") => DEFAULT_VLLM_FLASH_MODEL.to_string(),
        (ProviderKind::Deepinfra, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_DEEPINFRA_MODEL.to_string()
        }
        (ProviderKind::Deepinfra, "deepseek-v4-flash" | "deepseek-chat" | "deepseek-reasoner") => {
            DEFAULT_DEEPINFRA_FLASH_MODEL.to_string()
        }
        (ProviderKind::Together, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_TOGETHER_MODEL.to_string()
        }
        (
            ProviderKind::Together,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner",
        ) => DEFAULT_TOGETHER_FLASH_MODEL.to_string(),
        (ProviderKind::Together, "inkling" | "together-inkling" | "thinkingmachines/inkling") => {
            TOGETHER_INKLING_MODEL.to_string()
        }
        (
            ProviderKind::Moonshot,
            "kimi"
            | "kimi-k2"
            | "kimi-k2.7"
            | "kimi-k2-7"
            | "kimi-k2.7-code"
            | "kimi-k2-7-code"
            | "kimi-code"
            | "moonshot-kimi-k2.7-code",
        ) => DEFAULT_MOONSHOT_MODEL.to_string(),
        (ProviderKind::Moonshot, "kimi-k2.6" | "kimi-k2-6" | "moonshot-kimi-k2.6") => {
            MOONSHOT_KIMI_K2_6_MODEL.to_string()
        }
        _ => normalized,
    }
}

pub(crate) fn normalize_base_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    let deepseek_domains = ["api.deepseek.com", "api.deepseeki.com"];
    if deepseek_domains
        .iter()
        .any(|domain| trimmed.contains(domain))
    {
        return trimmed.trim_end_matches("/v1").to_string();
    }
    trimmed.to_string()
}

fn parse_http_headers(raw: &str) -> Result<HashMap<String, String>> {
    let mut headers = HashMap::new();
    for pair in raw.trim().split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            anyhow::bail!("invalid header pair '{pair}', expected name=value");
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            anyhow::bail!("header name cannot be empty");
        }
        if value.is_empty() {
            continue;
        }
        headers.insert(name.to_string(), value.to_string());
    }
    Ok(headers)
}

// A higher file layer's legacy root model must not be masked by an inherited
// provider slot. Clear that lower slot in memory so the existing root-model
// validation and foreign-model guards still decide what this layer means.
fn apply_layer_root_model(config: &mut Config, layer: &Config) {
    if layer.default_text_model.is_none() && layer.legacy_model.is_none() {
        return;
    }
    let Ok(identity) = config.active_provider_identity() else {
        return;
    };
    let mut scoped = layer.clone();
    if scoped.scope_to_provider_identity(&identity).is_err() {
        return;
    }
    if scoped
        .provider_config_for(&identity)
        .and_then(|entry| entry.model.as_ref())
        .is_none()
    {
        config.set_provider_model_override(&identity, None).unwrap();
    }
}

fn apply_profile(config: ConfigFile, profile: Option<&str>) -> Result<Config> {
    if let Some(profile_name) = profile {
        let profiles = config.profiles.as_ref();
        match profiles.and_then(|profiles| profiles.get(profile_name)) {
            Some(override_cfg) => {
                let mut merged = merge_config(*config.base, override_cfg.clone());
                apply_layer_root_model(&mut merged, override_cfg);
                Ok(merged)
            }
            None => {
                let available = profiles
                    .map(|profiles| {
                        let mut keys = profiles.keys().cloned().collect::<Vec<_>>();
                        keys.sort();
                        if keys.is_empty() {
                            "none".to_string()
                        } else {
                            keys.join(", ")
                        }
                    })
                    .unwrap_or_else(|| "none".to_string());
                // Profile names are user-typed (`--profile`), so the shareable
                // text omits the requested one like every other InvalidValue;
                // the available names are config table keys, not values.
                Err(SafeConfigDiagnostic::InvalidValue {
                    message: format!(
                        "Profile '{profile_name}' not found. Available profiles: {available}"
                    ),
                    shareable: format!(
                        "Profile not found (name not shown). Available profiles: {available}"
                    ),
                    fix: None,
                }
                .into())
            }
        }
    } else {
        Ok(*config.base)
    }
}

fn merge_config(base: Config, override_cfg: Config) -> Config {
    Config {
        custom_models: override_cfg.custom_models.or(base.custom_models),
        provider: override_cfg.provider.or(base.provider),
        telemetry: override_cfg.telemetry.or(base.telemetry),
        http_headers: override_cfg.http_headers.or(base.http_headers),
        default_text_model: override_cfg
            .default_text_model
            .or_else(|| override_cfg.legacy_model.clone())
            .or(base.default_text_model)
            .or_else(|| base.legacy_model.clone()),
        legacy_model: override_cfg.legacy_model.or(base.legacy_model),
        redaction: override_cfg.redaction.or(base.redaction),
        loaded_config_path: override_cfg.loaded_config_path.or(base.loaded_config_path),
        remembered_selection_scope: override_cfg
            .remembered_selection_scope
            .or(base.remembered_selection_scope),
        route_preferences_version: override_cfg
            .route_preferences_version
            .or(base.route_preferences_version),
        environment_model_applied: override_cfg.environment_model_applied
            || base.environment_model_applied,
        auth_mode: override_cfg.auth_mode.or(base.auth_mode),
        reasoning_effort: override_cfg.reasoning_effort.or(base.reasoning_effort),
        reasoning_effort_inferred_from_legacy_alias: override_cfg
            .reasoning_effort_inferred_from_legacy_alias
            || base.reasoning_effort_inferred_from_legacy_alias,
        fleet_operator_route_applied: override_cfg.fleet_operator_route_applied
            || base.fleet_operator_route_applied,
        fleet_operator_reasoning_applied: override_cfg.fleet_operator_reasoning_applied
            || base.fleet_operator_reasoning_applied,
        migrated_legacy_ollama_cloud_route: override_cfg.migrated_legacy_ollama_cloud_route
            || base.migrated_legacy_ollama_cloud_route,
        migrated_deepseek_model_alias: override_cfg
            .migrated_deepseek_model_alias
            .or(base.migrated_deepseek_model_alias),
        tools: override_cfg.tools.or(base.tools),
        skills_dir: override_cfg.skills_dir.or(base.skills_dir),
        mcp_config_path: override_cfg.mcp_config_path.or(base.mcp_config_path),
        mcp_oauth_callback_port: override_cfg
            .mcp_oauth_callback_port
            .or(base.mcp_oauth_callback_port),
        mcp_oauth_callback_url: override_cfg
            .mcp_oauth_callback_url
            .or(base.mcp_oauth_callback_url),
        notes_path: override_cfg.notes_path.or(base.notes_path),
        memory_path: override_cfg.memory_path.or(base.memory_path),
        vision_model: override_cfg.vision_model.or(base.vision_model),
        // #454: user-owned overlays such as profiles and managed config may
        // replace the instruction array. Project-scope config is filtered in
        // main.rs and cannot set instruction paths.
        instructions: override_cfg.instructions.or(base.instructions),
        stop_words: override_cfg.stop_words.or(base.stop_words),
        allow_shell: override_cfg.allow_shell.or(base.allow_shell),
        prompt_suggestion: override_cfg.prompt_suggestion.or(base.prompt_suggestion),
        yolo: override_cfg.yolo.or(base.yolo),
        verbosity: override_cfg.verbosity.or(base.verbosity),
        approval_policy: override_cfg.approval_policy.or(base.approval_policy),
        sandbox_mode: override_cfg.sandbox_mode.or(base.sandbox_mode),
        sandbox_network_access: override_cfg
            .sandbox_network_access
            .or(base.sandbox_network_access),
        project_instruction_imports: if override_cfg.project_instruction_imports.is_empty() {
            base.project_instruction_imports
        } else {
            override_cfg.project_instruction_imports
        },
        fallback_providers: if override_cfg.fallback_providers.is_empty() {
            base.fallback_providers
        } else {
            override_cfg.fallback_providers
        },
        sandbox_backend: override_cfg.sandbox_backend.or(base.sandbox_backend),
        sandbox_url: override_cfg.sandbox_url.or(base.sandbox_url),
        sandbox_api_key: override_cfg.sandbox_api_key.or(base.sandbox_api_key),
        prefer_bwrap: override_cfg.prefer_bwrap.or(base.prefer_bwrap),
        bwrap_ro_roots: if override_cfg.bwrap_ro_roots.is_empty() {
            base.bwrap_ro_roots
        } else {
            override_cfg.bwrap_ro_roots
        },
        bwrap_dev_roots: if override_cfg.bwrap_dev_roots.is_empty() {
            base.bwrap_dev_roots
        } else {
            override_cfg.bwrap_dev_roots
        },
        sandbox_denied_read_paths: if override_cfg.sandbox_denied_read_paths.is_empty() {
            base.sandbox_denied_read_paths
        } else {
            override_cfg.sandbox_denied_read_paths
        },
        sandbox_read_denylist_defaults: override_cfg
            .sandbox_read_denylist_defaults
            .or(base.sandbox_read_denylist_defaults),
        sandbox_read_denylist_exempt: if override_cfg.sandbox_read_denylist_exempt.is_empty() {
            base.sandbox_read_denylist_exempt
        } else {
            override_cfg.sandbox_read_denylist_exempt
        },
        managed_config_path: override_cfg
            .managed_config_path
            .or(base.managed_config_path),
        requirements_path: override_cfg.requirements_path.or(base.requirements_path),
        max_subagents: override_cfg.max_subagents.or(base.max_subagents),
        retry: override_cfg.retry.or(base.retry),
        stream: match (base.stream, override_cfg.stream) {
            (Some(base), Some(over)) => Some(StreamConfig {
                open_timeout_secs: over.open_timeout_secs.or(base.open_timeout_secs),
                chunk_timeout_secs: over.chunk_timeout_secs.or(base.chunk_timeout_secs),
                force_http1: over.force_http1.or(base.force_http1),
                max_resumes: over.max_resumes.or(base.max_resumes),
                max_transparent_retries: over
                    .max_transparent_retries
                    .or(base.max_transparent_retries),
                max_stream_errors: over.max_stream_errors.or(base.max_stream_errors),
                max_duration_secs: over.max_duration_secs.or(base.max_duration_secs),
                max_content_mb: over.max_content_mb.or(base.max_content_mb),
                connect_timeout_secs: over.connect_timeout_secs.or(base.connect_timeout_secs),
                tcp_keepalive_secs: over.tcp_keepalive_secs.or(base.tcp_keepalive_secs),
                http2_keep_alive_interval_secs: over
                    .http2_keep_alive_interval_secs
                    .or(base.http2_keep_alive_interval_secs),
                http2_keep_alive_timeout_secs: over
                    .http2_keep_alive_timeout_secs
                    .or(base.http2_keep_alive_timeout_secs),
            }),
            (base, over) => over.or(base),
        },

        auto_review: override_cfg.auto_review.or(base.auto_review),
        tui: override_cfg.tui.or(base.tui),
        transcript: override_cfg.transcript.or(base.transcript),
        hooks: override_cfg.hooks.or(base.hooks),
        lifecycle_outbox: override_cfg.lifecycle_outbox.or(base.lifecycle_outbox),
        control_socket: override_cfg.control_socket.or(base.control_socket),
        providers: merge_providers(base.providers, override_cfg.providers),
        features: merge_features(base.features, override_cfg.features),
        extension_host: override_cfg.extension_host.or(base.extension_host),
        plugins: override_cfg.plugins.or(base.plugins),
        notifications: override_cfg.notifications.or(base.notifications),
        approval: override_cfg.approval.or(base.approval),
        network: override_cfg.network.or(base.network),
        network_layer_is_managed: override_cfg.network_layer_is_managed
            || base.network_layer_is_managed,
        verifier: override_cfg.verifier.or(base.verifier),
        advisor: override_cfg.advisor.or(base.advisor),
        skills: merge_skills_config(base.skills, override_cfg.skills),
        snapshots: override_cfg.snapshots.or(base.snapshots),
        search: override_cfg.search.or(base.search),
        goal: override_cfg.goal.or(base.goal),
        memory: override_cfg.memory.or(base.memory),
        speech: override_cfg.speech.or(base.speech),
        auto: override_cfg.auto.or(base.auto),
        hotbar: override_cfg.hotbar.or(base.hotbar),
        update: override_cfg.update.or(base.update),
        cloud_facts: override_cfg.cloud_facts.or(base.cloud_facts),
        lsp: override_cfg.lsp.or(base.lsp),
        context: ContextConfig {
            project_pack: override_cfg
                .context
                .project_pack
                .or(base.context.project_pack),
        },
        compaction: override_cfg.compaction.or(base.compaction),
        fleet: override_cfg.fleet.or(base.fleet),
        workflow: override_cfg.workflow.or(base.workflow),
        subagents: override_cfg.subagents.or(base.subagents),
        strict_tool_mode: override_cfg.strict_tool_mode.or(base.strict_tool_mode),
        runtime_api: override_cfg.runtime_api.or(base.runtime_api),
        workshop: override_cfg.workshop.or(base.workshop),
        exec_policy_engine: override_cfg.exec_policy_engine,
        base_url_env_receipt: match override_cfg.base_url_env_receipt {
            BaseUrlEnvReceipt::Unrecorded => base.base_url_env_receipt,
            recorded => recorded,
        },
        legacy_root: base.legacy_root,
        legacy_root_custom_generation: base.legacy_root_custom_generation,
        account_model_access: base.account_model_access,
        required_account_model_owner: base.required_account_model_owner,
        account_profile: override_cfg.account_profile.or(base.account_profile),
        plugin_oauth_read_only: base.plugin_oauth_read_only || override_cfg.plugin_oauth_read_only,
        runtime_chat_isolated: override_cfg.runtime_chat_isolated || base.runtime_chat_isolated,
        runtime_thread_inference_unrelated: override_cfg.runtime_thread_inference_unrelated
            || base.runtime_thread_inference_unrelated,
        mini_window: override_cfg.mini_window.or(base.mini_window),
        reasoning_only: override_cfg.reasoning_only.or(base.reasoning_only),
        title: override_cfg.title.or(base.title),
    }
}

fn load_sibling_exec_policy_engine(config_path: Option<&Path>) -> Result<ExecPolicyEngine> {
    let Some(config_path) = config_path else {
        return Ok(ExecPolicyEngine::new(Vec::new(), Vec::new()));
    };
    let permissions_path = codewhale_config::permissions_path_for_config_path(config_path);
    if !permissions_path.exists() {
        return Ok(ExecPolicyEngine::new(Vec::new(), Vec::new()));
    }

    let raw = fs::read_to_string(&permissions_path).with_context(|| {
        format!(
            "Failed to read permissions file: {}",
            permissions_path.display()
        )
    })?;
    let permissions: codewhale_config::PermissionsToml = toml::from_str(&raw).map_err(|_| {
        anyhow::anyhow!(
            "Failed to parse permissions file {}; file contents were omitted",
            codewhale_config::quote_os_path(&permissions_path)
        )
    })?;
    if permissions.is_empty() {
        Ok(ExecPolicyEngine::new(Vec::new(), Vec::new()))
    } else {
        Ok(ExecPolicyEngine::with_rulesets(vec![permissions.ruleset()]))
    }
}

fn merge_skills_config(
    base: Option<SkillsConfig>,
    override_cfg: Option<SkillsConfig>,
) -> Option<SkillsConfig> {
    match (base, override_cfg) {
        (None, None) => None,
        (Some(base), None) => Some(base),
        (None, Some(override_cfg)) => Some(override_cfg),
        (Some(base), Some(override_cfg)) => Some(SkillsConfig {
            registry_url: override_cfg.registry_url.or(base.registry_url),
            max_install_size_bytes: override_cfg
                .max_install_size_bytes
                .or(base.max_install_size_bytes),
            scan_codewhale_only: override_cfg
                .scan_codewhale_only
                .or(base.scan_codewhale_only),
            flat_workspace_root: override_cfg
                .flat_workspace_root
                .or(base.flat_workspace_root),
        }),
    }
}

fn merge_provider_config(base: ProviderConfig, override_cfg: ProviderConfig) -> ProviderConfig {
    ProviderConfig {
        vendor: override_cfg.vendor.or(base.vendor),
        api_key: override_cfg.api_key.or(base.api_key),
        base_url: override_cfg.base_url.or(base.base_url),
        model: override_cfg.model.or(base.model),
        context_window: override_cfg.context_window.or(base.context_window),
        model_context_windows: override_cfg
            .model_context_windows
            .or(base.model_context_windows),
        mode: override_cfg.mode.or(base.mode),
        wire: override_cfg.wire.or(base.wire),
        auth_mode: override_cfg.auth_mode.or(base.auth_mode),
        oauth: override_cfg.oauth.or(base.oauth),
        plugin_authority: base.plugin_authority,
        oauth_credential_generation: override_cfg
            .oauth_credential_generation
            .or(base.oauth_credential_generation),
        insecure_skip_tls_verify: override_cfg
            .insecure_skip_tls_verify
            .or(base.insecure_skip_tls_verify),
        allow_insecure_http: override_cfg
            .allow_insecure_http
            .or(base.allow_insecure_http),
        http_headers: override_cfg.http_headers.or(base.http_headers),
        path_suffix: override_cfg.path_suffix.or(base.path_suffix),
        reasoning_stream_style: override_cfg
            .reasoning_stream_style
            .or(base.reasoning_stream_style),
        max_concurrency: override_cfg.max_concurrency.or(base.max_concurrency),
        auth: override_cfg.auth.or(base.auth),
        external_credentials: override_cfg
            .external_credentials
            .or(base.external_credentials),
        kind: override_cfg.kind.or(base.kind),
        api_key_env: override_cfg.api_key_env.or(base.api_key_env),
    }
}

/// Merge the per-name custom provider maps (#1519): the union of both key sets,
/// with each shared key deep-merged via [`merge_provider_config`] (override
/// wins field-by-field). Keys present in only one map are carried through as-is.
fn merge_custom_providers(
    mut base: HashMap<String, ProviderConfig>,
    override_cfg: HashMap<String, ProviderConfig>,
) -> HashMap<String, ProviderConfig> {
    for (name, entry) in override_cfg {
        let merged = match base.remove(&name) {
            Some(base_entry) => merge_provider_config(base_entry, entry),
            None => entry,
        };
        base.insert(name, merged);
    }
    base
}

fn merge_providers(
    base: Option<ProvidersConfig>,
    override_cfg: Option<ProvidersConfig>,
) -> Option<ProvidersConfig> {
    match (base, override_cfg) {
        (None, None) => None,
        (Some(base), None) => Some(base),
        (None, Some(override_cfg)) => Some(override_cfg),
        (Some(base), Some(override_cfg)) => Some(ProvidersConfig {
            deepseek: merge_provider_config(base.deepseek, override_cfg.deepseek),
            deepseek_cn: merge_provider_config(base.deepseek_cn, override_cfg.deepseek_cn),
            deepseek_anthropic: merge_provider_config(
                base.deepseek_anthropic,
                override_cfg.deepseek_anthropic,
            ),
            nvidia_nim: merge_provider_config(base.nvidia_nim, override_cfg.nvidia_nim),
            openai: merge_provider_config(base.openai, override_cfg.openai),
            anthropic: merge_provider_config(base.anthropic, override_cfg.anthropic),
            openmodel: merge_provider_config(base.openmodel, override_cfg.openmodel),
            atlascloud: merge_provider_config(base.atlascloud, override_cfg.atlascloud),
            wanjie_ark: merge_provider_config(base.wanjie_ark, override_cfg.wanjie_ark),
            openrouter: merge_provider_config(base.openrouter, override_cfg.openrouter),
            orcarouter: merge_provider_config(base.orcarouter, override_cfg.orcarouter),
            xiaomi_mimo: merge_provider_config(base.xiaomi_mimo, override_cfg.xiaomi_mimo),
            novita: merge_provider_config(base.novita, override_cfg.novita),
            fireworks: merge_provider_config(base.fireworks, override_cfg.fireworks),
            siliconflow: merge_provider_config(base.siliconflow, override_cfg.siliconflow),
            siliconflow_cn: merge_provider_config(base.siliconflow_cn, override_cfg.siliconflow_cn),
            arcee: merge_provider_config(base.arcee, override_cfg.arcee),
            moonshot: merge_provider_config(base.moonshot, override_cfg.moonshot),
            sglang: merge_provider_config(base.sglang, override_cfg.sglang),
            vllm: merge_provider_config(base.vllm, override_cfg.vllm),
            ollama: merge_provider_config(base.ollama, override_cfg.ollama),
            ollama_cloud: merge_provider_config(base.ollama_cloud, override_cfg.ollama_cloud),
            volcengine: merge_provider_config(base.volcengine, override_cfg.volcengine),
            huggingface: merge_provider_config(base.huggingface, override_cfg.huggingface),
            modelscope: merge_provider_config(base.modelscope, override_cfg.modelscope),
            deepinfra: merge_provider_config(base.deepinfra, override_cfg.deepinfra),
            together: merge_provider_config(base.together, override_cfg.together),
            qianfan: merge_provider_config(base.qianfan, override_cfg.qianfan),
            openai_codex: merge_provider_config(base.openai_codex, override_cfg.openai_codex),
            zai: merge_provider_config(base.zai, override_cfg.zai),
            stepfun: merge_provider_config(base.stepfun, override_cfg.stepfun),
            minimax: merge_provider_config(base.minimax, override_cfg.minimax),
            minimax_anthropic: merge_provider_config(
                base.minimax_anthropic,
                override_cfg.minimax_anthropic,
            ),
            sakana: merge_provider_config(base.sakana, override_cfg.sakana),
            longcat: merge_provider_config(base.longcat, override_cfg.longcat),
            opencode_go: merge_provider_config(base.opencode_go, override_cfg.opencode_go),
            opencode_zen: merge_provider_config(base.opencode_zen, override_cfg.opencode_zen),
            meta: merge_provider_config(base.meta, override_cfg.meta),
            xai: merge_provider_config(base.xai, override_cfg.xai),
            mistral: merge_provider_config(base.mistral, override_cfg.mistral),
            google: merge_provider_config(base.google, override_cfg.google),
            antigravity: merge_provider_config(base.antigravity, override_cfg.antigravity),
            telecomjs: merge_provider_config(base.telecomjs, override_cfg.telecomjs),
            edenai: merge_provider_config(base.edenai, override_cfg.edenai),
            zenmux: merge_provider_config(base.zenmux, override_cfg.zenmux),
            csdn: merge_provider_config(base.csdn, override_cfg.csdn),
            concentrate: merge_provider_config(base.concentrate, override_cfg.concentrate),
            codewhale: merge_provider_config(base.codewhale, override_cfg.codewhale),
            modelstudio_token_plan: merge_provider_config(
                base.modelstudio_token_plan,
                override_cfg.modelstudio_token_plan,
            ),
            modelstudio_token_plan_anthropic: merge_provider_config(
                base.modelstudio_token_plan_anthropic,
                override_cfg.modelstudio_token_plan_anthropic,
            ),
            modelstudio_coding_plan: merge_provider_config(
                base.modelstudio_coding_plan,
                override_cfg.modelstudio_coding_plan,
            ),
            modelstudio_coding_plan_anthropic: merge_provider_config(
                base.modelstudio_coding_plan_anthropic,
                override_cfg.modelstudio_coding_plan_anthropic,
            ),
            custom: merge_custom_providers(base.custom, override_cfg.custom),
        }),
    }
}

fn load_single_config_file(path: &Path) -> Result<Config> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    let parsed = parse_config_file(&contents).map_err(|_| {
        anyhow::anyhow!(
            "Failed to parse config file {}; file contents were omitted",
            codewhale_config::quote_os_path(path)
        )
    })?;
    Ok(*parsed.base)
}

/// Table key for a built-in route's `[providers.<key>]` entry.
#[cfg(test)]
impl Config {
    /// Test shorthand for a config file's legacy top-level `api_key` /
    /// `base_url`: they go through the #6394 canonicalizer exactly as a
    /// parsed file's would, landing in whichever table owns them.
    pub(crate) fn with_legacy_root(
        mut self,
        api_key: Option<String>,
        base_url: Option<String>,
    ) -> Self {
        self.set_legacy_root(api_key, base_url);
        self
    }

    /// `[providers.deepseek] api_key`, where a legacy top-level key lands.
    pub(crate) fn deepseek_table_api_key(&self) -> Option<&str> {
        self.providers.as_ref()?.deepseek.api_key.as_deref()
    }

    /// `[providers.deepseek] base_url`, where a legacy top-level endpoint
    /// lands.
    pub(crate) fn deepseek_table_base_url(&self) -> Option<&str> {
        self.providers.as_ref()?.deepseek.base_url.as_deref()
    }

    /// In-place form of [`Config::with_legacy_root`].
    pub(crate) fn set_legacy_root(&mut self, api_key: Option<String>, base_url: Option<String>) {
        use toml::Value;
        let mut root = toml::Table::new();
        if let Some(provider) = &self.provider {
            root.insert("provider".into(), Value::String(provider.clone()));
        }
        if let Some(model) = self
            .default_text_model
            .clone()
            .or(self.legacy_model.clone())
        {
            root.insert("default_text_model".into(), Value::String(model));
        }
        if let Some(key) = api_key {
            root.insert("api_key".into(), Value::String(key));
        }
        if let Some(url) = base_url {
            root.insert("base_url".into(), Value::String(url));
        }
        let entry_table = |entry: &ProviderConfig| {
            let mut table = toml::Table::new();
            if let Some(key) = &entry.api_key {
                table.insert("api_key".into(), Value::String(key.clone()));
            }
            if let Some(url) = &entry.base_url {
                table.insert("base_url".into(), Value::String(url.clone()));
            }
            if let Some(model) = &entry.model {
                table.insert("model".into(), Value::String(model.clone()));
            }
            if let Some(kind) = &entry.kind {
                table.insert("kind".into(), Value::String(kind.clone()));
            }
            table
        };
        let mut providers = toml::Table::new();
        if let Some(configured) = self.providers.as_ref() {
            for row in codewhale_config::descriptors::provider_compatibility() {
                if let Some(entry) =
                    codewhale_config::provider_config_table!(@read configured, row.id)
                {
                    providers.insert(row.config_key.to_string(), Value::Table(entry_table(entry)));
                }
            }
            for (name, entry) in &configured.custom {
                providers.insert(name.clone(), Value::Table(entry_table(entry)));
            }
        }
        root.insert("providers".into(), Value::Table(providers));
        if let Some(vision) = &self.vision_model {
            let mut table = toml::Table::new();
            if let Some(key) = &vision.api_key {
                table.insert("api_key".into(), Value::String(key.clone()));
            }
            root.insert("vision_model".into(), Value::Table(table));
        }

        self.legacy_root = codewhale_config::legacy_root::apply_to_table(&mut root);

        if let Some(provider) = root.get("provider").and_then(Value::as_str) {
            self.provider = Some(provider.to_string());
        }
        if let (Some(vision), Some(key)) = (
            self.vision_model.as_mut(),
            root.get("vision_model")
                .and_then(|table| table.get("api_key"))
                .and_then(Value::as_str),
        ) {
            vision.api_key = Some(key.to_string());
        }
        let Some(providers) = root.get("providers").and_then(Value::as_table) else {
            return;
        };
        for (key, table) in providers {
            let Some(table) = table.as_table() else {
                continue;
            };
            let entry = if let Some(row) = codewhale_config::descriptors::provider_compatibility()
                .iter()
                .find(|row| row.config_key == key && row.kind != ProviderKind::Custom)
            {
                let configured = self.providers.get_or_insert_with(ProvidersConfig::default);
                let Some(entry) =
                    codewhale_config::provider_config_table!(@write configured, row.id)
                else {
                    continue;
                };
                entry
            } else {
                self.providers
                    .get_or_insert_with(ProvidersConfig::default)
                    .custom
                    .entry(key.clone())
                    .or_default()
            };
            let get = |field: &str| table.get(field).and_then(Value::as_str).map(str::to_string);
            entry.api_key = get("api_key");
            entry.base_url = get("base_url");
            entry.model = get("model");
            entry.kind = get("kind");
        }
        self.bind_legacy_root_custom_generation();
    }
}

/// Parse one `config.toml`-shaped layer (no profile applied) through
/// [`parse_config_file`], so the legacy top-level keys are canonicalized.
pub(crate) fn parse_config_base(contents: &str) -> std::result::Result<Config, toml::de::Error> {
    let parsed = parse_config_file(contents)?;
    let mut config = *parsed.base;
    config.legacy_root = parsed.legacy_root;
    Ok(config)
}

/// Project the fresh receipt returned by the existing locked document writer.
/// This does not reuse the caller's diagnostic notes: a root-less document
/// receives no missing-id provenance, and verification still compares the
/// complete current table generation against the previously captured identity.
pub(crate) fn parse_config_after_locked_migration(
    contents: &str,
    moved: &codewhale_config::legacy_root::LegacyRootMigration,
) -> std::result::Result<Config, toml::de::Error> {
    let mut config = parse_config_base(contents)?;
    config.legacy_root.notes.extend(moved.notes.iter().cloned());
    config.bind_legacy_root_custom_generation();
    Ok(config)
}

/// Parse a `config.toml`-shaped document. The legacy top-level `base_url` /
/// `api_key` move into their `[providers.<name>]` tables first (#6394), so no
/// reader here ever sees them; this is the only way a `ConfigFile` is parsed.
fn parse_config_file(contents: &str) -> std::result::Result<ConfigFile, toml::de::Error> {
    let (text, legacy_root) = codewhale_config::legacy_root::canonicalize_text(contents)?;
    let mut parsed = toml::from_str::<ConfigFile>(&text)?;
    parsed.base.legacy_root = legacy_root.clone();
    parsed.base.bind_legacy_root_custom_generation();
    parsed.legacy_root = legacy_root;
    Ok(parsed)
}

/// Build a one-line warning when top-level-only keys are nested under a section
/// Codewhale does not define (`[general]` / `[sandbox]`). TOML silently drops
/// those keys, so e.g. `[general]\nallow_shell = true` never takes effect and
/// the shell tools (`exec_shell`, `task_shell_start`, …) are absent from the
/// catalog with no explanation. Returns `None` when nothing is misplaced.
///
/// This is the exact confusion behind #2589: `allow_shell` and `sandbox_mode`
/// belong at the top of the file, above any `[section]` header.
fn warn_on_misplaced_top_level_keys(raw: &str) -> Option<String> {
    let doc = toml::from_str::<toml::Value>(raw).ok()?;
    // Sections Codewhale does not recognize but users nest settings under.
    const UNKNOWN_SECTIONS: &[&str] = &["general", "sandbox"];
    // Keys that are only ever read from the top level of the config.
    const TOP_LEVEL_KEYS: &[&str] = &[
        "allow_shell",
        "sandbox_mode",
        "approval_policy",
        "verbosity",
    ];

    let mut hits: Vec<String> = Vec::new();
    for section in UNKNOWN_SECTIONS {
        let Some(table) = doc.get(*section).and_then(toml::Value::as_table) else {
            continue;
        };
        for key in TOP_LEVEL_KEYS {
            if table.contains_key(*key) {
                hits.push(format!("`{section}.{key}`"));
            }
        }
    }
    if hits.is_empty() {
        return None;
    }
    Some(format!(
        "Ignoring {} — Codewhale has no `[general]` or `[sandbox]` section, so these \
         keys are silently dropped. Move them to the TOP of the config file (above any \
         `[section]` header), e.g. `allow_shell = true`. Until then, shell tools stay \
         disabled. (#2589)",
        hits.join(", ")
    ))
}

fn apply_managed_overrides(config: &mut Config) -> Result<()> {
    let path = config
        .managed_config_path
        .as_deref()
        .map(expand_path)
        .or_else(default_managed_config_path);
    let Some(path) = path else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    let mut managed = load_single_config_file(&path)?;
    strip_external_credential_consent(&mut managed);
    let prior_route = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    let mut merged = merge_config(config.clone(), managed.clone());
    apply_layer_root_model(&mut merged, &managed);
    let merged_route = merged
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    if managed.provider.is_some()
        || managed.default_text_model.is_some()
        || managed.legacy_model.is_some()
        || managed
            .provider_config_for(&merged_route)
            .and_then(|entry| entry.model.as_ref())
            .is_some()
    {
        merged.remembered_selection_scope = Some(false);
    }
    if prior_route != merged_route || config_defines_base_url_for_effective_route(&managed, &merged)
    {
        // Managed configuration is a higher-precedence file layer. If it
        // selects a different route or supplies that route's endpoint, the
        // lower environment layer no longer owns the effective base URL.
        //
        // Record that as an explicit "nobody owns it" rather than clearing the
        // receipt. Clearing it would read as "this config never met the
        // environment layer", which re-enables the generic
        // `CODEWHALE_BASE_URL` fallback for every route — including pinned
        // cross-provider children, which would then borrow an ambient host
        // that managed routing had just taken authority over.
        //
        // DeepSeek's env-written endpoint lives in its own table now that
        // there is no shared top-level field (#6394). Managed authority takes
        // that ambient value away from every route, as it always did.
        if let BaseUrlEnvReceipt::Route(ProviderKind::Deepseek, key) = &config.base_url_env_receipt
            && let Ok(owner) = merged.resolve_persisted_provider_identity(
                Some(ProviderKind::Deepseek.as_str()),
                Some(key),
            )
            && managed
                .provider_config_for(&owner)
                .and_then(|entry| entry.base_url.as_ref())
                .is_none()
            && let Some(env_value) = env_base_url_override()
            && merged
                .provider_config_for(&owner)
                .and_then(|entry| entry.base_url.as_deref())
                == Some(env_value.as_str())
        {
            merged.provider_config_for_mut(&owner)?.base_url = None;
        }
        merged.base_url_env_receipt = BaseUrlEnvReceipt::NoOwner;
    }
    // A managed `[network]` table outranks the user document. Record that the
    // resolved policy is not the document's to replace, so a mid-session
    // re-read of that document folds onto it instead of widening it.
    if managed.network.is_some() {
        merged.network_layer_is_managed = true;
    }
    *config = merged;
    Ok(())
}

/// Organization-managed overlays may constrain routing and policy, but they
/// cannot consent on a user's behalf to credential files owned by another
/// CLI. Only the user config/profile loaded before this layer may carry these
/// grants. A managed `disabled` record is a tightening tombstone and is kept
/// so a lower-precedence user grant cannot survive an administrator deny.
fn strip_external_credential_consent(config: &mut Config) {
    if config.providers.is_none() {
        return;
    }
    for row in codewhale_config::descriptors::provider_compatibility() {
        if row.kind == ProviderKind::Custom {
            continue;
        }
        let Ok(identity) = config.resolve_persisted_provider_identity(Some(row.id), Some(row.id))
        else {
            continue;
        };
        let Ok(entry) = config.provider_config_for_mut(&identity) else {
            continue;
        };
        if entry.external_credentials.as_ref().is_some_and(|consent| {
            consent.access != codewhale_config::ExternalCredentialAccess::Disabled
        }) {
            entry.external_credentials = None;
        }
    }
    if let Some(providers) = config.providers.as_mut() {
        for provider in providers.custom.values_mut() {
            if provider
                .external_credentials
                .as_ref()
                .is_some_and(|consent| {
                    consent.access != codewhale_config::ExternalCredentialAccess::Disabled
                })
            {
                provider.external_credentials = None;
            }
        }
    }
}

fn config_defines_base_url_for_effective_route(source: &Config, effective: &Config) -> bool {
    let Ok(identity) = effective.active_provider_identity() else {
        return false;
    };
    let mut source = source.clone();
    if source.scope_to_provider_identity(&identity).is_err() {
        return false;
    }
    source
        .provider_route_string_with_deepseek_fallback(&identity, |entry| entry.base_url.clone())
        .is_some_and(|base| !base.trim().is_empty())
}

fn apply_requirements(config: &mut Config) -> Result<()> {
    let path = config
        .requirements_path
        .as_deref()
        .map(expand_path)
        .or_else(default_requirements_path);
    let Some(path) = path else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read requirements file: {}", path.display()))?;
    let requirements: RequirementsFile = toml::from_str(&contents).map_err(|_| {
        anyhow::anyhow!(
            "Failed to parse requirements file {}; file contents were omitted",
            codewhale_config::quote_os_path(&path)
        )
    })?;

    if !requirements.allowed_approval_policies.is_empty() {
        use codewhale_execpolicy::ApprovalMode;

        let policy = config
            .approval_policy
            .as_deref()
            .unwrap_or("on-request")
            .to_ascii_lowercase();
        if !requirements.allowed_approval_policies.iter().any(|p| {
            match (
                ApprovalMode::from_config_value(p),
                ApprovalMode::from_config_value(&policy),
            ) {
                (Some(allowed), Some(effective)) => allowed == effective,
                _ => p.eq_ignore_ascii_case(&policy),
            }
        }) {
            anyhow::bail!(
                "approval_policy '{policy}' is not allowed by requirements ({})",
                requirements.allowed_approval_policies.join(", ")
            );
        }
    }
    if !requirements.allowed_sandbox_modes.is_empty() {
        // At config load there is no live turn mode yet. Check the Agent
        // baseline with the engine's resolver; Plan can narrow it later.
        // Explicit settings retain their existing allow-list check.
        let mode = config
            .sandbox_mode
            .as_deref()
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| {
                use crate::core::authority::{SandboxNetworkAccess, sandbox_policy_for_turn};
                use crate::sandbox::SandboxPolicy;
                use codewhale_execpolicy::ApprovalMode;

                // Sandbox requirements lock approval posture at startup,
                // excluding saved preferences and every YOLO override.
                let approval = config
                    .approval_policy
                    .as_deref()
                    .and_then(ApprovalMode::from_config_value)
                    .unwrap_or_default();
                match sandbox_policy_for_turn(
                    codewhale_config::AppMode::Agent,
                    approval,
                    None,
                    Path::new("."),
                    SandboxNetworkAccess::from_config(config.sandbox_network_access),
                ) {
                    SandboxPolicy::ReadOnly => "read-only",
                    SandboxPolicy::WorkspaceWrite { .. } => "workspace-write",
                    SandboxPolicy::DangerFullAccess => "danger-full-access",
                    SandboxPolicy::ExternalSandbox { .. } => "external-sandbox",
                }
                .to_string()
            });
        if !requirements
            .allowed_sandbox_modes
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&mode))
        {
            anyhow::bail!(
                "sandbox_mode '{mode}' is not allowed by requirements ({})",
                requirements.allowed_sandbox_modes.join(", ")
            );
        }
    }

    Ok(())
}

fn merge_features(
    base: Option<FeaturesToml>,
    override_cfg: Option<FeaturesToml>,
) -> Option<FeaturesToml> {
    match (base, override_cfg) {
        (None, None) => None,
        (Some(mut base), Some(override_cfg)) => {
            for (key, value) in override_cfg.entries {
                base.entries.insert(key, value);
            }
            Some(base)
        }
        (Some(base), None) => Some(base),
        (None, Some(override_cfg)) => Some(override_cfg),
    }
}

pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        #[cfg(unix)]
        {
            // Tighten group/other bits on the parent dir as a hardening pass.
            // The dir lives under the user's home, so the chmod is best-effort:
            // filesystems that don't accept Unix permission bits (Docker
            // bind-mounts of NTFS, network shares, FAT, certain CI volumes —
            // see #897) return EPERM/ENOTSUP. The dir already exists by the
            // time we get here, so failing the whole save just because we
            // couldn't tighten perms strands the user mid-onboarding. Warn
            // loudly so a security-sensitive operator can still notice via
            // `RUST_LOG=warn`, then continue.
            if let Ok(meta) = fs::metadata(parent) {
                let mode = meta.permissions().mode();
                if mode & 0o077 != 0 {
                    let mut perms = meta.permissions();
                    perms.set_mode(mode & !0o077);
                    if let Err(err) = fs::set_permissions(parent, perms) {
                        tracing::warn!(
                            target: "codewhale::config",
                            path = %parent.display(),
                            error = %err,
                            "could not tighten parent dir permissions; \
                             filesystem may not support Unix chmod \
                             (Docker bind-mount, NTFS, network share). \
                             Continuing — the file will still be written."
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Write content to a config file with restrictive permissions (owner-only read/write).
/// On Unix this sets mode 0o600 before writing.
fn write_config_file_secure(path: &Path, content: &str) -> Result<()> {
    codewhale_config::create_config_document(path, content)
}

/// Where a saved credential ended up. Returned by [`save_api_key`] so
/// the caller can show a confirmation message without leaking the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedCredential {
    /// Stored in the durable secret store. The config file contains only
    /// non-secret provider metadata and has any matching plaintext `api_key`
    /// entry removed. The `backend` label is the value of
    /// [`codewhale_secrets::Secrets::backend_name`] at write time so the toast
    /// text can name the actual backend (`"system keyring"`,
    /// `"file-based (~/.codewhale/secrets/)"`).
    KeyringAndConfigFile {
        /// `Secrets::backend_name()` at write time.
        backend: String,
        /// Absolute path to the credential-free config metadata file.
        path: PathBuf,
    },
    /// Stored in the Codewhale config file only under `cfg(test)` so unit tests
    /// without an explicitly isolated secret backend do not pollute the host
    /// credential store. Production save flows never automatically downgrade
    /// a failed secret-store write to plaintext.
    ConfigFile(PathBuf),
}

impl SavedCredential {
    /// Human-readable description for status / log output. Never
    /// includes the key value.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::KeyringAndConfigFile { backend, path } => {
                format!(
                    "secret store ({backend}); credential-free config metadata in {}",
                    path.display()
                )
            }
            Self::ConfigFile(path) => path.display().to_string(),
        }
    }
}

/// Resolve the config document for CREDENTIAL writes: api_key values,
/// `auth_mode` markers, and oauth/external-credential pointers.
///
/// Credentials are user-global — a key saved while working in one repo must be
/// visible from every other repo (#5045, #5193). The ambient
/// `CODEWHALE_CONFIG_PATH`/`DEEPSEEK_CONFIG_PATH` override can point at a
/// workspace-scoped document (`<repo>/.codewhale/config.toml`, plaintext and
/// easy to commit by accident), so credential writes that would land there are
/// rescoped to the user-global config instead. Non-credential settings keep
/// the ambient scoping, and callers that pass an explicit config path never
/// consult this resolver; a per-workspace destination stays possible only as
/// that kind of explicit opt-in.
fn credential_config_path() -> anyhow::Result<PathBuf> {
    let resolved = try_default_config_path()?;
    if !codewhale_config::config_path_is_workspace_scoped(&resolved) {
        return Ok(resolved);
    }
    let global = home_config_path()
        .context("Failed to resolve user-global config path: home directory not found.")?;
    tracing::info!(
        ambient = %resolved.display(),
        global = %global.display(),
        "rescoping credential write from workspace config to user-global config"
    );
    Ok(global)
}

/// Save the active provider's API key.
///
/// The selected durable secret backend is attempted first. On success the
/// config keeps only non-secret auth metadata and any older plaintext copy is
/// removed. When the secret-store write fails (OS permission denied, corrupt
/// or read-only file backend, etc.), the save fails loudly rather than writing
/// the key to plaintext `config.toml`.
///
/// Under `cfg(test)` the secret-store path is enabled only when the test sets
/// both an isolated `CODEWHALE_HOME` and an explicit backend, preventing unit
/// tests from touching the developer's real credential store.
pub fn save_api_key(api_key: &str) -> Result<SavedCredential> {
    save_root_api_key_for_secret_slot(api_key, "deepseek", "deepseek")
}

fn save_root_api_key_for_secret_slot(
    api_key: &str,
    secret_slot: &str,
    table: &str,
) -> Result<SavedCredential> {
    // #6528: strip pasted invisible characters and whitespace in one place.
    let normalized = codewhale_secrets::normalize_api_key(api_key);
    let trimmed = normalized.as_str();
    if trimmed.is_empty() {
        anyhow::bail!("Refusing to save an empty API key.");
    }

    let path = credential_config_path().context("Failed to resolve config path for API key.")?;

    if let Some(secrets) = credential_secret_store() {
        // Same read-modify-write as the per-provider save below; hold the slot's
        // write lock across snapshot, store write, config write, and rollback.
        return crate::credentials::store::with_provider_write_lock(secret_slot, || {
            let prior_secret = secrets.get(secret_slot);
            match prior_secret.as_ref() {
                Ok(prior) => match secrets.set(secret_slot, trimmed) {
                    Ok(()) => {
                        if let Err(error) =
                            save_root_api_key_metadata_without_plaintext(&path, table)
                        {
                            let current = secrets.get(secret_slot).map_err(|rollback| {
                        anyhow::anyhow!(
                            "{error}; additionally could not verify secret-store rollback for {secret_slot}: {rollback}"
                        )
                    })?;
                            if current.as_deref() == Some(trimmed) {
                                match prior {
                            Some(previous) => secrets.set(secret_slot, previous),
                            None => secrets.delete(secret_slot),
                        }
                        .map_err(|rollback| {
                            anyhow::anyhow!(
                                "{error}; additionally failed to restore prior secret-store state for {secret_slot}: {rollback}"
                            )
                        })?;
                            }
                            return Err(error);
                        }
                        codewhale_config::scrub_plaintext_api_keys_from_config_backup(&path)?;
                        let backend = secrets.backend_name().to_string();
                        log_sensitive_event(
                            "credential.save",
                            json!({
                                "backend": backend.clone(),
                                "config_path": path.display().to_string(),
                                "plaintext_config_fallback": false,
                            }),
                        );
                        Ok(SavedCredential::KeyringAndConfigFile { backend, path })
                    }
                    Err(err) => Err(plaintext_credential_fallback_refused("write", &path, &err)),
                },
                Err(error) => Err(plaintext_credential_fallback_refused(
                    "snapshot", &path, &error,
                )),
            }
        });
    }

    let path = save_api_key_to_config_file(trimmed, table)?;
    codewhale_config::scrub_plaintext_api_keys_from_config_backup(&path)?;
    Ok(SavedCredential::ConfigFile(path))
}

fn plaintext_credential_fallback_refused(
    operation: &str,
    config_path: &Path,
    failure: &dyn std::fmt::Display,
) -> anyhow::Error {
    anyhow::anyhow!(
        "Secret storage {operation} failed: {failure}. Refusing to write the API key in plaintext to {}. Fix the configured secret backend and retry; Codewhale did not change that file.",
        codewhale_config::quote_os_path(config_path)
    )
}

/// The durable secret store for credential saves and logout-time deletes.
///
/// Under `cfg(test)` the store is only exposed when the test set both an
/// isolated `CODEWHALE_HOME` and an explicit backend, so unit tests can never
/// touch the developer's real credential store.
#[cfg(not(test))]
pub(crate) fn credential_secret_store() -> Option<codewhale_secrets::Secrets> {
    Some(codewhale_secrets::Secrets::auto_detect())
}

#[cfg(test)]
pub(crate) fn credential_secret_store() -> Option<codewhale_secrets::Secrets> {
    let isolated_home = codewhale_paths::codewhale_home_is_explicit();
    let explicit_backend = std::env::var_os("CODEWHALE_SECRET_BACKEND")
        .or_else(|| std::env::var_os("DEEPSEEK_SECRET_BACKEND"))
        .is_some_and(|value| !value.is_empty());
    (isolated_home && explicit_backend).then(codewhale_secrets::Secrets::auto_detect)
}

fn save_root_api_key_metadata_without_plaintext(config_path: &Path, table: &str) -> Result<()> {
    ensure_parent_dir(config_path)?;
    crate::config_persistence::mutate_config_document(config_path, |doc| {
        crate::config_persistence::set_document_value(doc, &["auth_mode"], "api_key")?;
        // Saving a key never pins a model (see
        // `codewhale_config::credentials::prepare_provider_api_key_metadata`).
        if !doc.contains_key("reasoning_effort") {
            crate::config_persistence::set_document_value(doc, &["reasoning_effort"], "max")?;
        }
        crate::config_persistence::unset_document_value(doc, &["api_key"])?;
        crate::config_persistence::unset_document_value(doc, &["providers", table, "api_key"])?;
        if table == "deepseek" {
            crate::config_persistence::unset_document_value(
                doc,
                &["providers", "deepseek-cn", "api_key"],
            )?;
        }
        Ok(())
    })
    .with_context(|| format!("Failed to write config to {}", config_path.display()))
}

/// Write the key directly to `[providers.<table>] api_key` in `config.toml`.
fn save_api_key_to_config_file(api_key: &str, table: &str) -> Result<PathBuf> {
    let config_path =
        credential_config_path().context("Failed to resolve config path for API key.")?;

    ensure_parent_dir(&config_path)?;

    if config_path.exists() {
        // TOML-aware upsert. The old line scan keyed off
        // `existing.contains("api_key")`, so a comment that merely mentioned
        // api_key made it skip the insert entirely; editing the document
        // replaces or inserts the real key and keeps user comments.
        crate::config_persistence::mutate_config_document(&config_path, |doc| {
            // DeepSeek's key lives in its own table (#6394); an explicit save
            // also ends any disagreement with an older top-level key.
            crate::config_persistence::set_document_value(
                doc,
                &["providers", table, "api_key"],
                api_key,
            )?;
            crate::config_persistence::unset_document_value(doc, &["api_key"])?;
            crate::config_persistence::set_document_value(doc, &["auth_mode"], "api_key")
        })
        .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    } else {
        // Create new minimal config
        let content = format!(
            r#"# codewhale Configuration
# Set provider credentials in this file or via environment variables.
# See /links in the TUI for provider-specific credential pages.

auth_mode = "api_key"

# Default model (unset follows the provider default)
# default_text_model = "{DEFAULT_TEXT_MODEL}"

# Thinking mode (DeepSeek V4 reasoning effort):
# "off" | "low" | "medium" | "high" | "max"
# Ctrl+T in the TUI (or /effort) cycles the active model's effort levels.
reasoning_effort = "max"

[providers.{table}]
api_key = "{api_key}"
# Base URL (default: https://api.deepseek.com/beta)
# Set https://api.deepseek.com to opt out of beta features.
# base_url = "https://api.deepseek.com/beta"
"#
        );
        crate::config_persistence::write_config_toml_atomic(&config_path, &content)
            .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    }

    log_sensitive_event(
        "credential.save",
        json!({
            "backend": "config_file",
            "config_path": config_path.display().to_string(),
        }),
    );

    Ok(config_path)
}

/// Check if the active provider has any API key configured anywhere the
/// runtime can resolve it.
///
/// The default secret store is file-backed and prompt-free. An OS credential
/// store is queried only when the user explicitly selects the system backend.
///
/// Used by the TUI app constructor to decide whether to gate
/// the user behind the in-TUI api-key onboarding screen — getting
/// this wrong made users get prompted for credentials in situations
/// where normal env/config auth was already available.
pub fn has_api_key(config: &Config) -> bool {
    config
        .active_provider_identity()
        .is_ok_and(|identity| has_api_key_for(config, &identity))
}

fn provider_uses_oauth_credentials(config: &Config, identity: &ProviderIdentity) -> bool {
    let provider = identity.provider;
    !auth_mode_disables_api_key(config.auth_mode_for_provider(identity).as_deref())
        && !config.provider_uses_custom_endpoint(identity)
        && (provider == ProviderKind::OpenaiCodex
            || (provider == ProviderKind::Anthropic
                && config.auth_mode_for_provider(identity).as_deref() == Some("oauth"))
            || (provider == ProviderKind::Moonshot
                && config
                    .provider_config_for(identity)
                    .is_some_and(provider_config_uses_kimi_imported_token))
            || (provider == ProviderKind::Xai
                && config
                    .provider_config_for(identity)
                    .is_some_and(provider_config_uses_xai_oauth)))
}

/// The environment variable name a provider route explicitly binds via
/// `[providers.<name>] api_key_env`, when credentials are bound to the active
/// endpoint. `None` when the route declares no binding.
fn bound_provider_api_key_env_name(config: &Config, identity: &ProviderIdentity) -> Option<String> {
    if !config.config_credentials_are_bound_to_provider_endpoint(identity) {
        return None;
    }
    config
        .provider_config_for(identity)
        .and_then(|entry| entry.api_key_env.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn provider_config_env_api_key(config: &Config, identity: &ProviderIdentity) -> Option<String> {
    let env_name = bound_provider_api_key_env_name(config, identity)?;
    std::env::var(env_name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[must_use]
pub fn active_provider_has_config_api_key(config: &Config) -> bool {
    let Ok(identity) = config.active_provider_identity() else {
        return false;
    };
    let provider = identity.provider;
    if auth_mode_disables_api_key(config.auth_mode_for_provider(&identity).as_deref()) {
        return false;
    }
    let custom_endpoint = config.provider_uses_custom_endpoint(&identity);

    if provider == ProviderKind::Moonshot
        && !custom_endpoint
        && config
            .provider_config_for(&identity)
            .is_some_and(provider_config_uses_kimi_imported_token)
    {
        return false;
    }
    if provider == ProviderKind::OpenaiCodex && !custom_endpoint {
        return crate::oauth::credentials_valid(crate::oauth::OAuthProvider::Chatgpt, config);
    }
    if !custom_endpoint
        && matches!(provider, ProviderKind::Huggingface)
        && std::env::var("HUGGINGFACE_API_KEY")
            .or_else(|_| std::env::var("HF_TOKEN"))
            .is_ok_and(|k| !k.trim().is_empty())
    {
        return true;
    }
    if !custom_endpoint
        && matches!(provider, ProviderKind::Modelscope)
        && std::env::var("MODELSCOPE_API_KEY").is_ok_and(|k| !k.trim().is_empty())
    {
        return true;
    }

    if config.config_credentials_are_bound_to_provider_endpoint(&identity)
        && config
            .provider_route_string_with_deepseek_fallback(&identity, |entry| entry.api_key.clone())
            .is_some_and(|key| {
                classify_config_api_key_value(&key) == ConfigApiKeyValueKind::Literal
            })
    {
        return true;
    }
    if !config.should_skip_secret_store_for_provider(&identity)
        && provider_secret_store_api_key(config, &identity).is_some()
    {
        return true;
    }

    false
}

#[must_use]
pub fn active_provider_has_env_api_key(config: &Config) -> bool {
    active_provider_env_api_key_source(config).is_some()
}

/// Where the active provider's environment key comes from, in the resolver's
/// env precedence (`credential_resolve` steps 2-4): `--api-key`, the route's
/// `api_key_env` variable, or the provider's own ambient variable. Returns the
/// place's name only; any value read to test presence is dropped here.
#[must_use]
pub(crate) fn active_provider_env_api_key_source(config: &Config) -> Option<String> {
    let identity = config.active_provider_identity().ok()?;
    let provider = identity.provider;
    if provider == ProviderKind::OpenaiCodex && !config.provider_uses_custom_endpoint(&identity) {
        return None;
    }
    if auth_mode_disables_api_key(config.auth_mode_for_provider(&identity).as_deref()) {
        return None;
    }
    if !provider_uses_oauth_credentials(config, &identity)
        && explicit_cli_api_key_override().is_some()
    {
        return Some("--api-key".to_string());
    }
    if provider_config_env_api_key(config, &identity).is_some() {
        return bound_provider_api_key_env_name(config, &identity);
    }
    if config.should_skip_secret_store_for_provider(&identity) {
        return None;
    }
    provider_env_api_key_named(provider).map(|(name, _)| name.to_string())
}

#[must_use]
pub fn active_provider_uses_env_only_api_key(config: &Config) -> bool {
    active_provider_has_env_api_key(config) && !active_provider_has_config_api_key(config)
}

/// A key saved in the user-global config file stays visible even when this
/// process loaded a DIFFERENT config (e.g. an explicit workspace `--config`
/// path). Credentials are user-global: a workspace override may select a
/// different route, but it must never make a global credential appear locked.
///
/// Bounded, read-only, non-migrating: parses the default config file's raw
/// provider table directly (never runs legacy migration, never opens a
/// write-capable backend). Returns the key only when it reads as a real
/// literal, not a placeholder.
struct UserGlobalConfigCache {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    json: serde_json::Value,
}

fn user_global_config_json() -> Option<serde_json::Value> {
    static CACHE: Mutex<Option<UserGlobalConfigCache>> = Mutex::new(None);
    let path = codewhale_config::default_config_path().ok()?;
    let meta = fs::metadata(&path).ok()?;
    let modified = meta.modified().ok();
    let len = meta.len();
    let mut guard = CACHE.lock().ok()?;
    if let Some(cached) = guard.as_ref()
        && cached.path == path
        && cached.modified == modified
        && cached.len == len
    {
        return Some(cached.json.clone());
    }
    let text = fs::read_to_string(&path).ok()?;
    let doc = codewhale_config::parse_config_toml(&text).ok()?;
    let json = serde_json::to_value(&doc).ok()?;
    *guard = Some(UserGlobalConfigCache {
        path,
        modified,
        len,
        json: json.clone(),
    });
    Some(json)
}

fn user_global_config_api_key(identity: &ProviderIdentity) -> Option<String> {
    let provider = identity.provider;
    if provider == ProviderKind::Custom {
        // Custom providers are per-config by nature; the probe applies to
        // built-in ids whose keys are saved under the user-global file.
        return None;
    }
    let json = user_global_config_json()?;
    let provider_config_key = identity.compatibility()?.config_key;
    let key = json
        .get("providers")?
        .get(provider_config_key)?
        .get("api_key")?
        .as_str()?;
    let key = key.trim();
    if key.is_empty() || classify_config_api_key_value(key) != ConfigApiKeyValueKind::Literal {
        return None;
    }
    Some(key.to_string())
}

/// Check whether the given provider has any usable API key — via env var,
/// provider/root config. Used by the `/provider` picker to decide whether to
/// prompt for a key inline.
#[must_use]
pub fn has_api_key_for(config: &Config, identity: &ProviderIdentity) -> bool {
    if identity.provider == ProviderKind::Custom
        && config
            .provider_config_for(identity)
            .is_some_and(|entry| entry.oauth.is_some())
    {
        return crate::provider_readiness::credential_state_for_provider(config, identity)
            == crate::provider_readiness::CredentialState::Saved;
    }
    credential_resolve::resolve_credential_source(config, identity).is_present()
}

/// `(key, source label)` as the active-route resolver returns it.
pub(crate) type ResolvedApiKey = (String, String);

/// Account label of the xAI sign-in that minted a key; `None` when its ID
/// token names no email.
pub(crate) type XaiSignInLabel = Option<String>;

/// Key-source label the resolver gives a key minted by xAI OAuth (owned
/// sign-in or consented Grok CLI import), named in authentication errors.
pub(crate) const XAI_OAUTH_KEY_SOURCE: &str = "xAI OAuth login";

impl Config {
    /// Resolve Codewhale's verified ChatGPT grant. Credential refresh remains
    /// serialized by the owned-store lifecycle transaction.
    pub(crate) fn codex_credentials(&self) -> Result<crate::oauth::OwnedOAuthCredentials> {
        let identity = self
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            identity.provider == ProviderKind::OpenaiCodex
                && !self.provider_uses_custom_endpoint(&identity),
            "ChatGPT credentials are only available on the official public API route"
        );
        crate::oauth::official_chatgpt_registration(self)?;
        crate::oauth::get_owned_credentials(crate::oauth::OAuthProvider::Chatgpt, self)
    }

    /// Account identifier from the selected Codewhale-owned ChatGPT grant.
    #[cfg(test)]
    pub(crate) fn codex_account_id(&self) -> Option<String> {
        self.codex_credentials()
            .ok()
            .and_then(|credentials| credentials.account_id)
    }
}

/// Whether a provider counts as "configured" for the default `/provider`
/// and `/model` manager views (#3830). Shared by both pickers so "what shows
/// up without browsing the full catalog" stays a single definition.
/// Self-hosted providers (Ollama/Sglang/Vllm) report `has_key = true`
/// unconditionally in [`has_api_key_for`] since they don't require auth to
/// route to — that's correct for routing, but wrong for "did the user set
/// this up," so a self-hosted provider only qualifies via an explicit
/// `[providers.<name>]` entry or being active, never via `has_key` alone
/// (otherwise every self-hosted provider type would always show up).
#[must_use]
pub(crate) fn provider_is_configured(
    provider: ProviderKind,
    is_active: bool,
    has_key: bool,
    configured: Option<&ProviderConfig>,
    is_named_custom_entry: bool,
) -> bool {
    // A *named* custom provider entry (one the user actually added) always
    // counts. The unconfigured `Custom` placeholder row that fills the slot
    // when no custom provider exists yet is not itself "configured" — it's
    // the catalog's invitation to add one.
    if is_active || is_named_custom_entry {
        return true;
    }
    if configured.is_some_and(provider_config_is_explicit) {
        return true;
    }
    if matches!(
        provider,
        ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm
    ) {
        return false;
    }
    has_key
}

/// Convenience wrapper around [`provider_is_configured`] for callers that
/// just want "is this provider configured given the active one," without
/// the provider picker's multi-row named-custom-provider bookkeeping
/// (`is_named_custom_entry`) — e.g. the `/model` picker (#3830), which only
/// ever resolves the single, currently-selected `Custom` slot via
/// [`Config::provider_config_for`], the same way model/route resolution
/// does everywhere else.
#[must_use]
pub(crate) fn provider_is_configured_for_active(
    config: &Config,
    identity: &ProviderIdentity,
    active: &ProviderIdentity,
) -> bool {
    provider_is_configured(
        identity.provider,
        identity == active,
        has_api_key_for(config, identity),
        config.provider_config_for(identity),
        identity.provider == ProviderKind::Custom,
    )
}

/// True when a `[providers.<name>]` table entry has any field the user would
/// have had to set explicitly — base URL, model, auth, etc. Used by
/// [`provider_is_configured`]: merely existing in the
/// (always-`Some`-once-any-provider-is-configured) `ProvidersConfig` struct
/// isn't enough, since untouched providers still resolve to a
/// `ProviderConfig::default()` there.
fn provider_config_is_explicit(entry: &ProviderConfig) -> bool {
    let non_empty = |value: Option<&String>| value.is_some_and(|value| !value.trim().is_empty());

    non_empty(entry.api_key.as_ref())
        || entry.vendor.is_some()
        || non_empty(entry.base_url.as_ref())
        || non_empty(entry.model.as_ref())
        || non_empty(entry.auth_mode.as_ref())
        || entry
            .auth
            .as_ref()
            .is_some_and(|auth| auth.validate().is_ok())
        || entry.context_window.is_some()
        || entry
            .model_context_windows
            .as_ref()
            .is_some_and(|table| !table.is_empty())
        || non_empty(entry.mode.as_ref())
        || entry.max_concurrency.is_some()
        || entry.http_headers.as_ref().is_some_and(|headers| {
            headers
                .iter()
                .any(|(name, value)| !name.trim().is_empty() && !value.trim().is_empty())
        })
        || non_empty(entry.path_suffix.as_ref())
        || non_empty(entry.reasoning_stream_style.as_ref())
        || entry.insecure_skip_tls_verify.is_some()
        || entry.allow_insecure_http.is_some()
        || non_empty(entry.kind.as_ref())
        || non_empty(entry.api_key_env.as_ref())
        || entry.external_credentials.is_some()
        || non_empty(entry.oauth_credential_generation.as_ref())
}

/// Save an API key to the appropriate place for the given provider.
/// DeepSeek goes through [`save_api_key`]. Other providers write
/// `[providers.<name>] api_key = "..."` to `~/.codewhale/config.toml`.
/// Returns the config file path.
#[cfg(test)]
pub fn save_api_key_for(provider: ProviderKind, api_key: &str) -> Result<PathBuf> {
    match save_api_key_for_identity(
        &ProviderIdentity {
            provider,
            key: provider.as_str().into(),
            exact_id: Some(provider.as_str().into()),
            migrated_legacy_ollama_cloud_route: false,
            legacy_root_custom_generation: None,
        },
        &Config {
            provider: Some(provider.as_str().to_string()),
            ..Config::default()
        },
        api_key,
    )? {
        SavedCredential::KeyringAndConfigFile { path, .. } | SavedCredential::ConfigFile(path) => {
            Ok(path)
        }
    }
}

/// Save an API key for the given provider identity and return where the
/// credential actually landed ([`SavedCredential`]) so callers can state the
/// true destination — the durable secret store plus credential-free config
/// metadata, or (tests only) the plaintext config file (#5195).
pub(crate) fn save_api_key_for_identity(
    identity: &ProviderIdentity,
    route_config: &Config,
    api_key: &str,
) -> Result<SavedCredential> {
    route_config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    if identity.provider == ProviderKind::Xai {
        return codewhale_config::with_xai_oauth_revocation_transaction(|| {
            save_api_key_for_identity_unlocked(identity, route_config, api_key)
        });
    }
    if identity.provider == ProviderKind::Anthropic {
        return codewhale_config::with_xai_oauth_lifecycle_lock(|_| {
            let saved = save_api_key_for_identity_unlocked(identity, route_config, api_key)?;
            codewhale_config::clear_all_claude_oauth_credentials_locked().context(
                "API-key billing was selected and saved, but old Claude sign-in cleanup failed",
            )?;
            Ok(saved)
        });
    }
    save_api_key_for_identity_unlocked(identity, route_config, api_key)
}

fn save_api_key_for_identity_unlocked(
    identity: &ProviderIdentity,
    route_config: &Config,
    api_key: &str,
) -> Result<SavedCredential> {
    route_config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    let provider = identity.provider;
    if provider == ProviderKind::OpenaiCodex {
        anyhow::bail!(codewhale_config::credentials::OPENAI_CODEX_API_KEY_REFUSAL);
    }
    let is_legacy_literal_custom = provider == ProviderKind::Custom
        && identity.key.as_str().trim() == ProviderKind::Custom.as_str()
        && identity.persisted_id().is_none();
    if matches!(provider, ProviderKind::Deepseek) {
        return save_api_key(api_key);
    }
    if is_legacy_literal_custom {
        return save_root_api_key_for_secret_slot(api_key, "custom", "custom");
    }

    let normalized = codewhale_secrets::normalize_api_key(api_key);
    let api_key = normalized.as_str();
    anyhow::ensure!(!api_key.is_empty(), "Refusing to save an empty API key.");

    let config_path =
        credential_config_path().context("Failed to resolve config path for provider API key.")?;
    ensure_parent_dir(&config_path)?;

    let key_inside = if provider == ProviderKind::Custom {
        let key = identity.key.as_str().trim();
        anyhow::ensure!(!key.is_empty(), "custom provider id cannot be empty");
        key
    } else {
        provider_config_key(identity).context("provider api key table")?
    };
    // A legacy, manually-selected Kimi CLI import implicitly routed Moonshot
    // traffic to Kimi Code. Once the user replaces that import with the
    // supported API-key route, persist the endpoint before changing auth_mode
    // so the key is not silently sent to the ordinary Moonshot endpoint.
    // Respect an explicit user-owned endpoint.
    let pin_kimi_code_base_url = provider == ProviderKind::Moonshot
        && route_config
            .provider_config_for(identity)
            .is_some_and(|entry| {
                provider_config_uses_kimi_imported_token(entry)
                    && entry
                        .base_url
                        .as_deref()
                        .is_none_or(|base_url| base_url.trim().is_empty())
            });

    if !route_config.should_skip_secret_store_for_provider(identity)
        && let Some(secrets) = credential_secret_store()
    {
        let secret_slot = provider_secret_store_slot(provider);
        // Snapshot -> write -> config-write -> rollback is a read-modify-write.
        // Hold this provider's credential write lock across the whole sequence
        // so a concurrent save or logout on the same slot cannot interleave and
        // leave the secret store and the config document disagreeing. This is
        // the `modify`-is-the-only-write-path rule ported from pi-mono; see
        // `crate::credentials::store`.
        return crate::credentials::store::with_provider_write_lock(secret_slot, || {
            let prior_secret = secrets.get(secret_slot);
            match prior_secret.as_ref() {
                Ok(prior) => match secrets.set(secret_slot, api_key) {
                    Ok(()) => {
                        let config_result = crate::config_persistence::mutate_config_document(
                            &config_path,
                            |doc| {
                                if pin_kimi_code_base_url {
                                    crate::config_persistence::set_document_value(
                                        doc,
                                        &["providers", key_inside, "base_url"],
                                        DEFAULT_KIMI_CODE_BASE_URL,
                                    )?;
                                }
                                crate::config_persistence::set_document_value(
                                    doc,
                                    &["providers", key_inside, "auth_mode"],
                                    "api_key",
                                )?;
                                crate::config_persistence::unset_document_value(
                                    doc,
                                    &["providers", key_inside, "external_credentials"],
                                )?;
                                if matches!(provider, ProviderKind::Xai | ProviderKind::Anthropic) {
                                    crate::config_persistence::unset_document_value(
                                        doc,
                                        &["providers", key_inside, "oauth_credential_generation"],
                                    )?;
                                }
                                crate::config_persistence::unset_document_value(
                                    doc,
                                    &["providers", key_inside, "api_key"],
                                )?;
                                Ok(())
                            },
                        )
                        .with_context(|| {
                            format!("Failed to write config to {}", config_path.display())
                        });
                        if let Err(error) = config_result {
                            let current = secrets.get(secret_slot).map_err(|rollback| {
                        anyhow::anyhow!(
                            "{error}; additionally could not verify secret-store rollback for {secret_slot}: {rollback}"
                        )
                    })?;
                            if current.as_deref() == Some(api_key) {
                                match prior {
                            Some(previous) => secrets.set(secret_slot, previous),
                            None => secrets.delete(secret_slot),
                        }
                        .map_err(|rollback| {
                            anyhow::anyhow!(
                                "{error}; additionally failed to restore prior secret-store state for {secret_slot}: {rollback}"
                            )
                        })?;
                            }
                            return Err(error);
                        }
                        codewhale_config::scrub_plaintext_api_keys_from_config_backup(
                            &config_path,
                        )?;
                        let backend = secrets.backend_name().to_string();
                        log_sensitive_event(
                            "credential.save",
                            json!({
                                "backend": backend.clone(),
                                "provider": identity.key,
                                "config_path": config_path.display().to_string(),
                                "plaintext_config_fallback": false,
                            }),
                        );
                        Ok(SavedCredential::KeyringAndConfigFile {
                            backend,
                            path: config_path,
                        })
                    }
                    Err(err) => Err(plaintext_credential_fallback_refused(
                        "write",
                        &config_path,
                        &err,
                    )),
                },
                Err(error) => Err(plaintext_credential_fallback_refused(
                    "snapshot",
                    &config_path,
                    &error,
                )),
            }
        });
    }

    // Edit the `[providers.<name>]` table in place so unrelated sections,
    // comments, and formatting survive the write.
    crate::config_persistence::mutate_config_document(&config_path, |doc| {
        if pin_kimi_code_base_url {
            crate::config_persistence::set_document_value(
                doc,
                &["providers", key_inside, "base_url"],
                DEFAULT_KIMI_CODE_BASE_URL,
            )?;
        }
        crate::config_persistence::set_document_value(
            doc,
            &["providers", key_inside, "auth_mode"],
            "api_key",
        )?;
        crate::config_persistence::unset_document_value(
            doc,
            &["providers", key_inside, "external_credentials"],
        )?;
        if matches!(provider, ProviderKind::Xai | ProviderKind::Anthropic) {
            crate::config_persistence::unset_document_value(
                doc,
                &["providers", key_inside, "oauth_credential_generation"],
            )?;
        }
        crate::config_persistence::set_document_value(
            doc,
            &["providers", key_inside, "api_key"],
            api_key,
        )
    })
    .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    log_sensitive_event(
        "credential.save",
        json!({
            "backend": "config_file",
            "provider": identity.key,
            "config_path": config_path.display().to_string(),
        }),
    );
    codewhale_config::scrub_plaintext_api_keys_from_config_backup(&config_path)?;

    Ok(SavedCredential::ConfigFile(config_path))
}

/// Persist a guided-setup model through the same canonical route writer used
/// by Runtime and the interactive model picker.
pub(crate) fn save_provider_model_for_identity(
    identity: &ProviderIdentity,
    route_config: &Config,
    model: &str,
) -> Result<PathBuf> {
    route_config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    let model = model.trim();
    anyhow::ensure!(!model.is_empty(), "model cannot be empty");
    let config_path =
        try_default_config_path().context("Failed to resolve config path for provider model.")?;
    crate::config_persistence::persist_provider_model_key(Some(&config_path), identity, model)
}

/// Persist a guided-setup endpoint choice into the provider's own
/// `[providers.<name>] base_url` (#4526).
///
/// Deliberately narrow: it never touches the root `base_url`, another
/// provider's table, or any other key, so a billing-route choice cannot
/// repoint an unrelated route.
pub(crate) fn save_provider_base_url_for_identity(
    identity: &ProviderIdentity,
    route_config: &Config,
    base_url: &str,
) -> Result<PathBuf> {
    route_config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    let base_url = base_url.trim();
    anyhow::ensure!(!base_url.is_empty(), "base URL cannot be empty");
    let config_path = try_default_config_path()
        .context("Failed to resolve config path for provider base URL.")?;
    ensure_parent_dir(&config_path)?;
    let key_inside = if identity.provider == ProviderKind::Custom {
        let key = identity.key.as_str().trim();
        anyhow::ensure!(!key.is_empty(), "custom provider id cannot be empty");
        key
    } else {
        provider_config_key(identity).context("provider base URL table")?
    };
    crate::config_persistence::mutate_config_document(&config_path, |doc| {
        crate::config_persistence::set_document_value(
            doc,
            &["providers", key_inside, "base_url"],
            base_url,
        )
    })
    .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    Ok(config_path)
}

/// Persist a guided-setup context-window choice without replacing the user's
/// surrounding TOML comments or formatting.
pub(crate) fn save_provider_context_window_for_identity(
    identity: &ProviderIdentity,
    route_config: &Config,
    context_window: u32,
) -> Result<PathBuf> {
    route_config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(context_window > 0, "context window must be greater than 0");
    let config_path = try_default_config_path()
        .context("Failed to resolve config path for provider context window.")?;
    ensure_parent_dir(&config_path)?;
    let key_inside = if identity.provider == ProviderKind::Custom {
        let key = identity.key.as_str().trim();
        anyhow::ensure!(!key.is_empty(), "custom provider id cannot be empty");
        key
    } else {
        provider_config_key(identity).context("provider context window table")?
    };
    crate::config_persistence::mutate_config_document(&config_path, |doc| {
        crate::config_persistence::set_document_value(
            doc,
            &["providers", key_inside, "context_window"],
            i64::from(context_window),
        )
    })
    .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    Ok(config_path)
}

/// Grant-time validation (#5772): read the exact file the user just confirmed,
/// through the same secure adapter the request path uses, and require it to
/// hold a usable credential.
///
/// This runs *after* the confirmation disclosure and *before* any consent
/// record is written, which is the whole ordering the consent model depends
/// on. Persisting first would leave a record claiming a credential exists for
/// a file that is missing, malformed, or expired — and every status surface
/// downstream would then have to trust it. Nothing here refreshes, rewrites,
/// or makes a network request, and no credential value escapes this function.
fn validate_external_credential_before_consent(
    consent_provider: codewhale_config::ProviderKind,
    source: codewhale_config::ExternalCredentialSource,
    path: &Path,
) -> Result<()> {
    let grant = codewhale_config::ExternalCredentialConsentToml::read_only(
        consent_provider,
        source,
        path.to_path_buf(),
    )
    .read_grant(consent_provider, source, path)?;
    match source {
        codewhale_config::ExternalCredentialSource::CodexCli => {
            crate::oauth::get_credentials(&grant).map(|_| ())
        }
        codewhale_config::ExternalCredentialSource::GrokCli => {
            crate::oauth::validate_grok_external_credentials(&grant)
        }
        codewhale_config::ExternalCredentialSource::DshCli => {
            crate::dsh_credentials::deepseek_api_key_from_grant(&grant)?
                .map(|_| ())
                .context("the DeepSeek Harness credentials file holds no DEEPSEEK_API_KEY")
        }
        // Retired: the reader is gone, so a legacy consent record validates
        // to nothing rather than resolving a route (PRD §4.4 PROD-002).
        codewhale_config::ExternalCredentialSource::AgyCli => {
            anyhow::bail!(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE)
        }
        codewhale_config::ExternalCredentialSource::KimiCodeCli => anyhow::bail!(
            "Kimi CLI credentials are never imported; configure a Kimi API key instead"
        ),
    }
}

/// Persist an explicitly confirmed read-only external credential grant and
/// update the live mirror only after the comment-preserving disk mutation
/// succeeds.
///
/// Order is load-bearing (#5772): the caller has already shown the
/// confirmation disclosure, this function then reads and validates the exact
/// consented file, and only a usable credential is allowed to produce a
/// persisted consent record.
pub(crate) fn persist_external_credential_consent_for_at(
    config_path: Option<&Path>,
    live_config: &mut Config,
    identity: &ProviderIdentity,
    consent_provider: codewhale_config::ProviderKind,
    source: codewhale_config::ExternalCredentialSource,
    path: &Path,
) -> Result<PathBuf> {
    live_config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    let provider = identity.provider;
    let expected = match provider {
        ProviderKind::OpenaiCodex => (
            codewhale_config::ProviderKind::OpenaiCodex,
            codewhale_config::ExternalCredentialSource::CodexCli,
        ),
        ProviderKind::Xai => (
            codewhale_config::ProviderKind::Xai,
            codewhale_config::ExternalCredentialSource::GrokCli,
        ),
        _ => anyhow::bail!(
            "{} has no supported external credential owner",
            provider.as_str()
        ),
    };
    anyhow::ensure!(
        (consent_provider, source) == expected,
        "external credential owner does not match provider {}",
        provider.as_str()
    );
    let path = codewhale_config::resolve_external_credential_path(path)?;
    let path_value = path.to_str().context(
        "external credential path cannot be persisted losslessly because it is not valid UTF-8",
    )?;
    validate_external_credential_before_consent(consent_provider, source, &path).with_context(
        || {
            format!(
                "no usable {} credential was found, so read-only consent was not saved",
                source.owner_label()
            )
        },
    )?;
    let config_path = match config_path {
        Some(path) => path.to_path_buf(),
        None => credential_config_path()
            .context("Failed to resolve config path for external credential consent.")?,
    };
    ensure_parent_dir(&config_path)?;
    let key_inside = provider_config_key(identity).context("external credential provider key")?;
    crate::config_persistence::mutate_config_document(&config_path, |doc| {
        crate::config_persistence::set_document_value(
            doc,
            &["providers", key_inside, "auth_mode"],
            "oauth",
        )?;
        let prefix = &["providers", key_inside, "external_credentials"];
        crate::config_persistence::set_document_value(
            doc,
            &[prefix[0], prefix[1], prefix[2], "access"],
            "read_only",
        )?;
        crate::config_persistence::set_document_value(
            doc,
            &[prefix[0], prefix[1], prefix[2], "provider"],
            consent_provider.as_str(),
        )?;
        crate::config_persistence::set_document_value(
            doc,
            &[prefix[0], prefix[1], prefix[2], "source"],
            source.as_str(),
        )?;
        crate::config_persistence::set_document_value(
            doc,
            &[prefix[0], prefix[1], prefix[2], "path"],
            path_value,
        )?;
        crate::config_persistence::set_document_value(
            doc,
            &[prefix[0], prefix[1], prefix[2], "consent_version"],
            i64::from(codewhale_config::EXTERNAL_CREDENTIAL_CONSENT_VERSION),
        )
    })
    .with_context(|| {
        format!(
            "Failed to write config to {}",
            codewhale_config::quote_os_path(&config_path)
        )
    })?;
    live_config
        .providers
        .get_or_insert_with(ProvidersConfig::default);
    let entry = live_config.provider_config_for_mut(identity)?;
    entry.auth_mode = Some("oauth".to_string());
    entry.external_credentials = Some(codewhale_config::ExternalCredentialConsentToml::read_only(
        consent_provider,
        source,
        path,
    ));
    Ok(config_path)
}

/// Revoke one provider's external-file access without inspecting that file.
pub(crate) fn revoke_external_credential_consent_for_at(
    config_path: Option<&Path>,
    live_config: &mut Config,
    identity: &ProviderIdentity,
) -> Result<PathBuf> {
    live_config
        .verify_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    let provider = identity.provider;
    anyhow::ensure!(
        matches!(provider, ProviderKind::OpenaiCodex | ProviderKind::Xai),
        "{} has no supported external credential owner",
        provider.as_str()
    );
    let config_path = match config_path {
        Some(path) => path.to_path_buf(),
        None => credential_config_path()
            .context("Failed to resolve config path for external credential consent.")?,
    };
    ensure_parent_dir(&config_path)?;
    let key_inside = provider_config_key(identity).context("external credential provider key")?;
    crate::config_persistence::mutate_config_document(&config_path, |doc| {
        crate::config_persistence::unset_document_value(
            doc,
            &["providers", key_inside, "external_credentials"],
        )?;
        Ok(())
    })
    .with_context(|| {
        format!(
            "Failed to write config to {}",
            codewhale_config::quote_os_path(&config_path)
        )
    })?;
    live_config
        .provider_config_for_mut(identity)?
        .external_credentials = None;
    Ok(config_path)
}

pub(crate) fn provider_config_key(identity: &ProviderIdentity) -> Result<&str> {
    if identity.provider == ProviderKind::Deepseek
        && identity.key.as_str() == ProviderKind::Deepseek.as_str()
    {
        anyhow::bail!("DeepSeek stores auth at the root config level");
    }
    identity.config_table_key()
}

fn provider_config_table_name(identity: &ProviderIdentity) -> Result<String> {
    Ok(format!("providers.{}", provider_config_key(identity)?))
}

/// Name of the ambient provider variable that currently holds a non-empty key
/// for `provider`. Presence only: the value is dropped here.
pub(crate) fn provider_env_api_key_var(provider: ProviderKind) -> Option<&'static str> {
    provider_env_api_key_named(provider).map(|(name, _)| name)
}

/// The provider's ambient env key and the variable that supplied it,
/// normalized with [`codewhale_secrets::normalize_api_key`] (#6528).
fn provider_env_api_key_named(provider: ProviderKind) -> Option<(&'static str, String)> {
    let names = provider.provider().env_vars();
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| codewhale_secrets::normalize_api_key(&value))
            .filter(|value| !value.is_empty())
            .map(|value| (*name, value))
    })
}

/// Canonical durable-credential slot shared with the CLI dispatcher.
fn provider_secret_store_slot(provider: ProviderKind) -> &'static str {
    provider.secret_store_slot()
}

/// Whether the secret-store save marker (`auth_mode = "api_key"` with no
/// config literal, written by the save path) exists for `provider` or for any
/// provider sharing its durable credential slot.
///
/// One Model Studio account authenticates all four plan/dialect variants, so
/// saving a key on `modelstudio-token-plan` marks only that variant's config
/// table; the sibling variants must still treat the family slot as saved.
fn secret_slot_save_marker_on_shared_slot(config: &Config, identity: &ProviderIdentity) -> bool {
    let slot = provider_secret_store_slot(identity.provider);
    codewhale_config::descriptors::provider_compatibility()
        .iter()
        .filter(|row| provider_secret_store_slot(row.kind) == slot)
        .filter_map(|row| {
            config
                .resolve_persisted_provider_identity(Some(row.id), Some(row.id))
                .ok()
        })
        .any(|candidate| {
            config
                .provider_config_for(&candidate)
                .is_some_and(|entry| auth_mode_requires_api_key(entry.auth_mode.as_deref()))
        })
}

/// Read only the durable secret-store layer (no environment fallback).
///
/// This keeps `config -> secret store -> env` precedence explicit in the TUI
/// and lets status surfaces distinguish a saved key from an ambient export.
pub(crate) fn provider_secret_store_api_key(
    config: &Config,
    identity: &ProviderIdentity,
) -> Option<String> {
    provider_secret_store_api_key_with_mode(config, identity, false)
}

fn provider_secret_store_api_key_with_mode(
    config: &Config,
    identity: &ProviderIdentity,
    read_only: bool,
) -> Option<String> {
    let provider = identity.provider;
    // Keep the named-custom exclusion at the credential boundary itself.
    // Callers also use this policy to avoid unnecessary keyring probes, but a
    // future caller must not be able to read the legacy `custom` slot for an
    // arbitrary `[providers.<name>]` endpoint by omitting that outer guard.
    if config.should_skip_secret_store_for_provider(identity) {
        return None;
    }

    // Unit tests must never inspect the developer's real credential store.
    // Secret-store regressions opt in with an isolated CODEWHALE_HOME and an
    // explicit backend, matching the secrets crate's own test discipline.
    #[cfg(test)]
    if !codewhale_paths::codewhale_home_is_explicit()
        || std::env::var_os("CODEWHALE_SECRET_BACKEND").is_none()
    {
        return None;
    }

    let secrets = if read_only {
        codewhale_secrets::Secrets::auto_detect_read_only()
    } else {
        codewhale_secrets::Secrets::auto_detect()
    };
    // Read through the credential-store trait so every read of a durable slot
    // goes through one adapter (`crate::credentials::store`), and the value is
    // carried as a type-tagged `Credential` rather than a bare String that can
    // drift into a log line.
    let store =
        crate::credentials::store::SecretStoreCredentials::new(secrets, known_secret_store_slots());
    let primary = store
        .read(provider_secret_store_slot(provider))
        .ok()
        .flatten()
        .map(|credential| credential.expose_secret().to_string());
    if primary.is_some() {
        return primary;
    }

    // The old local identity owned the hosted slot only when the live config
    // selected the exact Ollama Cloud route. Never apply this fallback to a
    // neighboring/custom endpoint or to an explicit new `ollama-cloud`
    // selection, and never write/copy/delete either slot while resolving.
    (provider == ProviderKind::OllamaCloud && identity.migrated_legacy_ollama_cloud_route)
        .then(|| {
            store
                .read(ProviderKind::Ollama.as_str())
                .ok()
                .flatten()
                .map(|credential| credential.expose_secret().to_string())
        })
        .flatten()
}

/// Every durable credential slot CodeWhale knows how to write.
///
/// The backing keyring exposes no key enumeration, so
/// [`crate::credentials::store::SecretStoreCredentials::list`] is given the
/// slot names to probe. Deduplicated because shared-account families collapse
/// several providers onto one slot.
fn known_secret_store_slots() -> Vec<String> {
    let mut slots: Vec<String> = codewhale_config::descriptors::provider_compatibility()
        .iter()
        .map(|row| provider_secret_store_slot(row.kind).to_string())
        .collect();
    slots.sort();
    slots.dedup();
    slots
}

/// The shadowing warning for a config-file `api_key` that wins over a live
/// secret-store credential, if both exist (#5194).
///
/// The config file intentionally outranks the secret store in the read
/// chain, but a shadowed slot is invisible: the user rotates the key with
/// `codewhale auth set` and nothing changes, because the stale plaintext
/// copy still wins. Mirror the fleet-roster shadowing rule (#5098):
/// precedence is normal, but it must be VISIBLE. The message names both
/// sources, which one won, and the command that resolves the shadow.
/// Split from [`warn_on_config_api_key_shadowing`] so the decision is
/// testable without capturing tracing output.
fn config_api_key_shadow_warning(
    config: &Config,
    identity: &ProviderIdentity,
    config_source: &str,
) -> Option<String> {
    let provider = identity.provider;
    if config.should_skip_secret_store_for_provider(identity) {
        return None;
    }
    provider_secret_store_api_key_with_mode(config, identity, true).map(|_| {
        let slot = provider_secret_store_slot(provider);
        let id = provider.as_str();
        format!(
            "both {config_source} in the config file and secret-store slot \"{slot}\" \
             hold a credential for provider {id}; the config-file key won. Run \
             `codewhale auth set --provider {id}` to move the key into the secret store \
             and strip the plaintext copy, or remove the config-file api_key."
        )
    })
}

/// Emit the #5194 shadowing warning at most once per provider slot per
/// process: credential resolution runs on every request, and a repeating
/// warning is noise, not signal.
fn warn_on_config_api_key_shadowing(
    config: &Config,
    identity: &ProviderIdentity,
    config_source: &str,
) {
    let provider = identity.provider;
    let Some(message) = config_api_key_shadow_warning(config, identity, config_source) else {
        return;
    };
    static WARNED_SLOTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashSet<&'static str>>,
    > = std::sync::OnceLock::new();
    let mut warned = WARNED_SLOTS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !warned.insert(provider_secret_store_slot(provider)) {
        return;
    }
    drop(warned);
    tracing::warn!("{message}");
}

/// The model this launch was explicitly asked for, if any.
///
/// The `codewhale` dispatcher forwards `--model` to this binary as
/// `CODEWHALE_MODEL` (with the legacy `DEEPSEEK_MODEL` alias), so an explicit
/// flag and an explicit shell export are the same signal here: *the user named
/// a model for this run*. That has to outrank the remembered per-provider
/// selection in `settings.toml`, which is a convenience memory of the last
/// `/model` pick — never a reason to run something the user did not ask for
/// (v0.9.1 kimi-k3 dogfood report).
pub(crate) fn explicit_launch_model_override() -> Option<String> {
    codewhale_env_var("CODEWHALE_MODEL", "DEEPSEEK_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The provider this launch was explicitly asked for, if any.
///
/// An environment/CLI override is a one-run instruction and must outrank the
/// user's saved startup default. A provider merely named in config.toml is a
/// seed instead: the user can deliberately replace that seed from `/model`.
pub(crate) fn explicit_launch_provider_override() -> Option<String> {
    codewhale_env_var("CODEWHALE_PROVIDER", "DEEPSEEK_PROVIDER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn explicit_cli_api_key_override() -> Option<String> {
    (cli_api_key_source().as_deref() == Some("cli"))
        .then(|| {
            std::env::var(codewhale_config::CLI_API_KEY_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .flatten()
}

pub(crate) fn cli_api_key_source() -> Option<String> {
    codewhale_env_var(
        codewhale_config::CLI_API_KEY_SOURCE_ENV,
        codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV,
    )
    .ok()
}

fn missing_provider_api_key_message(identity: &ProviderIdentity) -> Result<String> {
    let provider = identity.provider;
    let credential_hint = provider
        .provider()
        .credential_help()
        .credential_url
        .map(|url| format!(" Get a key: {url}."))
        .unwrap_or_default();
    let label = identity
        .compatibility()
        .map_or_else(|| provider.provider().display_name(), |row| row.label);
    Ok(format!(
        "{} API key not found.{} Run 'codewhale auth set --provider {}', set {}, or add [{}] api_key in ~/.codewhale/config.toml.",
        label,
        credential_hint,
        identity.key,
        provider.provider().env_vars().join(" / "),
        provider_config_table_name(identity)?
    ))
}

/// Clear every saved API key from config-file storage AND the durable
/// secret store.
///
/// The full-wipe logout path (`codewhale-tui --logout`, `auth logout`)
/// calls this to remove credentials so the next request can't
/// silently use a stale config key (#343). The function removes the legacy
/// root `api_key` entry *and* every `api_key` entry nested in a
/// `[providers.<name>]` table, leaving keys like `api_key_env`, comments,
/// and formatting untouched, then deletes every provider's secret-store
/// slot — symmetric with CLI logout (#5159) — so a stored credential cannot
/// survive logout and reappear through the read chain (#5196). The TUI
/// `/logout` command stays single-provider and goes through
/// [`clear_active_provider_api_key`] instead.
///
/// Environment variables (`DEEPSEEK_API_KEY`, etc.) are intentionally
/// **not** unset — they are managed by the user's shell and outside the
/// CLI's purview. `Config::active_route_api_key`'s explicit-override path
/// (Path 0) ensures a freshly-entered key still wins over a stale env
/// var that lingers from a previous session.
pub fn clear_api_key() -> Result<()> {
    codewhale_config::with_xai_oauth_revocation_transaction(clear_api_key_unlocked)
}

fn clear_api_key_unlocked() -> Result<()> {
    // Same read-modify-write as the saves: hold every durable slot's write
    // lock across the config-document mutation and the store deletes so a
    // concurrent save cannot interleave and leave the two disagreeing.
    crate::credentials::store::with_provider_write_locks(
        known_secret_store_slots(),
        clear_api_key_under_slot_locks,
    )
}

fn clear_api_key_under_slot_locks() -> Result<()> {
    // Strip api_key entries from config.toml, including provider-scoped
    // nested entries. Clearing a config file must not trigger platform
    // credential prompts. Clears target the same user-global document that
    // credential saves write, so logout removes what login stored (#5045).
    let config_path = credential_config_path()
        .context("Failed to resolve config path while clearing API keys.")?;

    if config_path.exists() {
        crate::config_persistence::mutate_config_document(&config_path, |doc| {
            crate::config_persistence::remove_document_key_recursive(doc.as_table_mut(), "api_key");
            crate::config_persistence::unset_document_value(
                doc,
                &["providers", "xai", "oauth_credential_generation"],
            )?;
            crate::config_persistence::unset_document_value(
                doc,
                &["providers", "xai", "auth_mode"],
            )?;
            crate::config_persistence::unset_document_value(
                doc,
                &["providers", "xai", "external_credentials"],
            )?;
            Ok(())
        })
        .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
        log_sensitive_event(
            "credential.clear",
            json!({
                "backend": "config_file",
                "config_path": config_path.display().to_string(),
                "scope": "root_and_provider_keys",
            }),
        );
    }

    // The config scrub alone leaves the durable secret-store credential
    // alive, and the read chain prefers the secret store over the file, so a
    // "cleared" key silently came back on the next launch (#5196). Delete
    // every provider slot too, symmetric with CLI logout (#5159). This runs
    // even when the config file is absent: the slot survives independently
    // of the file.
    if let Some(secrets) = credential_secret_store() {
        let failures = clear_all_provider_api_keys_from_secret_store(secrets);
        if !failures.is_empty() {
            anyhow::bail!(
                "failed to delete stored credentials for: {}",
                failures.join(", ")
            );
        }
    }

    Ok(())
}

/// Delete the credential slot of every provider that has one stored.
///
/// Mirrors the CLI logout helper (#5159): each slot is probed first so
/// backends that error on deleting a missing item stay quiet, slots shared
/// by several providers (e.g. the historical `siliconflow` slot) are deleted
/// once, and every deletion failure is returned as a human-readable entry so
/// the caller can fail loudly instead of claiming a clean logout while
/// credentials linger in the store (#5196).
fn clear_all_provider_api_keys_from_secret_store(
    secrets: codewhale_secrets::Secrets,
) -> Vec<String> {
    let mut failures = Vec::new();
    let store = crate::credentials::store::SecretStoreCredentials::new(
        secrets.clone(),
        known_secret_store_slots(),
    );
    // `list` enumerates the slots that actually hold something, without
    // exposing any value — the deduplication that used to live here is now the
    // slot table's job.
    let stored: Vec<crate::credentials::CredentialInfo> = match store.list() {
        Ok(stored) => stored,
        Err(error) => {
            failures.push(format!("secret store enumeration: {error}"));
            return failures;
        }
    };
    for entry in stored {
        // The caller already holds this slot's write lock for the whole
        // logout. Delete through the backend rather than `store.delete`,
        // which would re-acquire the same non-reentrant mutex and deadlock.
        if let Err(error) = secrets.delete(&entry.provider_id) {
            failures.push(format!("{}: {error}", entry.provider_id));
        }
    }
    failures
}

/// Clear only the active provider's API key from the config file and delete
/// that provider's durable secret-store slot (#5196).
/// Unlike `clear_api_key()` which strips ALL api_key entries, this
/// removes only the key for the specified provider section (plus a leftover
/// legacy top-level `api_key` for DeepSeek or the literal custom route).
pub fn clear_active_provider_api_key(provider: &str) -> Result<()> {
    if provider == ProviderKind::Xai.as_str() {
        return codewhale_config::with_xai_oauth_revocation_transaction(|| {
            clear_active_provider_api_key_unlocked(provider)
        });
    }
    clear_active_provider_api_key_unlocked(provider)
}

fn clear_active_provider_api_key_unlocked(provider: &str) -> Result<()> {
    let slot = compatibility_for_id(provider).map(|row| provider_secret_store_slot(row.kind));
    match slot {
        Some(slot) => crate::credentials::store::with_provider_write_lock(slot, || {
            clear_active_provider_api_key_under_lock(provider)
        }),
        None => clear_active_provider_api_key_under_lock(provider),
    }
}

fn clear_active_provider_api_key_under_lock(provider: &str) -> Result<()> {
    let config_path = credential_config_path()
        .context("Failed to resolve config path while clearing API keys.")?;

    if config_path.exists() {
        crate::config_persistence::mutate_config_document_with_migration(
            &config_path,
            |doc, moved| {
                // The write itself moved any older top-level `api_key` into its
                // provider table (#6394); clear a conflicting leftover too.
                let deepseek_family = provider == ProviderKind::Deepseek.as_str()
                    || provider == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id;
                if deepseek_family || provider == ProviderKind::Custom.as_str() {
                    crate::config_persistence::unset_document_value(doc, &["api_key"])?;
                }
                let table = compatibility_for_id(provider).map_or(provider, |row| row.config_key);
                let has_own_key = |doc: &toml_edit::DocumentMut| {
                    [table, provider].iter().any(|key| {
                        doc.get("providers")
                            .and_then(|providers| providers.get(key))
                            .and_then(|entry| entry.get("api_key"))
                            .and_then(toml_edit::Item::as_str)
                            .is_some_and(|value| !value.trim().is_empty())
                    })
                };
                // DeepSeek-CN reads `[providers.deepseek] api_key` only when it has
                // no key of its own, and older releases kept both behind one
                // top-level key. Signing CN out clears the DeepSeek key only when
                // it is that shared key: CN was reading it, or this write just
                // moved the top-level key there. A DeepSeek key the user saved
                // for DeepSeek itself stays.
                let clears_shared_deepseek_key = provider
                    == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id
                    && (!has_own_key(doc) || moved.moved_root_api_key_to("deepseek"));
                crate::config_persistence::unset_document_value(
                    doc,
                    &["providers", table, "api_key"],
                )?;
                if table != provider {
                    // Older writers used the provider id as the table key.
                    crate::config_persistence::unset_document_value(
                        doc,
                        &["providers", provider, "api_key"],
                    )?;
                }
                if clears_shared_deepseek_key {
                    crate::config_persistence::unset_document_value(
                        doc,
                        &["providers", "deepseek", "api_key"],
                    )?;
                }
                if provider == ProviderKind::Xai.as_str() {
                    crate::config_persistence::unset_document_value(
                        doc,
                        &["providers", "xai", "oauth_credential_generation"],
                    )?;
                    crate::config_persistence::unset_document_value(
                        doc,
                        &["providers", "xai", "auth_mode"],
                    )?;
                    crate::config_persistence::unset_document_value(
                        doc,
                        &["providers", "xai", "external_credentials"],
                    )?;
                }
                Ok(())
            },
        )
        .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
        log_sensitive_event(
            "credential.clear",
            json!({
                "backend": "config_file",
                "config_path": config_path.display().to_string(),
                "scope": provider,
            }),
        );
    }

    // The durable secret-store slot survives a config-file scrub and the
    // read chain prefers it, so the cleared key would silently come back
    // (#5196). Delete the provider's slot too — even when the config file
    // itself is absent. Exact named custom providers have no secret-store
    // slot, so an unmatched provider string skips this step.
    if let Some(secrets) = credential_secret_store()
        && let Some(slot) = ProviderKind::all()
            .iter()
            .find(|candidate| candidate.as_str() == provider)
            .map(|candidate| provider_secret_store_slot(*candidate))
    {
        let has_value = secrets
            .get(slot)
            .ok()
            .flatten()
            .is_some_and(|value| !value.trim().is_empty());
        if has_value {
            secrets
                .delete(slot)
                .with_context(|| format!("failed to delete stored credential for {slot}"))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;

/// #5045 regression coverage: credential writes must never land in a
/// workspace-scoped `.codewhale/config.toml`.
#[cfg(test)]
mod credential_scope_tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};

    /// With the ambient config path pointing at a workspace-local
    /// `.codewhale/config.toml` (a checkout the user works in), saving an
    /// API key must write the user-global config under the isolated
    /// `CODEWHALE_HOME`, never the project file. The `.git` marker stands in
    /// for cwd-inside-the-workspace: chdir is process-global and unsafe in a
    /// parallel test binary, and production classifies on either signal.
    #[test]
    fn api_key_save_rescopes_workspace_config_to_user_global() -> Result<()> {
        let _lock = lock_test_env();
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("repo");
        fs::create_dir_all(workspace.join(".git"))?;
        let project_dir = workspace.join(".codewhale");
        fs::create_dir_all(&project_dir)?;
        let project_config = project_dir.join("config.toml");
        fs::write(&project_config, "approval_policy = \"never\"\n")?;

        let user_home = temp.path().join("user-global-home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", user_home.as_os_str());
        let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", project_config.as_os_str());
        let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        // No explicit secret backend: under cfg(test) the save takes the
        // plaintext config-file path, which is exactly the surface this
        // regression guards.
        let _backend = EnvVarGuard::remove("CODEWHALE_SECRET_BACKEND");
        let _legacy_backend = EnvVarGuard::remove("DEEPSEEK_SECRET_BACKEND");

        let saved = save_api_key("workspace-rescope-test-key")?;

        let global_config = user_home.join("config.toml");
        // Compare canonicalized paths: the resolved config path runs through
        // `normalize_config_file_path`, which canonicalizes the parent, so on
        // macOS the lexical `/var/folders/…` tempdir and its canonical
        // `/private/var/folders/…` form are the same file. A lexical compare
        // both false-fails and false-passes on that symlink.
        let saved_path = match saved {
            SavedCredential::ConfigFile(path) => path,
            other => panic!("expected a config-file save, got {}", other.describe()),
        };
        assert_eq!(
            canonicalize_or_keep(&saved_path),
            canonicalize_or_keep(&global_config),
            "credential save must surface the user-global destination"
        );
        let global = fs::read_to_string(&global_config)?;
        assert!(
            global.contains("workspace-rescope-test-key"),
            "user-global config must hold the saved key: {global}"
        );
        let project = fs::read_to_string(&project_config)?;
        assert!(
            !project.contains("workspace-rescope-test-key"),
            "credential leaked into workspace config: {project}"
        );
        assert!(
            !project.contains("api_key"),
            "workspace config must stay credential-free: {project}"
        );
        Ok(())
    }

    /// Provider-table saves go through the same resolver: an OpenRouter key
    /// saved with a workspace-scoped ambient config path must land in the
    /// user-global document.
    #[test]
    fn provider_api_key_save_rescopes_workspace_config_to_user_global() -> Result<()> {
        let _lock = lock_test_env();
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("repo");
        fs::create_dir_all(workspace.join(".git"))?;
        let project_dir = workspace.join(".codewhale");
        fs::create_dir_all(&project_dir)?;
        let project_config = project_dir.join("config.toml");
        fs::write(&project_config, "approval_policy = \"never\"\n")?;

        let user_home = temp.path().join("user-global-home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", user_home.as_os_str());
        let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", project_config.as_os_str());
        let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let _backend = EnvVarGuard::remove("CODEWHALE_SECRET_BACKEND");
        let _legacy_backend = EnvVarGuard::remove("DEEPSEEK_SECRET_BACKEND");

        let path = save_api_key_for(ProviderKind::Openrouter, "workspace-rescope-openrouter-key")?;

        // Canonicalized comparison: see the root-key test above.
        assert_eq!(
            canonicalize_or_keep(&path),
            canonicalize_or_keep(&user_home.join("config.toml")),
            "provider save must report the user-global destination"
        );
        let global = fs::read_to_string(&path)?;
        assert!(global.contains("workspace-rescope-openrouter-key"));
        let project = fs::read_to_string(&project_config)?;
        assert!(
            !project.contains("workspace-rescope-openrouter-key"),
            "credential leaked into workspace config: {project}"
        );
        Ok(())
    }
}
