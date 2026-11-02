//! Model selection and auto-routing.
//!
//! The CLI, TUI, runtime threads, subagents, and command handlers all need
//! this behavior, so it intentionally lives outside the command tree.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::client::CodewhaleClient;
use crate::client::system_one::{DecisionRouterRoute, SystemOneAnswer, SystemOneResponse};
use crate::cost_status::{
    EffectiveRouteEnvelope, EffectiveRouteUsage, RuntimeUsageDropRecord, RuntimeUsageRecord,
};
use crate::llm_client::LlmClient;
use crate::model_inventory::{ModelInventory, probability_bp};
use crate::reasoning_preference::ReasoningEffort;
use codewhale_config::ProviderKind;
use codewhale_models::Role;
use codewhale_models::auto_route_evidence::AutoRouteDecisionEvidence;
use codewhale_models::{ContentBlock, Message, MessageRequest, MessageResponse, SystemPrompt};
use codewhale_runtime::config::{
    AutoRouterKind, Config, ProviderIdentity, normalize_model_name_for_provider,
};

/// Big/cheap model pair the auto-router may choose between for the active
/// provider (#3018).
///
/// `cheap == None` means the provider has no known cheap tier: the local
/// fallback stays on the current model (only thinking effort varies) and the
/// network router is skipped entirely (#1549).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterCandidates {
    pub big: String,
    pub cheap: Option<String>,
}

impl RouterCandidates {
    pub fn deepseek() -> Self {
        Self {
            big: "deepseek-v4-pro".to_string(),
            cheap: Some("deepseek-flash".to_string()),
        }
    }
}

/// Return a provider-owned strong/fast pair for model families whose catalog
/// exposes more than one tier.  The ids here are deliberately explicit: a
/// model name alone is not evidence that another provider can serve its
/// sibling, so unknown providers and unknown families remain single-tier.
fn catalog_family_candidates(
    provider: ProviderKind,
    current_model: &str,
) -> Option<RouterCandidates> {
    let normalized = normalize_model_name_for_provider(provider, current_model)
        .unwrap_or_else(|| current_model.trim().to_string());
    let lower = normalized.to_ascii_lowercase();

    let cheap = match provider {
        ProviderKind::Openai | ProviderKind::OpenaiCodex
            if matches!(lower.as_str(), "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra") =>
        {
            Some("gpt-5.6-luna".to_string())
        }
        ProviderKind::Anthropic
            if matches!(
                lower.as_str(),
                "claude-opus-4-8"
                    | "claude-sonnet-4-6"
                    | "claude-sonnet-5"
                    | "claude-opus-5-5"
                    | "claude-sonnet-5-5"
            ) =>
        {
            Some("claude-haiku-4-5".to_string())
        }
        ProviderKind::XiaomiMimo if lower == "mimo-v2.6-pro" => Some("mimo-v2.6-flash".to_string()),
        ProviderKind::Arcee
            if matches!(
                lower.as_str(),
                "trinity-large-thinking" | "trinity-large-preview"
            ) =>
        {
            Some("trinity-mini".to_string())
        }
        ProviderKind::Moonshot if lower == "kimi-k2.7-code" => Some("kimi-k2.6".to_string()),
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic if lower == "minimax-m2.7" => {
            Some("MiniMax-M2.7-highspeed".to_string())
        }
        ProviderKind::OpencodeGo if lower == "kimi-k3" => Some("kimi-k2.7-code".to_string()),
        ProviderKind::Openrouter
            if lower == "qwen/qwen3.6-max-preview"
                || lower == "qwen/qwen3.6-plus"
                || lower == "qwen/qwen3.6-27b"
                || lower == "qwen/qwen3.6-35b-a3b" =>
        {
            Some("qwen/qwen3.6-flash".to_string())
        }
        ProviderKind::Openrouter if lower == "xiaomi/mimo-v2.5-pro" => {
            Some("xiaomi/mimo-v2.5".to_string())
        }
        ProviderKind::Openrouter
            if matches!(
                lower.as_str(),
                "arcee-ai/trinity-large-thinking" | "arcee-ai/trinity-large-preview"
            ) =>
        {
            Some("arcee-ai/trinity-mini".to_string())
        }
        ProviderKind::Openrouter if lower == "moonshotai/kimi-k2.7-code" => {
            Some("moonshotai/kimi-k2.6".to_string())
        }
        ProviderKind::Openrouter
            if lower == "anthropic/claude-opus-4-8"
                || lower == "anthropic/claude-sonnet-4-6"
                || lower == "anthropic/claude-sonnet-5" =>
        {
            Some("anthropic/claude-haiku-4-5".to_string())
        }
        _ => None,
    }?;

    Some(RouterCandidates {
        big: normalized,
        cheap: Some(cheap),
    })
}

/// Derive the auto-router's candidate pair for the active provider (#3018).
///
/// DeepSeek providers route between the canonical pro/flash pair. Hosted
/// routes with known wire ids for that pair (NVIDIA NIM, OpenRouter, Novita,
/// SiliconFlow, SGLang, vLLM, Wanjie Ark, Volcengine) use their provider
/// spellings. Every other provider has no known cheap tier: `big` is the
/// session model and `cheap` is `None`, so auto mode never fabricates a
/// DeepSeek id for a provider that cannot serve it.
pub fn provider_router_candidates(
    provider: codewhale_config::ProviderKind,
    current_model: &str,
) -> RouterCandidates {
    use codewhale_config::ProviderKind;
    if let Some(candidates) = catalog_family_candidates(provider, current_model) {
        return candidates;
    }

    if provider == ProviderKind::Zai {
        let normalized =
            codewhale_runtime::config::normalize_model_name_for_provider(provider, current_model)
                .unwrap_or_else(|| current_model.to_string());
        return RouterCandidates {
            // GLM-5.3 routes faster/explore children to GLM-5.3-Flash.
            // GLM-5.2 still uses GLM-5-Turbo. Flash, Turbo, and 5.1 have no
            // cheaper tier and keep children on the parent model.
            cheap: if normalized
                == codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_MODEL
            {
                Some(
                    codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_FLASH_MODEL
                        .to_string(),
                )
            } else if normalized
                == codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
            {
                Some(
                    codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL
                        .to_string(),
                )
            } else {
                None
            },
            big: normalized,
        };
    }

    if provider == ProviderKind::Openrouter
        && let Some(normalized) =
            codewhale_runtime::config::normalize_model_name_for_provider(provider, current_model)
        && matches!(
            normalized.as_str(),
            codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_1_MODEL
                | codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_2_MODEL
                | codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_3_MODEL
                | codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_3_FLASH_MODEL
                | codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_TURBO_MODEL
        )
    {
        return RouterCandidates {
            // z-ai/glm-5.3 routes faster children to z-ai/glm-5.3-flash;
            // z-ai/glm-5.2 still uses z-ai/glm-5-turbo. Flash, turbo, and 5.1
            // have no cheaper tier and keep children on parent.
            cheap: if normalized
                == codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_3_MODEL
            {
                Some(
                    codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_3_FLASH_MODEL
                        .to_string(),
                )
            } else if normalized
                == codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_2_MODEL
            {
                Some(
                    codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_TURBO_MODEL
                        .to_string(),
                )
            } else {
                None
            },
            big: normalized,
        };
    }

    match provider {
        ProviderKind::Deepseek => RouterCandidates::deepseek(),
        ProviderKind::NvidiaNim
        | ProviderKind::Openrouter
        | ProviderKind::Novita
        | ProviderKind::Siliconflow
        | ProviderKind::SiliconflowCN
        | ProviderKind::Sglang
        | ProviderKind::Vllm
        | ProviderKind::WanjieArk
            if current_model.to_ascii_lowercase().contains("deepseek") =>
        {
            RouterCandidates {
                big: codewhale_runtime::config::wire_model_for_provider(
                    provider,
                    "deepseek-v4-pro",
                ),
                cheap: Some(codewhale_runtime::config::wire_model_for_provider(
                    provider,
                    "deepseek-v4-flash",
                )),
            }
        }
        ProviderKind::Volcengine if current_model.to_ascii_lowercase().contains("deepseek") => {
            RouterCandidates {
                big: codewhale_config::descriptors::defaults::DEFAULT_VOLCENGINE_MODEL.to_string(),
                cheap: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_VOLCENGINE_FLASH_MODEL
                        .to_string(),
                ),
            }
        }
        _ => RouterCandidates {
            big: current_model.to_string(),
            cheap: None,
        },
    }
}

/// The loud half of `RouterCandidates::cheap == None`.
///
/// `None` is a legitimate answer for a pair this router knows to be
/// single-tier, and a misconfiguration for a pair it has never heard of; both
/// land the child on the parent model at the parent's price. A caller that was
/// asked for a fast lane attaches this where the route is shown, so
/// `Faster`/`Auto` never resolves to full price without saying so.
#[must_use]
pub fn missing_fast_sibling_note(provider: ProviderKind, model: &str) -> String {
    // The model id is bounded so an absurd route cannot crowd the receipt it
    // travels on (`ChildRouteReceipt::fallback_note`, 1 KiB gate).
    let model: String = model.trim().chars().take(64).collect();
    format!(
        "no cheap sibling for {}/{}; the child runs on the parent model at parent price — pin a child model to choose",
        provider.as_str(),
        model
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoRouteSource {
    FlashRouter,
    Heuristic,
}

impl AutoRouteSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            AutoRouteSource::FlashRouter => "classifier",
            AutoRouteSource::Heuristic => "heuristic",
        }
    }
}

/// Provider-safe tier reported for the concrete Auto route.
///
/// `Selected` is deliberately neutral: a classifier may choose a runnable
/// inventory model that is not part of a known strong/fast pair, and the UI
/// must not invent a tier from the model id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoRouteTier {
    Strong,
    Fast,
    Only,
    Selected,
}

impl AutoRouteTier {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Fast => "fast",
            Self::Only => "only model",
            Self::Selected => "selected",
        }
    }
}

/// Scope from which the concrete Auto route was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoRouteScope {
    /// The network classifier could choose any runnable provider/model pair in
    /// the redacted inventory. Only reachable under the persisted
    /// `[auto] cross_provider = true` opt-in (#4411).
    RunnableProviders,
    /// The network classifier saw only the active provider's runnable routes —
    /// the default Auto scope (#4411).
    ActiveProvider,
    /// The local declared fallback selected within one resolved route.
    ResolvedProvider,
}

impl AutoRouteScope {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::RunnableProviders => "runnable providers",
            Self::ActiveProvider => "active provider only",
            Self::ResolvedProvider => "resolved provider",
        }
    }
}

/// Non-secret data path used to make an Auto decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoRouteDataPath {
    LocalHeuristic,
    Classifier {
        provider: codewhale_config::route::ProviderId,
        provider_kind: ProviderKind,
        model: String,
    },
    /// A System One decision model (#6525). Additive: older binaries cannot
    /// read a session that persisted this variant.
    Decision {
        route: DecisionRouterRoute,
        model: String,
    },
}

// Persistence/source facts only. Deserializing this projection never admits a route.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AutoRouteDataPathWire {
    LocalHeuristic,
    Classifier {
        provider: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
    },
    Decision {
        route: DecisionRouterRoute,
        model: String,
    },
}
impl Serialize for AutoRouteDataPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Self::LocalHeuristic => AutoRouteDataPathWire::LocalHeuristic,
            Self::Decision { route, model } => AutoRouteDataPathWire::Decision {
                route: *route,
                model: model.clone(),
            },
            Self::Classifier {
                provider,
                provider_kind,
                model,
            } => {
                let tag = codewhale_config::descriptors::tui_wire_tag_for_route(
                    *provider_kind,
                    provider.as_str(),
                )
                .ok_or_else(|| {
                    serde::ser::Error::custom("contradictory classifier source identity")
                })?;
                AutoRouteDataPathWire::Classifier {
                    provider: tag.into(),
                    model: model.clone(),
                    provider_id: (*provider_kind == ProviderKind::Custom
                        && provider.as_str() != "custom")
                        .then(|| provider.to_string()),
                }
            }
        };
        wire.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for AutoRouteDataPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match AutoRouteDataPathWire::deserialize(deserializer)? {
            AutoRouteDataPathWire::LocalHeuristic => Self::LocalHeuristic,
            AutoRouteDataPathWire::Decision { route, model } => Self::Decision { route, model },
            AutoRouteDataPathWire::Classifier {
                provider,
                model,
                provider_id,
            } => {
                let row = codewhale_config::descriptors::compatibility_from_wire_tag(&provider)
                    .ok_or_else(|| {
                        serde::de::Error::custom("unknown classifier source identity")
                    })?;
                let id = provider_id.as_deref().unwrap_or(row.id);
                let kind = codewhale_config::descriptors::kind_from_tui_wire_tag(&provider, id)
                    .ok_or_else(|| {
                        serde::de::Error::custom("contradictory classifier source identity")
                    })?;
                Self::Classifier {
                    provider: id.into(),
                    provider_kind: kind,
                    model,
                }
            }
        })
    }
}

impl AutoRouteDataPath {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::LocalHeuristic => "local only (no router request)".to_string(),
            Self::Classifier {
                provider,
                provider_kind,
                model,
            } => format!(
                "latest request + bounded recent context -> {} / {model}",
                codewhale_config::descriptors::compatibility_for_id(provider.as_str())
                    .filter(|row| row.kind == *provider_kind)
                    .map_or(provider.as_str(), |row| row.label)
            ),
            Self::Decision { route, model } => format!(
                "latest request + bounded recent context -> {} / {model} (decision model)",
                route.display_name()
            ),
        }
    }
}

/// Local signal that selected the provider-safe strong/fast candidate.
///
/// Since the #6290 rework the local fallback never judges request content:
/// without the flash classifier there is no per-request signal, so the route
/// is the configured default (or the runnable fast sibling under the explicit
/// `[auto] cost_saving` opt-in). The content-derived variants below are never
/// constructed for new routes; they are retained so saved sessions from before
/// the rework still deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoRouteHeuristicReason {
    /// Legacy: the deleted keyword/length classifier judged the request
    /// complex. Retained for saved-session serde compat only.
    ComplexRequest,
    /// Legacy: the deleted length rule judged the request short. Retained
    /// for saved-session serde compat only.
    ShortRequest,
    /// Legacy: the deleted length rule judged the request long. Retained
    /// for saved-session serde compat only.
    LongRequest,
    CostSavingPolicy,
    /// Legacy: the deleted classifier judged the request routine. Retained
    /// for saved-session serde compat only.
    RoutineRequest,
    NoFastSibling,
    NoRunnableCandidate,
    /// The configured default model: no classifier was available and no
    /// content signal was consulted.
    DeclaredDefault,
    /// A decision router answered below `[auto.router] min_confidence`.
    LowConfidence,
}

impl AutoRouteHeuristicReason {
    #[must_use]
    fn label(self) -> &'static str {
        match self {
            Self::ComplexRequest => "complex request",
            Self::ShortRequest => "short request",
            Self::LongRequest => "long request",
            Self::CostSavingPolicy => "cost-saving policy",
            Self::RoutineRequest => "routine request",
            Self::NoFastSibling => "no runnable fast sibling",
            Self::NoRunnableCandidate => "no runnable inventory candidate",
            Self::DeclaredDefault => "configured default (no classifier)",
            Self::LowConfidence => "decision confidence below threshold",
        }
    }
}

/// Why the route was selected. Classifier failures are intentionally
/// collapsed to a non-secret reason; provider errors and response bodies must
/// never enter diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoRouteReason {
    ClassifierRecommendation,
    /// The `local_heuristic` alias keeps sessions saved before the #6290
    /// rework loadable; new sessions persist `local_fallback`.
    #[serde(alias = "local_heuristic")]
    LocalFallback(AutoRouteHeuristicReason),
    ClassifierFallback(AutoRouteHeuristicReason),
}

impl AutoRouteReason {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::ClassifierRecommendation => "classifier recommendation".to_string(),
            Self::LocalFallback(reason) => format!("local fallback: {}", reason.label()),
            Self::ClassifierFallback(reason) => {
                format!("classifier fallback: {}", reason.label())
            }
        }
    }
}

/// Effective provider-scoped model pair used to classify the selected tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoRoutePair {
    pub strong: String,
    pub fast: Option<String>,
}

/// Per-turn Auto routing diagnostics. Provider/model identity remains owned by
/// the authoritative runtime `TurnRoute`; this receipt only records how the
/// concrete route was chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoRouteReceipt {
    pub tier: AutoRouteTier,
    pub pair: AutoRoutePair,
    pub scope: AutoRouteScope,
    pub data_path: AutoRouteDataPath,
    pub reason: AutoRouteReason,
    /// Decision-model evidence (#6525); absent for chat routers and for
    /// sessions saved before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<AutoRouteDecisionEvidence>,
    /// Why a configured router did not produce this route. Set by both router
    /// kinds, so a configured-but-failing router is shown as failing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_failure: Option<AutoRouterFailure>,
}

