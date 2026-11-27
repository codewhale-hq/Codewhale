# Models and providers

This document is the single owner for how Codewhale knows providers and models:
where bundled provider and model data lives, how model IDs are resolved, how
limits, capabilities and prices follow the resolved route, how signed
`registry.json` updates are verified and applied, and how to add a model or an
OpenAI-compatible provider without writing Rust code.

Two invariants govern the stack:

1. **One data schema.** Provider metadata, model-ID naming rules (`ids` policy,
   accepted globs, stripped prefixes, aliases, vendor wire slugs) and intrinsic
   model facts live in data, compiled and validated once. Rust code is reserved
   for wire protocols, authentication flows and billing rules that are genuine
   runtime behavior.
2. **One resolver.** `codewhale_config::route::resolve_model`
   (`crates/config/src/route/resolver.rs`) is the sole function that turns
   `(provider, raw_model_id, base_url)` into the logical model ID, the outbound
   wire ID, the canonical `ModelId` and the matched catalog offering. Every
   caller — the CLI (`codewhale model resolve`, `codewhale exec`), the TUI
   (`/model` picker and shorthands), the Runtime API (`crates/app-server`),
   subagent routing, speech and vision selection, context/output budgeting and
   token pricing — consumes that result.

Related documents own their specific surfaces rather than restating model
resolution:

- [Provider routes](PROVIDERS.md) — per-provider credentials, base URLs, wire
  dialects and reasoning-effort mappings.
- [Catalog refresh](CATALOG_REFRESH.md) — runtime Models.dev refresh, cache
  layers and offline seed regeneration commands.
- [Signed cloud facts](CLOUD_FACTS.md) — Ed25519 envelope format, pinned key
  roles, rollback floors and public relay transport.
- [Configuration](CONFIGURATION.md) — `config.toml` keys, environment variable
  precedence and per-route `context_window` overrides.

---

## 1. Where bundled data lives today

