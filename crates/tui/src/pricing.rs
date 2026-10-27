//! Cost estimation for API usage.
//!
//! Pricing is stored per million tokens. DeepSeek rows include their published
//! CNY rates; OpenRouter-curated rows are USD-only. Direct Xiaomi MiMo Token
//! Plan usage is credit/quota based and is intentionally left unknown until a
//! reliable balance endpoint exists.

use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Timelike, Utc, Weekday};
use codewhale_config::pricing::{
    Currency, LIVE_PRICING_MAX_AGE_SECS, LivePricingDefect, OfferingPricing, PricingProvenance,
    PricingQuoteError, TokenClass, TokenUsage,
};

use codewhale_config::ProviderKind;
#[cfg(test)]
use codewhale_config::descriptors::defaults::DEFAULT_STEPFUN_MODEL;
use codewhale_config::descriptors::defaults::{
    DEFAULT_STEPFUN_BASE_URL, DEFAULT_STEPFUN_PLAN_BASE_URL,
};
use codewhale_config::pricing::CostCurrency;
use codewhale_models::{Usage, has_date_snapshot_suffix};
use codewhale_runtime::config::{DEEPSEEK_ALIAS_RETIREMENT_UTC, canonical_model_id_for_provider};

/// Cost estimate in displayable currencies.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CostEstimate {
    pub usd: f64,
    pub cny: f64,
}

impl CostEstimate {
    pub fn usd_only(usd: f64) -> Self {
        Self { usd, cny: 0.0 }
    }

    pub fn is_positive(self) -> bool {
        self.is_finite_nonnegative() && (self.usd > 0.0 || self.cny > 0.0)
    }

    /// A cost is safe to persist/display only when both carried currencies are
    /// finite and nonnegative.
    #[must_use]
    pub fn is_finite_nonnegative(self) -> bool {
        self.usd.is_finite() && self.usd >= 0.0 && self.cny.is_finite() && self.cny >= 0.0
    }

    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            usd: if self.usd.is_finite() && self.usd >= 0.0 {
                self.usd
            } else {
                0.0
            },
            cny: if self.cny.is_finite() && self.cny >= 0.0 {
                self.cny
            } else {
                0.0
            },
        }
    }

    /// Add cost without ever producing NaN, infinity, or a negative total.
    /// Individual pricing rows are validated earlier; the saturation protects
    /// long-running accumulation from floating-point overflow.
    #[must_use]
    pub fn saturating_add(self, rhs: Self) -> Self {
        fn component(left: f64, right: f64) -> f64 {
            let sum = left + right;
            if sum.is_finite() { sum } else { f64::MAX }
        }
        let left = self.sanitized();
        let right = rhs.sanitized();
        Self {
            usd: component(left.usd, right.usd),
            cny: component(left.cny, right.cny),
        }
    }

    pub fn amount(self, currency: CostCurrency) -> f64 {
        match currency {
            CostCurrency::Usd => self.usd,
            CostCurrency::Cny => self.cny,
        }
    }
}

// === Provider Account Balance ===

/// Response from DeepSeek `GET /user/balance`. Other prepaid providers are
/// mapped onto [`BalanceInfo`] at the fetch seam.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct BalanceResponse {
    #[cfg_attr(not(test), expect(dead_code))]
    pub is_available: bool,
    pub balance_infos: Vec<BalanceInfo>,
}

/// Per-currency remaining-credit entry shown by `/balance` and the status chip.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct BalanceInfo {
    pub currency: String,
    #[serde(default)]
    pub total_balance: String,
    #[serde(default)]
    pub topped_up_balance: String,
    #[serde(default)]
    pub granted_balance: String,
}

impl BalanceInfo {
    /// Compact ledger chip, e.g. `$12.50` or `¥123.45`.
    #[must_use]
    pub fn chip_label(&self) -> Option<String> {
        let amount = self.total_balance.trim();
        if amount.is_empty() {
            return None;
        }
        Some(format_balance_amount(amount, &self.currency))
    }

    /// Full `/balance` report for one prepaid provider.
    #[must_use]
    pub fn report(&self, provider_name: &str) -> String {
        let amount = self
            .chip_label()
            .unwrap_or_else(|| self.total_balance.trim().to_string());
        let mut report = if amount.is_empty() {
            format!("{provider_name} account balance is unknown")
        } else {
            format!("{provider_name} account balance: {amount}")
        };
        let topped = self.topped_up_balance.trim();
        let granted = self.granted_balance.trim();
        if !topped.is_empty() || !granted.is_empty() {
            let mut parts = Vec::new();
            if !topped.is_empty() {
                parts.push(format!("topped up {topped}"));
            }
            if !granted.is_empty() {
                parts.push(format!("granted {granted}"));
            }
            report.push_str(&format!(" ({})", parts.join(", ")));
        }
        report
    }
}

fn format_balance_amount(amount: &str, currency: &str) -> String {
    match currency.trim().to_ascii_uppercase().as_str() {
        "CNY" | "RMB" | "¥" => format!("¥{amount}"),
        "USD" | "US$" | "$" => format!("${amount}"),
        "" => amount.to_string(),
        other => format!("{amount} {other}"),
    }
}

/// How a hand-sourced row bills cache-creation (cache-write) tokens.
///
/// The distinction matters because "no separate write rate published" and
/// "documented to cost the same as ordinary input" are different facts that used
/// to collapse onto the same `None`. Folding the unknown case into the input
/// rate invents a price; this enum keeps the invention impossible (#4318).
#[derive(Debug, Clone, Copy, PartialEq)]
enum CacheWritePolicy {
    /// The provider publishes a distinct cache-creation rate (per million).
    Rate(f64),
    /// Provider documentation states that cache creation carries **no separate
    /// charge** beyond the ordinary cache-miss input rate, so the miss rate is
    /// the published write rate rather than a substitute for a missing one.
    ///
    /// The `&'static str` is the documentation receipt this claim rests on, so
    /// the policy is auditable instead of asserted.
    DocumentedAsInputRate(&'static str),
    /// No published cache-write rate was found for this row. A turn that
    /// actually wrote to cache fails closed rather than being billed at a rate
    /// CodeWhale made up.
    Unpublished,
}

/// DeepSeek's context-caching docs: tokens that miss the cache are billed once
/// at the cache-miss rate and writing them into the cache costs nothing extra.
/// <https://api-docs.deepseek.com/guides/kv_cache>
const DEEPSEEK_CACHE_WRITE_IS_FREE: &str = "deepseek-kv-cache-no-write-charge";

impl CacheWritePolicy {
    /// The rate to bill cache-write tokens at, given the row's input rate.
    ///
    /// `None` means the row cannot price cache-write tokens at all.
    fn rate(self, input_cache_miss_per_million: f64) -> Option<f64> {
        match self {
            Self::Rate(rate) => Some(rate),
            Self::DocumentedAsInputRate(_) => Some(input_cache_miss_per_million),
            Self::Unpublished => None,
        }
    }
}

/// Per-million-token pricing for a model.
#[derive(Debug, Clone, Copy)]
struct CurrencyPricing {
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
    /// How cache-creation tokens are billed on this row.
    cache_write: CacheWritePolicy,
}

/// Per-million-token pricing for a model.
#[derive(Debug, Clone, Copy)]
struct ModelPricing {
    usd: CurrencyPricing,
    cny: Option<CurrencyPricing>,
}

pub(crate) const STEPFUN_PAYG_BILLING_SURFACE: &str = "stepfun-payg";
pub(crate) const STEPFUN_PLAN_BILLING_SURFACE: &str = "stepfun-plan";
const LEGACY_STEPFUN_PLAN_BASE_URL: &str = "https://api.stepfun.com/step_plan/v1";

/// Z.ai's dedicated Coding endpoint — the GLM Coding Plan subscription route.
pub(crate) const ZAI_CODING_PLAN_BILLING_SURFACE: &str = "zai-coding-plan";
/// Z.ai's ordinary public per-token API.
pub(crate) const ZAI_PAYG_BILLING_SURFACE: &str = "zai-payg";
/// Moonshot's Kimi Code subscription endpoint.
pub(crate) const MOONSHOT_KIMI_CODE_BILLING_SURFACE: &str = "moonshot-kimi-code";
/// Moonshot's ordinary public per-token API.
pub(crate) const MOONSHOT_PAYG_BILLING_SURFACE: &str = "moonshot-payg";
/// MiniMax's prepaid Token Plan endpoint.
pub(crate) const MINIMAX_TOKEN_PLAN_BILLING_SURFACE: &str = "minimax-token-plan";
/// MiniMax's ordinary public per-token API.
pub(crate) const MINIMAX_PAYG_BILLING_SURFACE: &str = "minimax-payg";
/// Xiaomi MiMo's prepaid token-plan endpoint.
pub(crate) const XIAOMI_TOKEN_PLAN_BILLING_SURFACE: &str = "xiaomi-mimo-token-plan";
/// Xiaomi MiMo's ordinary public per-token API.
pub(crate) const XIAOMI_PAYG_BILLING_SURFACE: &str = "xiaomi-mimo-payg";
/// An OAuth/subscription-brokered endpoint (Codex, Claude OAuth, Grok OAuth,
/// OpenCode Go). Never per-token metered from CodeWhale's side.
pub(crate) const OAUTH_SUBSCRIPTION_BILLING_SURFACE: &str = "oauth-subscription";
/// A loopback / self-hosted endpoint with no provider bill at all.
pub(crate) const LOCAL_BILLING_SURFACE: &str = "local-no-bill";
/// A provider's own first-party public per-token API, on its documented host.
pub(crate) const FIRST_PARTY_PAYG_BILLING_SURFACE: &str = "first-party-payg";
/// An aggregator/reseller endpoint: metered, but priced by the aggregator's own
/// catalog rather than by the upstream model owner's published rates.
pub(crate) const AGGREGATOR_BILLING_SURFACE: &str = "aggregator-payg";
pub(crate) const MODELSTUDIO_TOKEN_PLAN_BILLING_SURFACE: &str = "modelstudio-token-plan";
pub(crate) const MODELSTUDIO_CODING_PLAN_BILLING_SURFACE: &str = "modelstudio-coding-plan";
pub(crate) const VOLCENGINE_CODING_PLAN_BILLING_SURFACE: &str = "volcengine-coding-plan";
/// CSDN 星图's Coding Plan subscription product (the `glm_for_coding` route).
pub(crate) const CSDN_CODING_PLAN_BILLING_SURFACE: &str = "csdn-coding-plan";
/// CSDN 星图's ordinary metered marketplace access on the same endpoint.
pub(crate) const CSDN_PAYG_BILLING_SURFACE: &str = "csdn-payg";
/// A reachable endpoint CodeWhale could not match to any known billing surface.
/// Distinct from "not classified yet": this is a positive statement that the
/// surface is unknown, and it fails closed everywhere it is consumed.
pub(crate) const UNCLASSIFIED_BILLING_SURFACE: &str = "unclassified";

/// How a classified billing surface meters money.
///
/// This is the fact every cost surface actually needs: whether a dollar figure
/// is even the right unit for the route. `Unknown` is a real answer and is
/// treated as *possibly* metered — it is counted as missing spend rather than
/// excused as a subscription (#4318).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointMetering {
    /// Per-token money, priced against published rates.
    Money,
    /// An exactly-identified subscription or prepaid-quota endpoint. Money is
    /// the wrong unit here, so these turns are excluded from money coverage.
    ExactSubscription,
    /// Local/self-hosted: there is no provider bill.
    LocalNoBill,
    /// Could not be established. Fails closed as possibly-money.
    Unknown,
}

/// Classify a billing-surface id into its metering shape.
///
/// Unrecognized ids — including ones written by a newer build — resolve to
/// [`EndpointMetering::Unknown`] rather than being guessed into a bucket.
#[must_use]
pub fn endpoint_metering_for_billing_surface(billing_surface: Option<&str>) -> EndpointMetering {
    let Some(surface) = billing_surface.map(str::trim).filter(|s| !s.is_empty()) else {
        return EndpointMetering::Unknown;
    };
    // Exact, case-insensitive matches only. A prefix/substring rule here would
    // let an unrecognized future surface impersonate a known one.
    for (known, metering) in [
        (STEPFUN_PAYG_BILLING_SURFACE, EndpointMetering::Money),
        (ZAI_PAYG_BILLING_SURFACE, EndpointMetering::Money),
        (MOONSHOT_PAYG_BILLING_SURFACE, EndpointMetering::Money),
        (MINIMAX_PAYG_BILLING_SURFACE, EndpointMetering::Money),
        (XIAOMI_PAYG_BILLING_SURFACE, EndpointMetering::Money),
        (CSDN_PAYG_BILLING_SURFACE, EndpointMetering::Money),
        (FIRST_PARTY_PAYG_BILLING_SURFACE, EndpointMetering::Money),
        (AGGREGATOR_BILLING_SURFACE, EndpointMetering::Money),
        (
            MODELSTUDIO_TOKEN_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            MODELSTUDIO_CODING_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            VOLCENGINE_CODING_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            STEPFUN_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            ZAI_CODING_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            MOONSHOT_KIMI_CODE_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            MINIMAX_TOKEN_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            XIAOMI_TOKEN_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            CSDN_CODING_PLAN_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (
            OAUTH_SUBSCRIPTION_BILLING_SURFACE,
            EndpointMetering::ExactSubscription,
        ),
        (LOCAL_BILLING_SURFACE, EndpointMetering::LocalNoBill),
        (UNCLASSIFIED_BILLING_SURFACE, EndpointMetering::Unknown),
    ] {
        if surface.eq_ignore_ascii_case(known) {
            return metering;
        }
    }
    EndpointMetering::Unknown
}

/// A base URL reduced to the non-secret parts a billing classification may
/// depend on: scheme, host, normalized path. `None` when the URL carries
/// embedded credentials, a query, a fragment, a non-default port, or is not
/// HTTPS — any of which means CodeWhale cannot vouch for which surface it is.
struct EndpointShape {
    host: String,
    path: String,
}

fn endpoint_shape(base_url: &str) -> Option<EndpointShape> {
    let parsed = reqwest::Url::parse(base_url.trim()).ok()?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.port_or_known_default() != Some(443)
    {
        return None;
    }
    Some(EndpointShape {
        host: parsed.host_str()?.to_ascii_lowercase(),
        path: parsed.path().trim_end_matches('/').to_string(),
    })
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// Historical Codex backend receipts retain their original quota basis.
/// New Sign in with ChatGPT requests use the public API and additionally need
/// captured grant provenance; this endpoint check never proves that grant.
pub(crate) fn is_chatgpt_codex_backend(base_url: &str) -> bool {
    let Some(shape) = endpoint_shape(base_url) else {
        return false;
    };
    shape.host == "chatgpt.com"
        && (shape.path == "/backend-api" || shape.path.starts_with("/backend-api/"))
}

/// The public API base documented for official ChatGPT plan inference.
/// The same endpoint accepts metered API keys: this shape alone is not a
/// subscription claim. [`crate::route_billing`] also requires a verified grant.
pub(crate) fn is_official_chatgpt_api(base_url: &str) -> bool {
    endpoint_shape(base_url)
        .is_some_and(|shape| shape.host == "api.openai.com" && shape.path == "/v1")
}

/// Reduce a concrete request endpoint to non-secret billing provenance.
///
/// Every reachable endpoint now gets a positive classification, including
/// [`UNCLASSIFIED_BILLING_SURFACE`] for one CodeWhale cannot place. `None` is
/// reserved for "no endpoint was supplied", which is a different failure and is
/// also treated as unknown downstream. Nothing here consults credentials or
/// echoes a URL, so the result is safe to persist and log.
pub(crate) fn billing_surface_for_route(
    provider: ProviderKind,
    base_url: Option<&str>,
) -> Option<&'static str> {
    // Routes whose billing shape is a property of the provider itself, not of
    // the endpoint spelling.
    match provider {
        ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm => {
            return Some(LOCAL_BILLING_SURFACE);
        }
        // Ollama Cloud publishes plan/account terms, not a Codewhale-owned
        // per-token rate. Hosted is not local/free, but it is also not proof
        // of PAYG dollars: keep it in money coverage as unclassified until an
        // authoritative billing surface is available.
        ProviderKind::OllamaCloud => return Some(UNCLASSIFIED_BILLING_SURFACE),
        ProviderKind::OpencodeGo => return Some(OAUTH_SUBSCRIPTION_BILLING_SURFACE),
        // Preserve historical backend provenance, but public API plan access
        // is credential-shaped. Without the captured official grant it stays
        // unknown, including when no endpoint was supplied.
        ProviderKind::OpenaiCodex => {
            return Some(if base_url.is_some_and(is_chatgpt_codex_backend) {
                OAUTH_SUBSCRIPTION_BILLING_SURFACE
            } else {
                UNCLASSIFIED_BILLING_SURFACE
            });
        }
        // A named custom endpoint is never assumed to be metered; the billing
        // presentation layer decides that from explicit config.
        ProviderKind::Custom => return Some(UNCLASSIFIED_BILLING_SURFACE),
        _ => {}
    }

    let base_url = base_url.map(str::trim).filter(|url| !url.is_empty())?;
    let Some(shape) = endpoint_shape(base_url) else {
        return Some(UNCLASSIFIED_BILLING_SURFACE);
    };

    let surface = match provider {
        ProviderKind::Stepfun => stepfun_surface(&shape),
        ProviderKind::Zai => zai_surface(&shape),
        ProviderKind::Moonshot => moonshot_surface(&shape),
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => minimax_surface(&shape),
        ProviderKind::Csdn => csdn_surface(&shape),
        ProviderKind::XiaomiMimo => xiaomi_surface(&shape),
        ProviderKind::ModelstudioTokenPlan
        | ProviderKind::ModelstudioTokenPlanAnthropic
        | ProviderKind::ModelstudioCodingPlan
        | ProviderKind::ModelstudioCodingPlanAnthropic => modelstudio_surface(&shape),
        ProviderKind::Volcengine => volcengine_surface(&shape),
        ProviderKind::Openrouter
        | ProviderKind::NvidiaNim
        | ProviderKind::OpencodeZen
        | ProviderKind::Orcarouter => {
            is_official_default_endpoint(provider, &shape).then_some(AGGREGATOR_BILLING_SURFACE)
        }
        _ => is_official_default_endpoint(provider, &shape)
            .then_some(FIRST_PARTY_PAYG_BILLING_SURFACE),
    };
    Some(surface.unwrap_or(UNCLASSIFIED_BILLING_SURFACE))
}

// Token Plan and Coding Plan keys/endpoints are isolated from PAYG.
// https://www.alibabacloud.com/help/en/model-studio/token-plan-quick-start
// https://www.alibabacloud.com/help/en/model-studio/coding-plan-faq
fn modelstudio_surface(shape: &EndpointShape) -> Option<&'static str> {
    match (shape.host.as_str(), shape.path.as_str()) {
        (
            "token-plan.ap-southeast-1.maas.aliyuncs.com",
            "/compatible-mode/v1" | "/apps/anthropic" | "/apps/anthropic/v1",
        ) => Some(MODELSTUDIO_TOKEN_PLAN_BILLING_SURFACE),
        (
            "coding-intl.dashscope.aliyuncs.com" | "coding.dashscope.aliyuncs.com",
            "/v1" | "/apps/anthropic" | "/apps/anthropic/v1",
        ) => Some(MODELSTUDIO_CODING_PLAN_BILLING_SURFACE),
        ("dashscope-intl.aliyuncs.com" | "dashscope.aliyuncs.com", "/compatible-mode/v1") => {
            Some(FIRST_PARTY_PAYG_BILLING_SURFACE)
        }
        _ => None,
    }
}

// The Coding Plan gateway consumes plan quota; /api/v3 is billed separately.
// https://www.volcengine.com/docs/82379/1925114
fn volcengine_surface(shape: &EndpointShape) -> Option<&'static str> {
    match (shape.host.as_str(), shape.path.as_str()) {
        ("ark.cn-beijing.volces.com", "/api/coding" | "/api/coding/v3") => {
            Some(VOLCENGINE_CODING_PLAN_BILLING_SURFACE)
        }
        ("ark.cn-beijing.volces.com", "/api/v3") => Some(FIRST_PARTY_PAYG_BILLING_SURFACE),
        _ => None,
    }
}

fn stepfun_surface(shape: &EndpointShape) -> Option<&'static str> {
    if host_of(DEFAULT_STEPFUN_BASE_URL).is_some_and(|official| shape.host == official)
        && matches!(shape.path.as_str(), "" | "/v1")
    {
        return Some(STEPFUN_PAYG_BILLING_SURFACE);
    }
    let plan_host = [DEFAULT_STEPFUN_PLAN_BASE_URL, LEGACY_STEPFUN_PLAN_BASE_URL]
        .iter()
        .filter_map(|url| host_of(url))
        .any(|plan| plan == shape.host);
    if plan_host && matches!(shape.path.as_str(), "/step_plan" | "/step_plan/v1") {
        return Some(STEPFUN_PLAN_BILLING_SURFACE);
    }
    None
}

fn zai_surface(shape: &EndpointShape) -> Option<&'static str> {
    // The Coding Plan contract is the exact shipped Z.ai endpoint. Do not let
    // arbitrary future `/api/coding/*` paths, or the separate BigModel host,
    // inherit a subscription classification.
    if shape.host == "api.z.ai" && shape.path == "/api/coding/paas/v4" {
        Some(ZAI_CODING_PLAN_BILLING_SURFACE)
    } else if matches!(shape.host.as_str(), "api.z.ai" | "open.bigmodel.cn")
        && matches!(
            shape.path.as_str(),
            "/api/paas/v4" | "/api/anthropic" | "/v1" | ""
        )
    {
        Some(ZAI_PAYG_BILLING_SURFACE)
    } else {
        None
    }
}

fn moonshot_surface(shape: &EndpointShape) -> Option<&'static str> {
    // Kimi Code is a distinct membership product on api.kimi.com.  Accept the
    // exact shipped endpoint as well as its slash-normalized parent; do not
    // infer a plan from a model id or from an arbitrary host carrying a
    // `/coding` path.
    if shape.host == "api.kimi.com" && matches!(shape.path.as_str(), "/coding" | "/coding/v1") {
        Some(MOONSHOT_KIMI_CODE_BILLING_SURFACE)
    } else if matches!(shape.host.as_str(), "api.moonshot.ai" | "api.moonshot.cn")
        && matches!(shape.path.as_str(), "" | "/v1" | "/anthropic")
    {
        Some(MOONSHOT_PAYG_BILLING_SURFACE)
    } else {
        None
    }
}

fn minimax_surface(shape: &EndpointShape) -> Option<&'static str> {
    // MiniMax API keys and subscription-plan keys use the same normal
    // endpoints. The URL therefore proves neither PAYG nor plan billing; only
    // an explicit saved mode may produce a concrete MiniMax surface.
    let _is_supported_endpoint = matches!(
        shape.host.as_str(),
        "api.minimax.io" | "api.minimaxi.com" | "api.minimax.chat"
    ) && matches!(shape.path.as_str(), "" | "/v1" | "/anthropic");
    None
}

fn csdn_surface(shape: &EndpointShape) -> Option<&'static str> {
    // Coding Plan keys and general marketplace keys share the one
    // ai.csdn.net/api/model/v1 endpoint, so the URL proves neither product;
    // only the captured credential product can produce a concrete surface.
    let _is_supported_endpoint = shape.host == "ai.csdn.net"
        && matches!(shape.path.as_str(), "/api/model" | "/api/model/v1");
    None
}

fn xiaomi_surface(shape: &EndpointShape) -> Option<&'static str> {
    if matches!(
        shape.host.as_str(),
        "token-plan-cn.xiaomimimo.com"
            | "token-plan-sgp.xiaomimimo.com"
            | "token-plan-ams.xiaomimimo.com"
    ) && shape.path == "/v1"
    {
        return Some(XIAOMI_TOKEN_PLAN_BILLING_SURFACE);
    }
    if shape.host == "api.xiaomimimo.com" && shape.path == "/v1" {
        return Some(XIAOMI_PAYG_BILLING_SURFACE);
    }
    None
}

/// Exact default endpoint match for built-in providers whose billing surface
/// has no provider-specific split above.
///
/// A provider enum is not proof that a configured URL is that provider's own
/// billing surface.  This allowlist keeps `https://proxy.example/v1` from
/// inheriting OpenAI/Anthropic/DeepSeek/OpenRouter prices merely because the
/// selected protocol/provider name is familiar.
fn is_official_default_endpoint(provider: ProviderKind, shape: &EndpointShape) -> bool {
    let Some(default) =
        endpoint_shape(codewhale_config::descriptors::compatibility_for_kind(provider).base_url)
    else {
        return false;
    };
    if shape.host != default.host {
        return false;
    }
    if shape.path == default.path {
        return true;
    }
    match provider {
        ProviderKind::Deepseek => {
            matches!(shape.path.as_str(), "" | "/v1" | "/beta")
        }
        ProviderKind::DeepseekAnthropic => shape.path == "/anthropic",
        ProviderKind::Openai => matches!(shape.path.as_str(), "" | "/v1"),
        ProviderKind::Anthropic => matches!(shape.path.as_str(), "" | "/v1"),
        _ => false,
    }
}

// Official PAYG rates; Step Plan consumes subscription quota instead.
// https://platform.stepfun.ai/docs/en/guides/pricing/details (2026-09-19).
fn stepfun_payg_pricing(model: &str) -> Option<ModelPricing> {
    match model.trim().to_ascii_lowercase().as_str() {
        "step-5-preview" => Some(usd_pricing(
            0.05,
            1.00,
            2.70,
            CacheWritePolicy::DocumentedAsInputRate(
                "https://platform.stepfun.ai/docs/en/guides/pricing/details",
            ),
        )),
        "step-3.7-flash" => Some(usd_only_pricing(0.04, 0.20, 1.15)),
        "step-3.5-flash" | "step-3.5-flash-2603" => Some(usd_only_pricing(0.02, 0.10, 0.30)),
        _ => None,
    }
}

fn pricing_for_billing_surface(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
) -> Option<ModelPricing> {
    if provider == ProviderKind::Stepfun
        && billing_surface
            .is_some_and(|surface| surface.eq_ignore_ascii_case(STEPFUN_PAYG_BILLING_SURFACE))
    {
        stepfun_payg_pricing(model)
    } else {
        None
    }
}

fn route_requires_billing_surface(provider: ProviderKind, model: &str) -> bool {
    provider == ProviderKind::Stepfun || stepfun_payg_pricing(model).is_some()
}

/// Look up pricing for a model name.
fn pricing_for_model(model: &str) -> Option<ModelPricing> {
    pricing_for_model_at(model, Utc::now())
}

/// Return whether a model has a row in the pricing table.
#[must_use]
pub fn has_pricing_for_model(model: &str) -> bool {
    pricing_for_model(model).is_some()
}

/// Return whether the selected provider route exposes authoritative dollar
/// pricing for this model without endpoint provenance. ChatGPT/Codex OAuth is
/// subscription/account scoped, while StepFun needs PAYG-vs-Plan provenance.
#[must_use]
pub fn has_pricing_for_provider(provider: ProviderKind, model: &str) -> bool {
    calculate_turn_cost_estimate_for_provider(provider, model, &Usage::default()).is_some()
}

/// Return whether a provider/model route has authoritative pricing for an
/// already-classified billing surface.
#[cfg(test)]
#[must_use]
pub(crate) fn has_pricing_for_billing_surface(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
) -> bool {
    pricing_for_billing_surface(provider, model, billing_surface).is_some()
}

