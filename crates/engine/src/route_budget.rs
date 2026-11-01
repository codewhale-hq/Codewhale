use codewhale_config::route::RouteLimits;

use crate::provider_lake::provider_capability;
use crate::reasoning_preference::ReasoningEffort;
use codewhale_config::ProviderKind;
use codewhale_models::{DEFAULT_COMPACTION_TOKEN_THRESHOLD, context_window_for_model};
use codewhale_runtime::context_budget::ContextBudget;

/// Safe ordinary API request cap across provider routes.
const API_MAX_OUTPUT_TOKENS: u32 = 65_536;
pub const MIN_REQUEST_OUTPUT_TOKENS: u64 = 128;

/// Automatic request cap on a first-party DeepSeek route at max reasoning
/// effort, where DeepSeek documents 128K instead of the 64K thinking-mode
/// default (allowed range 1..=393216).
const DEEPSEEK_MAX_EFFORT_MAX_OUTPUT_TOKENS: u32 = 131_072;

/// Preserve only route limits that came from a concrete offering.
#[must_use]
pub fn known_route_limits(limits: RouteLimits) -> Option<RouteLimits> {
    limits.has_known_limit().then_some(limits)
}

/// Whether this exact transport can represent an explicit output allowance.
/// Codex OAuth Responses rejects the field; other supported dialects carry it.
pub fn route_supports_output_token_limit(
    provider: ProviderKind,
    protocol: codewhale_config::route::RequestProtocol,
) -> bool {
    !(provider == ProviderKind::OpenaiCodex
        && protocol == codewhale_config::route::RequestProtocol::Responses)
}

pub fn effective_max_output_tokens_for_turn(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    allowance: Option<std::num::NonZeroU32>,
    reasoning_effort: Option<&str>,
) -> u32 {
    effective_max_output_tokens_for_turn_with_window(
        provider,
        model,
        route_limits,
        None,
        allowance,
        reasoning_effort,
    )
}

pub fn effective_max_output_tokens_for_turn_with_window(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    effective_window: Option<u32>,
    allowance: Option<std::num::NonZeroU32>,
    reasoning_effort: Option<&str>,
) -> u32 {
    let ceiling = effective_max_output_tokens_for_route_with_cap(
        provider,
        model,
        route_limits,
        effective_window,
        automatic_max_output_tokens(provider, reasoning_effort),
    );
    allowance.map_or(ceiling, |allowance| ceiling.min(allowance.get()))
}

fn automatic_max_output_tokens(provider: ProviderKind, reasoning_effort: Option<&str>) -> u32 {
    let max_effort = reasoning_effort.is_some_and(|raw| {
        ReasoningEffort::parse_strict(raw)
            .is_ok_and(|effort| matches!(effort, ReasoningEffort::Max | ReasoningEffort::Ultra))
    });
    if provider == ProviderKind::Deepseek && max_effort {
        DEEPSEEK_MAX_EFFORT_MAX_OUTPUT_TOKENS
    } else {
        API_MAX_OUTPUT_TOKENS
    }
}

/// Context window for a resolved runtime route.
///
/// Route/offering facts win when known; otherwise this falls back to the
/// existing provider+model capability matrix so startup and custom/local
/// routes keep their previous conservative behavior.
#[must_use]
pub fn route_context_window_tokens(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
) -> u32 {
    route_limits
        .and_then(|limits| limits.context_tokens)
        .and_then(|tokens| u32::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)
        .unwrap_or_else(|| provider_capability(provider, model).context_window)
}

/// Share of the route's context window (in estimated characters) one tool
/// result may occupy inline.
const TOOL_OUTPUT_INLINE_WINDOW_PERCENT: u64 = 3;
/// Inline ceiling for one tool result when the route is unknown, and the cap
/// for every route unless the operator opted into a larger budget.
const TOOL_OUTPUT_INLINE_MAX_CHARS: usize = 100_000;
/// Hard ceiling on an operator-raised inline tool-result budget (#5367).
const TOOL_OUTPUT_INLINE_OPT_IN_MAX_CHARS: usize = 2 * 1024 * 1024;

/// The one budget for how much of a tool result the model sees inline (#6508).
///
/// Every tool result is measured against this number: 3% of the route's
/// context window (at about 4 characters per token), capped at 100,000
/// characters, or the full cap when the window is unknown. An opt-in
/// `tool_result_max_bytes` workshop setting may raise it, up to 2 MiB.
/// Anything past it is cut only after the raw output has been saved as a
/// session artifact that `retrieve_tool_result` can read back.
#[must_use]
pub fn route_inline_char_budget(window_tokens: Option<u32>) -> usize {
    route_inline_char_budget_with_raise(
        window_tokens,
        codewhale_runtime::active_workshop::active_tool_result_max_bytes(),
    )
}

#[must_use]
fn route_inline_char_budget_with_raise(window_tokens: Option<u32>, raise: Option<usize>) -> usize {
    let base = window_tokens
        .filter(|tokens| *tokens > 0)
        .map(|tokens| {
            let chars = u64::from(tokens)
                .saturating_mul(4)
                .saturating_mul(TOOL_OUTPUT_INLINE_WINDOW_PERCENT)
                / 100;
            usize::try_from(chars).unwrap_or(TOOL_OUTPUT_INLINE_MAX_CHARS)
        })
        .unwrap_or(TOOL_OUTPUT_INLINE_MAX_CHARS)
        .clamp(1, TOOL_OUTPUT_INLINE_MAX_CHARS);
    match raise {
        Some(bytes) if bytes > base => bytes.min(TOOL_OUTPUT_INLINE_OPT_IN_MAX_CHARS),
        _ => base,
    }
}

/// [`route_inline_char_budget`] for a resolved provider/model route.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn route_inline_char_budget_for_route(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
) -> usize {
    route_inline_char_budget(Some(route_context_window_tokens(
        provider,
        model,
        route_limits,
    )))
}

/// Provider/offering output cap, when the resolved route reports one.
#[must_use]
pub fn route_output_limit_tokens(route_limits: Option<RouteLimits>) -> Option<u32> {
    route_limits
        .and_then(|limits| limits.output_tokens)
        .and_then(|tokens| u32::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)
}

/// Provider/offering input cap, when the resolved route reports one.
#[must_use]
pub fn route_input_limit_tokens(route_limits: Option<RouteLimits>) -> Option<u32> {
    route_limits
        .and_then(|limits| limits.input_tokens)
        .and_then(|tokens| u32::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)
}