Until the Platform database cutover ([Section 6](#6-planned-not-shipped-yet))
completes, the Engine repository's checked-in assets are the offline and
build-time authority:

| Asset / script | Role | Compiled / checked by |
| --- | --- | --- |
| `crates/config/assets/provider_descriptors.json` | Single provider metadata file: `providers[]` (built-in `ProviderKind` routes), `descriptors[]` (data-only compatible hosts without a `ProviderKind` variant) and `legacy_tui` (`deepseek-cn` compatibility row). Every entry carries its `models` naming block alongside base URL, default model, auth env vars, secret slot, aliases and wire policy. | `crates/config/build.rs` → `crates/config/src/descriptors.rs`; `scripts/check-provider-registry.py` |
| `crates/config/assets/catalog_corrections.json` | Deliberate field patches (`providers`, `models`) applied to both offline and live Models.dev rows (`pricing_withheld`, `max_output`, capability holds), plus the `reviewed` supplement (`intrinsic` model facts, `thought_signatures_required`, `thinking_can_disable`, `transports`, `public_models`, `selections`, `search_models`, `route_model_sets`, `reference_prices` and `source_receipts`). | `crates/config/src/catalog/corrections.rs`, `crates/config/src/catalog/reviewed.rs` |
| `crates/config/assets/models_dev.bundled.json` | Generated offline Models.dev seed (`_meta.role`); reviewed facts stay in `catalog_corrections.json`. Never hand-edited; rendered deterministically from `scripts/catalog/models_dev_seed.toml`, `scripts/catalog/models_dev_seed.lock.json` and `catalog_corrections.json`. | `python3 scripts/catalog_models_dev.py seed render --check` and `seed age --max-days 30` |
| `crates/config/assets/responses_dialects.json` | Declarative OpenAI Responses API dialect profiles (`standard`, `codex`, and gateway profiles) and route bindings (`default_profile`, `custom_allowed_profiles`, `profiles`, `bindings`). | `crates/config/src/responses_dialect_data.rs`, `crates/config/build.rs` |
| `scripts/catalog_models_dev.py` | Secret-free maintainer CLI for upstream dry-run inspection (`refresh`), pinning (`seed lock`), deterministic rendering (`seed render`), staleness checks (`seed age`) and new-model discovery (`seed discover`). | `python3 -m unittest scripts/catalog_models_dev_test.py` |

Downstream surfaces derive from these same files instead of keeping parallel
lists:

- **Website (`web/`):** `web/scripts/derive-facts.mjs` (via
  `web/scripts/facts-lib.mjs` and `web/lib/provider-descriptors.mjs`) generates
  `web/lib/facts.generated.ts` and `web/data/agent-docs.generated.json` at
  prebuild time; `web/scripts/check-facts.mjs` fails CI on drift.
- **Desktop and browser clients:** read providers, models, limits and readiness
  from the Engine's Runtime API (`GET /v1/providers`,
  `GET /v1/providers/{id}/models`) and keep no separate model table.

---

## 2. Per-provider model naming rules (`models` block)

Every row in `providers[]`, `descriptors[]` and `legacy_tui` inside
`crates/config/assets/provider_descriptors.json` carries a `models` object:

```json
"models": {
  "ids": "catalog",
  "accept": [],
  "strip_prefixes": ["models/"],
  "aliases": {
    "gemini-3.7-flash": "gemini-3.8-flash"
  },
  "wire_slugs": {
    "deepseek-v4-pro": "deepseek-ai/DeepSeek-V4-Pro"
  }
}
```

### The four `models.ids` policies

`ModelIdsPolicy` (`crates/config/src/descriptors.rs`) controls how a provider
treats model IDs after prefix stripping and alias lookup:

| Policy | Behavior | Examples |
| --- | --- | --- |
| `catalog` | Resolves declared `aliases` and matches the provider's offerings (exact wire/canonical ID, unambiguous ASCII case-insensitive match, and — except on `zai` — unambiguous `/`-tail match for namespaced aggregators). Unknown IDs pass through unchanged with unknown catalog facts unless live `/models` discovery or `config.toml` supplies them. Default for compatible-host descriptors. | `google`, `anthropic`, `openrouter`, `orcarouter`, `together`, `deepinfra`, `fireworks`, `nvidia-nim`, `siliconflow`, `moonshot`, `zai`, `minimax`, `mistral`, `stepfun`, `opencode-zen`, all `descriptors[]` rows |
| `strict` | Accepts only declared `aliases`, known offerings for that provider, or IDs matching one of the `accept` globs (matched case-insensitively within `/` segments). Rejects any other ID with `RouteError::ForeignModelForDirectProvider`. | `deepseek`, `deepseek-anthropic` (`accept: ["deepseek*", "*/deepseek*"]`) |
| `roster` | Accepts only declared `aliases` and offerings on the provider's curated roster. Rejects unproven IDs — even when a custom `base_url` is set — with `RouteError::UnsupportedModelProtocol`. | `opencode-go` |
| `verbatim` | Never rewrites aliases or case-folds unknown strings after trimming and stripping `strip_prefixes`. Preserves the caller's exact wire spelling while still attaching catalog metadata when an exact offering exists. | `custom`, `codewhale`, `concentrate`, `openai`, `xai`, `meta`, `ollama`, `ollama-cloud`, `modelscope`, `qianfan`, `openmodel`, `atlascloud`, `telecomjs`, `edenai`, `zenmux`, `csdn`, `modelstudio-*` |

### Supporting fields in `models`

- **`accept`** (`string[]`): segment-scoped `*` wildcard globs evaluated against
  the lowercased candidate when `ids` is `"strict"`. Must be empty for all other
  `ids` policies.
- **`strip_prefixes`** (`string[]`): case-insensitive prefixes stripped once
  before lookup. Examples: `["models/"]` on `google` (so
  `models/gemini-4-hydrogen` from Google's model list resolves identically to
  `gemini-4-hydrogen`), `["opencode/", "opencode-zen/"]` on `opencode-zen`,
  `["opencode-go/"]` on `opencode-go` and `["concentrate/"]` on `concentrate`.
- **`aliases`** (`Record<string, string>`): maps a normalized lookup key
  (`normalize_model_lookup_key`: trimmed, ASCII-lowercased, with `_` and spaces
  folded to `-`) to the provider's target model spelling. Examples:
  `deepseek-chat` and `flash` → `deepseek-flash` on `deepseek`; `kimi` →
  `kimi-k2.7-code` on `moonshot`; `mimo-v2.5-pro` → `mimo-v2.6-pro` on
  `xiaomi-mimo`; `codestral-latest` → `mistral-code-latest` on `mistral`.
- **`wire_slugs`** (`Record<string, string>`): maps a normalized candidate ID to
  the vendor wire slug expected on that host's API while preserving the logical
  candidate name. Examples: `deepseek-v4-pro` →
  `deepseek-ai/DeepSeek-V4-Pro` on `siliconflow`, `together`, `deepinfra`,
  `huggingface`, `sglang` and `vllm`;
  `accounts/fireworks/models/deepseek-v4-pro` on `fireworks`;
  `deepseek/deepseek-v4-pro` on `openrouter`, `orcarouter` and `novita`;
  `inkling` → `thinkingmachines/inkling` on `together`.

### Compile-time validation (`crates/config/build.rs`)

`crates/config/build.rs` validates `provider_descriptors.json` at compile time
and fails the build if:

- any JSON object contains duplicate keys (`UniqueKeys`);
- `models` is missing, contains an unknown field, or names an unknown `ids`
  policy;
- `accept` is non-empty when `ids != "strict"`;
- any `aliases` or `wire_slugs` key is empty or not in normalized lookup form
  (`key.trim().to_ascii_lowercase().replace(['_', ' '], "-") == key`);
- any `aliases` or `wire_slugs` target is empty or has leading/trailing
  whitespace;
- a `verbatim` provider declares non-empty `aliases`;
- an alias target on a `strict` provider is neither a known offering nor matched
  by `accept`, or an alias target on a `roster` provider is not on its roster.

---

## 3. One resolver, one selection chain, and route-derived facts

### `resolve_model` step order

`codewhale_config::route::resolve_model(provider, raw, base_url)`
(`crates/config/src/route/resolver.rs`) returns
`Result<ResolvedModel, RouteError>`, where `ResolvedModel` carries `logical`
(the resolved provider-scoped model name), `wire_id` (the exact string sent on
the wire), `offering: Option<ProviderModelOffering>`, `model: Option<ModelId>`
(the canonical cross-provider catalog join, such as
`google/gemini-4-hydrogen`), `alias_matched` and `is_auto`.

It executes these steps in order:

1. **Trim and check `auto`.** Leading and trailing whitespace is removed; empty
   input returns `RouteError::EmptyModel`. An ASCII case-insensitive `"auto"`
   returns `is_auto: true` (`logical = "auto"`, `wire_id = "auto"`).
2. **Configured offerings and custom `base_url` verbatim rule.** The public
   `resolve_model` passes no configured offerings; `RouteResolver` and the
   config path `resolve_model_for_configured_route` pass the user-declared
   custom models, and for every policy except `roster` an exact match there
   wins immediately. Otherwise, when `base_url` is a
   custom endpoint for that provider
   (`provider_preserves_custom_base_url_model(provider, url)`), the trimmed ID
   is returned verbatim (`logical` and `wire_id` equal the trimmed string,
   attaching an exact-match offering if one exists). Custom-endpoint
   pass-through is decided here and nowhere else.
3. **Strip declared prefixes.** The first case-insensitively matching entry in
   `models.strip_prefixes` is stripped and the remainder is trimmed; an empty
   remainder (such as bare `"models/"`) returns `RouteError::EmptyModel`. If
   `models.ids` is `verbatim`, resolution finishes here with exact-match
   offering lookup.
4. **Look up aliases.** The stripped string is normalized with
   `normalize_model_lookup_key` and looked up in `models.aliases` (including
   active verified registry overlay aliases when enabled).
5. **Match offerings and `wire_slugs`.** `configured_offerings` is checked for
   the candidate, then the provider's catalog offerings are searched (exact wire
   or canonical ID first, then unambiguous ASCII case-insensitive match, then
   unambiguous `/`-tail match on `catalog` providers other than `zai`). If
   `models.wire_slugs` has an entry for the candidate, `wire_id` becomes the
   mapped vendor slug, `logical` stays the candidate, and offering metadata is
   joined on the slug. A `wire_slugs` hit returns here, before the `ids` policy
   in step 6 runs.