fn pricing_for_model_at(model: &str, now: DateTime<Utc>) -> Option<ModelPricing> {
    let lower = model.to_lowercase();
    if lower.starts_with("deepseek-ai/") {
        // NVIDIA NIM-hosted DeepSeek uses NVIDIA's catalog/account terms, not
        // DeepSeek Platform pricing. Avoid showing misleading DeepSeek costs.
        return None;
    }
    if lower == "claude-sonnet-5" {
        // Resolved ahead of the catalog through the recorded-time helper so
        // the first-party Anthropic override path (`hand_priced_audit`)
        // and this metadata lookup stay one contract (see
        // `claude_sonnet_5_pricing`).
        return Some(claude_sonnet_5_pricing(now));
    }
    if let Some(pricing) = known_pricing_for_model(&lower) {
        return Some(pricing);
    }
    // A new or expiring model ID does not inherit a neighboring model's
    // rates. Keep this metadata lookup as exact as the billing route owner.
    deepseek_time_aware_pricing(&lower, now)
}

fn deepseek_time_aware_pricing(model_lower: &str, now: DateTime<Utc>) -> Option<ModelPricing> {
    match model_lower {
        "deepseek-v4-pro" => Some(deepseek_v4_pro_pricing(now)),
        "deepseek-v4-flash" | "deepseek-flash" => Some(deepseek_v4_flash_pricing(now)),
        "deepseek-v4-flash-vision-exp" => {
            (now >= deepseek_flash_effective_from()).then(|| deepseek_flash_pricing(now))
        }
        _ => None,
    }
}

fn known_pricing_for_model(model_lower: &str) -> Option<ModelPricing> {
    let reviewed = codewhale_config::catalog::reviewed::bundled_reviewed();
    if let Some(policy) = reviewed.reference_price_policies.get(model_lower) {
        return match policy.as_str() {
            "grok" => grok_tiered_pricing(model_lower, false),
            "minimax_m3" => Some(minimax_m3_standard_pricing(false)),
            _ => None,
        };
    }
    reviewed.reference_prices.get(model_lower).map(|row| {
        usd_pricing(
            row.cache_read,
            row.input,
            row.output,
            row.cache_write
                .map_or(CacheWritePolicy::Unpublished, CacheWritePolicy::Rate),
        )
    })
}

/// A USD row whose provider publishes input/cache-read/output rates but **no**
/// cache-creation rate. Cache-write tokens on such a row are unpriced, not free
/// and not silently charged at the input rate (#4318).
fn usd_only_pricing(
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
) -> ModelPricing {
    usd_pricing(
        input_cache_hit_per_million,
        input_cache_miss_per_million,
        output_per_million,
        CacheWritePolicy::Unpublished,
    )
}

fn usd_pricing_with_write(
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
    cache_write_per_million: f64,
) -> ModelPricing {
    usd_pricing(
        input_cache_hit_per_million,
        input_cache_miss_per_million,
        output_per_million,
        CacheWritePolicy::Rate(cache_write_per_million),
    )
}

fn usd_pricing(
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
    cache_write: CacheWritePolicy,
) -> ModelPricing {
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million,
            input_cache_miss_per_million,
            output_per_million,
            cache_write,
        },
        cny: None,
    }
}

const MINIMAX_M3_LONG_CONTEXT_THRESHOLD: u32 = 512_000;
const GROK_4_6_LONG_CONTEXT_THRESHOLD: u32 = 200_000;
const OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD: u32 = 272_000;

/// OpenAI applies a higher price to the full request once these models exceed
/// 272K input tokens. Until the pricing layer can represent request-wide tiers,
/// refuse to report the lower static catalog price (#4317).
/// <https://developers.openai.com/api/docs/models/gpt-5.4>
/// <https://developers.openai.com/api/docs/models/gpt-5.5>
/// <https://developers.openai.com/api/docs/models/gpt-5.6-sol>
fn direct_openai_long_context_tier_is_unpriced(
    provider: ProviderKind,
    model: &str,
    input_tokens: u32,
) -> bool {
    let model_lower = model.trim().to_ascii_lowercase();
    let affected_model = matches!(
        model_lower.as_str(),
        "gpt-5.4"
            | "gpt-5.4-pro"
            | "gpt-5.5"
            | "gpt-5.6"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "gpt-6-sol"
            | "gpt-6.1-sol"
            | "gpt-6-luna"
    ) || has_date_snapshot_suffix(&model_lower, "gpt-5.4-")
        || has_date_snapshot_suffix(&model_lower, "gpt-5.4-pro-")
        || has_date_snapshot_suffix(&model_lower, "gpt-5.5-");
    provider == ProviderKind::Openai
        && input_tokens > OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD
        && affected_model
}

fn minimax_m3_standard_pricing(long_context: bool) -> ModelPricing {
    if long_context {
        usd_only_pricing(0.12, 0.60, 2.40)
    } else {
        usd_only_pricing(0.06, 0.30, 1.20)
    }
}

fn is_minimax_m3(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "minimax-m3" | "minimax/minimax-m3"
    )
}

/// xAI Grok standard-tier rates (cache-read, input, output per 1M) and the
/// doubled tier once a prompt reaches 200K tokens. Verified 2026-08-17 against
/// the model pages, whose embedded price tables carry both the standard and
/// `LongContext` columns at exactly 2x:
/// - <https://docs.x.ai/docs/models/grok-4.7>: 0.50 / 2.00 / 6.00 (verified
///   2026-09-23: $4.00 / $1.00 / $12.00 at or above 200K)
/// - <https://docs.x.ai/docs/models/grok-4.6>: 0.50 / 2.00 / 6.00
/// - <https://docs.x.ai/docs/models/grok-4.5>: 0.30 / 2.00 / 6.00
/// - <https://docs.x.ai/docs/models/grok-4.3>: 0.20 / 1.25 / 2.50
fn grok_tiered_pricing(model_lower: &str, long_context: bool) -> Option<ModelPricing> {
    let (cache_read, input, output) = match model_lower {
        "grok-4.7" | "grok-4.6" => (0.50, 2.00, 6.00),
        "grok-4.5" => (0.30, 2.00, 6.00),
        "grok-4.3" => (0.20, 1.25, 2.50),
        _ => return None,
    };
    let multiplier = if long_context { 2.0 } else { 1.0 };
    Some(usd_only_pricing(
        cache_read * multiplier,
        input * multiplier,
        output * multiplier,
    ))
}

fn is_grok_tiered(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "grok-4.7" | "grok-4.6" | "grok-4.5" | "grok-4.3"
    )
}

#[cfg(test)]
fn pricing_for_model_and_usage(model: &str, usage: &Usage) -> Option<ModelPricing> {
    if is_minimax_m3(model) {
        return Some(minimax_m3_standard_pricing(
            usage.input_tokens > MINIMAX_M3_LONG_CONTEXT_THRESHOLD,
        ));
    }
    if is_grok_tiered(model) {
        return grok_tiered_pricing(
            &model.trim().to_ascii_lowercase(),
            usage.input_tokens >= GROK_4_6_LONG_CONTEXT_THRESHOLD,
        );
    }
    pricing_for_model(model)
}

/// Claude Sonnet 5 pricing (<https://platform.claude.com/docs/en/about-claude/pricing>,
/// re-verified 2026-08-17): 2.00 / 10.00 (cache-read 0.20, 5m cache-write
/// 2.50) is now the standard price. Anthropic's pricing page states the
/// previously scheduled increase to 3.00 / 15.00 on 2026-09-01 "will not
/// occur" (release notes, 2026-08-10), so the former time-windowed flip is
/// gone; the recorded-time signature is kept so callers that price turns at
/// their recorded time (scorecard, usage aggregation) keep one contract for
/// every first-party time-aware row.
fn claude_sonnet_5_pricing(_now: DateTime<Utc>) -> ModelPricing {
    usd_pricing_with_write(0.20, 2.00, 10.00, 2.50)
}

/// DeepSeek publishes only cache-hit and cache-miss input rates *because* its
/// context cache charges nothing extra to write: a token that misses the cache
/// is billed once at the miss rate and is cached as a side effect. That makes
/// the miss rate the documented write rate, not a stand-in for a missing one.
///
/// Peak/off-peak tiers (verified against
/// <https://api-docs.deepseek.com/quick_start/pricing> on 2026-08-17):
/// off-peak rates are half the peak rates, and peak hours are 01:00–04:00
/// and 06:00–10:00 UTC (half-open). From 00:00 Beijing time on 2026-08-23 the
/// whole of Saturday and Sunday bills off-peak, peak hours included. Each turn
/// resolves its tier from its own recorded time, mirroring
/// `claude_sonnet_5_pricing`'s time-aware precedent.
fn deepseek_peak_hour(hour_utc: u32) -> bool {
    (1..4).contains(&hour_utc) || (6..10).contains(&hour_utc)
}

/// Beijing time, the zone DeepSeek states its weekend rule in. China has run a
/// fixed UTC+08:00 with no daylight saving since 1991, so a fixed offset is
/// exact here and needs no tzdata on the host.
fn deepseek_billing_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("+08:00 is a valid UTC offset")
}

/// The instant the weekend-wide off-peak rule takes effect: 00:00 Beijing time
/// on Sunday 2026-08-23, which is 2026-08-22T16:00Z.
fn deepseek_weekend_off_peak_from() -> DateTime<Utc> {
    deepseek_billing_offset()
        .with_ymd_and_hms(2026, 8, 23, 0, 0, 0)
        .single()
        .expect("2026-08-23 00:00 exists in a fixed offset")
        .with_timezone(&Utc)
}

/// Whether `now` falls on a Beijing-time Saturday or Sunday with the weekend-wide
/// off-peak rule already in force.
///
/// The weekend is bounded in Beijing time, so it runs 16:00Z Friday to 16:00Z
/// Sunday; `now.weekday()` taken in UTC covers a different 48 hours. Both
/// spellings agree on today's tiers, because the peak windows sit entirely
/// outside the 16 hours they disagree over. This one keeps agreeing if the
/// windows move.
fn deepseek_weekend_off_peak(now: DateTime<Utc>) -> bool {
    now >= deepseek_weekend_off_peak_from()
        && matches!(
            now.with_timezone(&deepseek_billing_offset()).weekday(),
            Weekday::Sat | Weekday::Sun
        )
}

/// Whether a turn recorded at `now` is billed at DeepSeek's peak tier.
fn deepseek_is_peak(now: DateTime<Utc>) -> bool {
    !deepseek_weekend_off_peak(now) && deepseek_peak_hour(now.hour())
}

/// The clock-dependent tier a DeepSeek route bills at right now: `Some(true)`
/// at peak, `Some(false)` off-peak, `None` for a model whose rates do not move
/// with the clock. The model set is exactly the time-aware arm of
/// [`pricing_for_model_at`], so the chip that shows this and the receipt that
/// prices the turn can never disagree about which routes are tiered.
#[must_use]
pub(crate) fn deepseek_time_tier(model: &str, now: DateTime<Utc>) -> Option<bool> {
    let lower = model.trim().to_ascii_lowercase();
    deepseek_time_aware_pricing(&lower, now).map(|_| deepseek_is_peak(now))
}

fn deepseek_v4_pro_pricing(now: DateTime<Utc>) -> ModelPricing {
    // September 11 vendor reversal: Pro remains available at its own rates.
    let peak = deepseek_is_peak(now);
    let (hit, miss, out) = if peak {
        (0.044, 1.32, 3.96)
    } else {
        (0.022, 0.66, 1.98)
    };
    let (cny_hit, cny_miss, cny_out) = if peak {
        (0.30, 9.0, 27.0)
    } else {
        (0.15, 4.5, 13.5)
    };
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million: hit,
            input_cache_miss_per_million: miss,
            output_per_million: out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        },
        cny: Some(CurrencyPricing {
            input_cache_hit_per_million: cny_hit,
            input_cache_miss_per_million: cny_miss,
            output_per_million: cny_out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        }),
    }
}

/// DeepSeek V4.1 Flash, shipped as the unversioned id `deepseek-flash`.
///
/// Rates and effective time are the vendor's own 2026-09-10 notice, not a
/// relay: cache hit $0.003, cache miss $0.15, output $0.60 per 1M off-peak,
/// doubling at peak, effective 04:00 UTC on 2026-09-10.
/// V4 Pro remains available at its own rates after the September 11 reversal.
///
/// CNY rates are the vendor's own Chinese-language notice: cache hit 0.02 元,
/// cache miss 1 元, output 4 元 off-peak, doubling at peak. Taken from the
/// published table rather than converted from USD — a converted rate would be
/// a receipt the vendor never issued.
///
/// That notice states the peak windows in Beijing time (Mon-Fri 09:00-12:00 and
/// 14:00-18:00), which is UTC+8 and therefore exactly the 01:00-04:00 and
/// 06:00-10:00 UTC the English notice gives. Both agree, so `deepseek_is_peak`
/// needs no change.
fn deepseek_flash_pricing(now: DateTime<Utc>) -> ModelPricing {
    let peak = deepseek_is_peak(now);
    let (hit, miss, out) = if peak {
        (0.006, 0.30, 1.20)
    } else {
        (0.003, 0.15, 0.60)
    };
    let (cny_hit, cny_miss, cny_out) = if peak {
        (0.04, 2.0, 8.0)
    } else {
        (0.02, 1.0, 4.0)
    };
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million: hit,
            input_cache_miss_per_million: miss,
            output_per_million: out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        },
        cny: Some(CurrencyPricing {
            input_cache_hit_per_million: cny_hit,
            input_cache_miss_per_million: cny_miss,
            output_per_million: cny_out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        }),
    }
}

fn deepseek_flash_effective_from() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 10, 4, 0, 0)
        .single()
        .expect("2026-09-10T04:00Z is a valid UTC instant")
}

fn deepseek_v4_flash_pricing(now: DateTime<Utc>) -> ModelPricing {
    if now >= deepseek_flash_effective_from() {
        return deepseek_flash_pricing(now);
    }
    let peak = deepseek_is_peak(now);
    let (hit, miss, out) = if peak {
        (0.014, 0.44, 1.32)
    } else {
        (0.007, 0.22, 0.66)
    };
    let (cny_hit, cny_miss, cny_out) = if peak {
        (0.10, 3.0, 9.0)
    } else {
        (0.05, 1.5, 4.5)
    };
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million: hit,
            input_cache_miss_per_million: miss,
            output_per_million: out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        },
        cny: Some(CurrencyPricing {
            input_cache_hit_per_million: cny_hit,
            input_cache_miss_per_million: cny_miss,
            output_per_million: cny_out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        }),
    }
}

/// Calculate cost from provider usage, honoring DeepSeek context-cache fields.
#[must_use]
#[cfg(test)]
pub fn calculate_turn_cost_from_usage(model: &str, usage: &Usage) -> Option<f64> {
    calculate_turn_cost_estimate_from_usage(model, usage).map(|estimate| estimate.usd)
}

/// Calculate cost from provider usage in both official currencies.
#[must_use]
#[cfg(test)]
pub fn calculate_turn_cost_estimate_from_usage(model: &str, usage: &Usage) -> Option<CostEstimate> {
    let pricing = pricing_for_model_and_usage(model, usage)?;
    Some(cost_estimate_with_pricing(pricing, usage))
}

/// Cost from a hand-sourced row, or `None` when the row cannot price a class
/// this turn actually used.
///
/// Only cache-write can fail here: input, cache-read, and output rates are
/// mandatory on every hand row, while a cache-creation rate exists only where a
/// provider publishes one or documents that writes cost nothing extra.
#[cfg(test)]
fn cost_estimate_with_pricing_checked(
    pricing: ModelPricing,
    usage: &Usage,
) -> Result<CostEstimate, Vec<TokenClass>> {
    let classes = token_usage_for_pricing(usage);
    let usd = currency_offering_pricing(pricing.usd, Currency::Usd);
    let missing = usd.unpriced_used_classes(&classes);
    if !missing.is_empty() {
        return Err(missing);
    }
    let Some(usd) = usd.estimate_cost(&classes) else {
        return Err(Vec::new());
    };
    let cny = match pricing.cny {
        Some(pricing) => currency_offering_pricing(pricing, Currency::Cny)
            .estimate_cost(&classes)
            .ok_or_else(Vec::new)?,
        None => 0.0,
    };
    Ok(CostEstimate { usd, cny })
}

/// Unchecked projection for the legacy model-only test helpers, which construct
/// usage they have already established the row can price.
///
/// Production paths must use [`cost_estimate_with_pricing_checked`] so an
/// unpublished cache-write rate fails closed instead of billing writes at the
/// input rate.
#[cfg(test)]
fn cost_estimate_with_pricing(pricing: ModelPricing, usage: &Usage) -> CostEstimate {
    cost_estimate_with_pricing_checked(pricing, usage)
        .expect("validated hand row and usage classes")
}

/// Calculate cost from provider/model usage when that pair identifies a single
/// billing surface. ChatGPT/Codex OAuth has no authoritative API dollar price,
/// while StepFun needs endpoint-derived PAYG-vs-Plan provenance; both stay
/// unpriced here rather than fabricating spend.
#[must_use]
pub fn calculate_turn_cost_estimate_for_provider(
    provider: ProviderKind,
    model: &str,
    usage: &Usage,
) -> Option<CostEstimate> {
    calculate_turn_cost_estimate_for_provider_at(provider, model, usage, Utc::now())
}

/// Calculate cost only for routes that are actually money-metered. OAuth and
/// token-plan routes deliberately return `None` even when the underlying model
/// also exists behind a separately-priced public API.
///
/// Production callers use [`audit_turn_cost_for_route`] instead: a caller that
/// adds to a total must also record why a turn was left out of it.
#[must_use]
#[cfg(test)]
pub fn calculate_turn_cost_estimate_for_route(
    provider: ProviderKind,
    model: &str,
    usage: &Usage,
    billing: crate::route_billing::BillingPresentation,
) -> Option<CostEstimate> {
    audit_turn_cost_for_route(provider, model, None, usage, Utc::now(), billing).estimate
}

/// Estimate a turn when endpoint-derived billing provenance is available.
/// StepFun's standard API and Step Plan share provider/model text but not a
/// billing system, so that route fails closed unless the PAYG surface is known.
#[must_use]
#[cfg(test)]
pub(crate) fn calculate_turn_cost_estimate_for_billing_surface(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
    usage: &Usage,
) -> Option<CostEstimate> {
    calculate_turn_cost_estimate_for_route_at(provider, model, billing_surface, usage, Utc::now())
}

/// Deterministic provider-aware estimate at the turn's recorded time.
#[must_use]
pub(crate) fn calculate_turn_cost_estimate_for_provider_at(
    provider: ProviderKind,
    model: &str,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> Option<CostEstimate> {
    audit_turn_cost_for_provider_at(provider, model, usage, recorded_at).estimate
}

/// Why a route produced no cost estimate.
///
/// Every `None` from the estimator carries one of these so `/cost`, `/cache`,
/// and the scorecard can say *why* a turn is missing from a total instead of
/// letting the total read as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnpricedReason {
    /// The route is **exactly identified** as one where money is not the unit:
    /// a named OAuth subscription, a named prepaid token plan, or a local
    /// endpoint with no provider bill. Only this reason excuses a turn from
    /// money coverage, and only exact evidence may produce it (#4318).
    NotMoneyMetered,
    /// The route may or may not meter money and CodeWhale could not establish
    /// which. Distinct from [`Self::NotMoneyMetered`] on purpose: an unknown
    /// basis is counted as *possibly missing spend*, never waved through as a
    /// subscription. A cross-provider child route with no dispatch config is
    /// the common case.
    UnknownBillingBasis,
    /// One provider/model pair spans several billing systems and the non-secret
    /// endpoint provenance needed to pick one is missing.
    AmbiguousBillingSurface,
    /// No endpoint classification was supplied for the route at all.
    ///
    /// Distinct from [`Self::UnknownBillingBasis`], which means an endpoint was
    /// classified and could not be placed. This means none was offered, so
    /// there is no evidence the turn was served by the provider's own official
    /// surface rather than a proxy, a gateway, or a self-hosted clone that
    /// happens to speak the same protocol. A provider enum plus a familiar
    /// model id is not that evidence (#4318).
    UnestablishedEndpoint,
    /// The turn's endpoint classified as a per-token surface, but the pricing
    /// layer holds no rates for that specific surface (as opposed to no rates
    /// for the model at all).
    UnpricedBillingSurface,
    /// The only pricing row found claims live provider provenance but is stale
    /// or was fetched from a different endpoint, so it is not authoritative for
    /// this turn. Never silently downgraded to "authoritative anyway".
    UnverifiedLivePricing,
    /// A compatibility alias whose published rate has been retired.
    RetiredAlias,
    /// The turn crossed a request-wide pricing tier the pricing layer cannot
    /// represent yet (for example OpenAI's >272K long-context surcharge).
    UnrepresentedTier,
    /// No pricing row exists for this provider/model route.
    NoPricingRow,
    /// Automatic gateway routing has not identified the upstream rate owner.
    RoutingDependentPrice,
    /// Saved usage predates cost coverage, or carries an unknown reason code.
    UnrecordedCoverage,
    /// Saved background accounting could not be recovered.
    LateUsageUnavailable,
    /// The bounded saved accounting ledger reached its capacity.
    LateUsageOverflow,
    /// A row exists, but a token class this turn actually used has no published
    /// price, so the estimate fails closed rather than under-reporting.
    MissingClassPrice,
    /// A catalog row contains a NaN, infinite, or negative rate. The whole row
    /// is rejected at the trust boundary rather than partially billed.
    InvalidPricingRow,
    /// The row is denominated in a currency CodeWhale does not carry. No
    /// conversion is invented.
    UnsupportedCurrency,
    /// Provider telemetry assigns more cache-hit/miss/write tokens than the
    /// reported input total. Pricing that contradictory partition would
    /// over-count input, so the call is retained but fails closed.
    InconsistentUsage,
    MissingUsageBreakdown,
}

impl UnpricedReason {
    /// Decode persisted receipts without guessing from the current provider.
    /// Older or future reason codes remain explicitly unrecorded coverage.
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        match label {
            "not_money_metered" => Self::NotMoneyMetered,
            "unknown_billing_basis" => Self::UnknownBillingBasis,
            "ambiguous_billing_surface" => Self::AmbiguousBillingSurface,
            "unestablished_endpoint" => Self::UnestablishedEndpoint,
            "unpriced_billing_surface" => Self::UnpricedBillingSurface,
            "unverified_live_pricing" => Self::UnverifiedLivePricing,
            "retired_alias" => Self::RetiredAlias,
            "unrepresented_pricing_tier" => Self::UnrepresentedTier,
            "no_pricing_row" => Self::NoPricingRow,
            "routing_dependent_price" => Self::RoutingDependentPrice,
            "missing_class_price" => Self::MissingClassPrice,
            "invalid_pricing_row" => Self::InvalidPricingRow,
            "unsupported_currency" | "currency_not_published" => Self::UnsupportedCurrency,
            "inconsistent_usage" => Self::InconsistentUsage,
            "missing_usage_breakdown" => Self::MissingUsageBreakdown,
            "late_usage_ledger_unavailable" => Self::LateUsageUnavailable,
            "late_usage_ledger_overflow" => Self::LateUsageOverflow,
            _ => Self::UnrecordedCoverage,
        }
    }

    #[must_use]
    pub const fn message_id(self) -> codewhale_localization::MessageId {
        use codewhale_localization::MessageId;
        match self {
            Self::NotMoneyMetered => MessageId::CostReasonNotMoney,
            Self::UnknownBillingBasis => MessageId::CostReasonBillingUnknown,
            Self::AmbiguousBillingSurface | Self::UnestablishedEndpoint => {
                MessageId::CostReasonEndpointUnknown
            }
            Self::UnpricedBillingSurface | Self::NoPricingRow => MessageId::CostReasonRateMissing,
            Self::UnverifiedLivePricing => MessageId::CostReasonLiveUnverified,
            Self::RetiredAlias => MessageId::CostReasonRetiredAlias,
            Self::UnrepresentedTier => MessageId::CostReasonTierMissing,
            Self::RoutingDependentPrice => MessageId::CostReasonRoutingDependent,
            Self::UnrecordedCoverage | Self::LateUsageUnavailable | Self::LateUsageOverflow => {
                MessageId::CostReasonCoverageMissing
            }
            Self::MissingClassPrice => MessageId::CostReasonTokenRateMissing,
            Self::InvalidPricingRow => MessageId::CostReasonInvalidRate,
            Self::UnsupportedCurrency => MessageId::CostReasonCurrencyMissing,
            Self::InconsistentUsage => MessageId::CostReasonUsageConflict,
            Self::MissingUsageBreakdown => MessageId::CostReasonUsageBreakdown,
        }
    }

    /// Stable, non-localized identifier for logs, JSON, and scorecards.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotMoneyMetered => "not_money_metered",
            Self::UnknownBillingBasis => "unknown_billing_basis",
            Self::AmbiguousBillingSurface => "ambiguous_billing_surface",
            Self::UnestablishedEndpoint => "unestablished_endpoint",
            Self::UnpricedBillingSurface => "unpriced_billing_surface",
            Self::UnverifiedLivePricing => "unverified_live_pricing",
            Self::RetiredAlias => "retired_alias",
            Self::UnrepresentedTier => "unrepresented_pricing_tier",
            Self::NoPricingRow => "no_pricing_row",
            Self::RoutingDependentPrice => "routing_dependent_price",
            Self::UnrecordedCoverage => "unrecorded_coverage",
            Self::LateUsageUnavailable => "late_usage_ledger_unavailable",
            Self::LateUsageOverflow => "late_usage_ledger_overflow",
            Self::MissingClassPrice => "missing_class_price",
            Self::InvalidPricingRow => "invalid_pricing_row",
            Self::UnsupportedCurrency => "unsupported_currency",
            Self::InconsistentUsage => "inconsistent_usage",
            Self::MissingUsageBreakdown => "missing_usage_breakdown",
        }
    }

    /// Whether a turn with this reason belongs in the money-metered coverage
    /// denominator `/cost` reports against its dollar total.
    ///
    /// Only [`Self::NotMoneyMetered`] — an *exactly* identified subscription,
    /// token plan, or local route — is excluded. Everything else, including an
    /// unknown billing basis, counts as spend the total is missing, because
    /// treating "don't know" as "not billed" is what let unpriced turns
    /// disappear from a total that then read as complete (#4318).
    #[must_use]
    pub fn counts_toward_money_coverage(self) -> bool {
        self != Self::NotMoneyMetered
    }
}

/// A turn cost plus the provenance and completeness needed to audit it.
///
/// `estimate.is_some()` and `unpriced_reason.is_none()` always agree: this type
/// is produced by the same code path that computes the estimate, so an audit
/// can never disagree with the number a total was built from.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnCostAudit {
    /// The cost, when the route is priced for every class this turn used.
    pub estimate: Option<CostEstimate>,
    /// Where the applied (or attempted) pricing row came from.
    pub provenance: Option<PricingProvenance>,
    /// Classes this turn used that carry no published price.
    pub unpriced_classes: Vec<TokenClass>,
    /// Why the estimate is absent, when it is.
    pub unpriced_reason: Option<UnpricedReason>,
    /// Set when a live catalog row could not be verified as authoritative for
    /// this route. Present both when the row was *degraded* to the bundled
    /// snapshot (the estimate is still priced, from the bundled row) and when
    /// there was no fallback at all. It is the receipt for the downgrade, so a
    /// `provider_live` label is never claimed for an unproven row.
    pub live_pricing_defect: Option<LivePricingDefect>,
    /// Whether the estimate is authoritative in each carried currency. A zero
    /// amount is still priced when usage is zero; these flags therefore cannot
    /// be inferred from `estimate > 0`.
    pub usd_priced: bool,
    pub cny_priced: bool,
    pub exact_usd: Option<String>,
    pub exact_cny: Option<String>,
}

