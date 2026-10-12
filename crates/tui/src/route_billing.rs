//! Route-aware billing presentation.
//!
//! Model pricing and the way a user pays for a route are different facts.
//! The same model can be metered through an API key or covered by an OAuth /
//! token-plan subscription.  Keep that decision in one small module so TUI
//! surfaces do not infer dollars from a model id alone.
//!
//! Display rule (TUI-DOG-010):
//! - dollars only for metered routes with a real priced usage basis and
//!   positive accrued spend;
//! - OAuth/token-plan routes show a quota label, or a real used % when one
//!   was supplied by the provider;
//! - unknown stays unknown — never `$0.00` and never an estimate-as-spend.

use crate::config::{Config, ProviderConfig, ProviderIdentity, ProviderKind};
use crate::pricing::{CostCurrency, UnpricedReason, format_cost_amount};
use codewhale_localization::{Locale, MessageId, tr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingPresentation {
    /// Per-token API usage may be rendered as a currency estimate.
    Metered,
    /// Account/subscription quota is the truthful owner; dollar estimates are
    /// intentionally hidden unless the provider later exposes real spend.
    Subscription(&'static str),
    /// The route is local or otherwise has no provider bill.
    Local,
    /// Billing basis is not known; never invent dollars or a fake zero.
    Unknown,
}

/// Truthful chip for session/footer/sidebar usage surfaces.
#[derive(Debug, Clone, PartialEq)]
pub enum UsageChip {
    /// Positive accrued spend on a metered route with real pricing.
    Money(String),
    /// Authoritatively priced portion of a mixed/legacy session whose complete
    /// spend is unknown. The amount remains visible without being called a
    /// total.
    PricedSubtotal {
        amount: String,
        legacy: bool,
        reasons: Vec<UnpricedReason>,
    },
    /// Subscription / OAuth allowance. `used_pct` is only set when the
    /// provider supplied a real percentage.
    Allowance {
        label: &'static str,
        used_pct: Option<f32>,
    },
    Local,
    Unknown(Vec<UnpricedReason>),
    /// Metered route with pricing, but nothing spent yet — omit the chip
    /// rather than rendering `$0.00` / `<$0.0001`.
    Hidden,
}

impl BillingPresentation {
    #[must_use]
    pub const fn shows_money(self) -> bool {
        matches!(self, Self::Metered)
    }

    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Self::Metered => None,
            Self::Subscription(label) => Some(label),
            Self::Local => Some("local"),
            Self::Unknown => Some("unknown"),
        }
    }
}

/// Serializable mirror of [`BillingPresentation`] for crossing the child →
/// parent mailbox boundary. `BillingPresentation` borrows a `&'static str`
/// label, which serde cannot deserialize, so the token-usage envelope carries
/// this owned form instead. Conversion back recognizes only the labels
/// [`for_route`] itself produces; an unrecognized free-text label fails
/// closed to `Unknown` rather than inventing a quota claim.
///
/// **Not on the production child path.** The wired child receipt is
/// [`crate::cost_status::EffectiveRouteEnvelope`], which carries the same
/// classification as a `RouteBillingMode` plus the billing surface, endpoint
/// fingerprint and dispatch instant, and is emitted by all three real
/// producers (`review`, `verify`, `rlm`) and by the sub-agent mailbox. This
/// owned-label mirror is retained only as the executable record of the
/// serialization contract; gate it with the tests so it cannot rot into a
/// second, drifting provenance channel.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChildBillingProvenance {
    Metered,
    Subscription { label: String },
    Local,
    Unknown,
}

#[cfg(test)]
impl From<BillingPresentation> for ChildBillingProvenance {
    fn from(billing: BillingPresentation) -> Self {
        match billing {
            BillingPresentation::Metered => Self::Metered,
            BillingPresentation::Subscription(label) => Self::Subscription {
                label: label.to_string(),
            },
            BillingPresentation::Local => Self::Local,
            BillingPresentation::Unknown => Self::Unknown,
        }
    }
}

#[cfg(test)]
impl ChildBillingProvenance {
    /// Convert back to the presentation form consumed by pricing.
    #[must_use]
    pub fn as_billing_presentation(&self) -> BillingPresentation {
        match self {
            Self::Metered => BillingPresentation::Metered,
            Self::Local => BillingPresentation::Local,
            Self::Unknown => BillingPresentation::Unknown,
            Self::Subscription { label } => static_subscription_label(label).map_or(
                BillingPresentation::Unknown,
                BillingPresentation::Subscription,
            ),
        }
    }
}

/// The subscription labels [`for_route`] can emit, mapped back to their
/// static form. Anything else is not a label this process vouches for.
#[cfg(test)]
fn static_subscription_label(label: &str) -> Option<&'static str> {
    Some(match label {
        "ChatGPT plan allowance" => "ChatGPT plan allowance",
        "Codex OAuth quota" => "Codex OAuth quota",
        "OpenCode Go quota" => "OpenCode Go quota",
        "Z.ai Coding Plan quota" => "Z.ai Coding Plan quota",
        "MiMo token plan" => "MiMo token plan",
        "Kimi Code quota" => "Kimi Code quota",
        "MiniMax Token Plan quota" => "MiniMax Token Plan quota",
        "Grok OAuth quota" => "Grok OAuth quota",
        "Claude OAuth quota" => "Claude OAuth quota",
        "StepFun Step Plan quota" => "StepFun Step Plan quota",
        "Alibaba Token Plan" => "Alibaba Token Plan",
        "Alibaba Coding Plan" => "Alibaba Coding Plan",
        "Volcengine Coding Plan" => "Volcengine Coding Plan",
        _ => return None,
    })
}

/// Immutable, non-secret receipt of the route a request was dispatched on.
///
/// This is what a child/non-active route must be billed from. Re-reading an
/// ambient `Config` for a non-active provider is unsound: `apply_env_overrides`
/// merges provider endpoint variables (`MOONSHOT_BASE_URL`, `KIMI_BASE_URL`,
/// …) into the **active** provider's table only, so a cross-provider child's
/// config entry does not describe the endpoint its client was built with.
///
/// Test-only: the production dispatch path captures a full
/// [`DispatchedReceipt`] at the client-freeze boundary and classifies with
/// [`for_dispatched_receipt`]. This pair exists so route-resolution tests can
/// assert that the pre-dispatch and receipt answers cannot disagree.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub struct DispatchedRoute<'a> {
    /// Provider the dispatched client is bound to.
    pub provider: ProviderKind,
    /// Base URL the dispatched client will call, verbatim.
    pub base_url: &'a str,
}

/// A fully captured, `Config`-free billing receipt.
///
/// This is what [`for_dispatched_receipt`] consumes. Every field is captured
/// at dispatch; nothing here can be re-derived later.
#[derive(Debug, Clone, Copy)]
pub struct DispatchedReceipt<'a> {
    /// Provider the dispatched client was bound to.
    pub provider: ProviderKind,
    /// Non-secret identity key that selected this route's table — the
    /// `[providers.<name>]` key for a named custom route, the provider's own
    /// key otherwise.
    ///
    /// `None` means the identity was not captured. For a named custom route
    /// that is fatal to any product claim: without it there is no way to say
    /// *which* custom vendor ran, and the classifier fails closed rather than
    /// reading whichever custom table happens to be selected now.
    pub identity: Option<&'a str>,
    /// Base URL the dispatched client called, verbatim.
    pub base_url: &'a str,
    /// Product truth captured when this client was built.
    pub product: RouteProduct,
}

/// Immutable, non-secret product truth for one route, captured at the moment
/// its client was built.
///
/// Several providers are *credential-shaped* rather than endpoint-shaped: the
/// same host sells both a metered and a subscription product, and only the
/// credential (or an operator-declared pay mode) separates them. That fact
/// cannot be recovered later from an ambient `Config` — the session may have
/// switched provider, custom table, or key since — so it has to travel with
/// the receipt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RouteProduct {
    /// No product fact was captured. Credential-shaped providers must fail
    /// closed on this: an uncaptured product is not a licence to guess.
    #[default]
    Unproven,
    /// The route's credential/pay mode is subscription-backed, with this
    /// user-facing quota label.
    Subscription(&'static str),
    /// The route bills per token.
    Metered,
}

/// Resolve how a provider route should present usage, from the endpoint that
/// route resolves to right now.
///
/// The endpoint is resolved exactly once, through the same identity-aware
/// [`Config::base_url_for_route`] the client is built from, and is then judged
/// by the same exact-product rules a dispatch receipt gets. There is no
/// separate "ambient" reading of a provider's table: a config entry with no
/// `base_url` still resolves to a real endpoint (an imported Kimi token
/// resolves to the Kimi Code membership host), and classifying from the raw
/// table field would call that route metered and invent dollars against a
/// membership quota.
///
/// This is the pre-dispatch answer — for a turn that already ran, bill from
/// its receipt with [`crate::route_billing::for_dispatched_receipt`] instead.
#[must_use]
pub fn for_route(config: &Config, identity: &ProviderIdentity) -> BillingPresentation {
    let base_url = config.base_url_for_route(identity);
    for_route_with_endpoint(config, identity, &base_url)
}

/// Capture billing from the concrete endpoint the client will dispatch on.
/// Candidate-selected endpoints can differ from the ambient config default;
/// credentials cannot turn a gateway endpoint into an official plan receipt.
#[must_use]
pub fn for_route_with_endpoint(
    config: &Config,
    identity: &ProviderIdentity,
    base_url: &str,
) -> BillingPresentation {
    if config.verify_provider_identity(identity).is_err() {
        return BillingPresentation::Unknown;
    }
    classify(
        identity.provider,
        Some(identity.key.as_str()),
        base_url,
        capture_product(config, identity),
    )
}

/// Capture the immutable product facts for `provider` from the config its
/// client is being built from, **at dispatch time**.
///
/// Call this while the config still describes the route being dispatched. The
/// result is what travels on [`crate::route_billing::DispatchedReceipt::product`];
/// nothing downstream
/// may re-derive it.
#[must_use]
pub fn capture_product(config: &Config, identity: &ProviderIdentity) -> RouteProduct {
    if config.verify_provider_identity(identity).is_err() {
        return RouteProduct::Unproven;
    }
    let provider = identity.provider;
    let provider_config = config.provider_config_for(identity);
    match provider {
        ProviderKind::OpenaiCodex => {
            if crate::oauth::official_chatgpt_registration(config).is_ok() {
                RouteProduct::Subscription("ChatGPT plan allowance")
            } else {
                RouteProduct::Unproven
            }
        }
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => {
            match minimax_credential_product(config, identity, provider_config) {
                CredentialProduct::Plan => RouteProduct::Subscription("MiniMax Token Plan quota"),
                CredentialProduct::PayAsYouGo => RouteProduct::Metered,
                CredentialProduct::Unprovable => RouteProduct::Unproven,
            }
        }
        ProviderKind::Csdn => match csdn_credential_product(provider_config) {
            CredentialProduct::Plan => RouteProduct::Subscription("CSDN Coding Plan quota"),
            CredentialProduct::PayAsYouGo => RouteProduct::Metered,
            CredentialProduct::Unprovable => RouteProduct::Unproven,
        },
        ProviderKind::XiaomiMimo => {
            if xiaomi_is_explicit_pay_as_you_go(provider_config) {
                RouteProduct::Metered
            } else {
                RouteProduct::Subscription("MiMo token plan")
            }
        }
        ProviderKind::Xai => {
            if provider_config.is_some_and(uses_xai_oauth)
                && crate::oauth::credentials_valid(crate::oauth::OAuthProvider::Xai, config)
            {
                RouteProduct::Subscription("Grok OAuth quota")
            } else {
                RouteProduct::Metered
            }
        }
        ProviderKind::Anthropic => {
            if provider_config.is_some_and(uses_anthropic_oauth) {
                RouteProduct::Subscription("Claude OAuth quota")
            } else {
                RouteProduct::Metered
            }
        }
        ProviderKind::Custom => match provider_config {
            Some(entry) if !custom_billing_unknown(entry) => RouteProduct::Metered,
            // No table, or a table with no declared pay mode: a custom vendor
            // that has not told us how it bills.
            _ => RouteProduct::Unproven,
        },
        // Endpoint-shaped and flat-rate providers need no credential fact.
        _ => RouteProduct::Unproven,
    }
}

/// Resolve billing for a route from its dispatch-time receipt.
///
/// Deliberately takes no `Config`: after dispatch there is no sound ambient
/// state to consult. The session can have switched provider, custom table, or
/// credential since the request went out, so every fact this needs must
/// already be on the receipt. A receipt that does not name a product fails
/// closed to [`BillingPresentation::Unknown`] rather than inventing one.
/// Classify a receipt with no `Config` in reach at all.
///
/// This is the entry point every post-dispatch caller must use. Because it
/// takes no config, a provider switch, a `/provider` change, or a different
/// custom table being selected after dispatch cannot retro-bill the turn onto
/// another route.
#[must_use]
pub fn for_dispatched_receipt(receipt: DispatchedReceipt<'_>) -> BillingPresentation {
    classify(
        receipt.provider,
        receipt.identity,
        receipt.base_url,
        receipt.product,
    )
}

/// Convenience wrapper for callers that still hold the route's own
/// **dispatch-time** config and have not captured a receipt yet.
///
/// Sound only while `config` still describes the dispatched route. Anything
/// that runs after the turn has already completed must capture a
/// [`DispatchedReceipt`] at dispatch and use [`for_dispatched_receipt`].
#[cfg(test)]
#[must_use]
pub fn for_dispatched_route(config: &Config, route: DispatchedRoute<'_>) -> BillingPresentation {
    let identity = config
        .active_provider_identity()
        .ok()
        .filter(|id| id.provider == route.provider)
        .or_else(|| config.builtin_provider_identity(route.provider).ok());
    identity
        .as_ref()
        .map_or(BillingPresentation::Unknown, |identity| {
            for_route_with_endpoint(config, identity, route.base_url)
        })
}

