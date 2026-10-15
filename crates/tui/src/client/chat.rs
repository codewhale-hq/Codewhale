//! Chat Completions API helpers for DeepSeek's OpenAI-compatible endpoint.
//!
//! This is the production code path. Streaming (`create_message_stream`),
//! request building (`build_chat_messages*`), and SSE parsing
//! (`parse_sse_chunk_with_reasoning_style`) all live here.

use std::collections::HashMap;
use std::io::Write;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::timeout as tokio_timeout;

use crate::config::{
    TOGETHER_INKLING_MODEL, is_exact_direct_moonshot_k3_route, is_exact_kimi_code_k3_route,
    is_exact_zai_chat_route, is_exact_zai_forced_thinking_route, is_exact_zai_tiered_effort_route,
    is_kimi_code_membership_model, minimax_m3_route_uses_max_completion_tokens,
    moonshot_base_url_is_exact_kimi_code, wire_model_for_provider_route,
};

use crate::config::ProviderKind;
use crate::llm_client::StreamEventBox;
use crate::logging;
use codewhale_models::{
    ContentBlock, ContentBlockStart, Delta, Message, MessageDelta, MessageRequest, MessageResponse,
    StreamEvent, SystemPrompt, Tool, ToolCaller, Usage, is_openai_gpt_56_api_model,
    model_is_openai_reasoning_family, model_supports_reasoning,
};

use super::prepared::WireDialect;
use super::role_placement::{RolePlacement, role_placement};
use super::wire::{extract_sse_data_value, flush_sse_line, push_sse_event_data, take_sse_line};
use super::{
    CodewhaleClient, ERROR_BODY_MAX_BYTES, SSE_BACKPRESSURE_HIGH_WATERMARK,
    SSE_BACKPRESSURE_SLEEP_MS, SSE_MAX_LINES_PER_CHUNK, acquire_stream_buffer,
    apply_reasoning_effort, bounded_error_text, from_api_tool_name, parse_usage,
    release_stream_buffer, system_to_instructions, to_api_tool_name,
};
use codewhale_config::route::RouteLimits;
use codewhale_models::Role;

fn apply_provider_token_limit(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    max_tokens: u32,
) {
    let use_max_completion_tokens = provider == ProviderKind::XiaomiMimo
        || (provider == ProviderKind::Openai && model_is_openai_reasoning_family(model))
        || minimax_m3_route_uses_max_completion_tokens(provider, base_url, model)
        || is_exact_direct_moonshot_k3_route(provider, base_url, model);
    if !use_max_completion_tokens {
        return;
    }

    if let Some(object) = body.as_object_mut() {
        object.remove("max_tokens");
    }
    body["max_completion_tokens"] = json!(max_tokens);
}

fn apply_openai_reasoning_effort(
    body: &mut Value,
    provider: ProviderKind,
    model: &str,
    effort: Option<&str>,
) {
    let model_lower = model.trim().to_ascii_lowercase();
    let is_gpt_56 =
        provider == ProviderKind::Openai && is_openai_gpt_56_api_model(model_lower.as_str());
    let is_openai_reasoning =
        provider == ProviderKind::Openai && model_is_openai_reasoning_family(model);
    let is_muse_spark = provider == ProviderKind::Meta
        && (model_lower == "muse-spark" || model_lower.starts_with("muse-spark-"));
    if !is_openai_reasoning && !is_muse_spark {
        return;
    }
    let Some(effort) =
        effort.and_then(|value| openai_compatible_reasoning_effort(value, is_gpt_56, !is_gpt_56))
    else {
        return;
    };
    body["reasoning_effort"] = json!(effort);
}

/// xAI's first-party `reasoning_effort` ladder, driven by the bundled
/// catalog row for the exact model id: a row that documents an `effort`
/// option gets the field (`xhigh` only where the row lists it — grok-4.7 and
/// grok-4.6 do, grok-4.5 maps it to `high`); a row without one (grok-4.3,
/// grok-build) or no row at all sends nothing. Grok reasoning cannot be
/// disabled, so `off` is sent as the documented default `high`
/// (<https://docs.x.ai/docs/guides/reasoning>).
fn apply_xai_grok_reasoning_effort(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if provider != ProviderKind::Xai
        || !codewhale_config::provider::is_exact_xai_platform_route(
            codewhale_config::ProviderKind::Xai,
            base_url,
        )
    {
        return;
    }
    let Some(effort) = effort else {
        return;
    };
    let model = model.trim().to_ascii_lowercase();
    // The bundled row with Codewhale's corrections applied (#6396): the raw
    // seed row carries Models.dev's ladder, which lists an effort control for
    // grok-4.3 that xAI does not document.
    let Some(row) =
        crate::provider_lake::bundled_catalog_offering_for_model(ProviderKind::Xai, &model)
    else {
        return;
    };
    let Some(documented) = row
        .reasoning_options
        .iter()
        .find(|option| option["type"] == "effort")
        .and_then(|option| option["values"].as_array())
    else {
        return;
    };
    let supports_xhigh = documented.iter().any(|value| value == "xhigh");
    let wire_effort = match effort.trim().to_ascii_lowercase().as_str() {
        "auto" | "automatic" | "" => return,
        "off" | "disabled" | "none" | "false" | "high" => "high",
        "minimal" | "minimum" | "low" | "light" => "low",
        "medium" | "mid" => "medium",
        "xhigh" | "max" | "maximum" | "highest" | "ultra" | "ultracode" => {
            if supports_xhigh {
                "xhigh"
            } else {
                "high"
            }
        }
        _ => return,
    };
    body["reasoning_effort"] = json!(wire_effort);
}

fn apply_inkling_reasoning_effort(
    body: &mut Value,
    provider: ProviderKind,
    model: &str,
    effort: Option<&str>,
) {
    if provider != ProviderKind::Together
        || !model.trim().eq_ignore_ascii_case(TOGETHER_INKLING_MODEL)
    {
        return;
    }

    // Inkling's official chat template accepts OpenAI's top-level
    // `reasoning_effort` field with this exact vocabulary. It does not use
    // Together's generic `thinking` extension or the `xhigh` wire value.
    if let Some(object) = body.as_object_mut() {
        object.remove("thinking");
    }
    let Some(effort) = effort else {
        return;
    };
    let wire_effort = match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" => "none",
        "minimal" => "minimal",
        "low" => "low",
        "medium" | "mid" | "" => "medium",
        "high" => "high",
        "max" | "xhigh" | "highest" | "ultra" | "ultracode" => "max",
        _ => return,
    };
    body["reasoning_effort"] = json!(wire_effort);
}

/// Apply Kimi Code K3's route-specific nested thinking effort after the
/// generic Moonshot shaping. Other Moonshot and Kimi-compatible routes accept
/// only the generic enabled/disabled form, so the exact endpoint and bare
/// model identifier are both part of this guard.
fn apply_kimi_code_k3_reasoning_effort(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if !is_exact_kimi_code_k3_route(provider, base_url, model) {
        return;
    }
    let Some(effort) = effort else {
        return;
    };

    let thinking = match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "disabled" | "false" | "low" | "minimum" | "minimal" | "light" => {
            json!({ "type": "enabled", "effort": "low" })
        }
        "medium" | "high" => json!({ "type": "enabled", "effort": "high" }),
        "xhigh" | "ultra" | "max" => json!({ "type": "enabled", "effort": "max" }),
        _ => return,
    };

    // K3 uses the nested `thinking.effort` dialect. Do not leave an
    // OpenAI-style effort value behind if another shaping layer was added
    // before this route-specific override.
    if let Some(object) = body.as_object_mut() {
        object.remove("reasoning_effort");
    }
    body["thinking"] = thinking;
}

/// Apply Moonshot's direct K3 reasoning dialect.
///
/// The pay-as-you-go K3 endpoint is always-thinking and accepts only the
/// top-level `reasoning_effort` values low/high/max. In particular, a generic
/// Moonshot `thinking: {type: disabled}` payload is not truthful for this
/// route. Treat a legacy raw `off` as the lowest supported tier defensively;
/// route-aware callers normalize it before it reaches this layer.
fn apply_direct_moonshot_k3_reasoning_effort(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if !is_exact_direct_moonshot_k3_route(provider, base_url, model) {
        return;
    }

    if let Some(object) = body.as_object_mut() {
        object.remove("thinking");
        object.remove("reasoning_effort");
    }
    let Some(effort) = effort else {
        return;
    };
    let wire_effort = match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "disabled" | "false" | "low" | "minimum" | "minimal" | "light" => "low",
        "medium" | "mid" | "high" | "" => "high",
        "xhigh" | "ultra" | "max" | "highest" | "ultracode" => "max",
        // `auto` and unknown legacy values leave the field omitted so the
        // direct API owns its documented default (`max`).
        _ => return,
    };
    body["reasoning_effort"] = json!(wire_effort);
}

/// Keep Z.ai controls on exact first-party routes only. The tiered-effort GLM
/// models (5.2, and the forced-thinking 5.3 family) receive the documented
/// top-level effort, GLM-5.1 and GLM-5-Turbo keep only the generic thinking
/// toggle, and compatible gateways receive neither field because their
/// request dialect is not known from provider/model selection alone.
fn apply_zai_route_reasoning_controls(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if provider != ProviderKind::Zai {
        return;
    }

    if let Some(object) = body.as_object_mut() {
        object.remove("reasoning_effort");
        if !is_exact_zai_chat_route(provider, base_url) {
            // A compatible gateway owns its own request dialect. Provider/model
            // selection alone is not evidence that Z.ai's `thinking` object is
            // supported there, so fail closed instead of leaking it.
            object.remove("thinking");
            return;
        }
    }
    if !crate::config::is_exact_known_zai_reasoning_route(provider, base_url, model) {
        if let Some(object) = body.as_object_mut() {
            object.remove("thinking");
        }
        return;
    }
    if !is_exact_zai_tiered_effort_route(provider, base_url, model) {
        // Exact first-party GLM-5-Turbo and GLM-5.1 keep only the generic
        // enabled/disabled thinking control.
        return;
    }
    if is_exact_zai_forced_thinking_route(provider, base_url, model) {
        apply_zai_forced_thinking_effort(body, effort);
        return;
    }
    match effort
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("high") => body["reasoning_effort"] = json!("high"),
        Some("xhigh") | Some("max") | Some("highest") | Some("ultra") | Some("ultracode") => {
            body["reasoning_effort"] = json!("max");
        }
        // Off, lower tiers, omitted effort, and unknown legacy values retain
        // only the generic Z.ai thinking control.
        _ => {}
    }
}

/// GLM-5.3 and GLM-5.3-Flash are forced-thinking on the exact first-party
/// Z.ai route: `thinking.type: "disabled"` is rejected with an error and
/// `reasoning_effort` accepts only low/high/max. The generic Z.ai layer emits
/// `disabled` for `off`, so a request that was valid for GLM-5.2 fails on
/// 5.3. Rewrite that payload the way the vendor migration note prescribes —
/// keep thinking enabled and send the lowest tier — and map the remaining
/// aliases onto the three documented values, leaving unknown legacy values
/// omitted so the API owns its documented default (`max`).
fn apply_zai_forced_thinking_effort(body: &mut Value, effort: Option<&str>) {
    let thinking_disabled = body
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        == Some("disabled");
    if thinking_disabled {
        body["thinking"] = json!({
            "type": "enabled",
            "clear_thinking": false,
        });
    }
    let Some(effort) = effort else {
        return;
    };
    let wire_effort = match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "disabled" | "false" | "low" | "minimum" | "minimal" | "light" => "low",
        "medium" | "mid" | "high" => "high",
        "xhigh" | "max" | "highest" | "ultra" | "ultracode" => "max",
        _ => return,
    };
    body["reasoning_effort"] = json!(wire_effort);
}

/// Add MiniMax's Chat-only reasoning controls only when endpoint and model
/// prove the exact first-party M3 route. A provider label alone is not enough
/// to send MiniMax-specific fields to a compatible gateway or unknown model.
fn apply_minimax_route_reasoning_controls(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if provider != ProviderKind::Minimax {
        return;
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("reasoning_split");
        object.remove("thinking");
    }
    if !crate::config::is_exact_minimax_m3_route(provider, base_url, model) {
        return;
    }

    body["reasoning_split"] = json!(true);
    match effort
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("off" | "disabled" | "none" | "false") => {
            body["thinking"] = json!({ "type": "disabled" });
        }
        Some(
            "low" | "minimal" | "medium" | "mid" | "high" | "xhigh" | "max" | "highest" | "ultra"
            | "ultracode" | "",
        ) => {
            body["thinking"] = json!({ "type": "adaptive" });
        }
        _ => {}
    }
}

/// Model Studio's OpenAI-compatible API uses its own top-level reasoning
/// controls. Keep them on verified Alibaba Chat Completions routes: a custom
/// `base_url` points the same provider identity at an arbitrary gateway, and
/// that gateway must not be handed Alibaba's dialect.
///
/// This is the *sole* writer of Model Studio reasoning fields —
/// `apply_reasoning_effort` deliberately writes nothing for the `Modelstudio*`
/// identities — so the strip below runs for all four variants, including the
/// two Anthropic-dialect ones. Those normally reach the Messages adapter
/// instead, but `wire = "openai"` can route them here, and an unmatched
/// `enable_thinking` left in the body would then go out unguarded.
fn apply_modelstudio_route_reasoning_controls(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if !matches!(
        provider,
        ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic
    ) {
        return;
    }

    if let Some(object) = body.as_object_mut() {
        object.remove("thinking");
        object.remove("enable_thinking");
        object.remove("preserve_thinking");
        object.remove("reasoning_effort");
    }
    if !is_exact_modelstudio_chat_route(provider, base_url) {
        return;
    }

    let thinking_only = modelstudio_model_is_thinking_only(model);
    if !thinking_only && !modelstudio_model_is_hybrid(model) {
        return;
    }

    let thinking_enabled = !modelstudio_effort_disables_thinking(effort);
    // Thinking-only models emit `reasoning_content` but reject an
    // enable/disable control. Hybrid models use `enable_thinking`.
    if !thinking_only {
        body["enable_thinking"] = json!(thinking_enabled);
    }
    if modelstudio_model_supports_preserve_thinking(model) {
        // Model Studio otherwise drops assistant `reasoning_content` from the
        // next turn's context. This applies even when the provider default
        // leaves thinking enabled and no explicit UI effort was selected.
        body["preserve_thinking"] = json!(thinking_only || thinking_enabled);
    }
    if !thinking_only
        && thinking_enabled
        && let Some(effort) = effort.and_then(modelstudio_reasoning_effort_for_model)
        && modelstudio_model_supports_reasoning_effort(model)
    {
        body["reasoning_effort"] = json!(effort);
    }
}

/// Fail-closed host guard: only Alibaba's own OpenAI-compatible Chat
/// Completions URL shapes count. Anything else (a proxy, a self-hosted
/// gateway, a typo) gets the Model Studio fields stripped and nothing added.
fn is_exact_modelstudio_chat_route(provider: ProviderKind, base_url: &str) -> bool {
    let trimmed = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    let Some((host, path)) = trimmed
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };

    // Includes Token Plan's default and workspace-scoped
    // `{workspace}.<region>.maas.aliyuncs.com/compatible-mode/v1` hosts.
    let token_plan_chat = host.ends_with(".maas.aliyuncs.com") && path == "compatible-mode/v1";
    let coding_plan_chat = host == "coding-intl.dashscope.aliyuncs.com" && path == "v1";
    // Alibaba's classic pay-as-you-go DashScope endpoints serve the same
    // models and the same dialect; leaving them off the allowlist silently
    // stripped every reasoning control on a genuine Alibaba host
    // (2026-08-04 review). The intl spelling matches the repo's own
    // provider defaults.
    let classic_dashscope_chat = matches!(
        host,
        "dashscope.aliyuncs.com" | "dashscope-intl.aliyuncs.com"
    ) && path == "compatible-mode/v1";

    match provider {
        // The primary Model Studio provider selects Coding Plan through
        // `mode = "coding-plan"`, which resolves this base URL without
        // changing the provider enum. Legacy Coding Plan identities remain
        // supported as well, so recognize either official Chat route for the
        // complete Model Studio OpenAI family. The `*Anthropic` identities
        // speak the Messages dialect and are never verified here.
        ProviderKind::ModelstudioTokenPlan | ProviderKind::ModelstudioCodingPlan => {
            token_plan_chat || coding_plan_chat || classic_dashscope_chat
        }
        _ => false,
    }
}

fn is_exact_modelstudio_thinking_only_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) -> bool {
    is_exact_modelstudio_chat_route(provider, base_url) && modelstudio_model_is_thinking_only(model)
}

fn modelstudio_effort_disables_thinking(effort: Option<&str>) -> bool {
    effort.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "disabled" | "none" | "false"
        )
    })
}

/// Models with no enable/disable control at all. `models_dev.bundled.json`
/// lists `qwen3.8-max` as `thinking: always_on` and gives `qwen3.8-max-preview`
/// effort/budget options with no `toggle`, so sending `enable_thinking` to
/// either is at best ignored and at worst a 400.
fn modelstudio_model_is_thinking_only(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    matches!(
        model.as_str(),
        "qwen3.8-max"
            | "qwen3.8-max-preview"
            // Kimi K2.7 Code is always-thinking. Keep both Alibaba-hosted and
            // Moonshot-supplied exact IDs separate from hybrid Kimi variants
            // so we never send the unsupported enable_thinking switch.
            | "kimi-k2.7-code"
            | "kimi/kimi-k2.7-code"
            | "kimi/kimi-k2.7-code-highspeed"
    )
}

fn modelstudio_model_is_hybrid(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("qwen3.7-")
        || model.starts_with("qwen3.6-")
        || model.starts_with("qwen3.5-")
        || model.starts_with("qwen3-")
        || model.starts_with("deepseek-v4")
        || model.starts_with("deepseek-v3.2")
        || model.starts_with("deepseek-v3.1")
        || model.starts_with("kimi-k2.6")
        || matches!(model.as_str(), "kimi/kimi-k2.6")
        || model.starts_with("kimi-k2.5")
        || model.starts_with("glm-")
}

fn modelstudio_model_supports_preserve_thinking(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    matches!(
        model.as_str(),
        "qwen3.7-max"
            | "qwen3.7-max-us"
            | "qwen3.7-max-2026-05-17"
            | "qwen3.7-max-2026-05-20"
            | "qwen3.7-max-2026-06-08"
            | "qwen3.7-max-preview"
            | "qwen3.7-plus"
            | "qwen3.7-plus-us"
            | "qwen3.7-plus-2026-05-26"
            | "qwen3.6-max-preview"
            | "qwen3.6-plus"
            | "qwen3.6-plus-2026-04-02"
            | "qwen3.6-flash"
            | "qwen3.6-flash-2026-04-16"
            | "kimi-k2.6"
            | "kimi-k2.7-code"
            | "kimi/kimi-k2.6"
            | "kimi/kimi-k2.7-code"
            | "kimi/kimi-k2.7-code-highspeed"
    )
}

fn modelstudio_model_supports_reasoning_effort(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("deepseek-v4") || matches!(model.as_str(), "glm-5.2" | "glm-5.1" | "glm-5")
}

fn modelstudio_reasoning_effort_for_model(effort: &str) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        // Model Studio documents low and medium as aliases for high.
        "minimal" | "low" | "medium" | "mid" | "high" | "" => Some("high"),
        "xhigh" | "max" | "highest" | "ultra" | "ultracode" => Some("max"),
        _ => None,
    }
}

/// Final reasoning-control pass shared by streaming and non-streaming Chat
/// Completions requests. Route-specific shapers run after the generic provider
/// layer so they can remove fields that are invalid for their exact endpoint.
pub(super) fn apply_route_reasoning_controls(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    let reasoning_provider =
        if provider == ProviderKind::Custom && requires_reasoning_content(model) {
            ProviderKind::Deepseek
        } else {
            provider
        };
    apply_reasoning_effort(body, effort, reasoning_provider);
    apply_modelstudio_route_reasoning_controls(body, provider, base_url, model, effort);
    apply_minimax_route_reasoning_controls(body, provider, base_url, model, effort);
    apply_inkling_reasoning_effort(body, provider, model, effort);
    apply_openai_reasoning_effort(body, provider, model, effort);
    apply_xai_grok_reasoning_effort(body, provider, base_url, model, effort);
    apply_direct_moonshot_k3_reasoning_effort(body, provider, base_url, model, effort);
    apply_kimi_code_k3_reasoning_effort(body, provider, base_url, model, effort);
    apply_zai_route_reasoning_controls(body, provider, base_url, model, effort);
    apply_mistral_route_reasoning_controls(body, provider, base_url, model, effort);
    apply_google_reasoning_effort(body, base_url, model, effort);
}

/// Mistral's polymorphic reasoning-content contract is only proven on its
/// first-party Chat Completions endpoints. A configured `mistral` provider may
/// point at an arbitrary OpenAI-compatible gateway, so provider identity alone
/// is not enough to opt that route into Mistral's request or response dialect.
fn is_exact_mistral_chat_route(provider: ProviderKind, base_url: &str) -> bool {
    if provider != ProviderKind::Mistral {
        return false;
    }
    let trimmed = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    let Some((host, path)) = trimmed
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    matches!(
        host,
        "api.mistral.ai" | "api.eu.mistral.ai" | "api.us.mistral.ai"
    ) && path == "v1"
}

/// Google's OpenAI-compatibility route, identified by the **resolved base
/// URL** rather than by provider identity. Thought signatures are captured
/// from tool-call `extra_content.google.thought_signature` and replayed on
/// the assistant tool-call messages of later turns; thinking models fail
/// closed when a replayed call has no signature.
///
/// The endpoint carries the signature contract, not the config row that
/// happens to name it: a manually configured `kind="openai-compatible"`
/// provider ([`ProviderKind::Custom`]) pointed at this exact host and path is
/// byte-for-byte the same endpoint as the built-in `google` row, so it must
/// preserve and replay signatures the same way. The converse still holds —
/// a `google` row pointed at some other gateway is not this route and never
/// carries Google-only fields off-endpoint.
fn is_google_openai_compat_chat_route(base_url: &str) -> bool {
    let trimmed = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    let Some((host, path)) = trimmed
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    host == "generativelanguage.googleapis.com" && path == "v1beta/openai"
}

/// Gemini models whose thinking makes thought signatures load-bearing on
/// the OpenAI-compat route. Gemini 2.5 Flash-Lite ships with thinking off
/// by default, so a missing signature there degrades with a warning
/// instead of failing the turn.
fn google_model_requires_thought_signatures(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    // Google names the same model both ways on this endpoint, and a route
    // configured as `models/gemini-3-pro` matched none of the prefixes below:
    // the model that most needs a signature looked like one that needs none,
    // so the fail-closed check waved it through and Google rejected the replay
    // instead (#6018).
    let model = model.strip_prefix("models/").unwrap_or(&model);
    if model.starts_with("gemini-3") {
        return true;
    }
    if model.starts_with("gemini-2.5-pro") {
        return true;
    }
    model.starts_with("gemini-2.5-flash") && !model.starts_with("gemini-2.5-flash-lite")
}

/// Google's compatibility endpoint accepts the ordinary `reasoning_effort`
/// field across Gemini 2.5 and 3. A top-level `google` object is rejected;
/// native thinking controls would require `extra_body.google` instead.
/// Use one control, since the endpoint rejects overlapping effort and native
/// thinking settings. https://ai.google.dev/gemini-api/docs/openai#thinking
fn apply_google_reasoning_effort(
    body: &mut serde_json::Value,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if !is_google_openai_compat_chat_route(base_url) {
        return;
    }
    let Some(effort) = effort else {
        return;
    };
    let model = model.trim().to_ascii_lowercase();
    let model = model.strip_prefix("models/").unwrap_or(&model);
    let can_disable = model.starts_with("gemini-2.5-") && !model.starts_with("gemini-2.5-pro");
    let effort = match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" if can_disable => "none",
        // Gemini 3 and 2.5 Pro cannot disable thinking. The compatibility
        // layer maps minimal to the selected model's lowest supported level.
        "off" | "disabled" | "none" | "false" | "minimal" => "minimal",
        "low" => "low",
        "medium" | "mid" | "" => "medium",
        "high" | "xhigh" | "max" | "highest" | "ultra" | "ultracode" => "high",
        _ => return,
    };
    body["reasoning_effort"] = json!(effort);
}

/// Fail closed before transport when Google's OpenAI-compat route would
/// replay tool calls without the thought signatures Google's thinking models
/// require. The error names the model and the tool call and tells the
/// operator how to recover instead of letting Google reject or corrupt the
/// tool loop.
///
/// Models whose thinking is off by default (Gemini 2.5 Flash-Lite) degrade
/// instead of failing — but never silently: the unsigned replay is reported
/// through the same warning path the reasoning-replay sanitizer uses, so a
/// later tool-turn failure has a receipt. Only tool-call identifiers and the
/// model id are logged; signature bytes never are.
fn validate_google_thought_signature_replay(
    base_url: &str,
    model: &str,
    messages: &[Value],
) -> Result<()> {
    if !is_google_openai_compat_chat_route(base_url) {
        return Ok(());
    }
    let requires_signatures = google_model_requires_thought_signatures(model);
    let mut unsigned_call_ids: Vec<&str> = Vec::new();
    for message in messages {
        let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in tool_calls {
            let missing = call
                .pointer("/extra_content/google/thought_signature")
                .and_then(Value::as_str)
                .is_none();
            if missing {
                let id = call.get("id").and_then(Value::as_str).unwrap_or("?");
                if requires_signatures {
                    anyhow::bail!(
                        "Gemini model `{model}` requires a thought signature to replay tool call \
                         `{id}`, but none was captured (the turn predates signature capture, or \
                         the provider omitted it). Start a new session before using tools on \
                         this route."
                    );
                }
                unsigned_call_ids.push(id);
            }
        }
    }
    if !unsigned_call_ids.is_empty() {
        // Bounded: identifiers only, and only the first few of them.
        let sample = unsigned_call_ids
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        tracing::warn!(
            model = %model,
            unsigned_tool_calls = unsigned_call_ids.len(),
            sample_tool_call_ids = %sample,
            "replaying tool calls without Google thought signatures on the Gemini \
             OpenAI-compatible route; later signed tool turns may be rejected"
        );
    }
    Ok(())
}

/// Captured Google signatures ride on tool calls as
/// `extra_content.google.thought_signature`. Only Google's OpenAI-compat
/// endpoint may carry them on the wire; every other route gets them stripped
/// so a route switch never leaks Google-only fields to a foreign gateway.
///
/// Returns how many tool calls lost a signature, so the caller can report a
/// route switch that silently drops signed history instead of dropping it
/// without a receipt. Never returns or logs the signature bytes.
fn strip_google_tool_call_extra_content(messages: &mut [Value]) -> usize {
    let mut stripped = 0usize;
    for message in messages {
        let Some(tool_calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
            continue;
        };
        for call in tool_calls {
            if let Some(extra) = call.get_mut("extra_content")
                && let Some(obj) = extra.as_object_mut()
            {
                if obj.remove("google").is_some() {
                    stripped += 1;
                }
                if obj.is_empty() {
                    call.as_object_mut().map(|c| c.remove("extra_content"));
                }
            }
        }
    }
    stripped
}

fn mistral_model_has_adjustable_reasoning(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("mistral-medium") || model.starts_with("mistral-small")
}

fn mistral_model_has_native_reasoning(model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("magistral")
}

fn mistral_model_supports_reasoning(model: &str) -> bool {
    mistral_model_has_adjustable_reasoning(model) || mistral_model_has_native_reasoning(model)
}

fn mistral_reasoning_effort_wire_value(effort: &str) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" => Some("none"),
        "high" | "xhigh" | "max" | "highest" | "ultra" | "ultracode" => Some("high"),
        _ => None,
    }
}

/// Rewrite assistant messages that carry `reasoning_content` back into the
/// polymorphic `content: [{type: thinking, thinking: [{type: text, text: ...}],
/// closed: bool}, {type: text, text: ...}]` shape that Mistral la Plateforme
/// emits and accepts on replay. Mistral tolerates plain-string history in a
/// thinking-capable conversation, but replaying the original thinking trace
/// keeps multi-turn reasoning quality high per the official docs
/// (docs.mistral.ai/capabilities/reasoning). Non-assistant messages and
/// assistant messages without stored thinking are left untouched.
fn reshape_mistral_messages_for_reasoning_replay(messages: &mut [Value]) {
    for message in messages.iter_mut() {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        if object.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(reasoning) = object.remove("reasoning_content") else {
            continue;
        };
        let reasoning_text = reasoning
            .as_str()
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty());
        let Some(reasoning_text) = reasoning_text else {
            continue;
        };
        let text_content = object
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut blocks = vec![json!({
            "type": "thinking",
            "thinking": [{"type": "text", "text": reasoning_text}],
            "closed": true,
        })];
        if let Some(text) = text_content.filter(|s| !s.trim().is_empty()) {
            blocks.push(json!({"type": "text", "text": text}));
        }
        object.insert("content".to_string(), Value::Array(blocks));
    }
}

/// Extract thinking and text content from a Mistral polymorphic `content`
/// value. Mistral la Plateforme returns `content` as either a plain string
/// (default) or an array of typed blocks (`{type: "thinking", thinking:
/// [{type: "text", text: "..."}], closed: bool}` and `{type: "text", text:
/// "..."}`). This helper flattens the thinking sub-array into a single
/// string and returns any inline text separately. It ignores plain-string
/// `content` (returns `(None, None)`) so the shared string fallback still
/// runs for non-reasoning responses.
fn extract_mistral_polymorphic_content(value: &Value) -> (Option<String>, Option<String>) {
    let Some(array) = value.get("content").and_then(Value::as_array) else {
        return (None, None);
    };
    let mut thinking = String::new();
    let mut text = String::new();
    for block in array {
        let Some(kind) = block.get("type").and_then(Value::as_str) else {
            continue;
        };
        match kind {
            "thinking" => {
                if let Some(inner) = block.get("thinking").and_then(Value::as_array) {
                    for sub in inner {
                        if let Some(sub_text) = sub
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            thinking.push_str(sub_text);
                        }
                    }
                } else if let Some(inline) = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    thinking.push_str(inline);
                }
            }
            "text" => {
                if let Some(sub_text) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    text.push_str(sub_text);
                }
            }
            _ => {}
        }
    }
    let thinking = (!thinking.is_empty()).then_some(thinking);
    let text = (!text.is_empty()).then_some(text);
    (thinking, text)
}

fn apply_mistral_route_reasoning_controls(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) {
    if provider != ProviderKind::Mistral {
        return;
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("thinking");
        object.remove("reasoning_effort");
    }
    if !is_exact_mistral_chat_route(provider, base_url)
        || !mistral_model_has_adjustable_reasoning(model)
    {
        return;
    }
    let Some(effort) = effort else {
        return;
    };
    if let Some(wire) = mistral_reasoning_effort_wire_value(effort) {
        body["reasoning_effort"] = json!(wire);
    }
}

/// The direct K3 Chat Completions schema exposes fixed sampling behavior and
/// omits `temperature` and `top_p`. Strip legacy/generic values only from the
/// exact first-party route so compatible gateways keep their own contract.
/// Source: <https://platform.kimi.ai/docs/guide/kimi-k3-quickstart> (verified 2026-07-20).
fn apply_direct_moonshot_k3_fixed_sampling(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) {
    if !is_exact_direct_moonshot_k3_route(provider, base_url, model) {
        return;
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("temperature");
        object.remove("top_p");
    }
}