impl TurnCostAudit {
    fn priced(
        estimate: CostEstimate,
        provenance: PricingProvenance,
        usd_priced: bool,
        cny_priced: bool,
    ) -> Self {
        Self {
            estimate: Some(estimate),
            provenance: Some(provenance),
            unpriced_classes: Vec::new(),
            unpriced_reason: None,
            live_pricing_defect: None,
            usd_priced,
            cny_priced,
            exact_usd: None,
            exact_cny: None,
        }
    }

    pub(crate) fn unpriced(reason: UnpricedReason) -> Self {
        Self {
            estimate: None,
            provenance: None,
            unpriced_classes: Vec::new(),
            unpriced_reason: Some(reason),
            live_pricing_defect: None,
            usd_priced: false,
            cny_priced: false,
            exact_usd: None,
            exact_cny: None,
        }
    }

    fn missing_classes(provenance: PricingProvenance, classes: Vec<TokenClass>) -> Self {
        Self {
            estimate: None,
            provenance: Some(provenance),
            unpriced_classes: classes,
            unpriced_reason: Some(UnpricedReason::MissingClassPrice),
            live_pricing_defect: None,
            usd_priced: false,
            cny_priced: false,
            exact_usd: None,
            exact_cny: None,
        }
    }

    fn unverified_live(defect: LivePricingDefect) -> Self {
        Self {
            estimate: None,
            // Deliberately not `ProviderLive`: an unverified row must never be
            // labelled with authoritative live provenance.
            provenance: Some(PricingProvenance::Unknown),
            unpriced_classes: Vec::new(),
            unpriced_reason: Some(UnpricedReason::UnverifiedLivePricing),
            live_pricing_defect: Some(defect),
            usd_priced: false,
            cny_priced: false,
            exact_usd: None,
            exact_cny: None,
        }
    }

    /// Attach a live-pricing downgrade receipt to an otherwise complete audit.
    fn with_live_defect(mut self, defect: Option<LivePricingDefect>) -> Self {
        if let Some(defect) = defect {
            self.live_pricing_defect = Some(defect);
        }
        self
    }

    /// Whether this turn contributed an authoritative number to a total.
    #[must_use]
    #[cfg(test)]
    pub fn is_priced(&self) -> bool {
        self.estimate.is_some()
    }

    /// Whether the estimate is authoritative in the requested display
    /// currency. Exact zero remains priced; the boolean provenance flags are
    /// intentionally not inferred from the numeric amount.
    #[must_use]
    pub fn is_priced_in(&self, currency: CostCurrency) -> bool {
        self.estimate.is_some()
            && match currency {
                CostCurrency::Usd => self.usd_priced,
                CostCurrency::Cny => self.cny_priced,
            }
    }

    /// Whether this turn belongs in the money-metered coverage denominator.
    ///
    /// Priced turns always do. Unpriced ones do unless the route was *exactly*
    /// identified as non-metered.
    #[must_use]
    pub fn counts_toward_money_coverage(&self) -> bool {
        self.unpriced_reason
            .is_none_or(UnpricedReason::counts_toward_money_coverage)
    }
}