/// Non-secret failure class for a configured router. Provider error bodies
/// never enter this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AutoRouterFailure {
    /// Declared but unusable: missing key, unknown kind, or client setup.
    NotRunnable,
    Timeout,
    Cancelled,
    Http {
        status: u16,
    },
    QuotaExhausted,
    /// The provider rejected the request (model, size, or policy).
    Rejected,
    Transport,
    /// The response could not be decoded or failed validation.
    InvalidAnswer,
}

impl AutoRouterFailure {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::NotRunnable => {
                "not runnable (check the router key and [auto.router])".to_string()
            }
            Self::Timeout => "timed out".to_string(),
            Self::Cancelled => "cancelled".to_string(),
            Self::Http { status } => format!("HTTP {status}"),
            Self::QuotaExhausted => "provider quota or credits exhausted".to_string(),
            Self::Rejected => "request rejected by the provider".to_string(),
            Self::Transport => "network error".to_string(),
            Self::InvalidAnswer => "invalid answer".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoRouteSelection {
    pub provider: ProviderIdentity,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub source: AutoRouteSource,
    /// Present for Auto decisions; explicit inventory lookups intentionally do
    /// not pretend to be Auto routing receipts.
    pub receipt: Option<AutoRouteReceipt>,
    /// Provider calls made to choose this route. These are deliberately kept
    /// separate from the selected parent route: a classifier may run on a
    /// different provider/model/quote, so pricing it under the eventual turn
    /// would double-charge the parent and lose the classifier's real route.
    ///
    /// Auto currently admits at most one classifier request per selection.
    pub routed_usage: Vec<RuntimeUsageRecord>,
    /// Exact frozen routes for admitted classifier calls whose provider
    /// response omitted usage. The count below remains authoritative and may
    /// exceed this bounded vector after overflow.
    pub routed_usage_drop_records: Vec<RuntimeUsageDropRecord>,
    /// Classifier requests admitted to dispatch whose response usage could not
    /// be recovered (timeout/transport failure). Consumers must surface this
    /// as incomplete coverage rather than silently treating it as zero spend.
    pub routed_usage_dropped_records: u64,
}

fn extract_first_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end >= start).then_some(&raw[start..=end])
}

fn parse_auto_route_reasoning_effort(effort: &str) -> Option<ReasoningEffort> {
    ReasoningEffort::parse_strict(effort).ok()
}

/// Normalize an Auto-route effort when only the provider is known.
///
/// This delegates to the one authoritative normalizer,
/// [`ReasoningEffort::normalize_for_route`], with an unresolved route (empty
/// endpoint and wire model), so the Auto path cannot carry a second copy of
/// the historic `low | medium -> high` provider collapse (Slice 4, D2).
#[must_use]
pub fn normalize_auto_route_effort_for_provider(
    provider: ProviderKind,
    effort: ReasoningEffort,
) -> ReasoningEffort {
    effort.normalize_for_route(provider, "", "")
}

/// Select the reasoning request that accompanies an Auto-model route.
///
/// Model routing and reasoning routing are independent. An explicit fixed
/// preference wins over the classifier's suggestion; an absent preference or
/// explicit `Auto` keeps reasoning under per-prompt control.
#[must_use]
pub fn resolve_auto_model_reasoning(
    preference: Option<ReasoningEffort>,
    routed: Option<ReasoningEffort>,
) -> (Option<ReasoningEffort>, bool) {
    match preference {
        Some(
            effort @ (ReasoningEffort::Off
            | ReasoningEffort::Minimal
            | ReasoningEffort::Low
            | ReasoningEffort::Medium
            | ReasoningEffort::High
            | ReasoningEffort::XHigh
            | ReasoningEffort::Ultra
            | ReasoningEffort::Max),
        ) => (Some(effort), false),
        None | Some(ReasoningEffort::Auto) => (routed, true),
    }
}

/// Route-aware equivalent of [`normalize_auto_route_effort_for_provider`].
/// The inventory knows the selected provider/model, and the route resolver
/// supplies the endpoint needed to distinguish Kimi Code's official bare-K3
/// contract from generic Moonshot.
#[must_use]
pub fn normalize_auto_route_effort_for_configured_route(
    config: &Config,
    identity: &ProviderIdentity,
    model: &str,
    effort: ReasoningEffort,
) -> ReasoningEffort {
    crate::route_runtime::resolve_runtime_route_for_identity(config, identity, Some(model))
        .map(|route| {
            effort.normalize_for_route_identity(
                identity.provider,
                identity.key.as_str(),
                &route.candidate.endpoint().base_url,
                &route.model,
            )
        })
        .unwrap_or_else(|_| normalize_auto_route_effort_for_provider(identity.provider, effort))
}

fn normalize_auto_route_selection_for_config(
    config: &Config,
    mut selection: AutoRouteSelection,
) -> AutoRouteSelection {
    selection.reasoning_effort = selection.reasoning_effort.map(|effort| {
        normalize_auto_route_effort_for_configured_route(
            config,
            &selection.provider,
            &selection.model,
            effort,
        )
    });
    selection
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InventoryAutoRouteRecommendation {
    provider: ProviderIdentity,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
}

/// One provider-backed classifier attempt. A provider-success response always
/// reaches this shape before its content is interpreted, so invalid JSON and
/// provider-declared incomplete output retain their exact routed usage.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InventoryAutoRouteAttempt {
    recommendation: Option<InventoryAutoRouteRecommendation>,
    routed_usage: Vec<RuntimeUsageRecord>,
    routed_usage_drop_records: Vec<RuntimeUsageDropRecord>,
    routed_usage_dropped_records: u64,
    /// Decision-model evidence, when a decision router answered.
    decision: Option<AutoRouteDecisionEvidence>,
    /// Why the router produced no usable recommendation.
    failure: Option<AutoRouterFailure>,
    /// Overrides the fallback reason when the router answered but its answer
    /// was not acted on (low confidence).
    fallback_reason: Option<AutoRouteHeuristicReason>,
}

impl InventoryAutoRouteAttempt {
    fn failed(failure: AutoRouterFailure) -> Self {
        Self {
            recommendation: None,
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_dropped_records: 0,
            decision: None,
            failure: Some(failure),
            fallback_reason: None,
        }
    }
}

pub async fn resolve_auto_route_with_inventory(
    config: &Config,
    latest_request: &str,
    recent_context: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
) -> Result<AutoRouteSelection> {
    resolve_auto_route_with_inventory_for_session(
        config,
        latest_request,
        recent_context,
        "agent",
        selected_model_mode,
        selected_thinking_mode,
    )
    .await
}

pub async fn resolve_auto_route_with_inventory_for_session(
    config: &Config,
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
) -> Result<AutoRouteSelection> {
    resolve_auto_route_with_inventory_for_session_and_cache_policy(
        config,
        latest_request,
        recent_context,
        session_mode,
        selected_model_mode,
        selected_thinking_mode,
        true,
    )
    .await
}

pub async fn resolve_auto_route_with_inventory_for_session_and_cache_policy(
    config: &Config,
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
    allow_response_cache: bool,
) -> Result<AutoRouteSelection> {
    let mut inventory = ModelInventory::from_config(config).map_err(anyhow::Error::msg)?;
    if let Some(active) = inventory.active_default()
        && active.provider == ProviderKind::Ollama
        && active.auth_source == crate::model_inventory::ModelAuthSource::KeylessLocal
        && active.context_window_unverified
        && config
            .context_window_for_provider_config(&active.identity)
            .is_none()
    {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            crate::provider_lake::refresh_local_ollama_catalog(config, &active.identity),
        )
        .await;
        inventory = ModelInventory::from_config(config).map_err(anyhow::Error::msg)?;
    }
    if !inventory.router_available {
        // Declared-default auto routing when no router is available. A router
        // that was declared but cannot run is marked failing on the receipt.
        return Ok(normalize_auto_route_selection_for_config(
            config,
            auto_route_without_router(config, &inventory),
        ));
    }

    if cfg!(any(test, feature = "test-support")) {
        return Ok(normalize_auto_route_selection_for_config(
            config,
            auto_route_declared_fallback(config, &inventory),
        ));
    }

    let selection = auto_route_via_router(
        config,
        &inventory,
        latest_request,
        recent_context,
        session_mode,
        selected_model_mode,
        selected_thinking_mode,
        allow_response_cache,
    )
    .await;
    Ok(normalize_auto_route_selection_for_config(config, selection))
}

/// The local fallback when the router is not available, marking a declared
/// but unusable router as failing rather than silently ignoring it.
fn auto_route_without_router(config: &Config, inventory: &ModelInventory) -> AutoRouteSelection {
    let mut selection = auto_route_declared_fallback(config, inventory);
    if inventory.router_setup_issue.is_some()
        && let Some(receipt) = selection.receipt.as_mut()
    {
        receipt.router_failure = Some(AutoRouterFailure::NotRunnable);
    }
    selection
}

/// Ask the configured router (either kind) for this turn's route. Callers
/// have already checked `inventory.router_available`. The `cfg!(any(test, feature = "test-support"))`
/// short-circuit lives in the caller, so tests exercise this directly.
#[allow(clippy::too_many_arguments)]
async fn auto_route_via_router(
    config: &Config,
    inventory: &ModelInventory,
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
    allow_response_cache: bool,
) -> AutoRouteSelection {
    let fallback = auto_route_declared_fallback(config, inventory);
    let attempt = match inventory.router_kind {
        AutoRouterKind::Decision => {
            // Options are tiers, not model ids: without a runnable strong/fast
            // pair there is nothing to decide, so no request and no spend.
            let Some(pair) = runnable_active_pair(inventory) else {
                let mut selection = fallback;
                if let Some(receipt) = selection.receipt.as_mut() {
                    receipt.reason =
                        AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::NoFastSibling);
                }
                return selection;
            };
            auto_route_decision_recommendation(
                config,
                inventory,
                &pair,
                latest_request,
                recent_context,
                session_mode,
                selected_thinking_mode,
            )
            .await
        }
        AutoRouterKind::Chat => {
            auto_route_inventory_recommendation(
                config,
                inventory,
                latest_request,
                recent_context,
                session_mode,
                selected_model_mode,
                selected_thinking_mode,
                allow_response_cache,
            )
            .await
        }
    };
    match attempt {
        Ok(attempt) => auto_route_from_classifier_attempt(fallback, inventory, attempt),
        // Client construction/preparation failed before a provider request was
        // admitted. There is no provider usage to invent and no dropped
        // response receipt to claim.
        Err(_) => {
            let mut selection = auto_route_classifier_fallback(fallback, inventory);
            if let Some(receipt) = selection.receipt.as_mut() {
                receipt.router_failure = Some(AutoRouterFailure::NotRunnable);
            }
            selection
        }
    }
}

/// Fixed synthetic request used by `/router` preset test calls.
pub const ROUTER_TEST_REQUEST: &str = "Rename the variable foo to bar in src/lib.rs";

/// One `/router` preset test call (#6525): exactly the per-turn routing path,
/// run once against a fixed synthetic request, with its wall-clock latency.
/// Returns `Err` with a non-secret reason whenever no request was sent, so
/// the setup view never reports a test that did not happen.
pub async fn test_auto_router(
    config: &Config,
) -> std::result::Result<(AutoRouteSelection, u64), String> {
    let inventory = ModelInventory::from_config(config)?;
    if !inventory.router_available {
        return Err(inventory
            .router_setup_issue
            .map_or("router is not configured", |issue| issue.label())
            .to_string());
    }
    // A decision router with no runnable strong/fast pair has nothing to
    // decide and sends nothing (see `auto_route_via_router`).
    if inventory.router_kind == AutoRouterKind::Decision
        && runnable_active_pair(&inventory).is_none()
    {
        return Err(AutoRouteHeuristicReason::NoFastSibling.label().to_string());
    }
    let started = Instant::now();
    let selection = auto_route_via_router(
        config,
        &inventory,
        ROUTER_TEST_REQUEST,
        "",
        "agent",
        "auto",
        "auto",
        false,
    )
    .await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // `NotRunnable` is set only when the client could not be built or the
    // request failed preflight: nothing reached the network.
    if let Some(failure @ AutoRouterFailure::NotRunnable) = selection
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.router_failure)
    {
        return Err(failure.label());
    }
    Ok((
        normalize_auto_route_selection_for_config(config, selection),
        latency_ms,
    ))
}

pub fn resolve_explicit_route_with_inventory(
    config: &Config,
    requested_model: &str,
) -> Option<AutoRouteSelection> {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() || requested_model.eq_ignore_ascii_case("auto") {
        return None;
    }

    let inventory = ModelInventory::from_config(config).ok()?;
    let active_identity = &inventory.active_identity;

    if let Some(candidate) = inventory.candidates.iter().find(|candidate| {
        &candidate.identity == active_identity
            && explicit_model_matches_candidate(candidate, requested_model)
    }) {
        return Some(AutoRouteSelection {
            provider: candidate.identity.clone(),
            model: candidate.model.clone(),
            reasoning_effort: config.reasoning_effort().map(|setting| {
                normalize_auto_route_effort_for_configured_route(
                    config,
                    &candidate.identity,
                    &candidate.model,
                    ReasoningEffort::from_setting(setting),
                )
            }),
            source: AutoRouteSource::Heuristic,
            receipt: None,
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_dropped_records: 0,
        });
    }

    let mut matches = inventory
        .candidates
        .iter()
        .filter(|candidate| explicit_model_matches_candidate(candidate, requested_model));
    let candidate = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    Some(AutoRouteSelection {
        provider: candidate.identity.clone(),
        model: candidate.model.clone(),
        reasoning_effort: config.reasoning_effort().map(|setting| {
            normalize_auto_route_effort_for_configured_route(
                config,
                &candidate.identity,
                &candidate.model,
                ReasoningEffort::from_setting(setting),
            )
        }),
        source: AutoRouteSource::Heuristic,
        receipt: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_dropped_records: 0,
    })
}

pub fn explicit_route_candidate_providers(
    config: &Config,
    requested_model: &str,
) -> Vec<ProviderIdentity> {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() || requested_model.eq_ignore_ascii_case("auto") {
        return Vec::new();
    }

    let Ok(inventory) = ModelInventory::from_config(config) else {
        return Vec::new();
    };
    let mut providers = Vec::new();
    for candidate in inventory
        .candidates
        .iter()
        .filter(|candidate| explicit_model_matches_candidate(candidate, requested_model))
    {
        if !providers.contains(&candidate.identity) {
            providers.push(candidate.identity.clone());
        }
    }
    providers
}

fn explicit_model_matches_candidate(
    candidate: &crate::model_inventory::ModelRouteCandidate,
    requested_model: &str,
) -> bool {
    candidate.model.eq_ignore_ascii_case(requested_model)
        || normalize_model_name_for_provider(candidate.provider, requested_model)
            .is_some_and(|model| candidate.model.eq_ignore_ascii_case(&model))
}

/// Declared local fallback for Auto routing when the flash classifier is
/// unavailable (or fails): the configured default model.
///
/// There is no per-request signal here by design. Until the #6290 rework this
/// guessed cheap-vs-big from request wording (`COMPLEX_KEYWORDS` plus
/// char-length thresholds) — host-side semantic determinism that made cost
/// and quality depend on vocabulary. The only content-blind override is the
/// explicit `[auto] cost_saving` opt-in, which pins the runnable fast
/// sibling; providers without one stay on the default.
fn auto_route_declared_fallback(config: &Config, inventory: &ModelInventory) -> AutoRouteSelection {
    let Some(active) = inventory.active_default() else {
        let model = config.default_model();
        return AutoRouteSelection {
            provider: inventory.active_identity.clone(),
            receipt: Some(auto_route_receipt(
                inventory,
                &inventory.active_identity,
                &model,
                AutoRouteScope::ResolvedProvider,
                AutoRouteDataPath::LocalHeuristic,
                AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::NoRunnableCandidate),
            )),
            model,
            reasoning_effort: Some(crate::auto_reasoning::select()),
            source: AutoRouteSource::Heuristic,
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_dropped_records: 0,
        };
    };
    let router_candidates = provider_router_candidates(active.provider, &active.model);
    let runnable_fast = router_candidates.cheap.as_deref().filter(|model| {
        inventory
            .candidate(active.identity.key.as_str(), model)
            .is_some_and(|candidate| candidate.readiness.can_attempt())
    });
    let (model, reason) = if config.auto_cost_saving() {
        match runnable_fast {
            Some(cheap) => (
                cheap.to_string(),
                AutoRouteHeuristicReason::CostSavingPolicy,
            ),
            None => (
                active.model.clone(),
                AutoRouteHeuristicReason::NoFastSibling,
            ),
        }
    } else {
        (
            active.model.clone(),
            AutoRouteHeuristicReason::DeclaredDefault,
        )
    };
    AutoRouteSelection {
        provider: active.identity.clone(),
        receipt: Some(auto_route_receipt(
            inventory,
            &active.identity,
            &model,
            AutoRouteScope::ResolvedProvider,
            AutoRouteDataPath::LocalHeuristic,
            AutoRouteReason::LocalFallback(reason),
        )),
        model,
        reasoning_effort: Some(crate::auto_reasoning::select()),
        source: AutoRouteSource::Heuristic,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_dropped_records: 0,
    }
}