/// Kimi Code's documented membership models own their sampling behavior.
/// Strip generic controls only on the exact first-party membership route;
/// custom gateways and unknown model ids retain their own wire contract.
/// Source: <https://www.kimi.com/code/docs/en/third-party-tools/codex.html>
/// (verified 2026-08-26).
fn apply_kimi_code_fixed_sampling(
    body: &mut Value,
    provider: ProviderKind,
    base_url: &str,
    model: &str,
) {
    if provider != ProviderKind::Moonshot
        || !moonshot_base_url_is_exact_kimi_code(base_url)
        || !is_kimi_code_membership_model(model)
    {
        return;
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("temperature");
        object.remove("top_p");
    }
}

fn openai_compatible_reasoning_effort(
    effort: &str,
    supports_max: bool,
    supports_minimal: bool,
) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" => Some("none"),
        "minimal" if supports_minimal => Some("minimal"),
        "minimal" => Some("low"),
        "low" => Some("low"),
        "medium" | "mid" | "" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" | "highest" | "ultra" | "ultracode" if supports_max => Some("max"),
        "max" | "highest" | "ultra" | "ultracode" => Some("xhigh"),
        _ => None,
    }
}

fn mirror_minimax_reasoning_details_for_messages(messages: &mut [Value]) {
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if message.get("reasoning_details").is_some() {
            continue;
        }
        let Some(reasoning) = message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|reasoning| !reasoning.trim().is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        message["reasoning_details"] = json!([
            {
                "type": "text",
                "text": reasoning,
            }
        ]);
    }
}

fn mirror_minimax_reasoning_details_for_body(body: &mut Value, provider: ProviderKind) {
    if provider != ProviderKind::Minimax {
        return;
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    mirror_minimax_reasoning_details_for_messages(messages);
}

/// Sanitize every Moonshot chat tool in place, dropping only the tools whose
/// parameters cannot pass MFJS compatibility validation.
///
/// Per-tool degradation: a single incompatible tool (e.g. a third-party MCP
/// server whose schema uses keywords outside the MFJS whitelist) is excluded
/// from this request with a warning instead of failing the whole request
/// before transport. The tool name is safe to log — it is already visible in
/// the UI — while the error's `Display` deliberately carries no schema values.
///
/// Returns the names of the dropped tools, in catalog order.
fn sanitize_moonshot_chat_tools(chat_tools: &mut Vec<Value>) -> Vec<String> {
    let mut dropped = Vec::new();
    chat_tools.retain_mut(|tool| {
        let Some(function) = tool
            .as_object_mut()
            .and_then(|tool| tool.get_mut("function"))
            .and_then(Value::as_object_mut)
        else {
            return true;
        };
        let Some(parameters) = function.get_mut("parameters") else {
            return true;
        };
        match crate::tools::schema_sanitize::sanitize_for_kimi_parameters(parameters) {
            Ok(note) => {
                if let Some(note) = note {
                    let description = function
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let description = if description.is_empty() {
                        note
                    } else {
                        format!("{description} {note}")
                    };
                    function.insert("description".to_string(), json!(description));
                }
                true
            }
            Err(error) => {
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("<unnamed>")
                    .to_string();
                tracing::warn!(
                    tool = %name,
                    error = %error,
                    "dropping Moonshot tool from this request: parameters failed safe compatibility validation"
                );
                dropped.push(name);
                false
            }
        }
    });
    dropped
}

/// The final Chat Completions wire payload for one request.
///
/// Produced by [`build_chat_wire_body`], the single place where a
/// `MessageRequest` becomes Chat-shaped JSON. It is reached only through
/// [`super::CodewhaleClient::prepare_outbound_request`], the shared outbound
/// seam that the blocking transport, the streaming transport, and
/// `/preview-request` all consume — so a preview cannot drift from what would
/// be sent, and no other dialect is projected through this builder.
///
/// Seam concept harvested from PR #1099 (`build_sanitized_chat_completion_body`)
/// by TaoMu (GTC2080); re-implemented against the current client shape.
pub(crate) struct ChatWireBody {
    /// Provider-shaped JSON body, post-sanitizers.
    pub(crate) body: Value,
    /// The model id actually placed on the wire (may differ from the
    /// configured/display model for routed providers).
    pub(crate) model: String,
    /// Tokens re-sent because thinking-mode replay substituted
    /// `reasoning_content`. Only computed on the streaming path, which is the
    /// only path that runs the replay sanitizer today.
    pub(crate) replay_input_tokens: Option<u32>,
    /// Wire-normalized tool names omitted because Moonshot's MFJS validator
    /// rejected their parameter schemas. Kept outside the wire body so the
    /// caller can surface one bounded diagnostic without leaking schema data.
    pub(crate) omitted_tool_names: Vec<String>,
}

/// Build the Chat Completions wire body for `request`.
///
/// `stream` selects the streaming shape (`stream` + `stream_options`) and, to
/// preserve historical behavior exactly, also gates the thinking-mode replay
/// sanitizer — the blocking path has never run it.
pub(crate) fn build_chat_wire_body(
    request: &MessageRequest,
    provider: ProviderKind,
    base_url: &str,
    stream: bool,
    route_limits: Option<RouteLimits>,
) -> Result<ChatWireBody> {
    let messages = PromptBuilder::for_request(request).build_for_provider_and_route(
        provider,
        base_url,
        route_limits,
    );
    let model = {
        let wire = wire_model_for_provider_route(provider, base_url, &request.model);
        codewhale_models::effective_muse_wire_id(&wire).to_string()
    };
    validate_google_thought_signature_replay(base_url, &model, &messages)?;
    let mut body = if stream {
        json!({
            "model": model.clone(),
            "messages": messages,
            "max_tokens": request.max_tokens,
            "stream": true,
            "stream_options": {
                "include_usage": true
            },
        })
    } else {
        json!({
            "model": model.clone(),
            "messages": messages,
            "max_tokens": request.max_tokens,
        })
    };
    apply_provider_token_limit(&mut body, provider, base_url, &model, request.max_tokens);

    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = json!(top_p);
    }
    let mut omitted_tool_names = Vec::new();
    if let Some(tools) = request.tools.as_ref() {
        let mut chat_tools: Vec<_> = tools
            .iter()
            .map(|tool| tool_to_chat_for_base_url(tool, base_url))
            .collect();
        // Moonshot function parameters must end at a plain object root.
        // Flatten root composition, preserve valid nested anyOf, and drop
        // only the tools whose parameters cannot pass MFJS validation so one
        // incompatible tool never sinks the whole request.
        if matches!(provider, crate::config::ProviderKind::Moonshot) {
            omitted_tool_names = sanitize_moonshot_chat_tools(&mut chat_tools);
        }
        // xAI rejects a parameters root that is not a plain object schema
        // (e.g. apply_patch's root `oneOf` required-groups) with a 400.
        if matches!(provider, crate::config::ProviderKind::Xai) {
            for t in &mut chat_tools {
                let Some(function) = t
                    .as_object_mut()
                    .and_then(|t| t.get_mut("function"))
                    .and_then(|f| f.as_object_mut())
                else {
                    continue;
                };
                let note = function.get_mut("parameters").and_then(|parameters| {
                    crate::tools::schema_sanitize::sanitize_for_xai_parameters(parameters)
                });
                if let Some(note) = note
                    && let Some(description) = function
                        .get_mut("description")
                        .and_then(|d| d.as_str().map(str::to_string))
                {
                    function.insert(
                        "description".to_string(),
                        json!(format!("{description} {note}")),
                    );
                }
            }
        }
        // When per-tool degradation (or the caller) left no tools, omit the
        // key entirely: an empty `tools` array — or a `tool_choice` pointing
        // at a dropped tool — is itself a fresh 400 on strict providers.
        if !chat_tools.is_empty() {
            body["tools"] = json!(chat_tools);
        }
    }
    if should_send_tool_choice_for_chat(provider, request.reasoning_effort.as_deref())
        && let Some(choice) = request.tool_choice.as_ref()
        && let Some(mapped) = map_tool_choice_for_chat(choice)
    {
        if matches!(provider, crate::config::ProviderKind::Moonshot)
            && let Some(name) = mapped.pointer("/function/name").and_then(Value::as_str)
            && omitted_tool_names.iter().any(|omitted| omitted == name)
        {
            bail!(
                "Moonshot cannot force tool '{name}' because its input schema is incompatible with this route"
            );
        }
        if body.get("tools").is_some() {
            body["tool_choice"] = mapped;
        }
    }
    apply_route_reasoning_controls(
        &mut body,
        provider,
        base_url,
        &model,
        request.reasoning_effort.as_deref(),
    );
    apply_direct_moonshot_k3_fixed_sampling(&mut body, provider, base_url, &model);
    apply_kimi_code_fixed_sampling(&mut body, provider, base_url, &model);

    // Bulletproof final sanitizer: walk the wire payload and force
    // `reasoning_content` onto any assistant message that has tool_calls
    // but no reasoning_content. DeepSeek's thinking-mode API rejects
    // such messages with a 400. This is the last line of defense after
    // engine-side and build-side substitution; if either upstream path
    // misses a case (e.g. a session restored from disk, a sub-agent
    // adding messages directly, or a cached prefix mismatch), this pass
    // still produces a valid request.
    let replay_input_tokens = if stream {
        sanitize_thinking_mode_messages_for_route(
            &mut body,
            &model,
            request.reasoning_effort.as_deref(),
            provider,
            base_url,
        )
    } else {
        None
    };
    mirror_minimax_reasoning_details_for_body(&mut body, provider);

    Ok(ChatWireBody {
        body,
        model,
        replay_input_tokens,
        omitted_tool_names,
    })
}

impl CodewhaleClient {
    pub(super) async fn create_message_chat(
        &self,
        prepared: &super::PreparedOutboundRequest,
        cacheable: bool,
    ) -> Result<MessageResponse> {
        let body = &prepared.body;

        let response_cache_key = if cacheable && self.plugin_provider.is_none() {
            let wire_body =
                serde_json::to_vec(&body).context("Failed to serialize Chat API cache key")?;
            let key = crate::llm_response_cache::ResponseCache::make_key(
                self.api_provider.as_str(),
                &self.base_url,
                self.path_suffix.as_deref(),
                &self.api_key,
                &wire_body,
            );
            if let Some(cached) = crate::llm_response_cache::response_cache().get(&key) {
                return Ok(cached);
            }
            Some(key)
        } else {
            None
        };

        // The endpoint was resolved by the shared seam alongside the body, so
        // a route-shape decision (e.g. DeepSeek's strict-tools `/beta` path)
        // cannot be made twice with two different answers.
        let url = prepared.endpoint.url.as_str();
        let response = self.send_json_with_retry(url, body).await?;

        let status = response.status();
        crate::client::record_provider_response(self.api_provider, status.as_u16());
        if !status.is_success() {
            let raw_error_text = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            let error_text = self.disclosed_http_error_body(
                &super::ErrorBodyDisclosure::Full,
                status.as_u16(),
                &raw_error_text,
            );
            anyhow::bail!(
                "Failed to call {} Chat Completions API: HTTP {status}: {error_text}",
                self.api_provider.provider().display_name()
            );
        }

        let response_text = response
            .text()
            .await
            .context("Failed to read Chat API response body")?;
        let value: Value =
            serde_json::from_str(&response_text).context("Failed to parse Chat API JSON")?;
        let parsed = parse_chat_message_for_route(&value, self.api_provider, &self.base_url)?;
        if let Some(key) = response_cache_key {
            crate::llm_response_cache::response_cache().put(key, parsed.clone());
        }
        Ok(parsed)
    }
}

impl CodewhaleClient {
    async fn open_chat_stream_response(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<(reqwest::Response, Duration)> {
        let open_req = self.stream_open_request();
        let idle_timeout = open_req.idle_timeout;
        let response = super::stream_entry::open_sse_response(&open_req, |policy| async move {
            match policy {
                // The prebuilt HTTP/1.1 twin carries the same default
                // headers/auth; send once, without the JSON retry loop
                // (matching the pre-seam H1-pin behavior).
                super::stream_entry::StreamHttpPolicy::Http1Only => {
                    let client = super::stream_entry::client_for_policy(
                        &self.http_client,
                        self.http1_fallback_client(),
                        policy,
                    );
                    Ok(self
                        .authorize_plugin_request(
                            client
                                .post(url)
                                .header(reqwest::header::CONTENT_TYPE, "application/json")
                                .json(body),
                        )
                        .await?
                        .send()
                        .await?)
                }
                super::stream_entry::StreamHttpPolicy::DualWithH1Fallback => {
                    // Stream open, not a JSON retry: the response body outlives
                    // the open, so this path must not carry any total deadline
                    // (`open_stream_json_with_retry`, not `send_json_with_retry`).
                    self.open_stream_json_with_retry(url, body).await
                }
            }
        })
        .await?;
        Ok((response, idle_timeout))
    }

    pub(super) async fn handle_chat_completion_stream(
        &self,
        prepared: super::PreparedOutboundRequest,
    ) -> Result<StreamEventBox> {
        // Try true SSE streaming via chat completions (widely supported).
        // Body and endpoint both come from the shared prepared-request seam,
        // so a preview or a non-stream call can never diverge from the
        // streamed request.
        let super::PreparedOutboundRequest {
            body,
            wire_model: model,
            replay_input_tokens,
            endpoint,
            ..
        } = prepared;
        let url = endpoint.url;

        let (response, stream_idle_timeout) = self.open_chat_stream_response(&url, &body).await?;

        let status = response.status();
        crate::client::record_provider_response(self.api_provider, status.as_u16());
        if !status.is_success() {
            let raw_error_text = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            let error_text = self.disclosed_http_error_body(
                &super::ErrorBodyDisclosure::Full,
                status.as_u16(),
                &raw_error_text,
            );
            // If DeepSeek rejected for missing reasoning_content despite the
            // sanitizer, dump the offending indices so we can diagnose where
            // they came from on the next failure.
            if error_text.contains("reasoning_content") {
                log_thinking_mode_violations(&body);
            }
            anyhow::bail!("SSE stream request failed: HTTP {status}: {error_text}");
        }

        let api_provider = self.api_provider;
        let base_url = self.base_url.clone();

        // Capture transport-shape headers before we consume `response` into
        // `bytes_stream()`. They are surfaced in the decode-error log path so
        // we can tell HTTP/2 RST_STREAM from chunked-encoding corruption from
        // gzip-compressor failure when investigating #103.
        let response_headers = format_stream_headers(response.headers());
        let byte_stream = response.bytes_stream();
        let configured_reasoning_stream_style = self.reasoning_stream_style.clone();

        let stream = async_stream::stream! {
            use futures_util::StreamExt;

            // Emit a synthetic MessageStart
            yield Ok(StreamEvent::MessageStart {
                message: MessageResponse {
                    id: String::new(),
                    r#type: "message".to_string(),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    model: model.clone(),
                    stop_reason: None,
                    stop_sequence: None,
                    container: None,
                    usage: Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                        ..Usage::default()
                    },
                },
            });

            let mut line_buf = String::new();
            let mut byte_buf = acquire_stream_buffer();
            let mut content_index: u32 = 0;
            let mut text_started = false;
            let mut thinking_started = false;
            let mut tool_indices: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
            let mut reasoning_detail_buffers: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
            let mut inline_reasoning_tags = InlineReasoningTagState::default();
            let reasoning_stream_style = reasoning_stream_style_for_route(
                api_provider,
                &base_url,
                &model,
                configured_reasoning_stream_style.as_deref(),
            );

            let mut byte_stream = std::pin::pin!(byte_stream);
            let idle = stream_idle_timeout;

            // Telemetry for #103 stream-decode diagnostics: bytes received
            // since the start of this stream and last successful event time.
            // Surfaces in the error log when reqwest yields a chunk error so
            // we can tell HTTP/2 RST_STREAM from chunk-decode-failure from
            // gzip-corruption when investigating a flaky session.
            let stream_start = std::time::Instant::now();
            let mut last_event_at = std::time::Instant::now();
            let mut bytes_received: usize = 0;
            // Set when a `[DONE]` sentinel was seen, so the post-loop flush does
            // not re-process trailing post-DONE bytes.
            let mut saw_done = false;
            // A number of OpenAI-compatible providers omit `[DONE]` but send a
            // terminal `finish_reason`. Either is valid terminal proof. A raw
            // HTTP EOF with neither is not: treating that as MessageStop turns
            // a truncated provider response into a successful empty turn.
            let mut saw_finish_reason = false;
            // Once an error has been emitted, do not follow it with a synthetic
            // MessageStop (or a second, less-specific premature-EOF error).
            let mut stream_failed = false;
            // Set when a complete line or unterminated flush failed UTF-8.
            // Skip further data-frame parsing so U+FFFD cannot enter the transcript.
            let mut decode_failed = false;

            let first_byte = super::stream_entry::first_byte_timeout(idle);
            'stream: loop {
                let wait = super::stream_entry::next_chunk_timeout(idle, first_byte, bytes_received);
                let chunk_result = match tokio_timeout(wait, byte_stream.next()).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break, // Stream ended normally
                    Err(_elapsed) => {
                        stream_failed = true;
                        yield Err(anyhow::anyhow!(super::stream_entry::body_timeout_message(
                            wait,
                            bytes_received,
                            stream_start.elapsed(),
                            last_event_at.elapsed(),
                            api_provider.provider().display_name(),
                        )));
                        break;
                    }
                };
                let chunk = match chunk_result {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        stream_failed = true;
                        // Walk the error source chain so reqwest's underlying
                        // hyper / h2 / io error is visible — without this the
                        // outer "error decoding response body" message tells
                        // us nothing about WHY the stream died.
                        let mut error_chain = format!("{e}");
                        let mut current: Option<&(dyn std::error::Error + 'static)> =
                            std::error::Error::source(&e);
                        while let Some(source) = current {
                            error_chain.push_str(&format!(" -> {source}"));
                            current = std::error::Error::source(source);
                        }
                        crate::logging::warn(format!(
                            "Stream read error: {error_chain} \
                             (elapsed: {}ms, bytes_received: {}, ms_since_last_event: {}, headers: {})",
                            stream_start.elapsed().as_millis(),
                            bytes_received,
                            last_event_at.elapsed().as_millis(),
                            response_headers,
                        ));
                        yield Err(anyhow::anyhow!("Stream read error: {e}"));
                        break;
                    }
                };

                bytes_received = bytes_received.saturating_add(chunk.len());
                last_event_at = std::time::Instant::now();
                byte_buf.extend_from_slice(&chunk);

                // Guard against unbounded buffer growth (e.g., malformed stream without newlines)
                const MAX_SSE_BUF: usize = 10 * 1024 * 1024; // 10 MB
                if byte_buf.len() > MAX_SSE_BUF {
                    stream_failed = true;
                    yield Err(anyhow::anyhow!("SSE buffer exceeded {MAX_SSE_BUF} bytes — aborting stream"));
                    break;
                }

                if byte_buf.len() > SSE_BACKPRESSURE_HIGH_WATERMARK {
                    tokio::time::sleep(Duration::from_millis(SSE_BACKPRESSURE_SLEEP_MS)).await;
                }

                // Process complete SSE lines from the buffer. Decode only after
                // a `\n` so an HTTP/2 DATA split mid-character cannot become
                // U+FFFD; genuine invalid bytes fail closed.
                let mut lines_processed = 0usize;
                loop {
                    let line = match take_sse_line(&mut byte_buf) {
                        Ok(Some(line)) => line,
                        Ok(None) => break,
                        Err(err) => {
                            decode_failed = true;
                            stream_failed = true;
                            yield Err(anyhow::anyhow!("{err}"));
                            break 'stream;
                        }
                    };

                    if line.is_empty() {
                        // Empty line = event boundary, process accumulated data
                        if !line_buf.is_empty() {
                            let data = std::mem::take(&mut line_buf);
                            match parse_sse_data_frame(
                                &data,
                                &mut content_index,
                                &mut text_started,
                                &mut thinking_started,
                                &mut tool_indices,
                                &mut reasoning_detail_buffers,
                                &mut inline_reasoning_tags,
                                reasoning_stream_style,
                            ) {
                                SseDataFrame::Done => {
                                    saw_done = true;
                                    break 'stream;
                                }
                                SseDataFrame::Events(events) => {
                                    for mut event in events {
                                        saw_finish_reason |= matches!(
                                            &event,
                                            StreamEvent::MessageDelta { delta, .. }
                                                if delta.stop_reason.as_deref().is_some_and(|reason| !reason.trim().is_empty())
                                        );
                                        // Stamp the client-side replay-token estimate
                                        // onto the final usage so the UI can surface
                                        // it (#30). We compute it pre-request and
                                        // overlay it on the server-reported usage at
                                        // stream completion.
                                        if let Some(tokens) = replay_input_tokens
                                            && let StreamEvent::MessageDelta {
                                                usage: Some(usage),
                                                ..
                                            } = &mut event
                                        {
                                            usage.reasoning_replay_tokens = Some(tokens);
                                        }
                                        yield Ok(event);
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    if line.starts_with(':') {
                        // SSE comment (`: keep-alive`, `: OPENROUTER PROCESSING`).
                        // Surface it as a ping so the engine counts a provider
                        // that is alive but queued/thinking as progress
                        // (#6184) instead of timing out on a live stream.
                        yield Ok(StreamEvent::Ping);
                        continue;
                    }

                    if let Some(data) = extract_sse_data_value(&line)
                        && let Err(err) = push_sse_event_data(&mut line_buf, data)
                    {
                        decode_failed = true;
                        stream_failed = true;
                        yield Err(anyhow::anyhow!("{err}"));
                        break 'stream;
                    }
                    // Ignore other SSE fields (event:, id:, retry:)

                    lines_processed = lines_processed.saturating_add(1);
                    if lines_processed >= SSE_MAX_LINES_PER_CHUNK {
                        // Backpressure relief: hand the executor a turn so a
                        // slow consumer is not starved. Keep draining after
                        // that — leaving complete lines buffered would strand
                        // them, because the outer loop only resumes draining
                        // once ANOTHER chunk arrives and the end-of-stream
                        // flush treats the whole remainder as a single
                        // unterminated line.
                        lines_processed = 0;
                        tokio::task::yield_now().await;
                    }
                }
            }

            // Flush a final SSE frame that arrived without a terminating blank
            // line (the stream closed straight after the last `data:` line, or
            // that line lacked a trailing newline). Without this the final delta
            // — last tokens, finish_reason, and usage — is silently dropped.
            // Skipped after `[DONE]`, whose frame was already processed, and
            // after a fail-closed UTF-8 error.
            if !saw_done && !decode_failed {
                match flush_sse_line(&mut byte_buf) {
                    Ok(Some(line)) => {
                        if let Some(data) = extract_sse_data_value(&line)
                            && let Err(err) = push_sse_event_data(&mut line_buf, data)
                        {
                            decode_failed = true;
                            stream_failed = true;
                            yield Err(anyhow::anyhow!("{err}"));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        decode_failed = true;
                        stream_failed = true;
                        yield Err(anyhow::anyhow!("{err}"));
                    }
                }
                if !decode_failed && !line_buf.is_empty() {
                    let data = std::mem::take(&mut line_buf);
                    match parse_sse_data_frame(
                        &data,
                        &mut content_index,
                        &mut text_started,
                        &mut thinking_started,
                        &mut tool_indices,
                        &mut reasoning_detail_buffers,
                        &mut inline_reasoning_tags,
                        reasoning_stream_style,
                    ) {
                        SseDataFrame::Done => saw_done = true,
                        SseDataFrame::Events(events) => {
                            for mut event in events {
                                saw_finish_reason |= matches!(
                                    &event,
                                    StreamEvent::MessageDelta { delta, .. }
                                        if delta.stop_reason.as_deref().is_some_and(|reason| !reason.trim().is_empty())
                                );
                                if let Some(tokens) = replay_input_tokens
                                    && let StreamEvent::MessageDelta {
                                        usage: Some(usage), ..
                                    } = &mut event
                                {
                                    usage.reasoning_replay_tokens = Some(tokens);
                                }
                                yield Ok(event);
                            }
                        }
                    }
                }
            }

            // Close any open blocks — content_index points to the
            // currently active open block (it is only incremented
            // *after* a block is closed, not when opened).
            if thinking_started || text_started {
                yield Ok(StreamEvent::ContentBlockStop { index: content_index });
            }

            release_stream_buffer(byte_buf);
            if !stream_failed && (saw_done || saw_finish_reason) {
                yield Ok(StreamEvent::MessageStop);
            } else if !stream_failed {
                yield Err(anyhow::anyhow!(
                    "Chat Completions stream closed before [DONE] or finish_reason"
                ));
            }
        };

        Ok(Pin::from(Box::new(stream)
            as Box<
                dyn futures_util::Stream<Item = Result<StreamEvent>> + Send,
            >))
    }
}

// === Chat Completions Helpers ===

#[cfg(test)]
pub(super) fn build_chat_messages(
    system: Option<&SystemPrompt>,
    messages: &[Message],
    model: &str,
) -> Vec<Value> {
    build_chat_messages_with_reasoning(
        system,
        messages,
        tool_result_sent_char_budget(model),
        should_replay_reasoning_content(model, None),
        false,
    )
}

#[cfg(test)]
pub(super) fn build_chat_messages_for_request(request: &MessageRequest) -> Vec<Value> {
    PromptBuilder::for_request(request).build()
}

#[cfg(test)]
pub(super) fn build_chat_messages_for_request_and_provider(
    request: &MessageRequest,
    provider: ProviderKind,
) -> Vec<Value> {
    build_chat_messages_for_request_and_provider_and_route(request, provider, "")
}

/// Build a wire prompt for one fully resolved provider route.
///
/// Most provider behavior is keyed only by the provider kind and model. Kimi
/// Code K3 is deliberately narrower: the bare `k3` model owns reasoning
/// replay only on its official membership-plan endpoint, so callers that have
/// a concrete base URL must retain it through prompt construction.
#[cfg(test)]
pub(super) fn build_chat_messages_for_request_and_provider_and_route(
    request: &MessageRequest,
    provider: ProviderKind,
    base_url: &str,
) -> Vec<Value> {
    PromptBuilder::for_request(request).build_for_provider_and_route(provider, base_url, None)
}

pub(crate) fn inspect_prompt_for_request(request: &MessageRequest) -> PromptInspection {
    PromptBuilder::for_request(request).inspect()
}

pub(crate) fn build_cache_warmup_request(request: &MessageRequest) -> MessageRequest {
    PromptBuilder::for_request(request).build_cache_warmup_request()
}

struct PromptBuilder<'a> {
    system: Option<&'a SystemPrompt>,
    messages: &'a [Message],
    tools: Option<&'a [Tool]>,
    model: &'a str,
    reasoning_effort: Option<&'a str>,
}

impl<'a> PromptBuilder<'a> {
    fn for_request(request: &'a MessageRequest) -> Self {
        Self {
            system: request.system.as_ref(),
            messages: &request.messages,
            tools: request.tools.as_deref(),
            model: &request.model,
            reasoning_effort: request.reasoning_effort.as_deref(),
        }
    }

    #[cfg(test)]
    fn build(self) -> Vec<Value> {
        build_chat_messages_with_reasoning(
            self.system,
            self.messages,
            tool_result_sent_char_budget(self.model),
            should_replay_reasoning_content(self.model, self.reasoning_effort),
            false,
        )
    }

    fn build_for_provider_and_route(
        self,
        provider: ProviderKind,
        base_url: &str,
        route_limits: Option<RouteLimits>,
    ) -> Vec<Value> {
        let mut messages = build_chat_messages_with_reasoning(
            self.system,
            self.messages,
            crate::route_budget::route_inline_char_budget_for_route(
                provider,
                self.model,
                route_limits,
            ),
            should_replay_reasoning_content_for_provider_on_route(
                provider,
                base_url,
                self.model,
                self.reasoning_effort,
            ),
            false,
        );
        if provider == ProviderKind::Arcee {
            apply_arcee_waf_safe_message_encoding(&mut messages);
        }
        if provider == ProviderKind::Minimax {
            mirror_minimax_reasoning_details_for_messages(&mut messages);
        }
        if is_exact_mistral_chat_route(provider, base_url) {
            reshape_mistral_messages_for_reasoning_replay(&mut messages);
        }
        if !is_google_openai_compat_chat_route(base_url) {
            // A signature captured on Google's endpoint is meaningless — and
            // potentially a leak — anywhere else, so it is stripped. Say so:
            // the model will behave differently on the replayed tool history,
            // and a silent strip is exactly what made this defect invisible.
            let stripped = strip_google_tool_call_extra_content(&mut messages);
            if stripped > 0 {
                tracing::warn!(
                    provider = ?provider,
                    stripped_tool_calls = stripped,
                    "dropping captured Google thought signatures: this route is not Google's \
                     OpenAI-compatible endpoint, so the replayed tool history is unsigned"
                );
            }
        }
        messages
    }

    fn inspect(self) -> PromptInspection {
        let messages = build_chat_messages_with_reasoning(
            self.system,
            self.messages,
            tool_result_sent_char_budget(self.model),
            should_replay_reasoning_content(self.model, self.reasoning_effort),
            true,
        );
        inspect_wire_request(self.tools, &messages)
    }

    fn build_cache_warmup_request(self) -> MessageRequest {
        let system = stable_system_prompt(self.system);
        let mut messages = stable_history_messages(self.messages);
        let tools = self
            .tools
            .filter(|tools| !tools.is_empty())
            .map(<[Tool]>::to_vec);
        let tool_choice = tools.as_ref().map(|_| json!("none"));
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: CACHE_WARMUP_USER_TAIL.to_string(),
                cache_control: None,
            }],
        });

        MessageRequest {
            model: self.model.to_string(),
            messages,
            max_tokens: CACHE_WARMUP_MAX_TOKENS,
            system,
            tools,
            tool_choice,
            metadata: None,
            thinking: None,
            // Warmup has an intentionally tiny answer contract ("OK"). Do not
            // let hidden reasoning consume that allowance before the cacheable
            // prefix is accepted by the provider.
            reasoning_effort: Some("off".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        }
    }
}

const SYSTEM_PROMPT_DUMP_ENV: &str = "CODEWHALE_DUMP_SYSTEM_PROMPT";
const SYSTEM_PROMPT_DUMP_BEGIN: &str = "<<<CODEWHALE_SYSTEM_PROMPT_BEGIN>>>";
const SYSTEM_PROMPT_DUMP_END: &str = "<<<CODEWHALE_SYSTEM_PROMPT_END>>>";
const ARCEE_WAF_TEXT_SPLIT_TRIGGERS: &[(&str, &str, &str)] = &[("python -c", "python ", "-c")];

/// Dump the wire prefix (system region + tools array) when the
/// system-prompt dump flag is set, so prefix-token audits can measure the
/// catalog beside the prompt. Called once from
/// [`PreparedOutboundRequest::new`](super::prepared::PreparedOutboundRequest::new),
/// the single funnel for all three wire dialects, so the bytes are the
/// exact-final-body bytes. Debug-only, zero production impact.
pub(crate) fn dump_wire_prefix_if_requested(body: &Value) {
    let Ok(flag) = std::env::var(SYSTEM_PROMPT_DUMP_ENV) else {
        return;
    };
    if !matches!(flag.trim(), "1" | "true" | "TRUE" | "yes" | "YES") {
        return;
    }
    let mut stderr = std::io::stderr().lock();
    if let Some(prompt) = wire_system_text(body) {
        let _ = writeln!(stderr, "{SYSTEM_PROMPT_DUMP_BEGIN}");
        let _ = writeln!(stderr, "{prompt}");
        let _ = writeln!(stderr, "{SYSTEM_PROMPT_DUMP_END}");
    }
    if let Some(tools) = body.get("tools") {
        let _ = writeln!(stderr, "<<<CODEWHALE_WIRE_TOOLS_BEGIN>>>");
        let _ = writeln!(stderr, "{tools}");
        let _ = writeln!(stderr, "<<<CODEWHALE_WIRE_TOOLS_END>>>");
    }
}