/// Explicit operator request cap, when configured.
///
/// Keep this separate from catalogue/default resolution: a published maximum
/// is a ceiling, while these environment variables are an actual request from
/// the operator. Route/window validation still clamps the value before it is
/// sent.
#[must_use]
fn explicit_max_output_tokens_override() -> Option<u32> {
    match std::env::var("CODEWHALE_MAX_OUTPUT_TOKENS") {
        Ok(raw) if !raw.trim().is_empty() => {
            // A non-blank canonical value is authoritative. Invalid/zero
            // values deliberately fall back to the safe automatic default;
            // they must not silently activate a stale legacy setting.
            return raw.trim().parse::<u32>().ok().filter(|tokens| *tokens > 0);
        }
        Ok(_) | Err(_) => {}
    }
    std::env::var("DEEPSEEK_MAX_OUTPUT_TOKENS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|tokens| *tokens > 0)
}

/// Effective `max_tokens` for a model before provider/route caps are applied.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn effective_max_output_tokens(model: &str) -> u32 {
    effective_max_output_tokens_with_cap(model, API_MAX_OUTPUT_TOKENS)
}

fn effective_max_output_tokens_with_cap(model: &str, automatic_cap: u32) -> u32 {
    if let Some(tokens) = explicit_max_output_tokens_override() {
        return tokens;
    }

    // A documented catalogue value is a capability ceiling, not necessarily a
    // sensible default request size. In particular, DeepSeek V4 advertises a
    // 384K maximum. Treating that maximum as the default made Codewhale ask a
    // 262K/327K self-hosted route for almost its whole context as output before
    // it had counted a single input token (#5516/#5518). Keep documented
    // ceilings through the normal compatibility intersection, but automatic
    // requests start at the ordinary cap — 64K, or DeepSeek's documented 128K
    // at max effort — unless the operator explicitly overrides it. A maximum
    // describes what a provider may allow, not what every response should
    // reserve by default.
    //
    // Provenance for the ceiling (deepseek-v4-flash/pro: 384_000 output):
    // - models_dev.bundled.json documents limit.output = 384000.
    // - The DS4 provider contract corroborates 384K
    //   (crates/config/src/model_reference.rs pins max_output 384_000 / "384K").
    // - Official DeepSeek API docs publish the output allowance: 64K by
    //   default in thinking mode, 128K at `reasoning_effort: max`, in the
    //   range 1..=393216 (#5373).
    if let Some(documented) = codewhale_models::max_output_tokens_for_model(model) {
        return documented.min(automatic_cap);
    }

    let window = context_window_for_model(model).unwrap_or(128_000);
    (window / 2).min(automatic_cap)
}

/// Automatic allowance when an exact remote model has no output metadata.
/// This is policy, not a discovered provider limit. Reasoning and tool arguments
/// share this allowance; an 8K fallback truncated ordinary file writes after
/// reasoning consumed most of the response. Known route limits still constrain
/// requests, and an explicit operator setting may replace this fallback.
const UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS: u32 = API_MAX_OUTPUT_TOKENS;

/// Assumed output ceiling for an Anthropic-family model the catalogue does
/// not describe (#5440). The 64K Messages floor is real, but applying it to
/// an unknown model is an assumption about that model, not a documented
/// fact, so it clamps under an `unverified` label.
const ANTHROPIC_UNKNOWN_MAX_OUTPUT_TOKENS: u32 = 64_000;

/// Assumed output ceiling for the ChatGPT/Codex OAuth route, which publishes
/// no output ceiling of its own (#5440). Same clamp as the long-standing 4K
/// policy — relabeled, not revalued.
const CODEX_OAUTH_MAX_OUTPUT_TOKENS: u32 = 4_096;

/// Why a route's compatibility output ceiling has the value it does.
///
/// Carried so a clamp is always attributable: "unknown" is only allowed to
/// mean "no clamp" when a route *truthfully publishes no ceiling*, never when
/// the catalogue simply has no row for the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCeilingSource {
    /// The static catalogue publishes an exact/conservative ceiling.
    Documented(u32),
    /// The route is known to publish no output maximum we can stand behind
    /// (Kimi Code membership ids, operator-owned self-hosted engines). Unknown
    /// stays unknown and nothing is clamped.
    RouteDeclaredUnknown,
    /// The catalogue has no row for this model. Fail closed to a conservative
    /// ceiling rather than treating absence as permission.
    Uncatalogued(u32),
    /// The route publishes no ceiling we can stand behind, but a defensible
    /// floor is still applied: an Anthropic-family model the catalogue does
    /// not describe (64K Messages floor), the Codex OAuth route (4K
    /// policy), and a model-only catalogue row reached through a custom
    /// endpoint, whose shared wire spelling does not establish the catalogued
    /// model. Clamping trades late provider failure for early truncation;
    /// lying about why is not part of that trade, so receipts and pickers
    /// must render this as an assumption, never as "documented" (#5440).
    Unverified(u32),
}

impl OutputCeilingSource {
    /// The ceiling to intersect a requested cap with, if any.
    #[must_use]
    pub const fn clamp_tokens(self) -> Option<u32> {
        match self {
            Self::Documented(tokens) | Self::Uncatalogued(tokens) | Self::Unverified(tokens) => {
                Some(tokens)
            }
            Self::RouteDeclaredUnknown => None,
        }
    }

    /// Stable provenance label, surfaced in exec stream metadata so a wrong
    /// ceiling is visible in a receipt rather than requiring packet capture.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Documented(_) => "documented",
            Self::Uncatalogued(_) => "uncatalogued",
            Self::RouteDeclaredUnknown => "route-declared",
            Self::Unverified(_) => "unverified",
        }
    }
}

/// Whether an absent compatibility ceiling is a *declared* unknown for this
/// route, rather than a gap in the catalogue.
///
/// Deliberately an allowlist. Everything not named here is uncatalogued and
/// gets the conservative ceiling.
#[must_use]
fn route_declares_unknown_output_ceiling(provider: ProviderKind, model: &str) -> bool {
    match provider {
        // Operator-owned engines: the local server, not this process, owns the
        // output ceiling, and it is routinely far above any catalogue row.
        ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm => true,
        // Kimi Code membership ids publish their limits in the membership
        // catalog rather than the static model catalogue.
        ProviderKind::Moonshot => codewhale_runtime::config::is_kimi_code_membership_model(model),
        _ => false,
    }
}

/// Resolve the compatibility output ceiling for a route, with its provenance.
#[must_use]
pub fn output_ceiling_source(provider: ProviderKind, model: &str) -> OutputCeilingSource {
    // #5440: two routes clamp to a number the route itself never documented.
    // The clamps stay (see `OutputCeilingSource::Unverified`); the labels must
    // not borrow the documented rung's authority.
    if provider == ProviderKind::OpenaiCodex {
        return OutputCeilingSource::Unverified(CODEX_OAUTH_MAX_OUTPUT_TOKENS);
    }
    if matches!(
        provider,
        ProviderKind::Anthropic | ProviderKind::MinimaxAnthropic | ProviderKind::Openmodel
    ) && codewhale_models::max_output_tokens_for_model(model).is_none()
    {
        return OutputCeilingSource::Unverified(ANTHROPIC_UNKNOWN_MAX_OUTPUT_TOKENS);
    }
    provider_capability(provider, model).max_output.map_or_else(
        || {
            if route_declares_unknown_output_ceiling(provider, model) {
                OutputCeilingSource::RouteDeclaredUnknown
            } else {
                OutputCeilingSource::Uncatalogued(UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS)
            }
        },
        |tokens| {
            if provider == ProviderKind::Custom {
                OutputCeilingSource::Unverified(tokens)
            } else {
                OutputCeilingSource::Documented(tokens)
            }
        },
    )
}