6. **Enforce `models.ids` and emit authoritative spelling.** `roster` without a
   matched offering returns `RouteError::UnsupportedModelProtocol`; `strict`
   without an alias match, offering match or `accept` glob match returns
   `RouteError::ForeignModelForDirectProvider`. Otherwise, a matched offering
   supplies its authoritative `wire_model_id` spelling (keeping an alias
   target's case when it matches the offering case-insensitively), and an
   unmatched `catalog` ID passes through as-is with `offering: None` and
   `model: None`.

### Unified configured-model selection chain (`CW-8250`)

Both `ConfigToml::resolve_runtime_options_with_secrets`
(`crates/config/src/config_resolve.rs`) and `Config::default_model`
(`crates/runtime/src/config.rs`) call one shared configured-model selection
chain in `crates/config`:

1. CLI `--model` override (`ModelSource::Cli`).
2. Process environment override (`CODEWHALE_MODEL`, legacy `DEEPSEEK_MODEL`, or
   the active provider's `<PROVIDER>_MODEL` when targeting that provider's
   endpoint; `ModelSource::Env`).
3. Provider table `[providers.<name>].model` (`ModelSource::ProviderConfig`).
4. Root `default_text_model` when it names a declared custom model; otherwise,
   only when `default_text_model` is unset, a legacy root `model` that names a
   declared custom model.
5. Moonshot Kimi Code default (`k3`) when using the Kimi Code endpoint or
   imported Kimi token auth (`ModelSource::ProviderDefault`).
6. Root `default_text_model`, guarded so a stale DeepSeek ID left behind after
   switching providers is not forwarded to a foreign direct provider
   (`ModelSource::RootDefaultTextModel`).
7. Legacy root `model`, consulted only when `default_text_model` is unset, with
   the same guard (`ModelSource::RootModel`).
8. Route default (`ModelSource::ProviderDefault`): the verified `cloud_facts`
   route default, then the ChatGPT/Codex OAuth preferred roster model, and
   finally the descriptor's `default_model`.

The chosen raw string is then passed through
`resolve_model_for_configured_route`, which checks declared custom models
before calling `resolve_model(provider, raw, Some(base_url))`. When `resolve_model` rejects an
ID (such as an unknown non-DeepSeek ID on `deepseek`, an unlisted ID on
`opencode-go`, or an empty string after prefix stripping), configuration loading,
`codewhale model resolve`, the TUI and the Runtime API all fail loud with the
same error naming the provider and model ID instead of silently sending the raw
string.

### How limits, capabilities and prices follow the resolved route (`CW-8251`, `CW-8252`)

Downstream subsystems read the resolved route and catalog model facts rather
than re-normalizing strings or maintaining separate per-provider tables:

- **Context windows, max output, thinking and payload mode**
  (`crates/engine/src/provider_lake.rs`): derived from `resolve_model`'s matched
  offering and canonical catalog model facts plus the provider's `wire_policy()`
  (retaining only genuine route-protocol rules: explicit `config.toml`
  `model_context_windows` / `context_window` overrides, Kimi Code safe floor,
  `openai-codex` preview budget, Anthropic Messages 200K/64K unknown-model
  floor, local `ollama` 8,192 fallback and 128,000 unknown fallback).
- **Unknown-output-ceiling route budgets**
  (`crates/engine/src/route_budget.rs`): read descriptor and catalog metadata
  instead of hard-coded provider allowlists; when a route publishes no output
  ceiling, Codewhale omits artificial `max_tokens` clamps rather than inventing
  a number.
- **Google Gemini thought signatures and thinking controls**
  (`crates/engine/src/client/chat.rs`): resolve the model through
  `resolve_model(ProviderKind::Google, model, None)` (applying `models/` prefix
  stripping and aliases) and read `thought_signatures_required` and
  `thinking_can_disable` from reviewed intrinsic catalog facts in
  `catalog_corrections.json` rather than prefix-matching raw strings.
- **Pricing** (`crates/engine/src/pricing.rs`): resolves logical model IDs via
  `resolve_model` so aliases and vendor wire slugs price against the same model
  the request sends. Time-of-day and retirement-cutoff pricing rules (such as
  DeepSeek windows and product-keyed subscription routes) remain explicit Rust
  policy.
- **Interactive and auxiliary selectors** (`CW-8252`): TUI `/model` shorthands,
  Runtime API config normalization (`crates/app-server/src/runtime_api.rs`),
  subagent model selection, speech/vision model selection and
  `route_preferences` all delegate directly to `resolve_model`.

Catalog source precedence (from layer 0 bundled seed up through layer 20 live
provider `/models` rosters, layer 25 account rosters and layers 30–40 user
config overrides) is documented in [Catalog refresh](CATALOG_REFRESH.md#layers-lowest--highest-priority).

---

## 4. Signed `registry.json` (`codewhale.registry.v1`) updates

When `[cloud_facts]` is enabled (`enabled = false` by default; overridden by
`CODEWHALE_CLOUD_FACTS=1|0` and hard-disabled by
`CODEWHALE_DISABLE_CLOUD_FACTS=1`), Codewhale can pull signed updates to the
models-and-providers registry without waiting for a binary release
(`crates/config/src/cloud_facts/registry.rs`,
`crates/cloud-facts/src/registry.rs`).

### Channel, endpoint and cache

- **Envelope and channel:** UTF-8 JSON bytes of `registry.json` are signed inside
  the standard Ed25519 `facts/v1` envelope (`channel: "registry"`, `envelope: 1`,
  `alg: "ed25519"`, maximum payload `8 MiB`). See
  [Signed cloud facts](CLOUD_FACTS.md) for the byte-level signature format and
  pinned trust table (`crates/config/src/cloud_facts/keys.rs` and
  `web/lib/cloud-facts/keys.ts`).
- **Source URL:** defaults to `https://codewhale.net/api/facts/v1/registry` (or
  `CODEWHALE_CLOUD_FACTS_URL` when it contains `{channel}`), overridable with
  `CODEWHALE_REGISTRY_URL` or a local signed envelope at
  `CODEWHALE_REGISTRY_PATH`. Network fetches are suppressed automatically in CI.
- **Disk cache and rollback floor:** verified envelopes are written atomically to
  `$CODEWHALE_HOME/facts/registry-cache.json`; the cache is reset when the
  source changes. No bundled `registry.json` sets an initial floor: the floor is
  the highest revision this client has cached. Startup
  loads a verified cached copy without blocking on the network; background
  refresh and `/model refresh` update it. `envelope.facts_version` must equal
  `registry.revision` (`>= 1`), `envelope.published_at` must equal
  `registry.generated_at` (RFC 3339 UTC ending in `Z`, not in the future beyond
  clock tolerance), `sha256` must match, and `revision` must be greater than or
  equal to the highest revision previously accepted. Failed or rejected pulls
  preserve the active copy, and a rejected cache file is cleared while its
  revision floor is kept. Disabling `[cloud_facts]`, setting
  `CODEWHALE_DISABLE_CLOUD_FACTS=1`, expiry past `not_after` or an invalid facts
  source drops the active registry overlay.

### Payload shape (`codewhale.registry.v1`)

A verified `registry.json` document carries:

- `schema`: `"codewhale.registry.v1"`
- `revision`: monotonic positive integer (`u64 >= 1`)
- `content_hash`: `"sha256:<64 lowercase hex>"` over canonical key-sorted
  `{ dialects, engine_compat, models, offerings, providers }`
- `generated_at`: RFC 3339 UTC timestamp (`...Z`)
- `providers`: ordered built-in and `origin: "descriptor"` provider rows
- `models`: canonical model fact rows ordered by `id`
- `offerings`: provider × wire-ID offerings ordered by `(provider, id)`
- `dialects`: Responses dialect object matching `responses_dialects.json`
  (without `schema_version`)
- `engine_compat`: lossless Engine round-trip projection

When active in `crates/config/src/cloud_facts/overlay.rs`, verified pulled
models, offerings, built-in `models.aliases` / `wire_slugs` updates and admitted
pulled providers take precedence over bundled seed rows in `resolve_model`,
`RouteResolver`, catalog lookups and provider discovery, and the config-crate
resolvers revert to the seed when `overlay::clear()` runs. The Engine's
process-wide catalog snapshot (`BUNDLED_SNAPSHOT` in
`crates/engine/src/provider_lake.rs`) is still built once, on first use, so
the Engine picker and route catalog keep whatever registry was active then.

### Key roles: `automation` vs `founder`

Pinned keys enforce strict separation between routine model updates and provider
admission:

- **`automation` key (`KeyRole::Automation`):** accepted on the `registry`
  channel only when the signed payload carries a valid, non-expired `not_after`
  within the bounded automation window (`MAX_AUTOMATION_WINDOW_SECS`, at most 7
  days after `generated_at`). Its scope is enforced at verification
  (`automation_provider_violation` and `automation_offering_violation` in
  `crates/config/src/cloud_facts/registry.rs`):
  - **May change:** `models` (canonical model facts), `offerings`, and a
    built-in provider row's `models.aliases` and `models.wire_slugs`. On a
    `strict` provider an alias or wire slug admits the mapped ID without an
    `accept` match; the host, key and protocol stay the release's.
  - **Must not change:** every `providers[]` row must name a bundled
    `provider_descriptors.json` provider or descriptor and carry no
    `admission.founder_signature` of any value. Any other provider field that is
    present must equal the bundled value: the frozen trust fields (`kind`,
    `base_url`, `wire_policy`, `wire`, `auth`, `api_key_env`, `env_vars`,
    `secret_store_slot`), `default_model` (route and descriptor defaults),
    `models.ids`, `models.accept`, `models.strip_prefixes`, and every other key
    such as `label`, `catalog` (`api`/`npm` headers) or `corrections`. An
    offering may not set `pricing_withheld`, carry `corrections`, or set a
    provider default (`default` / `default_for_provider` at the top level, in
    `catalog` or in `transport`).
  Anything outside that scope rejects the whole envelope
  (`FactsRejection::OutOfScope`).
- **`founder` key (`KeyRole::Founder`):** may sign everything above, newly
  admitted OpenAI-compatible descriptor providers, and the naming-rule, default,
  header, correction and `pricing_withheld` changes an automation key may not
  make. Even a founder-signed row that names a built-in provider cannot change
  its address, auth or wire protocol: `compile_registry` reads only its
  `models`, `catalog`, `corrections` and `default_model`.

### Pulled provider admission, SSRF and credential isolation

Pulled data can **never** delete a built-in or bundled provider or alter an
existing provider's `kind`, `base_url`, `auth`, `env_vars`, `secret_store_slot`,
`config_key` or `wire_policy`. A new provider row in a founder-signed
`registry.json` is admitted (`admit_registry` in
`crates/config/src/cloud_facts/registry.rs`) only when every check passes:

1. **Data-only OpenAI-compatible wire:** `kind` is absent and `wire_policy`
   (and `wire`, when present) is `"openai-compatible"`. Pulled providers use
   OpenAI-compatible Chat Completions only; Responses `dialects` remain compiled
   wire behavior in `crates/config`.
2. **Collision-free slug:** `id` (and every entry in `aliases`) matches
   `^[a-z][a-z0-9-]{0,47}$` with no trailing `-`, and does not collide (after
   normalizing `[_.]` to `-`) with any built-in or bundled provider ID, config
   key, secret slot, catalog ID, `tui_wire_tag`, `legacy_tui`, selector/subagent
   alias or earlier row in the same payload.
3. **SSRF-safe public HTTPS endpoint:** `base_url` is at most 512 characters,
   uses `https://` on a multi-label public domain name with the default port
   (`443` / omitted) and no userinfo, query or fragment. IP literals,
   single-label hosts, `localhost`, `*.localhost`, `*.local` and `*.internal`
   are rejected at admission. That check is static: a public host name that
   resolves to a private address is not caught when the row is admitted.
   At request time, the Engine's model client (`http_client_builder_with_auth_mode`
   in `crates/engine/src/client.rs`) builds a `custom` provider whose
   `base_url` host equals the host of a provider in the active verified registry
   on `codewhale_net::guard::public_address_reqwest_client_builder`: ambient
   proxies are off and every connection resolves the host through
   `PublicAddressResolver`, which refuses an answer containing a private,
   loopback, link-local, cloud-metadata or other restricted address
   (`is_restricted_ip`), including after a DNS rebind. This covers the chat,
   HTTP/1.1 fallback and `/models` clients of that route. It does not cover a
   saved custom provider whose registry row is no longer active (expired,
   disabled or removed), which then uses the ordinary platform client like any
   user-configured endpoint, or any request that does not go through the Engine
   model client.
4. **Strict credential isolation:** `auth.secret_slot` may only be `<id>`, and
   `auth.env_vars` may only be `["<UPPER_ID>_API_KEY"]` (with `-` replaced by
   `_`). That variable must not start with `CODEWHALE_` or `DEEPSEEK_`, match
   any built-in provider's environment variable, or match a product or search
   credential (`PRODUCT_RESERVED_ENV_VARS`: for example `TAVILY_API_KEY`,
   `EXA_API_KEY`, `GEMINI_API_KEY`, `OLLAMA_API_KEY`, `ZHIPU_API_KEY`). A
   pulled provider is inert until the user supplies a key in its own secret slot
   or derived `<UPPER_ID>_API_KEY`; it never borrows another provider's
   credentials. Account-scoped availability and billing overlays are never
   accepted in public registry rows.

A refused row is kept on the admitted registry with its reason
(`AdmittedRegistry::refused`), and every offering that names it is dropped.

---

## 5. How to add a model or a provider

### Add or update a model on an existing provider (data-only)

No Rust edits are needed when adding a model, alias, prefix rule or capability
fact to an existing provider:

1. **Model facts and seed selection:**
   - If the model is in upstream Models.dev, reference it in
     `scripts/catalog/models_dev_seed.toml` (or run
     `python3 scripts/catalog_models_dev.py seed discover --write`) and add any
     needed holds or source-verified overrides in
     `crates/config/assets/catalog_corrections.json`.
   - Record reviewed intrinsic facts (`context_window`, `max_output`,
     `reasoning`, `thought_signatures_required`, `thinking_can_disable`,
     modalities) and their vendor doc URLs in `reviewed.intrinsic`,
     `reviewed.public_models` and `reviewed.source_receipts` inside
     `crates/config/assets/catalog_corrections.json`.
2. **Naming rules:** add any shorthand, retired-ID redirect, `strip_prefixes`
   entry or aggregator `wire_slugs` mapping to that provider's `models` block in
   `crates/config/assets/provider_descriptors.json`.
3. **Regenerate and verify:**
   ```sh
   python3 scripts/catalog_models_dev.py seed render
   python3 -m unittest scripts/catalog_models_dev_test.py
   python3 scripts/check-provider-registry.py
   (cd web && node scripts/derive-facts.mjs && node scripts/check-facts.mjs)
   ```
   Then run `codewhale-config` and `codewhale-runtime` unit tests (including
   `config::model_resolution_golden`).

### Add a plain OpenAI-compatible provider (data-only)

- **For your own machine or project (no repo changes):** add a
  `[providers.<name>]` table in `~/.codewhale/config.toml` with
  `kind = "openai-compatible"`, `base_url`, `api_key_env` and `model` (or paste
  the base URL and key in `/provider`). See
  [Configuration](CONFIGURATION.md#custom-openai-compatible-gateways).
- **As a bundled compatible host in Codewhale (JSON + docs, zero Rust):** add an
  entry to `"descriptors"` in `crates/config/assets/provider_descriptors.json`
  with `id`, `label`, `wire: "openai-compatible"`, `base_url`, `api_key_env`,
  `default_model`, `discovery` (`"models_endpoint"`, `"public_models_endpoint"`
  or `"none"`), optional `aliases` / `docs_url` / `credential_url` / `guidance`,
  and a `models` block (`"ids": "catalog"`, `accept: []`, `strip_prefixes: []`,
  `aliases: {}`, `wire_slugs: {}`). Document the host in `docs/PROVIDERS.md`
  (and `docs/features.toml` when user-facing), regenerate website facts with
  `(cd web && node scripts/derive-facts.mjs)` and run
  `python3 scripts/check-provider-registry.py`. Do **not** add a `ProviderKind`
  enum variant for a standard OpenAI-compatible host.

### When Rust changes are required

Write Rust code only when a provider introduces a distinct wire protocol,
authentication mechanism (such as OAuth PKCE or device-code sign-in), or
provider-specific billing/time-of-day calculation — and key that behavior off a
descriptor or catalog field rather than scattering provider-name matches across
crates.

---

## 6. Planned, not shipped yet

The Engine-side single resolver, unified configured-model chain, route-derived
facts and signed `registry.json` pull client are in tree. The cross-repository
cutover making the Platform database the sole upstream authoring home remains
**planned** (not deployed or active by default in this checkout):

- **Platform registry database (`CW-8249`):** database registry tables on the
  Codewhale Platform with reviewed admission and source receipts, importing the
  Engine's catalog assets and exporting deterministic `registry.json`
  (`codewhale.registry.v1`), while keeping account-scoped availability and
  billing overlays outside the public export.
- **Signed publication and snapshot sync (`CW-8253`):** the website relay
  (`web/app/api/facts/v1/[channel]/route.ts`, serving `registry`) and publisher (`web/scripts/facts-publish.mjs`)
  that verify and serve the `registry` channel are in tree; activating the
  public channel row, founder signing, and a scheduled sync job that updates the
  Engine's bundled snapshot via pull request remain gated on signing-key custody
  and deployment authorization.
- **Platform consumer migration:** migrating Platform API (`/v1/models`) and
  web/account surfaces to read the same registry and deleting their separate
  hand-edited model and capability lists. The Engine already reads account
  `/v1/models` `codewhale.pricing` prices (`CW-8254`).
- **Engine asset cutover (`CW-8255`):** loading the bundled `registry.json`
  export directly at Engine build/startup and retiring the split Engine seed
  files (`models_dev.bundled.json`, `catalog_corrections.json`,
  `scripts/catalog_models_dev.py`) and `.github/workflows/catalog-refresh.yml`
  once Models.dev moves behind Platform intake.