/// Best-effort system-region text across the three wire dialects:
/// Anthropic `system` (string or blocks), Responses `instructions`, and
/// chat `messages[]` with a system/developer role.
fn wire_system_text(body: &Value) -> Option<String> {
    if let Some(system) = body.get("system") {
        return Some(match system {
            Value::String(text) => text.clone(),
            Value::Array(parts) => {
                let text = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    system.to_string()
                } else {
                    text
                }
            }
            _ => system.to_string(),
        });
    }
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        return Some(instructions.to_string());
    }
    body.get("messages")?
        .as_array()?
        .iter()
        .find_map(|message| {
            if !matches!(
                message.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            ) {
                return None;
            }
            match message.get("content")? {
                Value::String(text) => Some(text.clone()),
                Value::Array(parts) => {
                    // Same "\n" block join as the Anthropic arm above, so
                    // the audit text is dialect-consistent.
                    let text = parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!text.is_empty()).then_some(text)
                }
                _ => None,
            }
        })
}

fn apply_arcee_waf_safe_message_encoding(messages: &mut [Value]) {
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("system") {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_str) else {
            continue;
        };
        let Some(parts) = arcee_waf_safe_text_parts(content) else {
            continue;
        };
        message["content"] = json!(parts);
    }
}

fn arcee_waf_safe_text_parts(content: &str) -> Option<Vec<Value>> {
    let mut parts = Vec::new();
    let mut cursor = 0usize;
    let mut split_any = false;

    while cursor < content.len() {
        let Some((trigger_start, trigger, left, right)) = next_arcee_waf_trigger(content, cursor)
        else {
            push_text_part(&mut parts, &content[cursor..]);
            break;
        };

        push_text_part(&mut parts, &content[cursor..trigger_start]);
        push_text_part(&mut parts, left);
        push_text_part(&mut parts, right);
        cursor = trigger_start + trigger.len();
        split_any = true;
    }

    split_any.then_some(parts)
}

fn next_arcee_waf_trigger(content: &str, cursor: usize) -> Option<(usize, &str, &str, &str)> {
    ARCEE_WAF_TEXT_SPLIT_TRIGGERS
        .iter()
        .filter_map(|(trigger, left, right)| {
            content[cursor..]
                .find(trigger)
                .map(|offset| (cursor + offset, *trigger, *left, *right))
        })
        .min_by_key(|(start, _, _, _)| *start)
}

fn push_text_part(parts: &mut Vec<Value>, text: &str) {
    if !text.is_empty() {
        parts.push(json!({
            "type": "text",
            "text": text,
        }));
    }
}

pub(crate) const CACHE_WARMUP_USER_TAIL: &str = "请只回复 OK";
pub(crate) const CACHE_WARMUP_MAX_TOKENS: u32 = 8;
/// Wire backstop for tool results (#6508). The engine already fits every
/// result it gives the model to the route's inline budget, and marks any cut
/// with a recovery footer. This pass only catches history that never went
/// through the engine (legacy or restored raw results). Model-only inspection
/// uses the catalog window; outbound requests pass their resolved route budget.
/// Results with an engine recovery footer remain intact.
fn tool_result_sent_char_budget(model: &str) -> usize {
    crate::route_budget::route_inline_char_budget(codewhale_models::context_window_for_model(model))
}
/// Characters of an excerpted wire result spent on its labelled header.
const TOOL_RESULT_EXCERPT_FRAME_CHARS: usize = 1_024;
/// Tool results shorter than this stay inline even when repeated. The
/// extra prompt bytes are cheaper than adding an earlier-message reference
/// for tiny command outputs.
const TOOL_RESULT_DEDUP_MIN_CHARS: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PromptInspection {
    pub base_static_prefix_hash: String,
    pub full_request_prefix_hash: String,
    /// Hash of the rendered tool catalog JSON, or empty when no tools were supplied.
    pub tool_catalog_hash: String,
    pub layers: Vec<PromptLayerInspection>,
}

/// Identifies the stable prefix that a cache warmup primes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CacheWarmupKey {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub static_prefix_hash: String,
    pub tool_catalog_hash: String,
    pub project_pack_hash: String,
    pub skills_hash: String,
}

impl CacheWarmupKey {
    pub(crate) fn from_inspection(
        provider: &str,
        model: &str,
        base_url: &str,
        inspection: &PromptInspection,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: base_url.to_string(),
            static_prefix_hash: inspection.base_static_prefix_hash.clone(),
            tool_catalog_hash: inspection.tool_catalog_hash.clone(),
            project_pack_hash: layer_hash(inspection, "Project context pack"),
            skills_hash: layer_hash(inspection, "Skills"),
        }
    }

    pub(crate) fn hash_short(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let hash = sha256_hex(json.as_bytes());
        hash[..hash.len().min(12)].to_string()
    }
}

fn layer_hash(inspection: &PromptInspection, name: &str) -> String {
    inspection
        .layers
        .iter()
        .find(|layer| layer.name == name)
        .map(|layer| layer.sha256.clone())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PromptLayerInspection {
    pub name: String,
    pub stability: PromptLayerStability,
    pub char_len: usize,
    pub byte_len: usize,
    /// Rough token estimate for quick before/after cache-hit reports.
    pub token_estimate: usize,
    pub sha256: String,
    pub tool_result: Option<ToolResultInspection>,
    pub turn_meta: Option<TurnMetaInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolResultInspection {
    pub original_chars: usize,
    pub sent_chars: usize,
    pub truncated: bool,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TurnMetaInspection {
    pub original_chars: usize,
    pub sent_chars: usize,
    pub deduplicated: bool,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PromptLayerStability {
    Static,
    History,
    Dynamic,
}

#[cfg(test)]
impl PromptLayerStability {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::History => "history",
            Self::Dynamic => "dynamic",
        }
    }
}

fn inspect_wire_request(tools: Option<&[Tool]>, messages: &[Value]) -> PromptInspection {
    let mut layers = Vec::new();
    let mut base_static_prefix_parts = Vec::new();
    let mut full_request_prefix_parts = Vec::new();
    let mut tool_catalog_hash = String::new();
    let mut start_index = 0;

    if let Some(message) = messages.first() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let content = message_content_for_inspect(message);
        if role == "system" {
            for (name, stability, body) in split_system_layers(&content) {
                if stability == PromptLayerStability::Static {
                    base_static_prefix_parts.push(body.to_string());
                }
                if stability != PromptLayerStability::Dynamic {
                    full_request_prefix_parts.push(body.to_string());
                }
                layers.push(prompt_layer(name, stability, body));
            }
            start_index = 1;
        }
    }

    if let Some(tool_catalog) = tool_catalog_for_inspect(tools) {
        tool_catalog_hash = sha256_hex(tool_catalog.as_bytes());
        base_static_prefix_parts.push(tool_catalog.clone());
        full_request_prefix_parts.push(tool_catalog.clone());
        layers.push(prompt_layer(
            "Tool catalog".to_string(),
            PromptLayerStability::Static,
            &tool_catalog,
        ));
    }

    for (index, message) in messages.iter().enumerate().skip(start_index) {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let content = message_content_for_inspect(message);
        let is_last = index + 1 == messages.len();
        let stability = if (is_last && role == "user") || role == "tool" {
            PromptLayerStability::Dynamic
        } else {
            PromptLayerStability::History
        };
        let name = if is_last && role == "user" {
            "User task".to_string()
        } else {
            format!("Message #{index} {role}")
        };
        if stability != PromptLayerStability::Dynamic {
            full_request_prefix_parts.push(content.clone());
        }
        let mut layer = prompt_layer(name, stability, &content);
        layer.tool_result = tool_result_inspection_for_message(message);
        layer.turn_meta = turn_meta_inspection_for_message(message);
        layers.push(layer);
    }

    let base_static_prefix = base_static_prefix_parts.join("\n");
    let full_request_prefix = full_request_prefix_parts.join("\n");

    PromptInspection {
        base_static_prefix_hash: sha256_hex(base_static_prefix.as_bytes()),
        full_request_prefix_hash: sha256_hex(full_request_prefix.as_bytes()),
        tool_catalog_hash,
        layers,
    }
}

fn tool_catalog_for_inspect(tools: Option<&[Tool]>) -> Option<String> {
    let tools = tools.filter(|tools| !tools.is_empty())?;
    serde_json::to_string(&tools.iter().map(tool_to_chat).collect::<Vec<_>>()).ok()
}

fn message_content_for_inspect(message: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(content) = message.get("content").and_then(Value::as_str)
        && !content.is_empty()
    {
        parts.push(content.to_string());
    }
    if let Some(content) = message.get("content").and_then(Value::as_array) {
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        parts.push(text.to_string());
                    }
                }
                Some("image_url") => {
                    let url = part
                        .get("image_url")
                        .and_then(|image_url| image_url.get("url"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    parts.push(format!(
                        "[image_url:{}]",
                        summarize_image_url_for_inspect(url)
                    ));
                }
                _ => {}
            }
        }
    }
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        parts.push(reasoning.to_string());
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        parts.push(tool_calls.to_string());
    }
    parts.join("\n")
}

fn summarize_image_url_for_inspect(url: &str) -> String {
    let Some((prefix, encoded)) = url.split_once(";base64,") else {
        return first_chars(url, 96);
    };
    format!("{prefix};base64,<{} chars>", encoded.len())
}

fn tool_result_inspection_for_message(message: &Value) -> Option<ToolResultInspection> {
    if message.get("role").and_then(Value::as_str) != Some("tool") {
        return None;
    }
    let budget = message.get("_tool_result_budget")?;
    Some(ToolResultInspection {
        original_chars: budget
            .get("original_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        sent_chars: budget
            .get("sent_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        truncated: budget
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        deduplicated: budget
            .get("deduplicated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn turn_meta_inspection_for_message(message: &Value) -> Option<TurnMetaInspection> {
    let budget = message.get("_turn_meta_budget")?;
    Some(TurnMetaInspection {
        original_chars: budget
            .get("original_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        sent_chars: budget
            .get("sent_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        deduplicated: budget
            .get("deduplicated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        sha256: budget
            .get("sha256")
            .and_then(Value::as_str)
            .map(str::to_string)?,
    })
}

fn split_system_layers(content: &str) -> Vec<(String, PromptLayerStability, &str)> {
    let markers = [
        ("Project context", "<project_instructions"),
        ("Project context pack", "## Project Context Pack"),
        ("Environment", "## Environment"),
        ("Configured instructions", "<instructions "),
        ("User memory", "## User Memory"),
        ("Current session goal", "## Current Session Goal"),
        ("Skills", "## Skills"),
        ("Core execution", "## Core Execution"),
        ("Compact template", "## Compact"),
        ("Previous session relay", "## Previous Session Relay"),
    ];

    let mut starts: Vec<(usize, &str)> = markers
        .iter()
        .filter_map(|(name, marker)| content.find(marker).map(|idx| (idx, *name)))
        .collect();
    starts.sort_by_key(|(idx, _)| *idx);

    let mut layers = Vec::new();
    let first_marker = starts.first().map_or(content.len(), |(idx, _)| *idx);
    if first_marker > 0 {
        layers.push((
            "Global system prefix".to_string(),
            PromptLayerStability::Static,
            content[..first_marker].trim(),
        ));
    }

    for (i, (start, name)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(content.len(), |(idx, _)| *idx);
        let stability = if *name == "Previous session relay" {
            PromptLayerStability::Dynamic
        } else if is_static_base_layer(name) {
            PromptLayerStability::Static
        } else {
            PromptLayerStability::History
        };
        layers.push(((*name).to_string(), stability, content[*start..end].trim()));
    }

    if layers.is_empty() {
        layers.push((
            "Global system prefix".to_string(),
            PromptLayerStability::Static,
            content.trim(),
        ));
    }
    layers
}

fn is_static_base_layer(name: &str) -> bool {
    matches!(
        name,
        "Global system prefix"
            | "Environment"
            | "Skills"
            | "Project context"
            | "Project context pack"
            | "Core execution"
            | "Compact template"
    )
}

fn stable_system_prompt(system: Option<&SystemPrompt>) -> Option<SystemPrompt> {
    let instructions = system_to_instructions(system.cloned())?;
    let stable = split_system_layers(&instructions)
        .into_iter()
        .filter_map(|(_, stability, body)| {
            (stability == PromptLayerStability::Static).then_some(body)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if stable.trim().is_empty() {
        None
    } else {
        Some(SystemPrompt::Text(stable))
    }
}

fn stable_history_messages(messages: &[Message]) -> Vec<Message> {
    let mut end = messages.len();
    if messages
        .last()
        .is_some_and(|message| message.role.as_str() == "user")
    {
        end = end.saturating_sub(1);
    }
    messages[..end].to_vec()
}

fn prompt_layer(
    name: String,
    stability: PromptLayerStability,
    content: &str,
) -> PromptLayerInspection {
    let char_len = content.chars().count();
    let token_estimate = if char_len == 0 {
        0
    } else if content.is_ascii() {
        (char_len / 4).max(1)
    } else {
        char_len.max(1)
    };
    PromptLayerInspection {
        name,
        stability,
        char_len,
        byte_len: content.len(),
        token_estimate,
        sha256: sha256_hex(content.as_bytes()),
        tool_result: None,
        turn_meta: None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::hashing::sha256_hex(bytes)
}

#[derive(Clone)]
struct PendingToolCallInfo {
    tool_name: String,
    input: Value,
}

struct SeenToolResult {
    message_label: String,
    original_chars: usize,
}

struct WireToolResult {
    content: String,
    original_chars: usize,
    sent_chars: usize,
    truncated: bool,
    deduplicated: bool,
}

#[derive(Clone)]
struct TurnMetaBudget {
    original_chars: usize,
    sent_chars: usize,
    deduplicated: bool,
    sha256: String,
}

struct LastFullTurnMeta {
    sha256: String,
}

fn render_turn_meta_for_wire(
    text: &str,
    last_full_turn_meta: &mut Option<LastFullTurnMeta>,
) -> (String, TurnMetaBudget) {
    let original_chars = text.chars().count();
    let sha = sha256_hex(text.as_bytes());

    if last_full_turn_meta
        .as_ref()
        .is_some_and(|previous| previous.sha256 == sha)
    {
        // Keep the repeated metadata slot short without surfacing an
        // opaque hash the model cannot resolve.
        let rendered = "<turn_meta_unchanged />".to_string();
        let budget = TurnMetaBudget {
            original_chars,
            sent_chars: rendered.chars().count(),
            deduplicated: true,
            sha256: sha,
        };
        return (rendered, budget);
    }

    *last_full_turn_meta = Some(LastFullTurnMeta {
        sha256: sha.clone(),
    });
    (
        text.to_string(),
        TurnMetaBudget {
            original_chars,
            sent_chars: original_chars,
            deduplicated: false,
            sha256: sha,
        },
    )
}

fn is_turn_meta_text(text: &str) -> bool {
    text.trim_start().starts_with("<turn_meta>")
}

fn turn_meta_budget_json(turn_meta: &TurnMetaBudget) -> Value {
    json!({
        "original_chars": turn_meta.original_chars,
        "sent_chars": turn_meta.sent_chars,
        "deduplicated": turn_meta.deduplicated,
        "sha256": turn_meta.sha256,
    })
}

/// Mutating/write tools whose result body is a *confirmation* (it embeds
/// the unified diff + summary of what was just written), not retrievable
/// reference data. Two identical large `write_file` calls must each keep
/// their full confirmation inline: collapsing the later one to a
/// `<TOOL_RESULT_REF sha="..." />` makes the model lose the write-success
/// context and behave as if the file is missing (issue #1695). Read-style
/// tools (`read_file`, `grep_files`, `exec_shell`, …) may deduplicate medium
/// outputs by pointing at an earlier full message in the same request. They
/// never advertise a process-wide SHA as retrievable: that store cannot prove
/// session ownership.
fn is_mutation_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "write" | "edit" | "write_file" | "edit_file" | "apply_patch"
    )
}

fn compact_tool_result_for_wire(
    tool_name: &str,
    input: &Value,
    content: &str,
    message_label: &str,
    sent_budget: usize,
    seen_tool_results: &mut HashMap<String, SeenToolResult>,
) -> WireToolResult {
    let original_chars = content.chars().count();
    let sha = sha256_hex(content.as_bytes());

    // Only medium, non-mutation results can point back to a full earlier
    // message in this one request. Oversized results are already excerpts, so
    // a back-reference would falsely imply the exact bytes remain available.
    let dedup_eligible = (TOOL_RESULT_DEDUP_MIN_CHARS..=sent_budget).contains(&original_chars)
        && !is_mutation_tool(tool_name);

    if dedup_eligible && let Some(previous) = seen_tool_results.get(&sha) {
        let content = format!(
            "<TOOL_RESULT_REF sha=\"{sha}\" original_message=\"{label}\" chars=\"{chars}\">\n\
             source: full content appears in {label} earlier in this request\n\
             </TOOL_RESULT_REF>",
            label = previous.message_label,
            chars = previous.original_chars,
        );
        return WireToolResult {
            sent_chars: content.chars().count(),
            content,
            original_chars,
            truncated: false,
            deduplicated: true,
        };
    }

    if dedup_eligible {
        seen_tool_results.insert(
            sha.clone(),
            SeenToolResult {
                message_label: message_label.to_string(),
                original_chars,
            },
        );
    }

    if original_chars <= sent_budget {
        return WireToolResult {
            content: content.to_string(),
            original_chars,
            sent_chars: original_chars,
            truncated: false,
            deduplicated: false,
        };
    }

    // Content already bounded by the adaptive evidence envelope carries its
    // own honest footer: the omitted count, the on-disk artifact path, and a
    // recovery instruction. Truncating it again here would destroy that
    // recovery contract and falsely report that no session-owned artifact
    // was recorded, so pass it through untouched.
    if content.contains(crate::tools::truncate::SPILLOVER_RECOVERY_HINT) {
        return WireToolResult {
            content: content.to_string(),
            original_chars,
            sent_chars: original_chars,
            truncated: false,
            deduplicated: false,
        };
    }

    let excerpt_chars = sent_budget.saturating_sub(TOOL_RESULT_EXCERPT_FRAME_CHARS);
    let head_chars = excerpt_chars * 2 / 3;
    let head = first_chars(content, head_chars);
    let tail = last_chars(content, excerpt_chars - head_chars);
    let kept = head.chars().count() + tail.chars().count();
    let omitted = original_chars.saturating_sub(kept);
    let compacted = format!(
        "[TOOL_RESULT_TRUNCATED]\n\
         tool_name: {tool_name}\n\
         command_or_query: {}\n\
         exit_status: {}\n\
         original_chars: {original_chars}\n\
         sha256: {sha}\n\
         exact_detail: unavailable; no session-owned artifact was recorded\n\
         first_chars:\n\
         {head}\n\n\
         [... truncated {omitted} chars from middle ...]\n\n\
         last_chars:\n\
         {tail}",
        tool_command_or_query(input),
        tool_exit_status(content)
    );

    WireToolResult {
        sent_chars: compacted.chars().count(),
        content: compacted,
        original_chars,
        truncated: true,
        deduplicated: false,
    }
}

fn tool_command_or_query(input: &Value) -> String {
    for key in ["command", "cmd", "query", "q", "pattern", "path", "url"] {
        if let Some(value) = input.get(key) {
            return summarize_for_metadata(value, 500);
        }
    }
    summarize_for_metadata(input, 500)
}

fn tool_exit_status(content: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(content) {
        for key in ["exit_code", "exit_status", "status", "code"] {
            if let Some(value) = value.get(key) {
                return summarize_for_metadata(value, 120);
            }
        }
    }

    for line in content.lines().take(20) {
        let trimmed = line.trim();
        for prefix in ["Exit code:", "exit code:", "Exit status:", "exit status:"] {
            if let Some(value) = trimmed.strip_prefix(prefix) {
                return value.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

fn summarize_for_metadata(value: &Value, max_chars: usize) -> String {
    let raw = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    let mut summarized = first_chars(&raw.replace('\n', "\\n"), max_chars);
    if raw.chars().count() > max_chars {
        summarized.push_str("...");
    }
    summarized
}

fn first_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

fn last_chars(value: &str, count: usize) -> String {
    let mut chars: Vec<char> = value.chars().rev().take(count).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn merge_adjacent_user_content(previous: Value, current: Value) -> Value {
    match (previous, current) {
        (Value::String(left), Value::String(right)) => json!(format!("{left}\n\n{right}")),
        (left, right) => {
            let mut parts = Vec::new();
            for content in [left, right] {
                match content {
                    Value::Array(items) => parts.extend(items),
                    Value::String(text) => parts.push(json!({"type": "text", "text": text})),
                    other => parts.push(other),
                }
            }
            Value::Array(parts)
        }
    }
}

fn build_chat_messages_with_reasoning(
    system: Option<&SystemPrompt>,
    messages: &[Message],
    tool_result_budget: usize,
    include_reasoning: bool,
    include_tool_budget_metadata: bool,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pending_tool_calls: HashMap<String, PendingToolCallInfo> = HashMap::new();
    // Chat Completions requires every result for one assistant tool-call batch
    // to be contiguous. Keep tool-result media aside until the complete batch
    // has been emitted as `role: tool` messages.
    let mut deferred_tool_result_images = Vec::new();
    let mut seen_tool_results: HashMap<String, SeenToolResult> = HashMap::new();
    let mut last_full_turn_meta: Option<LastFullTurnMeta> = None;

    if let Some(instructions) = system_to_instructions(system.cloned())
        && !instructions.trim().is_empty()
    {
        out.push(json!({
            "role": "system",
            "content": instructions,
        }));
    }

    // Persisted compaction keeps its summary after the bounded last round.
    // On strict paired chat templates a user message after a tool result is
    // invalid. Reorder only a generated summary immediately after a tool
    // result; its independent provenance block rules out quoted user text.
    // The session log retains every original message and tool ID.
    // Limitation: this normalization applies to Chat Completions only.
    let summary_index = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            (index > 0
                && crate::compaction::is_wire_compaction_checkpoint_message(message)
                && messages[index - 1]
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolResult { .. })))
            .then_some(index)
        });
    let summary_target = summary_index.and_then(|summary_index| {
        messages[..summary_index]
            .iter()
            .rposition(|message| {
                crate::runtime_handoff::classify_user_turn_prompt(message)
                    != crate::runtime_handoff::UserTurnPromptKind::NotPrompt
            })
            .or_else(|| {
                messages[..summary_index]
                    .iter()
                    .position(|message| message.role.is_assistant_like())
            })
    });
    let wire_messages = (0..messages.len())
        .filter(|index| Some(*index) != summary_index || summary_target.is_none())
        .flat_map(|index| {
            if Some(index) == summary_target {
                [summary_index, Some(index)]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
            } else {
                vec![index]
            }
        });

    for message_index in wire_messages {
        let message = &messages[message_index];
        // Which wire channel this message belongs in is decided by the shared
        // placement table, not by an `if` chain local to this adapter.
        let placement = role_placement(&message.role, WireDialect::ChatCompletions);
        let mut text_parts = Vec::new();
        let mut image_parts = Vec::new();
        let mut thinking_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut tool_call_infos = Vec::new();
        let mut tool_results: Vec<(String, String, String, Vec<Value>)> = Vec::new();
        let mut turn_meta_budget: Option<TurnMetaBudget> = None;

        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    if is_turn_meta_text(text) {
                        let (rendered, budget) =
                            render_turn_meta_for_wire(text, &mut last_full_turn_meta);
                        text_parts.push(rendered);
                        turn_meta_budget = Some(budget);
                    } else {
                        text_parts.push(text.clone());
                    }
                }
                ContentBlock::ImageUrl { image_url } => {
                    image_parts.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": image_url.url.clone(),
                        },
                    }));
                }
                ContentBlock::Thinking { thinking, .. } => thinking_parts.push(thinking.clone()),
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    caller,
                    thought_signature,
                    ..
                } => {
                    let args = serde_json::to_string(input).unwrap_or_else(|_| input.to_string());
                    let mut call = json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": to_api_tool_name(name),
                            "arguments": args,
                        }
                    });
                    if let Some(signature) = thought_signature {
                        call["extra_content"]["google"]["thought_signature"] = json!(signature);
                    }
                    if let Some(caller) = caller {
                        call["caller"] = json!({
                            "type": caller.caller_type,
                            "tool_id": caller.tool_id,
                        });
                    }
                    tool_calls.push(call);
                    tool_call_infos.push((
                        id.clone(),
                        PendingToolCallInfo {
                            tool_name: name.clone(),
                            input: input.clone(),
                        },
                    ));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    content_blocks,
                    ..
                } => {
                    let message_label = format!("Message #{message_index}");
                    tool_results.push((
                        tool_use_id.clone(),
                        content.clone(),
                        message_label,
                        content_blocks.clone().unwrap_or_default(),
                    ));
                }
                ContentBlock::ServerToolUse { .. }
                | ContentBlock::ToolSearchToolResult { .. }
                | ContentBlock::CodeExecutionToolResult { .. } => {}
            }
        }

        let out_len_before_role_projection = out.len();
        if placement.is_assistant_channel() {
            let content = if placement == RolePlacement::InterruptedAssistant {
                format!(
                    "{}{}",
                    codewhale_models::INTERRUPTED_ASSISTANT_CONTEXT_PREFIX,
                    text_parts.join("\n")
                )
            } else {
                text_parts.join("\n")
            };
            let mut reasoning_content = thinking_parts.join("\n");
            let has_text = !content.trim().is_empty();
            let has_tool_calls = !tool_calls.is_empty();
            // Reasoning replay must be a function of the stored message ONLY,
            // never of later history. DeepSeek's prefix cache hashes the raw
            // bytes of every message; flipping `reasoning_content` on/off
            // depending on whether a follow-up user turn exists rewrites a
            // historical message between turns and busts the cache from that
            // point onwards. Always emit `reasoning_content` when the model
            // requires replay AND the stored message carries thinking text.
            // Tool-call messages with empty thinking still need a placeholder
            // (DeepSeek 400s without it), but text-only assistant messages
            // simply omit the field when there's nothing to replay.
            let mut has_reasoning = include_reasoning && !reasoning_content.trim().is_empty();
            if include_reasoning && has_tool_calls && !has_reasoning {
                logging::warn(
                    "Substituting placeholder reasoning_content for DeepSeek tool-call assistant message",
                );
                reasoning_content = String::from(REASONING_REPLAY_PLACEHOLDER);
                has_reasoning = true;
            }

            // DeepSeek rejects assistant messages where both `content` and
            // `tool_calls` are missing/null. Skip such entries even if they
            // carry reasoning-only metadata unless we can send a non-null
            // placeholder content field.
            if !has_text && !has_tool_calls && !has_reasoning {
                pending_tool_calls.clear();
                deferred_tool_result_images.clear();
                continue;
            }

            let mut msg = json!({
                "role": "assistant",
                "content": if has_text {
                    json!(content)
                } else if has_reasoning {
                    json!("")
                } else {
                    Value::Null
                },
            });
            if has_reasoning {
                msg["reasoning_content"] = json!(reasoning_content);
            }
            if has_tool_calls {
                msg["tool_calls"] = json!(tool_calls);
                let expected_tool_result_count = tool_call_infos.len();
                pending_tool_calls = tool_call_infos.into_iter().collect();
                deferred_tool_result_images.clear();
                if pending_tool_calls.len() != expected_tool_result_count {
                    logging::warn(
                        "Rejecting assistant tool-call batch with duplicate tool_call IDs",
                    );
                    pending_tool_calls.clear();
                }
            } else {
                pending_tool_calls.clear();
                deferred_tool_result_images.clear();
            }
            out.push(msg);
        } else if matches!(placement, RolePlacement::System | RolePlacement::Developer) {
            let content = text_parts.join("\n");
            if !content.trim().is_empty() {
                let mut msg = json!({
                    "role": if placement == RolePlacement::Developer {
                        "developer"
                    } else {
                        "system"
                    },
                    "content": content,
                });
                if include_tool_budget_metadata && let Some(turn_meta) = &turn_meta_budget {
                    msg["_turn_meta_budget"] = turn_meta_budget_json(turn_meta);
                }
                out.push(msg);
            }
        } else if placement == RolePlacement::User {
            let content = text_parts.join("\n");
            let has_text = !content.trim().is_empty();
            let has_images = !image_parts.is_empty();
            if has_text || has_images {
                let wire_content = if has_images {
                    let mut parts = Vec::new();
                    if has_text {
                        parts.push(json!({
                            "type": "text",
                            "text": content,
                        }));
                    }
                    parts.extend(image_parts);
                    json!(parts)
                } else {
                    json!(content)
                };
                let mut msg = json!({
                    "role": "user",
                    "content": wire_content,
                });
                if include_tool_budget_metadata && let Some(turn_meta) = &turn_meta_budget {
                    msg["_turn_meta_budget"] = turn_meta_budget_json(turn_meta);
                }
                if (Some(message_index) == summary_index
                    || Some(message_index) == summary_target
                    || crate::compaction::is_wire_compaction_checkpoint_message(message)
                    || crate::runtime_handoff::is_agent_topology_checkpoint(message)
                    || crate::runtime_handoff::is_restored_agent_topology_checkpoint(message))
                    && let Some(previous) = out.last_mut()
                    && previous.get("role").and_then(Value::as_str) == Some("user")
                {
                    let previous_content = previous["content"].take();
                    let current_content = msg["content"].take();
                    previous["content"] =
                        merge_adjacent_user_content(previous_content, current_content);
                    if previous.get("_turn_meta_budget").is_none()
                        && let Some(meta) = msg.get("_turn_meta_budget")
                    {
                        previous["_turn_meta_budget"] = meta.clone();
                    }
                } else {
                    out.push(msg);
                }
            }
        }

        // A user/system/developer wire message closes the contiguous run that
        // must follow an assistant tool-call message. If the same stored
        // message also carries a later tool result, reject it here instead of
        // briefly accepting the result and letting any synthesized media
        // escape after the safety pass strips the malformed batch.
        if out.len() > out_len_before_role_projection
            && !placement.is_assistant_channel()
            && !pending_tool_calls.is_empty()
        {
            logging::warn("Dropping tool-call batch interrupted by non-tool content");
            pending_tool_calls.clear();
            deferred_tool_result_images.clear();
        }

        if !tool_results.is_empty() {
            if pending_tool_calls.is_empty() {
                logging::warn("Dropping tool results without matching tool_calls");
            } else {
                for (tool_id, content, message_label, content_blocks) in tool_results {
                    if let Some(tool_info) = pending_tool_calls.remove(&tool_id) {
                        let (image, omitted) = crate::image_attach::provider_tool_result_image_refs(
                            Some(&content_blocks),
                        );
                        let content =
                            crate::image_attach::tool_result_text_with_omission(&content, omitted);
                        let wire_result = compact_tool_result_for_wire(
                            &tool_info.tool_name,
                            &tool_info.input,
                            &content,
                            &message_label,
                            tool_result_budget,
                            &mut seen_tool_results,
                        );
                        let mut tool_msg = json!({
                            "role": "tool",
                            "tool_call_id": tool_id,
                            "content": wire_result.content,
                        });
                        if include_tool_budget_metadata {
                            tool_msg["_tool_result_budget"] = json!({
                                "original_chars": wire_result.original_chars,
                                "sent_chars": wire_result.sent_chars,
                                "truncated": wire_result.truncated,
                                "deduplicated": wire_result.deduplicated,
                            });
                        }
                        out.push(tool_msg);
                        if let Some((mime_type, data)) = image {
                            deferred_tool_result_images.push(json!({
                                "type": "text",
                                "text": format!(
                                    "Image returned by tool `{}` (call `{tool_id}`):",
                                    tool_info.tool_name,
                                ),
                            }));
                            deferred_tool_result_images.push(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{mime_type};base64,{data}")
                                },
                            }));
                        }
                    } else {
                        logging::warn(format!(
                            "Dropping tool result for unknown tool_call_id: {tool_id}"
                        ));
                    }
                }
                if pending_tool_calls.is_empty() && !deferred_tool_result_images.is_empty() {
                    out.push(json!({
                        "role": "user",
                        "content": std::mem::take(&mut deferred_tool_result_images),
                    }));
                }
            }
        } else if !placement.is_assistant_channel() {
            pending_tool_calls.clear();
            deferred_tool_result_images.clear();
        }
    }

    // Safety net: after compaction, an assistant message may have tool_calls
    // whose results were summarized away. The API rejects these, so strip
    // the tool_calls (downgrading to a plain assistant message) and remove
    // the now-orphaned tool result messages.
    let mut i = 0;
    while i < out.len() {
        let is_assistant_with_tools = out[i].get("role").and_then(Value::as_str)
            == Some("assistant")
            && out[i].get("tool_calls").is_some();

        if is_assistant_with_tools {
            let expected_ids: Vec<String> = out[i]
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|c| c.get("id").and_then(Value::as_str).map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // Collect tool result IDs immediately following this assistant message.
            let mut found_ids = Vec::new();
            let mut tool_result_end = i + 1;
            while tool_result_end < out.len() {
                if out[tool_result_end].get("role").and_then(Value::as_str) == Some("tool") {
                    if let Some(id) = out[tool_result_end]
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                    {
                        found_ids.push(id.to_string());
                    }
                    tool_result_end += 1;
                } else {
                    break;
                }
            }

            // Chat Completions accepts only the immediately contiguous tool
            // run after its assistant tool-call message. Do not accept a
            // later tool result after user/system content has intervened.
            let results_match = expected_ids.len() == found_ids.len()
                && expected_ids.iter().all(|id| found_ids.contains(id));
            if !results_match {
                let missing: Vec<_> = expected_ids
                    .iter()
                    .filter(|id| !found_ids.contains(*id))
                    .collect();
                logging::warn(format!(
                    "Stripping orphaned tool_calls from assistant message \
                     (expected {} tool results, found {}, missing: {:?})",
                    expected_ids.len(),
                    found_ids.len(),
                    missing
                ));
                if let Some(obj) = out[i].as_object_mut() {
                    obj.remove("tool_calls");
                }
                // If tool_calls were the only assistant content, remove the now-invalid
                // assistant message entirely (DeepSeek requires content or tool_calls).
                let assistant_content_empty = out[i]
                    .get("content")
                    .is_none_or(|v| v.is_null() || v.as_str().is_some_and(str::is_empty));
                if assistant_content_empty {
                    // Remove orphaned tool results tied to this stripped assistant call set.
                    let mut j = out.len();
                    while j > i + 1 {
                        j -= 1;
                        if out[j].get("role").and_then(Value::as_str) == Some("tool")
                            && let Some(id) = out[j].get("tool_call_id").and_then(Value::as_str)
                            && expected_ids.iter().any(|expected| expected == id)
                        {
                            out.remove(j);
                        }
                    }
                    out.remove(i);
                    i = i.saturating_sub(1);
                    continue;
                }
                // Remove contiguous tool results first
                if tool_result_end > i + 1 {
                    out.drain((i + 1)..tool_result_end);
                }
                // Remove any remaining non-contiguous tool results referencing expected_ids
                // (scan backward to avoid index shifting issues)
                let mut j = out.len();
                while j > i + 1 {
                    j -= 1;
                    if out[j].get("role").and_then(Value::as_str) == Some("tool")
                        && let Some(id) = out[j].get("tool_call_id").and_then(Value::as_str)
                        && expected_ids.iter().any(|expected| expected == id)
                    {
                        out.remove(j);
                    }
                }
            }
        }
        i += 1;
    }

    out
}