/// Audit a turn on a provider/model route, without knowing which endpoint served
/// it. A live catalog row cannot be *confirmed* for an unknown endpoint, so this
/// path degrades to the bundled published snapshot; use
/// [`audit_turn_cost_for_provider_on_endpoint_at`] when the base URL is known.
#[must_use]
pub(crate) fn audit_turn_cost_for_provider_at(
    provider: ProviderKind,
    model: &str,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> TurnCostAudit {
    audit_turn_cost_for_provider_on_endpoint_at(provider, model, None, usage, recorded_at)
}

/// Audit a turn's cost on a provider/model route at its recorded time.
///
/// This is the single implementation; `calculate_turn_cost_estimate_*` are thin
/// projections of it, so no caller can build a total from one rule set while
/// reporting completeness from another.
#[must_use]
pub(crate) fn audit_turn_cost_for_provider_on_endpoint_at(
    provider: ProviderKind,
    model: &str,
    endpoint_fingerprint: Option<&str>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> TurnCostAudit {
    audit_turn_cost_for_provider_on_endpoint_for_identity_at(
        provider,
        None,
        model,
        endpoint_fingerprint,
        usage,
        recorded_at,
        true,
        None,
    )
}

/// Identity-aware provider audit for named compatible routes.
///
/// `ProviderKind::Custom` is only a transport family, so it is never sufficient
/// pricing provenance on its own. Baseten is the first reviewed compatible
/// provider whose authenticated live catalog can price actual usage; every
/// other custom identity stays unknown until it receives an equivalent
/// provider/endpoint contract.
#[must_use]
fn audit_turn_cost_for_provider_on_endpoint_for_identity_at(
    provider: ProviderKind,
    provider_identity: Option<&str>,
    model: &str,
    endpoint_fingerprint: Option<&str>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
    allow_cloud_catalog: bool,
    frozen_cloud_pricing: Option<OfferingPricing>,
) -> TurnCostAudit {
    if !usage_cache_partition_is_consistent(usage) {
        return TurnCostAudit::unpriced(UnpricedReason::InconsistentUsage);
    }
    if provider == ProviderKind::OpenaiCodex {
        return TurnCostAudit::unpriced(UnpricedReason::AmbiguousBillingSurface);
    }
    if provider == ProviderKind::Custom {
        // A transport family plus current mutable catalog state is not a
        // billing receipt. Reviewed custom routes are priced only by the
        // frozen dispatch quote handled in the route-audit path below.
        return TurnCostAudit::unpriced(UnpricedReason::UnknownBillingBasis);
    }
    if route_requires_billing_surface(provider, model) {
        return TurnCostAudit::unpriced(UnpricedReason::AmbiguousBillingSurface);
    }
    let normalized_model = model.trim();
    let model_lower = normalized_model.to_ascii_lowercase();
    let direct_deepseek = matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    );
    let Some(canonical_model) = canonical_model_id_for_provider(provider, normalized_model) else {
        return TurnCostAudit::unpriced(UnpricedReason::NoPricingRow);
    };
    let catalog_model = if direct_deepseek
        && matches!(model_lower.as_str(), "deepseek-chat" | "deepseek-reasoner")
    {
        let Ok(retirement) = DateTime::parse_from_rfc3339(DEEPSEEK_ALIAS_RETIREMENT_UTC) else {
            return TurnCostAudit::unpriced(UnpricedReason::NoPricingRow);
        };
        if recorded_at >= retirement.with_timezone(&Utc) {
            return TurnCostAudit::unpriced(UnpricedReason::RetiredAlias);
        }
        "deepseek-v4-flash".to_string()
    } else if direct_deepseek
        && matches!(
            model_lower.as_str(),
            "flash-vision" | "deepseek-v4flashvisionexp"
        )
        && recorded_at < deepseek_flash_effective_from()
    {
        "deepseek-v4-flash-vision-exp".to_string()
    } else {
        canonical_model
    };

    if direct_openai_long_context_tier_is_unpriced(provider, &catalog_model, usage.input_tokens) {
        return TurnCostAudit::unpriced(UnpricedReason::UnrepresentedTier);
    }

    // Direct DeepSeek pricing carries an authoritative CNY row and recorded-time
    // peak/off-peak tiers that a static catalog row cannot represent. These
    // exact first-party routes intentionally override the catalog; no other
    // provider/model text match is allowed to do so.
    if matches!(
        provider,
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
    ) && catalog_model.eq_ignore_ascii_case("minimax-m3")
    {
        return hand_provider_audit(provider, &catalog_model, usage, recorded_at);
    }

    if provider == ProviderKind::Xai && is_grok_tiered(&catalog_model) {
        return hand_provider_audit(provider, &catalog_model, usage, recorded_at);
    }

    if direct_deepseek
        || (provider == ProviderKind::Anthropic
            && catalog_model.eq_ignore_ascii_case("claude-sonnet-5"))
    {
        return hand_provider_audit(provider, &catalog_model, usage, recorded_at);
    }

    if let Some(pricing) = frozen_cloud_pricing {
        return audit_offering_pricing(pricing, usage);
    }
    let classes = token_usage_for_pricing(usage);
    // A live catalog row is only authoritative when it is fresh *and* was
    // fetched from the endpoint this turn was served on. When it is not, degrade
    // to the bundled published snapshot and receipt the defect; only if there is
    // no bundled row at all does the turn fail closed (#4318).
    let mut live_defect = None;
    let offering = match verified_catalog_offering(
        provider,
        provider_identity,
        &catalog_model,
        endpoint_fingerprint,
        recorded_at,
        allow_cloud_catalog,
    ) {
        VerifiedOffering::Usable(offering) => Some(offering),
        VerifiedOffering::DegradedToBundled { offering, defect } => {
            live_defect = Some(defect);
            Some(offering)
        }
        VerifiedOffering::Unusable(defect) => {
            live_defect = Some(defect);
            None
        }
        VerifiedOffering::FutureEffective => {
            return TurnCostAudit::unpriced(UnpricedReason::UnverifiedLivePricing);
        }
        VerifiedOffering::Absent => None,
    };

    if let Some(audit) = offering
        .as_ref()
        .and_then(|offering| refused_catalog_pricing_audit(offering, usage))
    {
        return audit.with_live_defect(live_defect);
    }

    if let Some(offering) = offering.as_ref()
        && let Some(pricing) =
            effective_offering_pricing(provider, &catalog_model, offering, &classes)
    {
        return audit_offering_pricing(pricing, usage).with_live_defect(live_defect);
    }

    // A few first-party rows predate or intentionally omit a Models.dev entry
    // (for example OpenAI API `gpt-5-codex` and MiniMax `minimax-m2.7`).
    // Preserve only an explicit provider-owned allowlist here;
    // a costless foreign/catalog route must remain unpriced.
    let hand_row = provider_owned_hand_pricing_at(provider, &catalog_model, recorded_at);

    // An unverifiable live row with no bundled fallback and no hand row is a
    // route CodeWhale cannot price truthfully. Say which, rather than reporting
    // the unverified rate or a bare "no pricing row".
    match (live_defect, hand_row) {
        (Some(defect), None) => TurnCostAudit::unverified_live(defect),
        // Concentrate publishes different upstream rates, and even a requested
        // provider/model can fail over. A slash is not a billing receipt. Keep
        // verified scoped offerings and operator overrides above authoritative;
        // absent those, do not inherit a model owner's or aggregate rate.
        // https://concentrate.ai/docs/api-reference/endpoint/auto-routing
        (None, None) if provider == ProviderKind::Concentrate => {
            TurnCostAudit::unpriced(UnpricedReason::RoutingDependentPrice)
        }
        (defect, hand_row) => hand_priced_audit(hand_row, usage).with_live_defect(defect),
    }
}

/// Convert malformed catalog numerics, or a context tier this turn reached,
/// into an explicit runtime audit reason.
/// Keeping this distinct from the ordinary `None` projection prevents a bad
/// published row from becoming indistinguishable from an absent price.
fn refused_catalog_pricing_audit(
    offering: &codewhale_config::catalog::CatalogOffering,
    usage: &Usage,
) -> Option<TurnCostAudit> {
    let cost = offering.cost.as_ref()?;
    if !codewhale_config::pricing::catalog_cost_is_valid(cost) {
        return Some(TurnCostAudit::unpriced(UnpricedReason::InvalidPricingRow));
    }
    let pricing = OfferingPricing::from_catalog_offering(offering)?;
    matches!(
        pricing.quote_cost(&token_usage_for_pricing(usage)),
        Err(PricingQuoteError::UnsupportedTier)
    )
    .then(|| TurnCostAudit::unpriced(UnpricedReason::UnrepresentedTier))
}

/// Outcome of checking a catalog row's pricing provenance against the route.
enum VerifiedOffering {
    /// The row is authoritative as-is (bundled, user override, or a live row
    /// proven fresh and endpoint-matched).
    Usable(codewhale_config::catalog::CatalogOffering),
    /// The live row could not be verified, so the bundled published row is used
    /// instead. The defect is retained as the receipt for why.
    DegradedToBundled {
        offering: codewhale_config::catalog::CatalogOffering,
        defect: LivePricingDefect,
    },
    /// The live row could not be verified and no bundled row exists.
    Unusable(LivePricingDefect),
    /// The row claims it was fetched after this turn was dispatched. Clock
    /// saturation must never turn a future price into an age-zero price.
    FutureEffective,
    /// No catalog row for this provider/model at all.
    Absent,
}

/// Resolve the catalog row to price against, refusing to treat an unverifiable
/// live row as authoritative.
///
/// `endpoint_fingerprint` is the non-secret SHA-256 digest of the base URL the turn
/// was actually served on (see [`codewhale_config::catalog::base_url_fingerprint`]).
/// Callers that do not know the endpoint pass `None`, which cannot *confirm* a
/// live row — so those callers degrade to the bundled snapshot rather than
/// billing against a rate whose endpoint scope is unproven.
fn verified_catalog_offering(
    provider: ProviderKind,
    provider_identity: Option<&str>,
    catalog_model: &str,
    endpoint_fingerprint: Option<&str>,
    recorded_at: DateTime<Utc>,
    allow_cloud_catalog: bool,
) -> VerifiedOffering {
    let Some(mut offering) = crate::provider_lake::catalog_offering_for_model_identity(
        provider,
        provider_identity,
        catalog_model,
    ) else {
        return VerifiedOffering::Absent;
    };
    if allow_cloud_catalog
        && let codewhale_config::catalog::CatalogSource::CloudFacts {
            fetched_at,
            valid_until,
            ..
        } = offering.pricing_source()
    {
        let at = u64::try_from(recorded_at.timestamp()).unwrap_or(0);
        if *fetched_at > at {
            return VerifiedOffering::FutureEffective;
        }
        if valid_until.is_some_and(|expires| at > expires) {
            return crate::provider_lake::bundled_catalog_offering_for_model(
                provider,
                catalog_model,
            )
            .map(VerifiedOffering::Usable)
            .unwrap_or(VerifiedOffering::Absent);
        }
    }
    let cloud_price = matches!(
        offering.pricing_source(),
        codewhale_config::catalog::CatalogSource::CloudFacts { .. }
    );
    if cloud_price
        && (!allow_cloud_catalog
            || endpoint_fingerprint.is_some_and(|fingerprint| {
                codewhale_config::catalog::base_url_fingerprint(
                    provider.provider().default_base_url(),
                ) != fingerprint
            }))
    {
        return crate::provider_lake::bundled_catalog_offering_for_model(provider, catalog_model)
            .map(VerifiedOffering::Usable)
            .unwrap_or(VerifiedOffering::Absent);
    }
    let models_dev_live_fetched_at = match offering.pricing_source() {
        codewhale_config::catalog::CatalogSource::Live { fetched_at, .. }
            if crate::provider_lake::live_catalog_origin(provider, catalog_model)
                == Some(crate::provider_lake::LiveSource::ModelsDev) =>
        {
            Some(*fetched_at)
        }
        _ => None,
    };
    if let Some(fetched_at) = models_dev_live_fetched_at {
        offering.cost_source =
            Some(codewhale_config::catalog::CatalogSource::ModelsDevLive { fetched_at });
    }
    let Some(pricing) = OfferingPricing::from_catalog_offering_at(
        &offering,
        u64::try_from(recorded_at.timestamp()).unwrap_or(0),
    ) else {
        // No priced row to verify; downstream treats this as unpriced.
        return VerifiedOffering::Usable(offering);
    };
    // `recorded_at` is the turn's own clock, which is the right reference for
    // "was this price current when the turn happened".
    let now_unix = u64::try_from(recorded_at.timestamp()).ok();
    if matches!(
        pricing.provenance,
        PricingProvenance::ProviderLive | PricingProvenance::ModelsDevLive
    ) && pricing
        .fetched_at
        .or(pricing.effective_at)
        .zip(now_unix)
        .is_some_and(|(effective_at, dispatched_at)| effective_at > dispatched_at)
    {
        return VerifiedOffering::FutureEffective;
    }
    let Some(defect) =
        pricing.live_pricing_defect(endpoint_fingerprint, now_unix, LIVE_PRICING_MAX_AGE_SECS)
    else {
        return VerifiedOffering::Usable(offering);
    };
    match crate::provider_lake::bundled_catalog_offering_for_model(provider, catalog_model) {
        Some(bundled) => VerifiedOffering::DegradedToBundled {
            offering: bundled,
            defect,
        },
        None => VerifiedOffering::Unusable(defect),
    }
}

/// Project a hand-sourced provider row into an audit.
///
/// A hand row always publishes input, cache-read, and output rates. Cache-write
/// is the one class that can be genuinely absent: only providers that publish a
/// write premium, or document that cache creation carries no separate charge,
/// can price it. A turn that wrote to cache on a row with neither fact fails
/// closed and names the class, rather than being billed at the input rate on the
/// strength of an assumption (#4318).
fn hand_priced_audit(pricing: Option<ModelPricing>, usage: &Usage) -> TurnCostAudit {
    let Some(pricing) = pricing else {
        return TurnCostAudit::unpriced(UnpricedReason::NoPricingRow);
    };
    audit_pricing_pair(
        currency_offering_pricing(pricing.usd, Currency::Usd),
        pricing
            .cny
            .map(|row| currency_offering_pricing(row, Currency::Cny)),
        usage,
    )
}

fn audit_pricing_pair(
    primary: OfferingPricing,
    secondary: Option<OfferingPricing>,
    usage: &Usage,
) -> TurnCostAudit {
    let mut audit = audit_offering_pricing(primary, usage);
    if audit.estimate.is_none() {
        return audit;
    }
    if let Some(secondary) = secondary {
        let secondary = audit_offering_pricing(secondary, usage);
        let Some(cost) = secondary.estimate else {
            return secondary;
        };
        audit.estimate.as_mut().expect("priced primary").cny = cost.cny;
        audit.cny_priced = secondary.cny_priced;
        audit.exact_cny = secondary.exact_cny;
    }
    audit
}

pub(crate) fn provider_owned_hand_quote_rows(
    provider: ProviderKind,
    model: &str,
    recorded_at: DateTime<Utc>,
) -> Option<(OfferingPricing, Option<OfferingPricing>)> {
    let pricing = provider_owned_hand_pricing_at(provider, model, recorded_at)?;
    let mut primary = currency_offering_pricing(pricing.usd, Currency::Usd);
    let mut secondary = pricing
        .cny
        .map(|row| currency_offering_pricing(row, Currency::Cny));
    let tier = if provider == ProviderKind::Xai && is_grok_tiered(model) {
        Some((
            u64::from(GROK_4_6_LONG_CONTEXT_THRESHOLD),
            true,
            grok_tiered_pricing(&model.to_ascii_lowercase(), true)?,
        ))
    } else if matches!(
        provider,
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
    ) && is_minimax_m3(model)
    {
        Some((
            u64::from(MINIMAX_M3_LONG_CONTEXT_THRESHOLD),
            false,
            minimax_m3_standard_pricing(true),
        ))
    } else {
        None
    };
    if let Some((size, inclusive, higher)) = tier {
        primary
            .context_tiers
            .push(codewhale_config::models_dev::ModelsDevCostTier {
                tier: codewhale_config::models_dev::ModelsDevContextTier {
                    kind: "context".into(),
                    size,
                },
                cost: currency_offering_pricing(higher.usd, Currency::Usd).cost_structure(),
            });
        primary.context_tiers_inclusive = inclusive;
        if let Some(row) = secondary.as_mut()
            && let Some(higher) = higher.cny
        {
            row.context_tiers
                .push(codewhale_config::models_dev::ModelsDevCostTier {
                    tier: codewhale_config::models_dev::ModelsDevContextTier {
                        kind: "context".into(),
                        size,
                    },
                    cost: currency_offering_pricing(higher, Currency::Cny).cost_structure(),
                });
            row.context_tiers_inclusive = inclusive;
        }
    }
    for row in std::iter::once(&mut primary).chain(secondary.iter_mut()) {
        row.provider = provider.as_str().into();
        row.wire_model_id = model.into();
        row.source_ref = Some(
            match provider {
                ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic => {
                    "https://api-docs.deepseek.com/quick_start/pricing"
                }
                ProviderKind::Xai => "https://docs.x.ai/docs/models",
                ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => {
                    "https://platform.minimax.io/docs/guides/pricing-paygo"
                }
                ProviderKind::Anthropic => {
                    "https://platform.claude.com/docs/en/about-claude/pricing"
                }
                _ => "codewhale:reviewed-provider-price",
            }
            .into(),
        );
    }
    Some((primary, secondary))
}

fn hand_provider_audit(
    provider: ProviderKind,
    model: &str,
    usage: &Usage,
    at: DateTime<Utc>,
) -> TurnCostAudit {
    provider_owned_hand_quote_rows(provider, model, at).map_or_else(
        || TurnCostAudit::unpriced(UnpricedReason::NoPricingRow),
        |(primary, secondary)| audit_pricing_pair(primary, secondary, usage),
    )
}

/// Recorded-time variant with explicit billing-surface provenance.
#[must_use]
#[cfg(test)]
pub(crate) fn calculate_turn_cost_estimate_for_route_at(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> Option<CostEstimate> {
    audit_turn_cost_for_route_at(provider, model, billing_surface, usage, recorded_at).estimate
}

/// Audit a turn's cost with endpoint-derived billing provenance.
#[must_use]
pub(crate) fn audit_turn_cost_for_route_at(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> TurnCostAudit {
    audit_turn_cost_for_route_on_endpoint_at(
        provider,
        model,
        billing_surface,
        None,
        usage,
        recorded_at,
    )
}

/// Audit a turn's cost with both endpoint-derived billing provenance and the
/// endpoint fingerprint needed to verify live catalog pricing.
#[must_use]
pub(crate) fn audit_turn_cost_for_route_on_endpoint_at(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
    endpoint_fingerprint: Option<&str>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> TurnCostAudit {
    audit_turn_cost_for_route_on_endpoint_for_identity_at(
        provider,
        None,
        model,
        billing_surface,
        endpoint_fingerprint,
        None,
        usage,
        recorded_at,
    )
}

/// Identity-aware route audit for an immutable dispatch receipt.
#[must_use]
pub(crate) fn audit_turn_cost_for_route_on_endpoint_for_identity_at(
    provider: ProviderKind,
    provider_identity: Option<&str>,
    model: &str,
    billing_surface: Option<&str>,
    endpoint_fingerprint: Option<&str>,
    provider_live_pricing: Option<&crate::provider_catalog_live::ProviderLivePricingQuote>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> TurnCostAudit {
    if matches!(
        endpoint_metering_for_billing_surface(billing_surface),
        EndpointMetering::ExactSubscription | EndpointMetering::LocalNoBill
    ) {
        return TurnCostAudit::unpriced(UnpricedReason::NotMoneyMetered);
    }

    let frozen_pricing = match provider_live_pricing {
        Some(quote) => {
            let binding = provider_identity
                .zip(endpoint_fingerprint)
                .zip(u64::try_from(recorded_at.timestamp()).ok());
            let Some(((identity, fingerprint), at)) = binding else {
                return TurnCostAudit::unpriced(UnpricedReason::UnverifiedLivePricing);
            };
            let Some(pricing) = quote.pricing_for_route(provider, identity, model, fingerprint, at)
            else {
                return TurnCostAudit::unpriced(UnpricedReason::UnverifiedLivePricing);
            };
            let secondary =
                quote.secondary_pricing_for_route(provider, identity, model, fingerprint, at);
            Some((pricing, secondary))
        }
        None => None,
    };
    let declared = frozen_pricing
        .as_ref()
        .is_some_and(|(pricing, _)| pricing.provenance == PricingProvenance::UserOverride);
    let reviewed_custom_metered = reviewed_custom_route_is_metered(provider, endpoint_fingerprint);
    let reviewed_provider_live =
        reviewed_provider_live_route_is_metered(provider, provider_identity, endpoint_fingerprint);
    // An explicitly recorded surface is evidence.  Exact non-metered surfaces
    // override provider guesses; an explicit unknown/unrecognized surface must
    // fail closed and may never fall through to a familiar model's hand row.
    match endpoint_metering_for_billing_surface(billing_surface) {
        EndpointMetering::ExactSubscription | EndpointMetering::LocalNoBill => {
            return TurnCostAudit::unpriced(UnpricedReason::NotMoneyMetered);
        }
        EndpointMetering::Unknown
            if billing_surface.is_some() && !reviewed_custom_metered && !declared =>
        {
            return TurnCostAudit::unpriced(UnpricedReason::UnknownBillingBasis);
        }
        EndpointMetering::Unknown | EndpointMetering::Money => {}
    }
    if !usage_cache_partition_is_consistent(usage) {
        return TurnCostAudit::unpriced(UnpricedReason::InconsistentUsage);
    }
    if declared && let Some((pricing, secondary)) = frozen_pricing.clone() {
        return audit_pricing_pair(pricing, secondary, usage);
    }
    if provider == ProviderKind::Stepfun {
        return match pricing_for_billing_surface(provider, model, billing_surface) {
            // Each model keeps its documented cache-write policy; unpublished
            // write rates fail closed instead of borrowing another model's rate.
            Some(pricing) => hand_priced_audit(Some(pricing), usage),
            // The surface classified as per-token but no rates exist for it, or
            // no surface was established at all.
            None => TurnCostAudit::unpriced(match billing_surface {
                Some(_) => UnpricedReason::UnpricedBillingSurface,
                None => UnpricedReason::AmbiguousBillingSurface,
            }),
        };
    }
    if stepfun_payg_pricing(model).is_some() {
        return TurnCostAudit::unpriced(UnpricedReason::AmbiguousBillingSurface);
    }
    // This is the *route* audit: the caller is asserting it knows which
    // endpoint served the turn. With no classification at all, nothing
    // distinguishes the provider's own official surface from a proxy, a
    // gateway, or a self-hosted clone speaking the same protocol — a provider
    // enum plus a familiar model id is not evidence of an official endpoint.
    // So the turn prices as unknown rather than at official rates.
    //
    // Callers that genuinely hold only a provider and a model use
    // `audit_turn_cost_for_provider_*`, which says so in its name and carries
    // its own weaker claim.
    if billing_surface.is_none() {
        return TurnCostAudit::unpriced(UnpricedReason::UnestablishedEndpoint);
    }
    if let Some((pricing, secondary)) = frozen_pricing {
        return audit_pricing_pair(pricing, secondary, usage);
    }
    if reviewed_provider_live {
        return if provider == ProviderKind::Openrouter {
            audit_openrouter_immutable_pricing(model, usage, recorded_at)
        } else {
            TurnCostAudit::unpriced(UnpricedReason::UnverifiedLivePricing)
        };
    }

    if provider == ProviderKind::Openrouter && provider_identity.is_some() {
        // A persisted built-in OpenRouter receipt that is missing the exact
        // official identity/endpoint binding (or its frozen quote) must not
        // fall through to the mutable process-wide provider lake.
        return TurnCostAudit::unpriced(UnpricedReason::UnverifiedLivePricing);
    }
    if provider == ProviderKind::Custom {
        return TurnCostAudit::unpriced(UnpricedReason::UnknownBillingBasis);
    }
    audit_turn_cost_for_provider_on_endpoint_for_identity_at(
        provider,
        provider_identity,
        model,
        endpoint_fingerprint,
        usage,
        recorded_at,
        false,
        None,
    )
}

fn audit_offering_pricing(pricing: OfferingPricing, usage: &Usage) -> TurnCostAudit {
    if pricing.rate_withheld {
        return TurnCostAudit::unpriced(UnpricedReason::NoPricingRow);
    }
    let classes = token_usage_for_pricing(usage);
    let quote = match pricing.quote_cost(&classes) {
        Ok(quote) => quote,
        Err(PricingQuoteError::MissingRate(class)) => {
            let mut missing = pricing.unpriced_used_classes(&classes);
            if !missing.contains(&class) {
                missing.push(class);
            }
            return TurnCostAudit::missing_classes(pricing.provenance, missing);
        }
        Err(PricingQuoteError::MissingUsage(_)) => {
            return TurnCostAudit::unpriced(UnpricedReason::MissingUsageBreakdown);
        }
        Err(PricingQuoteError::UnsupportedTier) => {
            return TurnCostAudit::unpriced(UnpricedReason::UnrepresentedTier);
        }
        Err(PricingQuoteError::InvalidUsage) => {
            return TurnCostAudit::unpriced(UnpricedReason::InconsistentUsage);
        }
        Err(PricingQuoteError::InvalidRate) => {
            return TurnCostAudit::unpriced(UnpricedReason::InvalidPricingRow);
        }
    };
    let Ok(amount) = quote.amount.parse::<f64>() else {
        return TurnCostAudit::unpriced(UnpricedReason::InvalidPricingRow);
    };
    let (estimate, usd, cny) = match quote.currency {
        Currency::Usd => (CostEstimate::usd_only(amount), true, false),
        Currency::Cny => (
            CostEstimate {
                usd: 0.0,
                cny: amount,
            },
            false,
            true,
        ),
        Currency::Other(_) => return TurnCostAudit::unpriced(UnpricedReason::UnsupportedCurrency),
    };
    let mut audit = TurnCostAudit::priced(estimate, pricing.provenance, usd, cny);
    if usd {
        audit.exact_usd = Some(quote.amount);
    } else {
        audit.exact_cny = Some(quote.amount);
    }
    audit
}

/// Price an exact official OpenRouter route without consulting mutable live
/// catalog state. This is the no-quote application-dispatch fallback used when
/// CodeWhale starts offline or the provider refresh has not completed yet.
fn audit_openrouter_immutable_pricing(
    model: &str,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
) -> TurnCostAudit {
    let Some(canonical_model) = canonical_model_id_for_provider(ProviderKind::Openrouter, model)
    else {
        return TurnCostAudit::unpriced(UnpricedReason::NoPricingRow);
    };
    if let Some(offering) = crate::provider_lake::bundled_catalog_offering_for_model(
        ProviderKind::Openrouter,
        &canonical_model,
    ) {
        if let Some(audit) = refused_catalog_pricing_audit(&offering, usage) {
            return audit;
        }
        if let Some(pricing) = OfferingPricing::from_catalog_offering(&offering) {
            return audit_offering_pricing(pricing, usage);
        }
    }

    hand_priced_audit(
        provider_owned_hand_pricing_at(ProviderKind::Openrouter, &canonical_model, recorded_at),
        usage,
    )
}

/// Whether a named custom route has a reviewed per-token billing contract.
///
/// Baseten is accepted only through the fingerprint of its documented Model
/// APIs endpoint (#6289). The table name is irrelevant: a Baseten identity
/// pointed at another host cannot become metered, and any table pointed at
/// Baseten carries Baseten's billing contract. A priced `/models` row alone
/// never mints metering.
#[must_use]
pub(crate) fn reviewed_custom_route_is_metered(
    provider: ProviderKind,
    endpoint_fingerprint: Option<&str>,
) -> bool {
    if provider != ProviderKind::Custom {
        return false;
    }
    endpoint_fingerprint.is_some_and(|fingerprint| {
        fingerprint
            == codewhale_config::catalog::base_url_fingerprint(
                codewhale_config::catalog::BASETEN_BASE_URL,
            )
    })
}

/// Exact routes whose mutable provider-live rates must be frozen at the
/// pre-permit application-dispatch boundary.
///
/// OpenRouter is accepted only as the built-in identity on its official API;
/// a custom table shadowing that name or an endpoint override is a different
/// billing contract. Custom tables are metered only on Baseten's endpoint
/// fingerprint and retain their exact, case-sensitive cache ownership.
#[must_use]
fn reviewed_provider_live_route_is_metered(
    provider: ProviderKind,
    provider_identity: Option<&str>,
    endpoint_fingerprint: Option<&str>,
) -> bool {
    match provider {
        ProviderKind::Openrouter => {
            provider_identity.map(str::trim) == Some(ProviderKind::Openrouter.as_str())
                && endpoint_fingerprint.is_some_and(|fingerprint| {
                    fingerprint
                        == codewhale_config::catalog::base_url_fingerprint(
                            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
                        )
                })
        }
        ProviderKind::Custom => reviewed_custom_route_is_metered(provider, endpoint_fingerprint),
        _ => false,
    }
}

/// Audit a turn against the route's billing presentation.
///
/// The three non-metered presentations are **not** interchangeable, and
/// collapsing them was the bug (#4318):
///
/// - [`BillingPresentation::Subscription`] and [`BillingPresentation::Local`]
///   are exact evidence that money is the wrong unit, so those turns are
///   `NotMoneyMetered` and drop out of the coverage denominator.
/// - [`BillingPresentation::Unknown`] is *not* such evidence. It means CodeWhale
///   could not establish the basis, so the turn is `UnknownBillingBasis`: still
///   unpriced, but counted as spend the total may be missing.
///
/// [`BillingPresentation::Subscription`]: crate::route_billing::BillingPresentation::Subscription
/// [`BillingPresentation::Local`]: crate::route_billing::BillingPresentation::Local
/// [`BillingPresentation::Unknown`]: crate::route_billing::BillingPresentation::Unknown
#[must_use]
#[cfg(test)]
pub fn audit_turn_cost_for_route(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
    billing: crate::route_billing::BillingPresentation,
) -> TurnCostAudit {
    audit_turn_cost_for_route_on_endpoint(
        provider,
        model,
        billing_surface,
        None,
        usage,
        recorded_at,
        billing,
    )
}

/// [`audit_turn_cost_for_route`] plus the endpoint fingerprint that lets live
/// catalog pricing be verified for this exact route.
#[must_use]
#[cfg(test)]
pub fn audit_turn_cost_for_route_on_endpoint(
    provider: ProviderKind,
    model: &str,
    billing_surface: Option<&str>,
    endpoint_fingerprint: Option<&str>,
    usage: &Usage,
    recorded_at: DateTime<Utc>,
    billing: crate::route_billing::BillingPresentation,
) -> TurnCostAudit {
    use crate::route_billing::BillingPresentation;
    match billing {
        BillingPresentation::Subscription(_) | BillingPresentation::Local => {
            return TurnCostAudit::unpriced(UnpricedReason::NotMoneyMetered);
        }
        BillingPresentation::Unknown => {
            return TurnCostAudit::unpriced(UnpricedReason::UnknownBillingBasis);
        }
        BillingPresentation::Metered => {}
    }
    // A metered presentation still has to survive the endpoint classification:
    // an endpoint that classifies as an exact subscription surface overrides a
    // metered guess, and an unclassifiable one fails closed.
    match endpoint_metering_for_billing_surface(billing_surface) {
        EndpointMetering::ExactSubscription | EndpointMetering::LocalNoBill => {
            return TurnCostAudit::unpriced(UnpricedReason::NotMoneyMetered);
        }
        // `Unknown` here is the common, benign case of a caller that has no
        // endpoint to classify; the provider/model path below still decides.
        EndpointMetering::Unknown if billing_surface.is_some() => {
            return TurnCostAudit::unpriced(UnpricedReason::UnknownBillingBasis);
        }
        EndpointMetering::Unknown | EndpointMetering::Money => {}
    }
    audit_turn_cost_for_route_on_endpoint_at(
        provider,
        model,
        billing_surface,
        endpoint_fingerprint,
        usage,
        recorded_at,
    )
}

fn provider_owned_hand_pricing_at(
    provider: ProviderKind,
    model: &str,
    recorded_at: DateTime<Utc>,
) -> Option<ModelPricing> {
    let model_lower = model.trim().to_ascii_lowercase();
    // Hosted Fireworks / OpenCode Zen rates are provider-owned docs rows, not
    // first-party DeepSeek's $0.0028 cache-hit card and not Models.dev.
    if provider == ProviderKind::Fireworks {
        return fireworks_bundled_fallback_pricing(&model_lower);
    }
    if provider == ProviderKind::OpencodeZen {
        return opencode_zen_bundled_fallback_pricing(&model_lower);
    }
    let provider_owns_row = match provider {
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic => {
            // `deepseek-flash` is V4.1 Flash on the first-party API. Only the
            // first-party family gains it: third-party hosts below keep their
            // own published tables and must not be assumed to serve a model
            // just because DeepSeek does.
            matches!(
                model_lower.as_str(),
                "deepseek-v4-pro"
                    | "deepseek-v4-flash"
                    | "deepseek-v4-flash-vision-exp"
                    | "deepseek-flash"
            )
        }
        ProviderKind::Openai => matches!(
            model_lower.as_str(),
            "gpt-5-codex"
                | "gpt-5.3-codex"
                | "gpt-5.5"
                | "gpt-5.5-pro"
                | "gpt-5.6"
                | "gpt-5.6-sol"
                | "gpt-5.6-terra"
                | "gpt-5.6-luna"
                | "gpt-6-sol"
                | "gpt-6.1-sol"
                | "gpt-6-luna"
        ),
        ProviderKind::Anthropic => matches!(
            model_lower.as_str(),
            "claude-opus-4-8"
                | "claude-sonnet-4-6"
                | "claude-haiku-4-5"
                | "claude-fable-5"
                | "claude-sonnet-5"
                | "claude-opus-5"
                | "claude-opus-5-5"
                | "claude-sonnet-5-5"
                | "claude-fable-5-1"
        ),
        ProviderKind::Xai => is_grok_tiered(&model_lower),
        // GLM-5.3 is deliberately absent: this allowlist declares that Z.ai
        // owns a *hand-written price row* for the model, and no GLM-5.3 rate
        // has been published. An absent price is honest; an owned-but-empty
        // row is not. See `glm_5_3_has_no_hardcoded_price` below.
        // GLM-5.3-Flash *does* have a published USD list (2026-08-26).
        ProviderKind::Zai => matches!(
            model_lower.as_str(),
            "glm-5.1" | "glm-5.2" | "glm-5.3-flash" | "glm-5-turbo"
        ),
        // `k3` (Kimi Code membership) is deliberately absent: it is quota
        // billed and must never inherit the direct-platform kimi-k3 rate.
        ProviderKind::Moonshot => matches!(
            model_lower.as_str(),
            "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k2.7-code-highspeed" | "kimi-k3"
        ),
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => matches!(
            model_lower.as_str(),
            "minimax-m3" | "minimax-m2.7" | "minimax-m2.7-highspeed"
        ),
        ProviderKind::Mistral => matches!(
            model_lower.as_str(),
            "mistral-medium-latest"
                | "mistral-medium-3-5"
                | "mistral-medium-3.5"
                | "mistral-medium-2604"
                | "mistral-large-latest"
                | "mistral-large-2512"
                | "mistral-small-latest"
                | "mistral-small-2603"
                | "mistral-code-latest"
                | "codestral-latest"
                | "codestral"
        ),
        ProviderKind::Arcee => model_lower == "trinity-large-thinking",
        // Every Muse tier owns a hand-written row the same way 1.1 does (see
        // `pricing_for_model_at`), so `DEFAULT_META_MODEL` (1.3) always has a
        // provider-owned fallback row.
        ProviderKind::Meta => matches!(
            model_lower.as_str(),
            "muse-spark-1.1"
                | "muse-spark-1.2"
                | "muse-spark-1.2-contributor"
                | "muse-spark-1.3"
                | "muse-spark-1.3-contributor"
                | "muse-wonder-1.0"
        ),
        // Deployment-style ids (Fireworks account prefix, OpenCode Zen
        // gateway) have no Models.dev cost fields. When the live control
        // plane 503s, these bundled family rates keep the session priced
        // instead of `unverified_live_pricing` forever (#5241).
        ProviderKind::Fireworks => {
            let bare = model_lower
                .strip_prefix("accounts/fireworks/models/")
                .unwrap_or(model_lower.as_str());
            matches!(bare, "deepseek-v4-flash" | "deepseek-v4-pro")
        }
        ProviderKind::OpencodeZen => {
            matches!(
                model_lower.as_str(),
                "deepseek-v4-flash" | "deepseek-v4-pro"
            )
        }
        _ => false,
    };
    let lookup = if provider == ProviderKind::Fireworks {
        model_lower
            .strip_prefix("accounts/fireworks/models/")
            .unwrap_or(model_lower.as_str())
            .to_string()
    } else {
        model_lower
    };
    provider_owns_row
        .then(|| pricing_for_model_at(&lookup, recorded_at))
        .flatten()
}

/// Fireworks serverless Standard rates (2026-08-15 audit).
/// <https://docs.fireworks.ai/serverless/pricing>
///
/// Cache-write is unpublished on that table. Do not inherit first-party
/// DeepSeek's $0.0028 cache-hit card — Fireworks publishes $0.028.
fn fireworks_bundled_fallback_pricing(model_lower: &str) -> Option<ModelPricing> {
    match fireworks_deployment_id(model_lower) {
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-0731" => {
            Some(hosted_deepseek_v4_flash_standard_pricing())
        }
        "deepseek-v4-pro" => Some(hosted_deepseek_v4_pro_standard_pricing()),
        // kimi-k3 stays unpriced until Fireworks publishes a rate for it
        // (see `fireworks_and_zen_flash_use_bundled_family_rates`).
        _ => None,
    }
}

fn fireworks_deployment_id(model_lower: &str) -> &str {
    model_lower
        .strip_prefix("accounts/fireworks/models/")
        .or_else(|| model_lower.strip_prefix("accounts/fireworks/routers/"))
        .unwrap_or(model_lower)
}

/// OpenCode Zen PAYG rates (2026-08-15 audit).
/// <https://opencode.ai/docs/zen/>
///
/// Cached write is unpublished (`-` on the Zen table). Flash cache-read is
/// $0.028, not first-party DeepSeek's $0.0028.
fn opencode_zen_bundled_fallback_pricing(model_lower: &str) -> Option<ModelPricing> {
    match model_lower {
        "deepseek-v4-flash" | "deepseek-v4-flash-0731" => {
            Some(hosted_deepseek_v4_flash_standard_pricing())
        }
        _ => None,
    }
}

fn hosted_deepseek_v4_flash_standard_pricing() -> ModelPricing {
    usd_only_pricing(0.028, 0.14, 0.28)
}

fn hosted_deepseek_v4_pro_standard_pricing() -> ModelPricing {
    usd_only_pricing(0.145, 1.74, 3.48)
}

/// The offering's pricing row as it actually applies to this route.
///
/// Two documented first-party routes publish no separate cache rate *because*
/// cache tokens are billed at the plain input rate; that substitution happens
/// here so cost estimation and the unpriced-class audit read the same row.
fn effective_offering_pricing(
    provider: ProviderKind,
    model: &str,
    offering: &codewhale_config::catalog::CatalogOffering,
    classes: &TokenUsage,
) -> Option<OfferingPricing> {
    let mut pricing = OfferingPricing::from_catalog_offering(offering)?;
    let model_lower = model.trim().to_ascii_lowercase();
    let cache_uses_input_rate = matches!(
        (provider, model_lower.as_str()),
        (ProviderKind::Openai, "gpt-5.5-pro") | (ProviderKind::Arcee, "trinity-large-thinking")
    );
    if cache_uses_input_rate {
        if classes.cache_read > 0 && pricing.cache_read_per_million.is_none() {
            pricing.cache_read_per_million = pricing.input_per_million;
        }
        if classes.cache_write > 0 && pricing.cache_write_per_million.is_none() {
            pricing.cache_write_per_million = pricing.input_per_million;
        }
        for tier in &mut pricing.context_tiers {
            if classes.cache_read > 0 && tier.cost.cache_read.is_none() {
                tier.cost.cache_read = tier.cost.input;
            }
            if classes.cache_write > 0 && tier.cost.cache_write.is_none() {
                tier.cost.cache_write = tier.cost.input;
            }
        }
    }
    Some(pricing)
}

/// Estimate usage only from the exact provider offering. Missing prices for a
/// used token class fail closed, except on the two documented first-party
/// routes where cache tokens are explicitly billed at the input rate.
#[cfg(test)]
fn catalog_cost_estimate_for_route(
    provider: ProviderKind,
    model: &str,
    offering: &codewhale_config::catalog::CatalogOffering,
    usage: &Usage,
) -> Option<CostEstimate> {
    let classes = token_usage_for_pricing(usage);
    let pricing = effective_offering_pricing(provider, model, offering, &classes)?;

    let amount = pricing.estimate_cost(&classes)?;
    match pricing.currency {
        Currency::Usd => Some(CostEstimate::usd_only(amount)),
        Currency::Cny => Some(CostEstimate {
            usd: 0.0,
            cny: amount,
        }),
        Currency::Other(_) => None,
    }
}

/// Project provider-normalized turn usage into canonical billable token
/// classes for the shared config pricing layer (#2961 / #4318).
///
/// `Usage::prompt_cache_miss_tokens` is billed as ordinary non-cached input.
/// `Usage::prompt_cache_write_tokens` maps to `TokenUsage::cache_write` so
/// providers that publish a write premium (Anthropic 1.25x–2x) are not
/// undercounted.
///
/// `Usage::reasoning_tokens` is deliberately **not** added to the billable
/// output. Every provider CodeWhale normalizes reports reasoning as a *subset*
/// of the completion count it already bills — OpenAI Responses nests
/// `reasoning_tokens` under `output_tokens_details` while `output_tokens` is
/// the total, and Chat Completions nests it under `completion_tokens_details`
/// while `completion_tokens` is the total. Adding it charged reasoning turns
/// twice for the same tokens (up to 2x on reasoning-heavy turns). It uses
/// its own class when a rate is published, replacing that subset of output.
#[must_use]
pub fn token_usage_for_pricing(usage: &Usage) -> TokenUsage {
    let mut classes = request_token_usage_for_pricing(usage);
    classes.request_usages = usage
        .request_usages
        .iter()
        .take(1025)
        .map(|request| {
            let mut classes = request_token_usage_for_pricing(request);
            if !request.request_usages.is_empty() {
                classes
                    .request_usages
                    .push(request_token_usage_for_pricing(request));
            }
            classes
        })
        .collect();
    classes
}

fn request_token_usage_for_pricing(usage: &Usage) -> TokenUsage {
    // `input_tokens` is the authoritative total. Even malformed provider
    // telemetry must never produce token classes whose sum exceeds it. The
    // audit path rejects contradictory partitions; this bounded projection
    // keeps token-only displays truthful while retaining deterministic class
    // priority (read, write, then miss/unclassified input).
    let total_input = usage.input_tokens;
    let cache_read = usage.prompt_cache_hit_tokens.unwrap_or(0).min(total_input);
    let after_read = total_input.saturating_sub(cache_read);
    let cache_write = usage.prompt_cache_write_tokens.unwrap_or(0).min(after_read);
    let after_write = after_read.saturating_sub(cache_write);
    let non_cached_reported = usage
        .prompt_cache_miss_tokens
        .unwrap_or(after_write)
        .min(after_write);
    let uncategorized_input = after_write.saturating_sub(non_cached_reported);
    let input = non_cached_reported.saturating_add(uncategorized_input);
    // Reasoning tokens are already inside `output_tokens`; see the doc comment.
    let output = usage.output_tokens;
    // Subset telemetry is clamped like the cache partition above: malformed
    // provider counts that exceed their parent total must never fail the
    // whole estimate closed or double-count.
    let reasoning = usage
        .reasoning_tokens
        .map(|tokens| u64::from(tokens.min(output)));
    let output_audio = usage.output_audio_tokens.map(|tokens| {
        let remaining = u64::from(output).saturating_sub(reasoning.unwrap_or(0));
        u64::from(tokens).min(remaining)
    });
    let input_audio = usage
        .input_audio_tokens
        .map(|tokens| u64::from(tokens).min(u64::from(input)));

    TokenUsage {
        input: u64::from(input),
        output: u64::from(output),
        cache_read: u64::from(cache_read),
        cache_write: u64::from(cache_write),

        reasoning,
        input_audio,
        output_audio,

        request_usages: Vec::new(),
    }
}

fn usage_cache_partition_is_consistent(usage: &Usage) -> bool {
    let reported = u64::from(usage.prompt_cache_hit_tokens.unwrap_or(0))
        + u64::from(usage.prompt_cache_miss_tokens.unwrap_or(0))
        + u64::from(usage.prompt_cache_write_tokens.unwrap_or(0));
    reported <= u64::from(usage.input_tokens)
}

fn currency_offering_pricing(pricing: CurrencyPricing, currency: Currency) -> OfferingPricing {
    OfferingPricing {
        currency,
        input_per_million: Some(pricing.input_cache_miss_per_million),
        output_per_million: Some(pricing.output_per_million),
        cache_read_per_million: Some(pricing.input_cache_hit_per_million),
        cache_write_per_million: pricing
            .cache_write
            .rate(pricing.input_cache_miss_per_million),
        provenance: PricingProvenance::ProviderDocs,
        ..Default::default()
    }
}

/// Estimate how much money was saved by serving `cache_hit_tokens` from the
/// prefix cache instead of billing them at the cache-miss rate.  Returns `None`
/// when the model's pricing is unknown or the number of cache-hit tokens is
/// zero (nothing to save).
#[must_use]
#[cfg(test)]
pub fn calculate_cache_savings(model: &str, cache_hit_tokens: u32) -> Option<CostEstimate> {
    if cache_hit_tokens == 0 {
        return None;
    }
    // M3's cache-read savings depend on whether total input crosses 512k;
    // this helper receives only cache-hit tokens, so an estimate would guess
    // the tier. The full turn-cost path has total input and remains precise.
    if is_minimax_m3(model) {
        return None;
    }
    let pricing = pricing_for_model(model)?;
    let tokens = cache_hit_tokens as f64 / 1_000_000.0;
    Some(CostEstimate {
        usd: tokens
            * (pricing.usd.input_cache_miss_per_million - pricing.usd.input_cache_hit_per_million),
        cny: pricing
            .cny
            .map(|pricing| {
                tokens
                    * (pricing.input_cache_miss_per_million - pricing.input_cache_hit_per_million)
            })
            .unwrap_or(0.0),
    })
}

/// The route's list price per million tokens, `in $X · out $Y`, when this
/// provider/model pair has authoritative pricing without endpoint
/// provenance. `None` otherwise — the price view omits the row rather than
/// quoting a rate the session is not actually billed at.
#[must_use]
pub(crate) fn model_rate_label(
    provider: ProviderKind,
    model: &str,
    currency: CostCurrency,
) -> Option<String> {
    if !has_pricing_for_provider(provider, model) {
        return None;
    }
    let pricing = pricing_for_model(model)?;
    let rates = match currency {
        CostCurrency::Usd => pricing.usd,
        CostCurrency::Cny => pricing.cny?,
    };
    Some(format!(
        "in {} · out {}",
        format_cost_amount(rates.input_cache_miss_per_million, currency),
        format_cost_amount(rates.output_per_million, currency),
    ))
}

/// Format a cost amount for compact display in the chosen currency.
#[must_use]
pub fn format_cost_amount(cost: f64, currency: CostCurrency) -> String {
    let symbol = currency.symbol();
    if cost == 0.0 {
        format!("{symbol}0.00")
    } else if cost > 0.0 && cost < 0.0001 {
        format!("<{symbol}0.0001")
    } else if cost < 0.01 {
        format!("{symbol}{cost:.4}")
    } else {
        format!("{symbol}{cost:.2}")
    }
}

/// Format a dual-currency estimate using the selected display currency.
#[must_use]
pub fn format_cost_estimate(estimate: CostEstimate, currency: CostCurrency) -> String {
    format_cost_amount(estimate.amount(currency), currency)
}

#[cfg(test)]
mod default_coverage_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn malformed_catalog_row_has_an_explicit_runtime_reason() {
        let offering = codewhale_config::catalog::CatalogOffering {
            provider: "openrouter".to_string(),
            wire_model_id: "openai/gpt-5.5".to_string(),
            cost: Some(codewhale_config::models_dev::ModelsDevCost {
                input: Some(f64::NAN),
                output: Some(30.0),
                cache_read: Some(0.05),
                cache_write: None,
                ..Default::default()
            }),
            ..Default::default()
        };

        let audit = refused_catalog_pricing_audit(&offering, &Usage::default())
            .expect("malformed row must become an explicit failed-closed audit");
        assert!(!audit.is_priced());
        assert_eq!(
            audit.unpriced_reason,
            Some(UnpricedReason::InvalidPricingRow)
        );
        assert_eq!(
            audit.unpriced_reason.unwrap().label(),
            "invalid_pricing_row"
        );
    }

    /// A hand-sourced row with **no published** cache-write rate must fail closed
    /// for a turn that wrote to cache, while a row whose provider *documents*
    /// that writes carry no separate charge prices it at the input rate.
    ///
    /// Both used to be `None` and both silently billed writes at the input rate,
    /// which invented a price for the first case (#4318).
    #[test]
    fn unpublished_cache_write_fails_closed_but_documented_same_rate_prices() {
        let write_heavy = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            prompt_cache_hit_tokens: Some(0),
            prompt_cache_miss_tokens: Some(900_000),
            prompt_cache_write_tokens: Some(100_000),
            ..Usage::default()
        };
        // Pinned off-peak (12:00 UTC) so the DeepSeek tier is deterministic.
        let now = Utc
            .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
            .single()
            .unwrap();

        // DeepSeek documents that a cache miss is billed once and cached for
        // free, so the miss rate *is* the published write rate. The policy
        // carries the documentation receipt rather than being an assumption.
        let deepseek = deepseek_v4_flash_pricing(now);
        assert_eq!(
            deepseek.usd.cache_write,
            CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE)
        );
        let priced = audit_turn_cost_for_provider_at(
            ProviderKind::Deepseek,
            "deepseek-v4-flash",
            &write_heavy,
            now,
        );
        assert!(priced.is_priced(), "{priced:?}");
        // 900k miss + 100k write, both at the off-peak 0.22/M miss rate.
        let expected = (0.9 + 0.1) * 0.22;
        assert!(
            (priced.estimate.expect("priced").usd - expected).abs() < 1e-12,
            "{priced:?}"
        );

        // StepFun's hand row publishes input/cache-read/output only. A write
        // turn is unpriced and names the class instead of borrowing the input
        // rate.
        let stepfun = pricing_for_billing_surface(
            ProviderKind::Stepfun,
            "step-3.7-flash",
            Some(STEPFUN_PAYG_BILLING_SURFACE),
        )
        .expect("StepFun PAYG row");
        assert_eq!(stepfun.usd.cache_write, CacheWritePolicy::Unpublished);
        let failed = audit_turn_cost_for_route_at(
            ProviderKind::Stepfun,
            "step-3.7-flash",
            Some(STEPFUN_PAYG_BILLING_SURFACE),
            &write_heavy,
            now,
        );
        assert!(!failed.is_priced(), "{failed:?}");
        assert_eq!(
            failed.unpriced_reason,
            Some(UnpricedReason::MissingClassPrice)
        );
        assert_eq!(failed.unpriced_classes, vec![TokenClass::CacheWrite]);

        // The same route with no cache-write tokens prices normally, proving the
        // gap is class-scoped rather than route-scoped.
        let no_write = Usage {
            prompt_cache_write_tokens: None,
            ..write_heavy.clone()
        };
        assert!(
            audit_turn_cost_for_route_at(
                ProviderKind::Stepfun,
                "step-3.7-flash",
                Some(STEPFUN_PAYG_BILLING_SURFACE),
                &no_write,
                now,
            )
            .is_priced()
        );
    }

    /// Every exact billing surface a route can carry must be understood, and
    /// anything unrecognized must fail closed as unknown rather than defaulting
    /// into per-token dollars (#4318).
    #[test]
    fn official_chatgpt_api_requires_the_exact_secure_base() {
        for endpoint in ["https://api.openai.com/v1", "https://api.openai.com/v1/"] {
            assert!(is_official_chatgpt_api(endpoint), "{endpoint}");
        }
        for endpoint in [
            "http://api.openai.com/v1",
            "https://api.openai.com:444/v1",
            "https://api.openai.com.example.net/v1",
            "https://api.openai.com/v1/responses",
            "https://api.openai.com/v1?route=plan",
            "https://api.openai.com/v1#plan",
            "https://user:secret@api.openai.com/v1",
            "https://chatgpt.com/backend-api",
            "",
        ] {
            assert!(!is_official_chatgpt_api(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn public_chatgpt_endpoint_does_not_prove_a_plan_without_grant_provenance() {
        assert_eq!(
            billing_surface_for_route(ProviderKind::OpenaiCodex, Some("https://api.openai.com/v1")),
            Some(UNCLASSIFIED_BILLING_SURFACE)
        );
        assert_eq!(
            billing_surface_for_route(ProviderKind::Openai, Some("https://api.openai.com/v1")),
            Some(FIRST_PARTY_PAYG_BILLING_SURFACE)
        );
        assert_eq!(
            billing_surface_for_route(
                ProviderKind::OpenaiCodex,
                Some("https://chatgpt.com/backend-api/codex")
            ),
            Some(OAUTH_SUBSCRIPTION_BILLING_SURFACE)
        );
    }

    #[test]
    fn endpoint_classification_covers_every_exact_billing_surface() {
        for (provider, base_url, expected_surface, expected_metering) in [
            (
                ProviderKind::Zai,
                "https://api.z.ai/api/coding/paas/v4",
                ZAI_CODING_PLAN_BILLING_SURFACE,
                EndpointMetering::ExactSubscription,
            ),
            (
                ProviderKind::Zai,
                "https://api.z.ai/api/paas/v4",
                ZAI_PAYG_BILLING_SURFACE,
                EndpointMetering::Money,
            ),
            (
                ProviderKind::Moonshot,
                codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL,
                MOONSHOT_KIMI_CODE_BILLING_SURFACE,
                EndpointMetering::ExactSubscription,
            ),
            (
                ProviderKind::Moonshot,
                "https://api.moonshot.ai/v1",
                MOONSHOT_PAYG_BILLING_SURFACE,
                EndpointMetering::Money,
            ),
            (
                ProviderKind::XiaomiMimo,
                codewhale_runtime::config::XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL,
                XIAOMI_PAYG_BILLING_SURFACE,
                EndpointMetering::Money,
            ),
            (
                ProviderKind::XiaomiMimo,
                codewhale_config::descriptors::defaults::DEFAULT_XIAOMI_MIMO_BASE_URL,
                XIAOMI_TOKEN_PLAN_BILLING_SURFACE,
                EndpointMetering::ExactSubscription,
            ),
            (
                ProviderKind::Stepfun,
                "https://api.stepfun.ai/step_plan/v1",
                STEPFUN_PLAN_BILLING_SURFACE,
                EndpointMetering::ExactSubscription,
            ),
            (
                ProviderKind::Stepfun,
                "https://api.stepfun.ai/v1",
                STEPFUN_PAYG_BILLING_SURFACE,
                EndpointMetering::Money,
            ),
            (
                ProviderKind::Anthropic,
                "https://api.anthropic.com/v1",
                FIRST_PARTY_PAYG_BILLING_SURFACE,
                EndpointMetering::Money,
            ),
            (
                ProviderKind::Openrouter,
                "https://openrouter.ai/api/v1",
                AGGREGATOR_BILLING_SURFACE,
                EndpointMetering::Money,
            ),
            (
                ProviderKind::Orcarouter,
                "https://api.orcarouter.ai/v1",
                AGGREGATOR_BILLING_SURFACE,
                EndpointMetering::Money,
            ),
        ] {
            let surface = billing_surface_for_route(provider, Some(base_url));
            assert_eq!(surface, Some(expected_surface), "{provider:?} {base_url}");
            assert_eq!(
                endpoint_metering_for_billing_surface(surface),
                expected_metering,
                "{provider:?} {base_url}"
            );
        }

        // Provider-intrinsic surfaces need no URL at all.
        for (provider, expected_surface, expected_metering) in [
            (
                ProviderKind::OpenaiCodex,
                UNCLASSIFIED_BILLING_SURFACE,
                EndpointMetering::Unknown,
            ),
            (
                ProviderKind::OpencodeGo,
                OAUTH_SUBSCRIPTION_BILLING_SURFACE,
                EndpointMetering::ExactSubscription,
            ),
            (
                ProviderKind::Ollama,
                LOCAL_BILLING_SURFACE,
                EndpointMetering::LocalNoBill,
            ),
            (
                ProviderKind::OllamaCloud,
                UNCLASSIFIED_BILLING_SURFACE,
                EndpointMetering::Unknown,
            ),
            (
                ProviderKind::Vllm,
                LOCAL_BILLING_SURFACE,
                EndpointMetering::LocalNoBill,
            ),
            // A named custom endpoint's pay mode is config, not URL shape.
            (
                ProviderKind::Custom,
                UNCLASSIFIED_BILLING_SURFACE,
                EndpointMetering::Unknown,
            ),
        ] {
            let surface = billing_surface_for_route(provider, None);
            assert_eq!(surface, Some(expected_surface), "{provider:?}");
            assert_eq!(
                endpoint_metering_for_billing_surface(surface),
                expected_metering,
                "{provider:?}"
            );
        }

        // An unrecognized surface id — including one a newer build might write —
        // is never guessed into a known bucket.
        for unknown in [
            Some("some-future-surface"),
            Some(""),
            Some("   "),
            Some(UNCLASSIFIED_BILLING_SURFACE),
            None,
        ] {
            assert_eq!(
                endpoint_metering_for_billing_surface(unknown),
                EndpointMetering::Unknown,
                "{unknown:?}"
            );
        }
    }

    /// An endpoint that was never established is not the official endpoint.
    ///
    /// The route audit used to fall through to the provider/model catalog when
    /// no billing surface was supplied, which meant a persisted or recorded row
    /// carrying nothing but `provider: "openai"` and a familiar model id got
    /// billed at OpenAI's published first-party rates — even though the turn
    /// could equally have been served by a proxy, a gateway, or a self-hosted
    /// clone speaking the same protocol. Absence of endpoint evidence is not
    /// evidence of the official endpoint.
    #[test]
    fn an_unestablished_endpoint_is_never_priced_as_the_official_one() {
        let usage = Usage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            ..Usage::default()
        };
        let now = Utc::now();
        for (provider, model) in [
            (ProviderKind::Openai, "gpt-5.5"),
            (ProviderKind::Anthropic, "claude-haiku-4-5"),
            (ProviderKind::Deepseek, "deepseek-v4-flash"),
            (ProviderKind::Openrouter, "openai/gpt-5.5"),
            (ProviderKind::Moonshot, "kimi-k2.7-code"),
        ] {
            let audit = audit_turn_cost_for_route_at(provider, model, None, &usage, now);
            assert_eq!(
                audit.unpriced_reason,
                Some(UnpricedReason::UnestablishedEndpoint),
                "{provider:?}/{model}: {audit:?}"
            );
            assert!(!audit.is_priced(), "{provider:?}/{model}: {audit:?}");
            assert_eq!(audit.estimate, None, "{provider:?}/{model}");
            // An unknown route is still possibly-spent money, so it stays in
            // the coverage denominator rather than being excused like an OAuth
            // or local route.
            assert!(
                audit.counts_toward_money_coverage(),
                "{provider:?}/{model}: an unknown route must not leave money coverage"
            );

            // The same route with its endpoint actually classified prices
            // normally: this is a fail-closed rule, not a refusal to price.
            // (OpenRouter is excluded here only because its aggregator surface
            // carries no bundled rate at all, which is a different gap.)
            if provider == ProviderKind::Openrouter {
                continue;
            }
            let classified = audit_turn_cost_for_route_at(
                provider,
                model,
                billing_surface_for_route(provider, Some(provider.provider().default_base_url())),
                &usage,
                now,
            );
            assert!(
                classified.is_priced(),
                "{provider:?}/{model} must price on its own official endpoint: {classified:?}"
            );
        }

        // The distinction is preserved end to end: "no endpoint offered" and
        // "endpoint offered but unplaceable" are different findings, and
        // neither is a price.
        let unplaceable = audit_turn_cost_for_route_at(
            ProviderKind::Openai,
            "gpt-5.5",
            billing_surface_for_route(ProviderKind::Openai, Some("https://proxy.example/v1")),
            &usage,
            now,
        );
        assert_eq!(
            unplaceable.unpriced_reason,
            Some(UnpricedReason::UnknownBillingBasis)
        );
    }

    #[test]
    fn builtin_provider_names_do_not_price_unofficial_proxy_endpoints() {
        let usage = Usage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            ..Usage::default()
        };
        for (provider, model) in [
            (ProviderKind::Deepseek, "deepseek-v4-flash"),
            (ProviderKind::Openai, "gpt-5.5"),
            (ProviderKind::Anthropic, "claude-haiku-4-5"),
            (ProviderKind::Openrouter, "openai/gpt-5.5"),
        ] {
            let surface = billing_surface_for_route(provider, Some("https://proxy.example/v1"));
            assert_eq!(surface, Some(UNCLASSIFIED_BILLING_SURFACE), "{provider:?}");
            let audit = audit_turn_cost_for_route_at(provider, model, surface, &usage, Utc::now());
            assert_eq!(
                audit.unpriced_reason,
                Some(UnpricedReason::UnknownBillingBasis),
                "{provider:?}: {audit:?}"
            );
            assert!(!audit.is_priced(), "{provider:?}: {audit:?}");
        }

        assert_eq!(
            billing_surface_for_route(
                ProviderKind::Moonshot,
                Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL)
            ),
            Some(MOONSHOT_KIMI_CODE_BILLING_SURFACE)
        );
        for (provider, endpoint) in [
            (ProviderKind::Minimax, "https://api.minimax.io/v1"),
            (
                ProviderKind::MinimaxAnthropic,
                "https://api.minimax.io/anthropic",
            ),
            (
                ProviderKind::Minimax,
                "https://api.minimax.io/v1/token-plan",
            ),
            (
                ProviderKind::XiaomiMimo,
                "https://token-plan-proxy.example/v1",
            ),
            (
                ProviderKind::Zai,
                "https://api.z.ai/api/coding/something-else",
            ),
        ] {
            assert_eq!(
                billing_surface_for_route(provider, Some(endpoint)),
                Some(UNCLASSIFIED_BILLING_SURFACE),
                "{provider:?} {endpoint}"
            );
        }
    }

    /// A route classified as an exact subscription surface is not money-metered
    /// even when the provider-level presentation guessed "metered", and it must
    /// never reach a per-token rate.
    #[test]
    fn exact_plan_surface_overrides_a_metered_presentation() {
        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 10_000,
            ..Usage::default()
        };
        let audit = audit_turn_cost_for_route(
            ProviderKind::Zai,
            "glm-5.2",
            Some(ZAI_CODING_PLAN_BILLING_SURFACE),
            &usage,
            Utc::now(),
            crate::route_billing::BillingPresentation::Metered,
        );
        assert!(!audit.is_priced(), "{audit:?}");
        assert_eq!(audit.unpriced_reason, Some(UnpricedReason::NotMoneyMetered));
        assert!(!audit.counts_toward_money_coverage());

        // The same model on the per-token surface is money-metered, so it stays
        // in the coverage denominator whether or not a price is found.
        let payg = audit_turn_cost_for_route(
            ProviderKind::Zai,
            "glm-5.2",
            Some(ZAI_PAYG_BILLING_SURFACE),
            &usage,
            Utc::now(),
            crate::route_billing::BillingPresentation::Metered,
        );
        assert!(payg.counts_toward_money_coverage(), "{payg:?}");
    }

    /// An unknown billing basis is *not* a subscription. It stays unpriced and
    /// stays inside the money-coverage denominator, so its spend is reported as
    /// missing rather than excused (#4318).
    #[test]
    fn unknown_billing_basis_is_not_excused_as_not_money_metered() {
        let usage = Usage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            ..Usage::default()
        };
        let unknown = audit_turn_cost_for_route(
            ProviderKind::Anthropic,
            "claude-haiku-4-5",
            None,
            &usage,
            Utc::now(),
            crate::route_billing::BillingPresentation::Unknown,
        );
        assert!(!unknown.is_priced());
        assert_eq!(
            unknown.unpriced_reason,
            Some(UnpricedReason::UnknownBillingBasis)
        );
        assert!(unknown.counts_toward_money_coverage());

        // Local and subscription presentations are exact, so they *are* excused.
        for billing in [
            crate::route_billing::BillingPresentation::Local,
            crate::route_billing::BillingPresentation::Subscription("plan"),
        ] {
            let audit = audit_turn_cost_for_route(
                ProviderKind::Anthropic,
                "claude-haiku-4-5",
                None,
                &usage,
                Utc::now(),
                billing,
            );
            assert_eq!(audit.unpriced_reason, Some(UnpricedReason::NotMoneyMetered));
            assert!(!audit.counts_toward_money_coverage());
        }
    }

    #[test]
    fn audit_names_why_a_turn_is_missing_from_a_total() {
        let write_heavy = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            prompt_cache_hit_tokens: Some(200_000),
            prompt_cache_write_tokens: Some(100_000),
            ..Usage::default()
        };

        // Anthropic publishes a cache-write rate: fully priced, provenance kept.
        let priced = audit_turn_cost_for_provider_at(
            ProviderKind::Anthropic,
            "claude-haiku-4-5",
            &write_heavy,
            Utc::now(),
        );
        assert!(priced.is_priced());
        assert_eq!(priced.unpriced_reason, None);
        assert!(priced.unpriced_classes.is_empty());
        assert!(priced.provenance.is_some());

        // Moonshot does not: the turn fails closed and names the class.
        let missing = audit_turn_cost_for_provider_at(
            ProviderKind::Moonshot,
            "kimi-k2.7-code",
            &write_heavy,
            Utc::now(),
        );
        assert!(!missing.is_priced());
        assert_eq!(
            missing.unpriced_reason,
            Some(UnpricedReason::MissingClassPrice)
        );
        assert_eq!(missing.unpriced_classes, vec![TokenClass::CacheWrite]);
        // Dropping the write tokens makes the very same route priceable, which
        // proves the gap is class-scoped rather than route-scoped.
        let no_write = Usage {
            prompt_cache_write_tokens: None,
            ..write_heavy.clone()
        };
        assert!(
            audit_turn_cost_for_provider_at(
                ProviderKind::Moonshot,
                "kimi-k2.7-code",
                &no_write,
                Utc::now(),
            )
            .is_priced()
        );

        // Subscription/OAuth and ambiguous-surface routes report their own
        // reasons rather than an absent price.
        assert_eq!(
            audit_turn_cost_for_provider_at(
                ProviderKind::OpenaiCodex,
                "gpt-5.5",
                &write_heavy,
                Utc::now(),
            )
            .unpriced_reason,
            Some(UnpricedReason::AmbiguousBillingSurface)
        );
        assert_eq!(
            audit_turn_cost_for_route_at(
                ProviderKind::Stepfun,
                DEFAULT_STEPFUN_MODEL,
                None,
                &write_heavy,
                Utc::now(),
            )
            .unpriced_reason,
            Some(UnpricedReason::AmbiguousBillingSurface)
        );
        assert_eq!(
            audit_turn_cost_for_provider_at(
                ProviderKind::Openai,
                "gpt-5.5",
                &Usage {
                    input_tokens: OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1,
                    ..Usage::default()
                },
                Utc::now(),
            )
            .unpriced_reason,
            Some(UnpricedReason::UnrepresentedTier)
        );
    }

    /// The audit and the estimator are the same computation, so every route
    /// must agree on whether it produced a number.
    #[test]
    fn audit_and_estimate_never_disagree() {
        let usage = Usage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            prompt_cache_hit_tokens: Some(2_000),
            prompt_cache_write_tokens: Some(1_000),
            ..Usage::default()
        };
        let now = Utc::now();
        for (provider, model) in [
            (ProviderKind::Anthropic, "claude-haiku-4-5"),
            (ProviderKind::Anthropic, "claude-sonnet-5"),
            (ProviderKind::Moonshot, "kimi-k2.7-code"),
            (ProviderKind::Openai, "gpt-5.5"),
            (ProviderKind::OpenaiCodex, "gpt-5.5"),
            (ProviderKind::Deepseek, "deepseek-v4-pro"),
            (ProviderKind::Ollama, "gpt-5.5"),
            (ProviderKind::Stepfun, DEFAULT_STEPFUN_MODEL),
        ] {
            let audit = audit_turn_cost_for_route_at(provider, model, None, &usage, now);
            let estimate =
                calculate_turn_cost_estimate_for_route_at(provider, model, None, &usage, now);
            assert_eq!(audit.estimate, estimate, "{provider:?}/{model}");
            assert_eq!(
                audit.is_priced(),
                audit.unpriced_reason.is_none(),
                "{provider:?}/{model}"
            );
        }
    }

    #[test]
    fn nvidia_nim_deepseek_model_does_not_use_deepseek_platform_pricing() {
        assert!(!has_pricing_for_model("deepseek-ai/deepseek-v4-pro"));
    }

    #[test]
    fn stepfun_current_model_rates_require_payg_provenance() {
        for (model, cache, input, output) in [
            ("step-5-preview", 0.05, 1.00, 2.70),
            ("step-3.7-flash", 0.04, 0.20, 1.15),
            ("step-3.5-flash", 0.02, 0.10, 0.30),
            ("step-3.5-flash-2603", 0.02, 0.10, 0.30),
        ] {
            let price = pricing_for_billing_surface(
                ProviderKind::Stepfun,
                model,
                Some(STEPFUN_PAYG_BILLING_SURFACE),
            )
            .unwrap();
            assert_eq!(price.usd.input_cache_hit_per_million, cache);
            assert_eq!(price.usd.input_cache_miss_per_million, input);
            assert_eq!(price.usd.output_per_million, output);
            assert!(
                pricing_for_billing_surface(
                    ProviderKind::Stepfun,
                    model,
                    Some(STEPFUN_PLAN_BILLING_SURFACE)
                )
                .is_none()
            );
            assert!(route_requires_billing_surface(ProviderKind::Custom, model));
            assert!(!has_pricing_for_provider(ProviderKind::Stepfun, model));
        }
    }

    #[test]
    fn stepfun_billing_surface_keeps_payg_separate_from_step_plan() {
        for base_url in [
            "https://api.stepfun.ai",
            "https://api.stepfun.ai/",
            "https://api.stepfun.ai/v1",
            "https://API.STEPFUN.AI/v1/",
        ] {
            assert_eq!(
                billing_surface_for_route(ProviderKind::Stepfun, Some(base_url)),
                Some(STEPFUN_PAYG_BILLING_SURFACE),
                "{base_url}"
            );
        }
        for base_url in [
            "https://api.stepfun.ai/step_plan",
            "https://api.stepfun.ai/step_plan/v1/",
            "https://api.stepfun.com/step_plan/v1",
        ] {
            assert_eq!(
                billing_surface_for_route(ProviderKind::Stepfun, Some(base_url)),
                Some(STEPFUN_PLAN_BILLING_SURFACE),
                "{base_url}"
            );
        }
        // Endpoints CodeWhale cannot place now classify *positively* as
        // unclassified rather than returning `None`. Both fail closed
        // identically, but "we looked and could not place this" is a different
        // fact from "no endpoint was supplied", and the audit reports it as
        // such (#4318).
        for base_url in [
            "http://api.stepfun.ai/v1",
            "https://token@api.stepfun.ai/v1",
            "https://api.stepfun.ai/v1?account=other",
            "https://api.stepfun.ai/STEP_PLAN/v1",
            "https://stepfun.example/v1",
        ] {
            assert_eq!(
                billing_surface_for_route(ProviderKind::Stepfun, Some(base_url)),
                Some(UNCLASSIFIED_BILLING_SURFACE),
                "{base_url}"
            );
            assert_eq!(
                endpoint_metering_for_billing_surface(Some(UNCLASSIFIED_BILLING_SURFACE)),
                EndpointMetering::Unknown
            );
        }
        // A StepFun URL paired with the OpenRouter protocol is a foreign custom
        // endpoint, not proof of either provider's billing surface.
        assert_eq!(
            billing_surface_for_route(ProviderKind::Openrouter, Some(DEFAULT_STEPFUN_BASE_URL)),
            Some(UNCLASSIFIED_BILLING_SURFACE)
        );
        // No endpoint at all stays `None`.
        assert_eq!(
            billing_surface_for_route(ProviderKind::Stepfun, None),
            None,
            "an absent endpoint is not a classification"
        );

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            prompt_cache_hit_tokens: Some(250_000),
            ..Default::default()
        };
        let payg = calculate_turn_cost_estimate_for_billing_surface(
            ProviderKind::Stepfun,
            "step-3.7-flash",
            Some(STEPFUN_PAYG_BILLING_SURFACE),
            &usage,
        )
        .expect("standard StepFun API has an authoritative token price");
        assert!((payg.usd - 0.735).abs() < 1e-12);
        assert_eq!(payg.cny, 0.0);

        // Provider/model-only legacy callers cannot distinguish PAYG from Step
        // Plan and must not add either route to spend or savings totals.
        assert!(
            calculate_turn_cost_estimate_for_provider(
                ProviderKind::Stepfun,
                "step-3.7-flash",
                &usage,
            )
            .is_none()
        );
        assert!(
            calculate_turn_cost_estimate_for_provider_at(
                ProviderKind::Stepfun,
                "step-3.7-flash",
                &usage,
                Utc::now(),
            )
            .is_none()
        );
        assert!(!has_pricing_for_provider(
            ProviderKind::Stepfun,
            "step-3.7-flash"
        ));

        for surface in [None, Some(STEPFUN_PLAN_BILLING_SURFACE)] {
            assert!(
                calculate_turn_cost_estimate_for_billing_surface(
                    ProviderKind::Stepfun,
                    "step-3.7-flash",
                    surface,
                    &usage,
                )
                .is_none()
            );
        }
        assert!(
            calculate_turn_cost_estimate_for_billing_surface(
                ProviderKind::Stepfun,
                "step-unknown",
                Some(STEPFUN_PAYG_BILLING_SURFACE),
                &usage,
            )
            .is_none()
        );
        for provider in [
            ProviderKind::Openrouter,
            ProviderKind::Ollama,
            ProviderKind::Custom,
        ] {
            assert!(
                calculate_turn_cost_estimate_for_billing_surface(
                    provider,
                    "step-3.7-flash",
                    Some(STEPFUN_PAYG_BILLING_SURFACE),
                    &usage,
                )
                .is_none(),
                "{provider:?}"
            );
            assert!(
                calculate_turn_cost_estimate_for_provider(provider, "step-3.7-flash", &usage,)
                    .is_none(),
                "{provider:?}"
            );
            assert!(
                calculate_turn_cost_estimate_for_provider_at(
                    provider,
                    "step-3.7-flash",
                    &usage,
                    Utc::now(),
                )
                .is_none(),
                "{provider:?}"
            );
            assert!(
                !has_pricing_for_provider(provider, "step-3.7-flash"),
                "{provider:?}"
            );
        }

        let recorded = calculate_turn_cost_estimate_for_route_at(
            ProviderKind::Stepfun,
            "step-3.7-flash",
            Some(STEPFUN_PAYG_BILLING_SURFACE),
            &usage,
            Utc::now(),
        )
        .expect("recorded PAYG route retains provider-scoped pricing");
        assert_eq!(recorded, payg);
    }

    #[test]
    fn catalog_sourced_models_have_usd_pricing() {
        for (model, input, output) in [
            ("minimax-m2.7", 0.3, 1.2),
            ("minimax/minimax-m2.7", 0.3, 1.2),
            ("step-3.7-flash", 0.2, 1.15),
            ("fugu-ultra-20260615", 5.0, 30.0),
            ("fugu-ultra", 5.0, 30.0),
        ] {
            let pricing = pricing_for_model_at(model, Utc::now()).expect(model);
            assert_eq!(pricing.usd.input_cache_miss_per_million, input, "{model}");
            assert_eq!(pricing.usd.output_per_million, output, "{model}");
            assert!(has_pricing_for_model(model));
        }
    }

    #[test]
    fn trinity_mini_stays_unpriced_without_verified_provider_rates() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            ..Usage::default()
        };

        assert!(pricing_for_model_at("trinity-mini", Utc::now()).is_none());
        assert!(!has_pricing_for_model("trinity-mini"));
        assert!(!has_pricing_for_provider(
            ProviderKind::Arcee,
            "trinity-mini"
        ));
        assert!(
            calculate_turn_cost_estimate_for_provider(ProviderKind::Arcee, "trinity-mini", &usage,)
                .is_none()
        );
    }

    #[test]
    fn minimax_m3_standard_pricing_tracks_the_512k_input_boundary() {
        for model in ["MiniMax-M3", "minimax/minimax-m3"] {
            for (input_tokens, cache_read, input, output) in
                [(512_000, 0.06, 0.30, 1.20), (512_001, 0.12, 0.60, 2.40)]
            {
                let usage = Usage {
                    input_tokens,
                    ..Usage::default()
                };
                let pricing = pricing_for_model_and_usage(model, &usage).expect("M3 pricing");
                assert_eq!(pricing.usd.input_cache_hit_per_million, cache_read);
                assert_eq!(pricing.usd.input_cache_miss_per_million, input);
                assert_eq!(pricing.usd.output_per_million, output);
            }
            assert!(calculate_cache_savings(model, 1).is_none());
        }
    }

    #[test]
    fn grok_46_pricing_tracks_the_200k_prompt_boundary() {
        for (input_tokens, cache_read, input, output) in
            [(199_999, 0.50, 2.00, 6.00), (200_000, 1.00, 4.00, 12.00)]
        {
            let usage = Usage {
                input_tokens,
                ..Usage::default()
            };
            let pricing =
                pricing_for_model_and_usage("grok-4.6", &usage).expect("Grok 4.6 pricing");
            assert_eq!(pricing.usd.input_cache_hit_per_million, cache_read);
            assert_eq!(pricing.usd.input_cache_miss_per_million, input);
            assert_eq!(pricing.usd.output_per_million, output);
        }
    }

    /// Published xAI rates per 1M tokens (cache-read, input, output) at the
    /// standard tier, verified 2026-08-17 on docs.x.ai/docs/models/grok-4.5
    /// and /grok-4.3; the pages' embedded price tables carry a `LongContext`
    /// column at exactly 2x for prompts past 200K.
    const GROK_4_5_USD_STANDARD: (f64, f64, f64) = (0.30, 2.00, 6.00);
    const GROK_4_5_USD_LONG_CONTEXT: (f64, f64, f64) = (0.60, 4.00, 12.00);
    const GROK_4_3_USD_STANDARD: (f64, f64, f64) = (0.20, 1.25, 2.50);
    const GROK_4_3_USD_LONG_CONTEXT: (f64, f64, f64) = (0.40, 2.50, 5.00);

    #[test]
    fn grok_45_and_43_pricing_track_the_200k_prompt_boundary() {
        for (model, standard, long_context) in [
            ("grok-4.5", GROK_4_5_USD_STANDARD, GROK_4_5_USD_LONG_CONTEXT),
            ("grok-4.3", GROK_4_3_USD_STANDARD, GROK_4_3_USD_LONG_CONTEXT),
        ] {
            for (input_tokens, expected) in [(199_999, standard), (200_000, long_context)] {
                let usage = Usage {
                    input_tokens,
                    ..Usage::default()
                };
                let pricing = pricing_for_model_and_usage(model, &usage)
                    .unwrap_or_else(|| panic!("{model} pricing"));
                assert_eq!(
                    pricing.usd.input_cache_hit_per_million, expected.0,
                    "{model} @ {input_tokens} cache-read"
                );
                assert_eq!(
                    pricing.usd.input_cache_miss_per_million, expected.1,
                    "{model} @ {input_tokens} input"
                );
                assert_eq!(
                    pricing.usd.output_per_million, expected.2,
                    "{model} @ {input_tokens} output"
                );
                assert!(pricing.cny.is_none());
            }
            // Metadata-only lookups report the standard tier.
            let metadata = pricing_for_model_at(model, Utc::now()).unwrap();
            assert_eq!(metadata.usd.input_cache_miss_per_million, standard.1);
        }
    }

    #[test]
    fn direct_xai_grok_45_and_43_own_usage_tier_without_leaking_to_other_providers() {
        for (model, standard_input, long_input) in
            [("grok-4.5", 2.00, 4.00), ("grok-4.3", 1.25, 2.50)]
        {
            for (input_tokens, input_rate) in [(199_999, standard_input), (200_000, long_input)] {
                let usage = Usage {
                    input_tokens,
                    ..Usage::default()
                };
                let estimate = calculate_turn_cost_estimate_for_provider_at(
                    ProviderKind::Xai,
                    model,
                    &usage,
                    Utc::now(),
                )
                .unwrap_or_else(|| panic!("direct xAI {model} has tiered pricing"));
                let expected = f64::from(input_tokens) / 1_000_000.0 * input_rate;
                assert!(
                    (estimate.usd - expected).abs() < 1e-12,
                    "{model} @ {input_tokens}: {} != {expected}",
                    estimate.usd
                );
            }
            assert!(
                provider_owned_hand_pricing_at(ProviderKind::Openrouter, model, Utc::now())
                    .is_none(),
                "{model}: OpenRouter must not inherit xAI billing"
            );
        }
    }

    #[test]
    fn direct_xai_grok_46_owns_usage_tier_without_leaking_to_other_providers() {
        for (input_tokens, input_rate) in [(199_999, 2.00), (200_000, 4.00)] {
            let usage = Usage {
                input_tokens,
                ..Usage::default()
            };
            let estimate = calculate_turn_cost_estimate_for_provider_at(
                ProviderKind::Xai,
                "grok-4.6",
                &usage,
                Utc::now(),
            )
            .expect("direct xAI route has authoritative tiered pricing");
            let expected = f64::from(input_tokens) / 1_000_000.0 * input_rate;
            assert!((estimate.usd - expected).abs() < 1e-12);
        }

        assert!(
            provider_owned_hand_pricing_at(ProviderKind::Openrouter, "grok-4.6", Utc::now(),)
                .is_none()
        );
    }

    #[test]
    fn provider_scoped_minimax_m3_keeps_usage_tiers_for_both_wire_protocols() {
        for provider in [ProviderKind::Minimax, ProviderKind::MinimaxAnthropic] {
            for (input_tokens, input_rate) in [(512_000, 0.30), (512_001, 0.60)] {
                let usage = Usage {
                    input_tokens,
                    ..Usage::default()
                };
                let estimate = calculate_turn_cost_estimate_for_provider_at(
                    provider,
                    "MiniMax-M3",
                    &usage,
                    Utc::now(),
                )
                .expect("direct MiniMax route has authoritative pricing");
                let expected = f64::from(input_tokens) / 1_000_000.0 * input_rate;
                assert!((estimate.usd - expected).abs() < 1e-12, "{provider:?}");
            }
        }
    }

    #[test]
    fn direct_openai_long_context_estimates_fail_closed_above_272k() {
        for model in [
            "gpt-5.5",
            "gpt-5.6",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ] {
            let at_boundary = Usage {
                input_tokens: OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD,
                ..Usage::default()
            };
            let above_boundary = Usage {
                input_tokens: OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1,
                ..Usage::default()
            };

            assert!(
                calculate_turn_cost_estimate_for_provider(
                    ProviderKind::Openai,
                    model,
                    &at_boundary,
                )
                .is_some(),
                "{model} should retain its standard price at 272K"
            );
            assert!(
                calculate_turn_cost_estimate_for_provider(
                    ProviderKind::Openai,
                    model,
                    &above_boundary,
                )
                .is_none(),
                "{model} must not report the lower static price above 272K"
            );
        }
    }

    #[test]
    fn direct_openai_gpt54_family_is_guarded_even_without_a_bundled_catalog_row() {
        for model in ["gpt-5.4", "gpt-5.4-pro"] {
            assert!(!direct_openai_long_context_tier_is_unpriced(
                ProviderKind::Openai,
                model,
                OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD,
            ));
            assert!(direct_openai_long_context_tier_is_unpriced(
                ProviderKind::Openai,
                model,
                OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1,
            ));

            let above_boundary = Usage {
                input_tokens: OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1,
                ..Usage::default()
            };
            assert!(
                calculate_turn_cost_estimate_for_provider(
                    ProviderKind::Openai,
                    model,
                    &above_boundary,
                )
                .is_none(),
                "{model} must remain unpriced if a live catalog row is available"
            );
        }
    }

    #[test]
    fn openai_long_context_guard_is_exact_and_provider_scoped() {
        let input_tokens = OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1;

        for provider in [
            ProviderKind::Openrouter,
            ProviderKind::OpenaiCodex,
            ProviderKind::Ollama,
            ProviderKind::Custom,
        ] {
            assert!(
                !direct_openai_long_context_tier_is_unpriced(provider, "gpt-5.5", input_tokens,),
                "{provider:?} must not inherit direct OpenAI tier handling"
            );
        }
        for model in [
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5.5-pro",
            "gpt-5.5-pro-2026-04-23",
            "gpt-5.5-2026-04-23-extra",
            "openai/gpt-5.5",
            "gpt-5.6-sol-preview",
        ] {
            assert!(
                !direct_openai_long_context_tier_is_unpriced(
                    ProviderKind::Openai,
                    model,
                    input_tokens,
                ),
                "non-documented id {model} must not be treated as an alias"
            );
        }

        let usage = Usage {
            input_tokens,
            output_tokens: 1,
            ..Usage::default()
        };
        assert!(calculate_turn_cost_estimate_from_usage("gpt-5.5", &usage).is_some());
        assert!(
            calculate_turn_cost_estimate_for_provider(ProviderKind::OpenaiCodex, "gpt-5.5", &usage,)
                .is_none()
        );
    }

    #[test]
    fn direct_openai_snapshots_use_the_same_strict_272k_boundary() {
        for snapshot in [
            "gpt-5.4-2026-03-05",
            "gpt-5.4-pro-2026-03-05",
            "gpt-5.5-2026-04-23",
        ] {
            assert!(!direct_openai_long_context_tier_is_unpriced(
                ProviderKind::Openai,
                snapshot,
                OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD,
            ));
            assert!(direct_openai_long_context_tier_is_unpriced(
                ProviderKind::Openai,
                snapshot,
                OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1,
            ));

            let above_boundary = Usage {
                input_tokens: OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1,
                ..Usage::default()
            };
            assert!(
                calculate_turn_cost_estimate_for_provider(
                    ProviderKind::Openai,
                    snapshot,
                    &above_boundary,
                )
                .is_none(),
                "{snapshot} must not report the lower static price above 272K"
            );
        }
    }

    #[test]
    fn direct_openai_long_context_guard_uses_total_input_with_mixed_cache_classes() {
        let at_boundary = Usage {
            input_tokens: OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD,
            output_tokens: 1_000,
            prompt_cache_hit_tokens: Some(100_000),
            prompt_cache_miss_tokens: Some(100_000),
            prompt_cache_write_tokens: Some(72_000),
            ..Usage::default()
        };
        let above_boundary = Usage {
            input_tokens: OPENAI_LONG_CONTEXT_SURCHARGE_THRESHOLD + 1,
            prompt_cache_write_tokens: Some(72_001),
            ..at_boundary.clone()
        };

        assert!(
            calculate_turn_cost_estimate_for_provider(
                ProviderKind::Openai,
                "gpt-5.6-sol",
                &at_boundary,
            )
            .is_some()
        );
        assert!(
            calculate_turn_cost_estimate_for_provider(
                ProviderKind::Openai,
                "gpt-5.6-sol",
                &above_boundary,
            )
            .is_none()
        );
    }

    #[test]
    fn minimax_m2_7_preserves_cache_read_and_write_rates() {
        let pricing = pricing_for_model_at("MiniMax-M2.7", Utc::now()).expect("M2.7 pricing");
        assert_eq!(pricing.usd.input_cache_hit_per_million, 0.06);
        assert_eq!(pricing.usd.input_cache_miss_per_million, 0.30);
        assert_eq!(pricing.usd.output_per_million, 1.20);
        assert_eq!(pricing.usd.cache_write, CacheWritePolicy::Rate(0.375));
    }

    /// The offline seed's price and the reviewed provider-owned table must
    /// agree wherever both price a row: the route audit reads a catalog rate
    /// before the hand table, so a disagreement silently changes the offline
    /// estimate (#6396). A deliberate difference belongs in
    /// `catalog_corrections.json`, which this reads through.
    #[test]
    fn bundled_seed_prices_agree_with_provider_owned_table() {
        let at = Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap();
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let mut checked = 0;
        let mut mismatches = Vec::new();
        for row in codewhale_config::catalog::bundled_catalog_offerings() {
            let Some(cost) = row.cost.as_ref() else {
                continue;
            };
            let Some(provider) = ProviderKind::parse(&row.provider) else {
                continue;
            };
            let Some(hand) = provider_owned_hand_pricing_at(provider, &row.wire_model_id, at)
            else {
                continue;
            };
            checked += 1;
            let usd = &hand.usd;
            let pairs = [
                ("input", cost.input, usd.input_cache_miss_per_million),
                ("output", cost.output, usd.output_per_million),
                (
                    "cache_read",
                    cost.cache_read,
                    usd.input_cache_hit_per_million,
                ),
            ];
            for (field, seed, table) in pairs {
                if let Some(seed) = seed
                    && !close(seed, table)
                {
                    mismatches.push(format!(
                        "{}/{} {field}: seed {seed} vs table {table}",
                        row.provider, row.wire_model_id
                    ));
                }
            }
        }
        assert!(
            checked > 0,
            "no bundled row has a provider-owned table price"
        );
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    #[test]
    fn curated_usd_only_models_have_pricing_and_accrue_cost() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            prompt_cache_hit_tokens: Some(250_000),
            prompt_cache_miss_tokens: Some(750_000),
            ..Default::default()
        };
        for (model, hit, miss, output) in [
            ("kimi-k2.6", 0.16, 0.95, 4.00),
            ("kimi-k2.7-code", 0.19, 0.95, 4.00),
            ("moonshotai/kimi-k2.7-code", 0.19, 0.95, 4.00),
            ("kimi-k2.7-code-highspeed", 0.38, 1.90, 8.00),
            ("moonshotai/kimi-k2.7-code-highspeed", 0.38, 1.90, 8.00),
            ("kimi-k3", 0.30, 3.00, 15.00),
            ("moonshotai/kimi-k3", 0.30, 3.00, 15.00),
            ("z-ai/glm-5.1", 0.26, 1.40, 4.40),
            ("glm-5.2", 0.26, 1.40, 4.40),
            ("z-ai/glm-5.2", 0.26, 1.40, 4.40),
            ("glm-5.3-flash", 0.03, 0.15, 0.50),
            ("z-ai/glm-5.3-flash", 0.03, 0.15, 0.50),
            ("glm-5-turbo", 0.24, 1.20, 4.00),
            ("z-ai/glm-5-turbo", 0.24, 1.20, 4.00),
            ("qwen/qwen3.6-plus", 0.325, 0.325, 1.95),
            ("qwen/qwen3.6-35b-a3b", 0.05, 0.14, 1.00),
            ("qwen/qwen3.6-27b", 0.15, 0.285, 2.40),
            // No published cache rate: cache-hit billed at the input rate.
            ("trinity-large-thinking", 0.25, 0.25, 0.80),
            ("nvidia/nemotron-3-ultra-550b-a55b", 0.10, 0.50, 2.20),
            ("claude-opus-4-8", 0.50, 5.00, 25.00),
            ("claude-opus-5", 0.50, 5.00, 25.00),
            ("claude-sonnet-4-6", 0.30, 3.00, 15.00),
            ("claude-haiku-4-5", 0.10, 1.00, 5.00),
            ("claude-fable-5", 1.00, 10.00, 50.00),
            ("gpt-5.5", 0.50, 5.00, 30.00),
            // GPT-5.5 Pro has no cached-input discount: cache-hit == input.
            ("gpt-5.5-pro", 30.00, 30.00, 180.00),
            ("gpt-5.6", 0.40, 4.00, 20.00),
            ("gpt-5.6-sol", 0.40, 4.00, 20.00),
            ("gpt-5.6-terra", 0.20, 2.00, 12.00),
            ("gpt-5.6-luna", 0.02, 0.20, 1.20),
            ("gpt-5-codex", 0.125, 1.25, 10.00),
            ("gpt-5.3-codex", 0.175, 1.75, 14.00),
            ("mistral-medium-latest", 0.15, 1.50, 7.50),
            ("mistral-medium-3-5", 0.15, 1.50, 7.50),
            ("mistral-large-latest", 0.05, 0.50, 1.50),
            ("mistral-large-2512", 0.05, 0.50, 1.50),
            ("mistral-small-latest", 0.015, 0.15, 0.60),
            ("mistral-small-2603", 0.015, 0.15, 0.60),
            ("mistral-code-latest", 0.03, 0.30, 0.90),
            ("codestral-latest", 0.03, 0.30, 0.90),
            ("qwen/qwen3.7-plus", 0.064, 0.32, 1.28),
            ("muse-spark-1.1", 0.15, 1.25, 4.25),
            ("muse-spark-1.2", 0.15, 1.25, 4.25),
            ("muse-spark-1.2-contributor", 0.002, 0.10, 0.20),
        ] {
            let pricing = pricing_for_model_at(model, Utc::now()).expect(model);
            assert_eq!(pricing.usd.input_cache_hit_per_million, hit);
            assert_eq!(pricing.usd.input_cache_miss_per_million, miss);
            assert_eq!(pricing.usd.output_per_million, output);
            assert!(pricing.cny.is_none());
            assert!(has_pricing_for_model(model));

            let estimate = calculate_turn_cost_estimate_from_usage(model, &usage).expect(model);
            assert!(estimate.usd > 0.0, "expected positive USD for {model}");
            assert_eq!(estimate.cny, 0.0);
        }

        // Anthropic / Qwen rows that publish a cache-write premium, and one row
        // (`gpt-5.5`) that publishes none — which is `Unpublished`, not a
        // licence to bill writes at the input rate (#4318).
        for (model, write) in [
            ("claude-opus-4-8", CacheWritePolicy::Rate(6.25)),
            ("claude-sonnet-4-6", CacheWritePolicy::Rate(3.75)),
            ("claude-haiku-4-5", CacheWritePolicy::Rate(1.25)),
            ("claude-fable-5", CacheWritePolicy::Rate(12.50)),
            ("qwen/qwen3.7-plus", CacheWritePolicy::Rate(0.40)),
            ("gpt-5.5", CacheWritePolicy::Unpublished),
        ] {
            let pricing = pricing_for_model_at(model, Utc::now()).expect(model);
            assert_eq!(
                pricing.usd.cache_write, write,
                "cache-write policy for {model}"
            );
        }
    }

    #[test]
    fn glm_5_3_has_no_hardcoded_price() {
        // GLM-5.3's catalog metadata is inherited from GLM-5.2, but Z.ai has
        // published no GLM-5.3 rate. Inheriting the 5.2 price would invent one,
        // so every price surface must report *unknown*, never a number and
        // never $0. If Z.ai publishes rates, delete this test and add the real
        // row — do not "fix" it by copying 5.2's.
        for model in ["glm-5.3", "z-ai/glm-5.3"] {
            assert!(
                pricing_for_model_at(model, Utc::now()).is_none(),
                "{model} must have no price row until Z.ai publishes one"
            );
            assert!(!has_pricing_for_model(model), "{model} must be unpriced");
            assert!(
                calculate_turn_cost_estimate_from_usage(
                    model,
                    &Usage {
                        input_tokens: 1_000_000,
                        output_tokens: 500_000,
                        ..Default::default()
                    },
                )
                .is_none(),
                "{model} must not accrue an invented cost estimate"
            );
        }
        // The priced sibling it inherits capabilities from is unaffected.
        assert!(has_pricing_for_model("glm-5.2"));
    }

    #[test]
    fn cache_write_tokens_increase_anthropic_cost_estimate() {
        let with_write = Usage {
            input_tokens: 12_048,
            output_tokens: 1,
            prompt_cache_hit_tokens: Some(10_000),
            prompt_cache_miss_tokens: Some(3),
            prompt_cache_write_tokens: Some(2_045),
            ..Default::default()
        };
        let write_as_miss = Usage {
            input_tokens: 12_048,
            output_tokens: 1,
            prompt_cache_hit_tokens: Some(10_000),
            prompt_cache_miss_tokens: Some(2_048),
            prompt_cache_write_tokens: None,
            ..Default::default()
        };

        let priced =
            calculate_turn_cost_estimate_from_usage("claude-fable-5", &with_write).expect("priced");
        let undercounted =
            calculate_turn_cost_estimate_from_usage("claude-fable-5", &write_as_miss)
                .expect("priced");
        // 2045 write @ 12.50 vs same tokens @ miss 10.00 → ~0.005 USD premium.
        assert!(
            priced.usd > undercounted.usd,
            "write premium should raise cost: priced={} undercounted={}",
            priced.usd,
            undercounted.usd
        );
        let expected_premium = (2_045.0 / 1_000_000.0) * (12.50 - 10.00);
        assert!(
            (priced.usd - undercounted.usd - expected_premium).abs() < 1e-9,
            "premium delta mismatch: {}",
            priced.usd - undercounted.usd
        );
    }

    #[test]
    fn catalog_pricing_uses_its_cache_write_rate() {
        let offering = codewhale_config::catalog::CatalogOffering {
            provider: "anthropic".to_string(),
            wire_model_id: "catalog-priced-model".to_string(),
            endpoint_key: "chat".to_string(),
            cost: Some(codewhale_config::models_dev::ModelsDevCost {
                input: Some(10.0),
                output: Some(50.0),
                cache_read: Some(1.0),
                cache_write: Some(12.5),
                ..Default::default()
            }),
            ..Default::default()
        };
        let usage = Usage {
            input_tokens: 13,
            output_tokens: 5,
            prompt_cache_hit_tokens: Some(2),
            prompt_cache_miss_tokens: Some(3),
            prompt_cache_write_tokens: Some(8),
            ..Default::default()
        };

        let estimate = catalog_cost_estimate_for_route(
            ProviderKind::Anthropic,
            "catalog-priced-model",
            &offering,
            &usage,
        )
        .expect("catalog cost estimate");
        assert!((estimate.usd - 0.000_382).abs() < 1e-15);
        assert_eq!(estimate.cny, 0.0);
    }

    #[test]
    fn recorded_time_provider_cost_keeps_catalog_cache_write_tier() {
        let request = Usage {
            input_tokens: 100_000,
            output_tokens: 0,
            prompt_cache_hit_tokens: Some(0),
            prompt_cache_miss_tokens: Some(0),
            prompt_cache_write_tokens: Some(100_000),
            ..Default::default()
        };
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            prompt_cache_hit_tokens: Some(0),
            prompt_cache_miss_tokens: Some(0),
            prompt_cache_write_tokens: Some(1_000_000),
            request_usages: vec![request; 10],
            ..Default::default()
        };

        let estimate = calculate_turn_cost_estimate_for_provider_at(
            ProviderKind::Openrouter,
            "qwen/qwen3.7-plus",
            &usage,
            Utc::now(),
        )
        .expect("provider catalog write price");

        assert!((estimate.usd - 0.40).abs() < f64::EPSILON);
        assert_eq!(estimate.cny, 0.0);
    }

    #[test]
    fn recorded_time_provider_cost_rejects_foreign_model_ids() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            ..Default::default()
        };

        assert!(
            calculate_turn_cost_estimate_for_provider_at(
                ProviderKind::Ollama,
                "gpt-5.5",
                &usage,
                Utc::now(),
            )
            .is_none()
        );
    }

    #[test]
    fn provider_cost_keeps_owned_hand_price_without_catalog_offering() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };
        assert!(
            crate::provider_lake::catalog_offering_for_model(ProviderKind::Openai, "gpt-5-codex")
                .is_none(),
            "regression fixture must exercise the hand-price fallback"
        );

        let estimate = calculate_turn_cost_estimate_for_provider_at(
            ProviderKind::Openai,
            "gpt-5-codex",
            &usage,
            Utc::now(),
        )
        .expect("OpenAI API owns the hand-priced model");

        assert!((estimate.usd - 1.25).abs() < f64::EPSILON);
        assert_eq!(estimate.cny, 0.0);
        assert!(has_pricing_for_provider(
            ProviderKind::Openai,
            "gpt-5-codex"
        ));
    }

    #[test]
    fn provider_price_does_not_invent_catalog_missing_cache_write_class() {
        let offering =
            crate::provider_lake::catalog_offering_for_model(ProviderKind::Openai, "gpt-5.5")
                .expect("bundled OpenAI route");
        let catalog_pricing =
            OfferingPricing::from_catalog_offering(&offering).expect("catalog pricing");
        assert!(catalog_pricing.cache_write_per_million.is_none());
        let usage = Usage {
            input_tokens: 250_000,
            output_tokens: 0,
            prompt_cache_miss_tokens: Some(0),
            prompt_cache_write_tokens: Some(250_000),
            ..Default::default()
        };

        let audit =
            audit_turn_cost_for_provider_at(ProviderKind::Openai, "gpt-5.5", &usage, Utc::now());

        assert!(audit.estimate.is_none());
        assert_eq!(
            audit.unpriced_reason,
            Some(UnpricedReason::MissingClassPrice)
        );
        assert_eq!(audit.unpriced_classes, vec![TokenClass::CacheWrite]);
    }

    #[test]
    fn provider_cost_does_not_fabricate_price_for_costless_catalog_route() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };
        let recorded_at = Utc::now();

        for (provider, model) in [
            (ProviderKind::Zai, "GLM-5.3"),
            (ProviderKind::XiaomiMimo, "mimo-v2.6-pro"),
            (ProviderKind::ModelstudioTokenPlan, "qwen3.8-max"),
        ] {
            let offering =
                crate::provider_lake::bundled_catalog_offering_for_model(provider, model)
                    .unwrap_or_else(|| panic!("missing bundled route: {provider:?}/{model}"));
            assert!(
                OfferingPricing::from_catalog_offering(&offering).is_none(),
                "{provider:?}/{model}"
            );
            assert!(
                calculate_turn_cost_estimate_for_provider_at(provider, model, &usage, recorded_at,)
                    .is_none(),
                "{provider:?}/{model}"
            );
            assert!(
                calculate_turn_cost_estimate_for_provider(provider, model, &usage).is_none(),
                "{provider:?}/{model}"
            );
            assert!(
                !has_pricing_for_provider(provider, model),
                "{provider:?}/{model}"
            );
        }

        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn recorded_time_provider_cost_bounds_deepseek_compatibility_aliases() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            ..Default::default()
        };
        let before_retirement: DateTime<Utc> =
            "2026-07-24T15:58:59Z".parse().expect("pre-retirement time");
        let at_retirement: DateTime<Utc> = DEEPSEEK_ALIAS_RETIREMENT_UTC
            .parse()
            .expect("retirement time");

        assert!(
            calculate_turn_cost_estimate_for_provider_at(
                ProviderKind::Deepseek,
                "deepseek-chat",
                &usage,
                before_retirement,
            )
            .is_some()
        );
        assert!(
            calculate_turn_cost_estimate_for_provider_at(
                ProviderKind::Deepseek,
                "deepseek-reasoner",
                &usage,
                at_retirement,
            )
            .is_none()
        );
    }

    #[test]
    fn deepseek_time_tier_names_the_window_for_tiered_models_only() {
        // Wednesday 2026-09-16: 02:00Z sits inside the 01:00-04:00 peak
        // window, 12:00Z outside every window.
        let peak = Utc.with_ymd_and_hms(2026, 9, 16, 2, 0, 0).unwrap();
        let off = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        // Saturday 02:00 Beijing time (Friday 18:00Z) bills off-peak even
        // inside a weekday peak hour.
        let weekend = Utc.with_ymd_and_hms(2026, 9, 18, 18, 0, 0).unwrap();
        for model in ["deepseek-v4-pro", "deepseek-v4-flash", "deepseek-flash"] {
            assert_eq!(deepseek_time_tier(model, peak), Some(true), "{model}");
            assert_eq!(deepseek_time_tier(model, off), Some(false), "{model}");
            assert_eq!(deepseek_time_tier(model, weekend), Some(false), "{model}");
        }
        assert_eq!(deepseek_time_tier(" DeepSeek-V4-Flash ", peak), Some(true));
        for flat in [
            "deepseek-chat",
            "deepseek-reasoner",
            "deepseek-ai/deepseek-v4-flash",
            "",
        ] {
            assert_eq!(deepseek_time_tier(flat, peak), None, "{flat}");
        }
    }

    #[test]
    fn deepseek_pricing_requires_exact_ids_or_explicit_route_aliases() {
        let at = utc_hm(2, 0);
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            ..Default::default()
        };
        let providers = [
            ProviderKind::Deepseek,
            ProviderKind::Deepseek,
            ProviderKind::DeepseekAnthropic,
        ];

        for model in [
            "deepseek-v4.1-flash-expires-on-0910",
            "deepseek-v4.1-flash",
            "deepseek-v4.1-pro",
            "deepseek-v4-flash-vendor-preview",
            "vendor/deepseek-v4-pro-unverified",
        ] {
            assert!(pricing_for_model_at(model, at).is_none(), "{model}");
            for provider in providers {
                assert!(
                    calculate_turn_cost_estimate_for_provider_at(provider, model, &usage, at)
                        .is_none(),
                    "{provider:?}/{model} must not inherit V4 rates"
                );
            }
        }

        for (canonical, aliases) in [
            (
                "deepseek-v4-flash",
                ["flash", "deepseek-v4flash", "deepseek-ai/deepseek-v4flash"],
            ),
            (
                "deepseek-v4-pro",
                ["pro", "deepseek-v4pro", "deepseek/deepseek-v4-pro"],
            ),
        ] {
            let expected = cost_estimate_with_pricing(
                pricing_for_model_at(canonical, at).expect("canonical V4 pricing"),
                &usage,
            );
            for provider in providers {
                for model in std::iter::once(canonical).chain(aliases) {
                    assert_eq!(
                        calculate_turn_cost_estimate_for_provider_at(provider, model, &usage, at),
                        Some(expected),
                        "{provider:?}/{model} must preserve the canonical rate"
                    );
                }
            }
        }
    }

    #[test]
    fn token_usage_for_pricing_maps_cache_classes_without_double_billing_reasoning() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            prompt_cache_hit_tokens: Some(250),
            prompt_cache_miss_tokens: Some(700),
            prompt_cache_write_tokens: Some(50),
            // Reasoning is a subset of the 100 reported output tokens, not an
            // extra 50 tokens of billable output.
            reasoning_tokens: Some(50),
            ..Default::default()
        };

        assert_eq!(
            token_usage_for_pricing(&usage),
            TokenUsage {
                input: 700,
                output: 100,
                cache_read: 250,
                cache_write: 50,

                reasoning: Some(50),
                input_audio: None,
                output_audio: None,

                request_usages: Vec::new(),
            }
        );

        // Informational reasoning telemetry must not move the billed output at
        // all: the same completion count costs the same with or without it.
        let without_reasoning = Usage {
            reasoning_tokens: None,
            ..usage.clone()
        };
        assert_eq!(
            token_usage_for_pricing(&usage).output,
            token_usage_for_pricing(&without_reasoning).output
        );
        assert_eq!(
            calculate_turn_cost_estimate_for_provider(
                ProviderKind::Anthropic,
                "claude-haiku-4-5",
                &usage,
            ),
            calculate_turn_cost_estimate_for_provider(
                ProviderKind::Anthropic,
                "claude-haiku-4-5",
                &without_reasoning,
            )
        );
    }

    #[test]
    fn contradictory_cache_partition_is_bounded_and_fails_closed() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 10,
            prompt_cache_hit_tokens: Some(80),
            prompt_cache_miss_tokens: Some(40),
            prompt_cache_write_tokens: Some(30),
            ..Usage::default()
        };

        let classes = token_usage_for_pricing(&usage);
        assert_eq!(
            classes.input + classes.cache_read + classes.cache_write,
            u64::from(usage.input_tokens),
            "token projection may never exceed the provider's input total"
        );
        let audit = audit_turn_cost_for_provider_on_endpoint_at(
            ProviderKind::Deepseek,
            "deepseek-v4-flash",
            None,
            &usage,
            Utc::now(),
        );
        assert!(audit.estimate.is_none());
        assert_eq!(
            audit.unpriced_reason,
            Some(UnpricedReason::InconsistentUsage)
        );

        let overflow_shape = Usage {
            input_tokens: u32::MAX,
            prompt_cache_hit_tokens: Some(u32::MAX),
            prompt_cache_miss_tokens: Some(1),
            ..Usage::default()
        };
        assert!(
            !usage_cache_partition_is_consistent(&overflow_shape),
            "consistency validation must not hide overflow via saturation"
        );
    }

    #[test]
    fn openai_codex_gpt55_cost_is_unavailable_even_with_usage() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            prompt_cache_hit_tokens: Some(250),
            prompt_cache_miss_tokens: Some(750),
            ..Default::default()
        };

        assert!(calculate_turn_cost_estimate_from_usage("gpt-5.5", &usage).is_some());
        assert!(has_pricing_for_provider(ProviderKind::Openai, "gpt-5.5"));
        assert!(!has_pricing_for_provider(
            ProviderKind::OpenaiCodex,
            "gpt-5.5"
        ));
        assert!(
            calculate_turn_cost_estimate_for_provider(ProviderKind::OpenaiCodex, "gpt-5.5", &usage)
                .is_none()
        );
    }

    #[test]
    fn subscription_route_does_not_inherit_same_models_api_price() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            ..Default::default()
        };
        assert!(
            calculate_turn_cost_estimate_for_billing_surface(
                ProviderKind::Anthropic,
                "claude-sonnet-5",
                Some(FIRST_PARTY_PAYG_BILLING_SURFACE),
                &usage,
            )
            .is_some()
        );
        assert!(
            calculate_turn_cost_estimate_for_route(
                ProviderKind::Anthropic,
                "claude-sonnet-5",
                &usage,
                crate::route_billing::BillingPresentation::Subscription("Claude OAuth quota"),
            )
            .is_none()
        );
    }

    #[test]
    fn token_usage_for_pricing_infers_missing_cache_miss_from_hit_source() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            prompt_cache_hit_tokens: Some(250),
            prompt_cache_miss_tokens: None,
            ..Default::default()
        };

        assert_eq!(
            token_usage_for_pricing(&usage),
            TokenUsage {
                input: 750,
                output: 100,
                cache_read: 250,
                cache_write: 0,

                reasoning: None,
                input_audio: None,
                output_audio: None,

                request_usages: Vec::new(),
            }
        );
    }

    #[test]
    fn catalog_pricing_overrides_known_row_when_present() {
        for model in [
            "catalog-priced-model",
            "deepseek-v4.1-flash-expires-on-0910",
        ] {
            assert!(
                pricing_for_model_at(model, Utc::now()).is_none(),
                "private unknown rows never gain global prices"
            );
        }
        // Exact configured prices are tested through provider_lake + endpoint
        // identity above; the primitive helper cannot see those scoped declarations.
    }

    /// Published Claude Sonnet 5 rates per 1M tokens (cache-hit, cache-miss,
    /// output, 5m cache-write), verified live on
    /// platform.claude.com/docs/en/about-claude/pricing on 2026-08-17: the
    /// $2/$10 launch rate is now standard and the 2026-09-01 increase to
    /// $3/$15 "will not occur".
    const CLAUDE_SONNET_5_USD: (f64, f64, f64, f64) = (0.20, 2.00, 10.00, 2.50);

    fn assert_sonnet_5_standard_rate(at: DateTime<Utc>) {
        let pricing = pricing_for_model_at("claude-sonnet-5", at).unwrap();
        let (hit, miss, out, write) = CLAUDE_SONNET_5_USD;
        assert_eq!(pricing.usd.input_cache_hit_per_million, hit, "{at} hit");
        assert_eq!(pricing.usd.input_cache_miss_per_million, miss, "{at} miss");
        assert_eq!(pricing.usd.output_per_million, out, "{at} output");
        assert_eq!(
            pricing.usd.cache_write,
            CacheWritePolicy::Rate(write),
            "{at} write"
        );
        assert!(pricing.cny.is_none());
    }

    #[test]
    fn sonnet_5_keeps_the_2_10_rate_before_the_former_2026_08_31_boundary() {
        assert_sonnet_5_standard_rate(
            Utc.with_ymd_and_hms(2026, 8, 31, 23, 59, 59)
                .single()
                .unwrap(),
        );
        assert!(has_pricing_for_model("claude-sonnet-5"));
    }

    #[test]
    fn sonnet_5_does_not_flip_to_3_15_on_2026_09_01() {
        // Regression for the retired intro window: the scheduled increase was
        // cancelled upstream, so neither boundary minute nor any later time
        // may resurface 0.30 / 3.00 / 15.00 / 3.75.
        for at in [
            Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).single().unwrap(),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).single().unwrap(),
        ] {
            assert_sonnet_5_standard_rate(at);
            let pricing = pricing_for_model_at("claude-sonnet-5", at).unwrap();
            assert_ne!(pricing.usd.input_cache_hit_per_million, 0.30);
            assert_ne!(pricing.usd.input_cache_miss_per_million, 3.00);
            assert_ne!(pricing.usd.output_per_million, 15.00);
            assert_ne!(pricing.usd.cache_write, CacheWritePolicy::Rate(3.75));
        }
    }

    #[test]
    fn claude_opus_5_matches_published_first_party_card() {
        // https://platform.claude.com/docs/en/about-claude/pricing (2026-08-17):
        // $5 in / $25 out, cache read 0.50, 5m cache write 6.25.
        let pricing = pricing_for_model_at("claude-opus-5", Utc::now()).expect("Opus 5 pricing");
        assert_eq!(pricing.usd.input_cache_hit_per_million, 0.50);
        assert_eq!(pricing.usd.input_cache_miss_per_million, 5.00);
        assert_eq!(pricing.usd.output_per_million, 25.00);
        assert_eq!(pricing.usd.cache_write, CacheWritePolicy::Rate(6.25));
        assert!(pricing.cny.is_none());
        assert!(
            provider_owned_hand_pricing_at(ProviderKind::Anthropic, "claude-opus-5", Utc::now())
                .is_some(),
            "direct Anthropic owns the Opus 5 row"
        );
        assert!(
            provider_owned_hand_pricing_at(ProviderKind::Openrouter, "claude-opus-5", Utc::now())
                .is_none(),
            "an aggregator must not inherit the first-party Opus 5 row"
        );
    }

    #[test]
    fn gpt_5_6_terra_and_luna_use_current_short_context_rates() {
        // https://developers.openai.com/api/docs/models/gpt-5.6-terra and
        // /gpt-5.6-luna (2026-08-17): Terra $2.00 / $0.20 / $12.00, Luna
        // $0.20 / $0.02 / $1.20 per 1M. The retired launch cards must not
        // resurface.
        for (model, hit, miss, out, stale) in [
            ("gpt-5.6-terra", 0.20, 2.00, 12.00, (0.25, 2.50, 15.00)),
            ("gpt-5.6-luna", 0.02, 0.20, 1.20, (0.10, 1.00, 6.00)),
        ] {
            let pricing = pricing_for_model_at(model, Utc::now()).expect(model);
            assert_eq!(pricing.usd.input_cache_hit_per_million, hit, "{model}");
            assert_eq!(pricing.usd.input_cache_miss_per_million, miss, "{model}");
            assert_eq!(pricing.usd.output_per_million, out, "{model}");
            assert_ne!(pricing.usd.input_cache_hit_per_million, stale.0);
            assert_ne!(pricing.usd.input_cache_miss_per_million, stale.1);
            assert_ne!(pricing.usd.output_per_million, stale.2);
        }
    }

    #[test]
    fn moonshot_direct_kimi_k3_is_priced_but_membership_k3_is_not() {
        // https://platform.kimi.ai/docs/pricing/chat-k3 (2026-08-17):
        // cache-hit 0.30 / cache-miss 3.00 / output 15.00 per 1M.
        let now = Utc::now();
        let pricing = provider_owned_hand_pricing_at(ProviderKind::Moonshot, "kimi-k3", now)
            .expect("direct Moonshot owns the kimi-k3 row");
        assert_eq!(pricing.usd.input_cache_hit_per_million, 0.30);
        assert_eq!(pricing.usd.input_cache_miss_per_million, 3.00);
        assert_eq!(pricing.usd.output_per_million, 15.00);
        assert!(
            provider_owned_hand_pricing_at(ProviderKind::Moonshot, "k3", now).is_none(),
            "Kimi Code membership `k3` is quota billed"
        );
        assert!(pricing_for_model_at("k3", now).is_none());
        // Fireworks-hosted K3 keeps its own (still unpublished) rate card.
        assert!(
            provider_owned_hand_pricing_at(
                ProviderKind::Fireworks,
                "accounts/fireworks/models/kimi-k3",
                now
            )
            .is_none()
        );
    }

    #[test]
    fn kimi_k2_7_code_highspeed_matches_published_rates() {
        // https://platform.kimi.ai/docs/pricing/chat-k27-code (2026-08-17).
        let now = Utc::now();
        let pricing =
            provider_owned_hand_pricing_at(ProviderKind::Moonshot, "kimi-k2.7-code-highspeed", now)
                .expect("direct Moonshot owns the K2.7 Code high-speed row");
        assert_eq!(pricing.usd.input_cache_hit_per_million, 0.38);
        assert_eq!(pricing.usd.input_cache_miss_per_million, 1.90);
        assert_eq!(pricing.usd.output_per_million, 8.00);
        // Exactly 2x the standard K2.7 Code card.
        let standard = pricing_for_model_at("kimi-k2.7-code", now).unwrap();
        assert!((standard.usd.input_cache_hit_per_million * 2.0 - 0.38).abs() < 1e-12);
        assert!((standard.usd.input_cache_miss_per_million * 2.0 - 1.90).abs() < 1e-12);
        assert!((standard.usd.output_per_million * 2.0 - 8.00).abs() < 1e-12);
    }

    #[test]
    fn minimax_m2_7_highspeed_preserves_cache_read_and_write_rates() {
        // https://platform.minimax.io/docs/guides/pricing-paygo (2026-08-17):
        // $0.6 in / $2.4 out / $0.06 cache read / $0.375 cache write.
        for provider in [ProviderKind::Minimax, ProviderKind::MinimaxAnthropic] {
            let pricing =
                provider_owned_hand_pricing_at(provider, "MiniMax-M2.7-highspeed", Utc::now())
                    .expect("direct MiniMax owns the M2.7 high-speed row");
            assert_eq!(
                pricing.usd.input_cache_hit_per_million, 0.06,
                "{provider:?}"
            );
            assert_eq!(
                pricing.usd.input_cache_miss_per_million, 0.60,
                "{provider:?}"
            );
            assert_eq!(pricing.usd.output_per_million, 2.40, "{provider:?}");
            assert_eq!(
                pricing.usd.cache_write,
                CacheWritePolicy::Rate(0.375),
                "{provider:?}"
            );
        }
    }

    #[test]
    fn mistral_first_party_rows_match_published_table_and_stay_provider_owned() {
        // https://docs.mistral.ai/inference/pricing (2026-08-17): Medium 3.5
        // 1.5 / 0.15 / 7.5, Large 3 0.5 / 0.05 / 1.5, Small 4 0.15 / 0.015 /
        // 0.6, Codestral 0.3 / 0.03 / 0.9 (input / cached input / output).
        let now = Utc::now();
        for (model, hit, miss, out) in [
            ("mistral-medium-latest", 0.15, 1.50, 7.50),
            ("mistral-large-latest", 0.05, 0.50, 1.50),
            ("mistral-small-latest", 0.015, 0.15, 0.60),
            ("mistral-code-latest", 0.03, 0.30, 0.90),
        ] {
            let pricing = provider_owned_hand_pricing_at(ProviderKind::Mistral, model, now)
                .unwrap_or_else(|| panic!("direct Mistral owns {model}"));
            assert_eq!(pricing.usd.input_cache_hit_per_million, hit, "{model}");
            assert_eq!(pricing.usd.input_cache_miss_per_million, miss, "{model}");
            assert_eq!(pricing.usd.output_per_million, out, "{model}");
            // No published cache-write rate: unpriced, never assumed.
            assert_eq!(
                pricing.usd.cache_write,
                CacheWritePolicy::Unpublished,
                "{model}"
            );
            assert!(
                provider_owned_hand_pricing_at(ProviderKind::Openrouter, model, now).is_none(),
                "{model}: aggregators must not inherit first-party Mistral rates"
            );
        }
    }

    /// Published DeepSeek V4 rates per 1M tokens (cache-hit, cache-miss,
    /// output), verified live on api-docs.deepseek.com/quick_start/pricing
    /// (and /zh-cn) on 2026-08-17. Off-peak is exactly half of peak.
    const DEEPSEEK_V4_FLASH_USD_OFF_PEAK: (f64, f64, f64) = (0.007, 0.22, 0.66);
    const DEEPSEEK_V4_FLASH_USD_PEAK: (f64, f64, f64) = (0.014, 0.44, 1.32);
    const DEEPSEEK_V4_FLASH_CNY_OFF_PEAK: (f64, f64, f64) = (0.05, 1.5, 4.5);
    const DEEPSEEK_V4_FLASH_CNY_PEAK: (f64, f64, f64) = (0.10, 3.0, 9.0);
    const DEEPSEEK_V4_PRO_USD_OFF_PEAK: (f64, f64, f64) = (0.022, 0.66, 1.98);
    const DEEPSEEK_V4_PRO_USD_PEAK: (f64, f64, f64) = (0.044, 1.32, 3.96);
    const DEEPSEEK_V4_PRO_CNY_OFF_PEAK: (f64, f64, f64) = (0.15, 4.5, 13.5);
    const DEEPSEEK_V4_PRO_CNY_PEAK: (f64, f64, f64) = (0.30, 9.0, 27.0);

    fn assert_currency_rates(actual: &CurrencyPricing, expected: (f64, f64, f64), ctx: &str) {
        assert_eq!(
            actual.input_cache_hit_per_million, expected.0,
            "{ctx} cache-hit"
        );
        assert_eq!(
            actual.input_cache_miss_per_million, expected.1,
            "{ctx} cache-miss"
        );
        assert_eq!(actual.output_per_million, expected.2, "{ctx} output");
        assert_eq!(
            actual.cache_write,
            CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
            "{ctx} cache-write"
        );
    }

    fn assert_deepseek_tier(model: &str, at: DateTime<Utc>, peak: bool) {
        let pricing = pricing_for_model_at(model, at).expect("DeepSeek V4 pricing");
        let (usd, cny) = match (model.contains("pro"), peak) {
            (true, true) => (DEEPSEEK_V4_PRO_USD_PEAK, DEEPSEEK_V4_PRO_CNY_PEAK),
            (true, false) => (DEEPSEEK_V4_PRO_USD_OFF_PEAK, DEEPSEEK_V4_PRO_CNY_OFF_PEAK),
            (false, true) => (DEEPSEEK_V4_FLASH_USD_PEAK, DEEPSEEK_V4_FLASH_CNY_PEAK),
            (false, false) => (
                DEEPSEEK_V4_FLASH_USD_OFF_PEAK,
                DEEPSEEK_V4_FLASH_CNY_OFF_PEAK,
            ),
        };
        let tier = if peak { "peak" } else { "off-peak" };
        assert_currency_rates(&pricing.usd, usd, &format!("{model} @ {at} USD {tier}"));
        let cny_pricing = pricing.cny.expect("DeepSeek pricing has CNY");
        assert_currency_rates(&cny_pricing, cny, &format!("{model} @ {at} CNY {tier}"));
    }

    fn utc_hm(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 17, hour, minute, 59)
            .single()
            .unwrap()
    }

    #[test]
    fn deepseek_peak_window_is_half_open_on_utc_hours() {
        for hour in 0..24 {
            let expected = matches!(hour, 1..=3 | 6..=9);
            assert_eq!(deepseek_peak_hour(hour), expected, "hour {hour}");
        }
    }

    #[test]
    fn deepseek_v4_tiers_flip_at_each_published_utc_boundary() {
        // Peak windows are 01:00-04:00 and 06:00-10:00 UTC, half-open: the
        // start minute is peak, the end minute is off-peak.
        let boundaries = [
            (utc_hm(0, 59), false),
            (utc_hm(1, 0), true),
            (utc_hm(3, 59), true),
            (utc_hm(4, 0), false),
            (utc_hm(5, 59), false),
            (utc_hm(6, 0), true),
            (utc_hm(9, 59), true),
            (utc_hm(10, 0), false),
        ];
        for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
            for (at, peak) in boundaries {
                assert_deepseek_tier(model, at, peak);
            }
        }
    }

    fn utc_ymd_h(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn deepseek_v4_bills_beijing_weekends_off_peak_from_the_published_date() {
        // 2026-08-22T16:00Z is 00:00 Beijing on Sunday 2026-08-23, when the rule
        // starts. Times are UTC; the Beijing day is in the comment.
        let cases = [
            (utc_ymd_h(2026, 8, 22, 6), true), // Sat 14:00 Beijing, rule not yet live
            (utc_ymd_h(2026, 8, 23, 1), false), // Sun 09:00 Beijing, first window it changes
            (utc_ymd_h(2026, 8, 23, 9), false), // Sun 17:00 Beijing
            (utc_ymd_h(2026, 8, 24, 1), true), // Mon 09:00 Beijing
            (utc_ymd_h(2026, 8, 28, 6), true), // Fri 14:00 Beijing
            (utc_ymd_h(2026, 8, 29, 6), false), // Sat 14:00 Beijing
        ];
        for (at, peak) in cases {
            assert_eq!(deepseek_is_peak(at), peak, "peak tier at {at}");
            for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
                assert_deepseek_tier(model, at, peak);
            }
        }
    }

    #[test]
    fn deepseek_weekend_edges_are_bounded_in_beijing_time_not_utc() {
        // All four instants are off-peak by the hour, so `deepseek_is_peak`
        // cannot tell them apart today. Pinning the predicate keeps the 16:00Z
        // edges right if the peak windows ever move.
        let cases = [
            (utc_ymd_h(2026, 8, 28, 15), false), // Fri 23:00 Beijing
            (utc_ymd_h(2026, 8, 28, 16), true),  // Sat 00:00 Beijing
            (utc_ymd_h(2026, 8, 30, 15), true),  // Sun 23:00 Beijing
            (utc_ymd_h(2026, 8, 30, 16), false), // Mon 00:00 Beijing
        ];
        for (at, weekend) in cases {
            assert_eq!(deepseek_weekend_off_peak(at), weekend, "weekend at {at}");
        }
    }

    #[test]
    fn deepseek_v4_pro_keeps_pro_rates_after_cancelled_retirement() {
        for day in [13, 14, 15, 21] {
            let at = utc_ymd_h(2026, 9, day, 12);
            let pro = pricing_for_model_at("deepseek-v4-pro", at).unwrap();
            let flash = pricing_for_model_at("deepseek-flash", at).unwrap();
            assert_eq!(pro.usd.output_per_million, 1.98);
            assert_ne!(pro.usd.output_per_million, flash.usd.output_per_million);
        }
    }

    #[test]
    fn deepseek_v4_pro_off_peak_and_peak_rates_match_published_table() {
        assert_deepseek_tier("deepseek-v4-pro", utc_hm(12, 0), false);
        assert_deepseek_tier("deepseek-v4-pro", utc_hm(2, 0), true);
        // Regression for #267 / #2489: the retired flat promo rates must not
        // resurface in either tier.
        for at in [utc_hm(12, 0), utc_hm(2, 0)] {
            let pricing = pricing_for_model_at("deepseek-v4-pro", at).unwrap();
            assert_ne!(pricing.usd.input_cache_hit_per_million, 0.003625);
            assert_ne!(pricing.usd.input_cache_miss_per_million, 0.435);
            assert_ne!(pricing.usd.output_per_million, 0.87);
        }
    }

    #[test]
    fn deepseek_v4_flash_off_peak_and_peak_rates_match_published_table() {
        assert_deepseek_tier("deepseek-v4-flash", utc_hm(12, 0), false);
        assert_deepseek_tier("deepseek-v4-flash", utc_hm(7, 0), true);
        for at in [utc_hm(12, 0), utc_hm(7, 0)] {
            let pricing = pricing_for_model_at("deepseek-v4-flash", at).unwrap();
            assert_ne!(pricing.usd.input_cache_hit_per_million, 0.0028);
            assert_ne!(pricing.usd.input_cache_miss_per_million, 0.14);
            assert_ne!(pricing.usd.output_per_million, 0.28);
        }
    }

    #[test]
    fn deepseek_v4_off_peak_is_exactly_half_of_peak() {
        for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
            let off = pricing_for_model_at(model, utc_hm(12, 0)).unwrap();
            let peak = pricing_for_model_at(model, utc_hm(2, 0)).unwrap();
            for (o, p) in [
                (
                    off.usd.input_cache_hit_per_million,
                    peak.usd.input_cache_hit_per_million,
                ),
                (
                    off.usd.input_cache_miss_per_million,
                    peak.usd.input_cache_miss_per_million,
                ),
                (off.usd.output_per_million, peak.usd.output_per_million),
            ] {
                assert!((o * 2.0 - p).abs() < 1e-12, "{model}: {o} * 2 != {p}");
            }
            let (off_cny, peak_cny) = (off.cny.unwrap(), peak.cny.unwrap());
            for (o, p) in [
                (
                    off_cny.input_cache_hit_per_million,
                    peak_cny.input_cache_hit_per_million,
                ),
                (
                    off_cny.input_cache_miss_per_million,
                    peak_cny.input_cache_miss_per_million,
                ),
                (off_cny.output_per_million, peak_cny.output_per_million),
            ] {
                assert!((o * 2.0 - p).abs() < 1e-12, "{model}: CNY {o} * 2 != {p}");
            }
        }
    }

    /// The route audit prices a DeepSeek turn at the tier of its RECORDED
    /// time, not the wall clock at audit time (same contract as Sonnet 5's
    /// recorded-time introductory window).
    #[test]
    fn deepseek_audit_uses_recorded_time_tier_not_now() {
        let usage = million_input_usage();
        for (provider, model) in [
            (ProviderKind::Deepseek, "deepseek-v4-flash"),
            (ProviderKind::Deepseek, "deepseek-v4-pro"),
            (ProviderKind::Deepseek, "deepseek-v4-flash"),
            (ProviderKind::DeepseekAnthropic, "deepseek-v4-pro"),
        ] {
            let off_peak_usd = if model.contains("pro") { 0.66 } else { 0.22 };
            let off_peak_cny = if model.contains("pro") { 4.5 } else { 1.5 };
            let off = audit_turn_cost_for_provider_at(provider, model, &usage, utc_hm(12, 0));
            assert!(off.is_priced(), "{provider:?}/{model}: {off:?}");
            let off_estimate = off.estimate.expect("priced");
            assert!(
                (off_estimate.usd - off_peak_usd).abs() < 1e-12,
                "{provider:?}/{model} off-peak: {}",
                off_estimate.usd
            );
            assert!(
                (off_estimate.cny - off_peak_cny).abs() < 1e-12,
                "{provider:?}/{model} off-peak CNY: {}",
                off_estimate.cny
            );

            let peak = audit_turn_cost_for_provider_at(provider, model, &usage, utc_hm(2, 0));
            assert!(peak.is_priced(), "{provider:?}/{model}: {peak:?}");
            let peak_estimate = peak.estimate.expect("priced");
            assert!(
                (peak_estimate.usd - 2.0 * off_peak_usd).abs() < 1e-12,
                "{provider:?}/{model} peak: {}",
                peak_estimate.usd
            );
            assert!(
                (peak_estimate.cny - 2.0 * off_peak_cny).abs() < 1e-12,
                "{provider:?}/{model} peak CNY: {}",
                peak_estimate.cny
            );
        }
    }

    #[test]
    fn fireworks_and_zen_flash_use_bundled_family_rates() {
        let now = Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, 0).single().unwrap();
        let fireworks = provider_owned_hand_pricing_at(
            ProviderKind::Fireworks,
            "accounts/fireworks/models/deepseek-v4-flash",
            now,
        )
        .expect("Fireworks Flash should inherit the bundled DeepSeek family row");
        let zen =
            provider_owned_hand_pricing_at(ProviderKind::OpencodeZen, "deepseek-v4-flash", now)
                .expect("OpenCode Zen Flash should inherit the bundled DeepSeek family row");
        assert_eq!(fireworks.usd.output_per_million, zen.usd.output_per_million);
        assert!(
            provider_owned_hand_pricing_at(
                ProviderKind::Fireworks,
                "accounts/fireworks/models/kimi-k3",
                now,
            )
            .is_none(),
            "kimi-k3 has no published bundled rate; do not invent one"
        );
    }

    #[test]
    fn xiaomi_mimo_token_plan_models_leave_cost_unknown() {
        let now = Utc.with_ymd_and_hms(2026, 6, 4, 0, 0, 0).single().unwrap();

        for model in [
            "mimo-v2.5-pro",
            "mimo-v2.5-pro-ultraspeed",
            "mimo-v2.5",
            "xiaomi/mimo-v2.5",
        ] {
            assert!(pricing_for_model_at(model, now).is_none());
            assert!(!has_pricing_for_model(model));
        }
    }

    #[test]
    fn cost_estimate_calculates_usd_and_cny() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            ..Default::default()
        };
        // Off-peak (12:00 UTC): 1M input at 0.22 + 0.5M output at 0.66 USD;
        // 1.5 + 0.5 * 4.5 CNY.
        let off_peak = Utc
            .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
            .single()
            .unwrap();
        let pricing = pricing_for_model_at("deepseek-v4-flash", off_peak).expect("pricing");
        let estimate = cost_estimate_with_pricing(pricing, &usage);
        assert!((estimate.usd - 0.55).abs() < 1e-12, "{}", estimate.usd);
        assert!((estimate.cny - 3.75).abs() < 1e-12, "{}", estimate.cny);

        // Peak (02:00 UTC) doubles both currencies.
        let peak = Utc.with_ymd_and_hms(2026, 8, 17, 2, 0, 0).single().unwrap();
        let pricing = pricing_for_model_at("deepseek-v4-flash", peak).expect("pricing");
        let estimate = cost_estimate_with_pricing(pricing, &usage);
        assert!((estimate.usd - 1.10).abs() < 1e-12, "{}", estimate.usd);
        assert!((estimate.cny - 7.5).abs() < 1e-12, "{}", estimate.cny);
    }

    #[test]
    fn cost_currency_accepts_yuan_aliases() {
        assert_eq!(CostCurrency::from_setting("usd"), Some(CostCurrency::Usd));
        assert_eq!(CostCurrency::from_setting("yuan"), Some(CostCurrency::Cny));
        assert_eq!(CostCurrency::from_setting("rmb"), Some(CostCurrency::Cny));
        assert_eq!(CostCurrency::from_setting("cny"), Some(CostCurrency::Cny));
        assert_eq!(CostCurrency::from_setting("eur"), None);
    }

    #[test]
    fn format_cost_amount_uses_selected_symbol() {
        assert_eq!(format_cost_amount(0.42, CostCurrency::Usd), "$0.42");
        assert_eq!(format_cost_amount(2.0, CostCurrency::Cny), "¥2.00");
        assert_eq!(format_cost_amount(0.0, CostCurrency::Usd), "$0.00");
        assert_eq!(format_cost_amount(0.00001, CostCurrency::Usd), "<$0.0001");
    }

    #[test]
    fn format_cost_amount_precise_keeps_report_precision() {
        assert_eq!(
            codewhale_commands::diagnostics_reports::format_cost_amount_precise(
                0.1234,
                codewhale_command_contract::types::CommandCurrency::Usd
            ),
            "$0.1234"
        );
        assert_eq!(
            codewhale_commands::diagnostics_reports::format_cost_amount_precise(
                0.1234,
                codewhale_command_contract::types::CommandCurrency::Cny
            ),
            "¥0.1234"
        );
        assert_eq!(
            codewhale_commands::diagnostics_reports::format_cost_amount_precise(
                0.0,
                codewhale_command_contract::types::CommandCurrency::Usd
            ),
            "$0.0000"
        );
        assert_eq!(
            codewhale_commands::diagnostics_reports::format_cost_amount_precise(
                0.00001,
                codewhale_command_contract::types::CommandCurrency::Usd
            ),
            "<$0.0001"
        );
    }

    #[test]
    fn accumulated_cost_stays_finite_and_nonnegative() {
        let saturated = CostEstimate {
            usd: f64::MAX,
            cny: 1.0,
        }
        .saturating_add(CostEstimate {
            usd: f64::MAX,
            cny: -1.0,
        });
        assert_eq!(saturated.usd, f64::MAX);
        assert_eq!(saturated.cny, 1.0);
        assert!(saturated.is_finite_nonnegative());

        assert_eq!(
            CostEstimate {
                usd: f64::NAN,
                cny: f64::INFINITY,
            }
            .sanitized(),
            CostEstimate::default()
        );
    }

    fn official_route_audit(provider: ProviderKind, model: &str, usage: &Usage) -> TurnCostAudit {
        let base = provider.provider().default_base_url();
        let at = Utc::now();
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(base);
        let quote = crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
            provider,
            provider.as_str(),
            model,
            base,
            at.timestamp() as u64,
        );
        audit_turn_cost_for_route_on_endpoint_for_identity_at(
            provider,
            Some(provider.as_str()),
            model,
            billing_surface_for_route(provider, Some(base)),
            Some(&fingerprint),
            quote.as_ref(),
            usage,
            at,
        )
    }

    fn million_input_usage() -> Usage {
        Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Usage::default()
        }
    }

    /// #5241: Fireworks flash / pro and OpenCode Zen flash must leave
    /// `unverified_live_pricing` via provider-docs bundled rates when live
    /// control-plane / Models.dev pricing is not a usable rate source.
    #[test]
    fn hosted_flash_and_pro_routes_price_from_bundled_docs_rates() {
        let usage = million_input_usage();
        let now = Utc::now();
        let cases = [
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4-flash-0731",
                0.14,
            ),
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4-flash",
                0.14,
            ),
            (ProviderKind::Fireworks, "deepseek-v4-flash", 0.14),
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4-pro",
                1.74,
            ),
            (ProviderKind::OpencodeZen, "deepseek-v4-flash", 0.14),
        ];
        for (provider, model, expected_usd) in cases {
            let audit = official_route_audit(provider, model, &usage);
            assert!(
                audit.is_priced(),
                "{provider:?}/{model} must price on its official endpoint: {audit:?}"
            );
            assert_ne!(
                audit.unpriced_reason,
                Some(UnpricedReason::UnverifiedLivePricing),
                "{provider:?}/{model}"
            );
            assert_eq!(
                audit.provenance,
                Some(PricingProvenance::ProviderDocs),
                "{provider:?}/{model}"
            );
            let estimate = audit.estimate.expect("priced");
            assert!(
                (estimate.usd - expected_usd).abs() < 1e-12,
                "{provider:?}/{model}: {} != {expected_usd}",
                estimate.usd
            );
            assert_eq!(estimate.cny, 0.0, "{provider:?}/{model}");

            let hand =
                provider_owned_hand_pricing_at(provider, model, now).expect("bundled fallback row");
            if model.contains("flash") {
                assert_eq!(hand.usd.input_cache_hit_per_million, 0.028);
                for first_party in [0.007, 0.014] {
                    assert_ne!(
                        hand.usd.input_cache_hit_per_million, first_party,
                        "must not inherit first-party DeepSeek cache-hit"
                    );
                }
            }
        }

        let off_peak = Utc
            .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
            .single()
            .unwrap();
        let peak = Utc.with_ymd_and_hms(2026, 8, 17, 2, 0, 0).single().unwrap();
        assert_eq!(
            deepseek_v4_flash_pricing(off_peak)
                .usd
                .input_cache_hit_per_million,
            0.007
        );
        assert_eq!(
            deepseek_v4_flash_pricing(peak)
                .usd
                .input_cache_hit_per_million,
            0.014
        );
    }

    #[test]
    fn models_dev_live_rates_price_the_published_estimate() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("timestamp");
        crate::provider_lake::set_live_snapshot(
            codewhale_config::catalog::CatalogSnapshot {
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "fireworks".to_string(),
                    wire_model_id: "accounts/fireworks/models/deepseek-v4-flash-0731".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(99.0),
                        output: Some(199.0),
                        cache_read: Some(9.0),
                        cache_write: None,
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: "models-dev-capabilities".to_string(),
                        fetched_at,
                    },
                    ..Default::default()
                }],
            },
            crate::provider_lake::LiveSource::ModelsDev,
        );

        let usage = million_input_usage();
        let audit = official_route_audit(
            ProviderKind::Fireworks,
            "accounts/fireworks/models/deepseek-v4-flash-0731",
            &usage,
        );
        crate::provider_lake::clear_live_snapshot();

        assert!(audit.is_priced(), "{audit:?}");
        assert_ne!(
            audit.unpriced_reason,
            Some(UnpricedReason::UnverifiedLivePricing)
        );
        assert_eq!(audit.provenance, Some(PricingProvenance::ModelsDevLive));
        assert_eq!(audit.live_pricing_defect, None);
        let estimate = audit.estimate.expect("priced");
        assert!(
            (estimate.usd - 99.0).abs() < 1e-12,
            "live models.dev input rate must price the estimate: {}",
            estimate.usd
        );
    }

    #[test]
    fn unverifiable_provider_live_rates_degrade_to_bundled_docs_with_defect() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("timestamp");
        crate::provider_lake::set_live_snapshot(
            codewhale_config::catalog::CatalogSnapshot {
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "opencode-zen".to_string(),
                    wire_model_id: "deepseek-v4-flash".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(99.0),
                        output: Some(199.0),
                        cache_read: Some(9.0),
                        cache_write: None,
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: "other-endpoint".to_string(),
                        fetched_at,
                    },
                    ..Default::default()
                }],
            },
            crate::provider_lake::LiveSource::PerProvider,
        );

        let usage = million_input_usage();
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            ProviderKind::OpencodeZen.provider().default_base_url(),
        );
        let audit = audit_turn_cost_for_provider_on_endpoint_at(
            ProviderKind::OpencodeZen,
            "deepseek-v4-flash",
            Some(&fingerprint),
            &usage,
            now,
        );
        crate::provider_lake::clear_live_snapshot();

        assert!(audit.is_priced(), "{audit:?}");
        assert_eq!(audit.provenance, Some(PricingProvenance::ProviderDocs));
        assert!(
            audit.live_pricing_defect.is_some(),
            "unverified provider-live must receipt a defect: {audit:?}"
        );
        let estimate = audit.estimate.expect("priced");
        assert!((estimate.usd - 0.14).abs() < 1e-12, "{}", estimate.usd);
    }

    #[test]
    fn verified_provider_live_rates_win_over_bundled_docs() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("timestamp");
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::descriptors::defaults::DEFAULT_FIREWORKS_BASE_URL,
        );
        crate::provider_lake::set_live_snapshot(
            codewhale_config::catalog::CatalogSnapshot {
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "fireworks".to_string(),
                    wire_model_id: "accounts/fireworks/models/kimi-k3".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(9.0),
                        output: Some(18.0),
                        cache_read: Some(1.0),
                        cache_write: None,
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: fingerprint.clone(),
                        fetched_at,
                    },
                    ..Default::default()
                }],
            },
            crate::provider_lake::LiveSource::PerProvider,
        );

        let usage = million_input_usage();
        let audit = official_route_audit(
            ProviderKind::Fireworks,
            "accounts/fireworks/models/kimi-k3",
            &usage,
        );
        crate::provider_lake::clear_live_snapshot();

        assert!(audit.is_priced(), "{audit:?}");
        assert_eq!(audit.provenance, Some(PricingProvenance::ProviderLive));
        assert_eq!(audit.live_pricing_defect, None);
        let estimate = audit.estimate.expect("priced");
        assert!(
            (estimate.usd - 9.0).abs() < 1e-12,
            "verified live must win: {}",
            estimate.usd
        );
    }

    #[test]
    fn future_effective_provider_live_rate_is_not_treated_as_age_zero() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let dispatched_at = Utc::now();
        let future_fetched_at = u64::try_from(dispatched_at.timestamp())
            .expect("timestamp")
            .saturating_add(1);
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::descriptors::defaults::DEFAULT_FIREWORKS_BASE_URL,
        );
        crate::provider_lake::set_live_snapshot(
            codewhale_config::catalog::CatalogSnapshot {
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "fireworks".to_string(),
                    wire_model_id: "accounts/fireworks/models/future-price-only".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(9.0),
                        output: Some(18.0),
                        cache_read: Some(1.0),
                        cache_write: None,
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: fingerprint.clone(),
                        fetched_at: future_fetched_at,
                    },
                    ..Default::default()
                }],
            },
            crate::provider_lake::LiveSource::PerProvider,
        );

        let audit = audit_turn_cost_for_provider_on_endpoint_at(
            ProviderKind::Fireworks,
            "accounts/fireworks/models/future-price-only",
            Some(&fingerprint),
            &million_input_usage(),
            dispatched_at,
        );
        crate::provider_lake::clear_live_snapshot();

        assert!(
            !audit.is_priced(),
            "future price must fail closed: {audit:?}"
        );
        assert_eq!(
            audit.unpriced_reason,
            Some(UnpricedReason::UnverifiedLivePricing)
        );
    }

    /// The fixture deliberately stamps `Live` rather than the `ModelsDevLive`
    /// the refresh now emits: this pins the *second*, independent check — the
    /// live partition the row sits in — which is what decides the price basis
    /// of a row mislabelled by an older publisher or a stale on-disk cache. Do
    /// not "correct" the source here; that would delete this belt's only
    /// coverage.
    #[test]
    fn models_dev_partition_live_rows_price_the_live_catalog_rate() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("timestamp");
        crate::provider_lake::set_live_snapshot(
            codewhale_config::catalog::CatalogSnapshot {
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "openai".to_string(),
                    wire_model_id: "gpt-5.5".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(99.0),
                        output: Some(199.0),
                        cache_read: Some(9.0),
                        cache_write: None,
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: "models-dev-capabilities".to_string(),
                        fetched_at,
                    },
                    ..Default::default()
                }],
            },
            crate::provider_lake::LiveSource::ModelsDev,
        );

        // Stay under the 272K long-context surcharge so this asserts the
        // catalog source, not the unrepresented-tier guard.
        let usage = Usage {
            input_tokens: 10_000,
            output_tokens: 0,
            ..Usage::default()
        };
        let audit = official_route_audit(ProviderKind::Openai, "gpt-5.5", &usage);
        crate::provider_lake::clear_live_snapshot();

        assert!(audit.is_priced(), "{audit:?}");
        assert_eq!(audit.provenance, Some(PricingProvenance::ModelsDevLive));
        assert_eq!(audit.live_pricing_defect, None);
        let estimate = audit.estimate.expect("priced");
        assert!(
            (estimate.usd - 0.99).abs() < 1e-12,
            "the models.dev live rate must price the estimate: {}",
            estimate.usd
        );
    }

    /// Concentrate publishes different upstream rates and can fail over even
    /// when a provider/model prefix is requested, so a requested model's own
    /// published rate is never inherited: without a verified scoped offering
    /// the route reports the explicit routing-dependent reason instead of
    /// dollars (#5976).
    #[test]
    fn concentrate_without_verified_scoped_pricing_reports_routing_dependent() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let usage = million_input_usage();

        // deepseek-v4-pro is the model owner's own hand-priced row; a
        // Concentrate request for it must not inherit that rate.
        let audit = official_route_audit(ProviderKind::Concentrate, "deepseek-v4-pro", &usage);
        assert!(!audit.is_priced(), "{audit:?}");
        assert_eq!(
            audit.unpriced_reason,
            Some(UnpricedReason::RoutingDependentPrice),
            "{audit:?}"
        );
        assert!(!has_pricing_for_provider(
            ProviderKind::Concentrate,
            "deepseek-v4-pro"
        ));
    }

    /// A fresh per-provider `/models` row fetched from the exact Concentrate
    /// endpoint is the scoped offering that *is* authoritative: with the
    /// endpoint fingerprint it prices at the scoped rate; without it the same
    /// row degrades to an unverified-live receipt rather than billing.
    #[test]
    fn concentrate_scoped_offering_prices_only_with_endpoint_provenance() {
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("timestamp");
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::descriptors::defaults::DEFAULT_CONCENTRATE_BASE_URL,
        );
        crate::provider_lake::set_live_snapshot(
            codewhale_config::catalog::CatalogSnapshot {
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "concentrate".to_string(),
                    wire_model_id: "deepseek-v4-pro".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(0.5),
                        output: Some(1.5),
                        cache_read: None,
                        cache_write: None,
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: fingerprint.clone(),
                        fetched_at,
                    },
                    ..Default::default()
                }],
            },
            crate::provider_lake::LiveSource::PerProvider,
        );

        let usage = million_input_usage();
        let surface = billing_surface_for_route(
            ProviderKind::Concentrate,
            Some(codewhale_config::descriptors::defaults::DEFAULT_CONCENTRATE_BASE_URL),
        );
        let scoped = audit_turn_cost_for_route_on_endpoint_at(
            ProviderKind::Concentrate,
            "deepseek-v4-pro",
            surface,
            Some(&fingerprint),
            &usage,
            now,
        );
        let unproven = audit_turn_cost_for_route_on_endpoint_at(
            ProviderKind::Concentrate,
            "deepseek-v4-pro",
            surface,
            None,
            &usage,
            now,
        );
        crate::provider_lake::clear_live_snapshot();

        assert!(scoped.is_priced(), "{scoped:?}");
        assert_eq!(scoped.provenance, Some(PricingProvenance::ProviderLive));
        let estimate = scoped.estimate.expect("priced");
        assert!(
            (estimate.usd - 0.5).abs() < 1e-12,
            "scoped Concentrate rate must govern: {}",
            estimate.usd
        );

        assert!(!unproven.is_priced(), "{unproven:?}");
        assert_eq!(
            unproven.unpriced_reason,
            Some(UnpricedReason::UnverifiedLivePricing),
            "{unproven:?}"
        );
    }

    // ── BalanceResponse / BalanceInfo ──────────────────────────────

    #[test]
    fn balance_response_deserializes_from_json() {
        let json = r#"{
            "is_available": true,
            "balance_infos": [
                {
                    "currency": "CNY",
                    "total_balance": "123.45",
                    "topped_up_balance": "100.00",
                    "granted_balance": "23.45"
                }
            ]
        }"#;
        let resp: BalanceResponse = serde_json::from_str(json).expect("valid JSON");
        assert!(resp.is_available);
        assert_eq!(resp.balance_infos.len(), 1);
        let info = &resp.balance_infos[0];
        assert_eq!(info.currency, "CNY");
        assert_eq!(info.total_balance, "123.45");
        assert_eq!(info.topped_up_balance, "100.00");
        assert_eq!(info.granted_balance, "23.45");
    }

    #[test]
    fn balance_response_defaults_empty_balance_infos_when_unavailable() {
        let json = r#"{"is_available": false, "balance_infos": []}"#;
        let resp: BalanceResponse = serde_json::from_str(json).expect("valid JSON");
        assert!(!resp.is_available);
        assert!(resp.balance_infos.is_empty());
    }

    #[test]
    fn balance_response_empty_list_is_valid() {
        let json = r#"{"is_available": true, "balance_infos": []}"#;
        let resp: BalanceResponse = serde_json::from_str(json).expect("valid JSON");
        assert!(resp.is_available);
        assert!(resp.balance_infos.is_empty());
    }

    #[test]
    fn balance_info_chip_label_uses_currency_prefix() {
        let cny = BalanceInfo {
            currency: "CNY".to_string(),
            total_balance: "123.45".to_string(),
            ..BalanceInfo::default()
        };
        assert_eq!(cny.chip_label().as_deref(), Some("¥123.45"));
        let usd = BalanceInfo {
            currency: "USD".to_string(),
            total_balance: "12.50".to_string(),
            ..BalanceInfo::default()
        };
        assert_eq!(usd.chip_label().as_deref(), Some("$12.50"));
        assert_eq!(
            usd.report("OpenRouter"),
            "OpenRouter account balance: $12.50"
        );
        let deepseek = BalanceInfo {
            currency: "CNY".to_string(),
            total_balance: "123.45".to_string(),
            topped_up_balance: "100.00".to_string(),
            granted_balance: "23.45".to_string(),
        };
        assert_eq!(
            deepseek.report("DeepSeek"),
            "DeepSeek account balance: ¥123.45 (topped up 100.00, granted 23.45)"
        );
    }

    struct CloudAuditReset;
    impl Drop for CloudAuditReset {
        fn drop(&mut self) {
            codewhale_config::cloud_facts::overlay::clear();
            crate::provider_lake::clear_live_snapshot();
            crate::provider_catalog_live::reset_cache_for_test();
        }
    }

    fn publish_audit_facts(
        channel: &str,
        version: u64,
        models: Vec<codewhale_config::cloud_facts::ModelFact>,
        now: u64,
    ) {
        use codewhale_config::cloud_facts::{
            CloudFactsState, CloudFactsStatus, FactsOrigin, ScopedFacts, overlay,
        };
        let ticket = overlay::configure(true, channel).unwrap();
        assert!(overlay::publish(
            &ticket,
            Some(ScopedFacts {
                channel: channel.into(),
                facts_version: version,
                key_id: "cwf-test-only".into(),
                valid_until: Some(now + 600),
                models,
                ..Default::default()
            }),
            CloudFactsStatus {
                state: CloudFactsState::Verified {
                    channel: channel.into(),
                    facts_version: version,
                    key_id: "cwf-test-only".into(),
                    fetched_at: now,
                    origin: FactsOrigin::LocalFile,
                    stale: false,
                    patches: 1,
                    defaults: 0,
                    announcements: 0
                },
                ..Default::default()
            }
        ));
    }

    fn audit_price_patch(
        provider: ProviderKind,
        model: &str,
        input: f64,
    ) -> codewhale_config::cloud_facts::ModelFact {
        use codewhale_config::cloud_facts::{ModelFact, PricingFact};
        ModelFact {
            provider: provider.as_str().into(),
            id: model.into(),
            context_window: Some(1_000_000),
            pricing: Some(PricingFact {
                input_per_m: Some(input),
                output_per_m: Some(2.0),
                cache_read_per_m: Some(0.1),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn cloud_prices_require_dispatch_quotes_and_never_reprice_old_turns() {
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _enabled = crate::test_support::EnvVarGuard::remove("CODEWHALE_DISABLE_CLOUD_FACTS");
        let _reset = CloudAuditReset;
        codewhale_config::cloud_facts::overlay::clear();
        crate::provider_lake::clear_live_snapshot();
        crate::provider_catalog_live::reset_cache_for_test();
        let provider = ProviderKind::Openai;
        let model = "cloud-audit-fixture";
        let base = provider.provider().default_base_url();
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(base);
        let now = Utc::now();
        let at = now.timestamp() as u64;
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };
        let audit = |quote| {
            audit_turn_cost_for_route_on_endpoint_for_identity_at(
                provider,
                Some("openai"),
                model,
                billing_surface_for_route(provider, Some(base)),
                Some(&fingerprint),
                quote,
                &usage,
                now,
            )
        };
        let before = audit(None);
        publish_audit_facts(
            "pricing-retro-test",
            1,
            vec![audit_price_patch(provider, model, 1.0)],
            at,
        );
        assert_eq!(
            audit(None),
            before,
            "a newly installed catalog cannot price an old dispatch without a quote"
        );
        let estimate = audit_turn_cost_for_provider_on_endpoint_at(
            provider,
            model,
            Some(&fingerprint),
            &usage,
            now,
        );
        assert_eq!(estimate.provenance, Some(PricingProvenance::CloudFacts));
        assert_eq!(estimate.estimate.unwrap().usd, 1.0);
        let before_fetch = audit_turn_cost_for_provider_on_endpoint_at(
            provider,
            model,
            Some(&fingerprint),
            &usage,
            now - chrono::Duration::seconds(1),
        );
        assert_eq!(
            before_fetch.unpriced_reason,
            Some(UnpricedReason::UnverifiedLivePricing)
        );
        let quote = crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
            provider, "openai", model, base, at,
        )
        .unwrap();
        assert_eq!(audit(Some(&quote)).estimate.unwrap().usd, 1.0);
        publish_audit_facts(
            "pricing-retro-test",
            2,
            vec![audit_price_patch(provider, model, 9.0)],
            at,
        );
        assert_eq!(audit(Some(&quote)).estimate.unwrap().usd, 1.0);
        assert_eq!(audit(None), before);
        codewhale_config::cloud_facts::overlay::clear();
        assert_eq!(audit(Some(&quote)).estimate.unwrap().usd, 1.0);
    }

    #[test]
    fn cloud_quote_keeps_provider_hand_tiers_and_recorded_time_authority() {
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _enabled = crate::test_support::EnvVarGuard::remove("CODEWHALE_DISABLE_CLOUD_FACTS");
        let _reset = CloudAuditReset;
        codewhale_config::cloud_facts::overlay::clear();
        crate::provider_lake::clear_live_snapshot();
        crate::provider_catalog_live::reset_cache_for_test();
        let now = Utc::now();
        let at = now.timestamp() as u64;
        let usage = Usage {
            input_tokens: 600_000,
            output_tokens: 1000,
            ..Default::default()
        };
        let routes = [
            (ProviderKind::Deepseek, "deepseek-v4-flash"),
            (ProviderKind::Anthropic, "claude-sonnet-5"),
            (ProviderKind::Minimax, "MiniMax-M3"),
            (ProviderKind::MinimaxAnthropic, "MiniMax-M3"),
            (ProviderKind::Xai, "grok-4.6"),
        ];
        let before: Vec<_> = routes
            .iter()
            .map(|(provider, model)| audit_turn_cost_for_provider_at(*provider, model, &usage, now))
            .collect();
        assert!(before.iter().all(TurnCostAudit::is_priced));
        publish_audit_facts(
            "pricing-hand-test",
            1,
            routes
                .iter()
                .map(|(provider, model)| audit_price_patch(*provider, model, 999.0))
                .collect(),
            at,
        );
        for ((provider, model), expected) in routes.into_iter().zip(before) {
            let base = provider.provider().default_base_url();
            let quote = crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
                provider,
                provider.as_str(),
                model,
                base,
                at,
            )
            .unwrap_or_else(|| {
                panic!(
                    "cloud quote missing for {provider:?} identity={} model={model}",
                    provider.as_str()
                )
            });
            // MiniMax shares these endpoints with subscription keys. Only a
            // saved PAYG mode supplies this receipt (as the client does); a
            // signed price cannot itself establish the account billing mode.
            let surface = if matches!(
                provider,
                ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
            ) {
                let unknown = audit_turn_cost_for_route_on_endpoint_for_identity_at(
                    provider,
                    Some(provider.as_str()),
                    model,
                    billing_surface_for_route(provider, Some(base)),
                    Some(&codewhale_config::catalog::base_url_fingerprint(base)),
                    Some(&quote),
                    &usage,
                    now,
                );
                assert_eq!(
                    unknown.unpriced_reason,
                    Some(UnpricedReason::UnknownBillingBasis)
                );
                Some(MINIMAX_PAYG_BILLING_SURFACE)
            } else {
                billing_surface_for_route(provider, Some(base))
            };
            let actual = audit_turn_cost_for_route_on_endpoint_for_identity_at(
                provider,
                Some(provider.as_str()),
                model,
                surface,
                Some(&codewhale_config::catalog::base_url_fingerprint(base)),
                Some(&quote),
                &usage,
                now,
            );
            assert_eq!(actual, expected, "{provider:?}/{model}");
        }
    }
}
