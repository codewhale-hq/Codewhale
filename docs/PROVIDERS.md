# Provider Registry

> 阅读简体中文版：[zh_hans/PROVIDERS.md](zh_hans/PROVIDERS.md)

Speech recognition, synthesis, translation and video have separate execution
paths. See [media models](MEDIA_MODELS.md) for current support and reviewed
upstream candidates; a chat catalogue row alone does not add a media backend.

This registry describes provider behavior that is wired into the current
Codewhale codebase. It is intentionally conservative: shipped entries are
limited to provider IDs, config keys, auth paths, base URLs, model resolution,
and capability metadata that the code already knows about.

DeepSeek remains the default provider, but every entry in `ProviderKind::ALL`
is a first-class selectable provider route. `ALL` is the catalog/picker
surface — one identity per vendor. Dual-wire dialect kinds (`*Anthropic`, e.g.
`deepseek-anthropic`) and the Model Studio plan variants stay on the enum for
serde and `provider_for_kind` but are deliberately **not** catalog rows:
a plan is `mode`/`base_url` and a dialect is `wire = openai|anthropic` on the
primary provider config (`crates/config/src/provider_kind.rs:221-226`). Hosted
routes, generic OpenAI-compatible endpoints, the OpenAI Codex/ChatGPT route,
native Anthropic, and local runtimes all run the same terminal harness against
the selected provider/model/base URL.

A host reached over plain Chat Completions is an ordinary named provider,
not a `ProviderKind`: enum variants are reserved for distinct *wires*
(Anthropic Messages, Codex Responses, Google thought signatures). Any such
host is a `[providers.<name>]` table with a base URL, a model, and a key env
(`docs/CONFIGURATION.md`); `/provider` and `/setup` keep a "paste a Base URL
and a key" path for exactly this. Offerings come from live `GET /v1/models`
plus the Codewhale catalog rather than a compiled roster (#5350, #6289).

Known-good hosts. These ship as bundled descriptor rows in
`crates/config/assets/provider_descriptors.json` (compiled in by
`crates/config/src/descriptors.rs`): each row says how to reach the host — wire,
base URL, key env, aliases — while model ids stay live from `GET /v1/models`
and the Codewhale catalog; the example model is only a bootstrap hint. Verify
against the vendor's own docs before trusting any value here:

| Host | Base URL | Example models | API key env |
| --- | --- | --- | --- |
| SenseNova | `https://token.sensenova.cn/v1` | `deepseek-v4-flash` | `SENSENOVA_API_KEY` |
| Baseten | `https://inference.baseten.co/v1` | `deepseek-ai/DeepSeek-V4-Pro` | `BASETEN_API_KEY` |
| Groq | `https://api.groq.com/openai/v1` | `llama-3.3-70b-versatile` | `GROQ_API_KEY` |
| Cerebras | `https://api.cerebras.ai/v1` | `llama-3.3-70b` | `CEREBRAS_API_KEY` |
| Command Code | `https://api.commandcode.ai/provider/v1` | `deepseek/deepseek-v4-flash` | `COMMAND_CODE_API_KEY` |
| Alibaba Model Studio (DashScope) | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | `qwen3.8-flash` | `DASHSCOPE_API_KEY` |
| AICraft | `https://aicraftapi.com/v1` | `claude-4.6-sonnet`; DeepSeek / Claude / Gemini / Qwen / GLM / MiniMax / Doubao families | `AICRAFT_API_KEY` |
| Tsubasa | `https://api.tsubasa.sh/v1` | `tsubasa-pro`, `tsubasa-fast` (32,768-token context) | `TSUBASA_API_KEY` |
| Cheaper Inference | `https://api.cheaperinference.com/v1` | `gpt-5.4-mini`, `claude-sonnet-5`, `gemini-3.1-pro` | `CHEAPER_INFERENCE_API_KEY` |
| Yolo-Auto | `https://yolo-auto.com/v1` | `qwen3.8-flash`; `yolo` / `yolo-small` | `YOLO_AUTO_API_KEY` |

AICraft's roster spans DeepSeek, Anthropic Claude, Google Gemini, Qwen, GLM,
MiniMax and Doubao ids on its OpenAI-compatible endpoint. The authority is
`GET https://aicraftapi.com/v1/models` with your key — pick a model from that
list, not from this table.
Tsubasa implements only `GET /v1/models` and Chat Completions. Its two public
model ids share a 32,768-token context, smaller than the 128,000 tokens Codewhale assumes
for an unknown model, so set it on the route after saving:
`codewhale config set providers.tsubasa.context_window 32768`.

Cheaper Inference is an OpenAI-compatible gateway with one key for models from
several labs. Model ids are bare, such as `gpt-5.4-mini` or `claude-sonnet-5`.
Pricing varies by model and route; consult the provider’s current catalog.
The authority is `GET https://api.cheaperinference.com/v1/models` with your key.
Docs: <https://cheaperinference.com/docs>.

OpenCode Zen and OpenCode Go are first-class provider routes, configured like
any other provider below; they are not part of this table. In `/provider`,
type to filter the list (letters not bound to a row action); `Ctrl+T` probes the
selected row's `/models` and records reachability only (a 2xx is not
model-ready).

Sources to keep in sync:

- `crates/config/assets/provider_descriptors.json` - built-in and compatible-host
  labels, defaults, aliases, key-env lists, config/secret slots, and credential
  guidance. `crates/config/build.rs` generates immutable typed views and the
  existing constant projections from this one data owner.
- `crates/config/src/provider_kind.rs` and `src/lib.rs` - legacy identity serde,
  config schema, environment precedence and Rust route/auth behavior.
- `crates/tui/src/config.rs` - captures and verifies exact configured provider
  identities and keeps provider-specific credential and route policy in Rust.
- `crates/config/assets/catalog_corrections.json` `reviewed` - intrinsic facts,
  scoped selector aliases, completion references and pure transport metadata.
  The existing seed renderer embeds this reviewed supplement in Models.dev.
- `crates/agent/src/lib.rs` - compatibility projection of those shared selector
  rows, used by `codewhale model resolve` (and the `[model]` config surface).
  `codewhale model list` reads the resolved provider lake instead, so a single
  list shows bundled, live and configured rows together.
- `config.example.toml` and `docs/CONFIGURATION.md` - user-facing config
  examples and environment variable reference.
- `scripts/check-provider-registry.py` - drift check for canonical provider
  IDs, live TUI provider IDs, TOML table names, static registry rows, and
  documented defaults.

## Captured Provider Identity

Presentation names, labels, aliases, and historical wire tags come from
`provider_descriptors.json`. `ProviderKind` remains the Rust-owned intrinsic
credential, protocol, and region distinction. A configured route captures both
its kind and exact table key; a custom table named `openai` stays custom and
does not acquire OpenAI credentials or protocol rules from its name. Case is
significant for custom keys.

DeepSeek China has three preserved representations: `deepseek_c_n` in released
TUI serde wrappers, `deepseek-cn` as its captured route ID, and `deepseek_cn` as
its config leaf. Its intrinsic kind is DeepSeek, while its table and endpoint
stay distinct.

An old custom route without an additive provider ID can resume only with a
verified root `base_url` migration into the active `providers.custom` table.
That private receipt is bound to the parsed table generation. A table-only,
profile-only, conflicting, or later replaced table cannot establish the
missing identity; an explicit empty ID is refused. Writes reuse the existing
locked config mutation and undo comparison, obtain the fresh migration receipt
under that lock, and verify the captured table before changing a leaf.

In-flight requests retain their captured identity, endpoint, and credential
generation. Health lookup cannot treat an opaque credential reference as ready
merely because an earlier request used the same authentication class.

## Provider Selection

With no saved model, no `default_text_model`, and no `CODEWHALE_MODEL` or
provider-specific model variable, a fresh install runs `deepseek-flash` on the
DeepSeek provider. Precedence is the active provider's configured default, then
its catalog default — so an explicit `default_text_model` is not silently
overridden by whichever model the shipped catalog lists first.

Refresh model catalogs without installing a new Codewhale release:

```sh
codewhale models --update
codewhale models --update --provider openai
codewhale models --provider openai --json
```

`models --update` (also `--refresh`) updates the shared Models.dev metadata
and calls the existing `/models` endpoint for each configured provider with
its own credentials. `--provider ID` restricts the refresh to that exact
provider, including named custom endpoints. It makes no inference requests
and never changes the saved provider or model. A command-line API key is
confined to the active provider; other routes are reported as skipped for
that invocation.

Plain `models` lists the active provider's saved catalog without provider requests.
For ChatGPT, it validates the selected registration locally before reading
that account's saved roster. Successful refreshes are saved under Codewhale's
catalog directory and used by the model/provider pickers. Cache files are
scoped to provider identity and endpoint; a failed refresh preserves prior
rows. Text output reports source, last successful fetch time (Unix seconds),
and freshness. `--update --json` adds per-source receipts and aggregate counts;
partial failures return a nonzero exit code after writing those receipts.
Ordinary `models --json` keeps its model-array format. Bundled/configured
fallbacks are not proof that an account can use every listed model.

### Sign in with ChatGPT