pub(super) fn tool_to_chat(tool: &Tool) -> Value {
    let mut value = json!({
        "type": "function",
        "function": {
            "name": to_api_tool_name(&tool.name),
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    });
    if let Some(strict) = tool.strict
        && let Some(function) = value.get_mut("function")
    {
        function["strict"] = json!(strict);
    }
    value
}

pub(super) fn tool_to_chat_for_base_url(tool: &Tool, base_url: &str) -> Value {
    let mut value = tool_to_chat(tool);
    if !deepseek_base_url_supports_strict_tools(base_url)
        && let Some(function) = value.get_mut("function")
        && let Some(obj) = function.as_object_mut()
    {
        obj.remove("strict");
    }
    value
}

fn deepseek_base_url_supports_strict_tools(base_url: &str) -> bool {
    let trimmed = base_url.trim_end_matches('/').to_ascii_lowercase();
    let is_deepseek = trimmed == "https://api.deepseek.com"
        || trimmed == "https://api.deepseek.com/v1"
        || trimmed == "https://api.deepseek.com/beta"
        || trimmed == "https://api.deepseeki.com"
        || trimmed == "https://api.deepseeki.com/v1"
        || trimmed == "https://api.deepseeki.com/beta";
    !is_deepseek || trimmed.ends_with("/beta")
}

fn map_tool_choice_for_chat(choice: &Value) -> Option<Value> {
    if let Some(choice_str) = choice.as_str() {
        return Some(json!(choice_str));
    }
    let Some(choice_type) = choice.get("type").and_then(Value::as_str) else {
        return Some(choice.clone());
    };

    match choice_type {
        "auto" | "none" => Some(json!(choice_type)),
        "any" => Some(json!("auto")),
        "tool" => choice.get("name").and_then(Value::as_str).map(|name| {
            json!({
                "type": "function",
                "function": { "name": to_api_tool_name(name) }
            })
        }),
        _ => Some(choice.clone()),
    }
}

fn should_send_tool_choice_for_chat(provider: ProviderKind, effort: Option<&str>) -> bool {
    if !matches!(provider, ProviderKind::Deepseek) {
        return true;
    }
    !reasoning_effort_enables_thinking(effort)
}

fn reasoning_effort_enables_thinking(effort: Option<&str>) -> bool {
    let Some(effort) = effort else {
        return false;
    };
    !matches!(
        effort.trim().to_ascii_lowercase().as_str(),
        "off" | "disabled" | "none" | "false"
    )
}

/// Final-pass sanitizer over the outgoing chat-completions JSON payload.
/// Forces a non-empty `reasoning_content` onto assistant messages that carry
/// `tool_calls`, when the model + effort combination requires it. DeepSeek's
/// thinking-mode API rejects such messages with a 400 error; substituting a
/// placeholder keeps the conversation chain intact. Non-tool assistant
/// reasoning can stay omitted once a later user text turn begins.
///
/// Also tallies the size of all replayed `reasoning_content` and logs it, so
/// users on `RUST_LOG=codewhale_tui=debug` can see how much of their input
/// budget is being spent re-sending prior thinking traces.
#[cfg(test)]
pub(super) fn sanitize_thinking_mode_messages(
    body: &mut Value,
    model: &str,
    effort: Option<&str>,
    provider: ProviderKind,
) -> Option<u32> {
    sanitize_thinking_mode_messages_for_route(body, model, effort, provider, "")
}

/// Route-aware variant of `sanitize_thinking_mode_messages`.
///
/// The wrapper above remains intentionally route-agnostic for existing test
/// helpers and generic callers. Production chat requests call this version so
/// exact Kimi Code K3 assistant tool turns retain the reasoning trace that
/// K3 expects on the next request.
pub(super) fn sanitize_thinking_mode_messages_for_route(
    body: &mut Value,
    model: &str,
    effort: Option<&str>,
    provider: ProviderKind,
    base_url: &str,
) -> Option<u32> {
    // Mistral replay is encoded inside polymorphic `content` blocks, not the
    // DeepSeek `reasoning_content` field. Running the DeepSeek placeholder
    // sanitizer after reshaping would add a second, invalid reasoning dialect
    // to assistant tool-call turns.
    if is_exact_mistral_chat_route(provider, base_url) {
        return None;
    }
    if !should_replay_reasoning_content_for_provider_on_route(provider, base_url, model, effort) {
        return None;
    }
    let messages = body.get_mut("messages").and_then(Value::as_array_mut)?;
    let mut substitutions: u32 = 0;
    let mut replay_chars: u64 = 0;
    let mut replay_messages: u32 = 0;
    for (idx, msg) in messages.iter_mut().enumerate() {
        if msg.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let has_tool_calls = msg.get("tool_calls").is_some();
        let needs_placeholder = msg
            .get("reasoning_content")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty());
        if has_tool_calls && needs_placeholder {
            msg["reasoning_content"] = json!(REASONING_REPLAY_PLACEHOLDER);
            substitutions = substitutions.saturating_add(1);
            logging::warn(format!(
                "Final sanitizer: forced reasoning_content placeholder on assistant[{idx}]",
            ));
        }
        if let Some(reasoning) = msg.get("reasoning_content").and_then(Value::as_str) {
            let len = reasoning.len() as u64;
            if len > 0 {
                replay_chars = replay_chars.saturating_add(len);
                replay_messages = replay_messages.saturating_add(1);
            }
        }
    }
    if substitutions > 0 {
        logging::warn(format!(
            "Final sanitizer: {substitutions} assistant message(s) needed reasoning_content placeholder",
        ));
    }
    if replay_messages == 0 {
        return None;
    }
    // ~4 chars/token is the standard rough estimate; DeepSeek tokens skew
    // a touch shorter on Chinese/code but this is order-of-magnitude info.
    let approx_tokens = (replay_chars / 4).min(u64::from(u32::MAX)) as u32;
    logging::info(format!(
        "Reasoning-content replay: {replay_messages} assistant message(s), ~{approx_tokens} input tokens ({replay_chars} chars) being re-sent in this request",
    ));
    Some(approx_tokens)
}

/// Sums the byte length of `reasoning_content` across all assistant messages in
/// an outgoing chat-completions body. Used by tests; the production sanitizer
/// computes the same number inline and logs it.
#[cfg(test)]
pub(super) fn count_reasoning_replay_chars(body: &Value) -> u64 {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return 0;
    };
    messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|m| m.get("reasoning_content").and_then(Value::as_str))
        .map(|s| s.len() as u64)
        .sum()
}

/// Render the transport-shape headers we care about for #103 diagnostics.
/// Always returns SOMETHING printable so the decode-error log line is parseable
/// even when the server stripped a header we expected.
fn format_stream_headers(headers: &reqwest::header::HeaderMap) -> String {
    const FIELDS: &[&str] = &[
        "content-encoding",
        "transfer-encoding",
        "connection",
        "server",
    ];
    let mut parts: Vec<String> = Vec::with_capacity(FIELDS.len());
    for field in FIELDS {
        let rendered = headers
            .get(*field)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(absent)");
        parts.push(format!("{field}={rendered}"));
    }
    parts.join(", ")
}

/// Diagnostic logger fired when DeepSeek rejects the request despite the
/// sanitizer. Walks the body and logs which assistant messages have tool_calls
/// but no `reasoning_content` — useful to track down a code path that bypasses
/// the sanitizer entirely.
fn log_thinking_mode_violations(body: &Value) {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        logging::warn("400-after-sanitizer: body has no `messages` array");
        return;
    };
    let mut violations: Vec<String> = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        if msg.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let reasoning = msg
            .get("reasoning_content")
            .and_then(Value::as_str)
            .unwrap_or("");
        let has_tc = msg.get("tool_calls").is_some();
        if reasoning.trim().is_empty() {
            violations.push(format!(
                "assistant[{idx}] (reasoning_content missing, tool_calls={has_tc})"
            ));
        }
    }
    if violations.is_empty() {
        logging::warn(
            "400-after-sanitizer: all assistant messages have reasoning_content — DeepSeek rejected for a different reason",
        );
    } else {
        logging::warn(format!(
            "400-after-sanitizer: {} assistant message(s) lack reasoning_content despite sanitizer: {}",
            violations.len(),
            violations.join(", ")
        ));
    }
}

fn requires_reasoning_content(model: &str) -> bool {
    let lower = model.to_lowercase();
    // V4-family direct model IDs.
    lower.contains("deepseek-v4")
        // Public DeepSeek API aliases routed server-side to the V4 family.
        // `deepseek-chat` resolves to `deepseek-v4-flash` and `deepseek-reasoner`
        // resolves to `deepseek-v4-pro`; both have thinking mode enabled by
        // default, so any assistant message carrying tool_calls must replay
        // `reasoning_content` on subsequent turns or the API returns 400.
        || lower.starts_with("deepseek-chat")
        || lower.starts_with("deepseek-reasoner")
        || has_deepseek_r_series_marker(&lower)
        // #6044: the V4.1 official id dropped the version number entirely
        // (`deepseek-flash`), so the literal arms above cannot see it and
        // the decode/replay classifiers depended on a later catalog fallback
        // to catch it. The catalog owns the capability — consult it here so
        // every caller (stream style, wire replay, prompt inspection) agrees
        // without another hardcoded id.
        || (lower.starts_with("deepseek-") && model_supports_reasoning(model))
}

fn should_replay_reasoning_content(model: &str, effort: Option<&str>) -> bool {
    if effort
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "disabled" | "none" | "false"
            )
        })
        .unwrap_or(false)
    {
        return false;
    }

    requires_reasoning_content(model)
}

#[cfg(test)]
fn should_replay_reasoning_content_for_provider(
    provider: ProviderKind,
    model: &str,
    effort: Option<&str>,
) -> bool {
    should_replay_reasoning_content_for_provider_on_route(provider, "", model, effort)
}

/// Route-aware reasoning replay policy.
///
/// Keep the bare K3 identifier out of the global model catalog: direct
/// Moonshot and arbitrary OpenAI-compatible routes can also expose a `k3`
/// model name, but only Kimi Code's exact membership-plan endpoint has this
/// replay contract.
fn should_replay_reasoning_content_for_provider_on_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    effort: Option<&str>,
) -> bool {
    // Exact always-thinking routes replay their reasoning trace regardless of
    // a stale caller effort: the API contract requires the assistant
    // reasoning field on later tool turns for multi-turn continuity.
    if is_exact_direct_moonshot_k3_route(provider, base_url, model)
        || is_exact_kimi_code_k3_route(provider, base_url, model)
        || (is_exact_mistral_chat_route(provider, base_url)
            && mistral_model_has_native_reasoning(model))
    {
        return true;
    }

    // The exact Model Studio route policy evaluates BEFORE any generic
    // model-name heuristic. Only models Alibaba documents as accepting
    // `preserve_thinking` replay historical `reasoning_content`, plus the
    // concrete DeepSeek V4 family ids whose own API contract requires the
    // reasoning field on tool turns. A model named `foo-thinking` or
    // `foo-reasoner` proves nothing about DashScope's request dialect and
    // must not gain replay here — replaying stale Thinking blocks feeds the
    // model its own past reasoning and re-triggers it every turn (observed
    // as a repeated handoff loop with the always-thinking qwen3.8 family).
    // Pi does not replay those either.
    if is_exact_modelstudio_chat_route(provider, base_url) {
        if modelstudio_model_supports_preserve_thinking(model) {
            // Thinking-only preserve models (kimi-k2.7-code) are
            // always-thinking and replay even with a stale `off`; hybrid
            // preserve models defer to the effort gate like every other
            // hybrid.
            if is_exact_modelstudio_thinking_only_route(provider, base_url, model)
                || !modelstudio_effort_disables_thinking(effort)
            {
                return true;
            }
            return false;
        }
        let lower = model.trim().to_ascii_lowercase();
        return lower.contains("deepseek-v4")
            || lower.starts_with("deepseek-chat")
            || lower.starts_with("deepseek-reasoner");
    }

    if effort
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "disabled" | "none" | "false"
            )
        })
        .unwrap_or(false)
    {
        return false;
    }

    if requires_reasoning_content(model) {
        return true;
    }

    // Xiaomi MiMo's API requires the assistant `reasoning_content` back on
    // tool-call turns (omitting it is a 400), for every MiMo chat model, not
    // only the ids the offline catalog marks as reasoning (#6501). A turn
    // that produced no reasoning replays nothing.
    if provider == ProviderKind::XiaomiMimo {
        return true;
    }

    if is_exact_mistral_chat_route(provider, base_url)
        && mistral_model_has_adjustable_reasoning(model)
    {
        return true;
    }

    if !provider_accepts_reasoning_content(provider) {
        // Generic non-DeepSeek model on a provider that rejects the field:
        // keep stripping it (preserves the #1542 fix). But a known DeepSeek
        // reasoning model pointed at a DeepSeek-compatible endpoint via the
        // generic `openai` provider still requires reasoning_content replay,
        // or the thinking-mode API returns 400 (#1739 / #1694).
        return false;
    }

    model_supports_reasoning(model)
}

/// Test shorthand: does this route surface `reasoning_content` /
/// `reasoning` / `reasoning_details` deltas as Thinking by default?
#[cfg(test)]
fn is_reasoning_model_for_stream(provider: ProviderKind, model: &str) -> bool {
    reasoning_stream_style_for_stream(provider, model, None) == ReasoningStreamStyle::SeparateField
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReasoningStreamStyle {
    SeparateField,
    InlineTags,
    MistralBlocks,
    None,
}

#[cfg(test)]
fn reasoning_stream_style_for_stream(
    provider: ProviderKind,
    model: &str,
    configured: Option<&str>,
) -> ReasoningStreamStyle {
    reasoning_stream_style_for_route(provider, "", model, configured)
}

/// Choose stream decoding semantics for a fully resolved provider route.
fn reasoning_stream_style_for_route(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    configured: Option<&str>,
) -> ReasoningStreamStyle {
    if is_exact_mistral_chat_route(provider, base_url) && mistral_model_supports_reasoning(model) {
        return ReasoningStreamStyle::MistralBlocks;
    }
    if let Some(configured) = configured {
        if let Some(style) = parse_reasoning_stream_style(configured) {
            return style;
        }
        logging::warn(format!(
            "Ignoring unrecognized reasoning_stream_style `{configured}`; expected separate_field, inline_tags, or none"
        ));
    }
    // #6501: the dedicated reasoning fields are reasoning on every
    // Chat Completions route that sends them (DeepSeek, xAI, Xiaomi MiMo,
    // Kimi, GLM, MiniMax, DashScope, vLLM/SGLang reasoning parsers, OpenRouter,
    // Ollama, ...), and the non-streaming parser already treats them that way
    // unconditionally. Gating the stream on a provider allowlist AND a
    // per-model catalog row meant every unlisted provider (xAI, StepFun,
    // Together, Ollama, the Codewhale gateway, ...) and every model id newer
    // than the offline catalog (grok-4.7, a fresh MiMo id) rendered its
    // reasoning as answer prose. Surfacing a field that never arrives costs
    // nothing; a gateway that really puts its answer in `reasoning_content`
    // opts out with `reasoning_stream_style = "none"`. Replay of reasoning in
    // request history stays separately gated by
    // `should_replay_reasoning_content_for_provider_on_route`.
    ReasoningStreamStyle::SeparateField
}

fn parse_reasoning_stream_style(value: &str) -> Option<ReasoningStreamStyle> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "separate_field" | "separate" | "field" => Some(ReasoningStreamStyle::SeparateField),
        "inline_tags" | "inline" | "think_tags" | "thinking_tags" => {
            Some(ReasoningStreamStyle::InlineTags)
        }
        "none" | "text" | "disabled" | "off" => Some(ReasoningStreamStyle::None),
        _ => None,
    }
}

/// Providers whose chat-completions API both returns and accepts a dedicated
/// `reasoning_content` field on assistant messages.
///
/// Arcee is intentionally included. Trinity-Large-Thinking natively emits
/// `<think>...</think>` traces, but Arcee's hosted API serves it through vLLM
/// with `--reasoning-parser deepseek_r1`, which parses those blocks into a
/// `reasoning_content` field (verified live against `api.arcee.ai`: thinking
/// streams as `delta.reasoning_content`, the answer as `delta.content`, with no
/// `<think>` tags on the wire). Arcee's docs require replaying `reasoning_content`
/// on assistant tool-call turns; dropping it makes the model emit tool calls as
/// raw XML inside its thinking ("xml_in_reasoning" pitfall). Do not remove Arcee
/// here without new live evidence — see docs.arcee.ai/capabilities/reasoning-traces.
fn provider_accepts_reasoning_content(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::Deepseek
            | ProviderKind::NvidiaNim
            | ProviderKind::Openrouter
            | ProviderKind::XiaomiMimo
            | ProviderKind::Novita
            | ProviderKind::Fireworks
            | ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Volcengine
            | ProviderKind::Arcee
            | ProviderKind::Minimax
            | ProviderKind::Sglang
            | ProviderKind::Zai
            | ProviderKind::Moonshot // #3016: Kimi thinking traces use reasoning_content
    )
}

fn has_deepseek_r_series_marker(model_lower: &str) -> bool {
    const PREFIX: &str = "deepseek-r";
    model_lower.match_indices(PREFIX).any(|(idx, _)| {
        model_lower[idx + PREFIX.len()..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
    })
}

/// Transport-only reasoning replay placeholder. DeepSeek-family chat wires
/// reject assistant tool-call messages with an empty `reasoning_content`, so
/// the request serializer substitutes this string for replay only. Providers
/// that mirror assistant history (GLM-5.x) stream the substituted field back
/// as a live reasoning delta; ingest must drop that exact echo so a wire-only
/// placeholder never becomes a persisted or displayed thinking block.
pub(crate) const REASONING_REPLAY_PLACEHOLDER: &str = "(reasoning omitted)";

#[must_use]
pub(crate) fn is_reasoning_replay_placeholder(text: &str) -> bool {
    text.trim() == REASONING_REPLAY_PLACEHOLDER
}

fn reasoning_delta(
    value: &Value,
    choice_index: u32,
    reasoning_detail_buffers: &mut std::collections::HashMap<u32, String>,
) -> Option<String> {
    if let Some(reasoning) = value
        .get("reasoning_content")
        .or_else(|| value.get("reasoning"))
        .and_then(Value::as_str)
    {
        if is_reasoning_replay_placeholder(reasoning) {
            return None;
        }
        return Some(reasoning.to_string());
    }

    let details = value.get("reasoning_details").and_then(Value::as_array)?;
    let full_text = details
        .iter()
        .filter_map(|detail| detail.get("text").and_then(Value::as_str))
        .collect::<String>();
    if full_text.is_empty() {
        return None;
    }

    let previous = reasoning_detail_buffers.entry(choice_index).or_default();
    let delta = full_text
        .strip_prefix(previous.as_str())
        .unwrap_or(&full_text)
        .to_string();
    *previous = full_text;
    Some(delta)
}

fn reasoning_message_text(value: &Value) -> Option<String> {
    if let Some(reasoning) = value
        .get("reasoning_content")
        .or_else(|| value.get("reasoning"))
        .and_then(Value::as_str)
    {
        if is_reasoning_replay_placeholder(reasoning) {
            return None;
        }
        return Some(reasoning.to_string());
    }
    value
        .get("reasoning_details")
        .and_then(Value::as_array)
        .map(|details| {
            details
                .iter()
                .filter_map(|detail| detail.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
}

#[cfg(test)]
pub(super) fn parse_chat_message(payload: &Value) -> Result<MessageResponse> {
    parse_chat_message_for_route(payload, ProviderKind::Openai, "")
}

fn parse_chat_message_for_route(
    payload: &Value,
    provider: ProviderKind,
    base_url: &str,
) -> Result<MessageResponse> {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl")
        .to_string();
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let choices = payload
        .get("choices")
        .and_then(Value::as_array)
        .context("Chat API response missing choices")?;
    let choice = choices
        .first()
        .context("Chat API response missing first choice")?;
    let message = choice
        .get("message")
        .context("Chat API response missing message")?;

    let mut content_blocks = Vec::new();
    if let Some(reasoning) =
        reasoning_message_text(message).filter(|reasoning| !reasoning.trim().is_empty())
    {
        content_blocks.push(ContentBlock::Thinking {
            signature: None,
            state: None,
            thinking: reasoning.to_string(),
        });
    }
    let (mistral_thinking, mistral_text) = if is_exact_mistral_chat_route(provider, base_url) {
        extract_mistral_polymorphic_content(message)
    } else {
        (None, None)
    };
    if let Some(thinking) = mistral_thinking.filter(|s| !s.trim().is_empty()) {
        content_blocks.push(ContentBlock::Thinking {
            signature: None,
            state: None,
            thinking,
        });
    }
    if let Some(text) = mistral_text.filter(|s| !s.trim().is_empty()) {
        content_blocks.push(ContentBlock::Text {
            text,
            cache_control: None,
        });
    } else if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        content_blocks.push(ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        });
    }

    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("tool_call")
                .to_string();
            let function = call.get("function");
            let name = tool_name_or_fallback(
                function.and_then(|f| f.get("name")).and_then(Value::as_str),
                &id,
                "Non-streaming response",
            );
            let arguments = function
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .map(|raw| serde_json::from_str(raw).unwrap_or(Value::String(raw.to_string())))
                .unwrap_or(Value::Null);
            let caller = call.get("caller").and_then(|v| {
                v.get("type")
                    .and_then(Value::as_str)
                    .map(|caller_type| ToolCaller {
                        caller_type: caller_type.to_string(),
                        tool_id: v
                            .get("tool_id")
                            .and_then(Value::as_str)
                            .map(std::string::ToString::to_string),
                    })
            });

            let thought_signature = call
                .pointer("/extra_content/google/thought_signature")
                .and_then(Value::as_str)
                .map(str::to_string);
            content_blocks.push(ContentBlock::ToolUse {
                execution_id: None,
                id,
                name: from_api_tool_name(&name),
                input: arguments,
                caller,
                thought_signature,
            });
        }
    }

    let usage = parse_usage(payload.get("usage"));

    Ok(MessageResponse {
        id,
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content: content_blocks,
        model,
        stop_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        stop_sequence: None,
        container: None,
        usage,
    })
}

#[derive(Debug, Default)]
struct InlineReasoningTagState {
    inside_think: bool,
    pending: String,
}

#[derive(Debug, PartialEq, Eq)]
enum ReasoningSegment {
    Text(String),
    Thinking(String),
}

fn inline_reasoning_segments(
    content: &str,
    state: &mut InlineReasoningTagState,
    flush: bool,
) -> Vec<ReasoningSegment> {
    state.pending.push_str(content);
    let mut segments = Vec::new();

    loop {
        if state.pending.is_empty() {
            break;
        }

        if state.inside_think {
            if let Some(close_at) = state.pending.find("</think>") {
                push_reasoning_segment(
                    &mut segments,
                    ReasoningSegment::Thinking(state.pending[..close_at].to_string()),
                );
                state.pending.drain(..close_at + "</think>".len());
                state.inside_think = false;
                continue;
            }

            let hold_len = if flush {
                0
            } else {
                trailing_tag_prefix_len(&state.pending, "</think>")
            };
            let emit_len = state.pending.len().saturating_sub(hold_len);
            if emit_len > 0 {
                push_reasoning_segment(
                    &mut segments,
                    ReasoningSegment::Thinking(state.pending[..emit_len].to_string()),
                );
                state.pending.drain(..emit_len);
            }
            break;
        }

        if let Some(open_at) = state.pending.find("<think>") {
            push_reasoning_segment(
                &mut segments,
                ReasoningSegment::Text(state.pending[..open_at].to_string()),
            );
            state.pending.drain(..open_at + "<think>".len());
            state.inside_think = true;
            continue;
        }

        let hold_len = if flush {
            0
        } else {
            trailing_tag_prefix_len(&state.pending, "<think>")
        };
        let emit_len = state.pending.len().saturating_sub(hold_len);
        if emit_len > 0 {
            push_reasoning_segment(
                &mut segments,
                ReasoningSegment::Text(state.pending[..emit_len].to_string()),
            );
            state.pending.drain(..emit_len);
        }
        break;
    }

    segments
}

fn trailing_tag_prefix_len(content: &str, tag: &str) -> usize {
    let max_len = tag.len().min(content.len());
    for len in (1..=max_len).rev() {
        let start = content.len() - len;
        if content.is_char_boundary(start) && tag.starts_with(&content[start..]) {
            return len;
        }
    }
    0
}

fn push_reasoning_segment(segments: &mut Vec<ReasoningSegment>, segment: ReasoningSegment) {
    match &segment {
        ReasoningSegment::Text(text) | ReasoningSegment::Thinking(text) if text.is_empty() => {}
        _ => segments.push(segment),
    }
}

fn push_text_delta(
    events: &mut Vec<StreamEvent>,
    content_index: &mut u32,
    text_started: &mut bool,
    thinking_started: &mut bool,
    text: String,
) {
    if *thinking_started {
        events.push(StreamEvent::ContentBlockStop {
            index: *content_index,
        });
        *content_index += 1;
        *thinking_started = false;
    }
    if !*text_started {
        events.push(StreamEvent::ContentBlockStart {
            index: *content_index,
            content_block: ContentBlockStart::Text {
                text: String::new(),
            },
        });
        *text_started = true;
    }
    events.push(StreamEvent::ContentBlockDelta {
        index: *content_index,
        delta: Delta::TextDelta { text },
    });
}

fn push_thinking_delta(
    events: &mut Vec<StreamEvent>,
    content_index: &mut u32,
    text_started: &mut bool,
    thinking_started: &mut bool,
    thinking: String,
) {
    if *text_started {
        events.push(StreamEvent::ContentBlockStop {
            index: *content_index,
        });
        *content_index += 1;
        *text_started = false;
    }
    if !*thinking_started {
        events.push(StreamEvent::ContentBlockStart {
            index: *content_index,
            content_block: ContentBlockStart::Thinking {
                thinking: String::new(),
            },
        });
        *thinking_started = true;
    }
    events.push(StreamEvent::ContentBlockDelta {
        index: *content_index,
        delta: Delta::ThinkingDelta { thinking },
    });
}

// === SSE Chunk Parser ===

enum SseDataFrame {
    Done,
    Events(Vec<StreamEvent>),
}

// The six `&mut` streaming-state fields plus the style flag are a deliberate,
// shared parser-state set (mirrored by `parse_sse_chunk*`); bundling them into a
// struct would only add reborrow noise on this hot SSE path.
#[allow(clippy::too_many_arguments)]
fn parse_sse_data_frame(
    data: &str,
    content_index: &mut u32,
    text_started: &mut bool,
    thinking_started: &mut bool,
    tool_indices: &mut std::collections::HashMap<u32, u32>,
    reasoning_detail_buffers: &mut std::collections::HashMap<u32, String>,
    inline_reasoning_tags: &mut InlineReasoningTagState,
    reasoning_stream_style: ReasoningStreamStyle,
) -> SseDataFrame {
    if data.trim() == "[DONE]" {
        return SseDataFrame::Done;
    }
    let events = serde_json::from_str::<Value>(data).map_or_else(
        |_| Vec::new(),
        |chunk_json| {
            parse_sse_chunk_with_reasoning_style(
                &chunk_json,
                content_index,
                text_started,
                thinking_started,
                tool_indices,
                reasoning_detail_buffers,
                inline_reasoning_tags,
                reasoning_stream_style,
            )
        },
    );
    SseDataFrame::Events(events)
}