fn auto_route_from_classifier(
    inventory: &ModelInventory,
    recommendation: InventoryAutoRouteRecommendation,
) -> AutoRouteSelection {
    let data_path = router_data_path(inventory);
    // Report the scope the classifier actually had, not the widest one it
    // could ever have (#4411). A decision router only ever chooses a tier of
    // the active provider.
    let scope = if inventory.cross_provider_auto && inventory.router_decision_route.is_none() {
        AutoRouteScope::RunnableProviders
    } else {
        AutoRouteScope::ActiveProvider
    };
    AutoRouteSelection {
        provider: recommendation.provider.clone(),
        receipt: Some(auto_route_receipt(
            inventory,
            &recommendation.provider,
            &recommendation.model,
            scope,
            data_path,
            AutoRouteReason::ClassifierRecommendation,
        )),
        model: recommendation.model,
        reasoning_effort: recommendation.reasoning_effort,
        source: AutoRouteSource::FlashRouter,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_dropped_records: 0,
    }
}

fn auto_route_from_classifier_attempt(
    fallback: AutoRouteSelection,
    inventory: &ModelInventory,
    attempt: InventoryAutoRouteAttempt,
) -> AutoRouteSelection {
    let InventoryAutoRouteAttempt {
        recommendation,
        routed_usage,
        routed_usage_drop_records,
        routed_usage_dropped_records,
        decision,
        failure,
        fallback_reason,
    } = attempt;
    let mut selection = recommendation.map_or_else(
        || auto_route_classifier_fallback(fallback, inventory),
        |recommendation| auto_route_from_classifier(inventory, recommendation),
    );
    if let Some(receipt) = selection.receipt.as_mut() {
        if let (Some(reason), AutoRouteReason::ClassifierFallback(_)) =
            (fallback_reason, receipt.reason)
        {
            receipt.reason = AutoRouteReason::ClassifierFallback(reason);
        }
        receipt.decision = decision;
        receipt.router_failure = failure;
    }
    selection.routed_usage = routed_usage;
    selection.routed_usage_drop_records = routed_usage_drop_records;
    selection.routed_usage_dropped_records = routed_usage_dropped_records;
    selection
}

fn auto_route_classifier_fallback(
    mut fallback: AutoRouteSelection,
    inventory: &ModelInventory,
) -> AutoRouteSelection {
    if let Some(receipt) = fallback.receipt.as_mut() {
        let fallback_reason = match receipt.reason {
            AutoRouteReason::LocalFallback(reason)
            | AutoRouteReason::ClassifierFallback(reason) => reason,
            AutoRouteReason::ClassifierRecommendation => AutoRouteHeuristicReason::DeclaredDefault,
        };
        receipt.data_path = router_data_path(inventory);
        receipt.reason = AutoRouteReason::ClassifierFallback(fallback_reason);
    }
    fallback
}

/// The non-secret data path of the configured router.
fn router_data_path(inventory: &ModelInventory) -> AutoRouteDataPath {
    match inventory.router_decision_route {
        Some(route) => AutoRouteDataPath::Decision {
            route,
            model: inventory.router_model.to_string(),
        },
        None => AutoRouteDataPath::Classifier {
            provider: inventory.router_identity.as_ref().map_or_else(
                || inventory.router_provider.as_str().into(),
                |identity| identity.key.clone(),
            ),
            provider_kind: inventory.router_provider,
            model: inventory.router_model.to_string(),
        },
    }
}

fn auto_route_receipt(
    inventory: &ModelInventory,
    identity: &ProviderIdentity,
    selected_model: &str,
    scope: AutoRouteScope,
    data_path: AutoRouteDataPath,
    reason: AutoRouteReason,
) -> AutoRouteReceipt {
    let pair = auto_route_pair(inventory, identity, selected_model);
    let tier = if pair
        .fast
        .as_deref()
        .is_some_and(|fast| fast.eq_ignore_ascii_case(selected_model))
    {
        AutoRouteTier::Fast
    } else if pair.strong.eq_ignore_ascii_case(selected_model) {
        if pair.fast.is_some() {
            AutoRouteTier::Strong
        } else {
            AutoRouteTier::Only
        }
    } else {
        AutoRouteTier::Selected
    };
    AutoRouteReceipt {
        tier,
        pair,
        scope,
        data_path,
        reason,
        decision: None,
        router_failure: None,
    }
}

fn auto_route_pair(
    inventory: &ModelInventory,
    identity: &ProviderIdentity,
    selected_model: &str,
) -> AutoRoutePair {
    // A provider can expose several unrelated model families. Derive the pair
    // from a runnable candidate that actually contains the selected model,
    // preferring a cheap-tier match before a strong-tier match. Falling back
    // to the provider default would report a truthful provider with a false
    // model family (for example OpenRouter GLM reported as DeepSeek).
    let matching_pair = inventory
        .candidates
        .iter()
        .filter(|candidate| candidate.identity == *identity && candidate.readiness.can_attempt())
        .map(|candidate| provider_router_candidates(identity.provider, &candidate.model))
        .find(|pair| {
            pair.cheap
                .as_deref()
                .is_some_and(|fast| fast.eq_ignore_ascii_case(selected_model))
        })
        .or_else(|| {
            inventory
                .candidates
                .iter()
                .filter(|candidate| {
                    candidate.identity == *identity && candidate.readiness.can_attempt()
                })
                .map(|candidate| provider_router_candidates(identity.provider, &candidate.model))
                .find(|pair| pair.big.eq_ignore_ascii_case(selected_model))
        });
    let Some(candidates) = matching_pair else {
        return AutoRoutePair {
            strong: selected_model.to_string(),
            fast: None,
        };
    };
    let Some(strong) = inventory
        .candidate(identity.key.as_str(), &candidates.big)
        .filter(|candidate| candidate.readiness.can_attempt())
        .map(|candidate| candidate.model.clone())
    else {
        return AutoRoutePair {
            strong: selected_model.to_string(),
            fast: None,
        };
    };
    let fast = candidates.cheap.as_deref().and_then(|model| {
        inventory
            .candidate(identity.key.as_str(), model)
            .filter(|candidate| candidate.readiness.can_attempt())
            .map(|candidate| candidate.model.clone())
    });
    AutoRoutePair { strong, fast }
}

fn auto_route_usage_has_reported_data(usage: &codewhale_models::Usage) -> bool {
    usage.input_tokens > 0
        || usage.output_tokens > 0
        || usage.prompt_cache_hit_tokens.is_some()
        || usage.prompt_cache_miss_tokens.is_some()
        || usage.prompt_cache_write_tokens.is_some()
        || usage.reasoning_tokens.is_some()
        || usage.reasoning_replay_tokens.is_some()
        || usage.server_tool_use.is_some()
}

/// Stable, persistence-safe identity for one classifier response. The raw
/// provider response id is hashed with the frozen dispatch route and instant;
/// neither it nor any custom route label crosses into telemetry/persistence.
pub fn auto_route_usage_source_id(route: &EffectiveRouteEnvelope, response_id: &str) -> String {
    use sha2::{Digest as _, Sha256};

    let mut digest = Sha256::new();
    let dispatched_at = route.dispatched_at.to_rfc3339();
    for part in [
        b"codewhale:auto-route-classifier:v1".as_slice(),
        route.provider.as_str().as_bytes(),
        route.provider_identity.as_bytes(),
        route.model.as_bytes(),
        route
            .endpoint_fingerprint
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
        dispatched_at.as_bytes(),
        response_id.as_bytes(),
    ] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    format!(
        "auto-router:{}",
        codewhale_runtime::hashing::hex_bytes(digest.finalize())
    )
}

fn auto_route_attempt_from_response(
    request_route: EffectiveRouteEnvelope,
    response: &MessageResponse,
    inventory: &ModelInventory,
) -> InventoryAutoRouteAttempt {
    // All-zero usage cannot price a routed segment. The dispatch caller owns
    // the stronger cache/provenance context and must explicitly classify this
    // as either a proven cache replay or missing provider billing evidence.
    let routed_usage = auto_route_usage_has_reported_data(&response.usage)
        .then(|| RuntimeUsageRecord {
            source_id: auto_route_usage_source_id(&request_route, &response.id),
            usage: EffectiveRouteUsage {
                route: request_route.sanitized_for_persistence(),
                usage: response.usage.clone(),
            },
        })
        .into_iter()
        .collect();
    let recommendation =
        (!codewhale_models::is_incomplete_stop_reason(response.stop_reason.as_deref()))
            .then(|| {
                parse_inventory_auto_route_recommendation(
                    &message_response_text(response),
                    inventory,
                )
            })
            .flatten();
    InventoryAutoRouteAttempt {
        failure: recommendation
            .is_none()
            .then_some(AutoRouterFailure::InvalidAnswer),
        recommendation,
        routed_usage,
        routed_usage_drop_records: Vec::new(),
        routed_usage_dropped_records: 0,
        decision: None,
        fallback_reason: None,
    }
}

fn auto_route_attempt_from_provider_response(
    request_route: EffectiveRouteEnvelope,
    response: &MessageResponse,
    inventory: &ModelInventory,
) -> InventoryAutoRouteAttempt {
    let drop_route = request_route.sanitized_for_persistence();
    let mut attempt = auto_route_attempt_from_response(request_route, response, inventory);
    // This classifier request is currently not cacheable (temperature is
    // provider-default, not the deterministic Some(0.0) cache contract), so a
    // decoded all-zero response is missing provider billing evidence even when
    // the caller permits response-cache use. Do not silently reinterpret the
    // policy boolean as cache-hit provenance.
    if attempt.routed_usage.is_empty() {
        attempt.routed_usage_drop_records = vec![RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: auto_route_usage_source_id(
                &drop_route,
                &format!("missing-usage:{}", response.id),
            ),
            route: drop_route,
        }];
        attempt.routed_usage_dropped_records = 1;
    }
    attempt
}

fn auto_route_attempt_with_dropped_response(
    request_route: EffectiveRouteEnvelope,
    failure: AutoRouterFailure,
) -> InventoryAutoRouteAttempt {
    let request_route = request_route.sanitized_for_persistence();
    InventoryAutoRouteAttempt {
        routed_usage_drop_records: vec![RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: auto_route_usage_source_id(&request_route, "transport-error"),
            route: request_route,
        }],
        routed_usage_dropped_records: 1,
        ..InventoryAutoRouteAttempt::failed(failure)
    }
}

/// Prove that the deterministic request seam accepts this classifier request
/// before capturing a quote or entering any provider permit/network path.
fn preflight_auto_route_request(client: &CodewhaleClient, request: &MessageRequest) -> Result<()> {
    client.prepare_outbound_request(request.clone(), false)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn auto_route_inventory_recommendation(
    config: &Config,
    inventory: &ModelInventory,
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
    allow_response_cache: bool,
) -> Result<InventoryAutoRouteAttempt> {
    let mut router_config = config.clone();
    // The classifier runs on the inventory's router route: the explicit
    // [auto.router] route when configured, else the DeepSeek flash default.
    let identity = inventory
        .router_identity
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("router identity was not admitted"))?;
    router_config
        .scope_to_provider_identity(identity)
        .map_err(anyhow::Error::msg)?;
    router_config.default_text_model = Some(inventory.router_model.clone());

    let client = CodewhaleClient::new(&router_config)?;
    let router_system = inventory_auto_router_system_prompt(inventory, config.auto_cost_saving());
    let router_prompt = classifier_prompt(
        &client,
        latest_request,
        recent_context,
        session_mode,
        selected_model_mode,
        selected_thinking_mode,
    );
    let max_tokens = client.effective_max_output_tokens(&inventory.router_model);
    let request = MessageRequest {
        model: inventory.router_model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: router_prompt,
                cache_control: None,
            }],
        }],
        max_tokens,
        system: Some(SystemPrompt::Text(router_system)),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: Some(
            inventory
                .router_thinking
                .clone()
                .unwrap_or_else(|| "off".to_string()),
        ),
        stream: Some(false),
        temperature: None,
        top_p: None,
    };

    // Freeze pricing at the last application seam before the provider future
    // starts. Prompt shaping above may be slow and may overlap a catalog
    // refresh; completion-time mutable catalog state must never reprice this
    // already-admitted classifier request.
    preflight_auto_route_request(&client, &request)?;
    let request_route =
        client.effective_route_envelope(&inventory.router_model, chrono::Utc::now());
    let response = if allow_response_cache {
        tokio::time::timeout(
            Duration::from_secs(inventory.router_timeout_secs),
            client.create_message(request),
        )
        .await
    } else {
        tokio::time::timeout(
            Duration::from_secs(inventory.router_timeout_secs),
            client.create_message_without_response_cache(request),
        )
        .await
    };
    let response = match response {
        Ok(Ok(response)) => response,
        // The request crossed Codewhale's dispatch boundary, but no exact
        // provider usage came back. Preserve the fallback while explicitly
        // failing cost coverage closed.
        Ok(Err(error)) => {
            return Ok(auto_route_attempt_with_dropped_response(
                request_route,
                crate::client::system_one::router_failure_from_error(&error),
            ));
        }
        // The local deadline cancels the future and can fire while the request
        // is still waiting on an application/provider permit. With no response
        // evidence we must not invent a provider call or a missing-usage
        // receipt. Transport errors returned by the client remain the
        // conservative explicit-dropped path above.
        Err(_) => {
            return Ok(InventoryAutoRouteAttempt::failed(
                AutoRouterFailure::Timeout,
            ));
        }
    };
    Ok(auto_route_attempt_from_provider_response(
        request_route,
        &response,
        inventory,
    ))
}

/// The active provider's runnable strong/fast pair, when both tiers can run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveTierPair {
    provider: ProviderIdentity,
    strong: String,
    fast: String,
}

fn runnable_active_pair(inventory: &ModelInventory) -> Option<ActiveTierPair> {
    let active = inventory.active_default()?;
    let candidates = provider_router_candidates(active.provider, &active.model);
    let runnable = |model: &str| {
        inventory
            .candidate(active.identity.key.as_str(), model)
            .filter(|candidate| candidate.readiness.can_attempt())
            .map(|candidate| candidate.model.clone())
    };
    Some(ActiveTierPair {
        provider: active.identity.clone(),
        strong: runnable(&candidates.big)?,
        fast: runnable(candidates.cheap.as_deref()?)?,
    })
}

// The decision model reads its criteria literally, so this wording is a
// product surface: changing it changes routing. Pinned by a snapshot test.
pub const DECISION_TIER_INSTRUCTIONS: &str =
    "Which model tier should handle the latest request in this coding-agent session?";
pub const DECISION_FAST_WHAT: &str = "A fast, cheaper model. Right for questions, explanations, lookups, small single-file edits, formatting, and routine follow-ups.";
pub const DECISION_FAST_NOT_FOR: &str = "Multi-step agentic work, debugging across files, architecture or design, security review, release work.";
pub const DECISION_STRONG_WHAT: &str = "The strongest model. Right for multi-step agentic coding, multi-file changes, debugging, architecture or design, security review, release work, or anything the fast tier would likely get wrong.";
pub const DECISION_STRONG_NOT_FOR: &str = "Trivial questions or one-line edits.";
pub const DECISION_THINKING_INSTRUCTIONS: &str =
    "How much reasoning should the chosen model spend on the latest request?";
pub const DECISION_THINKING_OFF: &str = "A trivial answer with no tools and no reasoning needed.";
pub const DECISION_THINKING_HIGH: &str = "Ordinary reasoning: a normal coding or explanation task.";
pub const DECISION_THINKING_MAX: &str =
    "Agentic, multi-file, debugging, architecture, security, release, or uncertain work.";

const DECISION_TIER_OPTIONS: [&str; 2] = ["fast", "strong"];
const DECISION_THINKING_OPTIONS: [&str; 3] = ["off", "high", "max"];
/// Under `[auto] cost_saving`, a `strong` decision needs at least this
/// probability (basis points) or the turn stays on the fast tier.
pub const COST_SAVING_STRONG_MIN_BP: u16 = 7_500;