/// Effective request output cap for a fully resolved provider/model route.
#[must_use]
pub fn effective_max_output_tokens_for_route(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
) -> u32 {
    effective_max_output_tokens_for_route_with_cap(
        provider,
        model,
        route_limits,
        None,
        API_MAX_OUTPUT_TOKENS,
    )
}

fn effective_max_output_tokens_for_route_with_cap(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    effective_window: Option<u32>,
    automatic_cap: u32,
) -> u32 {
    let window = effective_window
        .filter(|tokens| *tokens > 0)
        .unwrap_or_else(|| route_context_window_tokens(provider, model, route_limits));
    let compatibility_source = output_ceiling_source(provider, model);
    let compatibility_cap = compatibility_source.clamp_tokens();
    let route_cap = route_output_limit_tokens(route_limits);
    // With a known route window and no published output limit, reserve a
    // conservative part of that window. The model-only fallback reserved 64K even
    // for a configured 32K Ollama route, leaving just 1K for input (#5820).
    // The same squeeze hit the capability *fallback* window: an unknown local
    // Ollama tag resolves to an 8K window with no route limits, the model-only
    // 64K request clamped to 6K, and the input budget collapsed to 1K — every
    // turn tripped emergency compaction before its first request (#6540). So a
    // model-only request larger than half of whatever window is in force also
    // yields to the window-relative reservation.
    // Explicit requests and documented ceilings retain their existing rules.
    let model_only_cap = effective_max_output_tokens_with_cap(model, automatic_cap);
    let window_known = route_limits
        .and_then(|limits| limits.context_tokens)
        .is_some_and(|tokens| (1..=u64::from(u32::MAX)).contains(&tokens));
    let requested_cap = if explicit_max_output_tokens_override().is_none()
        && codewhale_models::max_output_tokens_for_model(model).is_none()
        && route_cap.is_none()
        && matches!(
            compatibility_source,
            OutputCeilingSource::RouteDeclaredUnknown | OutputCeilingSource::Uncatalogued(_)
        )
        && (window_known || model_only_cap > window / 2)
    {
        (window / 4).clamp(1, UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS)
    } else {
        model_only_cap
    };
    // Unknown means unknown only where a route *declares* it: membership ids
    // such as the `kimi-for-coding` family, and operator-owned self-hosted
    // engines. For those there is nothing to clamp against and the requested
    // cap stands. A model the catalogue simply has no row for is not the same
    // fact — absence keeps a labeled automatic allowance
    // (see `output_ceiling_source`). A concrete route/offering maximum is the
    // missing evidence for that exact route and may replace only the generic
    // uncatalogued guess or a custom endpoint's model-only assumption. An
    // explicit request may also rise past a compatibility cap to a maximum
    // the operator declared for this exact model and endpoint. Otherwise known
    // compatibility caps stay authoritative and are still intersected with
    // any route maximum.
    let custom_endpoint_assumption = provider == ProviderKind::Custom
        && matches!(compatibility_source, OutputCeilingSource::Unverified(_));
    let declared_route_override = explicit_max_output_tokens_override().is_some()
        && route_limits.is_some_and(|limits| limits.output_declared);
    let cap = match (compatibility_source, route_cap) {
        // A concrete route/offering maximum is evidence about this exact
        // route. It therefore outranks the generic fallback that exists only
        // because the static catalogue has no row for the wire id. With no
        // route fact the conservative guess still applies, and the route fact
        // can never raise the caller's requested cap.
        (OutputCeilingSource::Uncatalogued(_), Some(route_cap)) => requested_cap.min(route_cap),
        (_, Some(route_cap)) if custom_endpoint_assumption || declared_route_override => {
            requested_cap.min(route_cap)
        }
        (OutputCeilingSource::Uncatalogued(_), None)
            if explicit_max_output_tokens_override().is_some() =>
        {
            requested_cap
        }
        _ => {
            let cap = compatibility_cap.map_or(requested_cap, |compat| requested_cap.min(compat));
            route_cap.map_or(cap, |route_cap| cap.min(route_cap))
        }
    };
    // Clamp against the effective route window even when it came from the
    // capability fallback rather than an explicit offering. This keeps a
    // suffix/config/catalog-derived small window from ever receiving a request
    // cap larger than the window itself.
    u32::try_from(ContextBudget::new(u64::from(window), 0, u64::from(cap)).output_cap_tokens)
        .unwrap_or(cap)
        .max(1)
}

/// Share of one output allowance a single review pass must keep for visible
/// text, as a percentage.
///
/// A reasoning route shares one `max_tokens` allowance between hidden
/// reasoning and visible text, and Codewhale has no wire-level separation
/// (`thinking.budget_tokens` is not plumbed), so "reserving" means two things
/// together: state the reserve, and cap the reasoning level that may consume
/// it (`review::bounded_review_reasoning_effort`).
///
/// The share is sized from the model's reasoning behaviour rather than a flat
/// constant:
///
/// * `Some(false)` — nothing to reserve; the whole allowance is visible text.
/// * `Some(true)` in the summarized-reasoning families whose reasoning is
///   counted as ordinary output tokens
///   ([`codewhale_models::model_is_openai_reasoning_family`]) — half. These are
///   the models observed consuming an entire 64K allowance on reasoning and
///   returning zero visible text with stop reason `length` (#6285).
/// * `Some(true)` otherwise, and `None` (no catalogue row) — a quarter. A
///   review pass needs only enough text for its structured findings, and an
///   unknown model is not evidence that it does not reason (#6032).
#[must_use]
pub fn review_visible_text_reserve_percent(model: &str) -> u32 {
    review_reserve_percent_for(
        codewhale_models::model_reasoning_capability(model),
        codewhale_models::model_is_openai_reasoning_family(model),
    )
}

/// Pure core of [`review_visible_text_reserve_percent`]: the mapping from a
/// model's reasoning classification to the reserved share. Split out so the
/// mapping is testable without the process-global model catalog.
fn review_reserve_percent_for(capability: Option<bool>, openai_reasoning_family: bool) -> u32 {
    match capability {
        // Nothing to reserve; the whole allowance is visible text.
        Some(false) => 0,
        Some(true) if openai_reasoning_family => 50,
        Some(true) | None => 25,
    }
}