/// Parse a single SSE chunk from the Chat Completions streaming API into
/// our internal `StreamEvent` representation.
#[cfg(test)]
pub(super) fn parse_sse_chunk(
    chunk: &Value,
    content_index: &mut u32,
    text_started: &mut bool,
    thinking_started: &mut bool,
    tool_indices: &mut std::collections::HashMap<u32, u32>,
    reasoning_detail_buffers: &mut std::collections::HashMap<u32, String>,
    is_reasoning_model: bool,
) -> Vec<StreamEvent> {
    let mut inline_reasoning_tags = InlineReasoningTagState::default();
    let reasoning_stream_style = if is_reasoning_model {
        ReasoningStreamStyle::SeparateField
    } else {
        ReasoningStreamStyle::None
    };
    parse_sse_chunk_with_reasoning_style(
        chunk,
        content_index,
        text_started,
        thinking_started,
        tool_indices,
        reasoning_detail_buffers,
        &mut inline_reasoning_tags,
        reasoning_stream_style,
    )
}

// Same deliberate shared parser-state set as `parse_sse_data_frame`.
#[allow(clippy::too_many_arguments)]
fn parse_sse_chunk_with_reasoning_style(
    chunk: &Value,
    content_index: &mut u32,
    text_started: &mut bool,
    thinking_started: &mut bool,
    tool_indices: &mut std::collections::HashMap<u32, u32>,
    reasoning_detail_buffers: &mut std::collections::HashMap<u32, String>,
    inline_reasoning_tags: &mut InlineReasoningTagState,
    reasoning_stream_style: ReasoningStreamStyle,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    // OpenAI-compatible providers surface mid-stream failures as a chunk-level
    // `error` object (sometimes with `type: "error"`), delivered before
    // `[DONE]`. Silently dropping it turned rate-limit / context-length /
    // server errors into a truncated turn that looked successful — the frame
    // is now surfaced through the same `StreamEvent::Error` contract the
    // Anthropic path uses (#3014, ops R3).
    if let Some(error) = chunk.get("error") {
        let error = match error {
            Value::Object(_) => error.clone(),
            Value::String(message) => serde_json::json!({ "message": message }),
            _ => serde_json::json!({ "message": "provider stream error" }),
        };
        events.push(StreamEvent::Error { error });
        return events;
    }

    let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
        // Usage-only chunk (sent at end with stream_options)
        if let Some(usage_val) = chunk.get("usage") {
            let usage = parse_usage(Some(usage_val));
            events.push(StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: None,
                    stop_sequence: None,
                },
                usage: Some(usage),
            });
        }
        return events;
    };

    if choices.is_empty() {
        if let Some(usage_val) = chunk.get("usage") {
            let usage = parse_usage(Some(usage_val));
            events.push(StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: None,
                    stop_sequence: None,
                },
                usage: Some(usage),
            });
        }
        return events;
    }

    for choice in choices {
        let choice_index = choice.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        let delta = choice.get("delta");
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);

        if let Some(delta) = delta {
            let reasoning_text = reasoning_delta(delta, choice_index, reasoning_detail_buffers)
                .filter(|s| !s.is_empty());
            // Mistral la Plateforme streams reasoning as a polymorphic
            // `delta.content` value: an array of typed {type: thinking|text}
            // blocks while thinking, then a plain string once the final
            // answer starts. Flatten thinking sub-blocks into a single
            // reasoning delta and treat text sub-blocks as normal content
            // before the shared string fallback below.
            let (mistral_thinking, mistral_text) =
                if reasoning_stream_style == ReasoningStreamStyle::MistralBlocks {
                    extract_mistral_polymorphic_content(delta)
                } else {
                    (None, None)
                };
            if let Some(reasoning) = mistral_thinking.as_deref() {
                push_thinking_delta(
                    &mut events,
                    content_index,
                    text_started,
                    thinking_started,
                    reasoning.to_string(),
                );
            }
            let content_text = mistral_text.or_else(|| {
                delta
                    .get("content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });

            // Handle reasoning_content / reasoning thinking deltas.
            if reasoning_stream_style == ReasoningStreamStyle::SeparateField
                && let Some(reasoning) = reasoning_text.as_deref()
            {
                push_thinking_delta(
                    &mut events,
                    content_index,
                    text_started,
                    thinking_started,
                    reasoning.to_string(),
                );
            }

            // Generic OpenAI-compatible proxies sometimes stream answer text
            // in `reasoning_content`. If this route is configured with no
            // reasoning semantics, render that field as normal text when no
            // `content` delta is present.
            match (content_text, reasoning_stream_style) {
                (Some(content), ReasoningStreamStyle::InlineTags) => {
                    for segment in inline_reasoning_segments(&content, inline_reasoning_tags, false)
                    {
                        match segment {
                            ReasoningSegment::Text(text) => push_text_delta(
                                &mut events,
                                content_index,
                                text_started,
                                thinking_started,
                                text,
                            ),
                            ReasoningSegment::Thinking(thinking) => push_thinking_delta(
                                &mut events,
                                content_index,
                                text_started,
                                thinking_started,
                                thinking,
                            ),
                        }
                    }
                }
                (Some(content), _) => push_text_delta(
                    &mut events,
                    content_index,
                    text_started,
                    thinking_started,
                    content,
                ),
                (None, ReasoningStreamStyle::None) => {
                    if let Some(content) = reasoning_text {
                        push_text_delta(
                            &mut events,
                            content_index,
                            text_started,
                            thinking_started,
                            content,
                        );
                    }
                }
                (None, _) => {}
            }

            // Handle tool calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tool_calls {
                    let tc_index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                    let tool_block_index = match tool_indices.entry(tc_index) {
                        std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            // Close text block if transitioning to tool use
                            if *text_started {
                                events.push(StreamEvent::ContentBlockStop {
                                    index: *content_index,
                                });
                                *content_index += 1;
                                *text_started = false;
                            }
                            if *thinking_started {
                                events.push(StreamEvent::ContentBlockStop {
                                    index: *content_index,
                                });
                                *content_index += 1;
                                *thinking_started = false;
                            }

                            let block_index = *content_index;
                            let id = tc
                                .get("id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                // Some upstream gateways (and the responses-API
                                // bridge) elide the `id` on the first chunk of a
                                // tool call. Falling back to a constant string
                                // collides when the model emits parallel tool
                                // calls in the same delta — every call ended up
                                // with the same id and downstream tool-result
                                // routing matched the first one twice. Index by
                                // the content-block position to keep the
                                // fallback unique within the response.
                                .unwrap_or_else(|| format!("call_{block_index}"));
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str);
                            let name = tool_name_or_fallback(name, &id, "Streaming response chunk");
                            let caller = tc.get("caller").and_then(|v| {
                                v.get("type").and_then(Value::as_str).map(|caller_type| {
                                    ToolCaller {
                                        caller_type: caller_type.to_string(),
                                        tool_id: v
                                            .get("tool_id")
                                            .and_then(Value::as_str)
                                            .map(std::string::ToString::to_string),
                                    }
                                })
                            });

                            let thought_signature = tc
                                .pointer("/extra_content/google/thought_signature")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            events.push(StreamEvent::ContentBlockStart {
                                index: block_index,
                                content_block: ContentBlockStart::ToolUse {
                                    id,
                                    name: from_api_tool_name(&name),
                                    input: json!({}),
                                    caller,
                                    thought_signature,
                                },
                            });
                            *content_index = (*content_index).saturating_add(1);
                            entry.insert(block_index);
                            block_index
                        }
                    };

                    // Stream tool call arguments
                    if let Some(args) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        && !args.is_empty()
                    {
                        events.push(StreamEvent::ContentBlockDelta {
                            index: tool_block_index,
                            delta: Delta::InputJsonDelta {
                                partial_json: args.to_string(),
                            },
                        });
                    }
                }
            }
        }

        // Handle finish reason
        if let Some(reason) = finish_reason {
            if reasoning_stream_style == ReasoningStreamStyle::InlineTags {
                for segment in inline_reasoning_segments("", inline_reasoning_tags, true) {
                    match segment {
                        ReasoningSegment::Text(text) => push_text_delta(
                            &mut events,
                            content_index,
                            text_started,
                            thinking_started,
                            text,
                        ),
                        ReasoningSegment::Thinking(thinking) => push_thinking_delta(
                            &mut events,
                            content_index,
                            text_started,
                            thinking_started,
                            thinking,
                        ),
                    }
                }
            }
            // Close any open blocks
            if *text_started {
                events.push(StreamEvent::ContentBlockStop {
                    index: *content_index,
                });
                *text_started = false;
            }
            if *thinking_started {
                events.push(StreamEvent::ContentBlockStop {
                    index: *content_index,
                });
                *thinking_started = false;
            }
            // Close tool blocks
            let mut open_tool_indices: Vec<u32> =
                tool_indices.drain().map(|(_, idx)| idx).collect();
            open_tool_indices.sort_unstable();
            for tool_block_index in open_tool_indices {
                events.push(StreamEvent::ContentBlockStop {
                    index: tool_block_index,
                });
            }

            // Emit usage from the chunk if available
            let chunk_usage = chunk.get("usage").map(|u| parse_usage(Some(u)));
            events.push(StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(reason),
                    stop_sequence: None,
                },
                usage: chunk_usage,
            });
        }
    }

    events
}

fn tool_name_or_fallback(name: Option<&str>, id: &str, source: &str) -> String {
    let trimmed = name.unwrap_or("").trim();
    if trimmed.is_empty() {
        logging::warn(format!(
            "{source} returned an empty tool name for call {id}; using unknown_tool"
        ));
        "unknown_tool".to_string()
    } else {
        trimmed.to_string()
    }
}

// === #103 Phase 1: stream-decode diagnostics ===================================

#[cfg(test)]
mod stream_diagnostics_tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn stream_idle_timeout_reports_progress_and_timing() {
        let message = super::super::stream_entry::idle_timeout_message(
            Duration::from_secs(240),
            8192,
            Duration::from_millis(73_500),
            Duration::from_millis(41_250),
        );

        assert_eq!(
            message,
            "SSE stream idle timeout after 240s — no data received \
             (bytes_received=8192, stream_age_ms=73500, ms_since_last_chunk=41250)"
        );
    }

    #[test]
    fn chat_completions_error_frames_surface_as_stream_events() {
        let mut content_index = 0u32;
        let mut text_started = false;
        let mut thinking_started = false;
        let mut tool_indices = std::collections::HashMap::new();
        let mut reasoning_buffers = std::collections::HashMap::new();
        for chunk in [
            json!({ "error": { "message": "rate limit exceeded", "type": "rate_limit_error" } }),
            json!({ "type": "error", "error": { "message": "context length exceeded" } }),
            json!({ "error": "server error" }),
        ] {
            let events = parse_sse_chunk(
                &chunk,
                &mut content_index,
                &mut text_started,
                &mut thinking_started,
                &mut tool_indices,
                &mut reasoning_buffers,
                false,
            );
            assert_eq!(
                events.len(),
                1,
                "a chunk-level error frame must not be swallowed ({chunk})"
            );
            match &events[0] {
                StreamEvent::Error { error } => assert!(
                    !error.is_null()
                        && (error.get("message").and_then(Value::as_str).is_some()
                            || error.is_string()),
                    "the provider error message must survive parsing: {error}"
                ),
                other => panic!("expected StreamEvent::Error, got {other:?}"),
            }
        }
        // A normal content chunk still parses as a delta after the error path.
        let deltas = parse_sse_chunk(
            &json!({"choices": [{"index": 0, "delta": {"content": "ok"}}]}),
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_buffers,
            false,
        );
        assert!(
            deltas.iter().any(|event| {
                matches!(
                    event,
                    StreamEvent::ContentBlockDelta {
                        delta: Delta::TextDelta { text }, ..
                    } if text == "ok"
                )
            }),
            "content deltas still parse: {deltas:?}"
        );
    }

    #[test]
    fn deepseek_thinking_omits_tool_choice() {
        for effort in [Some("high"), Some("max"), Some("medium"), Some("")] {
            assert!(
                !should_send_tool_choice_for_chat(ProviderKind::Deepseek, effort),
                "DeepSeek thinking rejects explicit tool_choice for {effort:?}"
            );
            assert!(
                !should_send_tool_choice_for_chat(ProviderKind::Deepseek, effort),
                "DeepSeek CN thinking rejects explicit tool_choice for {effort:?}"
            );
        }

        for effort in [
            None,
            Some("off"),
            Some("disabled"),
            Some("none"),
            Some("false"),
        ] {
            assert!(should_send_tool_choice_for_chat(
                ProviderKind::Deepseek,
                effort
            ));
        }
        assert!(should_send_tool_choice_for_chat(
            ProviderKind::Openrouter,
            Some("high")
        ));
    }

    #[test]
    fn format_stream_headers_renders_all_fields_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", HeaderValue::from_static("gzip"));
        headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        headers.insert("server", HeaderValue::from_static("openresty/1.25.3.1"));

        let rendered = format_stream_headers(&headers);
        // Order is fixed by FIELDS in the helper; assert each field appears.
        assert!(
            rendered.contains("content-encoding=gzip"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains("transfer-encoding=chunked"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains("connection=keep-alive"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains("server=openresty/1.25.3.1"),
            "got: {rendered}"
        );
    }

    #[test]
    fn format_stream_headers_marks_missing_fields_as_absent() {
        // DeepSeek frequently omits content-encoding when not compressing.
        // The diagnostic must still produce a parseable line so log scrapers
        // don't lose the slot.
        let headers = HeaderMap::new();
        let rendered = format_stream_headers(&headers);
        assert!(
            rendered.contains("content-encoding=(absent)"),
            "missing field must be explicitly marked; got: {rendered}"
        );
        assert!(
            rendered.contains("transfer-encoding=(absent)"),
            "missing field must be explicitly marked; got: {rendered}"
        );
    }

    #[test]
    fn format_stream_headers_handles_non_ascii_value_gracefully() {
        // If a header value isn't UTF-8, `.to_str()` fails — we must not panic
        // and should still produce a parseable line.
        let mut headers = HeaderMap::new();
        // 0xFF is a valid byte but invalid UTF-8 start byte.
        headers.insert(
            "server",
            HeaderValue::from_bytes(b"\xff\xfemystery").expect("header value"),
        );
        let rendered = format_stream_headers(&headers);
        assert!(
            rendered.contains("server=(absent)"),
            "non-UTF8 header values fall back to (absent); got: {rendered}"
        );
    }
}

#[cfg(test)]
mod arcee_waf_message_encoding_tests {
    use super::build_chat_messages_for_request_and_provider;
    use crate::config::ProviderKind;
    use codewhale_models::{MessageRequest, SystemPrompt};
    use serde_json::Value;

    fn request_with_system(system: &str) -> MessageRequest {
        MessageRequest {
            model: "trinity-large-thinking".to_string(),
            messages: Vec::new(),
            max_tokens: 16,
            system: Some(SystemPrompt::Text(system.to_string())),
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    fn decoded_content(content: &Value) -> String {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        content
            .as_array()
            .expect("content parts")
            .iter()
            .map(|part| part.get("text").and_then(Value::as_str).expect("text part"))
            .collect()
    }

    #[test]
    fn arcee_splits_waf_trigger_without_changing_decoded_system_prompt() {
        let system = "Run calculations with `python -c 'print(1)'` when a tool is available.";
        let request = request_with_system(system);

        let messages = build_chat_messages_for_request_and_provider(&request, ProviderKind::Arcee);
        let content = &messages[0]["content"];

        assert!(
            content.is_array(),
            "Arcee system content with a WAF trigger should be encoded as text parts"
        );
        assert_eq!(decoded_content(content), system);
        let serialized = serde_json::to_string(&messages).expect("serialize messages");
        assert!(
            !serialized.contains("python -c"),
            "wire JSON should not contain the Cloudflare trigger contiguously: {serialized}"
        );
    }

    #[test]
    fn non_arcee_providers_keep_system_prompt_as_string() {
        let system = "Run calculations with `python -c 'print(1)'` when a tool is available.";
        let request = request_with_system(system);

        let messages = build_chat_messages_for_request_and_provider(&request, ProviderKind::Openai);

        assert_eq!(messages[0]["content"].as_str(), Some(system));
    }

    #[test]
    fn arcee_keeps_non_triggering_system_prompt_as_string() {
        let system = "Use read-only tools to inspect files before reporting results.";
        let request = request_with_system(system);

        let messages = build_chat_messages_for_request_and_provider(&request, ProviderKind::Arcee);

        assert_eq!(messages[0]["content"].as_str(), Some(system));
    }
}

#[cfg(test)]
mod minimax_reasoning_replay_tests {
    use super::{
        build_chat_messages_for_request_and_provider,
        build_chat_messages_for_request_and_provider_and_route,
    };
    use crate::config::{
        DEFAULT_KIMI_CODE_BASE_URL, DEFAULT_MINIMAX_MODEL, DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL,
        DEFAULT_MOONSHOT_BASE_URL, KIMI_CODE_K3_MODEL, ProviderKind,
    };
    use codewhale_models::Role;
    use codewhale_models::{ContentBlock, Message, MessageRequest};

    fn request_with_assistant_thinking() -> MessageRequest {
        MessageRequest {
            model: DEFAULT_MINIMAX_MODEL.to_string(),
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "Inspect tool state".to_string(),
                        signature: None,
                        state: None,
                    },
                    ContentBlock::Text {
                        text: "Done.".to_string(),
                        cache_control: None,
                    },
                ],
            }],
            max_tokens: 16,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    #[test]
    fn minimax_history_replays_thinking_as_reasoning_details() {
        let request = request_with_assistant_thinking();

        let messages =
            build_chat_messages_for_request_and_provider(&request, ProviderKind::Minimax);
        let assistant = &messages[0];

        assert_eq!(
            assistant
                .get("reasoning_content")
                .and_then(|value| value.as_str()),
            Some("Inspect tool state")
        );
        assert_eq!(
            assistant
                .pointer("/reasoning_details/0/type")
                .and_then(|value| value.as_str()),
            Some("text")
        );
        assert_eq!(
            assistant
                .pointer("/reasoning_details/0/text")
                .and_then(|value| value.as_str()),
            Some("Inspect tool state")
        );
    }

    #[test]
    fn kimi_code_k3_replays_thinking_only_on_the_exact_membership_route() {
        let mut request = request_with_assistant_thinking();
        request.model = KIMI_CODE_K3_MODEL.to_string();

        let exact = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::Moonshot,
            DEFAULT_KIMI_CODE_BASE_URL,
        );
        assert_eq!(
            exact[0]
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str),
            Some("Inspect tool state")
        );

        let neighbor = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::Moonshot,
            DEFAULT_MOONSHOT_BASE_URL,
        );
        assert!(
            neighbor[0].get("reasoning_content").is_none(),
            "a generic Moonshot k3 identifier must not inherit Kimi Code replay"
        );
    }

    #[test]
    fn modelstudio_qwen38_wire_body_replays_no_historical_reasoning_across_tool_loop() {
        // One user handoff, one assistant Thinking + Text + ToolUse turn, and
        // its matching ToolResult. On the exact Model Studio route the
        // always-thinking qwen3.8 family must not receive historical
        // `reasoning_content`, while the handoff occurs exactly once and the
        // tool call id, arguments, and result stay intact.
        let mut request = request_with_assistant_thinking();
        request.model = "qwen3.8-max".to_string();
        request.messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "HANDOFF-SENTINEL: fix the widget.".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "stale thinking from the prior turn".to_string(),
                        signature: None,
                        state: None,
                    },
                    ContentBlock::Text {
                        text: "I'll read the widget first.".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call_qwen38_001".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({ "path": "widget.rs" }),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_qwen38_001".to_string(),
                    content: "widget.rs: struct Widget { .. }".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let messages = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::ModelstudioTokenPlan,
            DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL,
        );

        // The handoff occurs exactly once, as a plain user text message.
        let handoff_carriers: Vec<&serde_json::Value> = messages
            .iter()
            .filter(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("user")
                    && message
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|content| content.contains("HANDOFF-SENTINEL"))
            })
            .collect();
        assert_eq!(
            handoff_carriers.len(),
            1,
            "the handoff must appear in exactly one user message: {messages:?}"
        );

        // The assistant turn keeps its text and tool call, but no historical
        // reasoning_content for qwen3.8.
        let assistant = messages
            .iter()
            .find(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
            })
            .expect("assistant message");
        assert_eq!(
            assistant.get("content").and_then(serde_json::Value::as_str),
            Some("I'll read the widget first.")
        );
        assert!(
            assistant.get("reasoning_content").is_none(),
            "qwen3.8 must not receive historical reasoning_content: {assistant:?}"
        );
        let tool_calls = assistant
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
            .expect("tool_calls array");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], serde_json::json!("call_qwen38_001"));
        assert_eq!(
            tool_calls[0].pointer("/function/name"),
            Some(&serde_json::json!("read"))
        );
        assert_eq!(
            tool_calls[0].pointer("/function/arguments"),
            Some(&serde_json::json!(r#"{"path":"widget.rs"}"#))
        );

        // The matching tool result rides along under its original id.
        let tool_result = messages
            .iter()
            .find(|message| message.get("role").and_then(serde_json::Value::as_str) == Some("tool"))
            .expect("tool result message");
        assert_eq!(
            tool_result.get("tool_call_id"),
            Some(&serde_json::json!("call_qwen38_001"))
        );
        assert_eq!(
            tool_result
                .get("content")
                .and_then(serde_json::Value::as_str),
            Some("widget.rs: struct Widget { .. }")
        );

        // Control: the same handoff/tool loop with a documented
        // preserve-thinking model replays its historical reasoning, proving
        // the strip above is model-gated, not route-gated.
        let mut preserve_request = request;
        preserve_request.model = "qwen3.7-plus".to_string();
        let preserve_messages = build_chat_messages_for_request_and_provider_and_route(
            &preserve_request,
            ProviderKind::ModelstudioTokenPlan,
            DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL,
        );
        let preserve_assistant = preserve_messages
            .iter()
            .find(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
            })
            .expect("assistant message");
        assert_eq!(
            preserve_assistant
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str),
            Some("stale thinking from the prior turn"),
            "documented preserve-thinking models keep replaying history"
        );
    }
}

// === #103 Phase 4: SSE decoder behavior on canned chunk sequences ============

#[cfg(test)]
#[path = "chat/tests/stream_decoder.rs"]
mod stream_decoder_tests;

#[cfg(test)]
mod alias_thinking_detection_tests {
    //! Regression coverage for the DeepSeek public model aliases.
    //!
    //! `deepseek-chat` and `deepseek-reasoner` are the canonical alias names
    //! published in DeepSeek's API docs. Server-side they resolve to V4-flash
    //! and V4-pro respectively, both of which have thinking mode enabled by
    //! default. If the TUI does not classify those aliases as reasoning
    //! models, the sanitizer skips replaying `reasoning_content` on tool-call
    //! assistant messages and DeepSeek returns a 400 ("the `reasoning_content`
    //! in the thinking mode must be passed back to the API") on the second
    //! turn. See upstream API docs:
    //! <https://api-docs.deepseek.com/guides/thinking_mode>
    use super::{
        ReasoningStreamStyle, apply_direct_moonshot_k3_fixed_sampling,
        apply_inkling_reasoning_effort, apply_kimi_code_fixed_sampling,
        apply_kimi_code_k3_reasoning_effort, apply_openai_reasoning_effort,
        apply_provider_token_limit, apply_route_reasoning_controls, is_reasoning_model_for_stream,
        provider_accepts_reasoning_content, reasoning_stream_style_for_route,
        requires_reasoning_content, should_replay_reasoning_content,
        should_replay_reasoning_content_for_provider,
        should_replay_reasoning_content_for_provider_on_route,
    };
    use crate::config::ProviderKind;
    use serde_json::json;

    #[test]
    fn aliases_routed_to_v4_require_reasoning_content() {
        // Documented public aliases.
        assert!(requires_reasoning_content("deepseek-chat"));
        assert!(requires_reasoning_content("deepseek-reasoner"));
        // Case-insensitive: users sometimes copy/paste with capitalisation.
        assert!(requires_reasoning_content("DeepSeek-Chat"));
        assert!(requires_reasoning_content("DEEPSEEK-REASONER"));
    }

    #[test]
    fn explicit_v4_ids_still_require_reasoning_content() {
        // Direct V4 IDs continue to match (regression guard for the existing
        // `lower.contains("deepseek-v4")` branch).
        assert!(requires_reasoning_content("deepseek-v4-flash"));
        assert!(requires_reasoning_content("deepseek-v4-pro"));
    }

    #[test]
    fn non_thinking_aliases_remain_excluded() {
        // Legacy non-thinking IDs and unrelated provider models must not be
        // misclassified, otherwise we would force a placeholder
        // `reasoning_content` on providers that reject the field.
        assert!(!requires_reasoning_content("deepseek-v3"));
        assert!(!requires_reasoning_content("deepseek-coder"));
        assert!(!requires_reasoning_content("qwen3-coder"));
        assert!(!requires_reasoning_content("claude-sonnet-4-6"));
    }

    #[test]
    fn alias_prefix_handles_suffixed_variants() {
        // OpenRouter / proxy deployments occasionally suffix the canonical
        // alias (e.g. `deepseek-chat:free`). Those routes still hit V4
        // server-side, so they must continue to require reasoning_content.
        assert!(requires_reasoning_content("deepseek-chat:free"));
        assert!(requires_reasoning_content("deepseek-reasoner-2025-05"));
    }

    #[test]
    fn explicit_reasoning_off_overrides_alias_detection() {
        // `reasoning_effort = "off"` is the documented escape hatch: even when
        // the model is in the thinking family, the user can opt out and the
        // sanitizer must respect that choice.
        assert!(!should_replay_reasoning_content(
            "deepseek-chat",
            Some("off")
        ));
        assert!(!should_replay_reasoning_content(
            "deepseek-reasoner",
            Some("disabled")
        ));
        // Without an explicit override, alias models still trigger replay.
        assert!(should_replay_reasoning_content("deepseek-chat", None));
        assert!(should_replay_reasoning_content(
            "deepseek-reasoner",
            Some("medium")
        ));
    }

    #[test]
    fn generic_openai_provider_does_not_accept_reasoning_content_semantics() {
        assert!(!provider_accepts_reasoning_content(ProviderKind::Openai));
        assert!(provider_accepts_reasoning_content(ProviderKind::Deepseek));
        assert!(provider_accepts_reasoning_content(ProviderKind::NvidiaNim));
        assert!(provider_accepts_reasoning_content(ProviderKind::XiaomiMimo));
        assert!(provider_accepts_reasoning_content(ProviderKind::Arcee));
        assert!(provider_accepts_reasoning_content(ProviderKind::Minimax));
        assert!(provider_accepts_reasoning_content(ProviderKind::Zai));
        // #3016: Moonshot's native endpoint streams Kimi thinking as
        // reasoning_content.
        assert!(provider_accepts_reasoning_content(ProviderKind::Moonshot));
    }

