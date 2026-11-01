# Local model reasoning controls

The `vllm` Chat Completions route also serves local SGLang deployments. The
server protocol does not define the model template's reasoning ladder. Reviewed
template facts in `crates/config/src/local_reasoning.json` refine this route:

| Served model family | Selectable controls | Wire values |
| --- | --- | --- |
| Qwen3.8-27B, Qwen3.8-Flash-Next | off, low, medium, max | thinking switch; low, medium, xhigh |
| Ornith-1.5-35B-A3B, Qwen3.6-35B-A3B | off, high (on) | thinking switch only |
| GLM-5.3-Flash | low, high, max | low, high, max |
| DeepSeek-V4-Flash-Vision-Exp | off, low, high, max | prior thinking switch; low, high, max |

Qwen3.8's `high` preference is a legacy name for its deepest tier and now sends
`xhigh`; its template rejects the literal `high`. Templates that only consume a
thinking switch no longer receive fictitious effort levels. GLM and DeepSeek
legacy `medium` preferences use their middle supported level, `high`. A stale
`off` preference on the GLM always-thinking ladder normalizes to `low`.
DeepSeek-V4-Vision retains its existing Off request spelling; this compatibility
change does not infer a new inability to turn thinking off from an effort enum.
Model-specific effort travels in `chat_template_kwargs.reasoning_effort`; the
top-level effort is omitted so older OpenAI schemas cannot reject xhigh/max
before the template is rendered. The registry records source revisions and legacy aliases; normalization does
not rewrite saved settings.

The canonical served model ID must match a reviewed name family (an optional
repository prefix and quantization/deployment suffix are accepted). Unknown
IDs, unrelated versions and other provider routes retain their existing
behavior. A custom template under a canonical name can differ from these facts;
do not assume arbitrary local aliases or customized templates are detected.

This changes only thinking fields. Endpoint selection, credentials, messages,
tools, streaming, sampling, token limits, retries and permission checks retain
their existing owners. No model is started, probed or redeployed by this logic.