/// Visible-text reserve in tokens for one review pass on this exact model and
/// resolved output allowance.
#[must_use]
pub fn review_visible_text_reserve_tokens(model: &str, allowance: u32) -> u32 {
    allowance.saturating_mul(review_visible_text_reserve_percent(model)) / 100
}

/// Output reservation used by the internal input budget for a route.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn route_output_reservation(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
) -> u32 {
    route_output_reservation_with_window(provider, model, route_limits, None)
}

pub fn route_output_reservation_with_window(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    effective_window: Option<u32>,
) -> u32 {
    // Use exactly the value that can reach the wire on every window size,
    // except on a route whose published output limit is more than half its
    // window: there the limit is the shared window itself, so a quarter of the
    // window (at most 64K) is reserved and each request's wire cap is sized
    // from what its input leaves (`request_output_cap_for_input`). The previous split
    // reserved 65K for a possible 325K wire request below 500K, then jumped to
    // an unrequested 262K reservation at 500K. Both directions made preflight
    // disagree with the actual request. Reasoning effort is a request control,
    // not separately metered non-wire output, so it does not justify a second
    // hidden context reservation.
    let window = effective_window
        .filter(|tokens| *tokens > 0)
        .unwrap_or_else(|| route_context_window_tokens(provider, model, route_limits));
    let ceiling = effective_max_output_tokens_for_route_with_cap(
        provider,
        model,
        route_limits,
        Some(window),
        API_MAX_OUTPUT_TOKENS,
    );
    if shares_context_window(route_limits, window) {
        ceiling.min((window / 4).clamp(1, UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS))
    } else {
        ceiling
    }
}

fn shares_context_window(route_limits: Option<RouteLimits>, window: u32) -> bool {
    route_limits
        .and_then(|limits| limits.context_tokens)
        .is_some_and(|tokens| (1..=u64::from(u32::MAX)).contains(&tokens))
        && explicit_max_output_tokens_override().is_none()
        && route_output_limit_tokens(route_limits).is_some_and(|cap| cap > window / 2)
}

pub fn route_admission_input_ceiling_with_window(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    effective_window: Option<u32>,
) -> Option<u64> {
    let window = effective_window
        .filter(|tokens| *tokens > 0)
        .unwrap_or_else(|| route_context_window_tokens(provider, model, route_limits));
    shares_context_window(route_limits, window).then(|| {
        ContextBudget::new_with_input_limit(
            u64::from(window),
            0,
            MIN_REQUEST_OUTPUT_TOKENS,
            route_input_limit_tokens(route_limits).map(u64::from),
        )
        .input_budget_ceiling
    })
}

#[must_use]
pub fn request_output_cap_for_input(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    ceiling: u32,
    estimated_input_tokens: u64,
) -> Option<u32> {
    request_output_cap_for_input_with_window(
        provider,
        model,
        route_limits,
        None,
        ceiling,
        estimated_input_tokens,
    )
}

pub fn request_output_cap_for_input_with_window(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    effective_window: Option<u32>,
    ceiling: u32,
    estimated_input_tokens: u64,
) -> Option<u32> {
    let window = effective_window
        .filter(|tokens| *tokens > 0)
        .unwrap_or_else(|| route_context_window_tokens(provider, model, route_limits));
    if !shares_context_window(route_limits, window) {
        return Some(ceiling);
    }
    let remaining = u64::from(window)
        .saturating_sub(estimated_input_tokens)
        .saturating_sub(codewhale_runtime::context_budget::CONTEXT_HEADROOM_TOKENS);
    u32::try_from(remaining.min(u64::from(ceiling)))
        .ok()
        .filter(|cap| *cap > 0)
}

#[must_use]
pub fn route_context_budget(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    input_tokens: usize,
) -> Option<ContextBudget> {
    route_context_budget_with_window(provider, model, route_limits, None, input_tokens)
}

pub fn route_context_budget_with_window(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    effective_window: Option<u32>,
    input_tokens: usize,
) -> Option<ContextBudget> {
    let window = effective_window
        .filter(|tokens| *tokens > 0)
        .unwrap_or_else(|| route_context_window_tokens(provider, model, route_limits));
    let output_cap =
        route_output_reservation_with_window(provider, model, route_limits, Some(window));
    Some(ContextBudget::new_with_input_limit(
        u64::from(window),
        u64::try_from(input_tokens).ok()?,
        u64::from(output_cap),
        route_input_limit_tokens(route_limits).map(u64::from),
    ))
}

#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn compaction_threshold_for_route_at_percent(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    percent: f64,
) -> usize {
    compaction_threshold_for_route_at_percent_with_window(
        provider,
        model,
        route_limits,
        None,
        percent,
    )
}

pub fn compaction_threshold_for_route_at_percent_with_window(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    effective_window: Option<u32>,
    percent: f64,
) -> usize {
    route_context_budget_with_window(provider, model, route_limits, effective_window, 0)
        .and_then(|budget| {
            usize::try_from(budget.compaction_trigger_for_percent(percent.clamp(10.0, 100.0))).ok()
        })
        .unwrap_or(DEFAULT_COMPACTION_TOKEN_THRESHOLD)
}