/// The System One request: one `tier` choice and one `thinking` choice over
/// the same redacted, bounded state.
fn decision_request_body(
    client: &CodewhaleClient,
    model: &str,
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_thinking_mode: &str,
) -> serde_json::Value {
    let recent_context = if recent_context.trim().is_empty() {
        "No prior context."
    } else {
        recent_context
    };
    serde_json::json!({
        "model": model,
        "state": {
            "session_mode": client.redact_model_bound_text(session_mode),
            "selected_thinking_mode": client.redact_model_bound_text(selected_thinking_mode),
            "recent_context": client.redact_model_bound_text(recent_context),
            "latest_request": client
                .redact_model_bound_text(&truncate_for_auto_router(latest_request, 4_000)),
        },
        "questions": {
            "tier": {
                "type": "choice",
                "instructions": DECISION_TIER_INSTRUCTIONS,
                "criteria": {
                    "fast": { "what": DECISION_FAST_WHAT, "not_for": DECISION_FAST_NOT_FOR },
                    "strong": { "what": DECISION_STRONG_WHAT, "not_for": DECISION_STRONG_NOT_FOR },
                },
            },
            "thinking": {
                "type": "choice",
                "instructions": DECISION_THINKING_INSTRUCTIONS,
                "criteria": {
                    "off": DECISION_THINKING_OFF,
                    "high": DECISION_THINKING_HIGH,
                    "max": DECISION_THINKING_MAX,
                },
            },
        },
    })
}

#[allow(clippy::too_many_arguments)]
async fn auto_route_decision_recommendation(
    config: &Config,
    inventory: &ModelInventory,
    pair: &ActiveTierPair,
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_thinking_mode: &str,
) -> Result<InventoryAutoRouteAttempt> {
    let route = inventory
        .router_decision_route
        .ok_or_else(|| anyhow::anyhow!("decision router has no route"))?;
    // Resolve environment/keyring credentials off the async worker.
    let decision_config = config.clone();
    let base_url = inventory.router_base_url.clone();
    #[cfg(any(test, feature = "test-support"))]
    let ticket = crate::test_support::env_scope_ticket();
    let client = tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _membership = crate::test_support::join_env_scope(ticket);
        CodewhaleClient::for_decision_route(&decision_config, route, base_url.as_deref())
    })
    .await??;
    let body = decision_request_body(
        &client,
        &inventory.router_model,
        latest_request,
        recent_context,
        session_mode,
        selected_thinking_mode,
    );
    // Both transports enter the existing usage ledger. TypeSafe carries its
    // own Custom/Unknown identity; no chat price or zero-cost claim is inherited.
    let request_route =
        Some(client.effective_route_envelope(&inventory.router_model, chrono::Utc::now()));
    let started = Instant::now();
    let dispatched = std::sync::atomic::AtomicBool::new(false);
    let outcome = tokio::time::timeout(
        Duration::from_secs(inventory.router_timeout_secs),
        client.system_one_decide(&body, &dispatched),
    )
    .await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(match outcome {
        // A deadline that fired after the request was handed to the transport
        // may still be billed: record the coverage gap. One that fired while
        // waiting on a permit sent nothing and invents no receipt.
        Err(_) => match request_route {
            Some(route) if dispatched.load(std::sync::atomic::Ordering::Acquire) => {
                auto_route_attempt_with_dropped_response(route, AutoRouterFailure::Timeout)
            }
            _ => InventoryAutoRouteAttempt::failed(AutoRouterFailure::Timeout),
        },
        Ok(Err(failure)) => match request_route {
            Some(route) if dispatched.load(std::sync::atomic::Ordering::Acquire) => {
                auto_route_attempt_with_dropped_response(route, failure)
            }
            _ => InventoryAutoRouteAttempt::failed(failure),
        },
        Ok(Ok(response)) => decision_attempt_from_response(
            config.auto_cost_saving(),
            inventory,
            pair,
            request_route,
            &response,
            latency_ms,
        ),
    })
}

/// A validated `choice` answer.
pub struct ValidChoice {
    pub choice: String,
    pub probabilities_bp: BTreeMap<String, u16>,
    pub confidence_bp: u16,
}

/// Validate one `choice` answer against the offered options. Any violation
/// rejects the whole answer; nothing is repaired.
pub fn validated_choice(answer: Option<&SystemOneAnswer>, options: &[&str]) -> Option<ValidChoice> {
    let answer = answer?;
    if answer.kind != "choice" {
        return None;
    }
    let choice = answer.choice.as_deref()?;
    if !options.contains(&choice) || answer.probabilities.len() != options.len() {
        return None;
    }
    let mut sum = 0.0;
    let mut probabilities_bp = BTreeMap::new();
    for option in options {
        let value = (*answer.probabilities.get(*option)?)?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return None;
        }
        sum += value;
        probabilities_bp.insert((*option).to_string(), probability_bp(value));
    }
    if (sum - 1.0).abs() > 0.02 {
        return None;
    }
    let selected = (*answer.probabilities.get(choice)?)?;
    if answer
        .probabilities
        .values()
        .any(|probability| probability.is_some_and(|v| v > selected))
    {
        return None;
    }
    let confidence = answer.confidence?;
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return None;
    }
    Some(ValidChoice {
        choice: choice.to_string(),
        probabilities_bp,
        confidence_bp: probability_bp(confidence),
    })
}

/// One decision response settled through the existing routed-usage ledger.
/// Malformed policy answers still carry provider usage; missing/all-zero usage
/// remains an explicit coverage gap. The source identity is route-bound.
pub fn decision_usage_batch(
    request_route: &EffectiveRouteEnvelope,
    response: &SystemOneResponse,
) -> crate::cost_status::RuntimeUsageBatch {
    let usage = codewhale_models::Usage {
        input_tokens: response.usage.as_ref().map_or(0, |u| u.input_tokens),
        output_tokens: response.usage.as_ref().map_or(0, |u| u.output_tokens),
        ..Default::default()
    };
    let source_id =
        auto_route_usage_source_id(request_route, response.id.as_deref().unwrap_or("systemone"));
    let route = request_route.sanitized_for_persistence();
    if response.usage.as_ref().is_some_and(|u| u.complete)
        && auto_route_usage_has_reported_data(&usage)
    {
        crate::cost_status::RuntimeUsageBatch {
            decisions: Vec::new(),
            records: vec![RuntimeUsageRecord {
                source_id,
                usage: EffectiveRouteUsage { route, usage },
            }],
            ..Default::default()
        }
    } else {
        crate::cost_status::RuntimeUsageBatch {
            decisions: Vec::new(),
            drop_records: vec![RuntimeUsageDropRecord {
                source_id,
                route,
                reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            }],
            dropped_records: 1,
            ..Default::default()
        }
    }
}

/// Turn a decoded System One response into a routing attempt. All policy is
/// here, in code: the decision model does no arithmetic.
fn decision_attempt_from_response(
    cost_saving: bool,
    inventory: &ModelInventory,
    pair: &ActiveTierPair,
    request_route: Option<EffectiveRouteEnvelope>,
    response: &SystemOneResponse,
    latency_ms: u64,
) -> InventoryAutoRouteAttempt {
    let mut attempt = InventoryAutoRouteAttempt {
        recommendation: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_dropped_records: 0,
        decision: None,
        failure: None,
        fallback_reason: None,
    };
    if let Some(request_route) = request_route {
        let batch = decision_usage_batch(&request_route, response);
        attempt.routed_usage = batch.records;
        attempt.routed_usage_dropped_records = batch.dropped_records;
        attempt.routed_usage_drop_records = batch.drop_records;
    }

    // Rejected answers still incurred spend. Keep bounded diagnostic evidence
    // before any policy return; valid tier evidence below replaces this shell.
    attempt.decision = Some(AutoRouteDecisionEvidence {
        choice: "invalid".to_string(),
        probabilities_bp: Default::default(),
        confidence_bp: 0,
        min_confidence_bp: inventory.router_min_confidence_bp,
        cost_saving_kept_fast: false,
        thinking: None,
        provider_reported_cost_usd: response
            .usage
            .as_ref()
            .and_then(|usage| usage.reported_cost()),
        latency_ms,
        response_model: response
            .model
            .as_deref()
            .map(|model| model.chars().take(128).collect()),
    });
    if response.answers_validated == Some(false) {
        attempt.failure = Some(AutoRouterFailure::InvalidAnswer);
        return attempt;
    }
    let Some(tier) = validated_choice(response.answers.get("tier"), &DECISION_TIER_OPTIONS) else {
        attempt.failure = Some(AutoRouterFailure::InvalidAnswer);
        return attempt;
    };
    let min_confidence_bp = inventory.router_min_confidence_bp;
    // An invalid or unsure `thinking` answer drops only the effort.
    let thinking = validated_choice(response.answers.get("thinking"), &DECISION_THINKING_OPTIONS)
        .filter(|thinking| thinking.confidence_bp >= min_confidence_bp)
        .map(|thinking| thinking.choice);
    let strong_bp = tier.probabilities_bp.get("strong").copied().unwrap_or(0);
    let cost_saving_kept_fast =
        cost_saving && tier.choice == "strong" && strong_bp < COST_SAVING_STRONG_MIN_BP;
    let acted = tier.confidence_bp >= min_confidence_bp;
    attempt.decision = Some(AutoRouteDecisionEvidence {
        choice: tier.choice.clone(),
        probabilities_bp: tier.probabilities_bp,
        confidence_bp: tier.confidence_bp,
        min_confidence_bp,
        cost_saving_kept_fast: acted && cost_saving_kept_fast,
        thinking: thinking.clone().filter(|_| acted),
        provider_reported_cost_usd: response
            .usage
            .as_ref()
            .and_then(|usage| usage.reported_cost()),
        latency_ms,
        response_model: response
            .model
            .as_deref()
            .map(|model| model.chars().take(128).collect()),
    });
    if !acted {
        attempt.fallback_reason = Some(AutoRouteHeuristicReason::LowConfidence);
        return attempt;
    }
    let model = if tier.choice == "strong" && !cost_saving_kept_fast {
        pair.strong.clone()
    } else {
        pair.fast.clone()
    };
    attempt.recommendation = Some(InventoryAutoRouteRecommendation {
        provider: pair.provider.clone(),
        model,
        reasoning_effort: thinking
            .as_deref()
            .and_then(parse_auto_route_reasoning_effort),
    });
    attempt
}

fn inventory_auto_router_system_prompt(inventory: &ModelInventory, cost_saving: bool) -> String {
    let mut prompt = if inventory.cross_provider_auto {
        String::new()
    } else {
        // The inventory JSON below is already scoped to the active provider
        // (#4411); say so, so the classifier does not try to name one it was
        // never shown.
        format!(
            "Auto routing is scoped to the active provider `{}`. Every model in the inventory \
below belongs to it; never select another provider.\n\n",
            inventory.active_identity.key.as_str()
        )
    };
    prompt.push_str(&format!(
        "You are the codewhale model-routing classifier. Return only compact JSON: \
{{\"provider\":\"<provider>\",\"model\":\"<model>\",\"thinking\":\"off|high|max\"}}.\n\
Choose only provider/model pairs present in the inventory JSON. Use off only for trivial no-tool answers, \
high for ordinary reasoning, and max for agentic, coding, multi-file, release, architecture, debugging, \
security, tool-heavy, or uncertain work.\n\nInventory JSON:\n{}",
        inventory.router_context_json()
    ));

    if cost_saving {
        if let Some(ActiveTierPair {
            provider,
            strong,
            fast,
        }) = runnable_active_pair(inventory)
        {
            prompt.push_str(&format!(
                "\n\nCost-saving mode is ON. For the active provider `{}`, `{fast}` is the fast tier \
and `{strong}` is the strong tier. Prefer `{fast}` for ambiguous, routine, or single-step work. \
Select `{strong}` only when the request is unmistakably agentic, multi-step, architecture/design, \
security review, debugging, or otherwise clearly beyond the fast tier. Keep the selected model paired \
with provider `{}`.",
                provider.key.as_str(),
                provider.key.as_str()
            ));
        } else {
            prompt.push_str(
                "\n\nCost-saving mode is ON, but the active provider has no known runnable fast sibling. \
Do not invent a model or cross-provider downgrade solely to save cost.",
            );
        }
    }

    prompt
}

fn parse_inventory_auto_route_recommendation(
    raw: &str,
    inventory: &ModelInventory,
) -> Option<InventoryAutoRouteRecommendation> {
    let json = extract_first_json_object(raw)?;
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let name = value.get("provider").and_then(serde_json::Value::as_str)?;
    let model = value.get("model").and_then(serde_json::Value::as_str)?;
    // Match only identities offered in this captured inventory. A name cannot
    // create a route, borrow a brand's authority, or broaden the Auto scope.
    let mut matches = inventory.candidates.iter().filter(|candidate| {
        let offered = candidate.identity.key.as_str() == name
            || candidate.identity.compatibility().is_some_and(|row| {
                row.tui_wire_tag == name
                    || row
                        .selector_aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(name))
            });
        offered
            && inventory.auto_scope_allows(candidate.identity.key.as_str())
            && candidate.readiness.can_attempt()
            && (candidate.model == model
                || (!candidate.user_declared && candidate.model.eq_ignore_ascii_case(model)))
    });
    let candidate = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let reasoning_effort = value
        .get("thinking")
        .or_else(|| value.get("reasoning_effort"))
        .or_else(|| value.get("effort"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_auto_route_reasoning_effort);
    Some(InventoryAutoRouteRecommendation {
        provider: candidate.identity.clone(),
        model: candidate.model.clone(),
        reasoning_effort,
    })
}

fn auto_route_prompt(
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
) -> String {
    format!(
        "Session mode: {}\nSelected model mode: {}\nSelected thinking mode: {}\n\nRecent context:\n{}\n\nLatest user request:\n{}\n\nReturn JSON only.",
        session_mode,
        selected_model_mode,
        selected_thinking_mode,
        if recent_context.trim().is_empty() {
            "No prior context."
        } else {
            recent_context
        },
        truncate_for_auto_router(latest_request, 4_000)
    )
}

fn classifier_prompt(
    client: &CodewhaleClient,
    latest_request: &str,
    recent_context: &str,
    session_mode: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
) -> String {
    client.redact_model_bound_text(&auto_route_prompt(
        latest_request,
        recent_context,
        session_mode,
        selected_model_mode,
        selected_thinking_mode,
    ))
}

fn message_response_text(response: &MessageResponse) -> String {
    let mut out = String::new();
    for block in &response.content {
        match block {
            ContentBlock::Text { text, .. } | ContentBlock::ToolResult { content: text, .. } => {
                append_router_text(&mut out, text);
            }
            ContentBlock::Thinking { thinking, .. } => {
                append_router_text(&mut out, thinking);
            }
            ContentBlock::ToolUse { name, .. } => {
                append_router_text(&mut out, &format!("[tool call: {name}]"));
            }
            _ => {}
        }
    }
    out
}

fn append_router_text(out: &mut String, text: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(text);
}