    /// Alibaba's classic pay-as-you-go DashScope endpoints are genuine
    /// Alibaba Chat Completions hosts serving the same models; before
    /// 2026-08-04 they were missing from the verifier allowlist, so every
    /// reasoning control was silently stripped there (fail-closed feature
    /// loss, not a leak). The intl spelling matches provider_defaults.
    #[test]
    fn classic_dashscope_hosts_are_verified_modelstudio_chat_routes() {
        for base_url in [
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/",
        ] {
            assert!(
                super::is_exact_modelstudio_chat_route(
                    ProviderKind::ModelstudioTokenPlan,
                    base_url
                ),
                "{base_url}"
            );
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                "qwen3.7-plus",
                Some("off"),
            );
            assert_eq!(body["enable_thinking"], json!(false), "{base_url}: {body}");
        }
        // Lookalike hosts stay unverified — fail closed.
        for base_url in [
            "https://dashscope.aliyuncs.com.evil.example/compatible-mode/v1",
            "https://notdashscope.aliyuncs.com/compatible-mode/v1",
            "https://dashscope.aliyuncs.com/other-path/v1",
        ] {
            assert!(
                !super::is_exact_modelstudio_chat_route(
                    ProviderKind::ModelstudioTokenPlan,
                    base_url
                ),
                "{base_url}"
            );
        }
    }

    #[test]
    fn modelstudio_hybrid_routes_send_documented_thinking_controls() {
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        for (effort, enabled) in [
            (None, true),
            (Some("low"), true),
            (Some("high"), true),
            (Some("xhigh"), true),
            (Some("off"), false),
        ] {
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                "qwen3.7-plus",
                effort,
            );

            assert_eq!(body["enable_thinking"], json!(enabled), "{effort:?}");
            assert_eq!(body["preserve_thinking"], json!(enabled), "{effort:?}");
            assert!(body.get("thinking").is_none(), "{effort:?}: {body}");
            assert!(body.get("reasoning_effort").is_none(), "{effort:?}: {body}");
        }
    }

    #[test]
    fn modelstudio_deepseek_v4_maps_effort_to_documented_values() {
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        for (requested, expected) in [("low", "high"), ("high", "high"), ("xhigh", "max")] {
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                "deepseek-v4-pro",
                Some(requested),
            );

            assert_eq!(body["enable_thinking"], json!(true), "{requested}");
            assert_eq!(body["reasoning_effort"], json!(expected), "{requested}");
        }
    }

    #[test]
    fn modelstudio_reasoning_controls_fail_closed_on_custom_gateways() {
        let mut body = json!({
            "enable_thinking": true,
            "preserve_thinking": true,
            "reasoning_effort": "high",
        });
        apply_route_reasoning_controls(
            &mut body,
            ProviderKind::ModelstudioTokenPlan,
            "https://proxy.example/v1",
            "qwen3.7-plus",
            Some("high"),
        );

        assert!(body.get("enable_thinking").is_none());
        assert!(body.get("preserve_thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn modelstudio_anthropic_identities_write_nothing_on_the_chat_path() {
        // The Messages adapter owns these two. If `wire = "openai"` ever routes
        // them through Chat Completions, the shaper must strip rather than
        // inherit the OpenAI-dialect fields — there is no provider-enum writer
        // left to re-add them.
        for provider in [
            ProviderKind::ModelstudioTokenPlanAnthropic,
            ProviderKind::ModelstudioCodingPlanAnthropic,
        ] {
            for base_url in [
                crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL,
                crate::config::MODELSTUDIO_TOKEN_PLAN_ANTHROPIC_BASE_URL,
            ] {
                let mut body = json!({ "enable_thinking": true });
                apply_route_reasoning_controls(
                    &mut body,
                    provider,
                    base_url,
                    "qwen3.7-plus",
                    Some("high"),
                );
                assert_eq!(body, json!({}), "{provider:?} {base_url}");
            }
        }
    }

    #[test]
    fn modelstudio_qwen38_route_streams_reasoning_without_replaying_history() {
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        for model in ["qwen3.8-max", "qwen3.8-max-preview"] {
            // qwen3.8 is thinking-only. Effort selection must never hide its
            // separate reasoning stream, including the stale `off` state
            // that can arrive before route normalization.
            assert_eq!(
                reasoning_stream_style_for_route(
                    ProviderKind::ModelstudioTokenPlan,
                    base_url,
                    model,
                    None,
                ),
                ReasoningStreamStyle::SeparateField,
                "{model}"
            );
            // ...but Alibaba does not document `preserve_thinking` for the
            // qwen3.8 family, so no historical `reasoning_content` may be
            // replayed — even when a stale effort claims thinking is off.
            // Replaying it feeds the model its own past Thinking blocks and
            // re-triggers them every turn (observed handoff loop).
            for effort in [None, Some("off"), Some("high"), Some("xhigh")] {
                assert!(
                    !should_replay_reasoning_content_for_provider_on_route(
                        ProviderKind::ModelstudioTokenPlan,
                        base_url,
                        model,
                        effort,
                    ),
                    "{model} {effort:?}"
                );
            }
            // ...and no enable/disable or preserve switch is ever sent for
            // them.
            for effort in [None, Some("off"), Some("high")] {
                let mut body = json!({});
                apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::ModelstudioTokenPlan,
                    base_url,
                    model,
                    effort,
                );
                assert!(body.get("enable_thinking").is_none(), "{model}: {body}");
                assert!(
                    body.get("preserve_thinking").is_none(),
                    "{model} {effort:?}: {body}"
                );
                assert!(body.get("reasoning_effort").is_none(), "{model}: {body}");
            }
        }
    }

    #[test]
    fn modelstudio_hybrid_route_classifies_reasoning_and_replays_history() {
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        assert_eq!(
            reasoning_stream_style_for_route(
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                "qwen3.7-plus",
                None,
            ),
            ReasoningStreamStyle::SeparateField,
        );
        assert!(should_replay_reasoning_content_for_provider_on_route(
            ProviderKind::ModelstudioTokenPlan,
            base_url,
            "qwen3.7-plus",
            None,
        ));
        assert!(!should_replay_reasoning_content_for_provider_on_route(
            ProviderKind::ModelstudioTokenPlan,
            base_url,
            "qwen3.7-plus",
            Some("off"),
        ));
    }

    #[test]
    fn modelstudio_replay_stays_narrow_until_a_live_key_confirms_it() {
        // Deliberately narrower than PR #5233: only `preserve_thinking` models
        // replay. GLM and DeepSeek-V3.x on Model Studio stay stripped until
        // someone with a key confirms DashScope accepts `reasoning_content` in
        // input messages. deepseek-v4* is unaffected — it replays through
        // `requires_reasoning_content` on every provider.
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        for model in ["glm-5.2", "deepseek-v3.2", "deepseek-v3.1"] {
            assert!(
                !should_replay_reasoning_content_for_provider_on_route(
                    ProviderKind::ModelstudioTokenPlan,
                    base_url,
                    model,
                    None,
                ),
                "{model}"
            );
        }
        assert!(should_replay_reasoning_content_for_provider_on_route(
            ProviderKind::ModelstudioTokenPlan,
            base_url,
            "deepseek-v4-pro",
            None,
        ));
    }

    #[test]
    fn modelstudio_coding_plan_chat_route_is_classified_for_all_supported_identities() {
        // The picker represents Coding Plan as mode = "coding-plan" under
        // the primary provider id, so the chat client receives
        // ModelstudioTokenPlan with the Coding Plan URL. Direct configuration
        // also retains the legacy ModelstudioCodingPlan identity.
        let base_url = crate::config::DEFAULT_MODELSTUDIO_CODING_PLAN_BASE_URL;
        for provider in [
            ProviderKind::ModelstudioTokenPlan,
            ProviderKind::ModelstudioCodingPlan,
        ] {
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                provider,
                base_url,
                "qwen3.7-plus",
                Some("high"),
            );

            assert_eq!(body["enable_thinking"], json!(true), "{provider:?}");
            assert_eq!(body["preserve_thinking"], json!(true), "{provider:?}");
            assert_eq!(
                reasoning_stream_style_for_route(provider, base_url, "qwen3.7-plus", None),
                ReasoningStreamStyle::SeparateField,
                "{provider:?}",
            );
            assert!(should_replay_reasoning_content_for_provider_on_route(
                provider,
                base_url,
                "qwen3.7-plus",
                None,
            ));
        }
    }

    #[test]
    fn modelstudio_workspace_scoped_token_plan_route_is_recognized() {
        let workspace_url =
            "https://workspace-123.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1";
        // Stream classification works on the workspace-scoped host...
        assert_eq!(
            reasoning_stream_style_for_route(
                ProviderKind::ModelstudioTokenPlan,
                workspace_url,
                "qwen3.8-max",
                None,
            ),
            ReasoningStreamStyle::SeparateField,
        );
        assert_eq!(
            reasoning_stream_style_for_route(
                ProviderKind::ModelstudioTokenPlan,
                workspace_url,
                "qwen3.7-plus",
                None,
            ),
            ReasoningStreamStyle::SeparateField,
        );
        // ...and the route gate decides replay the same way it does on the
        // default host: qwen3.8 never replays, documented preserve models do.
        assert!(!should_replay_reasoning_content_for_provider_on_route(
            ProviderKind::ModelstudioTokenPlan,
            workspace_url,
            "qwen3.8-max",
            None,
        ));
        assert!(should_replay_reasoning_content_for_provider_on_route(
            ProviderKind::ModelstudioTokenPlan,
            workspace_url,
            "qwen3.7-plus",
            None,
        ));
        let mut body = json!({});
        apply_route_reasoning_controls(
            &mut body,
            ProviderKind::ModelstudioTokenPlan,
            workspace_url,
            "qwen3.7-plus",
            Some("high"),
        );
        assert_eq!(body["preserve_thinking"], json!(true), "{body}");
    }

    #[test]
    fn modelstudio_thinking_named_unknown_model_cannot_bypass_the_route_gate() {
        // An unknown Model Studio model whose *name* suggests reasoning must
        // not gain replay: only the documented `preserve_thinking` list (plus
        // the concrete DeepSeek V4 family ids) authorizes historical
        // `reasoning_content` on exact Model Studio routes. The generic
        // `-thinking`/`reasoner` heuristics prove nothing about DashScope's
        // request dialect.
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        for model in [
            "foo-thinking",
            "foo-reasoner",
            "new-model-reasoning",
            "acme-reasoner-v2",
        ] {
            assert!(
                !should_replay_reasoning_content_for_provider_on_route(
                    ProviderKind::ModelstudioTokenPlan,
                    base_url,
                    model,
                    None,
                ),
                "{model}"
            );
            // The shaper writes no reasoning controls for it either — fail
            // closed on the wire.
            let mut body = json!({ "enable_thinking": true, "preserve_thinking": true });
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                model,
                Some("high"),
            );
            assert!(body.get("enable_thinking").is_none(), "{model}: {body}");
            assert!(body.get("preserve_thinking").is_none(), "{model}: {body}");
        }
        // A suggestive name is not a replay contract on any route.
        assert!(!requires_reasoning_content("foo-thinking"));
        assert!(!requires_reasoning_content("foo-reasoner"));
    }

    #[test]
    fn modelstudio_qwen36_flash_preserves_thinking_per_current_documentation() {
        // Current Alibaba documentation explicitly includes qwen3.6-flash
        // (and its documented snapshots) among the `preserve_thinking`
        // models. Keep positive coverage so any future narrowing of the
        // preserve list has to remove it deliberately.
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        for model in ["qwen3.6-flash", "qwen3.6-flash-2026-04-16"] {
            assert!(should_replay_reasoning_content_for_provider_on_route(
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                model,
                None,
            ));
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                model,
                Some("high"),
            );
            assert_eq!(body["enable_thinking"], json!(true), "{model}");
            assert_eq!(body["preserve_thinking"], json!(true), "{model}: {body}");
            // Hybrid semantics: an explicit off disables both, and replay
            // follows the effort gate.
            assert!(!should_replay_reasoning_content_for_provider_on_route(
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                model,
                Some("off"),
            ));
            let mut off_body = json!({});
            apply_route_reasoning_controls(
                &mut off_body,
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                model,
                Some("off"),
            );
            assert_eq!(off_body["enable_thinking"], json!(false), "{model}");
            assert_eq!(off_body["preserve_thinking"], json!(false), "{model}");
        }

        for invented in [
            "qwen3.6-flash-future",
            "qwen3.7-plus-proxy",
            "kimi-k2.7-code-unverified",
        ] {
            assert!(
                !should_replay_reasoning_content_for_provider_on_route(
                    ProviderKind::ModelstudioTokenPlan,
                    base_url,
                    invented,
                    None,
                ),
                "prefix lookalikes must fail closed: {invented}"
            );
        }
    }

    #[test]
    fn modelstudio_kimi_k27_code_is_thinking_only_and_preserves_trace() {
        // NOTE: unlike the qwen3.8 pair, this classification is asserted by
        // PR #5233 rather than corroborated by models_dev.bundled.json, which
        // lists kimi-k2.7-code with `reasoning: true` and no `always_on`.
        let base_url = crate::config::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL;
        for model in [
            "kimi-k2.7-code",
            "kimi/kimi-k2.7-code",
            "kimi/kimi-k2.7-code-highspeed",
        ] {
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                model,
                Some("off"),
            );

            assert!(body.get("enable_thinking").is_none(), "{model}: {body}");
            assert_eq!(body["preserve_thinking"], json!(true), "{model}");
            assert_eq!(
                reasoning_stream_style_for_route(
                    ProviderKind::ModelstudioTokenPlan,
                    base_url,
                    model,
                    None,
                ),
                ReasoningStreamStyle::SeparateField,
                "{model}",
            );
            assert!(should_replay_reasoning_content_for_provider_on_route(
                ProviderKind::ModelstudioTokenPlan,
                base_url,
                model,
                Some("off"),
            ));
        }
    }

    #[test]
    fn stream_classifies_moonshot_kimi_as_reasoning() {
        // #3016: without this, Kimi thinking leaked into answer text.
        assert!(is_reasoning_model_for_stream(
            ProviderKind::Moonshot,
            "kimi-k2.6"
        ));
        assert!(
            is_reasoning_model_for_stream(ProviderKind::Moonshot, "kimi-for-coding"),
            "Kimi Code's stable model id now maps to K2.7 Code and streams reasoning_content"
        );
    }

    #[test]
    fn moonshot_and_minimax_replay_reasoning_content_for_supported_models() {
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Moonshot,
            "kimi-k2.7-code",
            None,
        ));
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Moonshot,
            "kimi-for-coding",
            None,
        ));
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Minimax,
            "MiniMax-M3",
            None,
        ));
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Zai,
            "GLM-5.2",
            None,
        ));
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Zai,
            "GLM-5.3",
            None,
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            ProviderKind::Moonshot,
            "kimi-for-coding",
            Some("off"),
        ));
    }

    #[test]
    fn bare_k3_reasoning_semantics_are_scoped_to_exact_kimi_code_route() {
        let kimi_code = crate::config::DEFAULT_KIMI_CODE_BASE_URL;
        let direct_moonshot = crate::config::DEFAULT_MOONSHOT_BASE_URL;

        assert!(should_replay_reasoning_content_for_provider_on_route(
            ProviderKind::Moonshot,
            kimi_code,
            crate::config::KIMI_CODE_K3_MODEL,
            Some("high"),
        ));
        assert_eq!(
            reasoning_stream_style_for_route(
                ProviderKind::Moonshot,
                kimi_code,
                crate::config::KIMI_CODE_K3_MODEL,
                None,
            ),
            ReasoningStreamStyle::SeparateField
        );

        assert!(!should_replay_reasoning_content_for_provider_on_route(
            ProviderKind::Moonshot,
            direct_moonshot,
            crate::config::KIMI_CODE_K3_MODEL,
            Some("high"),
        ));
        // Display is not replay: a reasoning field that arrives on the
        // neighboring route still renders as Thinking (#6501), while the
        // replay contract above stays scoped to the exact membership route.
        assert_eq!(
            reasoning_stream_style_for_route(
                ProviderKind::Moonshot,
                direct_moonshot,
                crate::config::KIMI_CODE_K3_MODEL,
                None,
            ),
            ReasoningStreamStyle::SeparateField
        );
        assert!(
            should_replay_reasoning_content_for_provider_on_route(
                ProviderKind::Moonshot,
                kimi_code,
                crate::config::KIMI_CODE_K3_MODEL,
                Some("off"),
            ),
            "exact membership K3 stays always-thinking even for a stale raw Off caller"
        );
    }

    #[test]
    fn direct_moonshot_k3_is_always_thinking_and_replays_reasoning() {
        let direct = crate::config::DEFAULT_MOONSHOT_BASE_URL;
        let model = crate::config::MOONSHOT_KIMI_K3_MODEL;

        for effort in [Some("off"), Some("low"), Some("high"), Some("max"), None] {
            assert!(should_replay_reasoning_content_for_provider_on_route(
                ProviderKind::Moonshot,
                direct,
                model,
                effort,
            ));
        }
        assert_eq!(
            reasoning_stream_style_for_route(ProviderKind::Moonshot, direct, model, None),
            ReasoningStreamStyle::SeparateField
        );
    }

    #[test]
    fn xiaomi_mimo_uses_max_completion_tokens_payload_key() {
        let mut body = json!({
            "model": "mimo-v2.5-pro",
            "messages": [],
            "max_tokens": 8192,
        });

        apply_provider_token_limit(
            &mut body,
            ProviderKind::XiaomiMimo,
            "https://api.xiaomimimo.com/v1",
            "mimo-v2.5-pro",
            8192,
        );

        assert!(body.get("max_tokens").is_none());
        assert_eq!(
            body.get("max_completion_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(8192)
        );
    }

    #[test]
    fn openai_reasoning_model_uses_completion_token_limit_and_effort_field() {
        let mut body = json!({
            "model": "gpt-5.5",
            "messages": [],
            "max_tokens": 4096,
        });

        apply_provider_token_limit(
            &mut body,
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            "gpt-5.5",
            4096,
        );
        apply_openai_reasoning_effort(&mut body, ProviderKind::Openai, "gpt-5.5", Some("high"));

        assert!(body.get("max_tokens").is_none());
        assert_eq!(
            body.get("max_completion_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(4096)
        );
        assert_eq!(
            body.get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("high")
        );
    }

    #[test]
    fn gpt_56_uses_documented_max_reasoning_effort() {
        let mut body = json!({
            "model": "gpt-5.6-sol",
            "messages": [],
            "max_tokens": 8192,
        });

        apply_provider_token_limit(
            &mut body,
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            "gpt-5.6-sol",
            8192,
        );
        apply_openai_reasoning_effort(&mut body, ProviderKind::Openai, "gpt-5.6-sol", Some("max"));

        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], json!(8192));
        assert_eq!(body["reasoning_effort"], json!("max"));
    }

    #[test]
    fn grok_47_and_46_use_exact_first_party_reasoning_effort_ladder() {
        // Both catalog rows document low/medium/high/xhigh; reasoning cannot
        // be disabled, so `off` is the documented default `high`.
        for model in [
            crate::config::XAI_GROK_4_7_MODEL,
            crate::config::XAI_GROK_4_6_MODEL,
        ] {
            for (requested, expected) in [
                ("off", "high"),
                ("low", "low"),
                ("medium", "medium"),
                ("high", "high"),
                ("xhigh", "xhigh"),
                ("max", "xhigh"),
            ] {
                let mut body = json!({});
                apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::Xai,
                    crate::config::DEFAULT_XAI_BASE_URL,
                    model,
                    Some(requested),
                );
                assert_eq!(
                    body,
                    json!({ "reasoning_effort": expected }),
                    "{model} {requested}"
                );
            }
        }

        // A row without a documented effort option (grok-4.3) and an id the
        // catalog does not know send nothing.
        for model in [crate::config::XAI_GROK_4_3_MODEL, "grok-9-unknown"] {
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::Xai,
                crate::config::DEFAULT_XAI_BASE_URL,
                model,
                Some("medium"),
            );
            assert_eq!(body, json!({}), "{model}");
        }

        let mut provider_default = json!({});
        apply_route_reasoning_controls(
            &mut provider_default,
            ProviderKind::Xai,
            crate::config::DEFAULT_XAI_BASE_URL,
            crate::config::XAI_GROK_4_6_MODEL,
            Some("auto"),
        );
        assert_eq!(provider_default, json!({}));

        let mut custom = json!({});
        apply_route_reasoning_controls(
            &mut custom,
            ProviderKind::Xai,
            "https://gateway.example/v1",
            crate::config::XAI_GROK_4_6_MODEL,
            Some("medium"),
        );
        assert_eq!(custom, json!({}));
    }

    #[test]
    fn grok_45_uses_first_party_ladder_and_maps_xhigh_to_high() {
        for (requested, expected) in [
            ("off", "high"),
            ("low", "low"),
            ("medium", "medium"),
            ("high", "high"),
            ("xhigh", "high"),
            ("max", "high"),
        ] {
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::Xai,
                crate::config::DEFAULT_XAI_BASE_URL,
                crate::config::XAI_GROK_4_5_MODEL,
                Some(requested),
            );
            assert_eq!(body, json!({ "reasoning_effort": expected }), "{requested}");
        }
    }

    #[test]
    fn inkling_uses_its_exact_reasoning_vocabulary_without_thinking_extension() {
        for (requested, expected) in [
            ("off", "none"),
            ("minimal", "minimal"),
            ("low", "low"),
            ("medium", "medium"),
            ("high", "high"),
            ("max", "max"),
            ("xhigh", "max"),
        ] {
            let mut body = json!({
                "thinking": { "type": "enabled" },
                "reasoning_effort": "xhigh",
            });

            apply_inkling_reasoning_effort(
                &mut body,
                ProviderKind::Together,
                "thinkingmachines/inkling",
                Some(requested),
            );

            assert_eq!(body["reasoning_effort"], json!(expected));
            assert!(body.get("thinking").is_none());
        }
    }

    #[test]
    fn inkling_reasoning_override_is_scoped_to_the_exact_together_route() {
        let mut other_model = json!({ "thinking": { "type": "enabled" } });
        apply_inkling_reasoning_effort(
            &mut other_model,
            ProviderKind::Together,
            "deepseek-ai/DeepSeek-V4-Pro",
            Some("max"),
        );
        assert_eq!(other_model["thinking"]["type"], json!("enabled"));
        assert!(other_model.get("reasoning_effort").is_none());

        let mut other_provider = json!({ "thinking": { "type": "enabled" } });
        apply_inkling_reasoning_effort(
            &mut other_provider,
            ProviderKind::Openrouter,
            "thinkingmachines/inkling",
            Some("max"),
        );
        assert_eq!(other_provider["thinking"]["type"], json!("enabled"));
        assert!(other_provider.get("reasoning_effort").is_none());
    }

    #[test]
    fn kimi_code_k3_uses_documented_nested_thinking_effort() {
        for (requested, expected) in [
            ("low", json!({ "type": "enabled", "effort": "low" })),
            ("minimum", json!({ "type": "enabled", "effort": "low" })),
            ("light", json!({ "type": "enabled", "effort": "low" })),
            ("medium", json!({ "type": "enabled", "effort": "high" })),
            ("high", json!({ "type": "enabled", "effort": "high" })),
            ("xhigh", json!({ "type": "enabled", "effort": "max" })),
            ("ultra", json!({ "type": "enabled", "effort": "max" })),
            ("max", json!({ "type": "enabled", "effort": "max" })),
            ("none", json!({ "type": "enabled", "effort": "low" })),
            ("off", json!({ "type": "enabled", "effort": "low" })),
        ] {
            let mut body = json!({ "reasoning_effort": "stale" });
            apply_kimi_code_k3_reasoning_effort(
                &mut body,
                ProviderKind::Moonshot,
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                crate::config::KIMI_CODE_K3_MODEL,
                Some(requested),
            );

            assert_eq!(body["thinking"], expected, "requested {requested}");
            assert!(body.get("reasoning_effort").is_none());
        }
    }

    #[test]
    fn kimi_code_k3_256k_uses_k3_reasoning_and_membership_sampling_contracts() {
        let mut body = json!({
            "reasoning_effort": "stale",
            "temperature": 0.3,
            "top_p": 0.8,
        });
        apply_kimi_code_k3_reasoning_effort(
            &mut body,
            ProviderKind::Moonshot,
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_256K_MODEL,
            Some("max"),
        );
        apply_kimi_code_fixed_sampling(
            &mut body,
            ProviderKind::Moonshot,
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_256K_MODEL,
        );

        assert_eq!(
            body["thinking"],
            json!({ "type": "enabled", "effort": "max" })
        );
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn kimi_code_fixed_sampling_does_not_leak_to_neighbor_routes() {
        for (provider, base_url, model) in [
            (
                ProviderKind::Moonshot,
                crate::config::DEFAULT_MOONSHOT_BASE_URL,
                crate::config::KIMI_CODE_K3_256K_MODEL,
            ),
            (
                ProviderKind::Openrouter,
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                crate::config::KIMI_CODE_K3_256K_MODEL,
            ),
            (
                ProviderKind::Moonshot,
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                "k3-256k-preview",
            ),
        ] {
            let mut body = json!({ "temperature": 0.3, "top_p": 0.8 });
            apply_kimi_code_fixed_sampling(&mut body, provider, base_url, model);
            assert_eq!(body["temperature"], json!(0.3));
            assert_eq!(body["top_p"], json!(0.8));
        }
    }

    #[test]
    fn direct_moonshot_k3_uses_top_level_effort_and_never_disables_thinking() {
        for (requested, expected) in [
            ("off", "low"),
            ("none", "low"),
            ("low", "low"),
            ("medium", "high"),
            ("high", "high"),
            ("xhigh", "max"),
            ("max", "max"),
        ] {
            let mut body = json!({
                "model": crate::config::MOONSHOT_KIMI_K3_MODEL,
                "thinking": { "type": "disabled" },
            });
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::Moonshot,
                crate::config::DEFAULT_MOONSHOT_BASE_URL,
                crate::config::MOONSHOT_KIMI_K3_MODEL,
                Some(requested),
            );

            assert_eq!(body["reasoning_effort"], json!(expected), "{requested}");
            assert!(body.get("thinking").is_none(), "{requested}: {body}");
        }

        let mut provider_default = json!({ "thinking": { "type": "enabled" } });
        apply_route_reasoning_controls(
            &mut provider_default,
            ProviderKind::Moonshot,
            crate::config::DEFAULT_MOONSHOT_BASE_URL,
            crate::config::MOONSHOT_KIMI_K3_MODEL,
            Some("auto"),
        );
        assert!(provider_default.get("thinking").is_none());
        assert!(provider_default.get("reasoning_effort").is_none());
    }

    #[test]
    fn direct_moonshot_k3_uses_modern_token_field_and_fixed_sampling_only_on_exact_route() {
        let mut direct = json!({
            "max_tokens": 64,
            "temperature": 0.2,
            "top_p": 0.9,
        });
        apply_provider_token_limit(
            &mut direct,
            ProviderKind::Moonshot,
            crate::config::DEFAULT_MOONSHOT_BASE_URL,
            crate::config::MOONSHOT_KIMI_K3_MODEL,
            64,
        );
        apply_direct_moonshot_k3_fixed_sampling(
            &mut direct,
            ProviderKind::Moonshot,
            crate::config::DEFAULT_MOONSHOT_BASE_URL,
            crate::config::MOONSHOT_KIMI_K3_MODEL,
        );
        assert_eq!(direct["max_completion_tokens"], json!(64));
        assert!(direct.get("max_tokens").is_none());
        assert!(direct.get("temperature").is_none());
        assert!(direct.get("top_p").is_none());

        let mut neighbor = json!({
            "max_tokens": 64,
            "temperature": 0.2,
            "top_p": 0.9,
        });
        apply_provider_token_limit(
            &mut neighbor,
            ProviderKind::Moonshot,
            "https://proxy.example/v1",
            crate::config::MOONSHOT_KIMI_K3_MODEL,
            64,
        );
        apply_direct_moonshot_k3_fixed_sampling(
            &mut neighbor,
            ProviderKind::Moonshot,
            "https://proxy.example/v1",
            crate::config::MOONSHOT_KIMI_K3_MODEL,
        );
        assert_eq!(neighbor["max_tokens"], json!(64));
        assert!(neighbor.get("max_completion_tokens").is_none());
        assert_eq!(neighbor["temperature"], json!(0.2));
        assert_eq!(neighbor["top_p"], json!(0.9));
    }

    #[test]
    fn direct_and_membership_k3_reasoning_dialects_do_not_cross_routes() {
        let mut membership = json!({});
        apply_route_reasoning_controls(
            &mut membership,
            ProviderKind::Moonshot,
            crate::config::DEFAULT_KIMI_CODE_BASE_URL,
            crate::config::KIMI_CODE_K3_MODEL,
            Some("max"),
        );
        assert_eq!(
            membership["thinking"],
            json!({ "type": "enabled", "effort": "max" })
        );
        assert!(membership.get("reasoning_effort").is_none());

        for (base_url, model) in [
            (
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                crate::config::MOONSHOT_KIMI_K3_MODEL,
            ),
            (
                crate::config::DEFAULT_MOONSHOT_BASE_URL,
                crate::config::KIMI_CODE_K3_MODEL,
            ),
            (
                "https://proxy.example/v1",
                crate::config::MOONSHOT_KIMI_K3_MODEL,
            ),
        ] {
            let mut neighbor = json!({});
            apply_route_reasoning_controls(
                &mut neighbor,
                ProviderKind::Moonshot,
                base_url,
                model,
                Some("max"),
            );
            assert_eq!(
                neighbor["thinking"],
                json!({ "type": "enabled" }),
                "{base_url} / {model}"
            );
            assert!(neighbor.get("reasoning_effort").is_none());
            assert!(neighbor.pointer("/thinking/effort").is_none());
        }
    }

    #[test]
    fn kimi_code_k3_effort_override_never_leaks_to_neighbor_routes() {
        for (base_url, model) in [
            (crate::config::DEFAULT_KIMI_CODE_BASE_URL, "kimi-k3"),
            (
                crate::config::DEFAULT_KIMI_CODE_BASE_URL,
                crate::config::DEFAULT_KIMI_CODE_MODEL,
            ),
            (crate::config::DEFAULT_MOONSHOT_BASE_URL, "k3"),
        ] {
            let mut body = json!({ "thinking": { "type": "enabled" } });
            apply_kimi_code_k3_reasoning_effort(
                &mut body,
                ProviderKind::Moonshot,
                base_url,
                model,
                Some("max"),
            );

            assert_eq!(body["thinking"], json!({ "type": "enabled" }));
            assert!(
                body.pointer("/thinking/effort").is_none(),
                "{base_url} / {model}"
            );
            assert!(body.get("reasoning_effort").is_none());
        }
    }

    #[test]
    fn muse_spark_uses_meta_reasoning_effort_without_openai_token_rewrite() {
        let mut body = json!({
            "model": "muse-spark-1.1",
            "messages": [],
            "max_tokens": 8192,
        });

        apply_provider_token_limit(
            &mut body,
            ProviderKind::Meta,
            "https://api.meta.ai/v1",
            "muse-spark-1.1",
            8192,
        );
        apply_openai_reasoning_effort(&mut body, ProviderKind::Meta, "muse-spark-1.1", Some("max"));

        assert_eq!(body["max_tokens"], json!(8192));
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["reasoning_effort"], json!("xhigh"));
    }

    #[test]
    fn provider_regression_5853_muse_family_preserves_reasoning_effort() {
        for model in [
            "muse-spark-1.2",
            "muse-spark-1.3",
            "muse-spark-1.3-contributor",
            " MUSE-SPARK-1.3 ",
        ] {
            for (effort, wire) in [
                ("low", "low"),
                ("medium", "medium"),
                ("high", "high"),
                ("xhigh", "xhigh"),
                ("max", "xhigh"),
                ("ultra", "xhigh"),
            ] {
                let mut body = json!({"model": model});
                apply_openai_reasoning_effort(&mut body, ProviderKind::Meta, model, Some(effort));
                assert_eq!(body["reasoning_effort"], wire, "{model}, effort={effort}");
            }
        }
        for (provider, model, effort) in [
            (ProviderKind::Openai, "muse-spark-1.3", Some("high")),
            (ProviderKind::Meta, "muse-glimmer-fixture", Some("high")),
            (ProviderKind::Meta, "muse-sparks-fixture", Some("high")),
            (ProviderKind::Meta, "muse-spark-1.3", None),
        ] {
            let mut body = json!({"model": model});
            apply_openai_reasoning_effort(&mut body, provider, model, effort);
            assert!(
                body.get("reasoning_effort").is_none(),
                "{provider:?}, {model}"
            );
        }
    }

    #[test]
    fn openai_non_reasoning_model_omits_reasoning_only_fields() {
        let mut body = json!({
            "model": "gpt-4o",
            "messages": [],
            "max_tokens": 4096,
        });

        apply_provider_token_limit(
            &mut body,
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            "gpt-4o",
            4096,
        );
        apply_openai_reasoning_effort(&mut body, ProviderKind::Openai, "gpt-4o", Some("high"));

        assert_eq!(
            body.get("max_tokens").and_then(serde_json::Value::as_u64),
            Some(4096)
        );
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn openai_provider_deepseek_compatible_model_keeps_chat_token_field() {
        let mut body = json!({
            "model": "deepseek-v4-pro",
            "messages": [],
            "max_tokens": 4096,
        });

        apply_provider_token_limit(
            &mut body,
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            "deepseek-v4-pro",
            4096,
        );
        apply_openai_reasoning_effort(
            &mut body,
            ProviderKind::Openai,
            "deepseek-v4-pro",
            Some("high"),
        );

        assert_eq!(
            body.get("max_tokens").and_then(serde_json::Value::as_u64),
            Some(4096)
        );
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn deepseek_model_on_openai_provider_still_replays_reasoning_content() {
        // #1739 / #1694: a DeepSeek thinking model pointed at a
        // DeepSeek-compatible endpoint via the generic `openai` provider must
        // still replay reasoning_content, even though the provider itself does
        // not accept the field. Otherwise the thinking-mode API returns 400.
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Openai,
            "deepseek-v4-flash",
            None,
        ));
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Openai,
            "deepseek-v4-pro",
            None,
        ));
        assert!(should_replay_reasoning_content_for_provider(
            ProviderKind::Openai,
            "deepseek-reasoner",
            Some("medium"),
        ));
        // The documented escape hatch still wins over model detection.
        assert!(!should_replay_reasoning_content_for_provider(
            ProviderKind::Openai,
            "deepseek-v4-flash",
            Some("off"),
        ));
    }

    #[test]
    fn generic_model_on_openai_provider_still_strips_reasoning_content() {
        // #1542 no-regression guard: a genuine non-DeepSeek model on the
        // openai provider must continue to have reasoning_content stripped.
        assert!(!should_replay_reasoning_content_for_provider(
            ProviderKind::Openai,
            "qwen3-coder",
            None,
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            ProviderKind::Openai,
            "claude-sonnet-4-6",
            None,
        ));
    }

    #[test]
    fn suggestive_unknown_model_names_never_authorize_reasoning_replay() {
        for provider in [
            ProviderKind::Openai,
            ProviderKind::Deepseek,
            ProviderKind::Openrouter,
            ProviderKind::Moonshot,
            ProviderKind::Zai,
        ] {
            for model in [
                "foo-thinking",
                "foo-reasoner",
                "acme-reasoning",
                "future-reasoner-v9",
            ] {
                assert!(
                    !should_replay_reasoning_content_for_provider(provider, model, None),
                    "{provider:?} {model}"
                );
            }
        }
    }

    #[test]
    fn stream_classifies_deepseek_model_on_openai_provider_as_reasoning() {
        // #1739: the SSE parser must treat a DeepSeek thinking model on the
        // generic `openai` provider (DeepSeek-compatible endpoint) as a
        // reasoning model, or incoming `reasoning_content` tokens are stored
        // as answer text and the subsequent replay still 400s.
        assert!(is_reasoning_model_for_stream(
            ProviderKind::Openai,
            "deepseek-v4-flash"
        ));
        assert!(is_reasoning_model_for_stream(
            ProviderKind::Openai,
            "deepseek-v4-pro"
        ));
        assert!(is_reasoning_model_for_stream(
            ProviderKind::Openai,
            "deepseek-reasoner"
        ));
        // Native DeepSeek provider was already correct; stays correct.
        assert!(is_reasoning_model_for_stream(
            ProviderKind::Deepseek,
            "deepseek-v4-pro"
        ));
    }

    #[test]
    fn zai_tiered_effort_applies_to_glm_5_2_and_glm_5_3_but_not_5_1() {
        let zai = crate::config::DEFAULT_ZAI_BASE_URL;
        // GLM-5.3 and GLM-5.3-Flash inherit GLM-5.2's reasoning_options
        // (effort high/max), so they must take the same tiered wire path.
        for model in [
            crate::config::ZAI_GLM_5_2_MODEL,
            crate::config::ZAI_GLM_5_3_MODEL,
            crate::config::ZAI_GLM_5_3_FLASH_MODEL,
        ] {
            let mut body = json!({});
            apply_route_reasoning_controls(&mut body, ProviderKind::Zai, zai, model, Some("max"));
            assert_eq!(body["reasoning_effort"], json!("max"), "{model} at max");

            let mut body = json!({});
            apply_route_reasoning_controls(&mut body, ProviderKind::Zai, zai, model, Some("high"));
            assert_eq!(body["reasoning_effort"], json!("high"), "{model} at high");
        }

        // GLM-5.1 and GLM-5-Turbo keep only the generic thinking control.
        for model in [
            crate::config::ZAI_GLM_5_1_MODEL,
            crate::config::ZAI_GLM_5_TURBO_MODEL,
        ] {
            let mut body = json!({});
            apply_route_reasoning_controls(&mut body, ProviderKind::Zai, zai, model, Some("max"));
            assert!(
                body.get("reasoning_effort").is_none(),
                "{model} must not receive tiered effort"
            );
        }

        // A compatible gateway is not evidence of the Z.ai dialect, for 5.3
        // exactly as for 5.2.
        let mut body = json!({"thinking": {"type": "enabled"}});
        apply_route_reasoning_controls(
            &mut body,
            ProviderKind::Zai,
            "https://gateway.example.com/v1",
            crate::config::ZAI_GLM_5_3_MODEL,
            Some("max"),
        );
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn zai_forced_thinking_models_never_send_thinking_disabled() {
        // BigModel and Z.ai document GLM-5.3 / GLM-5.3-Flash as forced-thinking:
        // `thinking.type: "disabled"` errors, effort accepts only low/high/max,
        // and the migration note for a former `disabled` payload is
        // `enabled` + `reasoning_effort: "low"`. Both hosts of the first-party
        // open platform — api.z.ai and open.bigmodel.cn — get the rewrite.
        for zai in [
            crate::config::DEFAULT_ZAI_BASE_URL,
            "https://open.bigmodel.cn/api/paas/v4",
        ] {
            for model in [
                crate::config::ZAI_GLM_5_3_MODEL,
                crate::config::ZAI_GLM_5_3_FLASH_MODEL,
            ] {
                let mut body = json!({});
                apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::Zai,
                    zai,
                    model,
                    Some("off"),
                );
                assert_eq!(
                    body["thinking"]["type"],
                    json!("enabled"),
                    "{model} must not send the rejected disabled toggle"
                );
                assert_eq!(
                    body["reasoning_effort"],
                    json!("low"),
                    "{model} off becomes low"
                );

                let mut body = json!({});
                apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::Zai,
                    zai,
                    model,
                    Some("low"),
                );
                assert_eq!(
                    body["reasoning_effort"],
                    json!("low"),
                    "{model} low is native"
                );

                let mut body = json!({});
                apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::Zai,
                    zai,
                    model,
                    Some("medium"),
                );
                assert_eq!(
                    body["reasoning_effort"],
                    json!("high"),
                    "{model} medium maps to high"
                );

                let mut body = json!({});
                apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::Zai,
                    zai,
                    model,
                    Some("max"),
                );
                assert_eq!(
                    body["reasoning_effort"],
                    json!("max"),
                    "{model} max stays max"
                );

                // Unknown legacy values leave the field omitted so the API keeps
                // its documented default; nothing may reintroduce `disabled`.
                let mut body = json!({});
                apply_route_reasoning_controls(
                    &mut body,
                    ProviderKind::Zai,
                    zai,
                    model,
                    Some("auto"),
                );
                assert!(
                    body.get("reasoning_effort").is_none(),
                    "{model} auto stays omitted"
                );
                assert_ne!(body["thinking"]["type"], json!("disabled"));
            }

            // GLM-5.2 honours the generic disabled toggle on both hosts. On
            // BigModel that toggle now reaches the API (its docs still list
            // `disabled` for GLM-5.2) instead of being stripped by the
            // fail-closed gateway path.
            let mut body = json!({});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::Zai,
                zai,
                crate::config::ZAI_GLM_5_2_MODEL,
                Some("off"),
            );
            assert_eq!(body["thinking"]["type"], json!("disabled"));
            assert!(body.get("reasoning_effort").is_none());
        }
    }

    #[test]
    fn zai_bigmodel_adjacent_routes_stay_fail_closed() {
        // BigModel's `/preview` product and a plain-http neighbor are not the
        // documented Chat dialect, so they keep the gateway treatment: no
        // Z.ai reasoning fields, including on a forced-thinking model.
        for neighboring_route in [
            "https://open.bigmodel.cn/api/paas/v4/preview",
            "http://open.bigmodel.cn/api/paas/v4",
        ] {
            let mut body = json!({"thinking": {"type": "enabled"}});
            apply_route_reasoning_controls(
                &mut body,
                ProviderKind::Zai,
                neighboring_route,
                crate::config::ZAI_GLM_5_3_MODEL,
                Some("max"),
            );
            assert!(
                body.get("reasoning_effort").is_none(),
                "{neighboring_route} must not gain tiered effort"
            );
            assert!(
                body.get("thinking").is_none(),
                "{neighboring_route} must not keep the Z.ai thinking object"
            );
        }
    }

    #[test]
    fn stream_classifies_known_large_reasoning_models_as_reasoning() {
        // Xiaomi MiMo and OpenRouter/Qwen/Trinity can stream private reasoning through a
        // `reasoning` delta without using a DeepSeek-looking model name. The
        // renderer must still route that field into Thinking cells instead
        // of plain assistant prose.
        assert!(
            is_reasoning_model_for_stream(ProviderKind::XiaomiMimo, "mimo-v2.5-pro"),
            "mimo-v2.5-pro should stream reasoning as thinking on Xiaomi MiMo"
        );
        assert!(
            is_reasoning_model_for_stream(ProviderKind::Arcee, "trinity-large-thinking"),
            "trinity-large-thinking should stream reasoning as thinking on direct Arcee"
        );
        assert!(
            is_reasoning_model_for_stream(ProviderKind::Zai, "GLM-5.2"),
            "GLM-5.2 should stream reasoning_content as thinking on direct Z.ai"
        );
        assert!(
            is_reasoning_model_for_stream(ProviderKind::Zai, "GLM-5.3"),
            "GLM-5.3 inherits GLM-5.2's reasoning capability on direct Z.ai"
        );
        for model in [
            "arcee-ai/trinity-large-thinking",
            "minimax/minimax-m3",
            "xiaomi/mimo-v2.5-pro",
        ] {
            assert!(
                is_reasoning_model_for_stream(ProviderKind::Openrouter, model),
                "{model} should stream reasoning as thinking on OpenRouter"
            );
        }
    }

    #[test]
    fn stream_surfaces_reasoning_fields_on_every_unconfigured_route() {
        // #6501: grok-4.7 on xAI and a MiMo id newer than the offline catalog
        // streamed their reasoning as answer prose because the stream gate
        // required a provider allowlist AND a catalog row. The reasoning
        // fields are reasoning on every route that sends them; only an
        // explicit `reasoning_stream_style = "none"` restores pass-through.
        for (provider, model) in [
            (ProviderKind::Xai, "grok-4.7"),
            (ProviderKind::Xai, "grok-4.6"),
            (ProviderKind::XiaomiMimo, "mimo-v2.7-pro-unreleased"),
            (ProviderKind::XiaomiMimo, "mimo-v2.6-pro"),
            (ProviderKind::Openai, "qwen3-coder"),
            (ProviderKind::Deepseek, "qwen3-coder"),
            (ProviderKind::Stepfun, "step-3.5"),
            (ProviderKind::Together, "any/model"),
            (ProviderKind::Ollama, "gpt-oss:20b"),
            (ProviderKind::Codewhale, "grok-4.7"),
        ] {
            assert!(
                is_reasoning_model_for_stream(provider, model),
                "{provider:?} {model} must surface reasoning fields as Thinking"
            );
        }
        assert_eq!(
            super::reasoning_stream_style_for_stream(ProviderKind::Xai, "grok-4.7", Some("none")),
            ReasoningStreamStyle::None,
            "an explicit route override still wins"
        );
    }

    #[test]
    fn xiaomi_mimo_replays_reasoning_for_every_model_id() {
        // #6501: MiMo returns 400 when a tool-call turn omits the assistant
        // `reasoning_content`, including ids newer than the offline catalog.
        for model in ["mimo-v2.5-pro", "mimo-v2.6-pro", "mimo-v2.7-pro-unreleased"] {
            assert!(
                should_replay_reasoning_content_for_provider(ProviderKind::XiaomiMimo, model, None),
                "{model}"
            );
        }
        assert!(!should_replay_reasoning_content_for_provider(
            ProviderKind::XiaomiMimo,
            "mimo-v2.6-pro",
            Some("off"),
        ));
    }

    #[test]
    fn stream_display_does_not_authorize_reasoning_replay() {
        // #1542 stays fixed: rendering a reasoning delta as Thinking must not
        // make a provider that rejects `reasoning_content` receive it back.
        for (provider, model) in [
            (ProviderKind::Openai, "qwen3-coder"),
            (ProviderKind::Openai, "claude-sonnet-4-6"),
            (ProviderKind::Xai, "grok-4.7"),
        ] {
            assert!(is_reasoning_model_for_stream(provider, model));
            assert!(
                !should_replay_reasoning_content_for_provider(provider, model, None),
                "{provider:?} {model}"
            );
        }
    }
}