#[must_use]
pub fn auto_compact_default_for_route(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
) -> bool {
    // Every resolved route has either concrete offering limits or a
    // conservative provider/model fallback. Large windows need continuity too;
    // their size is not a reason to disable compaction entirely.
    route_context_window_tokens(provider, model, route_limits) > 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn route_inline_char_budget_is_three_percent_of_the_window_capped() {
        use super::route_inline_char_budget_with_raise as budget;
        assert_eq!(budget(Some(128_000), None), 15_360);
        assert_eq!(budget(Some(10_000), None), 1_200);
        assert_eq!(budget(None, None), 100_000);
        assert_eq!(budget(Some(0), None), 100_000);
        assert_eq!(budget(Some(1_000_000), None), 100_000);
        // An operator opt-in raises the budget, never lowers it, and stops at 2 MiB.
        assert_eq!(budget(Some(128_000), Some(80_000)), 80_000);
        assert_eq!(budget(Some(128_000), Some(1_000)), 15_360);
        assert_eq!(
            budget(Some(128_000), Some(64 * 1024 * 1024)),
            2 * 1024 * 1024
        );
    }

    use super::*;

    #[test]
    fn provider_regression_5820_small_unknown_windows_keep_room_for_input() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let model = "qwen2.5:7b";
        assert!(codewhale_models::max_output_tokens_for_model(model).is_none());
        for provider in [
            ProviderKind::Ollama,
            ProviderKind::Sglang,
            ProviderKind::Vllm,
            ProviderKind::Custom,
        ] {
            for (window, expected_output) in [
                (16_384, 4_096),
                (32_768, 8_192),
                (65_536, 16_384),
                (262_144, 65_536),
            ] {
                let limits = Some(RouteLimits {
                    context_tokens: Some(window),
                    ..RouteLimits::default()
                });
                let wire_cap = effective_max_output_tokens_for_route(provider, model, limits);
                let budget = route_context_budget(provider, model, limits, 6_225).unwrap();
                assert_eq!(wire_cap, expected_output, "{provider:?}, window={window}");
                assert_eq!(budget.output_cap_tokens, u64::from(wire_cap));
                assert_eq!(
                    budget.input_budget_ceiling,
                    window - u64::from(wire_cap) - 1_024
                );
                assert!(
                    budget.input_tokens < budget.input_budget_ceiling,
                    "{budget:?}"
                );
                assert!(!budget.should_compact(), "{budget:?}");
            }
        }
        let _explicit =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "16384");
        let limits = RouteLimits {
            context_tokens: Some(32_768),
            ..RouteLimits::default()
        };
        assert_eq!(
            effective_max_output_tokens_for_route(ProviderKind::Ollama, model, Some(limits)),
            16_384
        );
        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Ollama,
                model,
                Some(RouteLimits {
                    output_tokens: Some(4_096),
                    ..limits
                })
            ),
            4_096
        );
    }

    /// #6540: the runtime store's 15 failed compactions were all emergency
    /// passes on an unknown local Ollama tag (`qwen3:4b`) with no route
    /// limits: the capability fallback window (8K) minus a 6K output
    /// reservation left a ~1K input budget, so every first request of a turn
    /// tripped preflight recovery. The fallback window must keep the same
    /// input room a configured window of that size gets.
    #[test]
    fn provider_regression_6540_fallback_window_keeps_room_for_input() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let model = "qwen3:4b";
        assert!(codewhale_models::max_output_tokens_for_model(model).is_none());
        let window = route_context_window_tokens(ProviderKind::Ollama, model, None);
        assert_eq!(
            window, 8_192,
            "unknown local tags keep the conservative window"
        );

        let wire_cap = effective_max_output_tokens_for_route(ProviderKind::Ollama, model, None);
        assert_eq!(wire_cap, 2_048);
        let budget = route_context_budget(ProviderKind::Ollama, model, None, 0).unwrap();
        assert_eq!(budget.output_cap_tokens, u64::from(wire_cap));
        assert_eq!(budget.input_budget_ceiling, 8_192 - 2_048 - 1_024);
        // The recorded first-request estimates (~1.9K–3.9K) now fit.
        assert!(budget.input_budget_ceiling > 3_900, "{budget:?}");

        // Same answer as the explicitly configured 8K window (#5820).
        let configured = Some(RouteLimits {
            context_tokens: Some(8_192),
            ..RouteLimits::default()
        });
        assert_eq!(
            effective_max_output_tokens_for_route(ProviderKind::Ollama, model, configured),
            wire_cap
        );
    }

    /// Absence of a catalogue row is not evidence of a large ceiling. An
    /// unrecognized wire alias on a remote OpenAI-compatible route keeps the
    /// conservative compatibility ceiling, with an attributable source.
    #[test]
    fn uncatalogued_remote_model_keeps_a_conservative_ceiling() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let source = output_ceiling_source(ProviderKind::Openai, "totally-unknown-alias-v9");
        assert_eq!(
            source,
            OutputCeilingSource::Uncatalogued(UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS)
        );
        assert_eq!(
            source.clamp_tokens(),
            Some(UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS)
        );
        assert!(
            effective_max_output_tokens_for_route(
                ProviderKind::Openai,
                "totally-unknown-alias-v9",
                None
            ) <= UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS
        );
    }

    /// #5460: absence is not permission, but a positive output maximum on the
    /// resolved route is permission for that exact route. The concrete fact
    /// replaces only the catalogue-absence guess; it never raises the caller's
    /// requested cap, and an undeclared fact never raises a documented model
    /// ceiling.
    #[test]
    fn concrete_route_output_limit_outranks_uncatalogued_guess() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let model = "totally-unknown-alias-v9";

        assert_eq!(effective_max_output_tokens(model), 64_000);
        for provider in [ProviderKind::Openai, ProviderKind::Custom] {
            assert_eq!(
                output_ceiling_source(provider, model),
                OutputCeilingSource::Uncatalogued(UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS)
            );
            assert_eq!(
                effective_max_output_tokens_for_route(provider, model, None),
                64_000,
                "{provider:?}: no route fact must preserve the labeled automatic allowance"
            );
            for route_cap in [24_576, 64_000] {
                assert_eq!(
                    effective_max_output_tokens_for_route(
                        provider,
                        model,
                        Some(RouteLimits {
                            output_tokens: Some(route_cap),
                            ..RouteLimits::default()
                        }),
                    ),
                    u32::try_from(route_cap).unwrap(),
                    "{provider:?}: the exact route fact must replace the catalogue-absence guess"
                );
            }
            assert_eq!(
                effective_max_output_tokens_for_route(
                    provider,
                    model,
                    Some(RouteLimits {
                        output_tokens: Some(65_536),
                        ..RouteLimits::default()
                    }),
                ),
                64_000,
                "{provider:?}: a route fact must not raise the requested cap"
            );
        }

        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Moonshot,
                "kimi-k2.7-code",
                Some(RouteLimits {
                    output_tokens: Some(64_000),
                    ..RouteLimits::default()
                }),
            ),
            32_768,
            "a route fact must not raise a documented model ceiling"
        );
    }

    /// Routes that *declare* an unknown ceiling still avoid the clamp.
    #[test]
    fn route_declared_unknown_ceilings_are_not_clamped() {
        for (provider, model) in [
            (ProviderKind::Moonshot, "kimi-for-coding"),
            (ProviderKind::Moonshot, "kimi-for-coding-highspeed"),
            (ProviderKind::Ollama, "some-local-build"),
        ] {
            assert_eq!(
                output_ceiling_source(provider, model),
                OutputCeilingSource::RouteDeclaredUnknown,
                "{provider:?}/{model} must declare its unknown ceiling"
            );
            assert_eq!(output_ceiling_source(provider, model).clamp_tokens(), None);
        }
        // Bare `k3` is a membership id, but unlike the `kimi-for-coding`
        // family the K3 quickstart documents its output maximum, and the model
        // catalogue carries it. A documented ceiling is authoritative — the
        // membership allowlist only covers ids the catalogue has nothing to
        // say about, and must not turn a real fact back into an unknown.
        assert_eq!(
            output_ceiling_source(ProviderKind::Moonshot, "k3"),
            OutputCeilingSource::Documented(131_072)
        );
        assert_eq!(
            output_ceiling_source(ProviderKind::OllamaCloud, "some-cloud-build"),
            OutputCeilingSource::Uncatalogued(UNCATALOGUED_COMPAT_MAX_OUTPUT_TOKENS),
            "hosted Ollama Cloud must not inherit the local runtime's unbounded output semantics"
        );
    }

    /// #5440: an Anthropic-family model the catalogue does not describe keeps
    /// the 64K Messages floor as its clamp, but the floor is an assumption
    /// about that model — never a "documented" ceiling.
    #[test]
    fn anthropic_unknown_model_ceiling_is_an_unverified_assumed_floor() {
        let source = output_ceiling_source(ProviderKind::Anthropic, "claude-future-99");
        assert_eq!(source, OutputCeilingSource::Unverified(64_000));
        assert_eq!(source.as_str(), "unverified");
        assert_eq!(source.clamp_tokens(), Some(64_000));
        // Same honesty on the compatibility dialects of the same family.
        assert_eq!(
            output_ceiling_source(ProviderKind::MinimaxAnthropic, "claude-future-99"),
            OutputCeilingSource::Unverified(64_000)
        );
        assert_eq!(
            output_ceiling_source(ProviderKind::Openmodel, "claude-future-99"),
            OutputCeilingSource::Unverified(64_000)
        );
    }

    /// #5440: a model the catalogue does describe keeps its documented
    /// ceiling and its documented label — the unverified rung must not
    /// swallow real facts.
    #[test]
    fn anthropic_documented_model_ceiling_stays_documented() {
        let source = output_ceiling_source(ProviderKind::Anthropic, "claude-sonnet-4-6");
        assert_eq!(source, OutputCeilingSource::Documented(128_000));
        assert_eq!(source.as_str(), "documented");
        assert_eq!(source.clamp_tokens(), Some(128_000));
    }

    /// #5440: the Codex OAuth route clamps every response to 4K by policy,
    /// because the OAuth cache publishes no ceiling. The clamp stands; the
    /// receipt must call the number what it is.
    #[test]
    fn codex_oauth_ceiling_clamps_but_never_claims_documented() {
        let source = output_ceiling_source(ProviderKind::OpenaiCodex, "gpt-5.5");
        assert_eq!(source, OutputCeilingSource::Unverified(4_096));
        assert_eq!(source.as_str(), "unverified");
        assert_eq!(source.clamp_tokens(), Some(4_096));
        assert_eq!(
            effective_max_output_tokens_for_route(ProviderKind::OpenaiCodex, "gpt-5.5", None),
            4_096,
            "the honesty relabel must not revalue the long-standing clamp"
        );
    }

    #[test]
    fn codex_missing_route_metadata_uses_provider_context_floor() {
        assert_eq!(
            route_context_window_tokens(ProviderKind::OpenaiCodex, "gpt-5.5", None),
            128_000
        );
        // 80% of the 128K window (102_400) fits under the input ceiling.
        assert_eq!(
            compaction_threshold_for_route_at_percent(
                ProviderKind::OpenaiCodex,
                "gpt-5.5",
                None,
                80.0,
            ),
            102_400
        );
        assert!(auto_compact_default_for_route(
            ProviderKind::OpenaiCodex,
            "gpt-5.5",
            None,
        ));
    }

    /// The assertion values here depend on `explicit_max_output_tokens_override`
    /// seeing no ambient env override, and sibling tests in this binary
    /// (this module, `client`, `vision/tools`, `core/engine`) set
    /// `CODEWHALE_MAX_OUTPUT_TOKENS`/`DEEPSEEK_MAX_OUTPUT_TOKENS` while holding
    /// `lock_test_env`. Without the lock and guards this test could read a
    /// concurrent writer's value mid-assertion (process-global env, parallel
    /// threads), which is the order-dependent flake this guards against.
    #[test]
    fn v4_trigger_uses_window_percent_when_it_fits_spendable_input() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        let budget = route_context_budget(ProviderKind::Deepseek, "deepseek-v4-pro", None, 0)
            .expect("V4 route budget");

        assert_eq!(budget.window_tokens, 1_000_000);
        assert_eq!(budget.output_cap_tokens, u64::from(API_MAX_OUTPUT_TOKENS));
        assert_eq!(budget.input_budget_ceiling, 933_440);
        // 80% of the 1M window fits below the spendable input ceiling.
        assert_eq!(
            compaction_threshold_for_route_at_percent(
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
                None,
                80.0,
            ),
            800_000
        );
    }

    #[test]
    fn kimi_k3_defaults_auto_compaction_on() {
        assert!(auto_compact_default_for_route(
            ProviderKind::Moonshot,
            "kimi-k3",
            None,
        ));
    }

    #[test]
    fn kimi_catalog_output_ceiling_preserves_input_budget() {
        let _lock = crate::test_support::lock_test_env();
        let _max_output = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        // #4368/#4378: Models.dev may report Kimi's full 262K context as both
        // context and output ceilings. Reserve the route-effective 32K request
        // cap rather than treating that catalog maximum as the amount every
        // turn will emit.
        let limits = RouteLimits {
            context_tokens: Some(262_144),
            output_tokens: Some(262_144),
            ..RouteLimits::default()
        };
        let budget =
            route_context_budget(ProviderKind::Moonshot, "kimi-k2.7-code", Some(limits), 0)
                .expect("Kimi route budget");
        let trigger = compaction_threshold_for_route_at_percent(
            ProviderKind::Moonshot,
            "kimi-k2.7-code",
            Some(limits),
            80.0,
        );

        assert_eq!(budget.output_cap_tokens, 32_768);
        assert_eq!(budget.input_budget_ceiling, 228_352);
        // 80% of the 262_144 window; fits under the 228_352 ceiling because
        // the output reservation is the route-effective 32K request cap.
        assert_eq!(trigger, 209_715);
        assert!(trigger as u64 <= budget.input_budget_ceiling);
    }

    #[test]
    fn explicit_route_output_limit_beats_unknown_model_name_fallback() {
        let _lock = crate::test_support::lock_test_env();
        let _max_output =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "65536");
        let limits = RouteLimits {
            context_tokens: Some(262_144),
            output_tokens: Some(24_576),
            ..RouteLimits::default()
        };

        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Vllm,
                "arbitrary-local-wire-alias",
                Some(limits),
            ),
            24_576
        );
        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Vllm,
                "arbitrary-local-wire-alias",
                None,
            ),
            65_536,
            "an unknown compatibility cap must not clamp; only the requested cap applies"
        );
        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Vllm,
                "kimi-k2.7-code",
                Some(RouteLimits {
                    output_tokens: Some(262_144),
                    ..RouteLimits::default()
                }),
            ),
            32_768,
            "known model caps must remain authoritative on self-hosted routes"
        );
    }

    /// #4368 follow-up: the Kimi Code membership ids deliberately have no
    /// static output cap (the membership catalog owns their limits). The old
    /// generic `unwrap_or(4096)` in `provider_capability` turned that unknown
    /// into a hard 4K clamp here, silently truncating every offline membership
    /// turn. Unknown must mean "no compatibility clamp".
    #[test]
    fn kimi_membership_unknown_output_cap_does_not_clamp_to_4k() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        for model in ["kimi-for-coding", "kimi-for-coding-highspeed"] {
            assert_eq!(
                provider_capability(ProviderKind::Moonshot, model).max_output,
                None,
                "{model}: membership output ceiling must stay unknown, not a placeholder"
            );

            let cap = effective_max_output_tokens_for_route(ProviderKind::Moonshot, model, None);
            assert_eq!(
                cap,
                effective_max_output_tokens(model),
                "{model}: unknown compatibility cap must leave the requested cap intact"
            );
            assert_ne!(cap, 4_096, "{model}: must not inherit the old 4K fallback");
            // No invented sentinel ceiling either.
            assert_ne!(cap, u32::MAX);
            assert_ne!(cap, 32_768);
        }
    }

    /// A concrete membership offering limit is still authoritative — "unknown
    /// means no clamp" must not become "never clamp".
    #[test]
    fn kimi_membership_route_limit_still_caps_output() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        let limits = RouteLimits {
            context_tokens: Some(262_144),
            output_tokens: Some(16_384),
            ..RouteLimits::default()
        };
        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Moonshot,
                "kimi-for-coding",
                Some(limits),
            ),
            16_384
        );
    }

    /// GLM and MiniMax publish real output ceilings; those stay authoritative
    /// so relaxing the unknown case cannot leak into known routes.
    #[test]
    fn known_glm_and_minimax_output_caps_remain_authoritative() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        // GLM 5.2: 1M window, documented 131K output. The capability remains
        // known even though the safe automatic request starts at 64K.
        let glm = provider_capability(ProviderKind::Zai, "glm-5.2");
        assert_eq!(glm.max_output, Some(131_072));

        let minimax = provider_capability(ProviderKind::Minimax, "minimax-m3");
        assert_eq!(minimax.max_output, Some(524_288));

        // A known cap below the requested cap must still clamp.
        assert_eq!(
            effective_max_output_tokens_for_route(ProviderKind::Moonshot, "kimi-k2.7-code", None),
            32_768,
        );
    }

    /// A documented capability maximum is not itself a sane default request
    /// size. The capability remains documented and available as an explicit
    /// override; only the automatic request is bounded.
    #[test]
    fn documented_ceiling_is_a_bound_not_an_unbounded_default_request() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        for model in [
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "deepseek-v4flash",
            "deepseek-ai/deepseek-v4-pro",
            "deepseek-chat",
            "deepseek-reasoner",
        ] {
            assert_eq!(
                output_ceiling_source(ProviderKind::Deepseek, model),
                OutputCeilingSource::Documented(384_000),
                "{model}"
            );
        }
        assert_eq!(
            effective_max_output_tokens("deepseek-v4-flash"),
            API_MAX_OUTPUT_TOKENS,
            "a 384K capability maximum must not become the no-config request size"
        );
        assert_eq!(
            effective_max_output_tokens("glm-5.2"),
            API_MAX_OUTPUT_TOKENS,
            "a 131K capability maximum must also remain a ceiling, not a default"
        );
    }

    #[test]
    fn uncatalogued_deepseek_variants_require_exact_output_metadata() {
        let _env_lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        for provider in [
            ProviderKind::Deepseek,
            ProviderKind::Deepseek,
            ProviderKind::DeepseekAnthropic,
            ProviderKind::Custom,
        ] {
            for model in [
                "deepseek-v4.1-flash-expires-on-0910",
                "deepseek-v4-flash-vendor",
            ] {
                assert_eq!(provider_capability(provider, model).max_output, None);
                let source = output_ceiling_source(provider, model);
                assert_eq!(source, OutputCeilingSource::Uncatalogued(65_536));
                assert_eq!(source.as_str(), "uncatalogued");
                assert_eq!(
                    effective_max_output_tokens_for_route(provider, model, None),
                    64_000,
                    "{provider:?}: {model}"
                );
            }
        }

        assert_eq!(
            output_ceiling_source(ProviderKind::Deepseek, "deepseek-v4.1-flash"),
            OutputCeilingSource::Documented(384_000)
        );

        // Exact route output facts supply a ceiling only for that request.
        let limits = RouteLimits {
            context_tokens: Some(128_000),
            output_tokens: Some(24_576),
            ..RouteLimits::default()
        };
        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Deepseek,
                "deepseek-v4.1-flash-expires-on-0910",
                Some(limits)
            ),
            24_576
        );
        assert_eq!(
            codewhale_models::max_output_tokens_for_model("deepseek-v4.1-flash-expires-on-0910"),
            None
        );
    }

    #[test]
    fn deepseek_v4_explicit_mid_windows_share_one_safe_no_config_budget() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        for window in [262_144, 327_680, 393_216] {
            let limits = RouteLimits {
                context_tokens: Some(window),
                ..RouteLimits::default()
            };
            let cap = effective_max_output_tokens_for_route(
                ProviderKind::Vllm,
                "DeepSeek-V4-Flash",
                Some(limits),
            );
            let reservation =
                route_output_reservation(ProviderKind::Vllm, "DeepSeek-V4-Flash", Some(limits));
            let budget = route_context_budget(
                ProviderKind::Vllm,
                "DeepSeek-V4-Flash",
                Some(limits),
                105_000,
            )
            .expect("explicit vLLM route budget");

            assert_eq!(cap, API_MAX_OUTPUT_TOKENS, "window={window}");
            assert_eq!(reservation, cap, "window={window}");
            assert_eq!(
                budget.input_budget_ceiling,
                window - u64::from(API_MAX_OUTPUT_TOKENS) - 1_024,
                "window={window}"
            );
            assert!(
                105_000 < budget.input_budget_ceiling,
                "ordinary 85K-105K inputs must not trigger emergency compaction: {budget:?}"
            );
            assert!(budget.available_input_tokens > 0, "window={window}");
        }
    }

    #[test]
    fn explicit_output_override_is_preserved_and_reserved_on_mid_windows() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "100000");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let limits = RouteLimits {
            context_tokens: Some(327_680),
            ..RouteLimits::default()
        };

        let cap = effective_max_output_tokens_for_route(
            ProviderKind::Vllm,
            "DeepSeek-V4-Flash",
            Some(limits),
        );
        assert_eq!(cap, 100_000);
        assert_eq!(
            route_output_reservation(ProviderKind::Vllm, "DeepSeek-V4-Flash", Some(limits),),
            cap
        );
        let budget = route_context_budget(
            ProviderKind::Vllm,
            "DeepSeek-V4-Flash",
            Some(limits),
            105_000,
        )
        .expect("override route budget");
        assert_eq!(budget.input_budget_ceiling, 226_656);
        assert!(budget.available_input_tokens > 0);
    }

    #[test]
    fn explicit_uncatalogued_allowance_respects_route_and_context_limits() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "100000");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let model = "uncatalogued-preview-for-output-test";
        let limits = RouteLimits {
            context_tokens: Some(327_680),
            ..RouteLimits::default()
        };
        assert_eq!(
            effective_max_output_tokens_for_route(ProviderKind::Custom, model, Some(limits)),
            100_000
        );
        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Custom,
                model,
                Some(RouteLimits {
                    output_tokens: Some(32_768),
                    ..limits
                })
            ),
            32_768
        );
        let small = RouteLimits {
            context_tokens: Some(32_768),
            ..RouteLimits::default()
        };
        let cap = effective_max_output_tokens_for_route(ProviderKind::Custom, model, Some(small));
        assert_eq!(cap, 30_720);
        assert_eq!(
            route_output_reservation(ProviderKind::Custom, model, Some(small)),
            cap
        );
    }

    #[test]
    fn oversized_explicit_override_is_clamped_and_reserved_to_the_route_window() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "384000");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let limits = RouteLimits {
            context_tokens: Some(327_680),
            ..RouteLimits::default()
        };

        let cap = effective_max_output_tokens_for_route(
            ProviderKind::Vllm,
            "DeepSeek-V4-Flash",
            Some(limits),
        );
        assert_eq!(cap, 325_632);
        assert_eq!(
            route_output_reservation(ProviderKind::Vllm, "DeepSeek-V4-Flash", Some(limits),),
            cap,
            "preflight must reserve every token the explicit override can put on the wire"
        );
        let budget = route_context_budget(ProviderKind::Vllm, "DeepSeek-V4-Flash", Some(limits), 0)
            .expect("oversized override route budget");
        assert_eq!(budget.input_budget_ceiling, 1_024);
    }

    #[test]
    fn explicit_override_on_large_window_stays_unified() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "384000");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let limits = RouteLimits {
            context_tokens: Some(1_000_000),
            ..RouteLimits::default()
        };

        let cap = effective_max_output_tokens_for_route(
            ProviderKind::Vllm,
            "DeepSeek-V4-Flash",
            Some(limits),
        );
        let reservation =
            route_output_reservation(ProviderKind::Vllm, "DeepSeek-V4-Flash", Some(limits));
        let budget = route_context_budget(ProviderKind::Vllm, "DeepSeek-V4-Flash", Some(limits), 0)
            .expect("large explicit route budget");

        assert_eq!(cap, 384_000);
        assert_eq!(reservation, cap);
        assert_eq!(budget.output_cap_tokens, u64::from(cap));
        assert_eq!(budget.input_budget_ceiling, 614_976);
    }

    #[test]
    fn automatic_wire_cap_and_reservation_have_no_large_window_cliff() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        for window in [499_999, 500_000, 1_000_000] {
            let limits = RouteLimits {
                context_tokens: Some(window),
                ..RouteLimits::default()
            };
            let wire = effective_max_output_tokens_for_route(
                ProviderKind::Vllm,
                "DeepSeek-V4-Flash",
                Some(limits),
            );
            let reservation =
                route_output_reservation(ProviderKind::Vllm, "DeepSeek-V4-Flash", Some(limits));
            assert_eq!(wire, API_MAX_OUTPUT_TOKENS, "window={window}");
            assert_eq!(reservation, wire, "window={window}");
        }
    }

    #[test]
    fn concrete_route_input_limit_clamps_preflight_and_compaction() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let limits = RouteLimits {
            context_tokens: Some(1_000_000),
            input_tokens: Some(128_000),
            output_tokens: Some(64_000),
            output_declared: false,
        };

        let budget = route_context_budget(
            ProviderKind::Vllm,
            "DeepSeek-V4-Flash",
            Some(limits),
            200_000,
        )
        .expect("route budget");
        assert_eq!(route_input_limit_tokens(Some(limits)), Some(128_000));
        assert_eq!(budget.input_budget_ceiling, 128_000);
        assert_eq!(budget.available_input_tokens, 0);
        assert_eq!(budget.compaction_trigger_for_percent(80.0), 128_000);
    }

    #[test]
    fn canonical_output_override_blank_falls_through_but_invalid_is_authoritative() {
        let _lock = crate::test_support::lock_test_env();
        let _legacy = crate::test_support::EnvVarGuard::set("DEEPSEEK_MAX_OUTPUT_TOKENS", "100000");

        {
            let _canonical =
                crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "   ");
            assert_eq!(explicit_max_output_tokens_override(), Some(100_000));
        }
        for invalid in ["not-a-number", "0"] {
            let _canonical =
                crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", invalid);
            assert_eq!(explicit_max_output_tokens_override(), None, "{invalid}");
            assert_eq!(
                effective_max_output_tokens("deepseek-v4-pro"),
                API_MAX_OUTPUT_TOKENS
            );
        }
    }

    #[test]
    fn mid_window_internal_reservation_stays_on_the_ordinary_request_floor() {
        let _lock = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let reservation =
            route_output_reservation(ProviderKind::Arcee, "trinity-large-thinking", None);
        assert_eq!(reservation, API_MAX_OUTPUT_TOKENS);
        let budget = route_context_budget(ProviderKind::Arcee, "trinity-large-thinking", None, 0)
            .expect("trinity route budget");
        assert_eq!(budget.compaction_trigger_for_percent(80.0), 195_584);
    }

    #[test]
    fn review_reserve_is_sized_from_reasoning_classification() {
        // Mapping core (#6285): every classification arm.
        assert_eq!(review_reserve_percent_for(Some(false), false), 0);
        assert_eq!(review_reserve_percent_for(Some(false), true), 0);
        assert_eq!(review_reserve_percent_for(Some(true), false), 25);
        assert_eq!(review_reserve_percent_for(Some(true), true), 50);
        // Unknown (no catalogue row) is not evidence of no reasoning (#6032).
        assert_eq!(review_reserve_percent_for(None, false), 25);
        assert_eq!(review_reserve_percent_for(None, true), 25);
    }

    #[test]
    fn review_reserve_tokens_follow_the_model_classification() {
        // A model no catalogue row resolves for: a quarter of the allowance is
        // reserved as visible text, and the token math scales off the exact
        // resolved allowance.
        let unknown = "not-a-catalogue-model-6285";
        assert_eq!(review_visible_text_reserve_percent(unknown), 25);
        assert_eq!(review_visible_text_reserve_tokens(unknown, 65_536), 16_384);
        assert_eq!(review_visible_text_reserve_tokens(unknown, 0), 0);
    }
}