/// The one classifier, pure in its inputs.
///
/// `base_url` is the single resolved endpoint for this route and `product` is
/// the captured credential truth. There is no `Config` parameter on purpose:
/// this cannot read a provider table, a custom entry, or an active selection,
/// so a pre-dispatch answer and a receipt answer cannot drift apart and a
/// post-dispatch provider switch cannot retro-bill a turn onto another route.
fn classify(
    provider: ProviderKind,
    identity: Option<&str>,
    base_url: &str,
    product: RouteProduct,
) -> BillingPresentation {
    match provider {
        ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm => {
            BillingPresentation::Local
        }
        // The official public API accepts both API keys and ChatGPT grants.
        // Only the plan provider plus verified dispatch-time grant can claim
        // allowance; neither a shared host nor a provider name is proof.
        ProviderKind::OpenaiCodex if crate::pricing::is_official_chatgpt_api(base_url) => {
            match product {
                RouteProduct::Subscription("ChatGPT plan allowance") => {
                    BillingPresentation::Subscription("ChatGPT plan allowance")
                }
                _ => BillingPresentation::Unknown,
            }
        }
        // Keep legacy backend receipts interpretable for persisted history.
        ProviderKind::OpenaiCodex if crate::pricing::is_chatgpt_codex_backend(base_url) => {
            BillingPresentation::Subscription("Codex OAuth quota")
        }
        ProviderKind::OpenaiCodex => BillingPresentation::Unknown,
        ProviderKind::OpencodeGo => BillingPresentation::Subscription("OpenCode Go quota"),
        // StepFun already reduces an endpoint to a non-secret billing surface
        // and fails closed on anything it does not recognize.
        ProviderKind::Stepfun => stepfun_billing_for_endpoint(Some(base_url)),
        // Z.ai's dedicated Coding endpoint is the GLM Coding Plan route. Its
        // quota is subscription-backed, so a public API price estimate is not
        // truthful spend and must not appear as dollars in the UI. A
        // credentials-only `[providers.zai]` entry still resolves to that
        // endpoint, because it is also CodeWhale's Z.ai default.
        ProviderKind::Zai if base_url.trim().is_empty() => BillingPresentation::Unknown,
        ProviderKind::Zai if is_zai_coding_plan_endpoint(base_url) => {
            BillingPresentation::Subscription("Z.ai Coding Plan quota")
        }
        ProviderKind::Zai => endpoint_shaped_payg_billing(provider, base_url),
        ProviderKind::XiaomiMimo => product_billing(product),

        // Moonshot's direct platform is pay-as-you-go metered. Only the exact
        // Kimi Code membership endpoint bills against subscription quota.
        //
        // The endpoint must name one of the two known products outright. A
        // neighboring Kimi-hosted path, a gateway host, or a shipped default
        // reached for a route we cannot otherwise explain must not inherit
        // Moonshot's metered price list.
        //
        // Reading the resolved endpoint (not the provider table's `base_url`)
        // is what makes the imported-token membership route truthful: a Kimi
        // Code token with no `base_url` in its table still resolves to
        // api.kimi.com/coding/v1, and calling that metered would put invented
        // dollars against a membership quota.
        ProviderKind::Moonshot if crate::config::moonshot_base_url_is_exact_kimi_code(base_url) => {
            BillingPresentation::Subscription("Kimi Code quota")
        }
        ProviderKind::Moonshot
            if crate::config::moonshot_base_url_is_exact_direct_platform(base_url) =>
        {
            BillingPresentation::Metered
        }
        ProviderKind::Moonshot => BillingPresentation::Unknown,
        // Both MiniMax dialects (`[providers.minimax]` chat-completions and
        // `[providers.minimax_anthropic]` Messages) are reachable with the
        // same MINIMAX_API_KEY and sell the same PAYG/Token Plan duality over
        // the same endpoints, so the wire protocol must not change the billing
        // story and the endpoint cannot settle it either. Only the credential
        // product can, and when that is unprovable the route is Unknown.
        // A MiniMax gateway sells its own product on its own terms, and the
        // PAYG/Token Plan duality only describes MiniMax's own hosts. Settle
        // the endpoint first: anything off the supported direct routes is
        // Unknown no matter what credential was captured.
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
            if !minimax_base_url_is_supported_direct(base_url) =>
        {
            BillingPresentation::Unknown
        }
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => product_billing(product),
        // CSDN 星图 sells the Coding Plan (`glm_for_coding`) and metered
        // marketplace models over the same endpoint; the URL proves only that
        // the route is first-party, so the captured product decides. Anything
        // off the supported direct endpoint is Unknown no matter what was
        // captured.
        ProviderKind::Csdn
            if !codewhale_config::provider::is_exact_csdn_platform_route(
                codewhale_config::ProviderKind::Csdn,
                base_url,
            ) =>
        {
            BillingPresentation::Unknown
        }
        ProviderKind::Csdn => product_billing(product),
        ProviderKind::Xai | ProviderKind::Anthropic => product_billing(product),
        // A named custom route is billed from the identity and endpoint it
        // dispatched on. Without an identity there is no vendor to name, and
        // without an endpoint there is no route at all — either way the honest
        // answer is Unknown rather than whatever the active custom table says.
        ProviderKind::Custom
            if identity.is_none_or(|key| key.trim().is_empty()) || base_url.trim().is_empty() =>
        {
            BillingPresentation::Unknown
        }
        ProviderKind::Custom => product_billing(product),
        // These providers are endpoint-shaped — but
        // only on an endpoint we actually recognize. A first-party or
        // aggregator provider pointed at an unrecognized host is not evidence
        // that the host sells that provider's price list, so it must not fall
        // through to metered per-token dollars on the strength of a provider
        // name (#4318).
        // Keep this match exhaustive: onboarding a provider requires an
        // explicit billing decision and the default-route audit below.
        ProviderKind::Deepseek
        | ProviderKind::DeepseekAnthropic
        | ProviderKind::NvidiaNim
        | ProviderKind::Openai
        | ProviderKind::Atlascloud
        | ProviderKind::WanjieArk
        | ProviderKind::Volcengine
        | ProviderKind::Openrouter
        | ProviderKind::Orcarouter
        | ProviderKind::Novita
        | ProviderKind::Fireworks
        | ProviderKind::Siliconflow
        | ProviderKind::SiliconflowCN
        | ProviderKind::Arcee
        | ProviderKind::OllamaCloud
        | ProviderKind::Huggingface
        | ProviderKind::Modelscope
        | ProviderKind::Together
        | ProviderKind::Qianfan
        | ProviderKind::Openmodel
        | ProviderKind::Deepinfra
        | ProviderKind::Sakana
        | ProviderKind::LongCat
        | ProviderKind::OpencodeZen
        | ProviderKind::Meta
        | ProviderKind::Mistral
        | ProviderKind::Google
        | ProviderKind::Antigravity
        | ProviderKind::Telecomjs
        | ProviderKind::Edenai
        | ProviderKind::Zenmux
        | ProviderKind::Concentrate
        | ProviderKind::Codewhale
        | ProviderKind::ModelstudioTokenPlan
        | ProviderKind::ModelstudioTokenPlanAnthropic
        | ProviderKind::ModelstudioCodingPlan
        | ProviderKind::ModelstudioCodingPlanAnthropic => {
            endpoint_shaped_payg_billing(provider, base_url)
        }
    }
}

/// Metered only when the resolved endpoint reduces to a known money surface.
/// An unclassified endpoint is Unknown, never metered-by-provider-name.
fn endpoint_shaped_payg_billing(provider: ProviderKind, base_url: &str) -> BillingPresentation {
    use crate::pricing::EndpointMetering;

    let surface = crate::pricing::billing_surface_for_route(provider, Some(base_url));
    match crate::pricing::endpoint_metering_for_billing_surface(surface) {
        EndpointMetering::Money => BillingPresentation::Metered,
        EndpointMetering::LocalNoBill => BillingPresentation::Local,
        EndpointMetering::ExactSubscription => BillingPresentation::Subscription(match surface {
            Some(crate::pricing::MODELSTUDIO_TOKEN_PLAN_BILLING_SURFACE) => "Alibaba Token Plan",
            Some(crate::pricing::MODELSTUDIO_CODING_PLAN_BILLING_SURFACE) => "Alibaba Coding Plan",
            Some(crate::pricing::VOLCENGINE_CODING_PLAN_BILLING_SURFACE) => {
                "Volcengine Coding Plan"
            }
            _ => "provider plan",
        }),
        EndpointMetering::Unknown => BillingPresentation::Unknown,
    }
}

/// Billing presentation for callers that hold a provider and the concrete base
/// URL but **not** the app [`Config`] — background helpers (compaction,
/// purge) that run off a bare client.
///
/// Everything decidable from provider identity plus a classified endpoint is
/// decided; everything that depends on credentials or an auth mode CodeWhale
/// cannot see from here stays [`BillingPresentation::Unknown`]. In particular a
/// local, custom, or plan endpoint is never allowed to fall through to metered
/// per-token dollars on the strength of a provider name (#4318).
///
/// This is exactly a receipt with no identity and no captured product, so it
/// runs through the one [`classify`] path rather than keeping a second,
/// drift-prone copy of the endpoint rules: an uncaptured product makes every
/// credential-shaped provider Unknown, and a missing identity makes every
/// named custom route Unknown.
#[must_use]
pub fn for_endpoint_without_config(
    provider: ProviderKind,
    base_url: Option<&str>,
) -> BillingPresentation {
    classify(
        provider,
        None,
        base_url.unwrap_or_default(),
        RouteProduct::Unproven,
    )
}

/// Immutable billing surface captured when a foreground/child request is
/// dispatched. Endpoint classification owns ordinary providers; MiniMax and
/// OAuth-on-the-same-host providers require the saved route mode as additional
/// evidence and otherwise fail closed.
#[must_use]
pub fn billing_surface_for_dispatch(
    config: Option<&Config>,
    identity: &ProviderIdentity,
    base_url: Option<&str>,
) -> Option<&'static str> {
    let provider = identity.provider;
    if let Some(config) = config {
        if config.verify_provider_identity(identity).is_err() {
            return None;
        }
        let billing = base_url.map_or_else(
            || for_route(config, identity),
            |endpoint| for_route_with_endpoint(config, identity, endpoint),
        );
        match billing {
            BillingPresentation::Subscription(_) => {
                return Some(match provider {
                    ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => {
                        crate::pricing::MINIMAX_TOKEN_PLAN_BILLING_SURFACE
                    }
                    ProviderKind::Csdn => crate::pricing::CSDN_CODING_PLAN_BILLING_SURFACE,
                    ProviderKind::OpenaiCodex
                    | ProviderKind::OpencodeGo
                    | ProviderKind::Anthropic
                    | ProviderKind::Xai => crate::pricing::OAUTH_SUBSCRIPTION_BILLING_SURFACE,
                    _ => crate::pricing::billing_surface_for_route(provider, base_url)
                        .unwrap_or(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
                });
            }
            BillingPresentation::Metered
                if matches!(
                    provider,
                    ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
                ) =>
            {
                return Some(crate::pricing::MINIMAX_PAYG_BILLING_SURFACE);
            }
            BillingPresentation::Metered if provider == ProviderKind::Csdn => {
                return Some(crate::pricing::CSDN_PAYG_BILLING_SURFACE);
            }
            BillingPresentation::Local => return Some(crate::pricing::LOCAL_BILLING_SURFACE),
            BillingPresentation::Unknown | BillingPresentation::Metered => {}
        }
    }
    crate::pricing::billing_surface_for_route(provider, base_url)
}

/// Credential-shaped providers answer from the captured product and nothing
/// else. An uncaptured product is Unknown: no invented dollars, no invented
/// quota label.
fn product_billing(product: RouteProduct) -> BillingPresentation {
    match product {
        RouteProduct::Subscription(label) => BillingPresentation::Subscription(label),
        RouteProduct::Metered => BillingPresentation::Metered,
        RouteProduct::Unproven => BillingPresentation::Unknown,
    }
}

// MiniMax's own hosted routes, for both wire dialects. Single-sourced in
// `config` so billing classification and request shaping cannot disagree about
// which hosts are first-party.
use crate::config::minimax_base_url_is_supported_direct;

/// StepFun already reduces an endpoint to a non-secret billing surface and
/// fails closed on anything it does not recognize, so the resolved endpoint
/// and a dispatch receipt use the same reduction unchanged.
fn stepfun_billing_for_endpoint(base_url: Option<&str>) -> BillingPresentation {
    match crate::pricing::billing_surface_for_route(ProviderKind::Stepfun, base_url) {
        Some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE) => BillingPresentation::Metered,
        Some(crate::pricing::STEPFUN_PLAN_BILLING_SURFACE) => {
            BillingPresentation::Subscription("StepFun Step Plan quota")
        }
        _ => BillingPresentation::Unknown,
    }
}

fn is_zai_coding_plan_endpoint(base_url: &str) -> bool {
    base_url
        .trim()
        .trim_end_matches('/')
        .ends_with("/api/coding/paas/v4")
}

/// Billing for a child route. Billing is never guessed from provider
/// identity:
///
/// - `child_provenance` — the child's own route truth, classified by
///   [`for_dispatched_route`] from the immutable endpoint receipt captured
///   when its client was built, and carried on the usage envelope — always
///   wins.
/// - Without provenance, a child on the parent's provider runs the parent's
///   exact route (review/verify/rlm children reuse the session client), so
///   it inherits `parent_billing`.
/// - Without provenance, a cross-provider child fails closed: local routes
///   stay `Local`; everything else is `Unknown` — no invented dollars and no
///   invented subscription labels.
///
/// **Superseded by [`for_child_route_receipt`].** Retained for the
/// subagent-routing path and its existing coverage, which compare first-party
/// providers whose identity key is the provider string itself. It must not be
/// used where a named custom route can appear: every custom route maps to
/// `ProviderKind::Custom`, so the enum comparison below cannot tell custom
/// vendor A from custom vendor B.
///
/// Unknown is deliberately not a subscription label (#4318). A provider that
/// *can* be subscription-billed is not evidence that this child turn *was*,
/// and because non-metered routes are excused from money coverage, that guess
/// would quietly remove real spend from `/cost`'s denominator instead of
/// reporting it as missing.
#[must_use]
#[cfg(test)]
pub fn for_child_route(
    parent_provider: ProviderKind,
    parent_billing: BillingPresentation,
    child_provider: ProviderKind,
    child_provenance: Option<BillingPresentation>,
) -> BillingPresentation {
    if let Some(provenance) = child_provenance {
        return provenance;
    }
    if child_provider == parent_provider {
        return parent_billing;
    }
    match child_provider {
        // No provider bill exists for a local runtime under any configuration.
        ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm => {
            BillingPresentation::Local
        }
        _ => BillingPresentation::Unknown,
    }
}

