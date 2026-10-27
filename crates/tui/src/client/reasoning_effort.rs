use codewhale_config::ProviderKind;
use serde_json::{Value, json};

use super::deepseek_effort;

/// Write DeepSeek's Chat Completions thinking controls from the shared tier
/// table.
///
/// The table (`client::deepseek_effort`) is the only place the tier ladder is
/// written down; this function is just the Chat wire's spelling of it. An
/// effort string the table does not name is not a DeepSeek tier request, so
/// nothing is written rather than guessing a field the user did not ask for.
fn apply_deepseek_chat_reasoning_effort(body: &mut Value, normalized: &str) {
    let Some(tier) = deepseek_effort::deepseek_effort_tier(normalized) else {
        return;
    };
    if let Some(value) = tier.chat_reasoning_effort() {
        body["reasoning_effort"] = json!(value);
    }
    body["thinking"] = json!({
        "type": if tier.chat_thinking_enabled() { "enabled" } else { "disabled" },
    });
}

pub(crate) fn apply_reasoning_effort(
    body: &mut Value,
    effort: Option<&str>,
    provider: ProviderKind,
) {
    let Some(effort) = effort else {
        return;
    };
    let normalized = effort.trim().to_ascii_lowercase();
    // DeepSeek's first-party routes read their tier ladder from the one
    // annotated table (`client::deepseek_effort`), shared with the Responses
    // wire, so a documented mapping change is a single edit. Every other
    // provider keeps its own dialect below.
    if matches!(provider, ProviderKind::Deepseek) {
        apply_deepseek_chat_reasoning_effort(body, &normalized);
        return;
    }
    if provider == ProviderKind::Stepfun {
        // Step 3.5 has no documented effort selector. The other coding models
        // are always reasoning-capable; do not invent an Off wire value.
        let model = body.get("model").and_then(Value::as_str).unwrap_or("");
        let tier = match (model, normalized.as_str()) {
            ("step-5-preview" | "step-3.7-flash" | "step-3.5-flash-2603", "minimal" | "low") => {
                Some("low")
            }
            ("step-5-preview" | "step-3.7-flash", "medium") => Some("medium"),
            ("step-3.5-flash-2603", "medium") => Some("high"),
            (
                "step-5-preview" | "step-3.7-flash" | "step-3.5-flash-2603",
                "high" | "max" | "xhigh" | "ultra",
            ) => Some("high"),
            _ => None,
        };
        if let Some(tier) = tier {
            body["reasoning_effort"] = json!(tier);
        }
        return;
    }
    match normalized.as_str() {
        "off" | "disabled" | "none" | "false" => match provider {
            // Handled by the shared DeepSeek table above, before this match.
            ProviderKind::Deepseek => {}
            ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::XiaomiMimo
            | ProviderKind::Novita
            | ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Sglang
            | ProviderKind::Volcengine
            | ProviderKind::Deepinfra
            | ProviderKind::Together
            | ProviderKind::Atlascloud
            | ProviderKind::Zai => {
                body["thinking"] = json!({ "type": "disabled" });
            }
            // TelecomJS TokenHub: the gateway's OpenAI Chat Completions API
            // (POST /v1/chat/completions) does not document `reasoning_effort`
            // or `thinking` as supported parameters. The `thinking` field is
            // only available on the Anthropic Messages API (POST /v1/messages)
            // with a different shape ({"type":"enabled","budget_tokens":N}).
            // Since CodeWhale routes TelecomJS through the Chat Completions
            // path, we must NOT inject these fields — the gateway may silently
            // ignore them or reject the request, and not every gateway model
            // (qwen-max, deepseek-chat, gpt-4o, claude, etc.) accepts the same
            // reasoning dialect (#4188 review: verify against actual behavior).
            ProviderKind::Telecomjs => {}
            // Eden AI documents `thinking` only for Anthropic Claude models.
            // This gateway can route unrelated model families, so the generic
            // provider must not inject a model-specific reasoning dialect.
            ProviderKind::Edenai => {}
            ProviderKind::Zenmux => {}
            // CSDN 星图 is OpenAI-compatible but documents no provider-owned
            // reasoning dialect; do not invent one.
            ProviderKind::Csdn => {}
            // The Codewhale API is a passthrough to the account's own
            // connected provider; it documents no Codewhale-owned
            // reasoning-effort translation, so nothing is invented here.
            ProviderKind::Codewhale => {}
            // Concentrate rides the Responses wire (`reasoning.effort`), never
            // these Chat Completions controls.
            ProviderKind::Concentrate => {}
            // Model Studio (DashScope): its top-level controls are route- AND
            // model-specific, so the provider enum alone cannot decide them —
            // a custom `base_url` on the same identity is an arbitrary
            // gateway. `apply_modelstudio_route_reasoning_controls` in
            // client::chat is the sole writer; it strips these fields for all
            // four variants and re-adds them only on a verified Alibaba host.
            // Source: <https://www.alibabacloud.com/help/en/model-studio/deep-thinking>
            ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic => {}
            ProviderKind::OpenaiCodex => {
                // OpenAI Codex uses Responses API — thinking handled differently
            }
            ProviderKind::Fireworks => {}
            // vLLM is an OpenAI-protocol server, not an Anthropic-protocol one.
            // For Qwen3 / DeepSeek-R1 / other reasoning models hosted via vLLM,
            // the canonical OpenAI extension to disable thinking is
            // `chat_template_kwargs.enable_thinking`. The old
            // `thinking: {type: disabled}` field is Anthropic-native and
            // silently ignored by vLLM — the model still emits a full
            // reasoning trace into the `reasoning` field (which this client
            // doesn't surface), causing 10+ seconds of perceived "freeze"
            // before the first content token (PR #1480 by @h3c-hexin).
            ProviderKind::Vllm => {
                body["chat_template_kwargs"] = json!({
                    "enable_thinking": false,
                });
            }
            ProviderKind::Openai
            | ProviderKind::WanjieArk
            | ProviderKind::Qianfan
            | ProviderKind::Arcee
            | ProviderKind::Huggingface
            | ProviderKind::Modelscope
            | ProviderKind::Custom => {}
            ProviderKind::Moonshot => {
                // #3024: Kimi models accept thinking enable/disable.
                body["thinking"] = json!({ "type": "disabled" });
            }
            ProviderKind::Ollama => {
                // #3024: Ollama OpenAI-compat endpoint accepts think param.
                body["think"] = json!(false);
            }
            ProviderKind::OllamaCloud => {
                // Ollama Cloud stays on the documented OpenAI-compatible
                // `/v1/chat/completions` wire. Native `/api/chat` uses
                // `think`; this wire uses `reasoning_effort`.
                body["reasoning_effort"] = json!("none");
            }
            ProviderKind::Anthropic
            | ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel => {
                // Thinking shaping happens in the Messages adapter, which
                // applies each provider's supported control fields.
            }
            ProviderKind::NvidiaNim => {
                body["chat_template_kwargs"] = json!({
                    "thinking": false,
                });
            }
            ProviderKind::Minimax => {}
            ProviderKind::Stepfun => {}
            ProviderKind::Sakana => {}
            ProviderKind::LongCat => {}
            ProviderKind::OpencodeGo | ProviderKind::OpencodeZen => {}
            ProviderKind::Meta => {}
            ProviderKind::Xai => {}
            ProviderKind::Mistral => {}
            ProviderKind::Google => {}
            ProviderKind::Antigravity => {}
        },
        "low" | "minimal" | "medium" | "mid" | "high" | "" => match provider {
            // Handled by the shared DeepSeek table above, before this match.
            ProviderKind::Deepseek => {}
            // DeepSeek-compatible hosted routes: low/medium both map to high.
            // Their own wire contracts are not verified here, so the historic
            // collapse stays rather than inventing unsupported wire values.
            ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Sglang
            | ProviderKind::Volcengine
            | ProviderKind::Deepinfra
            | ProviderKind::Atlascloud => {
                body["reasoning_effort"] = json!("high");
                body["thinking"] = json!({ "type": "enabled" });
            }
            // TelecomJS: see comment in the "off" branch above — the gateway's
            // Chat Completions API does not support reasoning_effort or thinking.
            ProviderKind::Telecomjs => {}
            ProviderKind::Edenai => {}
            ProviderKind::Zenmux => {}
            // CSDN 星图 is OpenAI-compatible but documents no provider-owned
            // reasoning dialect; do not invent one.
            ProviderKind::Csdn => {}
            // The Codewhale API is a passthrough to the account's own
            // connected provider; it documents no Codewhale-owned
            // reasoning-effort translation, so nothing is invented here.
            ProviderKind::Codewhale => {}
            // Concentrate rides the Responses wire (`reasoning.effort`), never
            // these Chat Completions controls.
            ProviderKind::Concentrate => {}
            // Model Studio: see the "off" branch — the route- and model-aware
            // shaper in client::chat is the sole writer of these fields.
            ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic => {}
            // OpenRouter/OrcaRouter/Novita/Together: pass through the actual
            // user-chosen value. OpenRouter's unified scale is
            // none/minimal/low/medium/high/xhigh; DeepSeek models hosted there
            // accept those directly.
            ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::Novita
            | ProviderKind::Together => {
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::XiaomiMimo => {
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Arcee | ProviderKind::Huggingface | ProviderKind::Modelscope => {
                let value = match normalized.as_str() {
                    "minimal" => "minimal",
                    "low" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
            }
            ProviderKind::Fireworks => {
                body["reasoning_effort"] = json!("high");
            }
            ProviderKind::Vllm => {
                body["chat_template_kwargs"] = json!({
                    "enable_thinking": true,
                });
                // vLLM supports low/medium/high natively — pass through the
                // user-chosen value instead of hard-coding "high".
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
            }
            ProviderKind::Openai
            | ProviderKind::WanjieArk
            | ProviderKind::Qianfan
            | ProviderKind::OpenaiCodex
            | ProviderKind::Custom => {}
            ProviderKind::Moonshot => {
                // #3024: Kimi models accept thinking enable.
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Ollama => {
                // #3024: Ollama think param.
                body["think"] = json!(true);
            }
            ProviderKind::OllamaCloud => {
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
            }
            ProviderKind::Anthropic
            | ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel => {
                // Thinking shaping happens in the Messages adapter, which
                // applies each provider's supported control fields.
            }
            ProviderKind::NvidiaNim => {
                body["chat_template_kwargs"] = json!({
                    "thinking": true,
                    "reasoning_effort": "high",
                });
            }
            ProviderKind::Minimax => {}
            ProviderKind::Zai => {
                body["thinking"] = json!({
                    "type": "enabled",
                    "clear_thinking": false,
                });
            }
            ProviderKind::Stepfun => {}
            ProviderKind::Sakana => {}
            ProviderKind::LongCat => {}
            ProviderKind::OpencodeGo | ProviderKind::OpencodeZen => {}
            ProviderKind::Meta => {}
            ProviderKind::Xai => {}
            ProviderKind::Mistral => {}
            ProviderKind::Google => {}
            ProviderKind::Antigravity => {}
        },
        "xhigh" | "max" | "highest" | "ultra" | "ultracode" => match provider {
            // Handled by the shared DeepSeek table above, before this match.
            ProviderKind::Deepseek => {}
            ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Sglang
            | ProviderKind::Volcengine
            | ProviderKind::Deepinfra
            | ProviderKind::Atlascloud => {
                body["reasoning_effort"] = json!("max");
                body["thinking"] = json!({ "type": "enabled" });
            }
            // TelecomJS: see comment in the "off" branch above — the gateway's
            // Chat Completions API does not support reasoning_effort or thinking.
            ProviderKind::Telecomjs => {}
            ProviderKind::Edenai => {}
            ProviderKind::Zenmux => {}
            // CSDN 星图 is OpenAI-compatible but documents no provider-owned
            // reasoning dialect; do not invent one.
            ProviderKind::Csdn => {}
            // The Codewhale API is a passthrough to the account's own
            // connected provider; it documents no Codewhale-owned
            // reasoning-effort translation, so nothing is invented here.
            ProviderKind::Codewhale => {}
            // Concentrate rides the Responses wire (`reasoning.effort`), never
            // these Chat Completions controls.
            ProviderKind::Concentrate => {}
            // Model Studio: see the "off" branch — the route- and model-aware
            // shaper in client::chat is the sole writer of these fields.
            ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic => {}
            ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::Novita
            | ProviderKind::Together => {
                body["reasoning_effort"] = json!("xhigh");
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::XiaomiMimo => {
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Arcee | ProviderKind::Huggingface | ProviderKind::Modelscope => {
                body["reasoning_effort"] = json!("high");
            }
            ProviderKind::Fireworks => {
                body["reasoning_effort"] = json!("max");
            }
            ProviderKind::Vllm => {
                body["chat_template_kwargs"] = json!({
                    "enable_thinking": true,
                });
                // vLLM only supports none/low/medium/high — downgrade
                // "max" to "high" instead of sending an invalid value.
                body["reasoning_effort"] = json!("high");
            }
            ProviderKind::Openai
            | ProviderKind::WanjieArk
            | ProviderKind::Qianfan
            | ProviderKind::OpenaiCodex
            | ProviderKind::Custom => {}
            ProviderKind::Moonshot => {
                // #3024: Kimi models accept thinking enable.
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Ollama => {
                // #3024: Ollama think param.
                body["think"] = json!(true);
            }
            ProviderKind::OllamaCloud => {
                body["reasoning_effort"] = json!("max");
            }
            ProviderKind::Anthropic
            | ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel => {
                // Thinking shaping happens in the Messages adapter, which
                // applies each provider's supported control fields.
            }
            ProviderKind::NvidiaNim => {
                body["chat_template_kwargs"] = json!({
                    "thinking": true,
                    "reasoning_effort": "max",
                });
            }
            ProviderKind::Minimax => {}
            ProviderKind::Zai => {
                body["thinking"] = json!({
                    "type": "enabled",
                    "clear_thinking": false,
                });
            }
            ProviderKind::Stepfun => {}
            ProviderKind::Sakana => {}
            ProviderKind::LongCat => {}
            ProviderKind::OpencodeGo | ProviderKind::OpencodeZen => {}
            ProviderKind::Meta => {}
            ProviderKind::Xai => {}
            ProviderKind::Mistral => {}
            ProviderKind::Google => {}
            ProviderKind::Antigravity => {}
        },
        _ => {}
    }
}