#[cfg(test)]
mod image_block_wire_tests {
    //! The OpenAI-compatible projection of [`ContentBlock::ImageUrl`].
    //!
    //! Chat Completions is the wire format behind the large majority of
    //! CodeWhale's provider routes, so a regression here is a regression for
    //! most of them at once. The shape is fixed by OpenAI's spec: a `user`
    //! message whose `content` is an array of parts, with the image as
    //! `{"type":"image_url","image_url":{"url":…}}`.
    use super::{ProviderKind, build_chat_messages, build_chat_wire_body};
    use codewhale_models::Role;
    use codewhale_models::{ContentBlock, ImageUrlContent, Message, MessageRequest};

    const DATA_URL: &str = "data:image/png;base64,QUJD";

    #[test]
    fn compaction_checkpoint_keeps_complete_tool_round_on_wire() {
        let mut messages: Vec<Message> = serde_json::from_value(serde_json::json!([
            {"role":"user","content":[{"type":"text","text":"Analyze the data"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"read","input":{"path":"a.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"ready"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"call_2","name":"read","input":{"path":"b.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_2","content":"done"}]}
        ])).unwrap();
        let summary = codewhale_models::SystemPrompt::Text(
            crate::compaction::build_compaction_summary_block_text("Compacted summary", ""),
        );
        messages.push(crate::compaction::compaction_checkpoint_message(&summary));
        crate::runtime_handoff::replace_agent_topology_checkpoint(&mut messages, &[]);
        let stored = messages.clone();
        let wire = build_chat_messages(None, &messages, "gpt-4o");
        assert_eq!(
            messages, stored,
            "request construction must not alter saved history"
        );
        let roles: Vec<&str> = wire
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "tool", "assistant", "tool"]);
        let prompt = wire[0]["content"].as_str().unwrap();
        assert!(prompt.contains("Compacted summary"));
        assert!(prompt.contains("Analyze the data"));
        assert!(prompt.contains("codewhale.agent_topology.v1"));
        assert_eq!(wire[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(wire[2]["tool_call_id"], "call_1");
        assert_eq!(wire[3]["tool_calls"][0]["id"], "call_2");
        assert_eq!(wire[4]["tool_call_id"], "call_2");

        messages.push(
            serde_json::from_value(serde_json::json!({
                "role":"assistant","content":[{"type":"text","text":"Analysis complete"}]
            }))
            .unwrap(),
        );
        messages.push(
            serde_json::from_value(serde_json::json!({
                "role":"user","content":[{"type":"text","text":"What happened next?"}]
            }))
            .unwrap(),
        );
        let later_wire = build_chat_messages(None, &messages, "gpt-4o");
        let later_roles: Vec<&str> = later_wire
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            later_roles,
            [
                "user",
                "assistant",
                "tool",
                "assistant",
                "tool",
                "assistant",
                "user"
            ]
        );
        assert_eq!(later_wire[6]["content"], "What happened next?");

        let restored = crate::compaction::restore_compaction_checkpoint(
            crate::runtime_handoff::project_owned_messages_for_restore(messages.clone()),
            Some(&summary),
        );
        let restored_wire = build_chat_messages(None, &restored, "gpt-4o");
        let restored_roles: Vec<&str> = restored_wire
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(restored_roles, later_roles);
        assert!(
            restored_wire[0]["content"]
                .as_str()
                .unwrap()
                .contains("restored Agent topology checkpoint")
        );
        assert_eq!(restored_wire[6]["content"], "What happened next?");
    }

    #[test]
    fn quoted_compaction_marker_does_not_reorder_user_wire_messages() {
        // The current marker, and the legacy one older sessions still carry.
        for marker in [
            crate::compaction::COMPACTION_SUMMARY_MARKER,
            crate::compaction::LEGACY_V2_COMPACTION_SUMMARY_MARKER,
        ] {
            let quote = format!("Please explain: {marker}");
            let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
                {"role":"user","content":[{"type":"text","text":"First question"}]},
                {"role":"assistant","content":[{"type":"text","text":"First answer"}]},
                {"role":"user","content":[{"type":"text","text":quote}]},
                {"role":"assistant","content":[{"type":"text","text":"It introduces a summary."}]},
                {"role":"user","content":[{"type":"text","text":"Follow-up question"}]}
            ]))
            .unwrap();
            let wire = build_chat_messages(None, &messages, "gpt-4o");
            let roles: Vec<&str> = wire
                .iter()
                .map(|message| message["role"].as_str().unwrap())
                .collect();
            assert_eq!(roles, ["user", "assistant", "user", "assistant", "user"]);
            assert_eq!(wire[0]["content"], "First question");
            assert_eq!(wire[2]["content"], quote);
            assert_eq!(wire[4]["content"], "Follow-up question");
        }
        let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
            {"role":"user","content":[{"type":"text","text":"First question"}]},
            {"role":"assistant","content":[{"type":"text","text":"First answer"}]},
            {"role":"user","content":[{"type":"text","text":"placeholder"}]},
            {"role":"assistant","content":[{"type":"text","text":"It introduces a summary."}]},
            {"role":"user","content":[{"type":"text","text":"Follow-up question"}]}
        ]))
        .unwrap();
        let roles = ["user", "assistant", "user", "assistant", "user"];

        let quoted_exact_header = crate::compaction::build_compaction_summary_block_text(
            "This text was pasted by a user",
            "",
        );
        let mut with_exact_quote = messages.clone();
        with_exact_quote[2] = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: quoted_exact_header.clone(),
                cache_control: None,
            }],
        };
        let exact_wire = build_chat_messages(None, &with_exact_quote, "gpt-4o");
        let exact_roles: Vec<&str> = exact_wire
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(exact_roles, roles);
        assert_eq!(exact_wire[2]["content"], quoted_exact_header);

        let mut after_tool: Vec<Message> = serde_json::from_value(serde_json::json!([
            {"role":"user","content":[{"type":"text","text":"Read first"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"read","input":{"path":"a.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"contents"}]},
            {"role":"user","content":[{"type":"text","text":"placeholder"}]},
            {"role":"assistant","content":[{"type":"text","text":"Answer"}]}
        ])).unwrap();
        after_tool[3] = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: quoted_exact_header.clone(),
                cache_control: None,
            }],
        };
        let after_tool_wire = build_chat_messages(None, &after_tool, "gpt-4o");
        let after_tool_roles: Vec<&str> = after_tool_wire
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            after_tool_roles,
            ["user", "assistant", "tool", "user", "assistant"]
        );
        assert_eq!(after_tool_wire[3]["content"], quoted_exact_header);
    }

    #[test]
    fn topology_checkpoint_after_tool_result_keeps_wire_tool_pair() {
        let mut messages: Vec<Message> = serde_json::from_value(serde_json::json!([
            {"role":"user","content":[{"type":"text","text":"Read the file"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"read","input":{"path":"a.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"contents"}]}
        ])).unwrap();
        crate::runtime_handoff::replace_agent_topology_checkpoint(&mut messages, &[]);
        let wire = build_chat_messages(None, &messages, "gpt-4o");
        let roles: Vec<&str> = wire
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "tool"]);
        assert!(
            wire[0]["content"]
                .as_str()
                .unwrap()
                .contains("agent_topology_v1")
        );
        assert_eq!(wire[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(wire[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn compaction_without_retained_user_has_one_wire_user_before_tools() {
        let mut messages: Vec<Message> = serde_json::from_value(serde_json::json!([
            {"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"read","input":{"path":"a.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"contents"}]}
        ])).unwrap();
        let summary = codewhale_models::SystemPrompt::Text(
            crate::compaction::build_compaction_summary_block_text("Summary", ""),
        );
        messages.push(crate::compaction::compaction_checkpoint_message(&summary));
        crate::runtime_handoff::replace_agent_topology_checkpoint(&mut messages, &[]);
        let wire = build_chat_messages(None, &messages, "gpt-4o");
        let roles: Vec<&str> = wire
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "tool"]);
        let prompt = wire[0]["content"].as_str().unwrap();
        assert!(prompt.contains("Summary"));
        assert!(prompt.contains("agent_topology_v1"));
        assert_eq!(wire[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn compaction_after_unanswered_user_prompt_has_one_wire_user() {
        let mut messages: Vec<Message> = serde_json::from_value(serde_json::json!([
            {"role":"user","content":[{"type":"text","text":"Please continue"}]}
        ]))
        .unwrap();
        let summary = codewhale_models::SystemPrompt::Text(
            crate::compaction::build_compaction_summary_block_text("Earlier work", ""),
        );
        messages.push(crate::compaction::compaction_checkpoint_message(&summary));
        crate::runtime_handoff::replace_agent_topology_checkpoint(&mut messages, &[]);
        let wire = build_chat_messages(None, &messages, "gpt-4o");
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["role"], "user");
        let prompt = wire[0]["content"].as_str().unwrap();
        assert!(prompt.contains("Please continue"));
        assert!(prompt.contains("Earlier work"));
        assert!(prompt.contains("agent_topology_v1"));
    }

    fn fixture_tool_use(id: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            execution_id: None,
            id: id.to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": format!("{id}.txt")}),
            caller: None,
            thought_signature: None,
        }
    }

    fn fixture_tool_result(id: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: id.to_string(),
                content: format!("result for {id}"),
                is_error: Some(false),
                content_blocks: None,
            }],
        }
    }

    fn request_with_image() -> MessageRequest {
        MessageRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "what is in this screenshot?".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ImageUrl {
                        image_url: ImageUrlContent {
                            url: DATA_URL.to_string(),
                        },
                    },
                ],
            }],
            max_tokens: 128,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    #[test]
    fn user_image_becomes_a_multimodal_parts_array() {
        let body = build_chat_wire_body(
            &request_with_image(),
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            false,
            None,
        )
        .expect("wire body");

        let messages = body.body["messages"].as_array().expect("messages");
        let user = messages
            .iter()
            .find(|message| message["role"] == "user")
            .expect("a user message");
        let parts = user["content"]
            .as_array()
            .expect("content must be a parts array once an image is present, not a bare string");

        let image = parts
            .iter()
            .find(|part| part["type"] == "image_url")
            .expect("an image_url part");
        assert_eq!(image["image_url"]["url"], DATA_URL);

        let text = parts
            .iter()
            .find(|part| part["type"] == "text")
            .expect("the accompanying text part");
        assert!(
            text["text"]
                .as_str()
                .expect("text")
                .contains("what is in this screenshot?"),
            "the question must survive alongside the image: {user}"
        );
    }

    #[test]
    fn deepseek_vision_exp_uses_chat_image_url_request_shape() {
        let mut request = request_with_image();
        request.model = "deepseek-v4-flash-vision-exp".to_string();

        let body = build_chat_wire_body(
            &request,
            ProviderKind::Deepseek,
            "https://api.deepseek.com/beta",
            false,
            None,
        )
        .expect("DeepSeek vision wire body");

        assert_eq!(body.body["model"], "deepseek-v4-flash-vision-exp");
        let messages = body.body["messages"].as_array().expect("messages");
        let user = messages
            .iter()
            .find(|message| message["role"] == "user")
            .expect("a user message");
        let parts = user["content"]
            .as_array()
            .expect("DeepSeek vision content must use multimodal parts");

        assert!(parts.iter().any(|part| {
            part["type"] == "text" && part["text"] == "what is in this screenshot?"
        }));
        assert!(
            parts.iter().any(|part| {
                part["type"] == "image_url" && part["image_url"]["url"] == DATA_URL
            })
        );
    }

    #[test]
    fn a_message_with_no_image_keeps_its_plain_string_content() {
        // Promoting every user turn to a parts array would change the request
        // bytes for every text-only route, and with them the prompt-cache
        // prefix. Images must be the only thing that triggers the array form.
        let mut request = request_with_image();
        request.messages[0]
            .content
            .retain(|block| !matches!(block, ContentBlock::ImageUrl { .. }));

        let body = build_chat_wire_body(
            &request,
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            false,
            None,
        )
        .expect("wire body");

        let messages = body.body["messages"].as_array().expect("messages");
        let user = messages
            .iter()
            .find(|message| message["role"] == "user")
            .expect("a user message");
        assert!(
            user["content"].is_string(),
            "text-only turns must stay a plain string: {user}"
        );
    }

    #[test]
    fn tool_result_image_follows_its_tool_message_as_multimodal_user_content() {
        let mut request = request_with_image();
        request.messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call_image_1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"path": "shot.png"}),
                    caller: None,
                    thought_signature: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_image_1".to_string(),
                    content: "screenshot captured".to_string(),
                    is_error: Some(false),
                    content_blocks: Some(vec![serde_json::json!({
                        "type": "image",
                        "mime_type": "image/png",
                        "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
                    })]),
                }],
            },
        ];

        let body = build_chat_wire_body(
            &request,
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            false,
            None,
        )
        .expect("wire body");
        let messages = body.body["messages"].as_array().expect("messages");
        let tool_index = messages
            .iter()
            .position(|message| message["role"] == "tool")
            .expect("tool result");
        assert_eq!(messages[tool_index]["tool_call_id"], "call_image_1");
        assert_eq!(messages[tool_index]["content"], "screenshot captured");

        let image_message = &messages[tool_index + 1];
        assert_eq!(image_message["role"], "user");
        let parts = image_message["content"].as_array().expect("image parts");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        );
        assert!(
            parts[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("read") && text.contains("call_image_1"))
        );
    }

    #[test]
    fn tool_result_images_follow_the_entire_tool_call_batch() {
        let mut request = request_with_image();
        request.messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call_image_1".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({"path": "first.png"}),
                        caller: None,
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call_image_2".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({"path": "second.png"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_image_1".to_string(),
                    content: "first screenshot captured".to_string(),
                    is_error: Some(false),
                    content_blocks: Some(vec![serde_json::json!({
                        "type": "image",
                        "mime_type": "image/png",
                        "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
                    })]),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_image_2".to_string(),
                    content: "second screenshot captured".to_string(),
                    is_error: Some(false),
                    content_blocks: Some(vec![serde_json::json!({
                        "type": "image",
                        "mime_type": "image/png",
                        "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYPj/HwADAgH/5ncLrgAAAABJRU5ErkJggg==",
                    })]),
                }],
            },
        ];

        let body = build_chat_wire_body(
            &request,
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            false,
            None,
        )
        .expect("wire body");
        let messages = body.body["messages"].as_array().expect("messages");
        let roles: Vec<_> = messages
            .iter()
            .map(|message| message["role"].as_str().expect("role"))
            .collect();
        assert_eq!(roles, ["assistant", "tool", "tool", "user"]);
        assert_eq!(messages[1]["tool_call_id"], "call_image_1");
        assert_eq!(messages[2]["tool_call_id"], "call_image_2");

        let image_parts = messages[3]["content"].as_array().expect("image parts");
        assert_eq!(image_parts.len(), 4);
        assert_eq!(
            image_parts[1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        );
        assert_eq!(
            image_parts[3]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYPj/HwADAgH/5ncLrgAAAABJRU5ErkJggg=="
        );
    }

    #[test]
    fn out_of_order_tool_results_remain_a_contiguous_complete_batch() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![fixture_tool_use("call_one"), fixture_tool_use("call_two")],
            },
            fixture_tool_result("call_two"),
            fixture_tool_result("call_one"),
        ];

        let wire = build_chat_messages(None, &messages, "gpt-4o");
        let roles: Vec<_> = wire
            .iter()
            .map(|message| message["role"].as_str().expect("role"))
            .collect();
        assert_eq!(roles, ["assistant", "tool", "tool"]);
        assert_eq!(wire[1]["tool_call_id"], "call_two");
        assert_eq!(wire[2]["tool_call_id"], "call_one");
    }

    #[test]
    fn incomplete_tool_result_batch_is_downgraded_before_serialization() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![fixture_tool_use("call_one"), fixture_tool_use("call_two")],
            },
            fixture_tool_result("call_one"),
        ];

        let wire = build_chat_messages(None, &messages, "gpt-4o");
        assert!(
            !wire
                .iter()
                .any(|message| message.get("tool_calls").is_some()),
            "an incomplete tool batch must not reach the provider: {wire:?}"
        );
        assert!(
            !wire
                .iter()
                .any(|message| message["role"].as_str() == Some("tool")),
            "the partial result must be removed with its incomplete call batch: {wire:?}"
        );
    }

    #[test]
    fn interleaved_tool_result_batch_is_downgraded_before_serialization() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call_one".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({"path": "first.png"}),
                        caller: None,
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call_two".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({"path": "second.png"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_one".to_string(),
                    content: "first screenshot captured".to_string(),
                    is_error: Some(false),
                    content_blocks: Some(vec![serde_json::json!({
                        "type": "image",
                        "mime_type": "image/png",
                        "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
                    })]),
                }],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "interloper".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolResult {
                        execution_id: None,
                        tool_use_id: "call_two".to_string(),
                        content: "second screenshot captured".to_string(),
                        is_error: Some(false),
                        content_blocks: Some(vec![serde_json::json!({
                            "type": "image",
                            "mime_type": "image/png",
                            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
                        })]),
                    },
                ],
            },
        ];

        let wire = build_chat_messages(None, &messages, "gpt-4o");
        assert!(
            !wire
                .iter()
                .any(|message| message.get("tool_calls").is_some()),
            "a non-contiguous tool batch must not reach the provider: {wire:?}"
        );
        assert!(
            !wire
                .iter()
                .any(|message| message["role"].as_str() == Some("tool")),
            "orphaned tool replies must be removed with their stripped call batch: {wire:?}"
        );
        assert!(
            !wire.iter().any(|message| {
                message["content"].as_array().is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part["type"].as_str() == Some("image_url"))
                })
            }),
            "images from a stripped tool batch must not survive as user input: {wire:?}"
        );
    }

    #[test]
    fn duplicate_tool_call_ids_are_downgraded_before_serialization() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "duplicate".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({"path": "first.png"}),
                        caller: None,
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "duplicate".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({"path": "second.png"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "duplicate".to_string(),
                    content: "one result for two calls".to_string(),
                    is_error: Some(false),
                    content_blocks: Some(vec![serde_json::json!({
                        "type": "image",
                        "mime_type": "image/png",
                        "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
                    })]),
                }],
            },
        ];

        let wire = build_chat_messages(None, &messages, "gpt-4o");
        assert!(
            !wire
                .iter()
                .any(|message| message.get("tool_calls").is_some()),
            "duplicate call IDs cannot satisfy two tool calls: {wire:?}"
        );
        assert!(
            !wire
                .iter()
                .any(|message| message["role"].as_str() == Some("tool")),
            "the ambiguous tool result must be removed with the stripped batch: {wire:?}"
        );
        assert!(
            !wire.iter().any(|message| {
                message["content"].as_array().is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part["type"].as_str() == Some("image_url"))
                })
            }),
            "media from an ambiguous duplicate-ID batch must not survive: {wire:?}"
        );
    }
}

#[cfg(test)]
mod mistral_reasoning_tests {
    use super::*;