/// Identity-aware child billing, from the parent's frozen receipt.
///
/// **Not on the production child path**, for the same reason as
/// [`ChildBillingProvenance`]: `tui::tool_routing` bills a child from the
/// child's own [`crate::cost_status::EffectiveRouteEnvelope`], rehydrated from
/// the complete `child_*` metadata its producer emits, and an incomplete
/// payload fails closed to Unknown rather than inheriting anything (see
/// `legacy_child_usage_metadata_fails_closed_without_parent_route_fallback`).
/// The identity-comparison rule below is therefore structurally unreachable —
/// nothing inherits — and is kept with the tests as the record of it.
#[cfg(test)]
#[must_use]
pub fn for_child_route_receipt(
    parent: ChildParentRoute<'_>,
    child: ChildRouteClaim<'_>,
    child_provenance: Option<BillingPresentation>,
) -> BillingPresentation {
    if let Some(provenance) = child_provenance {
        return provenance;
    }
    // A child that claims no route at all ran in-process on the parent's own
    // client (review/verify/rlm critics reuse the session client), so the
    // parent's frozen receipt *is* its receipt. This is inheritance from an
    // immutable capture, not from live session state.
    if !child.named {
        return parent.billing;
    }
    // Same-route inheritance requires the *whole* route to match, not just the
    // provider enum. Every named custom route maps to `ProviderKind::Custom`,
    // so an enum comparison would let a child on custom vendor A inherit the
    // parent's product label from custom vendor B.
    if child.provider == Some(parent.provider)
        && child.identity.is_some_and(|key| key == parent.identity)
    {
        return parent.billing;
    }
    // A child that named a provider string this build cannot parse names no
    // route we can vouch for. That is not a licence to inherit: Unknown.
    match child.provider {
        Some(ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm) => {
            BillingPresentation::Local
        }
        _ => BillingPresentation::Unknown,
    }
}

/// Non-secret route facts a child tool must publish alongside its token usage.
///
/// Emitted from the child's own dispatched client, so the parent consumer never
/// has to infer which route ran. Keys are pinned by
/// `child_route_metadata_round_trips_through_the_consumer` so a producer and
/// the reader in `tui::tool_routing` cannot drift apart.
///
/// `product` is left [`RouteProduct::Unproven`] when the child has no
/// route-scoped `Config` in reach: that classifies credential-shaped providers
/// as Unknown, which is the honest answer rather than a guess. A child running
/// the parent's exact route is recognized by identity and inherits the
/// parent's frozen receipt instead.
/// Currently exercised only by
/// `child_route_metadata_round_trips_through_the_consumer`: no tool producer
/// emits the keys yet, and the reader in `tui::tool_routing` treats them as
/// optional. The pairing lives here so a producer and that reader cannot drift
/// apart when one is wired up.
#[cfg(test)]
#[must_use]
pub fn child_route_metadata(
    provider: ProviderKind,
    identity: &str,
    base_url: &str,
    product: RouteProduct,
) -> serde_json::Value {
    let billing = for_dispatched_receipt(DispatchedReceipt {
        provider,
        identity: Some(identity),
        base_url,
        product,
    });
    serde_json::json!({
        "child_provider": provider.as_str(),
        "child_provider_identity": identity,
        "child_billing": ChildBillingProvenance::from(billing),
    })
}

/// The parent turn's frozen receipt, as the only inheritance basis a child may
/// use.
///
/// Deliberately not `app.billing_presentation`: that chip is live session
/// state, rewritten on every `/provider` switch, so reading it when a child's
/// usage envelope arrives bills the child against whatever route the session
/// points at *now*.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub struct ChildParentRoute<'a> {
    pub provider: ProviderKind,
    /// The parent turn's captured identity key.
    pub identity: &'a str,
    /// Billing classified from the parent turn's dispatch receipt.
    pub billing: BillingPresentation,
}

/// What a child claims about its own route.
///
/// `named` distinguishes the two very different silences:
///
/// - `named: false` — the child published no route at all, which means it ran
///   on the parent's own client. Inheriting the parent's frozen receipt is
///   correct.
/// - `named: true` with `provider: None` — the child published a provider
///   string this build cannot parse. It named *some* route, just not one we
///   recognize, so inheritance would be a guess: Unknown.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ChildRouteClaim<'a> {
    /// Whether the child published any route string at all.
    pub named: bool,
    pub provider: Option<ProviderKind>,
    pub identity: Option<&'a str>,
}

/// Whether this route may show a dollar amount for the given model.
///
/// Requires both a metered billing presentation and an authoritative priced
/// basis for the model. OAuth/token-plan routes always return false even when
/// the same model id is priced on a public API route.
#[cfg(test)]
#[must_use]
pub fn has_priced_metered_basis(
    billing: BillingPresentation,
    provider: ProviderKind,
    model: &str,
) -> bool {
    billing.shows_money()
        && if provider == ProviderKind::Stepfun {
            crate::pricing::has_pricing_for_billing_surface(
                provider,
                model,
                Some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE),
            )
        } else {
            crate::pricing::has_pricing_for_provider(provider, model)
        }
}

/// Build the truthful usage chip for session surfaces.
///
/// `used_pct` is only honored for subscription/OAuth routes and must come from
/// a provider-supplied allowance reading — never from a local estimate.
#[must_use]
pub fn usage_chip(
    billing: BillingPresentation,
    provider: ProviderKind,
    model: &str,
    displayed_cost: f64,
    currency: CostCurrency,
    used_pct: Option<f32>,
) -> UsageChip {
    match billing {
        BillingPresentation::Local => UsageChip::Local,
        BillingPresentation::Unknown => {
            UsageChip::Unknown(vec![UnpricedReason::UnknownBillingBasis])
        }
        BillingPresentation::Subscription(label) => UsageChip::Allowance {
            label,
            used_pct: used_pct.filter(|pct| pct.is_finite() && *pct >= 0.0),
        },
        BillingPresentation::Metered => {
            let surface = (provider == ProviderKind::Stepfun)
                .then_some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE);
            let audit = if surface.is_some() {
                crate::pricing::audit_turn_cost_for_route_at(
                    provider,
                    model,
                    surface,
                    &codewhale_models::Usage::default(),
                    chrono::Utc::now(),
                )
            } else {
                crate::pricing::audit_turn_cost_for_provider_at(
                    provider,
                    model,
                    &codewhale_models::Usage::default(),
                    chrono::Utc::now(),
                )
            };
            if !audit.is_priced_in(currency) {
                UsageChip::Unknown(vec![
                    audit
                        .unpriced_reason
                        .unwrap_or(UnpricedReason::UnsupportedCurrency),
                ])
            } else if displayed_cost.is_finite() && displayed_cost > 0.0 {
                UsageChip::Money(format_cost_amount(displayed_cost, currency))
            } else {
                UsageChip::Hidden
            }
        }
    }
}

/// Compact footer/header chip text. `None` means omit the chip.
#[must_use]
pub fn format_usage_chip(chip: &UsageChip, locale: Locale) -> Option<String> {
    match chip {
        UsageChip::Money(amount) => Some(amount.clone()),
        UsageChip::PricedSubtotal {
            amount,
            legacy,
            reasons,
        } => Some(
            tr(
                locale,
                if *legacy {
                    MessageId::CostChipSavedSubtotal
                } else {
                    MessageId::CostChipSubtotal
                },
            )
            .replace("{amount}", amount)
            .replace("{reasons}", &format_unpriced_reasons(reasons, locale)),
        ),
        UsageChip::Allowance { label, used_pct } => Some(match used_pct {
            Some(pct) => tr(locale, MessageId::CostChipAllowancePercent)
                .replace("{plan}", label)
                .replace("{percent}", &format!("{pct:.0}")),
            None => tr(locale, MessageId::CostChipAllowance).replace("{plan}", label),
        }),
        UsageChip::Local => Some(tr(locale, MessageId::CostChipLocal).into_owned()),
        UsageChip::Unknown(reasons) => Some(
            tr(locale, MessageId::CostChipUnknown)
                .replace("{reasons}", &format_unpriced_reasons(reasons, locale)),
        ),
        UsageChip::Hidden => None,
    }
}

/// The same saved receipt explains missing coverage in every cost surface.
#[must_use]
pub fn format_unpriced_reasons(reasons: &[UnpricedReason], locale: Locale) -> String {
    if reasons.is_empty() {
        return tr(locale, UnpricedReason::UnrecordedCoverage.message_id()).into_owned();
    }
    let mut descriptions = Vec::new();
    for reason in reasons {
        let text = tr(locale, reason.message_id());
        if !descriptions.contains(&text) {
            descriptions.push(text);
        }
    }
    descriptions.join(", ")
}

fn custom_billing_unknown(config: &ProviderConfig) -> bool {
    // A custom OpenAI-compatible endpoint with no explicit pay mode and no
    // priced catalog is treated as unknown rather than inventing metered
    // dollars from a borrowed model id.
    let mode = auth_mode(config);
    !mode.as_deref().is_some_and(|mode| {
        matches!(
            mode,
            "api_key"
                | "api"
                | "key"
                | "keyring"
                | "payg"
                | "paygo"
                | "pay_as_you_go"
                | "metered"
                | "standard"
        )
    })
}

fn normalized(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn auth_mode(config: &ProviderConfig) -> Option<String> {
    config
        .auth_mode
        .as_deref()
        .or(config.mode.as_deref())
        .map(normalized)
}

fn uses_xai_oauth(config: &ProviderConfig) -> bool {
    auth_mode(config).is_some_and(|mode| crate::oauth::auth_mode_uses_xai_oauth(&mode))
}

fn uses_anthropic_oauth(config: &ProviderConfig) -> bool {
    auth_mode(config).is_some_and(|mode| {
        matches!(
            mode.as_str(),
            "oauth"
                | "anthropic_oauth"
                | "claude_oauth"
                | "claude_cli"
                | "claude_code"
                | "max"
                | "subscription"
        )
    })
}

/// What immutable, non-secret provenance can prove about the credential
/// product behind a dual-product route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialProduct {
    /// A subscription / token-plan product is proven.
    Plan,
    /// An ordinary metered (pay-as-you-go) product is proven.
    PayAsYouGo,
    /// Neither can be proven from route/auth provenance. Classification must
    /// fail closed rather than default to metered dollars.
    Unprovable,
}

/// MiniMax sells both a pay-as-you-go API and a Token Plan subscription over
/// the *same* endpoints and the same `MINIMAX_API_KEY`, so the product can
/// only come from an explicit pay mode or the credential's own product prefix.
///
/// A key held in the Codewhale secret store / OS keyring is deliberately not
/// probed: classification must never be a reason to open secret storage. When
/// no product marker is visible the route is `Unprovable`, and [`for_route`]
/// reports Unknown instead of inventing pay-as-you-go dollars.
fn minimax_credential_product(
    config: &Config,
    identity: &ProviderIdentity,
    provider_config: Option<&ProviderConfig>,
) -> CredentialProduct {
    // An explicit operator-set pay mode is the strongest non-secret
    // provenance available: the operator has told us how the account bills,
    // and it wins over key shape in both directions. An unrecognized mode is
    // not a product claim.
    if let Some(mode) = provider_config
        .and_then(|config| config.mode.as_deref())
        .filter(|mode| !mode.trim().is_empty())
        .map(normalized)
    {
        return match mode.as_str() {
            // `subscription_plan` is the spelling the cost lane's operator
            // docs and tests used; keep it recognized so an explicit operator
            // declaration is never silently discarded as "unprovable".
            "token_plan" | "tokenplan" | "plan" | "subscription" | "subscription_plan" => {
                CredentialProduct::Plan
            }
            "pay_as_you_go" | "payg" | "paygo" | "pay_as_go" | "metered" | "standard" | "api"
            | "api_key" | "default" => CredentialProduct::PayAsYouGo,
            _ => CredentialProduct::Unprovable,
        };
    }
    match visible_minimax_credential_is_plan_shaped(config, identity, provider_config) {
        Some(true) => CredentialProduct::Plan,
        Some(false) => CredentialProduct::PayAsYouGo,
        None => CredentialProduct::Unprovable,
    }
}

/// CSDN 星图 sells the Coding Plan and metered marketplace models over the
/// same `ai.csdn.net/api/model/v1` endpoint and `CSDN_API_KEY` slot, so the
/// product comes from an explicit saved pay mode or the routed model:
/// `glm_for_coding` is the Coding Plan's dedicated model id — the route the
/// plan sells — while every other model on the platform endpoint is ordinary
/// metered marketplace access. An explicit operator-set mode wins over the
/// model in both directions; an unrecognized mode is not a product claim.
fn csdn_credential_product(provider_config: Option<&ProviderConfig>) -> CredentialProduct {
    if let Some(mode) = provider_config
        .and_then(|config| config.mode.as_deref())
        .filter(|mode| !mode.trim().is_empty())
        .map(normalized)
    {
        return match mode.as_str() {
            "coding_plan" | "codingplan" | "plan" | "subscription" | "subscription_plan" => {
                CredentialProduct::Plan
            }
            "pay_as_you_go" | "payg" | "paygo" | "pay_as_go" | "metered" | "standard" | "api"
            | "api_key" | "default" => CredentialProduct::PayAsYouGo,
            _ => CredentialProduct::Unprovable,
        };
    }
    // No table at all still resolves to the plan model: `glm_for_coding` is
    // the shipped default for the `csdn` route.
    let model = provider_config
        .and_then(|entry| entry.model.as_deref())
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(crate::config::DEFAULT_CSDN_MODEL);
    if model.eq_ignore_ascii_case(crate::config::DEFAULT_CSDN_MODEL) {
        CredentialProduct::Plan
    } else {
        CredentialProduct::PayAsYouGo
    }
}