pub fn truncate_for_auto_router(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ProviderCatalogReset;

    impl Drop for ProviderCatalogReset {
        fn drop(&mut self) {
            crate::provider_catalog_live::reset_cache_for_test();
            crate::provider_lake::clear_live_snapshot();
        }
    }

    fn priced_openrouter_delta(
        model: &str,
        fingerprint: &str,
        fetched_at: u64,
        input: f64,
        output: f64,
    ) -> codewhale_config::catalog::ProviderCatalogDelta {
        use codewhale_config::catalog::{CatalogOffering, CatalogSource, ProviderCatalogDelta};

        ProviderCatalogDelta {
            provider: ProviderKind::Openrouter.as_str().to_string(),
            base_url_fingerprint: fingerprint.to_string(),
            fetched_at,
            offerings: vec![CatalogOffering {
                provider: ProviderKind::Openrouter.as_str().to_string(),
                wire_model_id: model.to_string(),
                endpoint_key: "chat".to_string(),
                source: CatalogSource::Live {
                    base_url_fingerprint: fingerprint.to_string(),
                    fetched_at,
                },
                cost: Some(codewhale_config::models_dev::ModelsDevCost {
                    input: Some(input),
                    output: Some(output),
                    cache_read: Some(input / 2.0),
                    cache_write: None,
                    ..Default::default()
                }),
                ..CatalogOffering::default()
            }],
        }
    }

    fn classifier_response(
        id: &str,
        text: &str,
        stop_reason: &str,
        usage: codewhale_models::Usage,
    ) -> MessageResponse {
        MessageResponse {
            id: id.to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            model: "router-response-alias-must-not-price".to_string(),
            stop_reason: Some(stop_reason.to_string()),
            stop_sequence: None,
            container: None,
            usage,
        }
    }

    #[test]
    fn classifier_semantic_fallbacks_keep_exact_quotes_and_replay_once() {
        let _env_lock = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let model = "synthetic/openrouter-auto-classifier";
        let config = {
            let __cw_config_init_0_0 = Some("openrouter".to_string());
            let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
                openrouter: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("test-openrouter-key".to_string()),
                    base_url: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL
                            .to_string(),
                    ),
                    model: Some(model.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let __cw_config_init_0_2 = Some(codewhale_runtime::config::AutoConfig {
                cost_saving: None,
                cross_provider: None,
                router: Some(codewhale_runtime::config::AutoRouterConfig {
                    provider: Some("openrouter".to_string()),
                    model: Some(model.to_string()),
                    thinking: Some("off".to_string()),
                    timeout_secs: None,
                    ..Default::default()
                }),
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value.auto = __cw_config_init_0_2;
            __cw_config_init_0_value
        };
        let inventory = ModelInventory::from_config(&config).unwrap();
        assert!(
            inventory
                .candidate(ProviderKind::Openrouter.as_str(), model)
                .is_some()
        );
        let client = CodewhaleClient::new(&config).expect("OpenRouter classifier client");
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
        );
        let first_at = chrono::Utc::now();
        let fetched_at = u64::try_from(first_at.timestamp()).expect("nonnegative timestamp");

        crate::provider_catalog_live::record_success(priced_openrouter_delta(
            model,
            &fingerprint,
            fetched_at,
            1.0,
            4.0,
        ));
        let first_route = client.effective_route_envelope(model, first_at);
        let valid = auto_route_attempt_from_response(
            first_route,
            &classifier_response(
                "same-provider-response-id",
                &format!(r#"{{"provider":"openrouter","model":"{model}","thinking":"off"}}"#),
                "stop",
                codewhale_models::Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    prompt_cache_hit_tokens: Some(3),
                    ..Default::default()
                },
            ),
            &inventory,
        );

        // Replace the live row in the same Unix second. The first routed
        // record must keep its old immutable revision and the new attempt must
        // freeze a distinct one at its own dispatch boundary.
        crate::provider_catalog_live::record_success(priced_openrouter_delta(
            model,
            &fingerprint,
            fetched_at,
            9.0,
            19.0,
        ));
        let second_at = first_at + chrono::Duration::nanoseconds(1);
        let invalid = auto_route_attempt_from_response(
            client.effective_route_envelope(model, second_at),
            &classifier_response(
                "same-provider-response-id",
                "not valid route json",
                "stop",
                codewhale_models::Usage {
                    input_tokens: 11,
                    output_tokens: 3,
                    ..Default::default()
                },
            ),
            &inventory,
        );
        let incomplete = auto_route_attempt_from_response(
            client.effective_route_envelope(model, second_at + chrono::Duration::nanoseconds(1)),
            &classifier_response(
                "same-provider-response-id",
                &format!(r#"{{"provider":"openrouter","model":"{model}"}}"#),
                "length",
                codewhale_models::Usage {
                    input_tokens: 12,
                    output_tokens: 4,
                    ..Default::default()
                },
            ),
            &inventory,
        );
        let missing_usage_route =
            client.effective_route_envelope(model, second_at + chrono::Duration::nanoseconds(2));
        let missing_usage = auto_route_attempt_from_provider_response(
            missing_usage_route.clone(),
            &classifier_response(
                "missing-usage-response-id",
                "not valid route json",
                "stop",
                codewhale_models::Usage::default(),
            ),
            &inventory,
        );
        assert!(missing_usage.routed_usage.is_empty());
        assert_eq!(missing_usage.routed_usage_dropped_records, 1);
        assert_eq!(missing_usage.routed_usage_drop_records.len(), 1);
        assert_eq!(
            missing_usage.routed_usage_drop_records[0].route,
            missing_usage_route.sanitized_for_persistence()
        );
        assert!(
            missing_usage.routed_usage_drop_records[0]
                .source_id
                .starts_with("auto-router:")
        );
        assert!(
            !missing_usage.routed_usage_drop_records[0]
                .source_id
                .contains("missing-usage-response-id")
        );

        let transport = auto_route_attempt_with_dropped_response(
            client.effective_route_envelope(model, second_at + chrono::Duration::nanoseconds(3)),
            AutoRouterFailure::Transport,
        );
        assert_eq!(transport.routed_usage_dropped_records, 1);
        assert_eq!(transport.routed_usage_drop_records.len(), 1);
        assert!(transport.routed_usage.is_empty());

        let fallback = auto_route_declared_fallback(&config, &inventory);
        let valid = auto_route_from_classifier_attempt(fallback.clone(), &inventory, valid);
        let invalid = auto_route_from_classifier_attempt(fallback.clone(), &inventory, invalid);
        let incomplete = auto_route_from_classifier_attempt(fallback, &inventory, incomplete);
        assert_eq!(valid.source, AutoRouteSource::FlashRouter);
        for fallback in [&invalid, &incomplete] {
            assert_eq!(fallback.source, AutoRouteSource::Heuristic);
            assert!(matches!(
                fallback.receipt.as_ref().map(|receipt| receipt.reason),
                Some(AutoRouteReason::ClassifierFallback(_))
            ));
            assert_eq!(fallback.routed_usage.len(), 1);
            assert_eq!(fallback.routed_usage_dropped_records, 0);
        }
        assert_eq!(valid.routed_usage.len(), 1);
        assert_eq!(valid.routed_usage[0].usage.usage.input_tokens, 10);
        assert_eq!(
            valid.routed_usage[0].usage.usage.prompt_cache_hit_tokens,
            Some(3)
        );

        let first_quote = valid.routed_usage[0]
            .usage
            .route
            .provider_live_pricing
            .as_ref()
            .expect("first exact quote");
        let second_quote = invalid.routed_usage[0]
            .usage
            .route
            .provider_live_pricing
            .as_ref()
            .expect("replacement exact quote");
        assert_ne!(first_quote.catalog_revision, second_quote.catalog_revision);
        assert_eq!(first_quote.input_per_million.as_deref(), Some("1"));
        assert_eq!(second_quote.input_per_million.as_deref(), Some("9"));

        let records = valid
            .routed_usage
            .iter()
            .chain(&invalid.routed_usage)
            .chain(&incomplete.routed_usage)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|record| {
            record.source_id.starts_with("auto-router:")
                && record.source_id.len() == "auto-router:".len() + 64
                && !record.source_id.contains("same-provider-response-id")
        }));

        // Exercise the canonical sink exactly as selection consumers do:
        // replaying any record cannot add parent-route spend or a second
        // routed segment, while distinct dispatches remain distinct.
        let _cost_scope = crate::cost_status::test_scope();
        let owner = "auto-router-selection-test-owner";
        crate::cost_status::register_interactive_runtime_usage_sink(
            owner,
            crate::cost_status::scope_token(),
        );
        let lease = crate::cost_status::acquire_runtime_usage_lease(owner)
            .expect("runtime usage owner lease");
        for record in &records {
            for _ in 0..2 {
                crate::cost_status::report_effective_route_for_runtime(
                    crate::cost_status::scope_token(),
                    Some(lease.owner()),
                    &record.source_id,
                    &record.usage.route,
                    &record.usage.usage,
                );
            }
        }
        crate::cost_status::finish_runtime_usage_owner(owner);
        drop(lease);
        let pending = crate::cost_status::drain();
        assert_eq!(pending.usage_source_fingerprints.len(), records.len());
        assert_eq!(pending.priced_turns, records.len() as u32);
        assert_eq!(pending.unpriced_turns, 0);
    }

    #[test]
    fn auto_model_reasoning_keeps_model_and_thinking_choices_independent() {
        assert_eq!(
            resolve_auto_model_reasoning(Some(ReasoningEffort::Low), Some(ReasoningEffort::Max)),
            (Some(ReasoningEffort::Low), false)
        );
        assert_eq!(
            resolve_auto_model_reasoning(Some(ReasoningEffort::Auto), Some(ReasoningEffort::Max)),
            (Some(ReasoningEffort::Max), true)
        );
        assert_eq!(
            resolve_auto_model_reasoning(None, Some(ReasoningEffort::High)),
            (Some(ReasoningEffort::High), true)
        );
    }

    #[test]
    fn auto_route_prompt_uses_current_session_mode() {
        let prompt = auto_route_prompt(
            "Please explain the change before editing files.",
            "No prior context.",
            "plan",
            "auto",
            "auto",
        );

        assert!(
            prompt.starts_with("Session mode: plan\n"),
            "auto-route prompt should reflect the active session mode, got: {prompt}"
        );
    }

    #[test]
    fn classifier_prompt_redacts_secret_after_tool_result_flattening() {
        let secret = "cw-router-secret-should-never-leave-process";
        let config =
            <Config as Default>::default().with_legacy_root(Some(secret.to_string()), None);
        let client = CodewhaleClient::new(&config).expect("classifier client");
        // `recent_auto_router_context` converts ToolResult blocks into ordinary
        // text before this boundary. Exercise that exact flattened shape.
        let recent_context = format!("assistant: [tool result] token={secret}");

        let prompt = classifier_prompt(
            &client,
            "continue the investigation",
            &recent_context,
            "agent",
            "auto",
            "auto",
        );

        assert!(
            !prompt.contains(secret),
            "flattened tool-result secret leaked"
        );
        assert!(
            prompt.contains(codewhale_config::persistence::REDACTED),
            "secret should be visibly redacted"
        );
        assert!(prompt.contains("continue the investigation"));
    }

    #[test]
    fn inventory_auto_router_prompt_names_cost_saving_zai_pair() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_2_0 = Some("zai".to_string());
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value
        };
        let inventory = ModelInventory::from_config(&config).unwrap();

        let balanced = inventory_auto_router_system_prompt(&inventory, false);
        let cost_saving = inventory_auto_router_system_prompt(&inventory, true);

        assert!(!balanced.contains("Cost-saving mode is ON"));
        assert!(
            cost_saving.contains(
                "For the active provider `zai`, `GLM-5.3-Flash` is the fast tier and `GLM-5.3` is the strong tier"
            ),
            "cost-saving classifier policy must name the provider-safe pair: {cost_saving}"
        );
        assert!(
            cost_saving.contains("Keep the selected model paired with provider `zai`"),
            "cost-saving policy must preserve provider/model validation: {cost_saving}"
        );
    }

    #[test]
    fn auto_route_effort_normalization_is_provider_aware() {
        // Slice 4, D2: the Auto path delegates to the canonical route
        // normalizer with an unresolved route. Two providers where the deleted
        // local copy disagreed with the authority are pinned explicitly.
        assert_eq!(
            normalize_auto_route_effort_for_provider(ProviderKind::Deepseek, ReasoningEffort::Low),
            ReasoningEffort::Low,
            "first-party DeepSeek documents low|high|max, so the canonical normalizer keeps low"
        );
        assert_eq!(
            normalize_auto_route_effort_for_provider(
                ProviderKind::Deepseek,
                ReasoningEffort::Medium
            ),
            ReasoningEffort::High
        );
        assert_eq!(
            normalize_auto_route_effort_for_provider(
                ProviderKind::OllamaCloud,
                ReasoningEffort::Minimal
            ),
            ReasoningEffort::Low,
            "OllamaCloud folds the Codewhale-only `minimal` spelling onto low"
        );
        assert_eq!(
            normalize_auto_route_effort_for_provider(
                ProviderKind::OllamaCloud,
                ReasoningEffort::Ultra
            ),
            ReasoningEffort::Max
        );
        // A provider with no exact-route rule keeps the historic collapse.
        assert_eq!(
            normalize_auto_route_effort_for_provider(ProviderKind::Moonshot, ReasoningEffort::Low),
            ReasoningEffort::High
        );
        assert_eq!(
            normalize_auto_route_effort_for_provider(ProviderKind::Moonshot, ReasoningEffort::Auto),
            ReasoningEffort::Auto
        );
        assert_eq!(
            normalize_auto_route_effort_for_provider(ProviderKind::Moonshot, ReasoningEffort::Max),
            ReasoningEffort::Max
        );
        // Codex keeps its provider-level mapping (off -> low, auto -> medium).
        assert_eq!(
            normalize_auto_route_effort_for_provider(
                ProviderKind::OpenaiCodex,
                ReasoningEffort::Low
            ),
            ReasoningEffort::Low
        );
        assert_eq!(
            normalize_auto_route_effort_for_provider(
                ProviderKind::OpenaiCodex,
                ReasoningEffort::Medium
            ),
            ReasoningEffort::Medium
        );
        assert_eq!(
            normalize_auto_route_effort_for_provider(
                ProviderKind::OpenaiCodex,
                ReasoningEffort::Off
            ),
            ReasoningEffort::Low
        );
    }

    #[test]
    fn configured_route_effort_normalizer_keeps_kimi_code_low_medium_local() {
        let mut config = {
            let __cw_config_init_3_0 = Some("moonshot".to_string());
            let __cw_config_init_3_1 = Some(codewhale_runtime::config::ProvidersConfig {
                moonshot: codewhale_runtime::config::ProviderConfig {
                    base_url: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL
                            .to_string(),
                    ),
                    model: Some("k3".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_3_value: Config = Default::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        };
        assert_eq!(
            normalize_auto_route_effort_for_configured_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot),
                "k3",
                ReasoningEffort::Low,
            ),
            ReasoningEffort::Low
        );
        assert_eq!(
            normalize_auto_route_effort_for_configured_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot),
                "k3",
                ReasoningEffort::Medium,
            ),
            ReasoningEffort::Medium
        );

        config
            .providers
            .as_mut()
            .expect("providers")
            .moonshot
            .base_url =
            Some(codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string());
        assert_eq!(
            normalize_auto_route_effort_for_configured_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot),
                "k3",
                ReasoningEffort::Low,
            ),
            ReasoningEffort::High
        );
    }

    #[test]
    fn inventory_auto_route_recommendation_requires_runnable_pair() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_4_0 = Some("zai".to_string());
            let __cw_config_init_4_1 =
                Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
            let mut __cw_config_init_4_value: Config = Default::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value.default_text_model = __cw_config_init_4_1;
            __cw_config_init_4_value
        };
        let inventory = ModelInventory::from_config(&config).unwrap();

        let route = parse_inventory_auto_route_recommendation(
            r#"{"provider":"zai","model":"GLM-5.2","thinking":"max"}"#,
            &inventory,
        )
        .expect("valid inventory route should parse");
        assert_eq!(route.provider.provider, ProviderKind::Zai);
        assert_eq!(
            route.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
        );
        assert_eq!(route.reasoning_effort, Some(ReasoningEffort::Max));

        assert!(
            parse_inventory_auto_route_recommendation(
                r#"{"provider":"zai","model":"deepseek-v4-pro","thinking":"max"}"#,
                &inventory,
            )
            .is_none(),
            "router must not pair a DeepSeek model with the Z.ai provider"
        );

        let wrapped = parse_inventory_auto_route_recommendation(
            r#"route: {"provider":"zai","model":"GLM-5-Turbo","reasoning_effort":"medium"}"#,
            &inventory,
        )
        .expect("wrapped inventory route should parse");
        assert_eq!(wrapped.provider.provider, ProviderKind::Zai);
        assert_eq!(
            wrapped.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL
        );
        // Parsing is strict and literal; the historic Medium->High coercion
        // is applied downstream by normalize_auto_route_selection_for_config
        // so route-specific contracts (Kimi Code K3) can keep Medium.
        assert_eq!(wrapped.reasoning_effort, Some(ReasoningEffort::Medium));
    }

    #[test]
    fn inventory_auto_route_recommendation_rejects_unready_candidate() {
        let _env_lock = crate::test_support::lock_test_env();
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_5_0 = Some("zai".to_string());
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value
        };
        let mut inventory = ModelInventory::from_config(&config).unwrap();
        let candidate = inventory
            .candidates
            .iter_mut()
            .find(|candidate| {
                candidate.provider == ProviderKind::Zai
                    && candidate.model
                        == codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
            })
            .expect("Z.ai strong candidate");
        candidate.readiness = crate::provider_readiness::ResolvedProviderReadiness::InvalidRoute;

        assert!(
            parse_inventory_auto_route_recommendation(
                r#"{"provider":"zai","model":"GLM-5.2","thinking":"max"}"#,
                &inventory,
            )
            .is_none(),
            "classifier output must not revive an unsupported route"
        );
    }

    #[test]
    fn inventory_auto_route_recommendation_accepts_wanjie_v4_ids() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _wanjie = crate::test_support::EnvVarGuard::set("WANJIE_ARK_API_KEY", "wanjie-key");
        let config = {
            let __cw_config_init_6_0 = Some("wanjie-ark".to_string());
            let mut __cw_config_init_6_value: Config = Default::default();
            __cw_config_init_6_value.provider = __cw_config_init_6_0;
            __cw_config_init_6_value
        };
        let inventory = ModelInventory::from_config(&config).unwrap();

        let route = parse_inventory_auto_route_recommendation(
            r#"{"provider":"wanjie-ark","model":"deepseek-v4-pro","thinking":"max"}"#,
            &inventory,
        )
        .expect("Wanjie V4 Pro inventory route should parse");
        assert_eq!(route.provider.provider, ProviderKind::WanjieArk);
        assert_eq!(route.model, "deepseek-v4-pro");
        assert_eq!(route.reasoning_effort, Some(ReasoningEffort::Max));

        let route = parse_inventory_auto_route_recommendation(
            r#"{"provider":"wanjie-ark","model":"deepseek-v4-flash","thinking":"off"}"#,
            &inventory,
        )
        .expect("Wanjie V4 Flash inventory route should parse");
        assert_eq!(route.provider.provider, ProviderKind::WanjieArk);
        assert_eq!(route.model, "deepseek-v4-flash");
        assert_eq!(route.reasoning_effort, Some(ReasoningEffort::Off));
    }

    #[test]
    fn explicit_route_to_nonactive_provider_uses_that_providers_effort() {
        // Active provider is DeepSeek (whose effort floor is low/medium), but the
        // explicit model `GLM-5.2` only routes to Z.ai. The resolved effort must
        // be normalized for Z.ai — not left at DeepSeek's raw `low` setting.
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_7_0 = Some("deepseek".to_string());
            let __cw_config_init_7_1 = Some("low".to_string());
            let mut __cw_config_init_7_value: Config = Default::default();
            __cw_config_init_7_value.provider = __cw_config_init_7_0;
            __cw_config_init_7_value.reasoning_effort = __cw_config_init_7_1;
            __cw_config_init_7_value
        };

        let route = resolve_explicit_route_with_inventory(&config, "GLM-5.2")
            .expect("explicit GLM route should resolve to its provider");

        assert_eq!(
            route.provider.provider,
            ProviderKind::Zai,
            "GLM-5.2 must route to Z.ai, not the active DeepSeek provider"
        );
        assert_eq!(
            route.reasoning_effort,
            Some(ReasoningEffort::High),
            "low must be normalized up to high for the Z.ai route, not passed through"
        );

        // GLM-5.3 is the default and a first-class route: same provider
        // ownership, same effort normalization, and it resolves to its own id.
        let route_53 = resolve_explicit_route_with_inventory(&config, "GLM-5.3")
            .expect("explicit GLM-5.3 route should resolve to its provider");
        assert_eq!(
            route_53.provider.provider,
            ProviderKind::Zai,
            "GLM-5.3 must route to Z.ai, not the active DeepSeek provider"
        );
        assert_eq!(
            route_53.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_MODEL,
            "GLM-5.3 must resolve to its own id"
        );
        // GLM-5.3 publishes its own ladder (low/high/max) instead of 5.2's, so
        // low reaches the wire (#6396).
        assert_eq!(route_53.reasoning_effort, Some(ReasoningEffort::Low));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn inventory_auto_route_resolves_active_authenticated_provider() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_8_0 = Some("zai".to_string());
            let mut __cw_config_init_8_value: Config = Default::default();
            __cw_config_init_8_value.provider = __cw_config_init_8_0;
            __cw_config_init_8_value
        };

        // #6290 rework: without the flash classifier there is no
        // per-request signal, so every wording resolves the same declared
        // default — the short chat and the complex ask below must agree.
        for prompt in [
            "quick status check",
            "please refactor this architecture and audit its security boundaries",
        ] {
            let route = resolve_auto_route_with_inventory(&config, prompt, "", "auto", "auto")
                .await
                .expect("inventory route should resolve with authenticated active provider");

            assert_eq!(route.provider.provider, ProviderKind::Zai);
            assert_eq!(
                route.model,
                codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
            );
            assert_eq!(route.source, AutoRouteSource::Heuristic);
            let receipt = route.receipt.expect("Auto route receipt");
            assert_eq!(receipt.tier, AutoRouteTier::Strong);
            assert_eq!(receipt.scope, AutoRouteScope::ResolvedProvider);
            assert_eq!(receipt.data_path, AutoRouteDataPath::LocalHeuristic);
            assert_eq!(
                receipt.reason,
                AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::DeclaredDefault),
                "prompt {prompt:?} must take the declared default, not a content judgment"
            );
            assert_eq!(
                receipt.pair.strong,
                codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
            );
            assert_eq!(
                receipt.pair.fast.as_deref(),
                Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_FLASH_MODEL)
            );
        }
    }

    #[test]
    fn classifier_receipt_discloses_active_provider_scope_and_data_path() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_9_0 = Some("zai".to_string());
            let mut __cw_config_init_9_value: Config = Default::default();
            __cw_config_init_9_value.provider = __cw_config_init_9_0;
            __cw_config_init_9_value
        };
        let inventory = ModelInventory::from_config(&config).unwrap();
        let recommendation = parse_inventory_auto_route_recommendation(
            r#"{"provider":"zai","model":"GLM-5-Turbo","thinking":"off"}"#,
            &inventory,
        )
        .expect("runnable classifier recommendation");

        let route = auto_route_from_classifier(&inventory, recommendation);

        assert_eq!(route.provider.provider, ProviderKind::Zai);
        assert_eq!(
            route.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL
        );
        assert_eq!(route.source, AutoRouteSource::FlashRouter);
        let receipt = route.receipt.expect("classifier receipt");
        assert_eq!(receipt.tier, AutoRouteTier::Fast);
        // #4411: the classifier only saw Z.ai routes, so the receipt says so
        // instead of claiming the wider runnable-providers scope.
        assert_eq!(receipt.scope, AutoRouteScope::ActiveProvider);
        assert_eq!(
            receipt.data_path,
            AutoRouteDataPath::Classifier {
                provider: "deepseek".into(),
                provider_kind: ProviderKind::Deepseek,
                model: "deepseek-flash".to_string(),
            }
        );
        assert_eq!(receipt.reason, AutoRouteReason::ClassifierRecommendation);
    }

    #[test]
    fn classifier_recommendation_for_another_provider_is_refused_by_default() {
        // #4411: the payload never named DeepSeek, but a classifier can still
        // emit one. The recommendation must not become a route unless the
        // persisted cross-provider opt-in is set.
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let scoped = {
            let __cw_config_init_10_0 = Some("zai".to_string());
            let mut __cw_config_init_10_value: Config = Default::default();
            __cw_config_init_10_value.provider = __cw_config_init_10_0;
            __cw_config_init_10_value
        };
        let scoped_inventory = ModelInventory::from_config(&scoped).unwrap();
        let raw = r#"{"provider":"deepseek","model":"deepseek-v4-flash","thinking":"off"}"#;

        assert!(
            parse_inventory_auto_route_recommendation(raw, &scoped_inventory).is_none(),
            "cross-provider classifier output must be refused by default"
        );
        // The same inventory still accepts an in-scope active-provider route.
        assert!(
            parse_inventory_auto_route_recommendation(
                r#"{"provider":"zai","model":"GLM-5.2","thinking":"max"}"#,
                &scoped_inventory,
            )
            .is_some()
        );

        let opted_in = {
            let __cw_config_init_11_0 = Some(codewhale_runtime::config::AutoConfig {
                cost_saving: None,
                cross_provider: Some(true),
                router: None,
            });
            let mut __cw_config_init_11_value: Config = scoped.clone();
            __cw_config_init_11_value.auto = __cw_config_init_11_0;
            __cw_config_init_11_value
        };
        let opted_in_route = parse_inventory_auto_route_recommendation(
            raw,
            &ModelInventory::from_config(&opted_in).unwrap(),
        )
        .expect("opt-in admits the cross-provider recommendation");
        assert_eq!(opted_in_route.provider.provider, ProviderKind::Deepseek);
    }

    #[test]
    fn classifier_prompt_declares_active_provider_scope_by_default() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_12_0 = Some("zai".to_string());
            let mut __cw_config_init_12_value: Config = Default::default();
            __cw_config_init_12_value.provider = __cw_config_init_12_0;
            __cw_config_init_12_value
        };

        let prompt = inventory_auto_router_system_prompt(
            &ModelInventory::from_config(&config).unwrap(),
            false,
        );

        assert!(
            prompt.contains("Auto routing is scoped to the active provider `zai`"),
            "{prompt}"
        );
        assert!(!prompt.contains("deepseek"), "{prompt}");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn active_provider_declared_default_survives_scoping() {
        // #4411: scoping keeps Auto on the active provider. #6290 rework:
        // without the flash classifier there is no per-request tier signal,
        // so both wordings resolve the same declared default on Zai.
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_13_0 = Some("zai".to_string());
            let mut __cw_config_init_13_value: Config = Default::default();
            __cw_config_init_13_value.provider = __cw_config_init_13_0;
            __cw_config_init_13_value
        };

        for prompt in [
            "refactor the routing module and audit its security boundaries",
            "hi",
        ] {
            let route = resolve_auto_route_with_inventory(&config, prompt, "", "auto", "auto")
                .await
                .expect("scoped Auto route");
            assert_eq!(route.provider.provider, ProviderKind::Zai);
            assert_eq!(
                route.model,
                codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
            );
            let receipt = route.receipt.expect("scoped receipt");
            assert_eq!(receipt.tier, AutoRouteTier::Strong);
            assert_eq!(receipt.scope, AutoRouteScope::ResolvedProvider);
            assert_eq!(
                receipt.reason,
                AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::DeclaredDefault),
                "prompt {prompt:?}"
            );
        }
    }

    #[test]
    fn config_auto_cross_provider_defaults_to_false() {
        assert!(!Config::default().auto_cross_provider());
        let opted_in = {
            let __cw_config_init_14_0 = Some(codewhale_runtime::config::AutoConfig {
                cost_saving: None,
                cross_provider: Some(true),
                router: None,
            });
            let mut __cw_config_init_14_value: Config = Default::default();
            __cw_config_init_14_value.auto = __cw_config_init_14_0;
            __cw_config_init_14_value
        };
        assert!(opted_in.auto_cross_provider());
    }

    #[test]
    fn classifier_receipt_never_reports_openrouter_default_for_another_family() {
        let _env_lock = crate::test_support::lock_test_env();
        let _openrouter =
            crate::test_support::EnvVarGuard::set("OPENROUTER_API_KEY", "openrouter-key");
        let config = {
            let __cw_config_init_15_0 = Some("openrouter".to_string());
            let mut __cw_config_init_15_value: Config = Default::default();
            __cw_config_init_15_value.provider = __cw_config_init_15_0;
            __cw_config_init_15_value
        };
        let inventory = ModelInventory::from_config(&config).unwrap();
        let recommendation = parse_inventory_auto_route_recommendation(
            r#"{"provider":"openrouter","model":"z-ai/glm-5.2","thinking":"max"}"#,
            &inventory,
        )
        .expect("runnable non-default OpenRouter family");

        let route = auto_route_from_classifier(&inventory, recommendation);
        let receipt = route.receipt.expect("classifier receipt");

        assert_eq!(
            route.model,
            codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_2_MODEL
        );
        assert_eq!(
            receipt.pair.strong,
            codewhale_config::catalog::reviewed::constants::OPENROUTER_GLM_5_2_MODEL
        );
        assert_ne!(
            receipt.pair.fast.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_FLASH_MODEL),
            "a GLM selection must not be described as the DeepSeek default pair"
        );
        assert!(matches!(
            receipt.tier,
            AutoRouteTier::Strong | AutoRouteTier::Only
        ));
    }

    #[test]
    fn classifier_fallback_preserves_attempted_data_path_without_error_text() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_16_0 = Some("zai".to_string());
            let mut __cw_config_init_16_value: Config = Default::default();
            __cw_config_init_16_value.provider = __cw_config_init_16_0;
            __cw_config_init_16_value
        };
        let inventory = ModelInventory::from_config(&config).unwrap();
        let fallback = auto_route_declared_fallback(&config, &inventory);

        let route = auto_route_classifier_fallback(fallback, &inventory);

        assert_eq!(route.source, AutoRouteSource::Heuristic);
        let receipt = route.receipt.expect("fallback receipt");
        assert_eq!(receipt.scope, AutoRouteScope::ResolvedProvider);
        assert!(matches!(
            receipt.data_path,
            AutoRouteDataPath::Classifier {
                ref provider,
                ref model,
                ..
            } if provider.as_str() == "deepseek" && model == "deepseek-flash"
        ));
        assert_eq!(
            receipt.reason,
            AutoRouteReason::ClassifierFallback(AutoRouteHeuristicReason::DeclaredDefault)
        );
        assert!(!receipt.reason.label().contains("secret-provider-error"));
    }

    #[test]
    fn pre_rework_receipt_shape_still_deserializes() {
        // Sessions saved before the #6290 rework persist
        // `local_heuristic` + content-derived reasons. They must keep
        // loading: the wrapper arrives via serde alias, the legacy reasons
        // are retained variants.
        let reason: AutoRouteReason =
            serde_json::from_str(r#"{"local_heuristic":"complex_request"}"#)
                .expect("pre-rework receipt reason deserializes");
        assert_eq!(
            reason,
            AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::ComplexRequest)
        );
        let receipt: AutoRouteReceipt = serde_json::from_str(
            r#"{"tier":"fast","pair":{"strong":"GLM-5.3","fast":"GLM-5.3-Flash"},"scope":"resolved_provider","data_path":"local_heuristic","reason":{"local_heuristic":"short_request"}}"#,
        )
        .expect("pre-rework receipt deserializes");
        assert_eq!(receipt.reason.label(), "local fallback: short request");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn inventory_auto_route_never_falls_back_across_providers_by_default() {
        // #4411: the active provider has no usable credential, but another
        // provider does. Auto must stay on the active provider and report a
        // no-runnable-candidate fallback instead of silently spending the
        // other provider's key.
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::remove("ZAI_API_KEY");
        let config = {
            let __cw_config_init_17_0 = Some("zai".to_string());
            let mut __cw_config_init_17_value: Config = Default::default();
            __cw_config_init_17_value.provider = __cw_config_init_17_0;
            __cw_config_init_17_value
        };

        let route =
            resolve_auto_route_with_inventory(&config, "quick status check", "", "auto", "auto")
                .await
                .expect("inventory route should resolve without leaving the active provider");

        assert_eq!(route.provider.provider, ProviderKind::Zai);
        assert_ne!(route.provider.provider, ProviderKind::Deepseek);
        assert_eq!(route.source, AutoRouteSource::Heuristic);
        let receipt = route.receipt.expect("Auto route receipt");
        assert_eq!(receipt.scope, AutoRouteScope::ResolvedProvider);
        assert_eq!(
            receipt.reason,
            AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::NoRunnableCandidate)
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn inventory_auto_route_crosses_providers_only_under_persisted_opt_in() {
        // The same configuration as above, plus the persisted
        // `[auto] cross_provider = true` opt-in (#4411).
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let _zai = crate::test_support::EnvVarGuard::remove("ZAI_API_KEY");
        let config = {
            let __cw_config_init_18_0 = Some("zai".to_string());
            let __cw_config_init_18_1 = Some(codewhale_runtime::config::AutoConfig {
                cost_saving: None,
                cross_provider: Some(true),
                router: None,
            });
            let mut __cw_config_init_18_value: Config = Default::default();
            __cw_config_init_18_value.provider = __cw_config_init_18_0;
            __cw_config_init_18_value.auto = __cw_config_init_18_1;
            __cw_config_init_18_value
        };

        let route =
            resolve_auto_route_with_inventory(&config, "quick status check", "", "auto", "auto")
                .await
                .expect("opted-in route should fall back to an authenticated provider");

        assert_eq!(route.provider.provider, ProviderKind::Deepseek);
        assert_eq!(route.model, "deepseek-flash");
        assert_eq!(route.source, AutoRouteSource::Heuristic);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn inventory_auto_route_cost_saving_pins_fast_sibling() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let balanced = {
            let __cw_config_init_19_0 = Some("zai".to_string());
            let mut __cw_config_init_19_value: Config = Default::default();
            __cw_config_init_19_value.provider = __cw_config_init_19_0;
            __cw_config_init_19_value
        };
        let cost_saving = {
            let __cw_config_init_20_0 = Some(codewhale_runtime::config::AutoConfig {
                cost_saving: Some(true),
                cross_provider: None,
                router: None,
            });
            let mut __cw_config_init_20_value: Config = balanced.clone();
            __cw_config_init_20_value.auto = __cw_config_init_20_0;
            __cw_config_init_20_value
        };

        let balanced_route = resolve_auto_route_with_inventory(
            &balanced,
            "Please implement a binary search",
            "",
            "auto",
            "auto",
        )
        .await
        .expect("balanced Auto route should resolve");
        let cost_saving_route = resolve_auto_route_with_inventory(
            &cost_saving,
            "Please implement a binary search",
            "",
            "auto",
            "auto",
        )
        .await
        .expect("cost-saving Auto route should resolve");

        assert_eq!(balanced_route.provider.provider, ProviderKind::Zai);
        assert_eq!(
            balanced_route.model,
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
        );
        assert_eq!(cost_saving_route.provider.provider, ProviderKind::Zai);
        assert_eq!(
            cost_saving_route.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_FLASH_MODEL
        );
        assert_eq!(cost_saving_route.source, AutoRouteSource::Heuristic);
        assert_eq!(
            balanced_route
                .receipt
                .as_ref()
                .map(|receipt| (receipt.tier, receipt.reason)),
            Some((
                AutoRouteTier::Strong,
                AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::DeclaredDefault),
            ))
        );
        assert_eq!(
            cost_saving_route
                .receipt
                .as_ref()
                .map(|receipt| (receipt.tier, receipt.reason)),
            Some((
                AutoRouteTier::Fast,
                AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::CostSavingPolicy),
            ))
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn inventory_auto_route_uses_wanjie_v4_pair_without_deepseek_router() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _wanjie = crate::test_support::EnvVarGuard::set("WANJIE_ARK_API_KEY", "wanjie-key");
        let config = {
            let __cw_config_init_21_0 = Some("wanjie-ark".to_string());
            let __cw_config_init_21_1 = Some("auto".to_string());
            let mut __cw_config_init_21_value: Config = Default::default();
            __cw_config_init_21_value.provider = __cw_config_init_21_0;
            __cw_config_init_21_value.default_text_model = __cw_config_init_21_1;
            __cw_config_init_21_value
        };

        // #6290 rework: no classifier, no content signal — both wordings
        // take the declared Wanjie default.
        for prompt in ["quick status check", "please refactor this architecture"] {
            let route = resolve_auto_route_with_inventory(&config, prompt, "", "auto", "auto")
                .await
                .expect("declared-default Wanjie route should resolve");
            assert_eq!(route.provider.provider, ProviderKind::WanjieArk);
            assert_eq!(route.model, "deepseek-reasoner");
            assert_eq!(route.source, AutoRouteSource::Heuristic);
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn inventory_auto_route_uses_volcengine_v4_pair_without_deepseek_router() {
        let _env_lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _volcengine =
            crate::test_support::EnvVarGuard::set("VOLCENGINE_API_KEY", "volcengine-key");
        let config = {
            let __cw_config_init_22_0 = Some("volcengine".to_string());
            let __cw_config_init_22_1 = Some("auto".to_string());
            let mut __cw_config_init_22_value: Config = Default::default();
            __cw_config_init_22_value.provider = __cw_config_init_22_0;
            __cw_config_init_22_value.default_text_model = __cw_config_init_22_1;
            __cw_config_init_22_value
        };

        // #6290 rework: no classifier, no content signal — both wordings
        // take the declared Volcengine default.
        for prompt in ["quick status check", "please refactor this architecture"] {
            let route = resolve_auto_route_with_inventory(&config, prompt, "", "auto", "auto")
                .await
                .expect("declared-default Volcengine route should resolve");
            assert_eq!(route.provider.provider, ProviderKind::Volcengine);
            assert_eq!(route.model, "deepseek-v4-pro");
            assert_eq!(route.source, AutoRouteSource::Heuristic);
        }
    }

    #[test]
    fn provider_router_candidates_cover_known_provider_classes() {
        use codewhale_config::ProviderKind;

        let deepseek = provider_router_candidates(ProviderKind::Deepseek, "deepseek-v4-pro");
        assert_eq!(deepseek.big, "deepseek-v4-pro");
        assert_eq!(deepseek.cheap.as_deref(), Some("deepseek-flash"));

        let openrouter =
            provider_router_candidates(ProviderKind::Openrouter, "deepseek/deepseek-v4-pro");
        assert_eq!(openrouter.big, "deepseek/deepseek-v4-pro");
        assert_eq!(
            openrouter.cheap.as_deref(),
            Some("deepseek/deepseek-v4-flash")
        );

        let wanjie = provider_router_candidates(ProviderKind::WanjieArk, "deepseek-reasoner");
        assert_eq!(wanjie.big, "deepseek-v4-pro");
        assert_eq!(wanjie.cheap.as_deref(), Some("deepseek-v4-flash"));

        let volcengine = provider_router_candidates(ProviderKind::Volcengine, "DeepSeek-V4-Pro");
        assert_eq!(volcengine.big, "DeepSeek-V4-Pro");
        assert_eq!(volcengine.cheap.as_deref(), Some("DeepSeek-V4-Flash"));

        let zai = provider_router_candidates(ProviderKind::Zai, "GLM-5.2");
        assert_eq!(zai.big, "GLM-5.2");
        // GLM-5.2 faster/explore children route to GLM-5-Turbo (same-family fast
        // sibling), not back down to GLM-5.1.
        assert_eq!(zai.cheap.as_deref(), Some("GLM-5-Turbo"));

        let openrouter_glm = provider_router_candidates(ProviderKind::Openrouter, "z-ai/glm-5.2");
        assert_eq!(openrouter_glm.big, "z-ai/glm-5.2");
        assert_eq!(openrouter_glm.cheap.as_deref(), Some("z-ai/glm-5-turbo"));

        // GLM-5.3's fast sibling is Flash; GLM-5.2 still uses Turbo.
        let zai_53 = provider_router_candidates(ProviderKind::Zai, "GLM-5.3");
        assert_eq!(zai_53.big, "GLM-5.3");
        assert_eq!(zai_53.cheap.as_deref(), Some("GLM-5.3-Flash"));

        let openrouter_glm_53 =
            provider_router_candidates(ProviderKind::Openrouter, "z-ai/glm-5.3");
        assert_eq!(openrouter_glm_53.big, "z-ai/glm-5.3");
        assert_eq!(
            openrouter_glm_53.cheap.as_deref(),
            Some("z-ai/glm-5.3-flash")
        );

        let zai_flash = provider_router_candidates(ProviderKind::Zai, "GLM-5.3-Flash");
        assert_eq!(zai_flash.big, "GLM-5.3-Flash");
        assert_eq!(zai_flash.cheap, None);

        // GLM-5.1 has no cheaper tier; faster children stay on the parent.
        let zai_51 = provider_router_candidates(ProviderKind::Zai, "GLM-5.1");
        assert_eq!(zai_51.big, "GLM-5.1");
        assert_eq!(zai_51.cheap, None);

        // GLM-5-Turbo is itself the cheap tier; no further downgrade.
        let zai_turbo = provider_router_candidates(ProviderKind::Zai, "GLM-5-Turbo");
        assert_eq!(zai_turbo.big, "GLM-5-Turbo");
        assert_eq!(zai_turbo.cheap, None);

        // Providers without a known cheap tier: big = session model, no cheap.
        let ollama = provider_router_candidates(ProviderKind::Ollama, "qwen3:32b");
        assert_eq!(ollama.big, "qwen3:32b");
        assert_eq!(ollama.cheap, None);

        let moonshot = provider_router_candidates(ProviderKind::Moonshot, "kimi-k2.6");
        assert_eq!(moonshot.big, "kimi-k2.6");
        assert_eq!(moonshot.cheap, None);
    }

    #[test]
    fn provider_router_candidates_cover_catalog_fast_siblings() {
        use codewhale_config::ProviderKind;

        let cases = [
            (ProviderKind::OpenaiCodex, "gpt-5.6-sol", "gpt-5.6-luna"),
            (
                ProviderKind::Anthropic,
                "claude-sonnet-4-6",
                "claude-haiku-4-5",
            ),
            (ProviderKind::XiaomiMimo, "mimo-v2.6-pro", "mimo-v2.6-flash"),
            (
                ProviderKind::Arcee,
                "trinity-large-thinking",
                "trinity-mini",
            ),
            (ProviderKind::Moonshot, "kimi-k2.7-code", "kimi-k2.6"),
            (
                ProviderKind::Minimax,
                "MiniMax-M2.7",
                "MiniMax-M2.7-highspeed",
            ),
            (ProviderKind::OpencodeGo, "kimi-k3", "kimi-k2.7-code"),
            (
                ProviderKind::Openrouter,
                "qwen/qwen3.6-max-preview",
                "qwen/qwen3.6-flash",
            ),
            (
                ProviderKind::Openrouter,
                "anthropic/claude-sonnet-4-6",
                "anthropic/claude-haiku-4-5",
            ),
        ];

        for (provider, strong, fast) in cases {
            let candidates = provider_router_candidates(provider, strong);
            assert_eq!(candidates.big, strong);
            assert_eq!(candidates.cheap.as_deref(), Some(fast));
            assert_eq!(
                provider_router_candidates(provider, fast).cheap,
                None,
                "already-fast model must not downgrade again: {provider:?}/{fast}"
            );
        }

        for (provider, model) in [
            (ProviderKind::Ollama, "qwen3:32b"),
            (ProviderKind::Custom, "gpt-5.6-sol"),
            (ProviderKind::OpenaiCodex, "gpt-5.6-luna"),
        ] {
            assert_eq!(provider_router_candidates(provider, model).cheap, None);
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn declared_fallback_without_cheap_tier_stays_on_default_model() {
        // #3018 AC: Ollama + auto must never fabricate a DeepSeek id. The
        // declared fallback returns the configured default verbatim, so no
        // sibling id can be invented regardless of request wording.
        let _env_lock = crate::test_support::lock_test_env();
        let config = {
            let __cw_config_init_23_0 = Some("ollama".to_string());
            let mut __cw_config_init_23_value: Config = Default::default();
            __cw_config_init_23_value.provider = __cw_config_init_23_0;
            __cw_config_init_23_value
        };
        for prompt in ["hi", "please refactor the auth module for security"] {
            let route = resolve_auto_route_with_inventory(&config, prompt, "", "auto", "auto")
                .await
                .expect("ollama Auto route should resolve");
            assert_eq!(route.provider.provider, ProviderKind::Ollama);
            assert_eq!(route.model, config.default_model());
            assert!(
                !route.model.to_ascii_lowercase().contains("deepseek"),
                "no DeepSeek id may be fabricated: {}",
                route.model
            );
        }
    }

    #[test]
    fn config_auto_cost_saving_defaults_to_false() {
        let cfg = Config::default();
        assert!(!cfg.auto_cost_saving());
    }

    #[test]
    fn config_auto_cost_saving_reads_table() {
        let cfg = {
            let __cw_config_init_24_0 = Some(codewhale_runtime::config::AutoConfig {
                cost_saving: Some(true),
                cross_provider: None,
                router: None,
            });
            let mut __cw_config_init_24_value: Config = Default::default();
            __cw_config_init_24_value.auto = __cw_config_init_24_0;
            __cw_config_init_24_value
        };
        assert!(cfg.auto_cost_saving());
    }
}

#[cfg(test)]
mod decision_router_tests {
    //! `[auto.router] kind = "decision"` (#6525) against wiremock. The
    //! resolver's `cfg!(any(test, feature = "test-support"))` short-circuit stays; these call the router path
    //! (`auto_route_via_router`) and its pure policy directly.

    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const MARKER: &str = "PROVIDER-BODY-MARKER-must-not-leak";

    /// Fields drop in declaration order: restore the environment before the
    /// lock is released, or another test observes our overrides.
    struct Env {
        _guards: Vec<crate::test_support::EnvVarGuard>,
        _home: tempfile::TempDir,
        _lock: crate::test_support::TestEnvLock,
    }

    fn hermetic_env() -> Env {
        let lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("test home");
        let guards = vec![
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path()),
            crate::test_support::EnvVarGuard::remove("OPENROUTER_API_KEY"),
            crate::test_support::EnvVarGuard::remove("TYPESAFE_API_KEY"),
        ];
        Env {
            _guards: guards,
            _home: home,
            _lock: lock,
        }
    }

    /// Active DeepSeek (pro/flash pair runnable) with an OpenRouter decision
    /// router pointed at `openrouter_base`.
    fn decision_config(openrouter_base: &str, cost_saving: bool, timeout_secs: u64) -> Config {
        {
            let __cw_config_init_25_0 = Some("deepseek".to_string());
            let __cw_config_init_25_1 = Some("deepseek-v4-pro".to_string());
            let __cw_config_init_25_2 = Some(codewhale_runtime::config::ProvidersConfig {
                deepseek: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("ds-test-key".to_string()),
                    ..Default::default()
                },
                openrouter: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("or-test-key".to_string()),
                    base_url: Some(openrouter_base.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let __cw_config_init_25_3 = Some(codewhale_runtime::config::AutoConfig {
                cost_saving: Some(cost_saving),
                cross_provider: None,
                router: Some(codewhale_runtime::config::AutoRouterConfig {
                    kind: Some("decision".to_string()),
                    provider: Some("openrouter".to_string()),
                    model: Some("typesafe/jev-1.13".to_string()),
                    timeout_secs: Some(timeout_secs),
                    ..Default::default()
                }),
            });
            let mut __cw_config_init_25_value: Config = Default::default();
            __cw_config_init_25_value.provider = __cw_config_init_25_0;
            __cw_config_init_25_value.default_text_model = __cw_config_init_25_1;
            __cw_config_init_25_value.providers = __cw_config_init_25_2;
            __cw_config_init_25_value.auto = __cw_config_init_25_3;
            __cw_config_init_25_value
        }
    }

    const TYPESAFE_TEST_KEY: &str = "tsbarekey0123456789";

    fn answer_body(strong: f64, confidence: f64) -> serde_json::Value {
        serde_json::json!({
            "id": "gen-dec-1",
            "model": "typesafe/jev-1.13-20260917",
            "provider": "TypeSafe",
            "answers": {
                "tier": {
                    "type": "choice",
                    "choice": if strong >= 0.5 { "strong" } else { "fast" },
                    "probabilities": { "fast": 1.0 - strong, "strong": strong },
                    "confidence": confidence,
                },
                "thinking": {
                    "type": "choice",
                    "choice": "max",
                    "probabilities": { "off": 0.02, "high": 0.21, "max": 0.77 },
                    "confidence": 0.66,
                },
            },
            "usage": { "cost": 0.000019992, "input_tokens": 476, "output_tokens": 70 },
        })
    }

    async fn route(config: &Config, latest: &str, context: &str) -> AutoRouteSelection {
        let inventory = ModelInventory::from_config(config).unwrap();
        assert!(
            inventory.router_available,
            "decision router must be available"
        );
        auto_route_via_router(
            config, &inventory, latest, context, "agent", "auto", "auto", false,
        )
        .await
    }

    fn receipt(selection: &AutoRouteSelection) -> &AutoRouteReceipt {
        selection.receipt.as_ref().expect("auto receipt")
    }

    #[tokio::test]
    async fn decision_request_has_pinned_shape_and_redacted_state() {
        let _env = hermetic_env();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/alpha/decisions"))
            .and(header("authorization", "Bearer or-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer_body(0.82, 0.64)))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = decision_config(&format!("{}/api/v1", server.uri()), false, 2);
        // A configured credential that a tool result echoed back.
        let secret = "cw-router-secret-should-never-leave-process";
        config
            .providers
            .as_mut()
            .expect("providers")
            .deepseek
            .api_key = Some(secret.to_string());
        let selection = route(
            &config,
            "Refactor the parser across files",
            &format!("assistant: [tool result] token={secret}"),
        )
        .await;
        assert_eq!(selection.model, "deepseek-v4-pro");

        let requests: Vec<Request> = server.received_requests().await.expect("recorded");
        assert_eq!(requests.len(), 1);
        let raw = String::from_utf8(requests[0].body.clone()).expect("utf8 body");
        assert!(
            !raw.contains(secret),
            "secret leaked into the decision state"
        );
        let body: serde_json::Value = serde_json::from_str(&raw).expect("json body");
        assert_eq!(body["model"], "typesafe/jev-1.13");
        let state_keys: Vec<&str> = body["state"]
            .as_object()
            .expect("state object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            state_keys,
            [
                "session_mode",
                "selected_thinking_mode",
                "recent_context",
                "latest_request"
            ]
        );
        // The criteria are a product surface: pin the exact wording.
        assert_eq!(
            body["questions"],
            serde_json::json!({
                "tier": {
                    "type": "choice",
                    "instructions": "Which model tier should handle the latest request in this coding-agent session?",
                    "criteria": {
                        "fast": {
                            "what": "A fast, cheaper model. Right for questions, explanations, lookups, small single-file edits, formatting, and routine follow-ups.",
                            "not_for": "Multi-step agentic work, debugging across files, architecture or design, security review, release work."
                        },
                        "strong": {
                            "what": "The strongest model. Right for multi-step agentic coding, multi-file changes, debugging, architecture or design, security review, release work, or anything the fast tier would likely get wrong.",
                            "not_for": "Trivial questions or one-line edits."
                        }
                    }
                },
                "thinking": {
                    "type": "choice",
                    "instructions": "How much reasoning should the chosen model spend on the latest request?",
                    "criteria": {
                        "off": "A trivial answer with no tools and no reasoning needed.",
                        "high": "Ordinary reasoning: a normal coding or explanation task.",
                        "max": "Agentic, multi-file, debugging, architecture, security, release, or uncertain work."
                    }
                }
            })
        );
    }

    #[tokio::test]
    async fn confident_strong_decision_routes_strong_with_evidence_and_usage() {
        let _env = hermetic_env();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/alpha/decisions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer_body(0.82, 0.64)))
            .mount(&server)
            .await;
        let config = decision_config(&format!("{}/api/v1", server.uri()), false, 2);
        let selection = route(&config, "Debug the release pipeline", "").await;

        assert_eq!(selection.provider.provider, ProviderKind::Deepseek);
        assert_eq!(selection.model, "deepseek-v4-pro");
        assert_eq!(selection.source, AutoRouteSource::FlashRouter);
        assert_eq!(selection.reasoning_effort, Some(ReasoningEffort::Max));
        let receipt = receipt(&selection);
        assert_eq!(receipt.reason, AutoRouteReason::ClassifierRecommendation);
        assert_eq!(receipt.tier, AutoRouteTier::Strong);
        assert_eq!(receipt.scope, AutoRouteScope::ActiveProvider);
        assert_eq!(
            receipt.data_path,
            AutoRouteDataPath::Decision {
                route: DecisionRouterRoute::Openrouter,
                model: "typesafe/jev-1.13".to_string(),
            }
        );
        assert_eq!(receipt.router_failure, None);
        let decision = receipt.decision.as_ref().expect("decision evidence");
        assert_eq!(decision.choice, "strong");
        assert_eq!(
            decision.probabilities_bp,
            BTreeMap::from([("fast".to_string(), 1800), ("strong".to_string(), 8200)])
        );
        assert_eq!(decision.confidence_bp, 6400);
        assert_eq!(decision.min_confidence_bp, 5000);
        assert_eq!(decision.thinking.as_deref(), Some("max"));
        assert_eq!(
            decision.provider_reported_cost_usd.as_deref(),
            Some("0.000019992")
        );
        assert_eq!(
            decision.response_model.as_deref(),
            Some("typesafe/jev-1.13-20260917")
        );
        assert_eq!(selection.routed_usage.len(), 1);
        assert_eq!(selection.routed_usage[0].usage.usage.input_tokens, 476);
        assert_eq!(selection.routed_usage[0].usage.usage.output_tokens, 70);
        assert!(
            selection.routed_usage[0]
                .source_id
                .starts_with("auto-router:")
        );
    }

    #[tokio::test]
    async fn low_confidence_takes_the_declared_fallback_and_keeps_usage() {
        let _env = hermetic_env();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/alpha/decisions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer_body(0.35, 0.3)))
            .mount(&server)
            .await;
        let config = decision_config(&format!("{}/api/v1", server.uri()), false, 2);
        let selection = route(&config, "What does this function do?", "").await;

        assert_eq!(selection.model, "deepseek-v4-pro", "declared default");
        assert_eq!(selection.source, AutoRouteSource::Heuristic);
        let receipt = receipt(&selection);
        assert_eq!(
            receipt.reason,
            AutoRouteReason::ClassifierFallback(AutoRouteHeuristicReason::LowConfidence)
        );
        assert_eq!(receipt.router_failure, None);
        let decision = receipt.decision.as_ref().expect("evidence is kept");
        assert_eq!(decision.choice, "fast");
        assert_eq!(decision.confidence_bp, 3000);
        assert_eq!(
            decision.thinking, None,
            "an unacted decision applies no effort"
        );
        assert_eq!(selection.routed_usage.len(), 1, "usage is still recorded");
    }

    #[tokio::test]
    async fn http_errors_fail_the_router_loudly_without_leaking_bodies() {
        for status in [401_u16, 402, 429, 500] {
            let _env = hermetic_env();
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/alpha/decisions"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_string(format!(r#"{{"error":{{"message":"{MARKER}"}}}}"#)),
                )
                .expect(1)
                .mount(&server)
                .await;
            let config = decision_config(&format!("{}/api/v1", server.uri()), false, 2);
            let selection = route(&config, "Explain the diff", "").await;

            assert_eq!(selection.model, "deepseek-v4-pro", "status {status}");
            let receipt = receipt(&selection);
            assert!(
                matches!(receipt.reason, AutoRouteReason::ClassifierFallback(_)),
                "status {status}"
            );
            assert_eq!(
                receipt.router_failure,
                Some(AutoRouterFailure::Http { status }),
                "status {status}"
            );
            let json = serde_json::to_string(receipt).expect("receipt json");
            assert!(!json.contains(MARKER), "status {status}: body leaked");
            assert!(!receipt.router_failure.unwrap().label().contains(MARKER));
            assert!(selection.routed_usage.is_empty());
            assert_eq!(selection.routed_usage_dropped_records, 1, "status {status}");
            assert_eq!(selection.routed_usage_drop_records.len(), 1);
        }
    }

    #[tokio::test]
    async fn dispatched_timeout_falls_back_and_marks_usage_missing() {
        let _env = hermetic_env();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/alpha/decisions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(answer_body(0.9, 0.8))
                    .set_delay(Duration::from_millis(1_600)),
            )
            .mount(&server)
            .await;
        let config = decision_config(&format!("{}/api/v1", server.uri()), false, 1);
        let selection = route(&config, "Explain the diff", "").await;

        let receipt = receipt(&selection);
        assert_eq!(receipt.router_failure, Some(AutoRouterFailure::Timeout));
        assert!(matches!(
            receipt.reason,
            AutoRouteReason::ClassifierFallback(_)
        ));
        assert!(selection.routed_usage.is_empty());
        // The POST was sent before the deadline: coverage must fail closed.
        assert_eq!(selection.routed_usage_drop_records.len(), 1);
        assert_eq!(selection.routed_usage_dropped_records, 1);
    }

    #[tokio::test]
    async fn no_fast_strong_pair_means_no_request() {
        let _env = hermetic_env();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer_body(0.9, 0.8)))
            .expect(0)
            .mount(&server)
            .await;
        let mut config = decision_config(&format!("{}/api/v1", server.uri()), false, 2);
        // A single-tier active provider: nothing for the decision to choose.
        config.provider = Some("openrouter".to_string());
        config.default_text_model = Some("synthetic/single-tier-model".to_string());
        let selection = route(&config, "Refactor everything", "").await;

        let receipt = receipt(&selection);
        assert_eq!(
            receipt.reason,
            AutoRouteReason::LocalFallback(AutoRouteHeuristicReason::NoFastSibling)
        );
        assert_eq!(receipt.data_path, AutoRouteDataPath::LocalHeuristic);
        assert!(selection.routed_usage.is_empty());

        // The setup test must say no call was made, not report a result.
        let reason = test_auto_router(&config)
            .await
            .expect_err("no test call without a strong/fast pair");
        assert_eq!(reason, AutoRouteHeuristicReason::NoFastSibling.label());
    }

    #[tokio::test]
    async fn typesafe_direct_uses_its_own_endpoint_and_key() {
        let _env = hermetic_env();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .and(header(
                "authorization",
                format!("Bearer {TYPESAFE_TEST_KEY}").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer_body(0.2, 0.6)))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = decision_config("https://openrouter.invalid/api/v1", false, 2);
        let providers = config.providers.as_mut().expect("providers");
        providers.openrouter.api_key = None;
        // From the environment: not part of any `[providers.*]` table, so
        // only the TypeSafe-specific redaction covers it.
        let _key = crate::test_support::EnvVarGuard::set("TYPESAFE_API_KEY", TYPESAFE_TEST_KEY);
        let router = config
            .auto
            .as_mut()
            .and_then(|auto| auto.router.as_mut())
            .expect("router");
        router.provider = Some("typesafe".to_string());
        router.model = Some("jev-latest".to_string());
        router.base_url = Some(format!("{}/v1", server.uri()));

        // The TypeSafe key is no chat provider's key; a bare echo of it in
        // context must still be redacted from the decision body.
        let selection = route(
            &config,
            "Rename a variable",
            &format!("assistant: [tool result] {TYPESAFE_TEST_KEY}"),
        )
        .await;
        let requests: Vec<Request> = server.received_requests().await.expect("recorded");
        let raw = String::from_utf8(requests[0].body.clone()).expect("utf8 body");
        assert!(!raw.contains(TYPESAFE_TEST_KEY), "TypeSafe key leaked");

        assert_eq!(selection.model, "deepseek-flash");
        let receipt = receipt(&selection);
        assert_eq!(receipt.reason, AutoRouteReason::ClassifierRecommendation);
        assert_eq!(
            receipt.data_path,
            AutoRouteDataPath::Decision {
                route: DecisionRouterRoute::Typesafe,
                model: "jev-latest".to_string(),
            }
        );
        assert!(receipt.decision.is_some());
        // A decision endpoint never inherits the active DeepSeek billing identity.
        assert_eq!(selection.routed_usage.len(), 1);
        let usage = &selection.routed_usage[0];
        assert_eq!(usage.usage.route.provider, ProviderKind::Custom);
        assert_eq!(usage.usage.route.provider_identity, "typesafe");
        assert_eq!(
            usage.usage.route.billing_mode,
            crate::cost_status::RouteBillingMode::Unknown
        );
        assert_eq!(usage.usage.usage.input_tokens, 476);
        assert_eq!(usage.usage.usage.output_tokens, 70);
        assert!(selection.routed_usage_drop_records.is_empty());
    }

    #[test]
    fn declared_router_without_a_key_is_shown_as_failing() {
        let _env = hermetic_env();
        let mut config = decision_config("https://openrouter.invalid/api/v1", false, 2);
        config
            .providers
            .as_mut()
            .expect("providers")
            .openrouter
            .api_key = None;
        let inventory = ModelInventory::from_config(&config).unwrap();
        assert!(!inventory.router_available);
        let selection = auto_route_without_router(&config, &inventory);
        assert_eq!(
            receipt(&selection).router_failure,
            Some(AutoRouterFailure::NotRunnable)
        );
    }

    fn parsed(json: serde_json::Value) -> SystemOneResponse {
        serde_json::from_str(&json.to_string()).expect("system one response")
    }

    fn pair() -> ActiveTierPair {
        ActiveTierPair {
            provider: Config::default().test_identity_for_kind(ProviderKind::Deepseek),
            strong: "deepseek-v4-pro".to_string(),
            fast: "deepseek-v4-flash".to_string(),
        }
    }

    fn policy(cost_saving: bool, response: &SystemOneResponse) -> InventoryAutoRouteAttempt {
        let _env = hermetic_env();
        let config = decision_config("https://openrouter.invalid/api/v1", cost_saving, 2);
        let inventory = ModelInventory::from_config(&config).unwrap();
        decision_attempt_from_response(cost_saving, &inventory, &pair(), None, response, 120)
    }

    #[test]
    fn cost_saving_needs_a_clear_strong_probability() {
        let unsure = policy(true, &parsed(answer_body(0.70, 0.9)));
        let recommendation = unsure.recommendation.expect("acted");
        assert_eq!(recommendation.model, "deepseek-v4-flash");
        assert!(unsure.decision.expect("evidence").cost_saving_kept_fast);

        let clear = policy(true, &parsed(answer_body(0.80, 0.9)));
        assert_eq!(
            clear.recommendation.expect("acted").model,
            "deepseek-v4-pro"
        );
        assert!(!clear.decision.expect("evidence").cost_saving_kept_fast);

        let balanced = policy(false, &parsed(answer_body(0.70, 0.9)));
        assert_eq!(
            balanced.recommendation.expect("acted").model,
            "deepseek-v4-pro"
        );
    }

    #[test]
    fn invalid_answers_are_rejected_not_repaired() {
        let mut outside = answer_body(0.8, 0.6);
        outside["answers"]["tier"]["choice"] = "medium".into();
        let mut bad_sum = answer_body(0.8, 0.6);
        bad_sum["answers"]["tier"]["probabilities"]["fast"] = 0.5.into();
        let mut missing = answer_body(0.8, 0.6);
        missing["answers"]
            .as_object_mut()
            .expect("answers")
            .remove("tier");
        let mut null_probability = answer_body(0.8, 0.6);
        null_probability["answers"]["tier"]["probabilities"]["fast"] = serde_json::Value::Null;
        let mut bad_confidence = answer_body(0.8, 0.6);
        bad_confidence["answers"]["tier"]["confidence"] = 1.5.into();
        for body in [outside, bad_sum, missing, null_probability, bad_confidence] {
            let attempt = policy(false, &parsed(body.clone()));
            assert_eq!(attempt.recommendation, None, "{body}");
            assert_eq!(
                attempt
                    .decision
                    .as_ref()
                    .and_then(|r| r.provider_reported_cost_usd.as_deref()),
                parsed(body.clone())
                    .usage
                    .as_ref()
                    .and_then(|u| u.reported_cost())
                    .as_deref(),
                "rejected policy retains cost"
            );
            assert_eq!(
                attempt.failure,
                Some(AutoRouterFailure::InvalidAnswer),
                "{body}"
            );
        }

        // An invalid `thinking` answer drops only the effort.
        let mut bad_thinking = answer_body(0.8, 0.6);
        bad_thinking["answers"]["thinking"]["choice"] = "ultra".into();
        let attempt = policy(false, &parsed(bad_thinking));
        let recommendation = attempt.recommendation.expect("tier still acted on");
        assert_eq!(recommendation.model, "deepseek-v4-pro");
        assert_eq!(recommendation.reasoning_effort, None);
    }

    #[tokio::test]
    async fn malformed_decision_envelopes_preserve_cost_without_a_route_hop() {
        let _env = hermetic_env();
        for (pointer, value, incomplete_usage) in [
            ("/answers/tier/choice", serde_json::json!(17), false),
            ("/model", serde_json::json!({"invalid":"model"}), false),
            ("/usage/input_tokens", serde_json::json!(u64::MAX), true),
        ] {
            let server = MockServer::start().await;
            let mut body = answer_body(0.9, 0.9);
            *body.pointer_mut(pointer).expect("fixture field") = value;
            Mock::given(method("POST"))
                .and(path("/api/alpha/decisions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            let config = decision_config(&format!("{}/api/v1", server.uri()), false, 2);
            let fallback =
                auto_route_without_router(&config, &ModelInventory::from_config(&config).unwrap());
            let selection = route(&config, "Explain a variable name", "").await;
            assert_eq!(selection.source, AutoRouteSource::Heuristic);
            assert_eq!(selection.provider, fallback.provider);
            assert_eq!(
                selection.model, fallback.model,
                "malformed policy cannot change the route"
            );
            let receipt = receipt(&selection);
            assert_eq!(
                receipt.router_failure,
                Some(AutoRouterFailure::InvalidAnswer)
            );
            assert_eq!(
                receipt
                    .decision
                    .as_ref()
                    .expect("billing evidence")
                    .provider_reported_cost_usd
                    .as_deref(),
                Some("0.000019992")
            );
            if incomplete_usage {
                assert!(selection.routed_usage.is_empty());
                assert_eq!(selection.routed_usage_dropped_records, 1);
                assert_eq!(selection.routed_usage_drop_records.len(), 1);
            } else {
                assert_eq!(selection.routed_usage.len(), 1);
                assert!(selection.routed_usage_drop_records.is_empty());
            }
        }
    }

    #[test]
    fn camel_case_usage_and_verbatim_cost_parse() {
        let response = parsed(serde_json::json!({
            "answers": {},
            "usage": { "inputTokens": 381, "outputTokens": 62, "cost": 0.000016002 },
        }));
        let usage = response.usage.expect("usage");
        assert_eq!(usage.input_tokens, 381);
        assert_eq!(usage.output_tokens, 62);
        assert_eq!(usage.reported_cost().as_deref(), Some("0.000016002"));
    }

    #[test]
    fn receipts_saved_before_decision_routing_still_load() {
        let json = r#"{"tier":"fast","pair":{"strong":"deepseek-v4-pro","fast":"deepseek-v4-flash"},"scope":"active_provider","data_path":{"classifier":{"provider":"deepseek","model":"deepseek-v4-flash"}},"reason":"classifier_recommendation"}"#;
        let receipt: AutoRouteReceipt = serde_json::from_str(json).expect("legacy receipt");
        assert_eq!(receipt.decision, None);
        assert_eq!(receipt.router_failure, None);
        // And the new fields stay off the wire when empty.
        assert_eq!(serde_json::to_string(&receipt).expect("json"), json);
    }
}