The `openai-codex` provider ID now uses OpenAI's official open-source
[Sign in with ChatGPT flow](https://developers.openai.com/siwc/token-sharing-open-source).
Sign in and refresh the selected account's catalog:

```sh
codewhale auth chatgpt
codewhale models --update --provider openai-codex
codewhale --provider openai-codex
```

Sign-in opens your system browser, uses a loopback callback and PKCE, validates
the returned identity, and stores the issued registration and renewable tokens
in Codewhale's protected credential storage. Permission to use your ChatGPT
plan is separate from identity sign-in. A declined or missing plan grant stops
inference; choose another provider explicitly if you want another billing path.
`codewhale auth chatgpt-revoke` signs out of the Codewhale-owned session.
This experimental implementation retains one active registration. A picker
for multiple saved ChatGPT accounts is not implemented yet.

Model discovery uses your granted bearer token at
`GET https://api.openai.com/v1/models`; inference uses the public
`POST https://api.openai.com/v1/responses` endpoint. The model picker keeps
OpenAI's display labels and order and lists only entries with `visibility: list`.
The catalog contains no credentials and is bound to the verified issuer,
issued client ID, and account subject. Another account or workspace never
inherits those model choices. Missing, invalid, or older-than-one-day rosters
are reported explicitly and provide no account entitlement evidence. Run the
refresh command after signing in. Neither model discovery nor ordinary listing
starts an inference request. Imported Codex CLI tokens and legacy process-token
variables cannot authorize this official plan route.

Eligible requests consume your ChatGPT plan or credits. ChatGPT Plus's
five-hour allowance is shared across apps; each app receives no separate
allowance. The documented five-hour limit does not apply to Pro. App-specific
limits can also apply. Review limits and access in
[ChatGPT usage settings](https://chatgpt.com/settings/usage). Codewhale does
not silently switch to an API key or another provider when a limit is reached.

This is an OpenAI preview for open-source/local apps. It does not grant access
to ChatGPT conversation history. Requests stream with `store: false` and
`stream: true`, while Codewhale retains its own session history and tools.
OpenAI-hosted image generation, file search, Code Interpreter, native computer
use, hosted MCP/connectors, and Responses `tool_search` are unavailable on this
route; Codewhale's own tools use supported function/custom tool calls.
The ChatGPT preview requests one function call at a time.
See [preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).
Paid or remotely hosted applications require the
[commercial partner interest process](https://openai.com/form/sign-in-with-chatgpt-interest/);
this local integration is not commercial approval.

The canonical provider IDs are the entries of `ProviderKind::ALL`
(`crates/config/src/provider_kind.rs`), in that order:

`deepseek`, `nvidia-nim`, `openai`, `atlascloud`, `wanjie-ark`, `volcengine`,
`openrouter`, `orcarouter`, `xiaomi-mimo`, `novita`, `fireworks`, `siliconflow`, `arcee`,
`siliconflow-CN`, `moonshot`, `sglang`, `vllm`, `ollama`, `ollama-cloud`, `huggingface`,
`together`, `qianfan`, `openai-codex`, `anthropic`, `openmodel`, `zai`,
`stepfun`, `minimax`, `deepinfra`, `sakana`, `longcat`, `opencode-go`,
`opencode-zen`, `meta`, `xai`, `mistral`, `telecomjs`, `modelstudio-token-plan`, `modelscope`,
`google`, `edenai`, `zenmux`, `csdn`, `concentrate`, `codewhale`, and `custom`.

`deepseek-anthropic` is *not* on this list — it is a wire dialect of
`deepseek`, reached with `wire = "anthropic"`, not a separate route to select.

Use any of these surfaces to select a provider:

- CLI: `codewhale --provider <id>`
- TUI: `/provider <id>` or the provider picker
- Env: `CODEWHALE_PROVIDER=<id>`; `DEEPSEEK_PROVIDER=<id>` is the legacy alias
- Config: `provider = "<id>"`

`deepseek-cn`, `deepseek_china`, `deepseekcn`, and `deepseek-china` are accepted
as legacy aliases for `deepseek`. They do not select a different official host;
DeepSeek uses the same official API host worldwide.

`deepseek_anthropic`, `deepseek-claude`, and `deepseek_claude` select
`deepseek-anthropic`, the opt-in DeepSeek route that speaks the Anthropic
Messages API at `https://api.deepseek.com/anthropic`. It keeps the normal
DeepSeek API key path but uses `x-api-key` plus `anthropic-version: 2023-06-01`
instead of Bearer auth. If the key already lives in official DeepSeek Harness
(`dsh`) at `$DSH_HOME/.credentials.yaml`, grant read-only access with
`codewhale auth external-consent --provider deepseek --mode read-only`.
Codewhale never writes that file and only reads `DEEPSEEK_API_KEY`.

`huggingface`, `hugging-face`, `hugging_face`, and `hf` all select the
Hugging Face Inference Providers route. This is the OpenAI-compatible router
path for chat/inference, not Hub browsing, model-card inspection, uploads, or
artifact export.

`telecomjs`, `telecom-js`, `telecom_js`, `telecomjs-cn`, and `tokenhub` all
select the TelecomJS TokenHub route. Its authenticated `/models` catalog is
key-scoped and remains isolated from every other provider's live snapshot.

Fresh shared config writes to `~/.codewhale/config.toml`. Existing
`~/.deepseek/config.toml` files are still read for compatibility.

### Legacy Antigravity tombstone

Antigravity is not a Codewhale provider and cannot be selected or run. Existing
legacy Antigravity provider state is recognized only as a non-runnable migration
tombstone. Run `codewhale auth clear --provider antigravity` to forget only
Codewhale-owned legacy configuration and consent metadata. This does not sign
out of, revoke, read, or otherwise alter any official Google or Antigravity
session. For Gemini, select the supported `google` provider and supply
`GEMINI_API_KEY`.

### Wire Protocol Compatibility

Provider selection is explicit. A model string prefix such as
`deepseek-ai/...`, `deepseek/...`, `qwen/...`, or `arcee-ai/...` is a
provider-owned wire ID or catalog namespace hint under the selected provider.
It is not a provider switch and must not be treated as proof that the route is
DeepSeek, OpenRouter, or any other provider.

Set the route with `provider = "<id>"`, `CODEWHALE_PROVIDER=<id>`, or
`codewhale --provider <id>`. Set the request model with `CODEWHALE_MODEL`, a
provider-specific model env var, top-level `default_text_model`, or
`[providers.<table>].model`. Set the endpoint with `CODEWHALE_BASE_URL`, a
provider-specific base URL env var, or `[providers.<table>].base_url`. Set auth
with `codewhale auth set --provider <id>`, `[providers.<table>].api_key`, or
the listed provider env vars.

| Provider ID | TOML table | Wire protocol | Auth env vars |
| --- | --- | --- | --- |
| `deepseek` | `[providers.deepseek]` | Model-aware: Responses default (`deepseek-flash`); Chat Completions (`deepseek-v4-pro`) | `DEEPSEEK_API_KEY` |
| `deepseek-anthropic` | `[providers.deepseek_anthropic]` | Anthropic Messages | `DEEPSEEK_API_KEY` |
| `nvidia-nim` | `[providers.nvidia_nim]` | OpenAI Chat Completions | `NVIDIA_API_KEY`, `NVIDIA_NIM_API_KEY` |
| `openai` | `[providers.openai]` | OpenAI Chat Completions | `OPENAI_API_KEY` |
| `atlascloud` | `[providers.atlascloud]` | OpenAI Chat Completions | `ATLASCLOUD_API_KEY` |
| `wanjie-ark` | `[providers.wanjie_ark]` | OpenAI Chat Completions | `WANJIE_ARK_API_KEY`, `WANJIE_API_KEY`, `WANJIE_MAAS_API_KEY` |
| `volcengine` | `[providers.volcengine]` | OpenAI Chat Completions | `VOLCENGINE_API_KEY`, `VOLCENGINE_ARK_API_KEY`, `ARK_API_KEY` |
| `openrouter` | `[providers.openrouter]` | OpenAI Chat Completions | `OPENROUTER_API_KEY` |
| `xiaomi-mimo` | `[providers.xiaomi_mimo]` | OpenAI Chat Completions | `XIAOMI_MIMO_TOKEN_PLAN_API_KEY`, `MIMO_TOKEN_PLAN_API_KEY`, `XIAOMI_MIMO_API_KEY`, `XIAOMI_API_KEY`, `MIMO_API_KEY` |
| `novita` | `[providers.novita]` | OpenAI Chat Completions | `NOVITA_API_KEY` |
| `fireworks` | `[providers.fireworks]` | OpenAI Chat Completions | `FIREWORKS_API_KEY` |
| `siliconflow` | `[providers.siliconflow]` | OpenAI Chat Completions | `SILICONFLOW_API_KEY` |
| `arcee` | `[providers.arcee]` | OpenAI Chat Completions | `ARCEE_API_KEY` |
| `siliconflow-CN` | `[providers.siliconflow_cn]` | OpenAI Chat Completions | `SILICONFLOW_API_KEY` |
| `moonshot` | `[providers.moonshot]` | OpenAI Chat Completions | `MOONSHOT_API_KEY`, `KIMI_API_KEY` |
| `sglang` | `[providers.sglang]` | OpenAI Chat Completions | `SGLANG_API_KEY` |
| `vllm` | `[providers.vllm]` | OpenAI Chat Completions | `VLLM_API_KEY` |
| `ollama` | `[providers.ollama]` | Local OpenAI-compatible Chat Completions | `OLLAMA_API_KEY` (optional; only for an authenticated local route) |
| `ollama-cloud` | `[providers.ollama_cloud]` | Hosted OpenAI-compatible Chat Completions | `OLLAMA_CLOUD_API_KEY`, `OLLAMA_API_KEY` |
| `huggingface` | `[providers.huggingface]` | OpenAI Chat Completions | `HUGGINGFACE_API_KEY`, `HF_TOKEN` |
| `modelscope` | `[providers.modelscope]` | OpenAI Chat Completions | `MODELSCOPE_API_KEY` |
| `together` | `[providers.together]` | OpenAI Chat Completions | `TOGETHER_API_KEY` |
| `qianfan` | `[providers.qianfan]` | OpenAI Chat Completions | `QIANFAN_API_KEY`, `BAIDU_QIANFAN_API_KEY` |
| `openai-codex` | `[providers.openai_codex]` | OpenAI Responses | Official Sign in with ChatGPT (`codewhale auth chatgpt`) with a validated Codewhale-owned plan grant |
| `anthropic` | `[providers.anthropic]` | Anthropic Messages | `ANTHROPIC_API_KEY` or `codewhale auth claude` |
| `openmodel` | `[providers.openmodel]` | Anthropic Messages | `OPENMODEL_API_KEY` |
| `zai` | `[providers.zai]` | OpenAI Chat Completions | `ZAI_API_KEY`, `Z_AI_API_KEY` |
| `stepfun` | `[providers.stepfun]` | OpenAI Chat Completions | `STEPFUN_API_KEY`, `STEP_API_KEY` |
| `minimax` | `[providers.minimax]` | OpenAI Chat Completions | `MINIMAX_API_KEY` |
| `deepinfra` | `[providers.deepinfra]` | OpenAI Chat Completions | `DEEPINFRA_API_KEY`, `DEEPINFRA_TOKEN` |
| `sakana` | `[providers.sakana]` | OpenAI Chat Completions | `FUGU_API_KEY`, `SAKANA_API_KEY` |
| `longcat` | `[providers.longcat]` | OpenAI Chat Completions | `LONGCAT_API_KEY` |
| `opencode-go` | `[providers.opencode_go]` | OpenAI Chat Completions | `OPENCODE_GO_API_KEY` |
| `opencode-zen` | `[providers.opencode_zen]` | Model-aware: OpenAI Responses, Anthropic Messages, or OpenAI Chat Completions | `OPENCODE_ZEN_API_KEY`, `OPENCODE_API_KEY` |
| `meta` | `[providers.meta]` | OpenAI Chat Completions | `META_MODEL_API_KEY`, `MODEL_API_KEY` |
| `telecomjs` | `[providers.telecomjs]` | OpenAI Chat Completions | `TELECOMJS_API_KEY` |
| `xai` | `[providers.xai]` | OpenAI Chat Completions | `XAI_API_KEY` |
| `mistral` | `[providers.mistral]` | OpenAI Chat Completions | `MISTRAL_API_KEY` |
| `google` | `[providers.google]` | OpenAI Chat Completions (official Gemini OpenAI-compat route; captures and replays thought signatures on tool calls) | `GOOGLE_API_KEY`, `GEMINI_API_KEY` |
| `edenai` | `[providers.edenai]` | OpenAI Chat Completions | `EDENAI_API_KEY` |
| `zenmux` | `[providers.zenmux]` | OpenAI Chat Completions | `ZENMUX_API_KEY` |
| `csdn` | `[providers.csdn]` | OpenAI Chat Completions | `CSDN_API_KEY` |
| `concentrate` | `[providers.concentrate]` | OpenAI Responses (`/v1/responses`) | `CONCENTRATE_API_KEY` |
| `codewhale` | `[providers.codewhale]` | Model-aware: OpenAI Chat Completions (`/v1/chat/completions`) or Anthropic Messages (`/v1/messages`), chosen per model by the account catalog | `CODEWHALE_API_KEY` |
| `modelstudio-token-plan` | `[providers.modelstudio_token_plan]` | OpenAI Chat Completions | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |
| `modelstudio-token-plan-anthropic` | `[providers.modelstudio_token_plan_anthropic]` | Anthropic Messages | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |
| `modelstudio-coding-plan` | `[providers.modelstudio_coding_plan]` | OpenAI Chat Completions | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |
| `modelstudio-coding-plan-anthropic` | `[providers.modelstudio_coding_plan_anthropic]` | Anthropic Messages | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |

Default base URLs and models for each route are listed in the shipped provider
table below. The wire protocol values above are derived from
`crates/config/src/provider.rs`: `ChatCompletions` is the default,
`openai-codex` overrides to `Responses`; `deepseek-anthropic`, `anthropic`, and
`openmodel` override to `AnthropicMessages`; `opencode-zen` resolves the
protocol from the selected model's curated offering; and `deepseek` is
model-aware — the shipped default `deepseek-flash` (and legacy
`deepseek-v4-flash`) rides the Responses endpoint while `deepseek-v4-pro`
stays on Chat Completions.

## Auth And Env Rules

For hosted providers, `codewhale auth set --provider <id>` saves an API key for
that provider. API-key environment variables are fallback inputs after saved
config and keyring credentials; an explicit process-level `--api-key` still
wins for that launch.

For base URL and model selection, prefer:

- `CODEWHALE_BASE_URL` / `CODEWHALE_MODEL` for the active provider.
- Provider-specific base URL/model env vars when listed below.
- `DEEPSEEK_BASE_URL`, `DEEPSEEK_MODEL`, and `DEEPSEEK_DEFAULT_TEXT_MODEL` as
  legacy aliases.

Non-local `http://` base URLs are rejected unless
`DEEPSEEK_ALLOW_INSECURE_HTTP=1` is set. Loopback HTTP URLs are allowed for
self-hosted runtimes.

## Custom DeepSeek-Compatible Endpoints

Most custom DeepSeek-compatible deployments can use an existing provider ID.
Do not create `[providers.deepseek_custom]`; the provider table names are fixed.
Instead, choose the closest shipped route and override its endpoint/model:

- DeepSeek-compatible hosted API: keep `provider = "deepseek"` and set
  `[providers.deepseek].base_url` plus `[providers.deepseek].model`, or launch
  with `DEEPSEEK_BASE_URL` and `DEEPSEEK_MODEL`.
- Generic OpenAI-compatible gateway: use `provider = "openai"` with
  `[providers.openai].base_url` plus `[providers.openai].model`, or launch with
  `OPENAI_BASE_URL` and `OPENAI_MODEL`.
- Multiple named OpenAI-compatible gateways, or local routes you want to pin
  from an AgentProfile, can use a custom table such as
  `[providers.lm-studio] kind = "openai-compatible"` and select it with
  `provider = "lm-studio"` or a profile `provider = "lm-studio"`.
- Local OpenAI-compatible runtimes: use `provider = "vllm"`, `"sglang"`, or
  `"ollama"` with the matching provider-specific base URL/model values.

Example user config for a DeepSeek-compatible host:

```toml
provider = "deepseek"

[providers.deepseek]
api_key = "YOUR_API_KEY"
base_url = "https://your-provider.example/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"
```

Example user config for a generic gateway:

```toml
provider = "openai"

[providers.openai]
api_key = "YOUR_GATEWAY_API_KEY"
base_url = "https://gateway.example/v1"
model = "your-deepseek-compatible-model"
```

Alibaba Cloud Model Studio (Bailian / DashScope) is a first-class provider as
of v0.9.4 with two plan profiles: Token Plan (Personal / Team) and Coding Plan.
Both plans expose an OpenAI-compatible Chat Completions endpoint and an
Anthropic-compatible Messages endpoint.

**Token Plan** (Personal and Team share the same AP-Southeast endpoint):

```toml
provider = "modelstudio-token-plan"

[providers.modelstudio_token_plan]
api_key = "YOUR_MODELSTUDIO_API_KEY"
# base_url defaults to https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1
model = "qwen3.8-max"   # or qwen3.8-max-preview | qwen3.7-plus | qwen3.7-max |
                        #    qwen3.6-flash | deepseek-v4-pro | deepseek-v4-flash-0731 |
                        #    glm-5.2
```

**Coding Plan** (separate international endpoint):

```toml
provider = "modelstudio-coding-plan"

[providers.modelstudio_coding_plan]
api_key = "YOUR_MODELSTUDIO_API_KEY"
# base_url defaults to https://coding-intl.dashscope.aliyuncs.com/v1
model = "qwen3.8-max"
```

**Anthropic-compatible dialect** — both plans also expose a native Anthropic
Messages path. Select it with the `-anthropic` provider suffix:

```toml
provider = "modelstudio-token-plan-anthropic"

[providers.modelstudio_token_plan_anthropic]
api_key = "YOUR_MODELSTUDIO_API_KEY"
# base_url defaults to https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic
model = "qwen3.8-max"
```

Create or copy a Model Studio API key from the
[Bailian console](https://bailian.console.aliyun.com/). The API key is shared
across all four provider IDs above; only the base URL and wire protocol differ.

**Thinking / reasoning.** Reasoning surfaces in the TUI's Thinking view on both
dialects, per Model Studio's
[deep-thinking docs](https://www.alibabacloud.com/help/en/model-studio/deep-thinking).

On the OpenAI-compatible routes the top-level controls are **route- and
model-specific**, and Codewhale fails closed: they are sent only when the
configured `base_url` is an official Alibaba Chat Completions host
(`*.maas.aliyuncs.com/compatible-mode/v1`, including workspace-scoped hosts, or
`coding-intl.dashscope.aliyuncs.com/v1`). A custom `base_url` on the same
provider ID gets `thinking`, `enable_thinking`, `preserve_thinking`, and
`reasoning_effort` stripped, so an arbitrary OpenAI-compatible gateway is never
handed Alibaba's dialect. On a verified host:

- **Hybrid models** (`qwen3.7-*`, `qwen3.6-*`, `deepseek-v4*`, `glm-*`,
  `kimi-k2.6*`) get `enable_thinking`: `false` for `off`, `true` otherwise.
- **Thinking-only models** — `qwen3.8-max` (catalogued `thinking: always_on`),
  `qwen3.8-max-preview` (effort/budget options, no toggle), and
  `kimi-k2.7-code` — get **no** enable/disable switch at all. Sending one is at
  best ignored.
- `preserve_thinking` is sent for the models documented to accept it
  (`qwen3.7-max`/`-plus`, `qwen3.6-max-preview`/`-plus`/`-flash`, `kimi-k2.6*`,
  `kimi-k2.7-code`), so the next turn keeps the assistant's trace.
- `reasoning_effort` is sent only for the two families with a documented ladder
  — `deepseek-v4*` and `glm-5`/`5.1`/`5.2` — mapped to `high` or `max`.

Reasoning streams back as `delta.reasoning_content`. It is replayed to the
provider on later turns only for the `preserve_thinking` models above and the
thinking-only models; `deepseek-v3.1`, `deepseek-v3.2`, and `glm-*` history
stays stripped pending live confirmation that DashScope accepts
`reasoning_content` in input messages. (`deepseek-v4*` replays regardless — the
DeepSeek thinking-mode contract requires it on every provider.)

On the Anthropic-compatible routes, thinking uses the documented
`{"type":"enabled","budget_tokens":N}` / `{"type":"disabled"}` shapes from the
[Anthropic-compatible Messages API](https://www.alibabacloud.com/help/en/model-studio/anthropic-api-messages),
with `budget_tokens` derived from the effort level.

DeepSeek (`deepseek-v4-pro`, `deepseek-v4-flash-0731`) and GLM (`glm-5.2`)
models served by Model Studio are provider-scoped and do not collide with the
first-party DeepSeek or Zhipu/Z.ai routes. Model Studio publishes no `glm-5.3`
entry, so Codewhale does not offer one on this route.
Pay-as-you-go workspace-id templating is not yet in the built-in provider; use
a custom provider entry for that plan until a follow-up adds it.

Private gateways with broken or intercepted certificates should use
`SSL_CERT_FILE` with a trusted CA bundle. The legacy
`insecure_skip_tls_verify = true` key is still parsed so `codewhale doctor` can
report stale configs, but provider clients reject it instead of skipping TLS
certificate verification.

Keep `provider`, `api_key`, and `base_url` in user config or process
environment. Project-local config overlays intentionally cannot set those keys,
so a repository cannot silently redirect prompts or credentials to another
endpoint.

## Local Models (DS4, Ollama, vLLM, SGLang)

Self-hosted OpenAI-compatible runtimes are first-class routes and are keyless
by default — set an API key only when your server requires one. Start your
runner, then point Codewhale at it with `--provider` / `/provider` or a config
table.

| Runner | Default base URL | Default model | Base URL override |
| --- | --- | --- | --- |
| `ollama` | `http://localhost:11434/v1` | live tag from `GET /v1/models` (pre-refresh: `unknown`) | `OLLAMA_BASE_URL` |
| `vllm` | `http://localhost:8000/v1` | `deepseek-ai/DeepSeek-V4-Pro` | `VLLM_BASE_URL` |
| `sglang` | `http://localhost:30000/v1` | `deepseek-ai/DeepSeek-V4-Pro` | `SGLANG_BASE_URL` |

### DS4 (DwarfStar)

[DS4](https://github.com/antirez/ds4/tree/84cc882352757baf628a1776badf7cc54d584e28)
serves DeepSeek V4 Flash and Pro locally
through an OpenAI-compatible API. Start DS4, then open Codewhale's prefilled,
keyless setup form:

```bash
./ds4-server --ctx 100000 --kv-disk-dir /tmp/ds4-kv --kv-disk-space-mb 8192
codewhale
# In Codewhale: /setup provider ds4
```

Review the prefilled route and press Enter to save it. The preset budgets a
100,000-token context to match that starter command and defaults to the Flash
compatibility alias. Check the local route explicitly with
`codewhale doctor --probe-local`.

DS4 loads the actual GGUF when the server starts. Its `deepseek-v4-flash` and
`deepseek-v4-pro` API ids are compatibility aliases; changing `/model` does
not swap the resident model. To run Pro, download the supported Pro weights
and start `ds4-server -m <pro.gguf> ...` as described by DS4. Update
`context_window` whenever the server's `--ctx` value changes.

The equivalent config is:

```toml
provider = "ds4"

[providers.ds4]
kind = "openai-compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "deepseek-v4-flash"
auth_mode = "none"
context_window = 100000
```

Codewhale reuses its existing OpenAI-compatible transport and DeepSeek
reasoning/tool-call shaping for DS4. It does not invent an API key, confuse an
API alias with the loaded GGUF, or silently switch to a hosted DeepSeek route.
The pinned DS4 [agent-client contract](https://github.com/antirez/ds4/blob/84cc882352757baf628a1776badf7cc54d584e28/README.md#agent-client-usage)
documents Chat Completions at `/v1`, DeepSeek thinking replay, streamed usage,
`max_tokens`, and no strict-tool mode; Codewhale follows those exact route
facts instead of inheriting unsupported capabilities from a generic gateway.
The primary sources for model-facing behavior are DeepSeek's official
[thinking-mode](https://api-docs.deepseek.com/guides/thinking_mode),
[tool-call](https://api-docs.deepseek.com/guides/tool_calls), and
[Chat Completion](https://api-docs.deepseek.com/api/create-chat-completion)
contracts. The pinned
[DeepSeek Harness adapter](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/llm/llm-deepseek/README.md)
is only a secondary implementation cross-check; it is not the API contract.

### Ollama

```bash
ollama serve          # if not already running
ollama pull <model>   # e.g. deepseek-v4-flash, or any tag you prefer
codewhale --provider ollama --model <model>
```

Provider-hinted model names are sent as-is, so `--model qwen3:8b` works with
any tag Ollama has pulled.

### Ollama Cloud

Ollama Cloud is a separate hosted provider. It uses the authenticated
OpenAI-compatible `/v1/chat/completions` route and defaults to `gpt-oss:120b`:

```toml
provider = "ollama-cloud"

[providers.ollama_cloud]
base_url = "https://ollama.com/v1"
model = "gpt-oss:120b"
```

Create a key in [Ollama account settings](https://ollama.com/settings/keys),
then run `codewhale auth set --provider ollama-cloud`. For ambient auth,
`OLLAMA_CLOUD_API_KEY` wins over Ollama's official `OLLAMA_API_KEY`.
`OLLAMA_CLOUD_BASE_URL` and `OLLAMA_CLOUD_MODEL` override the Cloud defaults;
arbitrary provider-owned model IDs pass through unchanged. Local `ollama`
remains a separate, keyless-by-default provider.

Compatibility is read-only and in memory: a released config that selected
`provider = "ollama"` with the exact normalized
`[providers.ollama] base_url = "https://ollama.com/v1"` tuple is treated as
`ollama-cloud` at runtime. Only that exact tuple may fall back to the legacy
`ollama` secret slot. Codewhale does not rewrite the config, copy or delete a
secret, migrate neighboring paths, or make an explicit `ollama-cloud` route
consume the legacy slot.

### vLLM

```bash
vllm serve <model> --port 8000
# or: python -m vllm.entrypoints.openai.api_server --model <model> --port 8000
codewhale --provider vllm --model <model>
```

vLLM's OpenAI-compatible server listens on port 8000 by default, matching
Codewhale's `VLLM_BASE_URL`.

### SGLang

```bash
python -m sglang.launch_server --model-path <model> --port 30000
codewhale --provider sglang --model <model>
```

SGLang's default port 30000 matches Codewhale's `SGLANG_BASE_URL`.

### Pinning a local route in config

```toml
provider = "ollama"       # or "vllm" / "sglang"

[providers.ollama]
model = "qwen3:8b"        # default is deepseek-v4-flash
# base_url defaults to http://localhost:11434/v1
```

Local models that print tool-call JSON without the wire markers: see
[When a Local Model Prints Tool JSON](#when-a-local-model-prints-tool-json).

## Credential Links

Provider setup surfaces use the same typed credential metadata as onboarding,
`/provider`, `/links`, setup receipts, and doctor output. A missing URL is
intentional: local, OAuth-only, and user-defined routes show their supported
configuration path instead of guessing a vendor page.

| Provider ID | Credential or console link |
| --- | --- |
| `deepseek`, `deepseek-anthropic` | [DeepSeek API keys](https://platform.deepseek.com/api_keys) |
| `nvidia-nim` | [NVIDIA NIM API keys](https://build.nvidia.com/settings/api-keys) |
| `openai` | [OpenAI API keys](https://platform.openai.com/api-keys) |
| `atlascloud` | [Atlas Cloud API keys](https://atlascloud.ai/docs/en/api-keys) |
| `wanjie-ark` | [Wanjie MaaS APIKEY docs](https://docs.wanjiedata.com/maas/maas-openapi-v1.html) |
| `volcengine` | [Volcengine Ark API keys](https://console.volcengine.com/ark/apiKey) |
| `openrouter` | [OpenRouter keys](https://openrouter.ai/settings/keys) |
| `xiaomi-mimo` | [Xiaomi MiMo Token Plan](https://platform.xiaomimimo.com/token-plan) |
| `novita` | [Novita key management](https://novita.ai/en/settings/key-management) |
| `fireworks` | [Fireworks API keys](https://fireworks.ai/api-keys) |
| `siliconflow` | [SiliconFlow global API keys](https://cloud.siliconflow.com/account/ak) |
| `siliconflow-CN` | [SiliconFlow China API keys](https://cloud.siliconflow.cn/account/ak) |
| `arcee` | [Arcee API key guide](https://docs.arcee.ai/other/create-your-first-api-key) |
| `moonshot` | [Kimi API platform keys](https://platform.kimi.ai/console/api-keys) or [Kimi Code membership console](https://www.kimi.com/code/console) |
| `zai` | [Z.ai model API](https://z.ai/model-api) |
| `stepfun` | [StepFun Open Platform](https://platform.stepfun.ai/) |
| `minimax`, `minimax-anthropic` | [MiniMax interface keys](https://platform.minimax.io/user-center/basic-information/interface-key) |
| `huggingface` | [Hugging Face tokens](https://huggingface.co/settings/tokens) |
| `modelscope` | [ModelScope API Keys](https://modelscope.cn/my/settings/token) |
| `deepinfra` | [DeepInfra API keys](https://deepinfra.com/dash/api_keys) |
| `together` | [Together API keys](https://api.together.ai/settings/api-keys) |
| `qianfan` | [Baidu Cloud access keys](https://console.bce.baidu.com/iam/#/iam/accesslist) |
| `anthropic` | [Anthropic API keys](https://console.anthropic.com/settings/keys) |
| `openmodel` | [OpenModel console](https://console.openmodel.ai/) ([authentication guide](https://docs.openmodel.ai/en/docs/getting-started/authentication)) |
| `openai-codex` | Official Sign in with ChatGPT via `codewhale auth chatgpt` (eligible ChatGPT plan or credits, Codewhale-owned tokens). The `openai` API-key route has separate billing. Legacy imported Codex credentials do not authorize this route. |
| `sglang`, `vllm` | Local OpenAI-compatible endpoints are keyless by default; configure a key only when the server requires one. |
| `ollama` | Local Ollama is keyless by default; configure a key only when the local server requires one. |
| `ollama-cloud` | Create an [Ollama API key](https://ollama.com/settings/keys), save it with `codewhale auth set --provider ollama-cloud`, or set `OLLAMA_CLOUD_API_KEY` / `OLLAMA_API_KEY` in that precedence order. |
| `sakana` | [Sakana AI API keys](https://console.sakana.ai/api-keys) ([get started](https://console.sakana.ai/get-started)) |
| `longcat` | [Meituan LongCat platform](https://longcat.chat/platform) |
| `opencode-go` | [OpenCode Go](https://opencode.ai/docs/go/) |
| `opencode-zen` | [OpenCode Zen](https://opencode.ai/docs/zen/) |
| `meta` | [Meta Model API](https://developer.meta.com/ai/) |
| `telecomjs` | [TelecomJS TokenHub](https://aigw.telecomjs.com/) |
| `xai` | [xAI Console](https://console.x.ai/) for an API key, Codewhale-owned device login, or explicitly consented read-only Grok CLI credentials. |
| `mistral` | [Mistral Console (la Plateforme)](https://console.mistral.ai/api-keys) |
| `google` | [Google AI Studio](https://aistudio.google.com/apikey) — Codewhale uses the official Gemini OpenAI-compatible endpoint and never reads Google OAuth files. |
| `edenai` | [Eden AI API keys](https://app.edenai.run/settings/api-keys) |
| `zenmux` | [ZenMux API keys](https://zenmux.ai/platform/pay-as-you-go) |
| `csdn` | [CSDN 星图 console](https://ai.csdn.net/workbench/api-key) — choose the Coding Plan key type for the `glm_for_coding` plan route; a general key bills metered. Docs: [Coding Plan](https://ai.csdn.net/coding-plan). |
| `concentrate` | [Concentrate dashboard](https://concentrate.ai/) → API Keys → Create API Key (a Universal API key); docs: [API introduction](https://concentrate.ai/docs/api-reference/introduction). BYOK only — the key stays in the local secret store. |
| `modelstudio-token-plan`, `modelstudio-token-plan-anthropic`, `modelstudio-coding-plan`, `modelstudio-coding-plan-anthropic` | [Alibaba Cloud Model Studio (Bailian console)](https://bailian.console.aliyun.com/) — create or copy a Model Studio API key. |
| `codewhale` | [Codewhale account settings](https://app.codewhale.net/settings?section=api) — create an API key with the `models:infer` scope, or run `codewhale account api-keys create --name <name> --use`. |
| `custom` | Set the named provider's `base_url` and `api_key_env` or `api_key`; no canonical vendor credential page exists. |

For Kimi, the official [quickstart](https://platform.kimi.ai/docs/overview)
directs users to sign in, open **API Keys**, create and copy a key, and keep it
secret. Codewhale links straight to that console and accepts the copied key.
It never probes or impersonates `kimi_cli`/`kimi_code_cli`; first-class Kimi
OAuth remains blocked on a vendor-registered Codewhale identity.

### Subscription sign-in accounts (ChatGPT, xAI)

Codewhale keeps one sign-in of its own per subscription provider. It is
separate from the browser, the ChatGPT or Grok apps, and the Codex or Grok
CLIs, so those can be signed in to a different account. A successful login
replaces the previous owned grant; there is no picker for multiple saved
ChatGPT accounts.

- **See which account is signed in.** `codewhale auth status --provider
  openai-codex` (or `--provider xai`) and `codewhale auth list` show an account
  label when the ID token contains an email. ChatGPT plan and workspace
  details appear only when those claims are present; a workspace label uses
  the first characters of its account ID. The label is display metadata, not
  proof of plan entitlement. A valid sign-in may have no email label. Status
  reads local credential state without contacting the issuer: an expired
  access token with a stored refresh token remains structurally usable, while
  missing or invalid storage, or an expired token without a refresh token,
  is reported unusable. The provider picker shows the label of the sign-in
  the route would use (for xAI this includes a consented Grok CLI import).
  Successful login reports the account when known and whether it replaced
  an earlier sign-in, in the TUI's selected language. No token is printed.
- **Reauthorize ChatGPT.** Run `/auth chatgpt` inside Codewhale to update the
  running session, or `codewhale auth chatgpt` from a shell and restart open
  Codewhale sessions. Ordinary login reuses the selected account/workspace's
  issued client ID and checks that the returned account is the same. The
  browser opens with `prompt=login`; `CODEWHALE_CHATGPT_OAUTH_NO_PROMPT=1`
  omits that parameter if the issuer refuses it.
- **Replace the ChatGPT account or workspace.** From a shell, run
  `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt`, then refresh its
  model catalog and restart open Codewhale sessions. If the browser holds the
  wrong account, sign out there or open the printed URL in a private window.
  Only a fully validated new grant replaces the active owned sign-in.
- **Switch xAI accounts.** Run `/auth xai-device` inside Codewhale, or
  `codewhale auth xai-device` from a shell and restart open sessions. The
  device page approves with whichever account the browser holds; sign out
  there or use a private window to approve another account. The login names
  the account it replaced, or says it is the same account as before.
- **Usage limits.** ChatGPT plan requests can fail with
  `subscription_sharing_usage_limit_exceeded` or
  `subscription_sharing_usage_unavailable`. These errors, and subscription
  quota errors from other providers, name the account that made the request
  when its label is known and offer recovery guidance. Codewhale does not
  retry these terminal errors or silently choose another billing route. An
  xAI route set to OAuth that fell back to an API key gets no sign-in
  guidance, since no sign-in was used.
- **ChatGPT credential authority.** The official ChatGPT plan route uses
  only its selected, verified Codewhale-owned grant.
  `OPENAI_CODEX_ACCESS_TOKEN`, `CODEX_ACCESS_TOKEN`, and consented Codex CLI
  tokens cannot override or authorize this route.

### External CLI credential consent

Credential files owned by another CLI are disabled by default. Without an
explicit grant, provider discovery, setup, routing, `auth status`, and doctor
do not stat, read, refresh, contact an identity provider for, or rewrite Codex,
Grok, Kimi, or future external credential files.

Codewhale currently supports exact-path, provider-scoped **read-only** grants
for the Codex CLI and Grok CLI. Codex grants remain available for legacy
credential inspection only; they cannot authorize the official ChatGPT plan
route. Use `codewhale auth chatgpt` for that route.

```bash
# Legacy Codex credential inspection; does not authorize ChatGPT plan requests.
codex login
codewhale auth external-consent --provider openai-codex --mode read-only

grok login
codewhale auth external-consent --provider xai --mode read-only

codewhale auth status --provider openai-codex
codewhale auth external-revoke --provider openai-codex
```

Pass `--path /absolute/path/to/auth.json` when the external CLI uses a custom
location. Consent persists the provider, external owner, exact absolute path,
and consent schema version. Later environment-variable changes do not redirect
that authority to a different file. Read-only grants never refresh, contact an
identity/discovery service, or rewrite the external file. Supported routes such
as xAI may use the external token after explicit selection. The official ChatGPT
plan route ignores imported Codex tokens and requires its own verified grant.
An expired external token fails with login guidance. Doctor reports structural
consent/config state without
opening credential files and is always non-mutating.

`managed` is reserved for a future provider-specific preservation adapter.
v0.9.1 rejects it before file or network I/O because no reviewed adapter can
yet preserve every unknown external schema field safely. Codewhale-started xAI
device login instead atomically activates a Codewhale-owned generation named
`$CODEWHALE_HOME/credentials/xai-auth-<generation>.json`, stores only that
validated basename in config, and revokes any Grok-file grant. Superseded
generations are cleaned only after the new config pointer commits.
Kimi remains API-key-only; external consent for Kimi is rejected.

The official DeepSeek Harness (`dsh`) is a third read-only credential owner:
`codewhale auth external-consent --provider deepseek --mode read-only` grants
exact-path read access to `DEEPSEEK_API_KEY` in `$DSH_HOME/.credentials.yaml`
(or `~/.dsh/.credentials.yaml`), which Codewhale never writes, refreshes, or
loads into the process environment. This is separate from the DSH *harness*
integration (`codewhale integrations dsh …`, see
[INTEGRATIONS_DSH.md](INTEGRATIONS_DSH.md)), which never touches credentials
in either direction: it pins Codewhale's route identity into a `--patch`
overlay and lets DSH resolve its own keys.

## Shipped Providers

| Provider ID | TOML table | Auth env | Base URL env and default | Default or static models | Notes |
| --- | --- | --- | --- | --- | --- |
| `deepseek` | `[providers.deepseek]` | `DEEPSEEK_API_KEY` | `CODEWHALE_BASE_URL` / `DEEPSEEK_BASE_URL`; default `https://api.deepseek.com/beta` | `deepseek-flash` (shipped default; V4.1 Flash, unversioned id), `deepseek-v4-pro`, `deepseek-v4-flash`, experimental `deepseek-v4-flash-vision-exp`; vision aliases `flash-vision`, `deepseek-v4flashvisionexp`; compatibility aliases `deepseek-chat`, `deepseek-reasoner` | First-class default. The live Pro backend is labeled `DeepSeek-V4-Pro-0813`; the callable API ID remains `deepseek-v4-pro`. Beta URL enables strict tool mode, chat prefix completion, and FIM completion. The documented V4 routes can use provider-native web search through a separate bounded Responses request; compatible custom endpoints do not inherit that capability. Set `https://api.deepseek.com` or `/v1` explicitly to opt out of beta-only features. The shipped default `deepseek-flash` speaks the Responses API (DeepSeek's documented path for Codex-style integration, since the 2026-07-31 Flash production update); the Chat-only controls (the `thinking` toggle and strict-tool `/beta` routing) apply to Chat Completions routes, and `deepseek-v4-pro` stays on Chat Completions until its announced Responses rollout. Reasoning effort follows DeepSeek's documented requested-to-actual mapping: `minimal`/`low` land on `low`, `medium`/`xhigh` on `high`, and `max`/`ultra` on `max`; `off` disables thinking (`thinking: {"type":"disabled"}` on Chat, `reasoning.effort: "none"` on Responses). The experimental vision ID was observed in the authenticated `/models` roster on 2026-08-21 and is advertised as image-input capable on the direct Chat Completions route only. Its limits, reasoning, and tool-call flags provisionally inherit Flash; pricing remains unknown, and no funded image round trip was made during this release work. |
| `deepseek-anthropic` | `[providers.deepseek_anthropic]` | `DEEPSEEK_API_KEY` | `DEEPSEEK_ANTHROPIC_BASE_URL`; default `https://api.deepseek.com/anthropic` | `deepseek-v4-pro`, `deepseek-v4-flash`; compatibility aliases `deepseek-chat`, `deepseek-reasoner` | Opt-in DeepSeek route for the Anthropic Messages wire protocol. Uses `/v1/messages`, `x-api-key`, and `anthropic-version: 2023-06-01`. Keep `provider = "deepseek"` for the default Chat Completions path. |
| `nvidia-nim` | `[providers.nvidia_nim]` | `NVIDIA_API_KEY`, `NVIDIA_NIM_API_KEY` | `NVIDIA_NIM_BASE_URL`, `NIM_BASE_URL`, `NVIDIA_BASE_URL`; default `https://integrate.api.nvidia.com/v1` | `deepseek-ai/deepseek-v4-pro`, `deepseek-ai/deepseek-v4-flash` | Hosted DeepSeek V4 through NVIDIA NIM. `NVIDIA_NIM_MODEL` is accepted by the TUI config path. |
| `openai` | `[providers.openai]` | `OPENAI_API_KEY` | `OPENAI_BASE_URL`; default `https://api.openai.com/v1` | `gpt-5.6` (default), `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` | Generic OpenAI-compatible route whose built-in endpoint and fallback catalog are native to OpenAI. The [GPT-5.6 family](https://developers.openai.com/api/docs/models/gpt-5.6-sol) uses OpenAI's documented 1.05M context, 128K max output, and reasoning levels. Custom gateways remain free to select an explicit gateway-owned model. `OPENAI_MODEL` is accepted. |
| `atlascloud` | `[providers.atlascloud]` | `ATLASCLOUD_API_KEY` | `ATLASCLOUD_BASE_URL`; default `https://api.atlascloud.ai/v1` | Default `deepseek-ai/deepseek-v4-flash`; explicit `vendor/model-id` values pass through when AtlasCloud is selected | OpenAI-compatible hosted route. `ATLASCLOUD_MODEL` is accepted by the TUI config path, the static `ModelRegistry` keeps DeepSeek V4 fallback rows, and provider-hinted CLI model IDs are sent to AtlasCloud exactly as requested. Use Atlas Cloud's own catalog or Coding Plan page for the current provider-owned model list and pricing. |
| `wanjie-ark` | `[providers.wanjie_ark]` | `WANJIE_ARK_API_KEY`, `WANJIE_API_KEY`, `WANJIE_MAAS_API_KEY` | `WANJIE_ARK_BASE_URL`, `WANJIE_BASE_URL`, `WANJIE_MAAS_BASE_URL`; default `https://maas-openapi.wanjiedata.com/api/v1` | `deepseek-reasoner` | OpenAI-compatible hosted route. `WANJIE_ARK_MODEL`, `WANJIE_MODEL`, and `WANJIE_MAAS_MODEL` are accepted. |
| `volcengine` | `[providers.volcengine]` | `VOLCENGINE_API_KEY`, `VOLCENGINE_ARK_API_KEY`, `ARK_API_KEY` | `VOLCENGINE_BASE_URL`, `VOLCENGINE_ARK_BASE_URL`, `ARK_BASE_URL`; default `https://ark.cn-beijing.volces.com/api/coding/v3` | `DeepSeek-V4-Pro`, `DeepSeek-V4-Flash` | Volcengine/Volcano Engine Ark OpenAI-compatible coding endpoint. `VOLCENGINE_MODEL` and `VOLCENGINE_ARK_MODEL` are accepted. |
| `openrouter` | `[providers.openrouter]` | `OPENROUTER_API_KEY` | `OPENROUTER_BASE_URL`; default `https://openrouter.ai/api/v1` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`; recent large IDs include `arcee-ai/trinity-large-thinking`, `minimax/minimax-m3`, `xiaomi/mimo-v2.5-pro`, `qwen/qwen3.6-flash`, `qwen/qwen3.6-35b-a3b`, `qwen/qwen3.6-max-preview`, `qwen/qwen3.6-27b`, `qwen/qwen3.6-plus`, `google/gemma-4-31b-it`, `z-ai/glm-5.1`, `z-ai/glm-5.2`, `moonshotai/kimi-k2.7-code`, `moonshotai/kimi-k2.6` | Additive open-model routing layer. It does not replace DeepSeek; it lets users route supported model IDs through OpenRouter when they choose it. |
| `orcarouter` | `[providers.orcarouter]` | `ORCAROUTER_API_KEY` | `ORCAROUTER_BASE_URL`; default `https://api.orcarouter.ai/v1` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`; router alias `orcarouter/auto`; recent large IDs mirror the OpenRouter namespaced catalog | [OrcaRouter](https://www.orcarouter.ai) OpenAI-compatible aggregation gateway. Shares the namespaced `vendor/model` wire-model format and DeepSeek model set with OpenRouter, so the OpenRouter base-URL and model-normalization rules apply. `ORCAROUTER_MODEL` is accepted. Provider aliases: `orcarouter`, `orca_router`, `orca`. |
| `xiaomi-mimo` | `[providers.xiaomi_mimo]` | `XIAOMI_MIMO_TOKEN_PLAN_API_KEY`, `MIMO_TOKEN_PLAN_API_KEY`, `XIAOMI_MIMO_API_KEY`, `XIAOMI_API_KEY`, `MIMO_API_KEY` | `XIAOMI_MIMO_BASE_URL`, `MIMO_BASE_URL`, `XIAOMI_MIMO_MODE`, `MIMO_MODE`; default `https://token-plan-sgp.xiaomimimo.com/v1` | Chat: `mimo-v2.5-pro`, `mimo-v2.5-pro-ultraspeed`, `mimo-v2.5`; speech/TTS: `mimo-v2.5-tts`, `mimo-v2.5-tts-voicedesign`, `mimo-v2.5-tts-voiceclone`, `mimo-v2-tts` | Xiaomi MiMo OpenAI-compatible chat completions route. `mimo-v2.5-pro` and `mimo-v2.5` can use the documented provider-native web-search plugin; ultraspeed, speech/TTS, and custom-compatible routes do not inherit it. Token Plan keys (`tp-...`) use `api-key` auth and the token-plan endpoint by default; pay-as-you-go mode uses standard API keys (`sk-...`) and `https://api.xiaomimimo.com/v1`. It sends `max_completion_tokens` and uses MiMo's `thinking` field for reasoning control. Token Plan cost/usage is credit/quota based; Codewhale shows it as unknown until Xiaomi exposes a reliable balance API. `codewhale speech` / `tts` uses the TTS models. |
| `novita` | `[providers.novita]` | `NOVITA_API_KEY` | `NOVITA_BASE_URL`; default `https://api.novita.ai/openai/v1` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash` | OpenAI-compatible hosted route for DeepSeek model IDs. Use config or `CODEWHALE_MODEL` / `DEEPSEEK_MODEL` for model overrides. |
| `fireworks` | `[providers.fireworks]` | `FIREWORKS_API_KEY` | `FIREWORKS_BASE_URL`; default `https://api.fireworks.ai/inference/v1` | `accounts/fireworks/models/deepseek-v4-pro` | OpenAI-compatible hosted route. Use config or `CODEWHALE_MODEL` / `DEEPSEEK_MODEL` for model overrides. |
| `siliconflow` | `[providers.siliconflow]` | `SILICONFLOW_API_KEY` | `SILICONFLOW_BASE_URL`; default `https://api.siliconflow.com/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | OpenAI-compatible hosted route. Official docs use the `.com` endpoint. `SILICONFLOW_MODEL` is accepted. Reasoning aliases `deepseek-reasoner` and `deepseek-r1` map to Pro; `deepseek-chat` and `deepseek-v3` map to Flash. |
| `siliconflow-CN` | `[providers.siliconflow_cn]` | `SILICONFLOW_API_KEY` | `SILICONFLOW_BASE_URL`; default `https://api.siliconflow.cn/v1` | Uses the SiliconFlow model set | China regional SiliconFlow route. Falls back to `[providers.siliconflow]` for api_key / base_url / model when unset. Select it with `provider = "siliconflow-CN"` or `CODEWHALE_PROVIDER=siliconflow-CN`. |
| `arcee` | `[providers.arcee]` | `ARCEE_API_KEY` | `ARCEE_BASE_URL`; default `https://api.arcee.ai/api/v1` | `trinity-large-thinking`, `trinity-large-preview` | Arcee AI direct OpenAI-compatible route, tracked as 256K-context BF16 serving. `ARCEE_MODEL` is accepted. OpenRouter's `arcee-ai/trinity-large-thinking` remains the OpenRouter namespaced model ID; direct Arcee uses the bare `trinity-large-thinking` ID. |
| `moonshot` | `[providers.moonshot]` | `MOONSHOT_API_KEY`, `KIMI_API_KEY` | `MOONSHOT_BASE_URL`, `KIMI_BASE_URL`; default `https://api.moonshot.ai/v1` | Direct Moonshot: `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.7-code-highspeed`, `kimi-k2.6`; Kimi Code membership: `k3`, `kimi-for-coding`, `kimi-for-coding-highspeed` at `https://api.kimi.com/coding/v1` | Moonshot/Kimi route. Exact direct `kimi-k3` routes use the documented Formula web-search tool/fiber loop; direct `kimi-k2.6` retains the built-in `$web_search` contract, and exact Kimi Code membership routes use their structured `/search` service. Adjacent paths, K2.7 direct models, and cross-product model IDs do not inherit native search. `kimi` and `kimi-k2` aliases select `kimi-k2.7-code`; `MOONSHOT_MODEL`, `KIMI_MODEL_NAME`, and `KIMI_MODEL` are accepted. Kimi thinking streams through `reasoning_content`; Codewhale keeps it in Thinking cells and replays it for thinking/tool-call continuity. For direct K3, use exact `base_url = "https://api.moonshot.ai/v1"` and `model = "kimi-k3"`; it is always-thinking and receives top-level `reasoning_effort = "low" | "high" | "max"` (`off` normalizes to `low`), uses only `max_completion_tokens`, and omits `temperature`/`top_p` per the [K3 quickstart](https://platform.kimi.ai/docs/guide/kimi-k3-quickstart). For Kimi Code K3, use a key from the [Kimi Code console](https://www.kimi.com/code/console), exact `base_url = "https://api.kimi.com/coding/v1"`, and bare `model = "k3"`; `off` becomes enabled `low`, while normal dispatched `auto` selects and sends a concrete Codewhale tier. Only an omitted reasoning setting leaves the provider default in control. That membership route defaults safely to 262,144 context tokens; the [Kimi Code model-tier table](https://www.kimi.com/code/docs/en/kimi-code/models.html) grants Allegretto and higher plans up to 1M, which those plans may express as `context_window = 1048576`. `k3[1m]` is Claude Code-only and Codewhale rejects it. `kimi-for-coding` remains the valid K2.7 membership route, and `kimi-for-coding-highspeed` is its own high-speed roster entry (262,144 context); membership ids are rejected on the direct platform endpoint, and `kimi-k3` stays rejected on the membership endpoint. Billing is decided by the endpoint the route resolves to, judged once against the two exact product endpoints: direct Moonshot (`https://api.moonshot.ai/v1` or the default) bills metered with dollar estimates, the exact Kimi Code membership endpoint bills as Kimi Code quota and never shows dollar estimates, and anything else — a gateway host, a neighboring Kimi-hosted path — reports `cost: unknown` rather than borrowing either product. An imported Kimi Code token with no `base_url` in its table still resolves to the membership endpoint, so it bills as Kimi Code quota and never accrues dollars. A completed turn, parent or sub-agent, is billed from the immutable endpoint receipt its own client was built with, never from a later config re-read: `MOONSHOT_BASE_URL`/`KIMI_BASE_URL` are merged into the *active* provider's table only, and an in-turn provider switch can move the ambient config off the route that actually ran. Legacy `auth_mode = "kimi_oauth"` fails to API-key guidance without probing Kimi CLI files. Codewhale does not impersonate `kimi_cli` or `kimi_code_cli`. **China-region keys:** contributor field evidence (@vFONGv, PR #5229, verified on Windows 10) reports that a China-region Moonshot key must be paired with `base_url = "https://api.moonshot.cn/v1"`; left on the default international host (`https://api.moonshot.ai/v1`) it fails authentication. We have no China-region key to verify this ourselves, so it is recorded as a user report rather than a tested route. Note also that editing `base_url` alone does not take effect until `codewhale auth set` is re-run for that provider. |
| `google` | `[providers.google]` | `GOOGLE_API_KEY`, `GEMINI_API_KEY` | `GOOGLE_BASE_URL`, `GEMINI_BASE_URL`; default `https://generativelanguage.googleapis.com/v1beta/openai/` | `gemini-3.1-pro-preview` (default); `/model` also lists `gemini-3-pro-preview`, `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-2.5-pro`, `gemini-2.5-flash` | Google Gemini as its own backend on the official OpenAI-compatible Chat Completions route. Thinking models capture `extra_content.google.thought_signature` on tool calls and replay it with the assistant tool-call messages; replaying a tool call whose signature was not captured fails closed with an actionable error instead of letting the tool loop break. `gemini-2.5-flash-lite` ships thinking off and degrades with a warning instead. Reasoning effort maps onto the documented `google.thinking_config.thinking_level` (`low`/`high`). The dialect binds to the exact official base URL: a `google` row pointed at another gateway gets plain OpenAI semantics and no signature requirements. Codewhale never reads Google OAuth files; only an AI Studio API key is used. Not live-tested against the real endpoint in this environment. |
| `zai` | `[providers.zai]` | `ZAI_API_KEY`, `Z_AI_API_KEY`, `ZHIPU_API_KEY`, `GLM_API_KEY` | `ZAI_BASE_URL`, `Z_AI_BASE_URL`; default `https://api.z.ai/api/coding/paas/v4`; general APIs `https://api.z.ai/api/paas/v4` and `https://open.bigmodel.cn/api/paas/v4` | `GLM-5.3` default; `/model` also lists `GLM-5.3-Flash`, `GLM-5.2`, `GLM-5.1`, and `GLM-5-Turbo` | Z.AI GLM Coding Plan route. All three first-party Chat routes (both api.z.ai products and BigModel's general platform endpoint) share one request dialect, so reasoning controls — the GLM-5.2 thinking toggle, tiered effort, and the forced-thinking GLM-5.3 rewrite that sends `off` as `enabled` + `reasoning_effort: "low"` — apply on BigModel too; neighboring paths such as `/preview` stay fail-closed. The two general API products expose structured provider-native web search (`search-prime` globally, `search_std` in China); Coding Plan and compatible custom endpoints do not inherit it. `GLM-5.3` is the default and a first-class picker row (`model = "GLM-5.3"` or `ZAI_MODEL=GLM-5.3`); `GLM-5.3-Flash` is the 1M multimodal fast sibling (`model = "GLM-5.3-Flash"`). An explicit `GLM-5.2` selection keeps its own id. Limits and reasoning options for 5.3 are inherited from `GLM-5.2` until Z.ai publishes distinct 5.3 metadata; 5.3 carries no price. Flash ships the published $0.15/$0.50 list. A live call can still 429 with entitlement code 1311 on accounts that are not provisioned for 5.3. |
| `stepfun` | `[providers.stepfun]` | `STEPFUN_API_KEY`, `STEP_API_KEY` | `STEPFUN_BASE_URL`, `STEP_BASE_URL`; default `https://api.stepfun.ai/v1`; Coding Plan endpoint `https://api.stepfun.ai/step_plan/v1` | `step-3.7-flash` | StepFun / StepFlash direct OpenAI-compatible route. `/provider` setup asks which billing route the key belongs to — pay-as-you-go or Step Plan — validates the key against the chosen endpoint, and writes the answer to `[providers.stepfun].base_url` only. A base URL that is neither recognized route is left alone and the question is skipped. You can also set `[providers.stepfun].base_url` or `STEP_BASE_URL` to the Coding Plan URL by hand. Offline accounting labels recognized routes as `stepfun-payg` or `stepfun-plan` without persisting the raw endpoint, and only the standard PAYG route receives token pricing. `STEPFUN_MODEL` and `STEP_MODEL` are accepted. |
| `minimax` | `[providers.minimax]` | `MINIMAX_API_KEY` | `MINIMAX_BASE_URL`; default `https://api.minimax.io/v1`; China `https://api.minimaxi.com/v1` | `MiniMax-M3`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed`, `MiniMax-M2.5`, `MiniMax-M2.5-highspeed`, `MiniMax-M2.1`, `MiniMax-M2.1-highspeed`, `MiniMax-M2` | MiniMax direct OpenAI-compatible route. Codewhale sends `reasoning_split = true` so MiniMax thinking arrives separately from answer text. Both MiniMax dialects sell pay-as-you-go and Token Plan over the same endpoints and the same key, so billing is classified from the credential *product*, never from the endpoint or from a default. `mode = "token-plan"` in `[providers.minimax]`/`[providers.minimax_anthropic]`, or a Token Plan key shaped `sk-cp…`, bills as MiniMax Token Plan quota with no dollar estimates; an explicit pay-as-you-go mode (`pay-as-you-go`/`payg`/`metered`) wins over key shape. The key's product prefix is only visible when the key is in config, bound by `api_key_env`, or exported as `MINIMAX_API_KEY` on an official endpoint — a key saved through `codewhale auth set` (secret store / OS keyring) is deliberately not read to classify billing. With no explicit mode and no visible product marker the route reports `cost: unknown` rather than assuming pay-as-you-go, so a Token Plan account is never charged invented dollars. Custom/gateway endpoints also fail closed with `cost: unknown`. Official M3 input modalities are text, image, and video; M2.7 is text-only. |
| `minimax-anthropic` | `[providers.minimax_anthropic]` | `MINIMAX_API_KEY` | `MINIMAX_ANTHROPIC_BASE_URL`; default `https://api.minimax.io/anthropic`; China `https://api.minimaxi.com/anthropic` | `MiniMax-M3`, `MiniMax-M2.7` | MiniMax direct Anthropic-compatible Messages route. Keep the `/anthropic` suffix because Codewhale appends `/v1/messages`; the route uses `x-api-key`. M3 supports adaptive or disabled thinking. M2.7 always keeps thinking enabled. |
| `sglang` | `[providers.sglang]` | Optional `SGLANG_API_KEY` | `SGLANG_BASE_URL`; default `http://localhost:30000/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | Self-hosted OpenAI-compatible route. Localhost deployments commonly omit auth. `SGLANG_MODEL` is accepted. |
| `vllm` | `[providers.vllm]` | Optional `VLLM_API_KEY` | `VLLM_BASE_URL`; default `http://localhost:8000/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | Self-hosted vLLM OpenAI-compatible route. Localhost deployments commonly omit auth. `VLLM_MODEL` is accepted. |
| `ollama` | `[providers.ollama]` | Local optional `OLLAMA_API_KEY` | `OLLAMA_BASE_URL`; default `http://localhost:11434/v1` | live tag from the local catalog; pre-refresh placeholder `unknown`; provider-hinted custom tags pass through | Local Ollama is keyless by default. `OLLAMA_MODEL` is accepted. The header must not paint a hosted id the local daemon did not list. |
| `ollama-cloud` | `[providers.ollama_cloud]` | `OLLAMA_CLOUD_API_KEY`, then `OLLAMA_API_KEY` | `OLLAMA_CLOUD_BASE_URL`; default `https://ollama.com/v1` | `gpt-oss:120b`; arbitrary provider-owned IDs pass through | Hosted OpenAI-compatible `/v1/chat/completions` route. Save credentials under `ollama-cloud`; the exact released `ollama` + Cloud URL tuple has bounded read-only in-memory compatibility with its legacy table and secret slot. `OLLAMA_CLOUD_MODEL` is accepted. |
| `huggingface` | `[providers.huggingface]` | `HUGGINGFACE_API_KEY`, `HF_TOKEN` | `HUGGINGFACE_BASE_URL`, `HF_BASE_URL`; default `https://router.huggingface.co/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | Hugging Face Inference Providers OpenAI-compatible router route. Accepted aliases: `huggingface`, `hugging-face`, `hugging_face`, `hf`. Org-prefixed model IDs pass through. `HUGGINGFACE_MODEL` and `HF_MODEL` are accepted. Hub browsing/export are separate future features. |
| `modelscope` | `[providers.modelscope]` | `MODELSCOPE_API_KEY` | `MODELSCOPE_BASE_URL`; default `https://api-inference.modelscope.cn/v1` | `Qwen/Qwen3.5-397B-A17B` (default), `Qwen/Qwen3.5-122B-A10B`, `Qwen/Qwen3.5-27B`, `Qwen/Qwen3.5-35B-A3B`, `Qwen/Qwen3.8-27B`, `Qwen/Qwen3.8-Flash-Next`, `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Pro-0813`, `deepseek-ai/DeepSeek-V4.1-Flash`, `ZhipuAI/GLM-4.7-Flash`, `ZhipuAI/GLM-5.2` | ModelScope OpenAI-compatible inference route. Org-prefixed model IDs pass through. `MODELSCOPE_MODEL` is accepted. |
| `deepinfra` | `[providers.deepinfra]` | `DEEPINFRA_API_KEY`, `DEEPINFRA_TOKEN` | `DEEPINFRA_BASE_URL`; default `https://api.deepinfra.com/v1/openai` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | DeepInfra OpenAI-compatible route. Drop-in replacement for OpenAI SDK. |
| `together` | `[providers.together]` | `TOGETHER_API_KEY` | `TOGETHER_BASE_URL`; default `https://api.together.xyz/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash`, `thinkingmachines/inkling` | Together AI OpenAI-compatible route. `TOGETHER_MODEL` is accepted. Model aliases `deepseek-v4-pro` and `deepseek-v4-flash` normalize to Together's org-prefixed IDs; `inkling` and `together-inkling` normalize to Together's published lowercase Inkling wire ID. Inkling uses the exact `none`/`minimal`/`low`/`medium`/`high`/`max` reasoning vocabulary from Thinking Machines' [official model repository](https://huggingface.co/thinkingmachines/Inkling). Together's [launch post](https://www.together.ai/blog/together-ai-brings-thinking-machines-labs-new-model-inkling-on-day-0) currently says Inkling is live with 1M context, while its [model detail page](https://www.together.ai/models/inkling) says coming soon with 256K context and publishes no price. Until Together's active `/models` endpoint and the Models.dev catalog resolve that conflict, Inkling is not seeded into Codewhale's offline picker and no route-specific context or cost is inferred. |
| `qianfan` | `[providers.qianfan]` | `QIANFAN_API_KEY`, `BAIDU_QIANFAN_API_KEY` | `QIANFAN_BASE_URL`, `BAIDU_QIANFAN_BASE_URL`; default `https://api.baiduqianfan.ai/v1` | `ernie-4.0-turbo-8k`; provider-scoped custom Qianfan service/model IDs pass through | Baidu Qianfan OpenAI-compatible route. Requests use Bearer auth and Chat Completions payloads. `QIANFAN_MODEL` and `BAIDU_QIANFAN_MODEL` are accepted; aliases `baidu-qianfan`, `baidu_qianfan`, and `baidu` resolve to this provider. Tool/function calling is model-scoped in Qianfan docs, so Codewhale preserves the selected wire model and leaves live capability proof to follow-up route/capability work. |
| `openai-codex` | `[providers.openai_codex]` | Official Sign in with ChatGPT (`codewhale auth chatgpt` / `/provider setup openai-codex`) | Official `https://api.openai.com/v1` | Selected account catalog; configured model IDs remain explicit selections | **Experimental.** Public Responses endpoint (`/v1/responses`) with a validated `chatgpt.tokens.use.direct` grant. Dynamic OSS registration issues a client ID for the selected account/workspace; Codewhale protects and renews its own tokens. The account catalog supplies selectable models; public model lists and legacy Codex tokens do not establish plan permission. No silent billing fallback. See [Sign in with ChatGPT](#sign-in-with-chatgpt) for setup, usage limits, and preview boundaries. |
| `anthropic` | `[providers.anthropic]` | `ANTHROPIC_API_KEY` | `ANTHROPIC_BASE_URL`; default `https://api.anthropic.com` | `claude-opus-4-8`, `claude-sonnet-4-6` (default), `claude-haiku-4-5` | Native Anthropic Messages API route (`/v1/messages`, `x-api-key` + `anthropic-version: 2023-06-01`) — not OpenAI-compatible. Prompt caching via `cache_control` breakpoints, adaptive thinking + `output_config.effort`, signed thinking blocks replayed verbatim, cache telemetry normalized per #2961. `ANTHROPIC_MODEL` is accepted. |
| `openmodel` | `[providers.openmodel]` | `OPENMODEL_API_KEY` | `OPENMODEL_BASE_URL`; default `https://api.openmodel.ai` | `deepseek-v4-flash`; provider-scoped custom model IDs pass through | OpenModel Anthropic-compatible Messages route. Uses `/v1/messages`, Bearer auth, and `anthropic-version: 2023-06-01`; OpenModel selects DeepSeek, DashScope, Xiaomi, Claude, and other routes by model id. `OPENMODEL_MODEL` is accepted. |
| `sakana` | `[providers.sakana]` | `FUGU_API_KEY`, `SAKANA_API_KEY` | `SAKANA_BASE_URL`; default `https://api.sakana.ai/v1` | `fugu` (default), `fugu-ultra-20260615` | Sakana AI Fugu OpenAI-compatible route. Standard Chat Completions wire protocol; streaming supported. `fugu-ultra-20260615` is the heavy/reasoning variant. Env var aliases: `FUGU_API_KEY` (primary), `SAKANA_API_KEY`; provider aliases: `sakana-ai`, `sakana_ai`, `fugu`. |
| `longcat` | `[providers.longcat]` | `LONGCAT_API_KEY` | `LONGCAT_BASE_URL`; default `https://api.longcat.chat/openai/v1` | `LongCat-2.0` (default) | Meituan LongCat curated model gateway. OpenAI-compatible Chat Completions wire protocol. Sign up at https://longcat.chat/platform for an API key. Provider aliases: `long-cat`, `meituan-longcat`, `meituan`. |
| `opencode-go` | `[providers.opencode_go]` | `OPENCODE_GO_API_KEY` | `OPENCODE_GO_BASE_URL`; default `https://opencode.ai/zen/go/v1` | `deepseek-v4-pro` (default), `grok-4.5`, `glm-5.2`, `glm-5.1`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `deepseek-v4-flash`, `mimo-v2.5`, `mimo-v2.5-pro` | [OpenCode Go](https://opencode.ai/docs/go/) subscription route using OpenAI-compatible Chat Completions. `OPENCODE_GO_MODEL` is accepted. Codewhale uses bare wire IDs; familiar `opencode-go/<model-id>` input aliases normalize to the bare ID. Go models documented only on the Anthropic `/messages` endpoint are deliberately not advertised by this route until Codewhale supports per-model wire selection. Billing surfaces show the Go allowance instead of token-price estimates. |
| `opencode-zen` | `[providers.opencode_zen]` | `OPENCODE_ZEN_API_KEY`, fallback `OPENCODE_API_KEY` | `OPENCODE_ZEN_BASE_URL`; default `https://opencode.ai/zen/v1` | `gpt-5.6` (default); current GPT, Claude, Qwen, DeepSeek, MiniMax, GLM, Kimi, Grok, Muse Spark, and free-model IDs | [OpenCode Zen](https://opencode.ai/docs/zen/) model-aware gateway. `OPENCODE_ZEN_MODEL` is accepted, and official `opencode/<model-id>` selectors normalize to bare wire IDs. Each model's wire comes from the curated snapshot, then from the AI SDK package its [Models.dev](https://models.dev) row names: GPT, Grok, and Muse Spark rows use `/responses`; Claude and most Qwen rows use `/messages`; DeepSeek, MiniMax, GLM, Kimi, `qwen3.8-max`, and the free rows use `/chat/completions`. Responses and Chat Completions authenticate with Bearer `Authorization`, while Anthropic Messages uses `x-api-key`; none of these routes use ChatGPT/Codex OAuth guidance or headers. Gemini fails closed because its model-specific Google wire protocol is not implemented; Models.dev rows marked `deprecated` and models no loaded catalog lists also fail closed. See [OpenCode Zen protocol catalog](#opencode-zen-protocol-catalog). |
| `meta` | `[providers.meta]` | `META_MODEL_API_KEY`, `MODEL_API_KEY` | `META_MODEL_API_BASE_URL`, `MODEL_API_BASE_URL`; default `https://api.meta.ai/v1` | `muse-spark-1.2` (default) | [Meta Model API](https://developer.meta.com/ai/resources/blog/build-with-muse-spark/) public-preview route using OpenAI-compatible Chat Completions. Muse Spark 1.2 keeps its wire ID, tool support, 1M context, 32K output metadata, and `none` through `xhigh` reasoning effort. `META_MODEL_API_MODEL` and `MODEL_API_MODEL` are accepted. Provider aliases: `meta-ai`, `meta_model_api`, `muse`, `muse-spark`. |
| `telecomjs` | `[providers.telecomjs]` | `TELECOMJS_API_KEY` | `TELECOMJS_BASE_URL`; default `https://aigw.telecomjs.com/v1` | `deepseek-v4-pro` conservative fallback; authenticated `/models` rows when a key is configured | TelecomJS TokenHub OpenAI-compatible Chat Completions route. Live catalogs are isolated by provider and key fingerprint, stale rows survive transient refresh failures, and unsupported reasoning request fields are omitted. `TELECOMJS_MODEL` is accepted. Provider aliases: `telecom-js`, `telecom_js`, `telecomjs-cn`, `tokenhub`. |
| `mistral` | `[providers.mistral]` | `MISTRAL_API_KEY` | `MISTRAL_BASE_URL`; default `https://api.mistral.ai/v1` | `mistral-code-latest` (default; `codestral-latest` accepted as alias), `mistral-medium-latest` (aliases: `mistral-medium-3-5`), `mistral-small-latest` (aliases: `mistral-small-2603`), `mistral-large-latest` | Mistral AI (la Plateforme) OpenAI-compatible Chat route. On the documented first-party HTTPS `/v1` hosts, Medium and Small send adjustable `reasoning_effort` (`none` or `high` only), parse Mistral's polymorphic thinking/text blocks, and replay stored thinking in that same wire shape. Deprecated native Magistral IDs remain explicit-configuration compatibility routes: they are always-reasoning and never receive the adjustable effort field. Code and Large are non-reasoning. A custom `MISTRAL_BASE_URL` keeps generic Chat semantics unless it is one of the documented first-party hosts. `MISTRAL_MODEL` is accepted. Provider aliases: `mistral-ai`, `mistralai`, `la-plateforme`. |
| `edenai` | `[providers.edenai]` | `EDENAI_API_KEY` | `EDENAI_BASE_URL`; default `https://api.edenai.run/v3`; EU `https://api.eu.edenai.run/v3` | `deepseek/deepseek-v4-pro` (default); live `/models` catalog of `provider/model` ids | Eden AI OpenAI-compatible aggregation gateway. Catalog rows remain provider-scoped; generic reasoning controls are omitted because supported fields depend on the selected upstream family. `EDENAI_MODEL` is accepted. The default `deepseek/deepseek-v4-pro` is listed on the global catalog only; on the EU endpoint set `EDENAI_MODEL` (or `model`) to a row from the EU `/models` list, for example `qwen/deepseek-v4-pro`. Provider aliases: `eden-ai`, `eden_ai`. |
| `zenmux` | `[providers.zenmux]` | `ZENMUX_API_KEY` | `ZENMUX_BASE_URL`; default `https://zenmux.ai/api/v1` | `deepseek/deepseek-v4.1-flash` (default); live `/models` catalog of `provider/model` ids (keyless-readable) | ZenMux OpenAI-compatible aggregation gateway (~190 models). Catalog rows remain provider-scoped; generic reasoning controls are omitted because supported fields depend on the selected upstream family. `ZENMUX_MODEL` is accepted. Provider aliases: `zen-mux`, `zen_mux`. |
| `csdn` | `[providers.csdn]` | `CSDN_API_KEY` | `CSDN_BASE_URL`; default `https://ai.csdn.net/api/model/v1` | `glm_for_coding` (default; the Coding Plan's dedicated model id); other marketplace model ids pass through | CSDN 星图 (Starmap) OpenAI-compatible hosted platform. Coding Plan keys and general marketplace keys share the one endpoint, so billing follows the credential product, never the URL alone: routing `glm_for_coding` — the shipped default — or setting `mode = "coding_plan"`/`"plan"`/`"subscription"` bills as CSDN Coding Plan quota with no dollar estimates; any other model, or an explicit `pay-as-you-go`/`metered` mode, bills metered; an unrecognized mode or an endpoint off `ai.csdn.net/api/model/v1` reports `cost: unknown`. `CSDN_MODEL` is accepted. Provider aliases: `csdn-ai`, `csdn_ai`, `csdn-coding-plan`, `csdn_coding_plan`, `starmap`. |
| `concentrate` | `[providers.concentrate]` | `CONCENTRATE_API_KEY` | `CONCENTRATE_BASE_URL`; default `https://api.concentrate.ai/v1` | `deepseek-v4-pro` (default; a plain catalog id lets the gateway pick the upstream provider); `provider/model` ids such as `openai/gpt-5.6-sol` pin one upstream; `concentrate/auto` is the gateway's own router; unauthenticated live `/v1/models` catalog | Concentrate OpenAI Responses-compatible gateway (`POST /v1/responses`, bearer Universal API key). Opt-in and BYOK only: your key, your Concentrate bill, zero Codewhale fee, no managed default. Requests carry only documented fields (the system prompt rides as a `system` input item). Contract: [API introduction](https://concentrate.ai/docs/api-reference/introduction), [request parameters](https://concentrate.ai/docs/api-reference/endpoint/request-parameters), [streaming](https://concentrate.ai/docs/api-reference/endpoint/streaming), [errors](https://concentrate.ai/docs/api-reference/endpoint/errors). See [Concentrate Notes](#concentrate-notes). |
| `codewhale` | `[providers.codewhale]` | `CODEWHALE_API_KEY` | `CODEWHALE_API_BASE`; default `https://api.codewhale.net/v1` | `deepseek/deepseek-v4-pro` (default), `anthropic/claude-sonnet-5`, `openai/gpt-5.6` are offline bootstrap rows only; the authenticated `GET /v1/models` listing of the account's connected providers is the catalog authority | Codewhale API: account-backed model access over the provider keys the customer connected to their Codewhale account. One `cwc_key_…` account API key with the `models:infer` scope authenticates every model. Model ids are `provider/model` exactly as the account catalog returns them, and each row states its protocol (`chat-completions` → `/v1/chat/completions`, `anthropic-messages` → `/v1/messages`, `responses` → `/v1/responses`); every protocol uses `Authorization: Bearer`, never `x-api-key`. `CODEWHALE_API_BASE` must be HTTPS except on loopback. Connect provider keys with `codewhale account keys set <provider>`. Provider aliases: `codewhale-api`, `cw-api`, `codewhale-cloud`. |
| `xai` | `[providers.xai]` | `XAI_API_KEY`, Codewhale-owned device OAuth, or explicit read-only Grok CLI consent | `XAI_BASE_URL`; default `https://api.x.ai/v1` | `grok-4.6` (default), `grok-4.7`, `grok-4.5`, `grok-4.3`, `grok-build`, `grok-composer-2.5-fast`, `grok-4.20-0309-reasoning`, `grok-4.20-0309-non-reasoning` | xAI/Grok OpenAI-compatible Chat Completions route. Grok 4.6 has a 500K context window, text/image input, function calls, structured output, server-side web search, and `low`/`medium`/`high`/`xhigh` reasoning (default `high`). [Grok 4.7](https://docs.x.ai/docs/models/grok-4.7) has the same 500K window, text/image input, function calls, structured output, reasoning ladder and $2.00 / $0.50 cached / $6.00 rates; xAI documents no web-search support for it yet, so Codewhale does not claim it. Grok reasoning cannot be disabled ([reasoning guide](https://docs.x.ai/docs/guides/reasoning)). Its standard rates double when the prompt reaches 200K tokens; the same 2x long-context rule applies to `grok-4.5` (500K context, $2.00 / $0.30 cached / $6.00) and `grok-4.3` (1M context, $1.25 / $0.20 cached / $2.50) per their [model pages](https://docs.x.ai/docs/models/grok-4.5). There is no documented `latest`/`fast` alias and no published numeric output limit. **API-key** (default): Bearer token from console.x.ai via `XAI_API_KEY` / keyring / `api_key`. **OAuth**: `codewhale auth xai-device` uses SSH-friendly device login and Codewhale-owned storage, which may refresh itself. Existing Grok CLI credentials require `codewhale auth external-consent --provider xai --mode read-only`; the granted external file is never refreshed or rewritten. OAuth may return HTTP 403 on some SuperGrok tiers — keep API-key as the reliable fallback. `XAI_MODEL` is accepted. Provider aliases: `x-ai`, `x_ai`, `grok`. |
| `modelstudio-token-plan` | `[providers.modelstudio_token_plan]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | `MODELSTUDIO_TOKEN_PLAN_BASE_URL`; default `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | `qwen3.8-max` (default), `qwen3.8-max-preview`, `qwen3.7-plus`, `qwen3.7-max`, `qwen3.6-flash`, `deepseek-v4-pro`, `deepseek-v4-flash-0731`, `glm-5.2` | Alibaba Cloud Model Studio Token Plan OpenAI-compatible Chat Completions route. Token Plan Personal and Team share this endpoint. `qwen3.8-max`, `qwen3.7-plus`, and `qwen3.7-max` can use provider-native web search through the Token Plan Responses Harness; the preview, Coding Plan, and Anthropic routes do not inherit that capability. All listed models are reasoning-capable text/coding models. DeepSeek and GLM entries are provider-scoped and do not collide with first-party routes. `MODELSTUDIO_TOKEN_PLAN_MODEL` is accepted. Provider aliases: `modelstudio-token-plan`, `alibaba-token-plan`, `dashscope-token-plan`. |
| `modelstudio-token-plan-anthropic` | `[providers.modelstudio_token_plan_anthropic]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | default `https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic` | Same model catalog as `modelstudio-token-plan` | Token Plan Anthropic-compatible Messages route (`/apps/anthropic`). Same API key as the OpenAI dialect. Provider aliases: `modelstudio-token-plan-anthropic`, `alibaba-token-plan-anthropic`. |
| `modelstudio-coding-plan` | `[providers.modelstudio_coding_plan]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | `MODELSTUDIO_CODING_PLAN_BASE_URL`; default `https://coding-intl.dashscope.aliyuncs.com/v1` | `qwen3.8-max` (default); same catalog as Token Plan | Alibaba Cloud Model Studio Coding Plan OpenAI-compatible Chat Completions route. `MODELSTUDIO_CODING_PLAN_MODEL` is accepted. Provider aliases: `modelstudio-coding-plan`, `alibaba-coding-plan`, `dashscope-coding-plan`. |
| `modelstudio-coding-plan-anthropic` | `[providers.modelstudio_coding_plan_anthropic]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | default `https://coding-intl.dashscope.aliyuncs.com/apps/anthropic` | Same model catalog as `modelstudio-coding-plan` | Coding Plan Anthropic-compatible Messages route (`/apps/anthropic`). Provider aliases: `modelstudio-coding-plan-anthropic`, `alibaba-coding-plan-anthropic`. |

StepFun's four coding models are available through both its standard API and
[Step Plan](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api).
Choose the billing route in `/provider`, then the model in `/model`; existing
Step 3.7 selections remain unchanged. [Step 5 Preview](https://platform.stepfun.ai/docs/en/guides/models/step-5-preview)
has a 1M context window. Step 5 and Step 3.7 expose low/medium/high reasoning;
Step 3.5 Flash 2603 exposes low/high; base Step 3.5 uses provider-default reasoning.
[Published API prices](https://platform.stepfun.ai/docs/en/guides/pricing/details)
apply only to verified PAYG routes. Step Plan displays subscription allowance.
Speech, music and image-generation models use separate interfaces and are not
presented as coding models. Provider video capability metadata does not imply
that every Codewhale client can attach video.


### OpenCode Zen protocol catalog

Zen Responses and Chat Completions requests authenticate with Bearer
`Authorization`; Zen Anthropic Messages requests use `x-api-key`. None of these
routes add ChatGPT/Codex OAuth headers.

Each Zen model's wire comes from two sources, and neither is guessed from a
model-id family (`qwen3.8-flash` is Messages while `qwen3.8-max` is Chat):

1. **The curated snapshot** compiled into Codewhale, verified against the
   [official endpoint table](https://opencode.ai/docs/zen/) and the `opencode`
   provider in [Models.dev](https://models.dev). It is the offline floor, and
   it wins when a catalog row names a different wire for the same id.
2. **The Models.dev catalog.** Its `opencode` provider is Zen's published
   catalog: each model row names the AI SDK package OpenCode itself uses, which
   Codewhale maps exactly — `@ai-sdk/openai` to Responses, `@ai-sdk/anthropic`
   to Anthropic Messages, and `@ai-sdk/openai-compatible` (the provider
   default) to Chat Completions. A Zen model released after this build routes
   once the catalog lists it, without a Codewhale release. The interactive TUI
   and `codewhale exec` both load the persisted catalog; refresh it with
   `codewhale models --update`.

The curated snapshot:

- Responses: `gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`,
  `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.5-pro`, `gpt-5.4`,
  `gpt-5.4-pro`, `gpt-5.4-mini`, `gpt-5.4-nano`, `gpt-5.3-codex`,
  `gpt-5.3-codex-spark`, `gpt-5.2`, `gpt-5.2-codex`, `gpt-5.1`,
  `gpt-5.1-codex`, `gpt-5.1-codex-max`, `gpt-5.1-codex-mini`, `gpt-5`,
  `gpt-5-codex`, `gpt-5-nano`, `grok-4.7`, `grok-4.6`, `grok-4.5`,
  `grok-build-0.1`, `muse-spark-1.3`, `muse-spark-1.3-contributor-free`,
  `muse-spark-1.2`, `muse-spark-1.2-contributor`,
  `muse-spark-1.2-contributor-free`.
- Anthropic Messages: `claude-fable-5-1`, `claude-fable-5`,
  `claude-opus-5-5`, `claude-opus-5`, `claude-opus-4-8`, `claude-opus-4-7`,
  `claude-opus-4-6`, `claude-opus-4-5`, `claude-sonnet-5-5`,
  `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-sonnet-4-5`,
  `claude-sonnet-4`, `claude-haiku-4-5`, `qwen3.8-flash`, `qwen3.7-max`,
  `qwen3.7-plus`, `qwen3.6-plus`, `qwen3.5-plus`.
- Chat Completions: `deepseek-v4.1-flash`, `deepseek-v4-pro`,
  `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`, `minimax-m3`,
  `minimax-m2.7`, `minimax-m2.5`, `glm-5.3-flash`, `glm-5.3`, `glm-5.2`,
  `glm-5.1`, `glm-5`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k2.5`,
  `qwen3.8-max`, `big-pickle`, `space-bunny-free`,
  `longcat-2.5-preview-free`, `mimo-v2.6-flash-free`, `mimo-v2.5-free`,
  `ling-3.0-flash-fin-free`, `north-mini-code-free`,
  `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`,
  `deepseek-v4-flash-free`.

These fail closed locally, with the reason in the error, instead of reaching
Zen: Gemini (`@ai-sdk/google`, Google's model-specific protocol, which Codewhale
does not speak); a catalog row naming any other package; a catalog row Models.dev
marks `deprecated`; and a model neither source lists. A miss never falls back to
another Zen wire shape, including when a custom Zen base URL is configured.

### Concentrate Notes

Concentrate (`concentrate`) is an OpenAI **Responses**-compatible gateway:
`POST https://api.concentrate.ai/v1/responses` with `Authorization: Bearer
<Universal API key>`. It is opt-in and **BYOK only**.

- **Model ids pass through.** A plain catalog id (`gpt-5.6-sol`,
  `deepseek-v4-pro`) lets the gateway choose the upstream provider;
  `provider/model` (`openai/gpt-5.6-sol`) pins the upstream provider inside the
  gateway; `concentrate/auto` sends the gateway's own `auto` router. Codewhale's
  bare `--model auto` remains the resolver sentinel (provider default), which is
  why the gateway router has its own explicit spelling. Once streaming begins
  the gateway commits to one provider ([auto routing](https://concentrate.ai/docs/api-reference/endpoint/auto-routing)).
- **Wire.** Fixed Responses dialect. The request carries only documented
  fields — `model`, `input`, `stream`, `max_output_tokens`, `tools`,
  `tool_choice`, `parallel_tool_calls`, `reasoning.effort` — and the system
  prompt travels as a leading `system` input item because `instructions` is
  not in the gateway's [parameter reference](https://concentrate.ai/docs/api-reference/endpoint/request-parameters).
  Streaming follows the typed `response.*` SSE events and ends on
  `response.completed` without a `[DONE]` sentinel ([streaming](https://concentrate.ai/docs/api-reference/endpoint/streaming)).
- **Errors.** `{ "error", "message", "model"?, "retry_after"? }` with 400
  (bad request / unknown model), 401 (invalid key), 402 (insufficient
  credits), 424 (upstream provider unavailable), 429 (rate limit) —
  surfaced verbatim and classified by message ([errors](https://concentrate.ai/docs/api-reference/endpoint/errors)).
- **Catalog.** `GET /v1/models` needs no key
  ([list models](https://concentrate.ai/docs/api-reference/endpoint/list-models)); the live rows
  are provider-scoped and unclaimed (no pricing or capability claims).
- **Commercial boundary.** Concentrate's [Terms of Service](https://concentrate.ai/legal/terms-of-service)
  forbid reselling, white-labeling, or providing the service on a
  service-bureau basis without written consent, and its
  [Acceptable Use Policy](https://concentrate.ai/legal/acceptable-use-policy)
  forbids sharing or sublicensing keys. Codewhale therefore ships **no**
  Codewhale-owned Concentrate key, no stored customer key, no default or
  managed routing, and no markup — activation of any hosted lane is gated on
  written consent, terms, and billing approval (tracked in the Codewhale ops
  evidence checklist `concentrate-gateway-20260829/CHECKLIST.md`). Public
  pricing states "No platform markup on tokens".
- **Self-test without a key.** `scripts/concentrate-selftest.sh` boots a
  local stub that speaks the documented contract and drives the real
  `codewhale exec` path through it, asserting the URL, bearer header, model
  passthrough, streaming, and the completed-turn receipt. No network call
  leaves the machine and no account is required.

### Hugging Face Provider vs MCP vs Hub

Codewhale's `huggingface` provider ID is only the OpenAI-compatible chat
inference route through Hugging Face Inference Providers. It is selected with
`/provider huggingface`, `CODEWHALE_PROVIDER=huggingface`, or
`provider = "huggingface"`.

Hugging Face MCP is a separate external-tool route. Configure it through the
MCP config described in `docs/MCP.md`, preferably using the settings-generated
snippet from <https://huggingface.co/settings/mcp>. In the TUI, `/hf mcp status`
checks whether the Hugging Face MCP server appears in the resolved MCP config,
`/hf mcp setup` prints the settings workflow and a placeholder-only shape, and
`/hf concepts` explains the provider/MCP/Hub distinction.

Hub publishing or repository management remains explicit user action through
Hub-native tooling such as `huggingface_hub` or git. The `/hf` helper does not
upload to Hugging Face and does not perform direct Hugging Face Hub HTTP search.

### Xiaomi MiMo Notes

`xiaomi-mimo` defaults to `mimo-v2.5-pro` for long-context reasoning and coding
work. The chat picker also exposes `mimo-v2.5-pro-ultraspeed` and the latest
Omni model `mimo-v2.5`. Xiaomi MiMo TTS is available through
`codewhale --provider xiaomi-mimo speech "text" --model tts` (or the `tts`
alias). In Act and Operate, the provider-specific `speech` / `tts` tools are
available through deferred discovery when the Xiaomi MiMo route is configured.

`/provider xiaomi-mimo ultraspeed` and `/provider xiaomi-mimo pro-ultraspeed`
both select `mimo-v2.5-pro-ultraspeed`. Speech aliases such as `tts`,
`voice-design`, and `voice-clone` are separate from normal chat defaults.

Token Plan keys default to the Singapore endpoint
`https://token-plan-sgp.xiaomimimo.com/v1`. If your MiMo account is provisioned
for the China region, set `base_url = "https://token-plan-cn.xiaomimimo.com/v1"`
explicitly in `[providers.xiaomi_mimo]` or set `mode = "token-plan-cn"`. Europe
Token Plan accounts can set
`base_url = "https://token-plan-ams.xiaomimimo.com/v1"` or use
`mode = "token-plan-ams"`; `mode = "pay-as-you-go"`
selects the standard API endpoint and standard MiMo key family. Xiaomi Token
Plan docs and console expose credit/quota semantics, but Codewhale does not
currently have a documented balance endpoint to poll, so cost display remains
unknown rather than reusing token-price estimates from another provider.

Voice-design and voice-clone shorthands map to `mimo-v2.5-tts-voicedesign` and
`mimo-v2.5-tts-voiceclone`. Xiaomi's current
[image-understanding guide](https://platform.xiaomimimo.com/docs/en-US/usage-guide/multimodal-understanding/image-understanding)
includes `mimo-v2.5` for image input. Codewhale exposes image analysis through the
separate `[vision_model]` / `image_analyze` path; set that model to
`mimo-v2.5` when using MiMo for vision.

### OpenRouter-Compatible Base URLs

OpenRouter-compatible gateways should usually stay on the `openrouter`
provider with a provider-scoped `base_url` override instead of moving through
the generic `openai` route. That keeps OpenRouter-style reasoning, streaming,
cache usage, and namespaced wire model parsing attached to the selected route:

```toml
provider = "openrouter"

[providers.openrouter]
api_key = "sk-..."
base_url = "https://openrouter-compatible.example/v1"
model = "deepseek/deepseek-v4-pro"
```

Codewhale preserves the `deepseek/` wire-model prefix under the OpenRouter
provider scope; it does not infer a switch to the direct DeepSeek provider from
that model string. Cache fields such as `prompt_cache_hit_tokens`,
`prompt_cache_miss_tokens`, and `prompt_tokens_details.cached_tokens` are
parsed when the upstream gateway sends them. If a key/account type omits those
fields, Codewhale treats them as absent for that response rather than as a
different provider route.

OrcaRouter (`https://api.orcarouter.ai/v1`) is a dedicated named route
([OrcaRouter](https://www.orcarouter.ai)) that speaks the same OpenAI
Chat Completions wire protocol and serves the same namespaced
`vendor/model` catalog. It does not need an OpenRouter-compatible
`base_url` override: select `provider = "orcarouter"` and its namespaced
wire models (for example `deepseek/deepseek-v4-pro` or its own
`orcarouter/auto` router) pass through verbatim, exactly as they do on the
OpenRouter provider scope.

#### OrcaRouter credentials: API key or OAuth 2.0 + PKCE

OrcaRouter accepts two credential sources, and both write the same durable
`sk-orca-…` key to the same `orcarouter` secret-store slot with
`auth_mode = "api_key"`:

- **API key** — `codewhale auth set --provider orcarouter`, `/provider`, or
  `ORCAROUTER_API_KEY`. Use this when you already have a key.
- **Connect with OrcaRouter** — `codewhale auth orcarouter` (CLI) or
  `/auth orcarouter` (in-session). This runs an OAuth 2.0 authorization-code
  flow with PKCE (S256) on a loopback redirect. The browser is sent to
  `GET https://www.orcarouter.ai/auth` with `callback_url`, `code_challenge`,
  `code_challenge_method=S256`, `state`, `app_name`, and `scope`; the code is
  exchanged at `POST https://www.orcarouter.ai/api/v1/auth/keys` for a
  durable API key. There is no client secret and no pre-registered redirect
  URI; state is compared in constant time before the code is used.

Authentication and inference use different origins: the consent and exchange
live on `https://www.orcarouter.ai`; models and chat live on
`https://api.orcarouter.ai/v1`. Neither is derived from the other, and
`/v1/auth/keys` on the API origin is not the exchange route. Self-hosted
deployments can override each origin explicitly with `ORCA_AUTH_BASE_URL`
(auth) and `ORCA_API_BASE_URL`/`ORCAROUTER_BASE_URL` (inference); the explicit
value wins. Remote origins must be HTTPS; plain HTTP is accepted only on
loopback.

A PKCE-issued key is a durable API key, **not** a refresh token: Codewhale
stores it, reuses it across restarts, and never sends a refresh grant. Revoke
the key on the OrcaRouter console (`/auth orcarouter-revoke` clears the local
copy) and sign in again to get a new one. A 401 from the relay marks that
credential generation as needing re-authentication rather than silently
retrying.

The chat model list is discovered live from `GET https://api.orcarouter.ai/v1/models`
with the configured key. OrcaRouter publishes `supported_endpoint_types` on
every row, so the chat selector keeps only rows advertising `openai`,
`anthropic`, `gemini`, or `openai-response`, and drops image-generation,
video, and rerank rows instead of guessing from a model name. A row that
states `architecture.input_modalities` with `image` is image-input capable;
rows that state no architecture are treated as unknown, not as text-only.

### Recent OpenRouter Large Models

OpenRouter completions and static registry rows include the April 2026 onward
large models verified through OpenRouter's model metadata:
`arcee-ai/trinity-large-thinking`, `qwen/qwen3.6-flash`,
`qwen/qwen3.6-35b-a3b`, `qwen/qwen3.6-max-preview`, `qwen/qwen3.6-27b`,
`qwen/qwen3.6-plus`, `minimax/minimax-m3`, `xiaomi/mimo-v2.5-pro`,
`xiaomi/mimo-v2.5`, `moonshotai/kimi-k2.7-code`, `moonshotai/kimi-k2.6`,
`z-ai/glm-5.1`, `z-ai/glm-5.2`, `z-ai/glm-5-turbo`, `tencent/hy3-preview`,
`google/gemma-4-31b-it`, `google/gemma-4-26b-a4b-it`, and
`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`.
`minimax/minimax-m3` was added from OpenRouter's May 31, 2026 listing as a 1M
context multimodal model for coding, tool use, and long-horizon agentic work.
`GLM-5.3` is now the default direct Z.AI Coding Plan model; `GLM-5.2` /
`z-ai/glm-5.2` remain available (explicit selections keep their own id),
`GLM-5.1` / `z-ai/glm-5.1` remain available as the smaller model,
`GLM-5.3-Flash` / `z-ai/glm-5.3-flash` is the faster/explore sibling of
`GLM-5.3`, and `GLM-5-Turbo` / `z-ai/glm-5-turbo` remains the faster sibling
of `GLM-5.2`.
`GLM-5.3` / `z-ai/glm-5.3` and `GLM-5.3-Flash` / `z-ai/glm-5.3-flash` are
first-class picker ids on the Z.ai and OpenRouter routes (`/model` after
`/provider zai`, or `model = "GLM-5.3-Flash"`).
Limits and reasoning options for 5.3 are inherited from
`GLM-5.2` until Z.ai publishes distinct 5.3 metadata, and 5.3 carries no
price. Flash ships the published $0.15/$0.50 list. A live call can still
429 with entitlement code 1311 on accounts that are not provisioned for 5.3.

## Static Model Registry

This table describes the static registry, which `codewhale model resolve` (and
the `[model]` config surface) still use to resolve selector aliases.

`codewhale model resolve` projects the reviewed `selections` in
`crates/config/assets/catalog_corrections.json` through
`crates/agent/src/lib.rs`. There is no independent Rust model roster. These
ordered aliases and flags are compatibility metadata, not account availability
or executable route permission. This differs from live `/models` discovery.
Use `/models` or `codewhale models` to fetch model IDs from the active API
endpoint when the endpoint supports model listing.

| Provider | Static registry entries | Tool calls | Registry reasoning flag |
| --- | --- | --- | --- |
| `deepseek` | `deepseek-v4-pro`, `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp` | yes | yes |
| `nvidia-nim` | `deepseek-ai/deepseek-v4-pro`, `deepseek-ai/deepseek-v4-flash` | yes | yes |
| `openai` | `deepseek-v4-pro`, `deepseek-v4-flash`, `gpt-5.6`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` | yes | yes |
| `atlascloud` | `deepseek-ai/deepseek-v4-flash`, `deepseek-ai/deepseek-v4-pro` | yes | yes |
| `wanjie-ark` | `deepseek-reasoner` | yes | yes |
| `volcengine` | `DeepSeek-V4-Pro`, `DeepSeek-V4-Flash` | yes | yes |
| `openrouter` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`, `arcee-ai/trinity-large-thinking`, `minimax/minimax-m3`, `minimax/minimax-m2.7`, `xiaomi/mimo-v2.5-pro`, `xiaomi/mimo-v2.5`, `qwen/qwen3.6-flash`, `qwen/qwen3.6-35b-a3b`, `qwen/qwen3.6-max-preview`, `qwen/qwen3.6-27b`, `qwen/qwen3.6-plus`, `qwen/qwen3.7-max`, `moonshotai/kimi-k2.7-code`, `moonshotai/kimi-k2.6`, `z-ai/glm-5.1`, `z-ai/glm-5.2`, `z-ai/glm-5.3`, `z-ai/glm-5.3-flash`, `z-ai/glm-5-turbo`, `tencent/hy3-preview`, `google/gemma-4-31b-it`, `google/gemma-4-26b-a4b-it`, `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`, `nvidia/nemotron-3-ultra-550b-a55b` | yes | yes |
| `orcarouter` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`, `orcarouter/auto` | yes | yes |
| `xiaomi-mimo` | `mimo-v2.5-pro`, `mimo-v2.5-pro-ultraspeed`, `mimo-v2.5`; speech/TTS IDs are selected through `codewhale speech` / `tts` | yes | yes for chat models; no for speech/TTS models |
| `novita` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash` | yes | yes |
| `fireworks` | `accounts/fireworks/models/deepseek-v4-pro` | yes | yes |
| `siliconflow` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | yes | yes |
| `arcee` | `trinity-large-thinking`, `trinity-large-preview`; provider-hinted custom model IDs pass through | yes | yes for `trinity-large-thinking`; no for `trinity-large-preview` |
| `moonshot` | `kimi-k2.7-code`, `kimi-k2.6` | yes | yes |
| `zai` | `GLM-5.3`, `GLM-5.3-Flash`, `GLM-5.2`, `GLM-5.1`, `GLM-5-Turbo`; provider-hinted custom model IDs pass through | yes | yes |
| `stepfun` | `step-3.7-flash` (default), `step-5-preview`, `step-3.5-flash`, `step-3.5-flash-2603` | yes | yes |
| `minimax` | `MiniMax-M3`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed`, `MiniMax-M2.5`, `MiniMax-M2.5-highspeed`, `MiniMax-M2.1`, `MiniMax-M2.1-highspeed`, `MiniMax-M2` | yes | yes |
| `minimax-anthropic` | `MiniMax-M3`, `MiniMax-M2.7` | yes | yes |
| `sglang` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | yes | yes |
| `vllm` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | yes | yes |
| `ollama` | live local tag; custom tags pass through when provider hint is `ollama` | yes | no |
| `ollama-cloud` | `gpt-oss:120b`; arbitrary provider-owned model IDs pass through | yes | yes |
| `huggingface` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | yes | no |
| `modelscope` | `Qwen/Qwen3.5-397B-A17B`, `Qwen/Qwen3.5-122B-A10B`, `Qwen/Qwen3.5-27B`, `Qwen/Qwen3.5-35B-A3B`, `Qwen/Qwen3.8-27B`, `Qwen/Qwen3.8-Flash-Next`, `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Pro-0813`, `deepseek-ai/DeepSeek-V4.1-Flash`, `ZhipuAI/GLM-4.7-Flash`, `ZhipuAI/GLM-5.2` | yes | no |
| `deepinfra` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | yes | yes |
| `together` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash`, `thinkingmachines/inkling` | yes | yes |
| `openai-codex` | `gpt-5.5` | yes | yes |
| `anthropic` | `claude-opus-5`, `claude-opus-4-8`, `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-fable-5`, `claude-haiku-4-5` | yes | yes except `claude-haiku-4-5` |
| `openmodel` | `deepseek-v4-flash`; provider-scoped custom model IDs pass through | yes | model-dependent |
| `sakana` | `fugu`, `fugu-ultra-20260615` | yes | yes for `fugu-ultra-20260615` |
| `longcat` | `LongCat-2.0` | yes | yes |
| `opencode-go` | `deepseek-v4-pro`, `grok-4.5`, `glm-5.2`, `glm-5.1`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `deepseek-v4-flash`, `mimo-v2.5`, `mimo-v2.5-pro` | yes | yes |
| `meta` | `muse-spark-1.2` | yes | yes |
| `xai` | `grok-4.6`, `grok-4.7`, `grok-4.5`, `grok-4.3`, `grok-build`, `grok-composer-2.5-fast`, `grok-4.20-0309-reasoning`, `grok-4.20-0309-non-reasoning` | yes | yes for `grok-4.6`, `grok-4.7`, `grok-4.5`, `grok-4.3`, `grok-build`, and `grok-4.20-0309-reasoning` |
| `google` | `gemini-3.1-pro-preview`, `gemini-3-pro-preview`, `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-2.5-pro`, `gemini-2.5-flash` | yes | yes except `gemini-3.5-flash-lite` |
| `mistral` | `mistral-code-latest`, `mistral-medium-latest`, `mistral-small-latest`, `mistral-large-latest` | yes | yes for Medium and Small (`reasoning_effort` `none` or `high` on exact first-party routes); deprecated native Magistral remains an always-on explicit compatibility ID; no for Code and Large |
| `modelstudio-token-plan`, `modelstudio-coding-plan` | `qwen3.8-max`, `qwen3.8-max-preview`, `qwen3.7-plus`, `qwen3.7-max`, `qwen3.6-flash`, `deepseek-v4-pro`, `deepseek-v4-flash-0731`, `glm-5.2` | yes | yes |
| `csdn` | `glm_for_coding`; other marketplace model IDs pass through | yes | yes |

AtlasCloud keeps the same default model as the config layer and adds
provider-scoped aliases for the Pro and Flash rows. Other AtlasCloud model IDs
should still be selected through `ATLASCLOUD_MODEL`, config, or live model
listing when available.

## Capability Metadata

`codewhale doctor --json` exposes the `capability` object. It is static
metadata, not a live API probe. Current fields are:

`resolved_provider`, `resolved_model`, `context_window`, `max_output`,
`thinking_supported`, `cache_telemetry_supported`, and `request_payload_mode`.

When configuration cannot be loaded or validated, `doctor --json` exits
nonzero and prints a bounded, secret-redacted JSON error envelope with
`status = "error"` and `error.kind = "config_validation"` instead of emitting
misleading route or capability metadata.

Most shipped providers use the Chat Completions request payload mode. Native
Messages routes, including `minimax-anthropic`, use `/v1/messages`, and
`openai-codex` uses Responses.

For OpenAI-compatible gateways or self-hosted runtimes whose real window
differs from the static table, set `[providers.<name>] context_window = N`.
The configured value becomes the route-effective context window for prompts,
context-pressure checks, compaction, and output-cap budgeting.

`max_output` is optional and truthful: it is `null` (and omitted from the
capability struct on the wire) when the route publishes no output maximum we
can stand behind — the Kimi Code membership `kimi-for-coding` family is the
canonical example, since the membership catalog owns their limits. An unknown
output ceiling is never backfilled with a placeholder, and it applies **no**
compatibility clamp to a turn's requested `max_tokens`; only a concrete
route/offering maximum narrows the request. A model the catalogue simply has no
row for is a different fact — absence is not permission, so an uncatalogued id
keeps a conservative ceiling. The "Max output metadata" column below reads
`unknown` wherever no documented maximum exists.

The exact Kimi Code membership roster contains `k3`, `k3-256k`,
`kimi-for-coding`, and `kimi-for-coding-highspeed`. The two K3 ids share the
same reasoning and fixed-sampling contract; `k3-256k` stays at 262,144 tokens,
while bare `k3` can use an entitled 1M override.

| Provider/model class | Context window | Max output metadata | Thinking support | Cache telemetry | FIM endpoint |
| --- | --- | --- | --- | --- | --- |
| DeepSeek V4 (`deepseek-v4-pro`, `deepseek-v4-flash`) | 1,000,000 | 384,000 | yes | yes | DeepSeek beta only |
| DeepSeek V4 Flash Vision experimental (`deepseek-v4-flash-vision-exp`) | 1,000,000 inherited from Flash | 384,000 inherited from Flash | yes, inherited | yes, inherited | not claimed; Chat Completions route only |
| DeepSeek compatibility aliases (`deepseek-chat`, `deepseek-reasoner`) | 1,000,000 | 384,000 | yes | yes | DeepSeek beta only |
| NVIDIA NIM V4 registry models | 1,000,000 | 384,000 | yes | yes | not documented in code |
| Volcengine Ark V4 model IDs | 1,000,000 | 384,000 | yes | yes | not documented in code |
| OpenRouter, Novita, Fireworks, SiliconFlow, SGLang, and vLLM V4 model IDs | 1,000,000 | 384,000 | yes | no | not documented in code |
| Xiaomi MiMo `mimo-v2.5-pro`, `mimo-v2.5-pro-ultraspeed`, `mimo-v2.5` | 1,000,000 | 131,072 | yes | no | not documented in code |
| OpenRouter Qwen 3.6 Flash / Plus | 1,000,000 | 65,536 | yes | no | not documented in code |
| OpenRouter Qwen 3.6 35B / 27B | 262,144 | 262,140 | yes | no | not documented in code |
| OpenRouter Qwen 3.6 Max Preview | 262,144 | 65,536 | yes | no | not documented in code |
| OpenAI API `gpt-5.5` | 1,050,000 | 128,000 | yes | no | not documented in code |
| OpenAI API `gpt-5.6`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` | 1,050,000 | 128,000 | yes | no | not documented in code |
| Anthropic API `claude-opus-5`, `claude-opus-4-8`, `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-fable-5` | 1,000,000 | 128,000 | yes | yes | not documented in code |
| Google Gemini API `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-3.1-pro-preview`, `gemini-2.5-pro`, `gemini-2.5-flash` | 1,048,576 | 65,536 | model-dependent | no | not documented in code |
| Meta Model API `muse-spark-1.2` | 1,000,000 | 32,000 | yes | no | not documented in code |
| ChatGPT plan route (`openai-codex`) | conservatively budgeted; account listing does not state a window | no output cap sent in preview | model dependent | no | public `/v1/responses`; no context limit is inferred from account eligibility |
| OpenModel default/custom model IDs | 200,000 fallback unless model metadata or config overrides it | 64,000 fallback | model-dependent | no | route uses Messages payload at `/v1/messages` |
| Wanjie Ark `reasoner` / `r1` model IDs | 128,000 | unknown (no documented maximum) | yes | no | not documented in code |
| Direct Arcee API `trinity-large-thinking` | 262,144 | 262,144 | yes | no | not documented in code |
| Direct Arcee API `trinity-large-preview` | 262,144 | unknown (no documented maximum) | no in doctor capability metadata | no | not documented in code |
| Direct Moonshot `kimi-k3` | 1,048,576 | 1,048,576 documented maximum; 131,072 provider default | yes | no | exact route uses `max_completion_tokens` and omits fixed sampling fields ([K3 quickstart](https://platform.kimi.ai/docs/guide/kimi-k3-quickstart)) |
| Kimi Code membership `k3` | 262,144 safe baseline; 1,048,576 with an explicit entitled-plan override | 131,072 conservative default ceiling; membership maximum is not published | yes | no | exact `https://api.kimi.com/coding/v1` route |
| Kimi Code membership `k3-256k` | 262,144 fixed | 131,072 conservative default ceiling; membership maximum is not published | yes | no | exact `https://api.kimi.com/coding/v1` route |
| Direct Moonshot/Kimi K2.7/K2.6 (`kimi-k2.7-code`, `kimi-k2.7-code-highspeed`, `kimi-k2.6`) | 262,144 | 32,768 | yes | no | provider-reported bundled catalog |
| Kimi Code membership `kimi-for-coding`, `kimi-for-coding-highspeed` | 262,144 | unknown — the membership catalog owns these limits and no client-side ceiling is claimed | yes | no | exact `https://api.kimi.com/coding/v1` route |
| Direct Z.AI `GLM-5.3` (default) | 1,000,000 | 131,072 | yes | no | live on the GLM Coding Plan; limits inherited from `GLM-5.2` until Z.ai publishes distinct 5.3 numbers; no USD price |
| Direct Z.AI `GLM-5.3-Flash` | 1,000,000 | 131,072 | yes | no | natively multimodal; $0.15/$0.50 list (2026-08-26); faster/explore sibling of `GLM-5.3` |
| Direct Z.AI `GLM-5.2` | 1,000,000 | 131,072 | yes | no | not documented in code |
| Direct Z.AI `GLM-5.1` | 202,752 | 131,072 | yes | no | not documented in code |
| Direct Z.AI `GLM-5-Turbo` | 202,752 | 131,072 | yes | no | faster/explore sub-agent sibling |
| Direct MiniMax `MiniMax-M3` | 1,000,000 | 524,288 | yes | no | not documented in code |
| Direct MiniMax M2.x models | 204,800 | unknown until MiniMax output metadata is promoted | yes | no | not documented in code |
| MiniMax Messages route (`MiniMax-M3`, `MiniMax-M2.7`) | model-specific values above | model-specific values above | yes | no | route uses `/anthropic/v1/messages` |
| Generic `openai` and AtlasCloud | 128,000 | unknown (no documented maximum) | no in doctor capability metadata | no | not documented in code |
| Ollama | 8,192 | unknown (no documented maximum) | no | no | not documented in code |
| Hugging Face Inference Providers V4 model IDs | 131,072 | unknown (no documented maximum) | yes | no | not documented in code |
| Other recognized DeepSeek model IDs | 128,000 unless the model name carries an explicit `Nk` hint | unknown (no documented maximum) | no unless V4/reasoner logic matches | DeepSeek/NIM only | DeepSeek beta only |

MiniMax M3 uses input-length and service tiers. Codewhale omits
`service_tier`, so requests use the standard tier and cost estimates select the
correct standard rate from total input usage. Priority rates are listed to keep
the official tier structure visible. Prices are USD per million tokens.

| Model / service tier | Input length | Input | Output | Cache read | Cache write |
| --- | --- | ---: | ---: | ---: | ---: |
| `MiniMax-M3` standard | up to 512,000 input tokens | $0.30 | $1.20 | $0.06 | not published |
| `MiniMax-M3` standard | over 512,000 input tokens | $0.60 | $2.40 | $0.12 | not published |
| `MiniMax-M3` priority | up to 512,000 input tokens | $0.45 | $1.80 | $0.09 | not published |
| `MiniMax-M3` priority | over 512,000 input tokens | $0.90 | $3.60 | $0.18 | not published |
| `MiniMax-M2.7` standard | all supported inputs | $0.30 | $1.20 | $0.06 | $0.375 |

These values come from the [MiniMax pay-as-you-go pricing
guide](https://platform.minimax.io/docs/guides/pricing-paygo). M3 thinking is
adaptive or disabled; the OpenAI-compatible API defaults to adaptive and the
Anthropic-compatible API defaults to disabled. M2.7 thinking cannot be
disabled. Codewhale sends explicit controls when the user selects a reasoning
mode.

Tool-call support is tracked separately by the static `ModelRegistry` and by
the endpoint's ability to accept OpenAI-compatible `tools` payloads. A custom
OpenAI-compatible or local endpoint can still reject tool calls even if
Codewhale can send the schema.

### Hugging Face Inference Providers Notes

The shipped Hugging Face route targets the OpenAI-compatible Inference Providers
router at `https://router.huggingface.co/v1`. Configure auth with
`HUGGINGFACE_API_KEY` first, or `HF_TOKEN` as a fallback. Configure the endpoint
with `HUGGINGFACE_BASE_URL` first, or `HF_BASE_URL` as a fallback; configure the
model with `HUGGINGFACE_MODEL` first, or `HF_MODEL` as a fallback.

This route does not imply Hub browsing, model-card metadata, dataset access,
Jobs, uploads, or export. Those remain explicit Model Lab work items so
provider auth and artifact movement stay separate.

### When a Local Model Prints Tool JSON

Codewhale only executes tools when the provider returns Chat Completions
`tool_calls` or streamed `delta.tool_calls`. If a local model prints text such
as `{"name":"File","arguments":{"action":"search_content",...}}` in the
assistant message, that is ordinary model output, not an executable tool
request.

For OpenAI-compatible or local runtimes, check:

- The endpoint accepts the `tools` array in `/v1/chat/completions` requests.
- The selected model or chat template is configured for function/tool calls.
- The server returns `tool_calls` in the response rather than plain JSON text.
- The compatibility layer does not strip tools before forwarding the request.
- If in doubt, test a small `File` `read` or `search_content` action against a
  known tool-calling model before debugging Codewhale's tool registry.

Changing `provider`, `base_url`, or `model` can select a route that supports the
OpenAI-compatible payload shape, but Codewhale cannot convert arbitrary JSON
text into a trusted tool call after the model has emitted it as prose.

DeepSeek will retire `deepseek-chat` and `deepseek-reasoner` on 2026-07-24 at
15:59 UTC. Codewhale migrates either name to `deepseek-v4-flash` before a
request reaches DeepSeek's first-party OpenAI or Anthropic endpoint. If no
reasoning tier was configured, `deepseek-chat` also migrates to `off` and
`deepseek-reasoner` to `high`, preserving their former non-thinking / thinking
intent; an explicit `reasoning_effort` remains authoritative. The mapping is
deliberately not global: Wanjie Ark, aggregators, self-hosted runtimes, and
custom endpoints continue to own their model ids.

## Reasoning Effort

`/reasoning <effort>` (and the `reasoning_effort` config key) is translated to
each provider's wire dialect by the client before the request is sent. `off`
disables thinking where the route supports it. Both exact K3 routes map `off`
to their lowest supported tier, `low`, and the model is never switched to
satisfy `off` — but they do so for different reasons:

- **Kimi Code membership K3** (exact `https://api.kimi.com/coding/v1` with
  `model = "k3"` or `model = "k3-256k"`) — the membership roster declares K3 always-thinking, so `off`
  cannot be honored without changing what the model is. The clamp preserves the
  fixed K3 identity.
- **Direct Moonshot K3** (exact `https://api.moonshot.ai/v1` with
  `model = "kimi-k3"`) — this clamp is *defensive*, not a documented contract.
  The direct platform publishes no `off` state for K3, and Codewhale will not
  assert a fixed-thinking guarantee it cannot verify for a given key's
  entitlement, so the requested `off` is normalized to the lowest tier with the
  live entitlement left unknown.

Normal dispatched
`auto` uses Codewhale's auto-reasoning selector and sends a concrete tier;
only an omitted reasoning setting leaves the provider default in control.
Providers marked "omitted" receive no reasoning fields at all for that tier.

| Provider | `off` | `low`/`medium`/`high` | `max`/`xhigh` |
| --- | --- | --- | --- |
| `deepseek`, `deepseek-cn`, `siliconflow`, `siliconflow-CN`, `sglang`, `volcengine`, `atlascloud` | `thinking: {type: disabled}` | `reasoning_effort: "high"` + `thinking: {type: enabled}` | `reasoning_effort: "max"` + `thinking: {type: enabled}` |
| `openrouter`, `novita`, other `together` models | `thinking: {type: disabled}` | `reasoning_effort` pass-through + `thinking: {type: enabled}` | `reasoning_effort: "xhigh"` + `thinking: {type: enabled}` |
| `together` + `thinkingmachines/inkling` | `reasoning_effort: "none"` | exact `minimal`/`low`/`medium`/`high` `reasoning_effort` | `reasoning_effort: "max"` |
| Direct Moonshot `kimi-k3` at exact `https://api.moonshot.ai/v1` | top-level `reasoning_effort: "low"` (effective normalization) | top-level `reasoning_effort: "low"` / `"high"` (`medium` becomes `high`) | top-level `reasoning_effort: "max"` |
| Kimi Code membership `k3`, `k3-256k` at exact `https://api.kimi.com/coding/v1` | `thinking: {type: enabled, effort: "low"}` (effective normalization) | `thinking: {type: enabled, effort: "low" | "high"}` | `thinking: {type: enabled, effort: "max"}` |
| Other `moonshot` routes | `thinking: {type: disabled}` | `thinking: {type: enabled}` | `thinking: {type: enabled}` |
| `ollama` | `think: false` | `think: true` | `think: true` |
| `ollama-cloud` | `reasoning_effort: "none"` | exact `low`/`medium`/`high` `reasoning_effort` | `reasoning_effort: "max"` |
| `xiaomi-mimo` | `thinking: {type: disabled}` | `thinking: {type: enabled}` | `thinking: {type: enabled}` |
| First-party `minimax` `MiniMax-M3` | `reasoning_split: true` + `thinking: {type: disabled}` | `reasoning_split: true` + `thinking: {type: adaptive}`; effective tier granularity unavailable | `reasoning_split: true` + `thinking: {type: adaptive}`; effective tier granularity unavailable |
| First-party Z.ai `GLM-5.2` | `thinking: {type: disabled}`; no `reasoning_effort` | enabled thinking; only effective `high` adds `reasoning_effort: "high"` | enabled thinking + `reasoning_effort: "max"` |
| First-party Z.ai `GLM-5.3` | `thinking: {type: disabled}`; no `reasoning_effort` | enabled thinking; only effective `high` adds `reasoning_effort: "high"` | enabled thinking + `reasoning_effort: "max"` |
| First-party Z.ai `GLM-5.3-Flash` | `thinking: {type: disabled}`; no `reasoning_effort` | enabled thinking; only effective `high` adds `reasoning_effort: "high"` | enabled thinking + `reasoning_effort: "max"` |
| First-party Z.ai `GLM-5-Turbo` | `thinking: {type: disabled}` | enabled thinking; effort granularity unavailable | enabled thinking; effort granularity unavailable |
| Compatible gateways configured as `zai` | omitted; effective unavailable | omitted; effective unavailable | omitted; effective unavailable |
| `nvidia-nim` | `chat_template_kwargs.thinking: false` | `chat_template_kwargs`: `thinking: true` + `reasoning_effort: "high"` | `chat_template_kwargs`: `thinking: true` + `reasoning_effort: "max"` |
| `vllm` | `chat_template_kwargs.enable_thinking: false` | `chat_template_kwargs.enable_thinking: true` + `reasoning_effort` low/medium/high | `chat_template_kwargs.enable_thinking: true` + `reasoning_effort: "high"` (vLLM has no max tier) |
| `arcee`, `huggingface` | omitted | `reasoning_effort` pass-through | `reasoning_effort: "high"` |
| `fireworks` | omitted | `reasoning_effort: "high"` | `reasoning_effort: "max"` |
| `openai`, `wanjie-ark`, `telecomjs` | omitted | omitted | omitted |
| `openmodel` | Anthropic Messages adapter handles thinking/output configuration | Anthropic Messages adapter handles thinking/output configuration | Anthropic Messages adapter handles thinking/output configuration |
| `openai-codex` | Responses API `reasoning` field (handled by the Responses bridge) | Responses API `reasoning` field | Responses API `reasoning` field |

AtlasCloud serves DeepSeek models, so it speaks the DeepSeek reasoning dialect,
including the `max` tier (#3024).

On the exact MiniMax OpenAI-compatible Chat endpoints, `MiniMax-M3` uses
`max_completion_tokens`. Other MiniMax models and compatible gateways retain
`max_tokens`; the MiniMax Anthropic endpoints use the separate Messages
adapter.

## Drift Check

Run this before changing provider IDs, provider TOML tables, static model
registry rows, or provider default strings:

```bash
python3 scripts/check-provider-registry.py
```

The check fails when:

- `docs/PROVIDERS.md` omits a canonical `ProviderKind::as_str()` ID.
- Descriptor presentation IDs or released wire tags are duplicated or drift
  from intrinsic kinds, or a second provider enum/ordinal bridge is introduced.
- The shipped-provider table omits or adds a `[providers.*]` TOML table.
- The static model registry table drifts from providers used by
  `crates/agent/src/lib.rs`.
- A provider default model or base URL constant in `crates/tui/src/config.rs`
  is no longer mentioned here.

## Provider Gotchas

Three failures that look like something else (measured by a provider operator
on live traffic):

- **A cold model looks broken.** The first call to a model the provider has not
  loaded can take 90 to 150 seconds, then drop to a second or less once warm.
  Raise `[stream].open_timeout_secs` (default 45 seconds, at most 300) before reading a timeout as "model unavailable".
- **Listed is not callable.** A catalog entry can be visible and still fail
  every call. The same error text on every attempt (for example a "resources
  busy" 502) is not transient: a genuinely busy upstream returns successes in
  between. Probe a model before trusting it, and stop retrying one that fails
  identically.
- **A truncated answer can report `stop`.** A reasoning model can spend the
  whole `max_tokens` allowance on reasoning. When the provider reports
  `completion_tokens` exactly equal to the `max_tokens` the request asked for
  and the finish reason is `stop`, Codewhale treats the turn as cut at the
  ceiling rather than finished (`stop_contradicts_output_ceiling` in
  `codewhale-models`); retrying the same prompt stops at the same count, so
  raise the ceiling or shorten the task instead. No inference is made when the
  provider reports no usage.

## Planned, Not Shipped Yet

These items belong to the v0.8.48+ provider-abstraction milestone or related
provider docs work, but they are not native shipped behavior in this checkout:

- A unified `Provider` trait in `codewhale-agent` that owns env precedence,
  secret resolution, base URL normalization, auth-header construction, and
  provider metadata. Those responsibilities are still split across
  `crates/config`, `crates/secrets`, and `crates/tui/src/client.rs`.
- Hugging Face model passport metadata in the picker, including license, base
  model, context length, chat template, tool-call support, reasoning support,
  and gated/private status.

## Sign in with Claude

Run `codewhale auth claude` (or `/auth claude` in a running session), open the
printed Claude sign-in URL, and paste the complete `code#state` callback.
`/provider setup anthropic` also offers API-key and Claude Pro / Max choices.
Then select `anthropic` or its `claude` alias. API keys continue to use separate
Anthropic API billing; selecting subscription sign-in never falls back to one.

Codewhale stores the grant in its private credentials directory, commits the
provider's credential pointer atomically, and refreshes it before requests on a
worker. Subscription tokens go only to `https://api.anthropic.com`; a custom
endpoint is rejected. Each request keeps the existing Messages adapter, tool
protocol, cache prefix, and Codewhale client identity. No Claude Code credential
file or keychain entry is read or modified.

`codewhale auth claude-revoke` (or `/auth claude-revoke`) removes the owned local
grant. End remote access separately in your Claude account settings. The
subscription route then needs another login; select API-key billing explicitly
to use an API key. Login and inference follow Anthropic's provider-side
availability; local checks do not
establish that an account is eligible or that live subscription inference works.

The callback and JSON token exchange follow the interoperable flow used by
[pi's Anthropic OAuth adapter](https://github.com/fivewillow/badlogic-pi-mono/blob/main/packages/ai/src/utils/oauth/anthropic.ts).