/// Whether a MiniMax credential is visible in non-secret-store provenance,
/// and if so whether it carries the Token Plan (`sk-cp…`) product prefix.
///
/// Only the product marker is returned — the credential value never leaves
/// this function, nothing is logged, and the secret store is never opened.
/// `None` means "no visible credential", which is the honest answer for a
/// key resolved from the keyring, from an OAuth/command source, or from
/// nowhere at all.
fn visible_minimax_credential_is_plan_shaped(
    config: &Config,
    identity: &ProviderIdentity,
    provider_config: Option<&ProviderConfig>,
) -> Option<bool> {
    let is_plan_shaped = |key: &str| key.trim_start().starts_with("sk-cp");
    // 1. An explicit `[providers.minimax*] api_key` is file-owned route truth.
    if let Some(key) = provider_config
        .and_then(|config| config.api_key.as_deref())
        .filter(|key| {
            crate::config::classify_config_api_key_value(key)
                == crate::config::ConfigApiKeyValueKind::Literal
        })
        .map(str::trim)
    {
        return Some(is_plan_shaped(key));
    }
    // 2. `api_key_env = "…"` binds one variable to this route by name, so the
    //    binding itself is config-owned provenance even though the value is
    //    ambient.
    if let Some(value) = provider_config
        .and_then(|config| config.api_key_env.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
    {
        return Some(is_plan_shaped(&value));
    }
    // 3. Ambient `MINIMAX_API_KEY` only describes the route when the route is
    //    still an official MiniMax endpoint. Credential resolution refuses to
    //    send ambient provider keys to a custom host, so on a custom endpoint
    //    the exported key proves nothing about what this route bills.
    if config.provider_uses_custom_endpoint(identity) {
        return None;
    }
    std::env::var("MINIMAX_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .map(|key| is_plan_shaped(&key))
}

fn xiaomi_is_explicit_pay_as_you_go(config: Option<&ProviderConfig>) -> bool {
    if let Some(mode) = std::env::var("XIAOMI_MIMO_MODE")
        .ok()
        .filter(|mode| !mode.trim().is_empty())
        .map(|mode| normalized(&mode))
    {
        return matches!(
            mode.as_str(),
            "standard" | "default" | "payg" | "paygo" | "pay_as_you_go" | "pay_as_go"
        );
    }
    if let Some(base_url) = std::env::var("XIAOMI_MIMO_BASE_URL")
        .ok()
        .filter(|base_url| !base_url.trim().is_empty())
    {
        return !base_url.to_ascii_lowercase().contains("token-plan-");
    }
    let token_plan_env = ["XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "MIMO_TOKEN_PLAN_API_KEY"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()));
    let standard_env = ["XIAOMI_MIMO_API_KEY", "XIAOMI_API_KEY", "MIMO_API_KEY"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()));
    if standard_env && !token_plan_env {
        return true;
    }
    let Some(config) = config else {
        // The shipped MiMo default is a token-plan endpoint.
        return false;
    };
    if let Some(mode) = config
        .mode
        .as_deref()
        .filter(|mode| !mode.trim().is_empty())
        .map(normalized)
    {
        return matches!(
            mode.as_str(),
            "pay_as_you_go" | "payg" | "paygo" | "api" | "standard" | "default"
        );
    }
    if let Some(api_key) = config.api_key.as_deref().filter(|key| {
        crate::config::classify_config_api_key_value(key)
            == crate::config::ConfigApiKeyValueKind::Literal
    }) {
        return !api_key.trim_start().starts_with("tp-");
    }
    config.base_url.as_deref().is_some_and(|base_url| {
        let lower = base_url.to_ascii_lowercase();
        !lower.contains("token-plan-") && !lower.contains("token_plan_")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::CostCurrency;
    use codewhale_models::Usage;

    fn config_with(provider: ProviderKind, provider_config: ProviderConfig) -> Config {
        let mut config = Config::default();
        *config
            .provider_config_for_mut(&config.test_identity_for_kind(provider))
            .unwrap() = provider_config;
        config
    }

    /// Clear every variable that could otherwise supply a Moonshot endpoint,
    /// so the resolver has to answer from the config alone.
    fn moonshot_endpoint_env_lock() -> [crate::test_support::EnvVarGuard; 4] {
        [
            crate::test_support::EnvVarGuard::remove("CODEWHALE_BASE_URL"),
            crate::test_support::EnvVarGuard::remove("DEEPSEEK_BASE_URL"),
            crate::test_support::EnvVarGuard::remove("MOONSHOT_BASE_URL"),
            crate::test_support::EnvVarGuard::remove("KIMI_BASE_URL"),
        ]
    }

    #[test]
    fn imported_token_moonshot_without_table_base_url_bills_membership_quota() {
        let _lock = crate::test_support::lock_test_env();
        let _env = moonshot_endpoint_env_lock();
        // An imported Kimi Code token with no `base_url` in its table. The
        // table field is empty, but the route still resolves to the exact
        // membership endpoint, so classifying from the raw field would call a
        // membership quota metered and invent dollars against it.
        let config = config_with(
            ProviderKind::Moonshot,
            ProviderConfig {
                auth_mode: Some("kimi_oauth".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert_eq!(
            config.base_url_for_route(&config.test_identity_for_kind(ProviderKind::Moonshot)),
            crate::config::DEFAULT_KIMI_CODE_BASE_URL
        );

        let billing = for_route(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Moonshot),
        );
        assert_eq!(
            billing,
            BillingPresentation::Subscription("Kimi Code quota")
        );
        assert!(!billing.shows_money());

        let chip = usage_chip(
            billing,
            ProviderKind::Moonshot,
            crate::config::DEFAULT_KIMI_CODE_MODEL,
            12.34,
            CostCurrency::Usd,
            None,
        );
        assert!(!matches!(chip, UsageChip::Money(_)));
        assert_eq!(
            format_usage_chip(&chip, codewhale_localization::Locale::En).as_deref(),
            Some("usage: Kimi Code quota")
        );
        // The label names the membership product, never the credential import
        // mechanism, and never a dollar figure.
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains("OAuth")
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains("imported token")
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn turn_complete_kimi_code_receipt_accrues_no_dollars() {
        let _lock = crate::test_support::lock_test_env();
        let _env = moonshot_endpoint_env_lock();
        // Pins the exact decision the `EngineEvent::TurnComplete` arm makes:
        // classify from the event's immutable `base_url` receipt, then accrue
        // only when the result shows money. The ambient config deliberately
        // points at a *different* provider to prove the arm cannot re-resolve
        // its way onto another route's price list.
        let mut config = config_with(
            ProviderKind::Deepseek,
            ProviderConfig {
                api_key: Some("sk-session-deepseek".to_string()),
                ..ProviderConfig::default()
            },
        );
        config.provider = Some("deepseek".to_string());

        let billing = for_dispatched_route(
            &config,
            DispatchedRoute {
                provider: ProviderKind::Moonshot,
                base_url: "https://api.kimi.com/coding/v1",
            },
        );
        assert_eq!(
            billing,
            BillingPresentation::Subscription("Kimi Code quota")
        );
        // `shows_money()` is the gate guarding `accrue_session_cost_estimate`.
        assert!(!billing.shows_money());

        // A missing receipt must not fall back to the session's metered route.
        let no_receipt = for_dispatched_route(
            &config,
            DispatchedRoute {
                provider: ProviderKind::Moonshot,
                base_url: "",
            },
        );
        assert_eq!(no_receipt, BillingPresentation::Unknown);
        assert!(!no_receipt.shows_money());
    }

    #[test]
    fn moonshot_ambient_and_dispatch_billing_agree_on_the_resolved_endpoint() {
        let _lock = crate::test_support::lock_test_env();
        let _env = moonshot_endpoint_env_lock();
        let cases = [
            // (table base_url, auth_mode, expected)
            (
                None,
                Some("kimi_oauth"),
                BillingPresentation::Subscription("Kimi Code quota"),
            ),
            (None, None, BillingPresentation::Metered),
            (
                Some("https://api.kimi.com/coding/v1"),
                None,
                BillingPresentation::Subscription("Kimi Code quota"),
            ),
            (
                Some("https://api.moonshot.ai/v1"),
                None,
                BillingPresentation::Metered,
            ),
            (
                Some("https://proxy.example.test/v1"),
                None,
                BillingPresentation::Unknown,
            ),
        ];
        for (base_url, auth_mode, expected) in cases {
            let config = config_with(
                ProviderKind::Moonshot,
                ProviderConfig {
                    base_url: base_url.map(str::to_string),
                    auth_mode: auth_mode.map(str::to_string),
                    ..ProviderConfig::default()
                },
            );
            let resolved =
                config.base_url_for_route(&config.test_identity_for_kind(ProviderKind::Moonshot));
            let ambient = for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot),
            );
            let dispatched = for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Moonshot,
                    base_url: &resolved,
                },
            );
            assert_eq!(ambient, expected, "{base_url:?}/{auth_mode:?}");
            assert_eq!(
                ambient, dispatched,
                "{base_url:?}/{auth_mode:?} resolved to {resolved}: the pre-dispatch and \
                 receipt classifications must not be able to disagree"
            );
        }
    }

    #[test]
    fn moonshot_custom_gateway_is_unknown_not_metered() {
        let _lock = crate::test_support::lock_test_env();
        let _env = moonshot_endpoint_env_lock();
        // A Moonshot-compatible gateway sells its own product on its own
        // terms. Inheriting Moonshot's metered price list would invent
        // dollars; inheriting a membership label would invent a quota.
        for base_url in [
            "https://proxy.example.test/v1",
            "https://gateway.internal.test/moonshot/v1",
        ] {
            let config = config_with(
                ProviderKind::Moonshot,
                ProviderConfig {
                    base_url: Some(base_url.to_string()),
                    ..ProviderConfig::default()
                },
            );
            let billing = for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot),
            );
            assert_eq!(
                billing,
                BillingPresentation::Unknown,
                "{base_url} must not inherit a Moonshot product"
            );
            assert!(!billing.shows_money());
            let chip = usage_chip(
                billing,
                ProviderKind::Moonshot,
                "kimi-k2.7-code",
                12.34,
                CostCurrency::Usd,
                None,
            );
            assert!(!matches!(chip, UsageChip::Money(_)));
            assert!(
                !format_usage_chip(&chip, codewhale_localization::Locale::En)
                    .unwrap_or_default()
                    .contains('$')
            );
        }
    }

    #[test]
    fn moonshot_direct_platform_stays_metered_with_priced_model() {
        let config = config_with(
            ProviderKind::Moonshot,
            ProviderConfig {
                base_url: Some("https://api.moonshot.ai/v1".to_string()),
                ..ProviderConfig::default()
            },
        );
        let billing = for_route(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Moonshot),
        );
        assert_eq!(billing, BillingPresentation::Metered);
        assert!(billing.shows_money());
        let chip = usage_chip(
            billing,
            ProviderKind::Moonshot,
            "kimi-k2.7-code",
            0.42,
            CostCurrency::Usd,
            None,
        );
        assert!(matches!(chip, UsageChip::Money(_)));
        assert!(
            format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn moonshot_exact_kimi_code_endpoint_is_subscription_quota() {
        let config = config_with(
            ProviderKind::Moonshot,
            ProviderConfig {
                base_url: Some("https://api.kimi.com/coding/v1".to_string()),
                ..ProviderConfig::default()
            },
        );
        let billing = for_route(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Moonshot),
        );
        assert_eq!(
            billing,
            BillingPresentation::Subscription("Kimi Code quota")
        );
        assert!(!billing.shows_money());
        // `kimi-k2.7-code` is priced on the metered route; the subscription
        // classification must still win over the priced row.
        let chip = usage_chip(
            billing,
            ProviderKind::Moonshot,
            "kimi-k2.7-code",
            12.34,
            CostCurrency::Usd,
            None,
        );
        assert!(!matches!(chip, UsageChip::Money(_)));
        assert_eq!(
            chip,
            UsageChip::Allowance {
                label: "Kimi Code quota",
                used_pct: None,
            }
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn moonshot_neighboring_kimi_paths_are_unknown_not_metered() {
        let _lock = crate::test_support::lock_test_env();
        let _env = moonshot_endpoint_env_lock();
        // A Kimi-hosted path that is not the exact membership endpoint names
        // no product we can stand behind. It must claim neither the Kimi Code
        // quota nor Moonshot's metered price list — the pre-dispatch and
        // receipt answers are the same fail-closed Unknown.
        for base_url in [
            "https://api.kimi.com/coding/v2",
            "https://api.kimi.com/v1",
            "https://api.kimi.com/coding/v1/preview",
        ] {
            let config = config_with(
                ProviderKind::Moonshot,
                ProviderConfig {
                    base_url: Some(base_url.to_string()),
                    ..ProviderConfig::default()
                },
            );
            let billing = for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot),
            );
            assert_eq!(
                billing,
                BillingPresentation::Unknown,
                "{base_url} must claim neither Kimi Code quota nor metered dollars"
            );
            assert!(!billing.shows_money());
            assert_eq!(
                billing,
                for_dispatched_route(
                    &config,
                    DispatchedRoute {
                        provider: ProviderKind::Moonshot,
                        base_url,
                    },
                )
            );
        }
    }

    /// The second release blocker. `apply_env_overrides` merges
    /// `MOONSHOT_BASE_URL`/`KIMI_BASE_URL` into the ACTIVE provider's table
    /// only, so a Moonshot child spawned from (say) a DeepSeek session has an
    /// empty `[providers.moonshot]` entry no matter what the operator
    /// exported. Re-reading that config calls a membership route metered;
    /// the dispatch receipt — the endpoint the child's client was actually
    /// built with — tells the truth.
    #[test]
    fn dispatched_moonshot_receipt_owns_billing_over_any_later_config_state() {
        let _lock = crate::test_support::lock_test_env();
        // Env-only endpoint selection: nothing is in the provider table.
        let _generic = crate::test_support::EnvVarGuard::remove("CODEWHALE_BASE_URL");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_BASE_URL");
        let _moonshot = crate::test_support::EnvVarGuard::remove("MOONSHOT_BASE_URL");
        let _kimi = crate::test_support::EnvVarGuard::set(
            "KIMI_BASE_URL",
            "https://api.kimi.com/coding/v1",
        );
        let config = config_with(ProviderKind::Moonshot, ProviderConfig::default());

        // The pre-dispatch answer resolves the same env-selected endpoint
        // instead of reading the empty provider table — that blind spot is
        // what let an imported-token membership route look metered.
        assert_eq!(
            for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot)
            ),
            BillingPresentation::Subscription("Kimi Code quota")
        );

        // A receipt still wins outright. A turn dispatched on the direct
        // platform bills metered even though the config resolves to the
        // membership host now.
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Moonshot,
                    base_url: "https://api.moonshot.ai/v1",
                },
            ),
            BillingPresentation::Metered,
            "the endpoint the turn actually dispatched to owns its billing"
        );
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Moonshot,
                    base_url: "https://api.kimi.com/coding/v1",
                },
            ),
            BillingPresentation::Subscription("Kimi Code quota")
        );
    }

    /// A dispatched endpoint must NAME a known product. The exact direct
    /// platform is metered; a gateway host, a neighboring Kimi path, and a
    /// blank receipt are all ambiguous and fail closed.
    #[test]
    fn dispatched_moonshot_endpoint_must_name_a_known_product() {
        assert_eq!(
            for_dispatched_route(
                &Config::default(),
                DispatchedRoute {
                    provider: ProviderKind::Moonshot,
                    base_url: "https://api.moonshot.ai/v1",
                },
            ),
            BillingPresentation::Metered
        );
        for ambiguous in [
            "",
            "   ",
            "https://api.kimi.com/v1",
            "https://api.kimi.com/coding/v1/preview",
            "https://gateway.internal.example/v1",
        ] {
            let billing = for_dispatched_route(
                &Config::default(),
                DispatchedRoute {
                    provider: ProviderKind::Moonshot,
                    base_url: ambiguous,
                },
            );
            assert_eq!(
                billing,
                BillingPresentation::Unknown,
                "{ambiguous:?} names no Moonshot product"
            );
            assert!(!billing.shows_money());
        }
    }

    #[test]
    fn chatgpt_plan_never_claims_api_dollars() {
        let billing = for_dispatched_receipt(DispatchedReceipt {
            provider: ProviderKind::OpenaiCodex,
            identity: Some("openai_codex"),
            base_url: "https://api.openai.com/v1",
            product: RouteProduct::Subscription("ChatGPT plan allowance"),
        });
        assert_eq!(
            billing,
            BillingPresentation::Subscription("ChatGPT plan allowance")
        );
        let chip = usage_chip(
            billing,
            ProviderKind::OpenaiCodex,
            "gpt-5.5",
            12.34,
            CostCurrency::Usd,
            None,
        );
        assert_eq!(
            format_usage_chip(&chip, codewhale_localization::Locale::En).as_deref(),
            Some("usage: ChatGPT plan allowance")
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn chatgpt_plan_requires_captured_official_grant_and_endpoint() {
        let receipt = DispatchedReceipt {
            provider: ProviderKind::OpenaiCodex,
            identity: Some("openai_codex"),
            base_url: "https://api.openai.com/v1",
            product: RouteProduct::Subscription("ChatGPT plan allowance"),
        };
        for product in [
            RouteProduct::Unproven,
            RouteProduct::Metered,
            RouteProduct::Subscription("Codex OAuth quota"),
        ] {
            assert_eq!(
                for_dispatched_receipt(DispatchedReceipt { product, ..receipt }),
                BillingPresentation::Unknown,
                "{product:?} is not an official ChatGPT grant"
            );
        }
        for endpoint in [
            "https://gateway.example/v1",
            "https://api.openai.com.example.net/v1",
            "https://api.openai.com/v1/preview",
            "https://api.openai.com/v1?billing=plan",
            "https://api.openai.com/v1#plan",
            "https://user:secret@api.openai.com/v1",
            "http://api.openai.com/v1",
            "",
        ] {
            assert_eq!(
                for_dispatched_receipt(DispatchedReceipt {
                    base_url: endpoint,
                    ..receipt
                }),
                BillingPresentation::Unknown,
                "{endpoint} is not the official plan endpoint"
            );
        }
        assert_eq!(
            for_dispatched_receipt(DispatchedReceipt {
                provider: ProviderKind::Openai,
                identity: Some("openai"),
                ..receipt
            }),
            BillingPresentation::Metered,
            "API-key provider must not inherit ChatGPT plan allowance"
        );
        assert_eq!(
            for_dispatched_receipt(receipt),
            BillingPresentation::Subscription("ChatGPT plan allowance"),
            "receipt interpretation needs no ambient config or credentials"
        );
    }

    #[test]
    fn chatgpt_plan_without_owned_grant_stays_unknown() {
        let config = Config::default();
        assert_eq!(
            for_route_with_endpoint(
                &config,
                &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
                "https://api.openai.com/v1"
            ),
            BillingPresentation::Unknown
        );
        assert_eq!(
            for_endpoint_without_config(
                ProviderKind::OpenaiCodex,
                Some("https://api.openai.com/v1")
            ),
            BillingPresentation::Unknown
        );
        assert_eq!(
            billing_surface_for_dispatch(
                Some(&config),
                &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
                Some("https://api.openai.com/v1")
            ),
            Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE)
        );
    }

    #[test]
    fn xai_api_key_fallback_is_metered_when_external_oauth_is_unavailable() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("xAI billing fixture");
        let grok_path = temp.path().join("external-grok-auth.json");
        std::fs::write(&grok_path, "must-never-be-read").expect("external trap");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let _grok = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &grok_path);

        let config = config_with(
            ProviderKind::Xai,
            ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                api_key: Some("xai-api-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        crate::external_credentials::reset_side_effect_trap();
        assert_eq!(
            for_route(&config, &(config).test_identity_for_kind(ProviderKind::Xai)),
            BillingPresentation::Metered
        );
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (0, 0)
        );
        assert_eq!(
            std::fs::read_to_string(grok_path).expect("external trap unchanged"),
            "must-never-be-read"
        );
    }

    #[test]
    fn opencode_go_quota_never_claims_token_dollars() {
        let billing = for_route(
            &Config::default(),
            &(Config::default()).test_identity_for_kind(ProviderKind::OpencodeGo),
        );
        assert_eq!(
            billing,
            BillingPresentation::Subscription("OpenCode Go quota")
        );
        let chip = usage_chip(
            billing,
            ProviderKind::OpencodeGo,
            "deepseek-v4-pro",
            12.34,
            CostCurrency::Usd,
            None,
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::OpencodeGo,
                None,
            ),
            BillingPresentation::Unknown,
            "provider identity alone must not claim OpenCode Go quota"
        );
        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::OpencodeGo,
                Some(BillingPresentation::Subscription("OpenCode Go quota")),
            ),
            BillingPresentation::Subscription("OpenCode Go quota"),
            "the child's own route truth is what may claim the quota"
        );
    }

    #[test]
    fn zai_coding_plan_endpoint_never_claims_api_dollars() {
        let config = config_with(
            ProviderKind::Zai,
            ProviderConfig {
                base_url: Some("https://api.z.ai/api/coding/paas/v4".to_string()),
                ..ProviderConfig::default()
            },
        );
        let billing = for_route(&config, &(config).test_identity_for_kind(ProviderKind::Zai));
        assert_eq!(
            billing,
            BillingPresentation::Subscription("Z.ai Coding Plan quota")
        );
        let chip = usage_chip(
            billing,
            ProviderKind::Zai,
            "glm-5.2",
            0.05,
            CostCurrency::Usd,
            None,
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn zai_default_coding_endpoint_never_claims_api_dollars() {
        // The route resolves its shipped default, so the ambient generic
        // endpoint override has to be locked out for the assertion to be
        // about the default at all.
        let _lock = crate::test_support::lock_test_env();
        let _generic = crate::test_support::EnvVarGuard::remove("CODEWHALE_BASE_URL");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_BASE_URL");
        let config = config_with(ProviderKind::Zai, ProviderConfig::default());
        assert_eq!(
            for_route(&config, &(config).test_identity_for_kind(ProviderKind::Zai)),
            BillingPresentation::Subscription("Z.ai Coding Plan quota")
        );
    }

    #[test]
    fn stepfun_payg_shows_money_but_step_plan_stays_subscription_billed() {
        // Same reason as the Z.ai default test: the PAYG half asserts against
        // StepFun's shipped default endpoint.
        let _lock = crate::test_support::lock_test_env();
        let _generic = crate::test_support::EnvVarGuard::remove("CODEWHALE_BASE_URL");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_BASE_URL");
        let payg_billing = for_route(
            &Config::default(),
            &(Config::default()).test_identity_for_kind(ProviderKind::Stepfun),
        );
        assert_eq!(payg_billing, BillingPresentation::Metered);
        let payg_chip = usage_chip(
            payg_billing,
            ProviderKind::Stepfun,
            crate::config::DEFAULT_STEPFUN_MODEL,
            0.42,
            CostCurrency::Usd,
            None,
        );
        assert_eq!(
            format_usage_chip(&payg_chip, codewhale_localization::Locale::En).as_deref(),
            Some("$0.42")
        );

        let plan_config = config_with(
            ProviderKind::Stepfun,
            ProviderConfig {
                base_url: Some("https://api.stepfun.ai/step_plan/v1".to_string()),
                ..ProviderConfig::default()
            },
        );
        let plan_billing = for_route(
            &plan_config,
            &(plan_config).test_identity_for_kind(ProviderKind::Stepfun),
        );
        assert_eq!(
            plan_billing,
            BillingPresentation::Subscription("StepFun Step Plan quota")
        );
        let plan_chip = usage_chip(
            plan_billing,
            ProviderKind::Stepfun,
            crate::config::DEFAULT_STEPFUN_MODEL,
            0.42,
            CostCurrency::Usd,
            None,
        );
        assert!(
            !format_usage_chip(&plan_chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );

        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Stepfun,
                None,
            ),
            BillingPresentation::Unknown
        );
    }

    /// A dual-mode child provider with no dispatch config is *unknown*, not a
    /// subscription. It still never shows dollars, but the distinction is what
    /// keeps its spend inside `/cost`'s coverage denominator instead of being
    /// excused as quota-billed (#4318).
    #[test]
    fn routed_zai_child_never_claims_api_dollars_without_full_route_config() {
        let billing = for_child_route(
            ProviderKind::Deepseek,
            BillingPresentation::Metered,
            ProviderKind::Zai,
            None,
        );
        assert_eq!(
            billing,
            BillingPresentation::Unknown,
            "without the child's route truth, fail closed instead of guessing a quota"
        );
        assert!(!billing.shows_money());
        assert_eq!(billing.label(), Some("unknown"));
    }

    /// Child-route billing for each shape a child can take. Without the
    /// child's own provenance, only a local runtime is exactly non-metered;
    /// every other cross-provider child fails closed to Unknown, and Unknown
    /// (unlike a subscription label) keeps the turn inside `/cost`'s money
    /// coverage denominator instead of excusing it as quota-billed (#4318).
    #[test]
    fn child_route_billing_fails_closed_for_every_ambiguous_provider() {
        use crate::pricing::UnpricedReason;

        let usage = codewhale_models::Usage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            ..Default::default()
        };
        let now = chrono::Utc::now();

        // Nothing about a provider name — not an aggregator, not a first-party
        // PAYG API, not an OAuth-only broker — is evidence of what this child
        // turn billed. Every one of them is Unknown without provenance, and
        // the cost audit counts them toward money coverage rather than
        // excusing them.
        for provider in [
            ProviderKind::Openrouter,
            ProviderKind::Openai,
            ProviderKind::Zai,
            ProviderKind::Moonshot,
            ProviderKind::Anthropic,
            ProviderKind::XiaomiMimo,
            ProviderKind::Xai,
            ProviderKind::Minimax,
            ProviderKind::MinimaxAnthropic,
            ProviderKind::Stepfun,
            ProviderKind::Custom,
            ProviderKind::OpenaiCodex,
            ProviderKind::OpencodeGo,
        ] {
            let billing = for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                provider,
                None,
            );
            assert_eq!(billing, BillingPresentation::Unknown, "{provider:?}");
            assert!(!billing.shows_money(), "{provider:?}");
            let audit = crate::pricing::audit_turn_cost_for_route(
                provider,
                "some-model",
                None,
                &usage,
                now,
                billing,
            );
            assert_eq!(
                audit.unpriced_reason,
                Some(UnpricedReason::UnknownBillingBasis),
                "{provider:?}"
            );
            assert!(
                audit.counts_toward_money_coverage(),
                "{provider:?} must stay in the coverage denominator"
            );
        }

        // A local runtime has no provider bill under any configuration, so it
        // is exactly non-metered and is excluded from money coverage.
        for provider in [
            ProviderKind::Ollama,
            ProviderKind::Sglang,
            ProviderKind::Vllm,
        ] {
            let billing = for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                provider,
                None,
            );
            assert_eq!(billing, BillingPresentation::Local, "{provider:?}");
            let audit = crate::pricing::audit_turn_cost_for_route(
                provider,
                "some-model",
                None,
                &usage,
                now,
                billing,
            );
            assert_eq!(
                audit.unpriced_reason,
                Some(UnpricedReason::NotMoneyMetered),
                "{provider:?}"
            );
            assert!(!audit.counts_toward_money_coverage(), "{provider:?}");
        }

        // A child on the parent's own provider ran the parent's exact route,
        // so it inherits the parent's frozen billing — the one inheritance
        // that is a fact rather than a guess.
        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Deepseek,
                None,
            ),
            BillingPresentation::Metered
        );

        // The child's own captured provenance is the only thing that prices
        // (or excuses) the route.
        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Openrouter,
                Some(BillingPresentation::Metered),
            ),
            BillingPresentation::Metered
        );
        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Anthropic,
                Some(BillingPresentation::Subscription("Claude OAuth quota")),
            ),
            BillingPresentation::Subscription("Claude OAuth quota")
        );
    }

    #[test]
    fn oauth_allowance_percent_is_shown_when_provider_supplies_it() {
        let chip = usage_chip(
            BillingPresentation::Subscription("Grok OAuth quota"),
            ProviderKind::Xai,
            "grok-4",
            0.0,
            CostCurrency::Usd,
            Some(37.0),
        );
        assert_eq!(
            format_usage_chip(&chip, codewhale_localization::Locale::En).as_deref(),
            Some("usage: Grok OAuth quota · 37%")
        );
    }

    #[test]
    fn api_key_metered_shows_dollars_only_with_priced_positive_spend() {
        let billing = BillingPresentation::Metered;
        assert!(has_priced_metered_basis(
            billing,
            ProviderKind::Deepseek,
            "deepseek-v4-flash"
        ));
        let spent = usage_chip(
            billing,
            ProviderKind::Deepseek,
            "deepseek-v4-flash",
            0.42,
            CostCurrency::Usd,
            None,
        );
        assert_eq!(
            format_usage_chip(&spent, codewhale_localization::Locale::En).as_deref(),
            Some("$0.42")
        );

        let zero = usage_chip(
            billing,
            ProviderKind::Deepseek,
            "deepseek-v4-flash",
            0.0,
            CostCurrency::Usd,
            None,
        );
        assert_eq!(zero, UsageChip::Hidden);
        assert!(format_usage_chip(&zero, codewhale_localization::Locale::En).is_none());
        assert!(
            !format_usage_chip(&zero, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn local_free_routes_never_show_dollars() {
        assert_eq!(
            for_route(
                &Config::default(),
                &(Config::default()).test_identity_for_kind(ProviderKind::Ollama)
            ),
            BillingPresentation::Local
        );
        let chip = usage_chip(
            BillingPresentation::Local,
            ProviderKind::Ollama,
            "llama3.2",
            9.99,
            CostCurrency::Usd,
            None,
        );
        assert_eq!(
            format_usage_chip(&chip, codewhale_localization::Locale::En).as_deref(),
            Some("cost: local")
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn ollama_cloud_is_unknown_and_counts_as_possible_spend() {
        let config = Config {
            provider: Some("ollama-cloud".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                ollama_cloud: crate::config::ProviderConfig {
                    api_key: Some("cloud-key".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let billing = for_route(
            &config,
            &(config).test_identity_for_kind(ProviderKind::OllamaCloud),
        );
        assert_eq!(billing, BillingPresentation::Unknown);
        assert!(!billing.shows_money());

        let audit = crate::pricing::audit_turn_cost_for_route(
            ProviderKind::OllamaCloud,
            crate::config::DEFAULT_OLLAMA_CLOUD_MODEL,
            Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
            &Usage {
                input_tokens: 1_000,
                output_tokens: 100,
                ..Usage::default()
            },
            chrono::Utc::now(),
            billing,
        );
        assert_eq!(
            audit.unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnknownBillingBasis)
        );
        assert!(audit.counts_toward_money_coverage());
    }

    #[test]
    fn unknown_is_unknown_not_zero_dollars() {
        let chip = usage_chip(
            BillingPresentation::Metered,
            ProviderKind::NvidiaNim,
            "deepseek-ai/deepseek-v4-pro",
            0.0,
            CostCurrency::Usd,
            None,
        );
        assert_eq!(chip, UsageChip::Unknown(vec![UnpricedReason::NoPricingRow]));
        assert_eq!(
            format_usage_chip(&chip, codewhale_localization::Locale::En).as_deref(),
            Some("cost: unknown (rate unavailable)")
        );
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );

        let unknown_billing = usage_chip(
            BillingPresentation::Unknown,
            ProviderKind::Custom,
            "anything",
            1.23,
            CostCurrency::Usd,
            None,
        );
        assert_eq!(
            unknown_billing,
            UsageChip::Unknown(vec![UnpricedReason::UnknownBillingBasis])
        );
        assert!(
            !format_usage_chip(&unknown_billing, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn xai_oauth_and_api_key_routes_stay_distinct() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("xAI owned credential fixture");
        let owned_home = temp
            .path()
            .canonicalize()
            .expect("canonical xAI owned credential fixture");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &owned_home);
        let owned_path = owned_home.join("credentials/xai-auth.json");
        std::fs::create_dir_all(owned_path.parent().expect("owned credential parent"))
            .expect("create owned credential directory");
        #[cfg(windows)]
        crate::external_credentials::secure_codewhale_owned_windows_path(
            owned_path.parent().expect("owned credential parent"),
            true,
        )
        .expect("secure owned credential directory");
        let scope = format!(
            "{}::{}",
            crate::oauth::XAI_OIDC_ISSUER,
            crate::oauth::GROK_OIDC_CLIENT_ID
        );
        std::fs::write(
            &owned_path,
            serde_json::json!({
                scope: {
                    "key": crate::test_support::future_test_jwt("billing"),
                    "auth_mode": "oidc"
                }
            })
            .to_string(),
        )
        .expect("write Codewhale-owned xAI credential");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&owned_path, std::fs::Permissions::from_mode(0o600))
                .expect("secure owned credential file");
        }
        #[cfg(windows)]
        crate::external_credentials::secure_codewhale_owned_windows_path(&owned_path, false)
            .expect("secure owned credential file");
        let oauth = config_with(
            ProviderKind::Xai,
            ProviderConfig {
                auth_mode: Some("grok-oauth".to_string()),
                ..ProviderConfig::default()
            },
        );
        let api = config_with(
            ProviderKind::Xai,
            ProviderConfig {
                auth_mode: Some("api-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert!(
            !for_route(&oauth, &(oauth).test_identity_for_kind(ProviderKind::Xai)).shows_money()
        );
        assert!(for_route(&api, &(api).test_identity_for_kind(ProviderKind::Xai)).shows_money());
    }

    #[test]
    fn future_claude_oauth_does_not_inherit_anthropic_api_prices() {
        let oauth = config_with(
            ProviderKind::Anthropic,
            ProviderConfig {
                auth_mode: Some("claude-code".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert_eq!(
            for_route(
                &oauth,
                &(oauth).test_identity_for_kind(ProviderKind::Anthropic)
            )
            .label(),
            Some("Claude OAuth quota")
        );
    }

    #[test]
    fn xiaomi_defaults_to_token_plan_but_explicit_payg_is_metered() {
        let _lock = crate::test_support::lock_test_env();
        let _mode = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_MODE");
        let _base = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_BASE_URL");
        let _token = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_TOKEN_PLAN_API_KEY");
        let _token_alias = crate::test_support::EnvVarGuard::remove("MIMO_TOKEN_PLAN_API_KEY");
        let _standard_a = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_API_KEY");
        let _standard_b = crate::test_support::EnvVarGuard::remove("XIAOMI_API_KEY");
        let _standard_c = crate::test_support::EnvVarGuard::remove("MIMO_API_KEY");
        assert!(
            !for_route(
                &Config::default(),
                &(Config::default()).test_identity_for_kind(ProviderKind::XiaomiMimo)
            )
            .shows_money()
        );
        let payg = config_with(
            ProviderKind::XiaomiMimo,
            ProviderConfig {
                mode: Some("pay-as-you-go".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert!(
            for_route(
                &payg,
                &(payg).test_identity_for_kind(ProviderKind::XiaomiMimo)
            )
            .shows_money()
        );
        let standard_key = config_with(
            ProviderKind::XiaomiMimo,
            ProviderConfig {
                api_key: Some("sk-standard".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert!(
            for_route(
                &standard_key,
                &(standard_key).test_identity_for_kind(ProviderKind::XiaomiMimo)
            )
            .shows_money()
        );
    }

    #[test]
    fn minimax_requires_an_explicit_saved_billing_mode() {
        let _lock = crate::test_support::lock_test_env();
        let _env = minimax_env_guard();
        for provider in [ProviderKind::Minimax, ProviderKind::MinimaxAnthropic] {
            assert_eq!(
                for_route(
                    &Config::default(),
                    &(Config::default()).test_identity_for_kind(provider)
                ),
                BillingPresentation::Unknown
            );
            assert_eq!(
                for_endpoint_without_config(provider, Some(provider.provider().default_base_url())),
                BillingPresentation::Unknown
            );

            let payg = config_with(
                provider,
                ProviderConfig {
                    mode: Some("pay-as-you-go".to_string()),
                    ..ProviderConfig::default()
                },
            );
            assert_eq!(
                for_route(&payg, &(payg).test_identity_for_kind(provider)),
                BillingPresentation::Metered
            );
            assert_eq!(
                billing_surface_for_dispatch(
                    Some(&payg),
                    &(payg).test_identity_for_kind(provider),
                    Some(provider.provider().default_base_url())
                ),
                Some(crate::pricing::MINIMAX_PAYG_BILLING_SURFACE)
            );

            let plan = config_with(
                provider,
                ProviderConfig {
                    mode: Some("subscription-plan".to_string()),
                    ..ProviderConfig::default()
                },
            );
            assert_eq!(
                for_route(&plan, &(plan).test_identity_for_kind(provider)),
                // The product's own name, not a generic "subscription plan":
                // MiniMax sells PAYG and Token Plan over the same endpoint.
                BillingPresentation::Subscription("MiniMax Token Plan quota")
            );
            assert_eq!(
                billing_surface_for_dispatch(
                    Some(&plan),
                    &(plan).test_identity_for_kind(provider),
                    Some(provider.provider().default_base_url())
                ),
                Some(crate::pricing::MINIMAX_TOKEN_PLAN_BILLING_SURFACE)
            );
        }
    }

    #[test]
    fn unknown_cross_provider_oauth_capable_child_never_invents_dollars() {
        assert!(
            !for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Xai,
                None,
            )
            .shows_money()
        );
        // Identity alone no longer claims metered dollars either: without the
        // child's own route truth a cross-provider child fails closed.
        assert!(
            !for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Openrouter,
                None,
            )
            .shows_money()
        );
        // Unknown, not an invented "provider quota" subscription.
        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Xai,
                None,
            ),
            BillingPresentation::Unknown
        );
        // The child's own metered provenance is what prices the route.
        assert!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Openrouter,
                Some(BillingPresentation::Metered),
            )
            .shows_money()
        );
    }

    #[test]
    fn standard_mimo_env_key_uses_metered_presentation() {
        let _lock = crate::test_support::lock_test_env();
        let _mode = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_MODE");
        let _base = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_BASE_URL");
        let _token = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_TOKEN_PLAN_API_KEY");
        let _token_alias = crate::test_support::EnvVarGuard::remove("MIMO_TOKEN_PLAN_API_KEY");
        let _standard_a = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_API_KEY");
        let _standard_b = crate::test_support::EnvVarGuard::remove("XIAOMI_API_KEY");
        let _standard = crate::test_support::EnvVarGuard::set("MIMO_API_KEY", "sk-metered");

        assert!(
            for_route(
                &Config::default(),
                &(Config::default()).test_identity_for_kind(ProviderKind::XiaomiMimo)
            )
            .shows_money()
        );
    }

    #[test]
    fn custom_without_pay_mode_stays_unknown() {
        let mut config = Config::from_saved_document(
            r#"provider = "acme"
[providers.acme]
kind = "openai-compatible"
base_url = "https://acme.example.test/v1"
model = "fixture-model"
"#,
            None,
        )
        .unwrap();
        let identity = config.active_provider_identity().unwrap();
        assert_eq!(for_route(&config, &identity), BillingPresentation::Unknown);
        config.provider_config_for_mut(&identity).unwrap().auth_mode = Some("api-key".into());
        assert_eq!(for_route(&config, &identity), BillingPresentation::Metered);
    }

    #[test]
    fn dispatched_endpoint_shaped_routes_classify_from_the_receipt() {
        let config = Config::default();
        // StepFun: plan endpoint, PAYG endpoint, unrecognized host.
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Stepfun,
                    base_url: "https://api.stepfun.ai/step_plan/v1",
                },
            ),
            BillingPresentation::Subscription("StepFun Step Plan quota")
        );
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Stepfun,
                    base_url: crate::config::DEFAULT_STEPFUN_BASE_URL,
                },
            ),
            BillingPresentation::Metered
        );
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Stepfun,
                    base_url: "https://gateway.internal.example/v1",
                },
            ),
            BillingPresentation::Unknown
        );
        // Z.ai: the Coding Plan path is quota-billed; a blank receipt is not
        // an excuse to fall back to the plan default.
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Zai,
                    base_url: "https://api.z.ai/api/coding/paas/v4",
                },
            ),
            BillingPresentation::Subscription("Z.ai Coding Plan quota")
        );
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Zai,
                    base_url: "",
                },
            ),
            BillingPresentation::Unknown
        );
        // Identity-owned routes are unchanged by the receipt.
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Ollama,
                    base_url: "http://localhost:11434/v1",
                },
            ),
            BillingPresentation::Local
        );
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::OpenaiCodex,
                    base_url: "https://chatgpt.com/backend-api/codex",
                },
            ),
            BillingPresentation::Subscription("Codex OAuth quota")
        );
        // The OAuth token pointed anywhere else proves no Codex quota.
        for elsewhere in [
            "https://codex-gateway.example.com/backend-api",
            "https://chatgpt.com.example.net/backend-api",
            "http://chatgpt.com/backend-api",
            "https://chatgpt.com/v1",
            "",
        ] {
            assert_eq!(
                for_dispatched_route(
                    &config,
                    DispatchedRoute {
                        provider: ProviderKind::OpenaiCodex,
                        base_url: elsewhere,
                    },
                ),
                BillingPresentation::Unknown,
                "{elsewhere:?}"
            );
            // The persisted billing surface agrees: a custom endpoint is not
            // an OAuth subscription that would drop out of money coverage.
            if !elsewhere.is_empty() {
                assert_eq!(
                    billing_surface_for_dispatch(
                        None,
                        &Config::default().test_identity_for_kind(ProviderKind::OpenaiCodex),
                        Some(elsewhere)
                    ),
                    Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
                    "{elsewhere:?}"
                );
            }
        }
        assert_eq!(
            billing_surface_for_dispatch(
                None,
                &Config::default().test_identity_for_kind(ProviderKind::OpenaiCodex),
                Some("https://chatgpt.com/backend-api/codex")
            ),
            Some(crate::pricing::OAUTH_SUBSCRIPTION_BILLING_SURFACE)
        );
    }

    #[test]
    fn minimax_defaults_to_pay_as_you_go_metered() {
        let _lock = crate::test_support::lock_test_env();
        let _key = crate::test_support::EnvVarGuard::remove("MINIMAX_API_KEY");
        let config = config_with(
            ProviderKind::Minimax,
            ProviderConfig {
                base_url: Some("https://api.minimax.io/v1".to_string()),
                api_key: Some("sk-test-payg-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        let billing = for_route(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Minimax),
        );
        assert_eq!(billing, BillingPresentation::Metered);
        assert!(billing.shows_money());
        let chip = usage_chip(
            billing,
            ProviderKind::Minimax,
            "MiniMax-M3",
            0.42,
            CostCurrency::Usd,
            None,
        );
        assert!(matches!(chip, UsageChip::Money(_)));
        assert!(
            format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn minimax_explicit_token_plan_mode_is_subscription_quota() {
        let _lock = crate::test_support::lock_test_env();
        let _key = crate::test_support::EnvVarGuard::remove("MINIMAX_API_KEY");
        let config = config_with(
            ProviderKind::Minimax,
            ProviderConfig {
                mode: Some("token-plan".to_string()),
                api_key: Some("sk-test-payg-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        let billing = for_route(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Minimax),
        );
        assert_eq!(
            billing,
            BillingPresentation::Subscription("MiniMax Token Plan quota")
        );
        assert!(!billing.shows_money());
        // `MiniMax-M3` is priced on the metered route; the subscription
        // classification must still win over the priced row.
        let chip = usage_chip(
            billing,
            ProviderKind::Minimax,
            "MiniMax-M3",
            12.34,
            CostCurrency::Usd,
            None,
        );
        assert!(!matches!(chip, UsageChip::Money(_)));
        assert!(
            !format_usage_chip(&chip, codewhale_localization::Locale::En)
                .unwrap_or_default()
                .contains('$')
        );
    }

    #[test]
    fn minimax_sk_cp_config_key_is_subscription_quota() {
        let _lock = crate::test_support::lock_test_env();
        let _key = crate::test_support::EnvVarGuard::remove("MINIMAX_API_KEY");
        let config = config_with(
            ProviderKind::Minimax,
            ProviderConfig {
                api_key: Some("sk-cp-test-token-plan-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        let billing = for_route(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Minimax),
        );
        assert_eq!(
            billing,
            BillingPresentation::Subscription("MiniMax Token Plan quota")
        );
        assert!(!billing.shows_money());
    }

    /// The Anthropic-dialect MiniMax route is the same product behind a
    /// different wire protocol: same MINIMAX_API_KEY, same PAYG/Token Plan
    /// duality. Classifying only the chat-completions dialect would show
    /// invented dollars for a Token Plan key on `[providers.minimax_anthropic]`.
    #[test]
    fn minimax_anthropic_dialect_shares_the_token_plan_classification() {
        let _lock = crate::test_support::lock_test_env();
        let _key = crate::test_support::EnvVarGuard::remove("MINIMAX_API_KEY");

        let plan = config_with(
            ProviderKind::MinimaxAnthropic,
            ProviderConfig {
                api_key: Some("sk-cp-test-token-plan-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        let plan_billing = for_route(
            &plan,
            &(plan).test_identity_for_kind(ProviderKind::MinimaxAnthropic),
        );
        assert_eq!(
            plan_billing,
            BillingPresentation::Subscription("MiniMax Token Plan quota")
        );
        assert!(!plan_billing.shows_money());

        let explicit_plan = config_with(
            ProviderKind::MinimaxAnthropic,
            ProviderConfig {
                mode: Some("token-plan".to_string()),
                api_key: Some("sk-test-payg-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert_eq!(
            for_route(
                &explicit_plan,
                &(explicit_plan).test_identity_for_kind(ProviderKind::MinimaxAnthropic)
            ),
            BillingPresentation::Subscription("MiniMax Token Plan quota")
        );

        // Pay-as-you-go on the same dialect stays metered.
        let payg = config_with(
            ProviderKind::MinimaxAnthropic,
            ProviderConfig {
                api_key: Some("sk-test-payg-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        let payg_billing = for_route(
            &payg,
            &(payg).test_identity_for_kind(ProviderKind::MinimaxAnthropic),
        );
        assert_eq!(payg_billing, BillingPresentation::Metered);
        assert!(payg_billing.shows_money());
    }

    #[test]
    fn minimax_sk_cp_env_key_is_subscription_quota() {
        let _lock = crate::test_support::lock_test_env();
        let _key =
            crate::test_support::EnvVarGuard::set("MINIMAX_API_KEY", "sk-cp-test-token-plan-key");
        let config = config_with(ProviderKind::Minimax, ProviderConfig::default());
        assert_eq!(
            for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Minimax)
            ),
            BillingPresentation::Subscription("MiniMax Token Plan quota")
        );
    }

    #[test]
    fn minimax_explicit_pay_as_you_go_wins_over_sk_cp_key() {
        let _lock = crate::test_support::lock_test_env();
        let _key = crate::test_support::EnvVarGuard::remove("MINIMAX_API_KEY");
        for mode in ["pay-as-you-go", "payg", "metered"] {
            let config = config_with(
                ProviderKind::Minimax,
                ProviderConfig {
                    mode: Some(mode.to_string()),
                    api_key: Some("sk-cp-test-token-plan-key".to_string()),
                    ..ProviderConfig::default()
                },
            );
            let billing = for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Minimax),
            );
            assert_eq!(
                billing,
                BillingPresentation::Metered,
                "explicit mode {mode} must win over the sk-cp key shape"
            );
            assert!(billing.shows_money());
        }
    }

    /// Clear the only ambient variable `minimax_credential_product` reads, so
    /// a developer's real shell cannot decide a billing regression's outcome.
    fn minimax_env_guard() -> crate::test_support::EnvVarGuard {
        crate::test_support::EnvVarGuard::remove("MINIMAX_API_KEY")
    }

    /// The release blocker: a MiniMax key saved through `codewhale auth set`
    /// lives in the secret store, so neither the config table nor
    /// `MINIMAX_API_KEY` carries a product marker. Classification must not
    /// open the secret store to find out, and must not silently call the
    /// route pay-as-you-go — a Token Plan account would then accrue invented
    /// dollars on every benchmark receipt.
    #[test]
    fn minimax_keyring_or_opaque_credential_is_unclassified_not_metered() {
        let _lock = crate::test_support::lock_test_env();
        let _env = minimax_env_guard();
        for provider in [ProviderKind::Minimax, ProviderKind::MinimaxAnthropic] {
            // No credential visible at all (keyring/OAuth/command-sourced).
            let opaque = config_with(provider, ProviderConfig::default());
            assert_eq!(
                for_route(&opaque, &(opaque).test_identity_for_kind(provider)),
                BillingPresentation::Unknown,
                "{provider:?} must not claim pay-as-you-go it cannot prove"
            );
            // The legacy keyring placeholder is not a credential and carries
            // no product prefix.
            for sentinel in [crate::config::API_KEYRING_SENTINEL, "  __KEYRING__  "] {
                let sentinel = config_with(
                    provider,
                    ProviderConfig {
                        api_key: Some(sentinel.to_string()),
                        ..ProviderConfig::default()
                    },
                );
                assert_eq!(
                    for_route(&sentinel, &(sentinel).test_identity_for_kind(provider)),
                    BillingPresentation::Unknown,
                    "{provider:?} keyring sentinel is not a pay-as-you-go proof"
                );
            }
            let chip = usage_chip(
                for_route(&opaque, &(opaque).test_identity_for_kind(provider)),
                provider,
                "MiniMax-M3",
                12.34,
                CostCurrency::Usd,
                None,
            );
            assert_eq!(
                chip,
                UsageChip::Unknown(vec![UnpricedReason::UnknownBillingBasis])
            );
            assert!(
                !format_usage_chip(&chip, codewhale_localization::Locale::En)
                    .unwrap_or_default()
                    .contains('$')
            );
        }
    }

    /// Provenance-by-source, both dialects: config value, route-bound
    /// `api_key_env`, and ambient `MINIMAX_API_KEY` are each sufficient to
    /// prove a product, and each proves it the same way.
    #[test]
    fn minimax_credential_provenance_classifies_both_dialects_identically() {
        let _lock = crate::test_support::lock_test_env();
        let _env = minimax_env_guard();
        for provider in [ProviderKind::Minimax, ProviderKind::MinimaxAnthropic] {
            // 1. Config-owned key.
            for (key, expected) in [
                (
                    "sk-cp-plan-key",
                    BillingPresentation::Subscription("MiniMax Token Plan quota"),
                ),
                ("sk-payg-key", BillingPresentation::Metered),
            ] {
                let config = config_with(
                    provider,
                    ProviderConfig {
                        api_key: Some(key.to_string()),
                        ..ProviderConfig::default()
                    },
                );
                assert_eq!(
                    for_route(&config, &(config).test_identity_for_kind(provider)),
                    expected,
                    "{provider:?} {key}"
                );
            }

            // 2. Route-bound `api_key_env`: the binding is config-owned even
            //    though the value is ambient.
            for (key, expected) in [
                (
                    "sk-cp-plan-key",
                    BillingPresentation::Subscription("MiniMax Token Plan quota"),
                ),
                ("sk-payg-key", BillingPresentation::Metered),
            ] {
                let _bound =
                    crate::test_support::EnvVarGuard::set("CW_TEST_MINIMAX_BOUND_KEY", key);
                let config = config_with(
                    provider,
                    ProviderConfig {
                        api_key_env: Some("CW_TEST_MINIMAX_BOUND_KEY".to_string()),
                        ..ProviderConfig::default()
                    },
                );
                assert_eq!(
                    for_route(&config, &(config).test_identity_for_kind(provider)),
                    expected,
                    "{provider:?} api_key_env {key}"
                );
            }

            // 3. Ambient provider environment on an official endpoint.
            for (key, expected) in [
                (
                    "sk-cp-plan-key",
                    BillingPresentation::Subscription("MiniMax Token Plan quota"),
                ),
                ("sk-payg-key", BillingPresentation::Metered),
            ] {
                let _ambient = crate::test_support::EnvVarGuard::set("MINIMAX_API_KEY", key);
                let config = config_with(provider, ProviderConfig::default());
                assert_eq!(
                    for_route(&config, &(config).test_identity_for_kind(provider)),
                    expected,
                    "{provider:?} MINIMAX_API_KEY {key}"
                );
            }
        }
    }

    /// Ambient provider credentials are never sent to a custom host, so an
    /// exported `MINIMAX_API_KEY` proves nothing about what a gateway route
    /// bills. That route is Unknown, not metered-by-default.
    #[test]
    fn minimax_ambient_key_does_not_classify_a_custom_endpoint() {
        let _lock = crate::test_support::lock_test_env();
        let _env = minimax_env_guard();
        let _ambient = crate::test_support::EnvVarGuard::set("MINIMAX_API_KEY", "sk-payg-key");
        let config = config_with(
            ProviderKind::Minimax,
            ProviderConfig {
                base_url: Some("https://gateway.internal.example/v1".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert_eq!(
            for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Minimax)
            ),
            BillingPresentation::Unknown
        );
    }

    /// An operator pay mode we do not recognize is not a product claim.
    #[test]
    fn minimax_unrecognized_pay_mode_is_unclassified() {
        let _lock = crate::test_support::lock_test_env();
        let _env = minimax_env_guard();
        let config = config_with(
            ProviderKind::Minimax,
            ProviderConfig {
                mode: Some("enterprise-committed-spend".to_string()),
                api_key: Some("sk-cp-plan-key".to_string()),
                ..ProviderConfig::default()
            },
        );
        assert_eq!(
            for_route(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Minimax)
            ),
            BillingPresentation::Unknown
        );
    }

    /// MiniMax billing is credential-shaped, not endpoint-shaped: a dispatch
    /// receipt pointing at the shipped default URL still cannot invent a
    /// product.
    #[test]
    fn dispatched_minimax_default_endpoint_does_not_invent_a_product() {
        let _lock = crate::test_support::lock_test_env();
        let _env = minimax_env_guard();
        let config = config_with(ProviderKind::Minimax, ProviderConfig::default());
        assert_eq!(
            for_dispatched_route(
                &config,
                DispatchedRoute {
                    provider: ProviderKind::Minimax,
                    base_url: "https://api.minimax.io/v1",
                },
            ),
            BillingPresentation::Unknown
        );
    }

    #[test]
    fn same_provider_child_without_provenance_inherits_parent_billing() {
        assert_eq!(
            for_child_route(
                ProviderKind::Moonshot,
                BillingPresentation::Subscription("Kimi Code quota"),
                ProviderKind::Moonshot,
                None,
            ),
            BillingPresentation::Subscription("Kimi Code quota")
        );
        assert_eq!(
            for_child_route(
                ProviderKind::Minimax,
                BillingPresentation::Metered,
                ProviderKind::Minimax,
                None,
            ),
            BillingPresentation::Metered
        );
    }

    #[test]
    fn cross_provider_child_without_provenance_fails_closed_unknown() {
        // Moonshot and MiniMax both run metered AND subscription routes, so
        // identity alone must never guess either direction.
        for child in [ProviderKind::Moonshot, ProviderKind::Minimax] {
            assert_eq!(
                for_child_route(
                    ProviderKind::Deepseek,
                    BillingPresentation::Metered,
                    child,
                    None,
                ),
                BillingPresentation::Unknown,
                "{child:?} identity must not guess subscription or metered billing"
            );
        }
        // Local routes are the one identity-derived fact that stays truthful.
        for child in [
            ProviderKind::Ollama,
            ProviderKind::Sglang,
            ProviderKind::Vllm,
        ] {
            assert_eq!(
                for_child_route(
                    ProviderKind::Deepseek,
                    BillingPresentation::Metered,
                    child,
                    None,
                ),
                BillingPresentation::Local
            );
        }
    }

    #[test]
    fn child_provenance_wins_over_parent_route_and_provider_identity() {
        // Direct-platform Moonshot child under a Kimi Code membership
        // parent: the child's own metered truth must price the route.
        assert_eq!(
            for_child_route(
                ProviderKind::Moonshot,
                BillingPresentation::Subscription("Kimi Code quota"),
                ProviderKind::Moonshot,
                Some(BillingPresentation::Metered),
            ),
            BillingPresentation::Metered
        );
        // Membership Moonshot child under a metered parent: quota wins.
        assert_eq!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Moonshot,
                Some(BillingPresentation::Subscription("Kimi Code quota")),
            ),
            BillingPresentation::Subscription("Kimi Code quota")
        );
        // MiniMax Token Plan provenance never invents dollars; metered
        // provenance is allowed to accrue.
        assert!(
            !for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Minimax,
                Some(BillingPresentation::Subscription(
                    "MiniMax Token Plan quota"
                )),
            )
            .shows_money()
        );
        assert!(
            for_child_route(
                ProviderKind::Deepseek,
                BillingPresentation::Metered,
                ProviderKind::Minimax,
                Some(BillingPresentation::Metered),
            )
            .shows_money()
        );
    }

    #[test]
    fn child_billing_provenance_round_trips_through_serde() {
        for billing in [
            BillingPresentation::Metered,
            BillingPresentation::Subscription("ChatGPT plan allowance"),
            BillingPresentation::Subscription("Kimi Code quota"),
            BillingPresentation::Subscription("MiniMax Token Plan quota"),
            BillingPresentation::Local,
            BillingPresentation::Unknown,
        ] {
            let provenance = ChildBillingProvenance::from(billing);
            let json = serde_json::to_string(&provenance).expect("serialize provenance");
            let back: ChildBillingProvenance =
                serde_json::from_str(&json).expect("deserialize provenance");
            assert_eq!(back.as_billing_presentation(), billing);
        }
        // An unrecognized free-text label fails closed rather than
        // inventing a quota claim.
        assert_eq!(
            ChildBillingProvenance::Subscription {
                label: "free lunch".to_string(),
            }
            .as_billing_presentation(),
            BillingPresentation::Unknown
        );
    }

    /// Two named custom routes are the same `ProviderKind::Custom`. Identity,
    /// not the enum, decides whether a child may inherit the parent's product.
    #[test]
    fn custom_siblings_do_not_inherit_each_others_product() {
        let parent = ChildParentRoute {
            provider: ProviderKind::Custom,
            identity: "gateway-a",
            billing: BillingPresentation::Metered,
        };

        // Same vendor: inheritance is sound.
        assert_eq!(
            for_child_route_receipt(
                parent,
                ChildRouteClaim {
                    named: true,
                    provider: Some(ProviderKind::Custom),
                    identity: Some("gateway-a"),
                },
                None,
            ),
            BillingPresentation::Metered
        );

        // Sibling vendor on the same enum: must not borrow gateway-a's product.
        assert_eq!(
            for_child_route_receipt(
                parent,
                ChildRouteClaim {
                    named: true,
                    provider: Some(ProviderKind::Custom),
                    identity: Some("gateway-b"),
                },
                None,
            ),
            BillingPresentation::Unknown
        );
    }

    /// A child that names an unparseable provider named *some* route, just not
    /// one this build knows. That is never a licence to inherit.
    #[test]
    fn unparseable_child_provider_is_unknown_not_inherited() {
        let parent = ChildParentRoute {
            provider: ProviderKind::Anthropic,
            identity: "anthropic",
            billing: BillingPresentation::Subscription("Claude OAuth quota"),
        };
        assert_eq!(
            for_child_route_receipt(
                parent,
                ChildRouteClaim {
                    named: true,
                    provider: None,
                    identity: Some("some-future-vendor"),
                },
                None,
            ),
            BillingPresentation::Unknown
        );
        // But a child that claims nothing ran the parent's own client.
        assert_eq!(
            for_child_route_receipt(parent, ChildRouteClaim::default(), None),
            BillingPresentation::Subscription("Claude OAuth quota")
        );
    }

    /// The producer's metadata keys are exactly the ones the consumer reads.
    /// Pins the wire contract that previously had a reader and no producer.
    #[test]
    fn child_route_metadata_round_trips_through_the_consumer() {
        let metadata = child_route_metadata(
            ProviderKind::Ollama,
            "ollama",
            "http://localhost:11434/v1",
            RouteProduct::Unproven,
        );

        assert_eq!(metadata["child_provider"], "ollama");
        assert_eq!(metadata["child_provider_identity"], "ollama");
        let provenance: ChildBillingProvenance =
            serde_json::from_value(metadata["child_billing"].clone())
                .expect("child_billing must deserialize with the consumer's type");
        assert_eq!(
            provenance.as_billing_presentation(),
            BillingPresentation::Local
        );
    }

    /// A dispatched-route classification survives the child → parent mailbox
    /// boundary and still beats provider identity at the consumer.
    #[test]
    fn dispatched_receipt_survives_the_child_provenance_boundary() {
        let _lock = crate::test_support::lock_test_env();
        let _kimi = crate::test_support::EnvVarGuard::set(
            "KIMI_BASE_URL",
            "https://api.kimi.com/coding/v1",
        );
        let config = config_with(ProviderKind::Moonshot, ProviderConfig::default());
        let dispatched = for_dispatched_route(
            &config,
            DispatchedRoute {
                provider: ProviderKind::Moonshot,
                base_url: "https://api.kimi.com/coding/v1",
            },
        );
        let wire = serde_json::to_string(&ChildBillingProvenance::from(dispatched))
            .expect("serialize dispatch receipt");
        let back: ChildBillingProvenance =
            serde_json::from_str(&wire).expect("deserialize dispatch receipt");
        let billing = for_child_route(
            ProviderKind::Deepseek,
            BillingPresentation::Metered,
            ProviderKind::Moonshot,
            Some(back.as_billing_presentation()),
        );
        assert_eq!(
            billing,
            BillingPresentation::Subscription("Kimi Code quota")
        );
        assert!(!billing.shows_money());
    }

    /// Every provider env contract that can move a default route's endpoint.
    /// The audit below pins shipped defaults, so these must not leak in.
    const BASE_URL_ENV_VARS: &[&str] = &[
        "CODEWHALE_BASE_URL",
        "DEEPSEEK_BASE_URL",
        "NIM_BASE_URL",
        "NVIDIA_BASE_URL",
        "NVIDIA_NIM_BASE_URL",
        "OPENAI_BASE_URL",
        "ATLASCLOUD_BASE_URL",
        "OPENROUTER_BASE_URL",
        "ORCAROUTER_BASE_URL",
        "MIMO_BASE_URL",
        "XIAOMI_MIMO_BASE_URL",
        "WANJIE_ARK_BASE_URL",
        "WANJIE_BASE_URL",
        "WANJIE_MAAS_BASE_URL",
        "VOLCENGINE_BASE_URL",
        "VOLCENGINE_ARK_BASE_URL",
        "ARK_BASE_URL",
        "NOVITA_BASE_URL",
        "FIREWORKS_BASE_URL",
        "SILICONFLOW_BASE_URL",
        "ARCEE_BASE_URL",
        "MOONSHOT_BASE_URL",
        "KIMI_BASE_URL",
        "SGLANG_BASE_URL",
        "VLLM_BASE_URL",
        "OLLAMA_BASE_URL",
        "OLLAMA_CLOUD_BASE_URL",
        "HF_BASE_URL",
        "HUGGINGFACE_BASE_URL",
        "META_MODEL_API_BASE_URL",
        "MODEL_API_BASE_URL",
        "MISTRAL_BASE_URL",
        "XAI_BASE_URL",
        "GEMINI_BASE_URL",
        "GOOGLE_BASE_URL",
        "TELECOMJS_BASE_URL",
        "EDENAI_BASE_URL",
        "CONCENTRATE_BASE_URL",
        "MODELSTUDIO_TOKEN_PLAN_BASE_URL",
        "MODELSTUDIO_CODING_PLAN_BASE_URL",
        "OPENCODE_GO_BASE_URL",
        "OPENCODE_ZEN_BASE_URL",
    ];

    /// The shipped default-route billing decision for every runnable provider.
    /// Onboarding or re-defaulting a provider must update this table and the
    /// audit artifact (`docs/PROVIDERS.md` billing column) deliberately.
    const DEFAULT_ROUTE_BILLING_AUDIT: &[(ProviderKind, BillingPresentation)] = &[
        (ProviderKind::Deepseek, BillingPresentation::Metered),
        (
            ProviderKind::DeepseekAnthropic,
            BillingPresentation::Metered,
        ),
        (ProviderKind::NvidiaNim, BillingPresentation::Metered),
        (ProviderKind::Openai, BillingPresentation::Metered),
        (ProviderKind::Atlascloud, BillingPresentation::Metered),
        (ProviderKind::WanjieArk, BillingPresentation::Metered),
        (
            ProviderKind::Volcengine,
            BillingPresentation::Subscription("Volcengine Coding Plan"),
        ),
        (ProviderKind::Openrouter, BillingPresentation::Metered),
        (ProviderKind::Orcarouter, BillingPresentation::Metered),
        (
            ProviderKind::XiaomiMimo,
            BillingPresentation::Subscription("MiMo token plan"),
        ),
        (ProviderKind::Novita, BillingPresentation::Metered),
        (ProviderKind::Fireworks, BillingPresentation::Metered),
        (ProviderKind::Siliconflow, BillingPresentation::Metered),
        (ProviderKind::Arcee, BillingPresentation::Metered),
        (ProviderKind::SiliconflowCN, BillingPresentation::Metered),
        (ProviderKind::Moonshot, BillingPresentation::Metered),
        (ProviderKind::Sglang, BillingPresentation::Local),
        (ProviderKind::Vllm, BillingPresentation::Local),
        (ProviderKind::Ollama, BillingPresentation::Local),
        (ProviderKind::OllamaCloud, BillingPresentation::Unknown),
        (ProviderKind::Huggingface, BillingPresentation::Metered),
        (ProviderKind::Modelscope, BillingPresentation::Metered),
        (ProviderKind::Together, BillingPresentation::Metered),
        (ProviderKind::Qianfan, BillingPresentation::Metered),
        (ProviderKind::OpenaiCodex, BillingPresentation::Unknown),
        (ProviderKind::Anthropic, BillingPresentation::Metered),
        (ProviderKind::Openmodel, BillingPresentation::Metered),
        (
            ProviderKind::Zai,
            BillingPresentation::Subscription("Z.ai Coding Plan quota"),
        ),
        (ProviderKind::Stepfun, BillingPresentation::Metered),
        (ProviderKind::Minimax, BillingPresentation::Unknown),
        (ProviderKind::MinimaxAnthropic, BillingPresentation::Unknown),
        (ProviderKind::Deepinfra, BillingPresentation::Metered),
        (ProviderKind::Sakana, BillingPresentation::Metered),
        (ProviderKind::LongCat, BillingPresentation::Metered),
        (
            ProviderKind::OpencodeGo,
            BillingPresentation::Subscription("OpenCode Go quota"),
        ),
        (ProviderKind::OpencodeZen, BillingPresentation::Metered),
        (ProviderKind::Meta, BillingPresentation::Metered),
        (ProviderKind::Xai, BillingPresentation::Metered),
        (ProviderKind::Mistral, BillingPresentation::Metered),
        (ProviderKind::Telecomjs, BillingPresentation::Metered),
        (
            ProviderKind::ModelstudioTokenPlan,
            BillingPresentation::Subscription("Alibaba Token Plan"),
        ),
        (
            ProviderKind::ModelstudioTokenPlanAnthropic,
            BillingPresentation::Subscription("Alibaba Token Plan"),
        ),
        (
            ProviderKind::ModelstudioCodingPlan,
            BillingPresentation::Subscription("Alibaba Coding Plan"),
        ),
        (
            ProviderKind::ModelstudioCodingPlanAnthropic,
            BillingPresentation::Subscription("Alibaba Coding Plan"),
        ),
        // Retired identity: never selectable or runnable. A tombstone cannot
        // prove a billing product; retain its row so the audit stays exhaustive.
        (ProviderKind::Antigravity, BillingPresentation::Unknown),
        (ProviderKind::Google, BillingPresentation::Metered),
        (ProviderKind::Edenai, BillingPresentation::Metered),
        (ProviderKind::Zenmux, BillingPresentation::Metered),
        (
            ProviderKind::Csdn,
            BillingPresentation::Subscription("CSDN Coding Plan quota"),
        ),
        (ProviderKind::Concentrate, BillingPresentation::Metered),
        (ProviderKind::Codewhale, BillingPresentation::Metered),
        (ProviderKind::Custom, BillingPresentation::Unknown),
    ];

    /// Default-route billing is a deliberate, audited decision for every
    /// provider `ProviderKind::all()` exposes — 51 rows covering the primary
    /// route and every dialect/plan-variant alternate identity.
    #[test]
    fn default_route_billing_audit_covers_every_provider() {
        let _lock = crate::test_support::lock_test_env();
        let _env: Vec<_> = BASE_URL_ENV_VARS
            .iter()
            .copied()
            .map(crate::test_support::EnvVarGuard::remove)
            .collect();
        // Credential shape also steers MiniMax's default product; the audit
        // pins the no-credential answer.
        let _minimax = crate::test_support::EnvVarGuard::remove("MINIMAX_API_KEY");

        let audited: Vec<_> = DEFAULT_ROUTE_BILLING_AUDIT
            .iter()
            .map(|(provider, _)| provider)
            .collect();
        for (index, provider) in audited.iter().enumerate() {
            assert!(
                !audited[..index].contains(provider),
                "duplicate audit row for {provider:?}"
            );
        }
        for provider in ProviderKind::all() {
            assert!(
                audited.contains(&provider),
                "{provider:?} is missing from DEFAULT_ROUTE_BILLING_AUDIT"
            );
        }
        assert_eq!(
            DEFAULT_ROUTE_BILLING_AUDIT.len(),
            52,
            "the audit covers every provider identity, primary and alternate"
        );

        let mut config = Config::default();
        config
            .providers
            .get_or_insert_with(Default::default)
            .custom
            .insert(
                "custom".into(),
                crate::config::ProviderConfig {
                    kind: Some("openai-compatible".into()),
                    base_url: Some("http://localhost:1234/v1".into()),
                    model: Some("fixture-model".into()),
                    ..Default::default()
                },
            );
        for (provider, expected) in DEFAULT_ROUTE_BILLING_AUDIT {
            let mut scoped = config.clone();
            scoped.provider = Some(provider.as_str().into());
            let identity = scoped.active_provider_identity().unwrap();
            let actual = for_route(&scoped, &identity);
            assert_eq!(
                &actual, expected,
                "{provider:?} default route billing changed; update the audit deliberately"
            );
        }
    }
}