    fn request_with_assistant_thinking_and_tool() -> MessageRequest {
        MessageRequest {
            model: "mistral-medium-latest".to_string(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentBlock::Thinking {
                            thinking: "Inspect the current state before calling the tool."
                                .to_string(),
                            signature: None,
                            state: None,
                        },
                        ContentBlock::Text {
                            text: "I will inspect it now.".to_string(),
                            cache_control: None,
                        },
                        ContentBlock::ToolUse {
                            execution_id: None,
                            id: "call-1".to_string(),
                            name: "read_file".to_string(),
                            input: json!({"path": "README.md"}),
                            caller: None,
                            thought_signature: None,
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        execution_id: None,
                        tool_use_id: "call-1".to_string(),
                        content: "contents".to_string(),
                        is_error: None,
                        content_blocks: None,
                    }],
                },
            ],
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("high".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    #[test]
    fn mistral_effort_wire_value_covers_codewhale_tiers() {
        assert_eq!(mistral_reasoning_effort_wire_value("off"), Some("none"));
        assert_eq!(
            mistral_reasoning_effort_wire_value("disabled"),
            Some("none")
        );
        assert_eq!(mistral_reasoning_effort_wire_value("none"), Some("none"));
        assert_eq!(mistral_reasoning_effort_wire_value("false"), Some("none"));
        assert_eq!(mistral_reasoning_effort_wire_value("high"), Some("high"));
        assert_eq!(mistral_reasoning_effort_wire_value("xhigh"), Some("high"));
        assert_eq!(mistral_reasoning_effort_wire_value("max"), Some("high"));
        assert_eq!(mistral_reasoning_effort_wire_value("ultra"), Some("high"));
        assert_eq!(
            mistral_reasoning_effort_wire_value("ultracode"),
            Some("high")
        );
        // Intermediate tiers must be omitted so the request falls back to
        // Mistral's own default rather than 400 code 3051 on unsupported
        // values like "low"/"medium" that the server does not accept today.
        assert_eq!(mistral_reasoning_effort_wire_value("low"), None);
        assert_eq!(mistral_reasoning_effort_wire_value("medium"), None);
        assert_eq!(mistral_reasoning_effort_wire_value("mid"), None);
        assert_eq!(mistral_reasoning_effort_wire_value("minimal"), None);
    }

    #[test]
    fn mistral_model_gate_only_matches_reasoning_capable_families() {
        assert!(mistral_model_supports_reasoning("mistral-medium-latest"));
        assert!(mistral_model_supports_reasoning("mistral-medium-3-5"));
        assert!(mistral_model_supports_reasoning("mistral-small-latest"));
        assert!(mistral_model_supports_reasoning("mistral-small-2603"));
        assert!(mistral_model_supports_reasoning("magistral-small-latest"));
        assert!(mistral_model_supports_reasoning("MISTRAL-MEDIUM-LATEST"));
        assert!(!mistral_model_supports_reasoning("mistral-code-latest"));
        assert!(!mistral_model_supports_reasoning("codestral-latest"));
        assert!(!mistral_model_supports_reasoning("mistral-large-latest"));
        assert!(!mistral_model_supports_reasoning("mistral-nemo-2407"));
    }

    #[test]
    fn mistral_route_shaper_writes_reasoning_only_for_supported_models() {
        let mut body = json!({"model": "mistral-medium-latest"});
        apply_mistral_route_reasoning_controls(
            &mut body,
            ProviderKind::Mistral,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
            "mistral-medium-latest",
            Some("high"),
        );
        assert_eq!(body["reasoning_effort"], json!("high"));

        let mut body = json!({"model": "mistral-code-latest"});
        apply_mistral_route_reasoning_controls(
            &mut body,
            ProviderKind::Mistral,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
            "mistral-code-latest",
            Some("high"),
        );
        assert!(
            body.get("reasoning_effort").is_none(),
            "non-reasoning models must never see reasoning_effort (Mistral 400s on 3051): {body}"
        );

        let mut body = json!({"model": "mistral-medium-latest", "reasoning_effort": "stale"});
        apply_mistral_route_reasoning_controls(
            &mut body,
            ProviderKind::Mistral,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
            "mistral-medium-latest",
            Some("low"),
        );
        assert!(
            body.get("reasoning_effort").is_none(),
            "intermediate tiers must be stripped rather than sent unsupported: {body}"
        );

        // Non-Mistral providers must not be touched by this shaper.
        let mut body = json!({"model": "deepseek-v4-pro", "reasoning_effort": "high"});
        apply_mistral_route_reasoning_controls(
            &mut body,
            ProviderKind::Deepseek,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
            "deepseek-v4-pro",
            Some("high"),
        );
        assert_eq!(body["reasoning_effort"], json!("high"));

        let mut body = json!({
            "model": "mistral-medium-latest",
            "thinking": {"type": "enabled"},
            "reasoning_effort": "stale",
        });
        apply_mistral_route_reasoning_controls(
            &mut body,
            ProviderKind::Mistral,
            "https://gateway.example.test/v1",
            "mistral-medium-latest",
            Some("high"),
        );
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());

        let mut native = json!({"model": "magistral-small-latest"});
        apply_mistral_route_reasoning_controls(
            &mut native,
            ProviderKind::Mistral,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
            "magistral-small-latest",
            Some("off"),
        );
        assert!(
            native.get("reasoning_effort").is_none(),
            "legacy native Magistral is always-reasoning and does not use the adjustable effort field"
        );
    }

    #[test]
    fn mistral_wire_dialect_is_limited_to_exact_first_party_routes() {
        for official in [
            "https://api.mistral.ai/v1",
            "https://api.eu.mistral.ai/v1/",
            "https://api.us.mistral.ai/v1",
        ] {
            assert!(is_exact_mistral_chat_route(ProviderKind::Mistral, official));
        }
        for neighbor in [
            "http://api.mistral.ai/v1",
            "https://api.mistral.ai/v2",
            "https://proxy.example.test/v1",
            "https://api.mistral.ai.evil.test/v1",
        ] {
            assert!(!is_exact_mistral_chat_route(
                ProviderKind::Mistral,
                neighbor
            ));
        }
        assert!(!is_exact_mistral_chat_route(
            ProviderKind::Openai,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
        ));
        assert_eq!(
            reasoning_stream_style_for_route(
                ProviderKind::Mistral,
                crate::config::DEFAULT_MISTRAL_BASE_URL,
                "mistral-medium-latest",
                None,
            ),
            ReasoningStreamStyle::MistralBlocks
        );
        assert_eq!(
            reasoning_stream_style_for_route(
                ProviderKind::Mistral,
                "https://gateway.example.test/v1",
                "mistral-medium-latest",
                None,
            ),
            ReasoningStreamStyle::SeparateField
        );
    }

    #[test]
    fn extract_mistral_polymorphic_content_flattens_thinking_and_text() {
        // Non-reasoning response: plain string content. Extractor returns
        // (None, None) so the shared string fallback still runs.
        let plain = json!({"content": "Hello world"});
        assert_eq!(extract_mistral_polymorphic_content(&plain), (None, None));

        // Missing content: no panic, returns (None, None).
        let empty = json!({});
        assert_eq!(extract_mistral_polymorphic_content(&empty), (None, None));

        // Reasoning response: nested thinking array + text block.
        let reasoning = json!({"content": [
            {"type": "thinking", "thinking": [
                {"type": "text", "text": "First "},
                {"type": "text", "text": "second."},
            ], "closed": true},
            {"type": "text", "text": "Final answer."},
        ]});
        let (thinking, text) = extract_mistral_polymorphic_content(&reasoning);
        assert_eq!(thinking.as_deref(), Some("First second."));
        assert_eq!(text.as_deref(), Some("Final answer."));

        // Thinking-only chunk (mid-stream) with no closing text yet.
        let thinking_only = json!({"content": [
            {"type": "thinking", "thinking": [{"type": "text", "text": "still thinking"}]},
        ]});
        let (thinking, text) = extract_mistral_polymorphic_content(&thinking_only);
        assert_eq!(thinking.as_deref(), Some("still thinking"));
        assert_eq!(text, None);
    }

    #[test]
    fn reshape_mistral_messages_reconstructs_polymorphic_shape_for_assistant_replay() {
        // Assistant message with stored reasoning_content is reshaped into
        // Mistral's polymorphic content-as-array shape.
        let mut messages = vec![
            json!({"role": "user", "content": "compute 3+4"}),
            json!({
                "role": "assistant",
                "content": "The answer is 7.",
                "reasoning_content": "Let me add 3 and 4 to get 7.",
            }),
            json!({"role": "user", "content": "now multiply by 2"}),
        ];
        reshape_mistral_messages_for_reasoning_replay(&mut messages);

        assert_eq!(messages[0]["role"], "user");
        assert!(
            messages[0]["content"].is_string(),
            "user turns are left untouched: {}",
            messages[0]
        );

        assert_eq!(messages[1]["role"], "assistant");
        assert!(
            messages[1].get("reasoning_content").is_none(),
            "reasoning_content field must be removed after reshape: {}",
            messages[1]
        );
        let content = messages[1]["content"]
            .as_array()
            .expect("assistant content is now an array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["closed"], true);
        assert_eq!(content[0]["thinking"][0]["type"], "text");
        assert_eq!(
            content[0]["thinking"][0]["text"],
            "Let me add 3 and 4 to get 7."
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "The answer is 7.");

        // Assistant with no reasoning stays as-is (plain string content).
        let mut plain = vec![json!({"role": "assistant", "content": "hi"})];
        reshape_mistral_messages_for_reasoning_replay(&mut plain);
        assert_eq!(plain[0]["content"], "hi");

        // Empty reasoning is treated as absent — no reshape.
        let mut empty = vec![json!({
            "role": "assistant",
            "content": "hi",
            "reasoning_content": "   ",
        })];
        reshape_mistral_messages_for_reasoning_replay(&mut empty);
        assert_eq!(empty[0]["content"], "hi");
    }

    #[test]
    fn mistral_prompt_builder_replays_stored_thinking_as_polymorphic_content() {
        let request = request_with_assistant_thinking_and_tool();
        let exact = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::Mistral,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
        );
        let assistant = &exact[0];
        assert!(assistant.get("reasoning_content").is_none());
        assert!(assistant.get("tool_calls").is_some());
        let content = assistant["content"]
            .as_array()
            .expect("exact Mistral history uses polymorphic content");
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(
            content[0]["thinking"][0]["text"],
            "Inspect the current state before calling the tool."
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "I will inspect it now.");

        for (provider, base_url) in [
            (ProviderKind::Mistral, "https://gateway.example.test/v1"),
            (
                ProviderKind::Openai,
                crate::config::DEFAULT_MISTRAL_BASE_URL,
            ),
        ] {
            let neighbor = build_chat_messages_for_request_and_provider_and_route(
                &request, provider, base_url,
            );
            assert!(neighbor[0].get("reasoning_content").is_none());
            assert!(
                neighbor[0]["content"].is_string(),
                "unproven routes must not inherit Mistral's polymorphic dialect: {}",
                neighbor[0]
            );
        }
    }

    #[test]
    fn mistral_stream_tool_call_replay_does_not_gain_reasoning_content() {
        let request = request_with_assistant_thinking_and_tool();
        let wire = build_chat_wire_body(
            &request,
            ProviderKind::Mistral,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
            true,
            None,
        )
        .expect("Mistral stream wire body");
        let assistant = &wire.body["messages"][0];
        assert!(assistant["content"].is_array());
        assert!(assistant.get("reasoning_content").is_none());
        assert!(assistant.get("tool_calls").is_some());
        assert_eq!(wire.body["reasoning_effort"], "high");
        assert_eq!(wire.replay_input_tokens, None);
    }

    #[test]
    fn mistral_nonstream_parser_is_route_isolated() {
        let payload = json!({
            "id": "chatcmpl-mistral",
            "model": "mistral-medium-latest",
            "choices": [{
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": [
                        {"type": "text", "text": "private trace"}
                    ], "closed": true},
                    {"type": "text", "text": "public answer"}
                ]}
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2}
        });
        let mistral = parse_chat_message_for_route(
            &payload,
            ProviderKind::Mistral,
            crate::config::DEFAULT_MISTRAL_BASE_URL,
        )
        .expect("Mistral payload parses");
        assert!(matches!(
            &mistral.content[0],
            ContentBlock::Thinking { thinking, .. } if thinking == "private trace"
        ));
        assert!(matches!(
            &mistral.content[1],
            ContentBlock::Text { text, .. } if text == "public answer"
        ));

        let generic = parse_chat_message(&payload).expect("generic payload parses");
        assert!(
            !generic
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Thinking { .. })),
            "typed arrays from another provider must not be reinterpreted as Mistral thinking"
        );
    }

    #[test]
    fn mistral_shared_capability_matches_wire_contract() {
        for model in [
            "mistral-medium-latest",
            "mistral-small-latest",
            "magistral-small-latest",
        ] {
            assert!(codewhale_models::model_supports_reasoning(model), "{model}");
        }
        for model in ["mistral-code-latest", "mistral-large-latest"] {
            assert!(
                !codewhale_models::model_supports_reasoning(model),
                "{model}"
            );
        }
    }
}

#[cfg(test)]
mod google_thought_signature_tests {
    use super::*;

    // ── Google thought signatures (#v0.9.8 Google backend) ──────────────
    use crate::config::{DEFAULT_GOOGLE_BASE_URL, DEFAULT_OPENAI_BASE_URL};

    /// One signed tool turn. `paired` false is the restart shape: the process
    /// died between the tool call and its result, so the terminal
    /// `tool_result` never reached durable history.
    fn signed_history(signature: Option<&str>, paired: bool) -> Vec<Message> {
        let mut messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Read the config.".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Reading now.".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call-g-1".to_string(),
                        name: "read".to_string(),
                        input: json!({"path": "config.toml"}),
                        caller: None,
                        thought_signature: signature.map(str::to_string),
                    },
                ],
            },
        ];
        if paired {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-g-1".to_string(),
                    content: "key = \"value\"".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            });
        }
        messages
    }

    fn request_from(messages: Vec<Message>) -> MessageRequest {
        MessageRequest {
            model: "gemini-3.1-pro-preview".to_string(),
            messages,
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("high".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    fn google_request_with_signed_tool(signature: Option<&str>) -> MessageRequest {
        request_from(signed_history(signature, true))
    }

    #[tokio::test]
    async fn gateway_thought_signature_rejection_explains_recovery_after_transport() {
        use crate::llm_client::LlmClient;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _ = rustls::crypto::ring::default_provider().install_default();
        // An unsigned replay must reach the gateway: it may manage Google's
        // signatures itself. Only an actual rejection warrants recovery advice.
        for streaming in [false, true] {
            for status in [200, 400] {
                let server = MockServer::start().await;
                let response = if status == 400 {
                    ResponseTemplate::new(status).set_body_json(json!({
                        "error": {
                            "code": 400,
                            "message": "Function call is missing a thought_signature in functionCall parts."
                        }
                    }))
                } else if streaming {
                    ResponseTemplate::new(status)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string("data: [DONE]\n\n")
                } else {
                    ResponseTemplate::new(status).set_body_json(json!({
                        "id": "gateway-replay",
                        "model": "gemini-3.1-pro-preview",
                        "choices": [{
                            "message": {"role": "assistant", "content": "Done."},
                            "finish_reason": "stop"
                        }]
                    }))
                };
                Mock::given(method("POST"))
                    .and(path("/v1/chat/completions"))
                    .respond_with(response)
                    .expect(1)
                    .mount(&server)
                    .await;

                let mut client = CodewhaleClient::new(&crate::config::Config {
                    provider: Some("openai".to_string()),
                    providers: Some(crate::config::ProvidersConfig {
                        openai: crate::config::ProviderConfig {
                            api_key: Some("gateway-test-key".to_string()),
                            base_url: Some(format!("{}/v1", server.uri())),
                            model: Some("gemini-3.1-pro-preview".to_string()),
                            ..crate::config::ProviderConfig::default()
                        },
                        ..crate::config::ProvidersConfig::default()
                    }),
                    ..crate::config::Config::default()
                })
                .expect("gateway client");
                client.isolated_request_state = true;
                let request = google_request_with_signed_tool(None);
                let result = if streaming {
                    client.create_message_stream(request).await.map(|_| ())
                } else {
                    client
                        .create_message_without_response_cache(request)
                        .await
                        .map(|_| ())
                };
                if status == 400 {
                    let error = result.expect_err("gateway rejects unsigned replay");
                    let message = error.to_string();
                    assert!(message.contains("built-in `google` provider"), "{message}");
                    assert!(message.contains("start a new session"), "{message}");
                    assert!(matches!(
                        error.downcast_ref::<crate::llm_client::LlmError>(),
                        Some(crate::llm_client::LlmError::InvalidRequest { status: 400, .. })
                    ));
                } else {
                    result.expect("gateway-managed signatures must still work");
                }
                server.verify().await;
            }
        }
    }

    #[test]
    fn google_route_round_trips_thought_signatures_on_replayed_tool_calls() {
        let request = google_request_with_signed_tool(Some("SIG-abc123"));
        let messages = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::Google,
            DEFAULT_GOOGLE_BASE_URL,
        );
        let assistant = messages
            .iter()
            .find(|m| m.get("role") == Some(&json!("assistant")))
            .expect("assistant replay message");
        let signature = assistant
            .pointer("/tool_calls/0/extra_content/google/thought_signature")
            .and_then(serde_json::Value::as_str);
        assert_eq!(signature, Some("SIG-abc123"));
    }

    #[test]
    fn google_route_fails_closed_when_replayed_signature_is_missing() {
        let request = google_request_with_signed_tool(None);
        let error = build_chat_wire_body(
            &request,
            ProviderKind::Google,
            DEFAULT_GOOGLE_BASE_URL,
            false,
            None,
        )
        .err()
        .expect("missing signature must fail closed before transport");
        assert!(
            error.to_string().contains("thought signature"),
            "error must name the missing signature: {error}"
        );
    }

    /// Google names the same model `gemini-3-pro` and `models/gemini-3-pro` on
    /// this endpoint. The prefixed spelling used to match none of the thinking
    /// families, so the model that most needs a signature was treated as one
    /// that needs none and the replay reached Google unsigned (#6018).
    #[test]
    fn google_route_fails_closed_for_a_models_prefixed_thinking_id() {
        let mut request = google_request_with_signed_tool(None);
        request.model = "models/gemini-3-pro-preview".to_string();
        let error = build_chat_wire_body(
            &request,
            ProviderKind::Google,
            DEFAULT_GOOGLE_BASE_URL,
            false,
            None,
        )
        .err()
        .expect("a models/-prefixed thinking id must fail closed like its bare spelling");
        assert!(
            error.to_string().contains("thought signature"),
            "error must name the missing signature: {error}"
        );
    }

    #[test]
    fn google_missing_signature_is_a_warning_not_an_error_for_flash_lite() {
        // 2.5 Flash-Lite ships thinking off; Google may legitimately omit
        // signatures there, so replay proceeds.
        let mut request = google_request_with_signed_tool(None);
        request.model = "gemini-2.5-flash-lite".to_string();
        build_chat_wire_body(
            &request,
            ProviderKind::Google,
            DEFAULT_GOOGLE_BASE_URL,
            false,
            None,
        )
        .expect("flash-lite replay must not require a signature");
    }

    #[test]
    fn non_google_routes_never_see_google_extra_content() {
        let request = google_request_with_signed_tool(Some("SIG-abc123"));
        let messages = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::Openai,
            DEFAULT_OPENAI_BASE_URL,
        );
        for message in &messages {
            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                for call in tool_calls {
                    assert!(
                        call.get("extra_content").is_none(),
                        "Google-only fields must not leak to other providers"
                    );
                }
            }
        }
    }

    #[test]
    fn google_neighbor_base_url_does_not_get_google_dialect() {
        // A Google provider row pointed at some other gateway must not
        // carry signatures or fail closed: the dialect binds to the exact
        // official route, not to provider identity alone.
        let request = google_request_with_signed_tool(None);
        build_chat_wire_body(
            &request,
            ProviderKind::Google,
            "https://gateway.example.com/v1",
            false,
            None,
        )
        .expect("non-official Google base URL must not require signatures");
        let messages = build_chat_messages_for_request_and_provider_and_route(
            &google_request_with_signed_tool(Some("SIG")),
            ProviderKind::Google,
            "https://gateway.example.com/v1",
        );
        assert!(
            messages
                .iter()
                .all(|m| m.pointer("/tool_calls/0/extra_content").is_none()),
            "signatures must not be sent to a non-Google endpoint"
        );
    }

    /// The manually configured OpenAI-compatible row (#1519,
    /// `ProviderKind::Custom`) pointed at Google's OpenAI-compat endpoint is
    /// byte-for-byte the same endpoint as the built-in `google` row. C22: it
    /// used to fail the `provider == Google` half of the route gate, so its
    /// signatures were stripped on replay with no warning and later signed
    /// tool turns failed. The gate now binds to the endpoint.
    #[test]
    fn manually_configured_openai_compatible_google_endpoint_replays_signatures() {
        let request = google_request_with_signed_tool(Some("SIG-abc123"));
        for base_url in [
            DEFAULT_GOOGLE_BASE_URL,
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "https://GenerativeLanguage.googleapis.com/v1beta/openai/",
        ] {
            let messages = build_chat_messages_for_request_and_provider_and_route(
                &request,
                ProviderKind::Custom,
                base_url,
            );
            let assistant = messages
                .iter()
                .find(|m| m.get("role") == Some(&json!("assistant")))
                .expect("assistant replay message");
            assert_eq!(
                assistant
                    .pointer("/tool_calls/0/extra_content/google/thought_signature")
                    .and_then(serde_json::Value::as_str),
                Some("SIG-abc123"),
                "custom row at Google's endpoint must replay the signature ({base_url})"
            );
        }
    }

    /// Signature preservation is a property of the endpoint, never of the
    /// reasoning setting: an operator who turns reasoning off (or whose
    /// effort is simply absent) still has to replay signed tool history.
    #[test]
    fn signatures_survive_replay_regardless_of_the_reasoning_setting() {
        for effort in [None, Some("off"), Some("low"), Some("high")] {
            for (provider, base_url) in [
                (ProviderKind::Google, DEFAULT_GOOGLE_BASE_URL),
                (
                    ProviderKind::Custom,
                    "https://generativelanguage.googleapis.com/v1beta/openai",
                ),
            ] {
                let mut request = google_request_with_signed_tool(Some("SIG-abc123"));
                request.reasoning_effort = effort.map(str::to_string);
                let body = build_chat_wire_body(&request, provider, base_url, true, None)
                    .expect("signed replay builds on a signature-bearing route");
                let assistant = body.body["messages"]
                    .as_array()
                    .expect("messages")
                    .iter()
                    .find(|m| m.get("role") == Some(&json!("assistant")))
                    .expect("assistant replay message");
                assert_eq!(
                    assistant
                        .pointer("/tool_calls/0/extra_content/google/thought_signature")
                        .and_then(serde_json::Value::as_str),
                    Some("SIG-abc123"),
                    "reasoning={effort:?} must not govern signature replay ({base_url})"
                );
            }
        }
    }

    /// Fail closed on the manually configured row too — the missing-signature
    /// error is the useful feedback that replaces a silent strip.
    #[test]
    fn custom_row_at_google_endpoint_fails_closed_without_a_signature() {
        let request = google_request_with_signed_tool(None);
        let error = build_chat_wire_body(
            &request,
            ProviderKind::Custom,
            "https://generativelanguage.googleapis.com/v1beta/openai",
            false,
            None,
        )
        .err()
        .expect("missing signature must fail closed before transport");
        let rendered = error.to_string();
        assert!(
            rendered.contains("thought signature") && rendered.contains("call-g-1"),
            "error must name the missing signature and the tool call: {rendered}"
        );
    }

    /// A custom row pointed somewhere else is still a foreign gateway: the
    /// signature is stripped, and the strip reports how much it removed so
    /// the drop is never silent.
    #[test]
    fn custom_row_off_google_endpoint_strips_and_reports_signatures() {
        let request = google_request_with_signed_tool(Some("SIG-abc123"));
        let mut messages = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::Google,
            DEFAULT_GOOGLE_BASE_URL,
        );
        assert_eq!(
            strip_google_tool_call_extra_content(&mut messages),
            1,
            "the strip must report the signatures it dropped"
        );
        assert!(
            messages
                .iter()
                .all(|m| m.pointer("/tool_calls/0/extra_content").is_none()),
            "stripped history must carry no Google-only fields"
        );
        let via_route = build_chat_messages_for_request_and_provider_and_route(
            &request,
            ProviderKind::Custom,
            "https://gateway.example.com/v1",
        );
        assert!(
            via_route
                .iter()
                .all(|m| m.pointer("/tool_calls/0/extra_content").is_none()),
            "signatures must not reach a non-Google endpoint"
        );
    }

    #[test]
    fn google_reasoning_uses_compatible_effort_without_rejected_native_fields() {
        // Wire examples and model limits from Google's compatibility docs.
        // Cover the actual request builder, both transport modes and both
        // ways a fresh install can configure the official endpoint (#6018).
        for (model, effort, expected) in [
            ("gemini-3.1-pro-preview", Some("low"), Some("low")),
            ("gemini-3.1-pro-preview", Some("medium"), Some("medium")),
            ("gemini-3.1-pro-preview", Some("high"), Some("high")),
            ("gemini-3.1-pro-preview", Some("max"), Some("high")),
            ("gemini-3.5-flash-lite", Some("off"), Some("minimal")),
            ("gemini-2.5-flash", Some("off"), Some("none")),
            ("models/gemini-2.5-flash-lite", Some("off"), Some("none")),
            ("gemini-2.5-pro", Some("off"), Some("minimal")),
            ("gemini-3.1-pro-preview", None, None),
        ] {
            for provider in [ProviderKind::Google, ProviderKind::Custom] {
                for streaming in [false, true] {
                    let mut request = google_request_with_signed_tool(Some("SIG"));
                    request.model = model.to_string();
                    request.reasoning_effort = effort.map(str::to_string);
                    let wire = build_chat_wire_body(
                        &request,
                        provider,
                        DEFAULT_GOOGLE_BASE_URL,
                        streaming,
                        None,
                    )
                    .expect("valid signed Google request");
                    assert_eq!(
                        wire.body.get("reasoning_effort").and_then(Value::as_str),
                        expected,
                        "{model}: {effort:?}, {provider:?}, streaming={streaming}"
                    );
                    assert!(wire.body.get("google").is_none());
                    assert!(wire.body.get("extra_body").is_none());
                    assert!(wire.body.get("thinking").is_none());
                }
            }
        }
    }

    #[test]
    fn google_reasoning_control_does_not_rewrite_other_endpoints() {
        for provider in [ProviderKind::Google, ProviderKind::Custom] {
            let request = google_request_with_signed_tool(Some("SIG"));
            let wire = build_chat_wire_body(
                &request,
                provider,
                "https://gateway.example.com/v1",
                false,
                None,
            )
            .expect("valid gateway request");
            assert!(wire.body.get("reasoning_effort").is_none());
            assert!(wire.body.get("google").is_none());
            assert!(wire.body.get("extra_body").is_none());
        }
    }

    #[test]
    fn google_signature_captured_from_non_streaming_tool_call() {
        let payload = json!({
            "id": "resp-1",
            "model": "gemini-3.1-pro-preview",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-g-9",
                        "type": "function",
                        "function": {
                            "name": "read",
                            "arguments": "{\"path\":\"x\"}"
                        },
                        "extra_content": {
                            "google": { "thought_signature": "SIG-stream" }
                        }
                    }]
                }
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let response = parse_chat_message(&payload).expect("parses");
        let signature = response.content.iter().find_map(|block| match block {
            ContentBlock::ToolUse {
                thought_signature, ..
            } => thought_signature.clone(),
            _ => None,
        });
        assert_eq!(signature.as_deref(), Some("SIG-stream"));
    }

    #[test]
    fn google_signature_captured_from_streaming_first_chunk() {
        let chunk = json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call-g-7",
                        "type": "function",
                        "function": { "name": "read", "arguments": "{}" },
                        "extra_content": {
                            "google": { "thought_signature": "SIG-delta" }
                        }
                    }]
                }
            }]
        });
        let mut content_index = 0u32;
        let mut text_started = false;
        let mut thinking_started = false;
        let mut tool_indices = std::collections::HashMap::new();
        let mut reasoning_buffers = std::collections::HashMap::new();
        let mut inline_tags = InlineReasoningTagState::default();
        let events = parse_sse_chunk_with_reasoning_style(
            &chunk,
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_buffers,
            &mut inline_tags,
            ReasoningStreamStyle::None,
        );
        let signature = events.iter().find_map(|event| match event {
            StreamEvent::ContentBlockStart {
                content_block:
                    ContentBlockStart::ToolUse {
                        thought_signature, ..
                    },
                ..
            } => thought_signature.clone(),
            _ => None,
        });
        assert_eq!(signature.as_deref(), Some("SIG-delta"));
    }

    /// Run the production restart/resume chain over a message history:
    /// persist to disk, reload, repair crashed tool pairs, then project for
    /// restore exactly as `apply.rs` does before assigning `api_messages`.
    /// Returns the recovery receipt, the restored messages, and the raw
    /// session JSON as it actually sits on disk.
    fn resumed(
        messages: &[Message],
    ) -> (
        crate::session_manager::SessionRecovery,
        Vec<Message>,
        String,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = crate::session_manager::SessionManager::new(dir.path().join("sessions"))
            .expect("session manager");
        let session = crate::session_manager::create_saved_session(
            messages,
            "gemini-3.1-pro-preview",
            dir.path(),
            0,
            None,
        );
        let id = session.metadata.id.clone();
        let path = manager.save_session(&session).expect("save session");
        let on_disk = std::fs::read_to_string(&path).expect("read persisted session");
        let recovery = manager
            .recover_session_for_resume(&id)
            .expect("recover session for resume");
        let restored = crate::runtime_handoff::project_owned_messages_for_restore(
            recovery.session.messages.clone(),
        );
        (recovery, restored, on_disk)
    }

    fn replayed_signature(body: &serde_json::Value) -> Option<String> {
        body["messages"]
            .as_array()
            .expect("wire messages")
            .iter()
            // The crash repair appends a trailing assistant text receipt, so
            // find the tool-call message by shape, never by index.
            .find(|message| message.get("tool_calls").is_some())
            .expect("assistant tool-call message")
            .pointer("/tool_calls/0/extra_content/google/thought_signature")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    /// C22 done-evidence item 3: restarting and resuming must continue the
    /// signed history. This walks the whole persistence chain — durable JSON,
    /// reload, restore projection, wire body — because the signature can be
    /// lost at any of them, and a serde round-trip alone would prove none of
    /// it. Local unit evidence only: it does not exercise a real Gemini call.
    #[test]
    fn resumed_google_session_replays_signed_tool_calls() {
        let original = signed_history(Some("SIG-abc123"), true);
        let (recovery, restored, on_disk) = resumed(&original);

        assert!(
            !recovery.changed,
            "a fully paired history needs no repair on resume"
        );
        assert_eq!(
            recovery.session.messages, original,
            "reload must return the signed history unchanged"
        );
        assert_eq!(
            restored, original,
            "the restore projection must not touch signed tool history"
        );
        assert!(
            on_disk.contains("\"thought_signature\"") && on_disk.contains("SIG-abc123"),
            "the signature must reach durable storage, not just live memory"
        );

        for (provider, base_url) in [
            (ProviderKind::Google, DEFAULT_GOOGLE_BASE_URL),
            (
                ProviderKind::Custom,
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
        ] {
            let body = build_chat_wire_body(
                &request_from(restored.clone()),
                provider,
                base_url,
                true,
                None,
            )
            .expect("a resumed signed history must build, not fail closed");
            assert_eq!(
                replayed_signature(&body.body).as_deref(),
                Some("SIG-abc123"),
                "resumed history must still replay the signature ({base_url})"
            );
        }
    }

    /// The real restart shape: the process died between the tool call and its
    /// result, so resume runs `repair_tool_call_pairs`, which rebuilds every
    /// message. That rebuild must not strip `ToolUse` fields — if it did, the
    /// repaired history would fail closed on Gemini 3 forever after.
    #[test]
    fn crash_repaired_resume_keeps_the_signature_on_the_repaired_tool_call() {
        let (recovery, restored, on_disk) = resumed(&signed_history(Some("SIG-abc123"), false));

        assert!(recovery.changed, "a dangling tool call must be repaired");
        assert_eq!(recovery.repaired_call_count, 1);
        assert_eq!(recovery.duplicate_result_count, 0);
        assert_eq!(recovery.orphan_result_count, 0);
        assert!(
            on_disk.contains("\"thought_signature\"") && on_disk.contains("SIG-abc123"),
            "the signature must reach durable storage, not just live memory"
        );
        assert!(
            restored
                .iter()
                .any(|message| message.content.iter().any(|block| matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, content, .. }
                        if tool_use_id == "call-g-1" && content.contains("crashed_and_repaired")
                ))),
            "the repair must pair the dangling call with a terminal result"
        );

        let body = build_chat_wire_body(
            &request_from(restored),
            ProviderKind::Google,
            DEFAULT_GOOGLE_BASE_URL,
            true,
            None,
        )
        .expect("a crash-repaired signed history must build, not fail closed");
        assert_eq!(
            replayed_signature(&body.body).as_deref(),
            Some("SIG-abc123"),
            "crash repair must not strip the thought signature"
        );
    }
}
