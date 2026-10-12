# Configuration

codewhale reads configuration from a TOML file plus environment variables.
At process startup it may also load literal built-in-provider credentials from
a workspace-local `.env` file. Use the tracked `.env.example` as the template;
copy it to `.env`, then add only credential values.

A workspace is not configuration authority. Codewhale therefore ignores
config/profile/home paths, provider/model/base-URL routing, MCP/plugin state,
approval/sandbox/shell posture, executable paths, runtime settings, and every
other non-credential `.env` entry. Variable expansion is rejected so a
repository cannot substitute an ambient secret into a credential value. Use
`config.toml`, CLI flags, or values exported by the launching shell for those
explicit control-plane settings. `.env` is read from a stable regular-file
handle, is capped at 1 MiB, and symbolic links, reparse points, and multiply
linked files are rejected.

## Reading and checking configuration from the CLI

`codewhale config get <key>` reads scalar keys, whole tables such as `tools`,
and nested paths such as `tools.user_input_timeout_seconds`. Displayed tables
and nested values apply the same recursive credential redaction as `config dump`.

`config set` supports its named scalar keys and the provider, route, and
notification commands. It refuses a key that nothing reads and suggests the
nearest real key (`config set calm_mod on` names `calm_mode`). A settings.toml
key such as `calm_mode` or `tool_collapse` is validated and written to the
user-global settings.toml, not config.toml; `config get` reads it back from
there even when an old config.toml copy (which nothing reads) is still present,
and names that copy so you can `config unset` it. Settings keys have no project
scope, so `--project` refuses them. A config.toml key is checked against the
type its reader expects and stored as a TOML boolean or number where the
reader needs one (`yolo = true`, `max_subagents = 4`); `reasoning_effort`
accepts the same aliases as `/effort` (#6563). Other dotted writes fail before
modifying the file and name the TOML table to edit. For example, set a tools
timeout in the file as:

```toml
[tools]
user_input_timeout_seconds = 0
```

`codewhale config doctor` checks credential presence and endpoint shape, and
warns about each config.toml root key that nothing reads: a settings.toml key
left in config.toml is named as misplaced, and any other unread key gets a
did-you-mean. Keys read by any runtime reader are not reported. The warnings do
not fail the check, and a clean result does not validate every runtime setting's
value (#6083, #6563).

## Constitution, project instructions, and repo authority

Codewhale has several instruction surfaces. They are deliberately separate so a
personal constitution, repo policy, project instructions, and runtime security
controls do not blur together.

- **Bundled global Constitution** — the compiled base law in the binary. It is
  the default floor for every session.
- **User-global constitution** — the normal guided setup output. Manage it with
  `/constitution` or `/setup`; Codewhale stores structured data at
  `$CODEWHALE_HOME/constitution.json` (default `~/.codewhale/constitution.json`)
  and renders it into a separate `<codewhale_user_constitution>` prose block.
  This can express preferences and stop conditions, but it does not change
  runtime approval policy, sandbox, shell, network, trust, or MCP permissions.
- **Repo-local constitution** — optional project policy in
  `.codewhale/constitution.json`, described below.
- **`AGENTS.md`** — cross-agent **project instructions** (prose). This is the
  canonical file for "how should an agent work in this repo." Run `/init` to
  scaffold one. `CLAUDE.md` and `.claude/instructions.md` are read as
  compatibility fallbacks.
- **Memory and handoffs** — recalled state. Useful, but lower authority than
  constitutions and project instructions.

### Managing the user-global constitution (`/setup` and `/constitution`)

The bundled **working agreement** is the safe default and no longer adds a
required first-run screen. Customize it later through `/constitution` or the
progressive `/setup` guide. Provider/model readiness, workspace trust, and
runtime posture stay separate from this guidance.

On the **Working agreement** (constitution) step:

- **`1`–`6`** tune the guided draft. **`G`** previews it, and **`G`** again
  ratifies and saves a fresh structured `constitution.json`.
- **`A`** (shown only when a provider is configured) asks your first configured
  model to draft the constitution. Drafting is **not** saving: the draft is
  rendered through the same preview and you still press **`G`** to ratify
  before anything persists.
- **`K`** keeps your existing loaded constitution unchanged (shown only when a
  valid file is already present).
- **`U`** (or `/constitution bundled`) records the bundled/default law.

`/constitution` (alias `/law`) is the primary management surface once you are
set up. Subcommands: `status` (the default), `preview`, `review`, `repo` (the
repo-local law block), `explain`, `base`, `suggestions`, `ratify <id>`,
`migrate`, `rollback`, `edit`/`guided`, `repair`, `posture`, and `bundled`.
Managing the constitution never changes runtime approval, sandbox,
shell, network, trust, default mode, or MCP authority — those stay in runtime
posture/config.

Each repo can carry two distinct, complementary files:

- **`AGENTS.md`** — ordinary project working instructions.
- **`.codewhale/constitution.json`** — Codewhale-specific **repo authority /
  prioritization policy**: when local sources conflict, which should Codewhale
  trust first, and what to verify before claiming a task is done. `.codewhale/`
  lives inside the repo (like `.github/`). Example:

  ```json
  {
    "schema_version": 1,
    "authority": [
      "current user request",
      "live code and tests",
      "GitHub issue/PR details",
      "AGENTS.md",
      "memory",
      "old handoffs"
    ],
    "protected_invariants": [
      "do not break old-session transcript replay"
    ],
    "branch_policy": "PRs target the integration branch, not main",
    "verification_policy": {
      "before_claiming_done": ["run focused tests", "read changed files back"]
    },
    "escalate_when": [
      "a destructive action was not explicitly authorized"
    ]
  }
  ```

  All fields are optional. When present, the file is rendered into the system
  prompt as concise prose in a higher-authority block. Legacy `WHALE.md` files
  are ignored and reported as migration-only diagnostics.

  Each `protected_invariants` entry may be either a plain string (advisory
  prose, the historical shape) or an object carrying path globs, which is
  additionally **mechanically enforced** in the tool gate. See
  [Enforced repo-law invariants](#enforced-repo-law-invariants) below.

  This is the **repo-local law** layer. When guidance conflicts, the
  **Whose word wins** section of the effective base constitution owns the
  ordering; the bundled source is
  [`BASE_PROMPT`](../crates/tui/src/prompts/text.rs). Use `/constitution base`
  to inspect the effective base, including any opted-in expert override.
  The order in which these files are described or assembled is not an
  authority ranking. Runtime policy (permissions, sandbox and cost limits
  enforced in code) is separate from prompt guidance; editing a constitution
  does not grant permissions.

> **`WHALE.md` is deprecated.** It overlapped confusingly with `AGENTS.md`.
> Codewhale no longer reads `WHALE.md` as project or global context. If one is
> present, setup/context diagnostics report it as ignored so you can migrate it.
> Move ordinary instructions to `AGENTS.md` and Codewhale-specific authority
> policy to `.codewhale/constitution.json`. Personal standing guidance belongs
> in `/constitution` / `$CODEWHALE_HOME/constitution.json`. (The global
> Codewhale Constitution shipped in the model prompt is a separate thing and is
> unaffected.)

### Enforced repo-law invariants

By default a `protected_invariants` entry is advisory prose: it is rendered into
the prompt as guidance the agent should honor, but nothing stops a write. An
entry written as an **object with `paths`** is different — it compiles into a
mechanical write hold that the engine's tool gate evaluates before the write
runs. The law becomes mechanism, not just a request.

An enforced entry has this shape:

```json
{
  "schema_version": 1,
  "protected_invariants": [
    "Keep DeepSeek support first-class.",
    {
      "text": "The wire format is frozen; protocol changes need a human.",
      "paths": ["crates/protocol/**"],
      "action": "block"
    },
    {
      "text": "Release notes need human review.",
      "paths": ["CHANGELOG.md"],
      "action": "ask"
    }
  ]
}
```

- `text` — required. The reason surfaced on the hold. An empty `text` is skipped.
- `paths` — workspace-relative globs (globset syntax, e.g. `crates/protocol/**`,
  `**/secrets.toml`, `CHANGELOG.md`). An object with no usable `paths` stays
  advisory-only despite the object shape.
- `action` — optional, defaults to `ask`. `ask` force-prompts in Ask and
  Auto-Review; in Full Access it denies the protected write without opening a
  modal. `block` **denies the write outright** in every posture.

Semantics:

- **Tighten-only.** The schema has no allow/widen shape, so law can only *add*
  holds — a crafted constitution can never grant authority or weaken a gate
  above it.
- **Not bypassable by mode.** Like the built-in safety floor, an `ask` hold
  force-prompts in Ask and Auto-Review. Full Access never opens approval
  modals, so the same hold fails closed as a hard block; `block` always denies.
  Mode cannot turn a hold off.
- **Repo-local only.** Only the repo's `.codewhale/constitution.json`
  participates. The user-global constitution stays advisory prose and never
  reaches this mechanism.
- **Fails safe.** A missing file, parse error, or invalid glob degrades to
  fewer or zero rules — never a hold on unprotected paths and never a poisoned
  gate. Across matches the strongest action wins, so `block` outranks `ask`.
- **Leaves a receipt.** Every hold emits a `tool.repo_law_decision` tool-audit
  event naming the invariant, the matched path, and the source file; the
  approval/denial reason names the invariant too.

**Coverage is deliberately limited.** Holds are evaluated only for the write
tools `write_file`, `edit_file`, `apply_patch`, and `fim_edit`, and only
against the filesystem targets named in their inputs (`path`/`target`/
`destination`/`file_path`, `changes[].path`, and unified-diff /
`apply_patch`-envelope headers). A shell command that writes a protected path is **not** held by
repo law — those writes are still governed by the ordinary approval, sandbox,
and shell-write gates, not by this mechanism.

### Expert full base-prompt override (#3638)

The global Constitution (the base system prompt, normally compiled in from
`crates/tui/src/prompts/text.rs` as `BASE_PROMPT`) can be replaced per-user
without rebuilding. This is
an expert escape hatch, not the normal `/constitution` guided setup output.
Because this is a prompt trust boundary, it takes **two deliberate steps** — a
file alone is not enough:

1. Drop the replacement at `~/.codewhale/prompts/constitution.md` (under
   `$CODEWHALE_HOME` when set).
2. Set the explicit opt-in flag `CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE=1`
   (`true`/`on`/`yes` also accepted).

If the file exists but the flag is unset, the override is **ignored** (with a
log line pointing to the flag) and the bundled Constitution stays in place.
This is intended for repurposing the TUI beyond software engineering — e.g.
long-form writing or document review — where the engineering-oriented base
prompt is a poor fit. It is loaded once at startup; a **missing or empty file
is a no-op**, so existing installs keep the bundled prompt.

Scope is deliberately narrow: only the byte-stable **base prompt segment** is
overridable. Mode deltas, the approval policy, the tool taxonomy, Context
Management, and the Compaction Relay are still owned by Codewhale's runtime
assembly, so an override **cannot remove safety-relevant guidance** (sandbox,
approvals) — it only swaps the task/voice framing. To customize ordinary
personal behavior, prefer `/constitution`; to customize per-repo behavior,
prefer `AGENTS.md` + `.codewhale/constitution.json` above.

## Where It Looks

Default config path:

- `~/.codewhale/config.toml`
- Legacy fallback: `~/.deepseek/config.toml`

Overrides:

- CLI: `codewhale --config /path/to/config.toml`
- Env: `CODEWHALE_CONFIG_PATH=/path/to/config.toml`
- Legacy env alias: `DEEPSEEK_CONFIG_PATH=/path/to/config.toml`

If both are set, `--config` wins. Environment variable overrides are applied after the file is loaded.

### TUI editability audit

Inside the TUI, run `/config audit` to see which documented keys can be changed
from the current session, which ones can also be persisted, and which ones stay
file-only or restart-only. The audit includes current values for the high-impact
runtime controls such as `approval_policy`, `allow_shell`,
`stream_chunk_timeout_secs`, `base_url`, `mcp_config_path`, and the
`[subagents]` concurrency/depth/timeout keys.

Use the command's "Command / reason" column as the source of truth before
editing by hand. For example, `/config approval_mode on-request --save` writes
top-level `approval_policy = "on-request"`, while provider base URLs are saved
but still require restarting the model client.

### User workspace entries

Interactive Agent sessions expose shell tools by default with approval gating
unless you explicitly disable them. For a shell opt-in that should live in the
user's global config for noninteractive or durable-task profiles rather than in
the repository, add a workspace-scoped entry:

```toml
[workspace.'/absolute/path/to/project']
allow_shell = true
```

The entry applies only when the launched workspace path matches the table key.
The legacy `[projects."/absolute/path/to/project"]` table is also accepted for
this user-owned override.

In interactive mode, the per-project overlay
`<workspace>/.codewhale/config.toml` is applied after this user entry. A
project-level `allow_shell = false` can still tighten the session; project-level
`allow_shell = true` is ignored.

### Per-project overlay (#485)

When the TUI starts in a workspace that contains a regular-file
`<workspace>/.codewhale/config.toml`, the safe values declared in that file are
merged on top of the global config. Legacy
`<workspace>/.deepseek/config.toml` files are still read when the Codewhale path
is absent. Symlinked project config files are rejected. This lets a repo suggest
a model or tighten local safety posture without touching the user's
`~/.codewhale/config.toml`. Pass `--no-project-config` to skip the overlay for
one launch.

Supported keys in the project overlay (top-level fields only):

| Key | Effect |
|---|---|
| `model` | override `default_text_model` |
| `reasoning_effort` | force `"high"` / `"max"` for a complex repo |
| `approval_policy` | only values that tighten the user's current permission posture |
| `sandbox_mode` | only values that tighten the user's current sandbox posture |
| `max_subagents` | clamp sub-agent concurrency for a constrained repo (clamped to 1..=128) |
| `allow_shell` | `false` can disable shell access; `true` is ignored |

The overlay is intentionally narrow — it covers the fields a repo
maintainer is most likely to want to standardize across contributors.
Credential, endpoint, provider-selection, MCP config, hooks, skills,
retry, hotbar bindings, and `instructions = [...]` settings stay user-global.
If a repo-local config declares `api_key`, `base_url`, `providers`, `provider`,
`mcp_config_path`, `notes_path`, `hotbar`, `allow_shell = true`, or `instructions`,
Codewhale ignores that key and keeps the user's global setting.

The consolidated `codewhale` runtime uses `~/.codewhale/config.toml` for
provider and model settings. `codewhale auth set --provider deepseek` saves
the key in the local secret store (a private file under `~/.codewhale/secrets/`
by default; the OS keychain is an explicit option), rather than writing it
into that config file. Legacy `~/.deepseek/config.toml` is migrated on first
launch when needed. `codewhale --model deepseek-v4-flash` is forwarded to the
TUI as `CODEWHALE_MODEL`. The dispatcher no longer writes `DEEPSEEK_*` twins;
a value you set yourself remains a legacy alias when its `CODEWHALE_*`
equivalent is unset.

### Account provider keys

A Codewhale account lets you **manage your provider API keys in one place**.
[Register](https://app.codewhale.net/register) or [sign in](https://app.codewhale.net/login),
then add, replace or remove keys in [Providers settings](https://app.codewhale.net/providers)
or from the terminal:

```bash
codewhale login                         # browser sign-in; also: account login
codewhale account keys set deepseek      # hidden prompt; add or replace a key
codewhale account keys list              # configured status, never secret values
codewhale --provider codewhale           # use the account's model route
```

Account keys stay on the service and are used for account-routed requests.
Sign in on another device and choose Codewhale in `/provider` to use them
without pasting keys again. `/models` lists models available to that account.
Replacing a saved account key affects subsequent requests through this route;
it does not change independent local copies or revoke keys at the provider.
`codewhale account keys remove deepseek` removes the account's saved key.

Login does **not** upload existing local keys. If you want to copy one, choose
`codewhale account keys set deepseek --from-local` explicitly. Normal local
provider setup remains `codewhale auth set --provider <provider>`; local keys
and local models work without a Codewhale account. Signing in preserves an
explicit or already configured local provider. Choose `--provider codewhale`
when you want the account route instead.

The account session is separate from provider keys. `codewhale account status`
inspects it; `codewhale account logout` removes it for the selected `--profile`
without removing your provider keys. Account sessions use the private `0600`
Codewhale secrets file, scoped to the profile and account API origin; they do
not use the OS keychain. The older `codewhale cloud ...` spelling remains an
alias for account commands.

### Portable config bundles

`codewhale config export --portable [--project] [--out FILE]` writes a
portable, secret-free bundle of your configuration: sorted TOML with
credential and machine-specific keys (API keys, base URLs, socket paths)
dropped, never a redacted placeholder in their place. Without `--out` the
bundle goes to stdout. Typed tables, arrays, numbers, booleans, and datetimes
remain typed. Machine-bound authority is deliberately non-portable: project
trust overlays, credential readers, auto-running hooks, executable LSP
definitions, and local path bindings are omitted rather than copied to a new
host. So is trust posture (`yolo`, `allow_shell`, `approval_policy`,
`sandbox_mode`, `sandbox_network_access`, and the sandbox read-denylist
paths) and local endpoints or executables (`[lifecycle_outbox]`,
`[extension_host]`, `[control_socket]`). URLs that carry a credential —
userinfo, a token-named query parameter, or a Slack/Discord/Teams webhook
path — are treated as credentials.

`codewhale config import <FILE|HTTPS_URL|-> [--dry-run] [--yes] [--project]`
applies a bundle. The envelope is strict (`schema_version = 1`, kind
`codewhale.portable-config`; unknown fields fail). Import prints a
deterministic plan — added / changed / skipped / conflicting / rejected —
then asks for consent unless `--yes` is given; headless use requires it.
Credential-shaped entries are rejected by key name and by value shape;
rejections name the field, never a value. Remote bundles come from HTTPS
only (loopback http excepted) with a 5 MiB cap. Application backs up the
target document to `<config>.bundle-backup-<timestamp>-<random>`, rolls back on any
failure, and re-importing an applied bundle changes nothing.

Import also rejects the non-portable authority classes omitted by export,
including nested/camel/dotted credential keys and cookie headers. This keeps a
hand-authored or remote bundle from reintroducing machine trust, local
credential access, or automatically executable commands that a local export
would refuse to carry. Structured tables are deep-merged: a portable model or
preference update does not erase target-local provider credentials, endpoints,
hooks, or executable definitions that were deliberately omitted from the
bundle. Arrays and scalar values still replace the corresponding portable
value.

Sections map to scope: `[project]` entries land only in the workspace
document (`--project`, which must target an actual workspace config),
`[global]` only in the user-global one; `preferences`, `profiles`, and
`plugins` apply at either scope. A global bundle operation refuses a workspace
document just as a project operation refuses the user-global document.

### Credential read precedence (#5197)

Credential reads are **folder-independent by default**: every layer below is
user-global or process-scoped, so a key saved in one repo resolves identically
in every other repo. Repo-local config never carries credential material — the
project overlay above reads only its allowlisted keys and ignores `api_key`,
and a credential write aimed at a workspace-scoped config is rescoped to the
user-global `~/.codewhale/config.toml` (#5045, #5193).

For the active provider, the runtime resolves the API key in this exact order
(first match wins):

1. **Route-specific auth contract.** Routes whose `auth_mode` disables API
   keys stop here with no credential. OAuth routes use their explicitly
   granted token: the official `openai-codex` plan route accepts only
   Codewhale-owned credentials from `codewhale auth chatgpt`, bound to its
   issued registration and verified account. Imported Codex CLI and process
   tokens cannot authorize this route;
   `[providers.xai] auth_mode = "oauth"` reads Codewhale's own xAI
   device-login store (or a consent-granted Grok CLI file).
2. **Explicit CLI key.** `--api-key` forwarded with its source marker wins
   over every saved slot.
3. **Config file `api_key`.** The `[providers.<name>] api_key` table slot for
   the active provider. `deepseek-CN` also reads `[providers.deepseek]`. An
   older top-level `api_key` is read as `[providers.deepseek] api_key` (see
   [Legacy top-level `base_url` and `api_key`](#legacy-top-level-base_url-and-api_key)).
   File-owned keys stay bound to their file-owned endpoint: when the
   environment replaces the route's base URL with a custom host, the saved
   key is not sent there.
4. **`api_key_env` binding.** `[providers.<name>] api_key_env = "VAR"` reads
   the named environment variable. For custom providers an unset or empty
   binding is a loud error, not a silent fallback (#5104).
5. **Secret store.** The durable per-provider slot written by
   `codewhale auth set` (file-backed under `~/.codewhale/secrets/` by
   default; the OS keyring only when explicitly selected). Skipped for named
   custom routes, self-hosted providers, custom endpoints other than an
   explicitly authenticated loopback, and routes whose `auth_mode` needs no
   key.
6. **Ambient environment.** The provider's own variable
   (`DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, `MOONSHOT_API_KEY`, …).
   Ambient keys are only ever sent to the provider's official endpoint and
   are skipped under the same conditions as the secret store.
7. **Keyless fallback.** Self-hosted providers and loopback endpoints may run
   with no credential; every other route fails with provider-specific setup
   guidance.

Legacy compatibility: `~/.deepseek/config.toml` is migrated into
`~/.codewhale/config.toml` on first launch, `DEEPSEEK_*` environment
variables remain accepted aliases for the `CODEWHALE_*` forms, and
`DEEPSEEK_SECRET_BACKEND` is the legacy alias for `CODEWHALE_SECRET_BACKEND`.

Run `codewhale auth status` to inspect the active provider's config
file, OS keyring backend, environment variable, winning source, and last-four
label without printing the key itself. The command only probes the active
provider's keyring entry.

For hosted, generic OpenAI-compatible, self-hosted, OpenAI Responses, or native
Anthropic providers, set `provider = "<id>"` or pass
`codewhale --provider <id>`. The canonical provider IDs are `deepseek`,
`nvidia-nim`, `openai`, `atlascloud`, `wanjie-ark`, `volcengine`,
`openrouter`, `orcarouter`, `xiaomi-mimo`, `novita`, `fireworks`,
`siliconflow`, `arcee`, `siliconflow-CN`, `moonshot`, `sglang`, `vllm`,
`ollama`, `ollama-cloud`, `huggingface`, `modelscope`, `together`, `qianfan`, `openai-codex`,
`anthropic`, `openmodel`, `zai`, `stepfun`, `minimax`, `deepinfra`,
`sakana`, `longcat`, `opencode-go`, `opencode-zen`, `meta`, `xai`,
`mistral`, `telecomjs`, `modelstudio-token-plan`, `google`,
`edenai`, `concentrate`, `codewhale`, and `custom` (a user-defined OpenAI-compatible endpoint via
`[providers.<name>]`).
For the provider-by-provider registry, including wire protocol, auth variables,
default base URLs, model IDs, and capability metadata, see
[PROVIDERS.md](PROVIDERS.md).
The facade saves provider credentials in the local secret store and forwards
the resolved key, base URL, provider, and model to the TUI process. Use
`codewhale auth set --provider nvidia-nim --api-key "YOUR_NVIDIA_API_KEY"` or
`codewhale auth set --provider openai --api-key "YOUR_OPENAI_COMPATIBLE_API_KEY"` or
`codewhale auth set --provider atlascloud --api-key "YOUR_ATLASCLOUD_API_KEY"` or
`codewhale auth set --provider wanjie-ark --api-key "YOUR_WANJIE_API_KEY"` or
`codewhale auth set --provider xiaomi-mimo --api-key "YOUR_XIAOMI_KEY"` or
`codewhale auth set --provider fireworks --api-key "YOUR_FIREWORKS_API_KEY"` or
`codewhale auth set --provider siliconflow --api-key "YOUR_SILICONFLOW_API_KEY"` or
`codewhale auth set --provider arcee --api-key "YOUR_ARCEE_API_KEY"` or the
matching provider ID from [PROVIDERS.md](PROVIDERS.md) to save provider keys
through the facade. The generic `openai` provider defaults
to `https://api.openai.com/v1`, accepts `OPENAI_BASE_URL`, and defaults to
`gpt-5.6`. A custom OpenAI-compatible gateway can still select its own model
explicitly. `atlascloud` defaults to
`https://api.atlascloud.ai/v1`, accepts `ATLASCLOUD_BASE_URL`, and uses
`deepseek-ai/deepseek-v4-flash` as its default model. `wanjie-ark` targets
Wanjie Ark's OpenAI-compatible endpoint at
`https://maas-openapi.wanjiedata.com/api/v1`, defaults to `deepseek-reasoner`,
and passes model IDs through unchanged because Wanjie model access is
account-scoped. SGLang, vLLM, and Ollama are
self-hosted and can run without an API key by default. Ollama defaults to
`http://localhost:11434/v1` and sends model tags such as `codewhale-coder:1.3b`
or `qwen2.5-coder:7b` unchanged. Self-hosted providers and loopback custom
URLs (`localhost`, `127.0.0.1`, `[::1]`, `0.0.0.0`) do not read the secret store
unless API-key auth is explicitly requested; use an env var or config-file key
when a local server does require bearer auth.
Ollama Cloud is the separate hosted `ollama-cloud` provider. It defaults to
`https://ollama.com/v1` and `gpt-oss:120b`; save its key with
`codewhale auth set --provider ollama-cloud`. Ambient auth reads
`OLLAMA_CLOUD_API_KEY` first, then Ollama's official `OLLAMA_API_KEY`.
SiliconFlow defaults to `https://api.siliconflow.com/v1`, accepts
`SILICONFLOW_BASE_URL`, and uses `deepseek-ai/DeepSeek-V4-Pro` by default.
`provider = "siliconflow-CN"` selects the China regional default
`https://api.siliconflow.cn/v1` with the `[providers.siliconflow_cn]` table and
`SILICONFLOW_API_KEY` credential slot.
Arcee AI defaults to `https://api.arcee.ai/api/v1`, accepts `ARCEE_BASE_URL`,
and uses `trinity-large-thinking` by default for Codewhale agent work.
`trinity-large-preview` is also listed as a direct Arcee API model; OpenRouter's
`arcee-ai/trinity-large-thinking` remains the OpenRouter namespaced form, while
the direct Arcee provider uses the bare `trinity-large-thinking` ID. Direct
Arcee large-model API calls are tracked as 256K-context BF16 serving; Thinking
is reasoning-capable, while Preview is not marked as a thinking model.

### OpenRouter vendor pinning

OpenRouter serves each model through several upstream vendors, and Codewhale
can pin requests to a vendor with `[providers.openrouter] vendor` (#6007):

```toml
provider = "openrouter"
[providers.openrouter]
model = "deepseek/deepseek-v4-pro"
vendor = "deepinfra" # copy the vendor slug from the model's OpenRouter page
```

This sends `"provider": {"order": ["deepinfra"], "allow_fallbacks": false}`
on OpenRouter requests. A base slug can match multiple endpoint variants;
copy a full slug such as `deepinfra/turbo` to select one variant. An unavailable
pin fails at OpenRouter. Codewhale's separate `fallback_providers` setting can
still switch the whole route after a recoverable error.

The pin applies across OpenRouter models, including auxiliary requests on that
route. Set `vendor = ""` to clear it. Reload config or restart to apply edits;
requests already in flight keep their captured route. Other providers do not
inherit the pin. `/preview-request` shows the primary request's routing fields.

Model strings still pass through verbatim: `:floor` sorts by price, `:nitro`
sorts by throughput, and `@preset/my-team-preset` references an account preset.
See OpenRouter's [provider routing](https://openrouter.ai/docs/guides/routing/provider-selection)
and [presets](https://openrouter.ai/docs/guides/features/presets) documentation.
Codewhale does not fetch per-vendor endpoint prices or availability; pinned
usage reports a routing-dependent unknown cost instead of a catalog estimate.

### Custom OpenAI-Compatible Gateways

For a single third-party service that implements the OpenAI Chat Completions
API, the simplest setup is the built-in `openai` provider name pointed at the
gateway:

```toml
provider = "openai"
default_text_model = "your-model-id"

[providers.openai]
api_key = "YOUR_OPENAI_COMPATIBLE_API_KEY"
base_url = "https://your-gateway.example/v1"
```

Put the endpoint under `[providers.openai]`, not the legacy top-level
`base_url`, so the OpenAI-compatible provider receives it. `default_text_model`
is the model ID sent to the gateway; `[providers.openai].model` can be used as
the OpenAI-provider-specific override.

If you keep several OpenAI-compatible gateways, or need a stable name for an
AgentProfile provider pin, define a user-named custom provider table:

```toml
provider = "lm-studio"

[providers.lm-studio]
kind = "openai-compatible"
base_url = "http://127.0.0.1:1234/v1"
api_key = "lm-studio"
model = "qwen-2.5-7b"
```

Custom provider names may be selected with `provider = "<name>"`,
`--provider <name>`, or an AgentProfile `provider = "<name>"` when the matching
`[providers.<name>]` table exists.

StepFun has a first-class provider entry, so keep Coding Plan credentials and
base URL scoped to `[providers.stepfun]`:

```toml
provider = "stepfun"

[providers.stepfun]
api_key = "YOUR_STEPFUN_API_KEY"
base_url = "https://api.stepfun.ai/step_plan/v1"
model = "step-3.7-flash"
```

`/provider` setup asks which StepFun billing route the key belongs to —
pay-as-you-go (`https://api.stepfun.ai/v1`) or a Step Plan subscription
(`https://api.stepfun.ai/step_plan/v1`) — and validates the key against the
endpoint you pick before saving it. The answer is written to
`[providers.stepfun].base_url` and nowhere else. If that key already holds a
base URL Codewhale does not recognize as one of those two routes, the question
is skipped and your value is left untouched.

Alibaba Bailian / Model Studio DashScope Qwen routes use the same OpenAI
provider shape:

```toml
provider = "openai"

[providers.openai]
api_key = "YOUR_DASHSCOPE_API_KEY"
base_url = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
model = "qwen-plus"
context_window = 1000000
```

Use the regional DashScope `compatible-mode/v1` base URL that matches the
region of your API key. Codewhale keeps `qwen-plus` scoped to the `openai`
provider route and does not infer a different provider from the model prefix.
The same rule applies to all provider-prefixed model strings: a prefix such as
`deepseek-ai/...` or `deepseek/...` is a provider-owned wire ID under the
selected provider, not an automatic switch to the DeepSeek provider.
Set `context_window` to the gateway/model's real total context window when it
differs from Codewhale's static model metadata. See
[Context length (context window)](#context-length-context-window) for the full
resolution order and for how to check which value is in effect.

If the gateway accepts `POST /chat/completions` but rejects
`/v1/chat/completions`, set a provider-local `path_suffix`:

```toml
[providers.openai]
base_url = "https://your-gateway.example/v1"
path_suffix = "/chat/completions"
```

The suffix applies only to chat-completion requests. Model listing and
DeepSeek beta paths keep their built-in routing so a generic gateway override
does not accidentally rewrite `/models` or `/beta/completions`.

For private gateways with broken or intercepted certificates, use
`SSL_CERT_FILE` with a trusted CA bundle. The legacy provider-table key
`insecure_skip_tls_verify = true` is still parsed so `codewhale doctor` can
report stale configs, but provider clients reject it instead of disabling TLS
certificate verification.

Local HTTP endpoints such as Ollama, SGLang, and vLLM are allowed by default
when they use localhost or loopback addresses. For a non-local `http://`
gateway, launch with `DEEPSEEK_ALLOW_INSECURE_HTTP=1` only on a trusted network:

```bash
DEEPSEEK_ALLOW_INSECURE_HTTP=1 codewhale
```

Third-party OpenAI-compatible gateways that need extra request headers can set
`http_headers = { "X-Model-Provider-Id" = "your-model-provider" }` at the top
level or under a provider table such as `[providers.deepseek]`. When configured,
codewhale sends those custom headers on model API requests. The equivalent
environment override is `DEEPSEEK_HTTP_HEADERS`, using comma-separated
`name=value` pairs such as
`X-Model-Provider-Id=your-model-provider,X-Gateway-Route=dev`. `Authorization`
and `Content-Type` are managed by the client and are not overridden by this
setting.

### Vision Model

Codewhale's chat provider and `image_analyze` tool are configured separately.
The main chat path remains the selected text/tool provider; image analysis runs
through `[vision_model]` when the `vision_model` feature is enabled.

Xiaomi's current image-understanding docs include `mimo-v2.5` for image input.
To use MiMo for `image_analyze`, configure the vision model explicitly:

```toml
[features]
vision_model = true

[vision_model]
model = "mimo-v2.5"
api_key = "YOUR_XIAOMI_KEY"
base_url = "https://api.xiaomimimo.com/v1"
```

The example above uses Xiaomi MiMo's pay-as-you-go OpenAI-compatible endpoint.
If you are using a Token Plan key (`tp-...`) for `[vision_model]`, you must set
`base_url` explicitly because this generic OpenAI-compatible block does not
auto-select MiMo endpoints. Use
`https://token-plan-sgp.xiaomimimo.com/v1` for Singapore accounts,
`https://token-plan-cn.xiaomimimo.com/v1` for China-region accounts, or
`https://token-plan-ams.xiaomimimo.com/v1` for Europe/Amsterdam accounts.

### Auto Model Routing (`[auto]`, `[auto.router]`)

With `model = "auto"`, each turn runs on your **declared default model** unless
you have opted into something else. Auto never guesses a cheaper or stronger
model from how a request is worded; the old keyword-and-length heuristic was
removed (`auto_route_declared_fallback` in `crates/tui/src/model_routing.rs`).
Two optional layers change that:

- an `[auto.router]` classifier, which you write down, that picks a model per
  turn; and
- `[auto] cost_saving`, which prefers the active provider's fast sibling.

With neither set, Auto is local and free: the turn uses the default model and
no classifier call is made.

**There is no default classifier.** With `[auto.router]` unset, no classifier
call happens, whatever keys you hold. Holding a DeepSeek key used to elect
`deepseek-v4-flash` automatically. That was removed because it spent tokens on
a route the user never chose and privileged one provider over the rest
(`AutoRouterConfig` in `crates/tui/src/config.rs`). Electing a network
classifier is now something you write down.

Point the classifier at any configured provider with `[auto.router]`:

```toml
[auto.router]
provider = "zai"
model = "glm-5-turbo"
thinking = "off"        # optional; defaults to off
timeout_secs = 4        # optional; default 4, 0 = default, capped at 300
```

A classifier call happens only when `[auto.router]` names both `provider` and
`model` *and* that provider has a key:
`router_available = router_configured && has_api_key_for(...)` in
`ModelInventory::from_config` (`crates/tui/src/model_inventory.rs`). If either
condition fails, or the classifier call errors or times out, the local
fallback decides: the default model, or the fast sibling under `cost_saving`.
The turn's route receipt (Turn Inspector, Ctrl+Alt+O or `/turn inspect`, "Model
route + tokens/cost") records which path was taken, and a
router you configured that cannot run or fails (missing key, HTTP error,
timeout, invalid answer) is shown as `Auto router: failing — …` rather than
silently ignored.

#### Set up model routing

`/router` (also `/model router`) opens one Router setup view with these
presets. Each writes only `[auto.router]`; none is ever chosen for you.

| Preset | What it writes | Cost and privacy |
| --- | --- | --- |
| `/router jev` | Jev, TypeSafe's decision model, over OpenRouter (`typesafe/jev-1.13`) or TypeSafe direct, whichever key you have | About $0.00002 per turn ($0.042 per million input tokens, output free). Your latest request and up to six recent context lines go to OpenRouter → TypeSafe (or TypeSafe). |
| `/router fast` | The active provider's runnable fast tier with thinking off | Your existing key; the classifier sees the same request text. |
| `/router off` | Removes `[auto.router]` | No router call; Auto turns use the default model, or the fast tier while `[auto] cost_saving = true` (Off leaves that setting alone). |
| `/router custom` | Nothing; prints the TOML to edit | — |

Choosing Jev or Fast makes **one test call** with a fixed sample request and
shows the tier it picked, the probabilities and confidence, the latency and the
provider-reported cost. `Enter` (or `/router save <preset>`) then saves through
the normal config writer; `Esc` discards it. TypeSafe paused new signups on
2026-09-22, so OpenRouter is the default route for new users. A TypeSafe key is
read from `TYPESAFE_API_KEY`, the `typesafe` secret-store entry, or
`[providers.typesafe] api_key` / `api_key_env`.

#### Decision routers (`kind = "decision"`)

A decision router asks a non-generative decision model typed Choice questions
for the active provider's `fast` and `strong` tiers and thinking level, then
reads calibrated probabilities. No prose is parsed.

```toml
[auto.router]
kind = "decision"             # default "chat"
provider = "openrouter"       # or "typesafe"
model = "typesafe/jev-1.13"   # "~typesafe/jev-latest" also works; TypeSafe direct: "jev-latest"
timeout_secs = 2
min_confidence = 0.5          # default 0.5, clamped to 0..1
```

- The router is called only when the active provider has a runnable strong/fast
  pair; otherwise there is no call and no spend.
- An answer below `min_confidence` takes the local fallback. Under
  `[auto] cost_saving`, a `strong` answer also needs a probability of at least
  0.75, or the turn stays on the fast tier.
- An unknown `kind`, or a decision `provider` other than `openrouter` /
  `typesafe`, leaves the router unconfigured and shown as failing.
- `[auto.router] thinking` is ignored for decision routers. Both routes settle tokens through
  the originating session's routed-usage ledger. TypeSafe is a named Custom route
  with unknown billing; its price is never borrowed from the active chat provider.
  Provider-reported cost remains verbatim on the decision receipt.
- OpenRouter uses `POST /api/alpha/decisions`; TypeSafe uses `POST /v1/systemone`.
  The shared client validates Choice, Noul and Score against the offered questions
  and bounds responses to 256 KiB. Malformed answers fail open with usage retained.

#### Shadow Decision Gate (experimental, off by default)

The Superfast Decision Gate asks a decision model three typed questions about
the latest user message — does it need a tool, can it be answered from the
conversation, and what is its intent — and logs a conservative recommendation.
It is shadow-only: it never changes routing, never skips or delays the model
call, and fails open on any error, timeout or malformed answer. It uses the same
System One client as the decision router above; there is no separate HTTP
client. It is configured from the environment and reads it when a turn starts:

```sh
SUPERFAST_ENABLED=1              # off unless set
SUPERFAST_PROVIDER=typesafe      # or openrouter; required when enabled
SUPERFAST_BASE_URL=http://localhost:8000/v1  # optional TypeSafe-route base, e.g. self-hosted
SUPERFAST_MODEL=jev-latest       # default jev-latest / ~typesafe/jev-latest
SUPERFAST_TIMEOUT_MS=150         # 1..=10000, default 150
```

- Enabling the gate never picks an endpoint by itself: without
  `SUPERFAST_PROVIDER` nothing is sent and a warning is logged.
- The key comes from the same place the decision router reads it. The TypeSafe
  route always authenticates, so a self-hosted server that ignores auth still
  needs a placeholder `TYPESAFE_API_KEY`.
- Only the latest user message is sent, truncated to 4,000 characters and
  redacted of configured secrets. The log (target `superfast`) carries the
  route, failure class and latency, never prompt text.
- The gate retains the originating turn's accounting owner and cancellation.
  Its tokens settle through the shared ledger; missing usage or a cancelled/timed
  out request after dispatch creates an explicit coverage gap. Unknown TypeSafe
  pricing is recorded as unpriced rather than free.
- Provider-reported cost survives rejected answers and late responses in bounded
  receipts on the originating turn or session. These are diagnostic evidence;
  unknown pricing never becomes an authoritative dollar total. Incomplete token
  counters record a coverage gap while preserving the reported raw evidence.

The wire contracts are documented in [TypeSafe's OpenAPI schema](https://api.typesafe.ai/openapi.json)
and [OpenRouter's Decisions examples](https://openrouter.ai/blog/insights/what-is-jev/).

The Decision Gate concept and reference implementation are by Andrea Bruno,
released under CC BY 4.0:
[harness-superfast](https://github.com/Andrea-Bruno/harness-superfast).

Two `[auto]` keys shape routing (`AutoConfig` in `crates/tui/src/config.rs`):

```toml
[auto]
cost_saving = false     # default false
cross_provider = false  # default false
```

- **`cost_saving`** (default `false`). Without a classifier, Auto pins the
  active provider's validated fast sibling instead of the default model. A
  provider with no runnable fast sibling stays on the default. With a
  classifier, the classifier is told to prefer the fast tier for routine or
  ambiguous work and to pick the strong tier only for clearly agentic,
  multi-step, architecture, security or debugging work. Cost-saving never
  switches provider just to save money.
- **`cross_provider`** (default `false`). Auto stays on the provider the session
  is configured to use. The classifier is only shown that provider's models,
  and the fallback never leaves it. Setting `cross_provider = true` lets the
  classifier choose among every runnable provider. There is no interactive
  toggle for `cross_provider`; it has to be set in config. A decision router
  always chooses within the active provider.

To bootstrap MCP and skills directories at their resolved paths, run `codewhale setup`.
To only scaffold MCP, run `codewhale mcp init`.

Note: `setup`, `doctor`, `mcp`, `features`, `sessions`, `resume`/`fork`, `exec`,
`review`, and `eval` are all available from the installed `codewhale` command.
The consolidated dispatcher also provides `auth`, `config`, `model`, `thread`, `sandbox`,
`app-server`, `mcp-server`, `completions`, `login`/`logout`, `account`,
`metrics`, `update`, `lane`, `workflow`, and `web`. Plain prompts enter the
in-process TUI runtime. Release installers expose the same bytes as `codew`.

### Startup Update Checks

By default, the TUI starts a background check for the latest stable Codewhale
release and shows a short toast only when a newer release is available and the
official release assets are complete. The check never blocks startup, never
blocks a turn, and fails silently when offline.

Disable the startup check entirely for air-gapped, corporate-proxy, or managed
desktop environments:

```toml
[update]
check_for_updates = false
```

#### Throttling

The answer is cached in `~/.codewhale/update-check.json` and reused for
`check_interval_hours` (default `1`). Only the *network request* is throttled —
the notice still appears on every launch while an update is outstanding. Set `0`
to check on every launch.

```toml
[update]
check_interval_hours = 1
```

A failed check is not cached, so an outage does not suppress the notice until
the interval elapses.

#### Automatic suppression

Checks are skipped, without contacting the network, when any of these is set to
a non-falsey value:

| Variable | Why |
| --- | --- |
| `CODEWHALE_NO_UPDATE_CHECK` | Explicit opt-out. |
| `NO_UPDATE_NOTIFIER` | The cross-CLI convention, honored for compatibility. |
| `CI`, `CONTINUOUS_INTEGRATION`, `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `CIRCLECI`, `JENKINS_URL`, `TEAMCITY_VERSION`, `TF_BUILD` | Automated build; nobody is at the terminal. |

Values of `""`, `0`, `false`, `no`, and `off` do not count as set, so a
`CI=false` export does not disable checks for ordinary users.

#### Which update command is offered

Codewhale never installs anything on its own — it only tells you an update
exists. The command it names depends on how the running binary was installed,
detected from its path:

| Install | Command offered |
| --- | --- |
| GitHub release binary (including Termux) | `codewhale update` |
| npm (`node_modules` on the path) | `npm install -g codewhale@latest` |
| Homebrew (`Cellar` / `linuxbrew` prefix) | `brew upgrade codewhale` |
| `cargo install` (`~/.cargo/bin`) | `cargo install codewhale-cli --locked --force` |

For package-managed installs the notice also warns against `codewhale update`:
replacing a binary Homebrew or npm owns leaves the manager describing a version
that is no longer on disk, and the next upgrade silently reverts you.

Override the detection with `CODEWHALE_INSTALL_METHOD=npm|homebrew|cargo|binary`
if you relocated the binary somewhere the path heuristics cannot read.

To redirect the startup check, set `update_uri` to an internal endpoint that
returns GitHub-compatible latest-release JSON. Minimal mirror metadata with a
`tag_name` field is accepted; if `assets` are present, Codewhale requires the
same uploaded asset set as the official release before showing the toast.

```toml
[update]
check_for_updates = true
update_uri = "https://internal.mirror.example/codewhale/releases/latest"
```

When `update_uri` is not set, startup checks honor release mirror environment
variables such as `CODEWHALE_RELEASE_BASE_URL` before falling back to the
official GitHub API endpoint. If a configured `update_uri` cannot be fetched or
parsed and a release mirror env var is set, the TUI falls back to that mirror
instead of failing startup.

## Workshop output budgets

By default an oversized tool result uses bounded spillover: a result
under the byte threshold stays inline, a larger one gets a head/tail
preview plus a session artifact the model can read back. There is no
synthesis sub-agent and no per-call `raw = true` escape.

`[workshop] large_output_threshold_tokens` and
`[workshop.per_tool_thresholds]` (exact model-visible tool names such as
`bash`) only take effect when the process opts in to adaptive evidence
routing with `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING=1`; they set where a result
stops being inline and becomes handle-only evidence.

Two optional byte ceilings (#5367) apply either way. They raise the
model-visible floor and never lower it:

- `read_result_max_bytes` — cap for a single `read` / `read_file`
  result. Absent keeps the compile-time defaults (100000 bytes for
  `read`, which has no line cap; 16KiB / 500 lines for `read_file`).
  For `read` this is the middle of a three-layer budget: the model's
  own per-call `max_bytes` (hard maximum 500000) raises the budget for
  one call, this setting raises the floor for the whole process, and
  either way 2MiB is the absolute ceiling. Highest wins; neither layer
  can lower a budget the other granted.
- `tool_result_max_bytes` — cap for a generic tool result after
  spillover. Absent keeps the 12K-character compact floor (48K on
  windows ≥500K tokens). Hard cap is 2MiB.

## Context length (context window)

Also called context size, context limit, max context, or window. This is the
total token window Codewhale budgets against, and it drives the header/footer
context percent, the auto-compaction trigger, context-pressure checks, and the
request output cap. If Codewhale compacts at 128K on a model you know serves a
1M window, this is the setting to change (#5134).

**See what is in effect, and where the value came from.** Every one of these
prints the resolved window *and* its source:

- `/status` — a `Context window:` row with the percent and token counts, and a
  `Window source:` row naming the provenance and the exact key that overrides
  it.
- `/config` → Provider — `Context window` (your override, or `(not set)`) and
  `Effective context window` (`1048576 tokens · configured`). Typing
  `context length` in the `/config` filter jumps straight to them.
- `/context report` — `Window: 1048576 tokens (12.4% used, ...; source: configured)`.
- `/context json` — machine-readable `context_window_tokens` and
  `context_window_source`.

**Change it** with the provider-table key `context_window`:

```toml
[providers.moonshot]
context_window = 1048576
```

or from the CLI:

```bash
codewhale config set providers.moonshot.context_window 1048576
codewhale config unset providers.moonshot.context_window   # back to automatic
```

Use the table for the provider you are actually on (`providers.openai`,
`providers.deepseek`, `providers.moonshot`, …); `/status` names it for you. The
value is a positive token count for the route's *total* window.

When one gateway fronts models with heterogeneous windows, scope the override
to an exact wire model id with `[providers.<name>.model_context_windows]`:

```toml
[providers.command_code]
context_window = 204800

[providers.command_code.model_context_windows]
"MiniMaxAI/MiniMax-M2.5" = 204800
"google/gemini-3.1-flash-lite" = 1000000
```

Keys are the exact wire model ids the route sends (dotted and `org/model`
spellings both work as TOML keys when quoted); each value must be a positive
token count. A matching entry beats the provider-level `context_window` for
that model only — every other model on the provider still resolves against
`context_window` and the rungs below. From the CLI:

```bash
codewhale config set 'providers.command_code.model_context_windows."MiniMaxAI/MiniMax-M2.5"' 204800
codewhale config unset 'providers.command_code.model_context_windows."MiniMaxAI/MiniMax-M2.5"'
```

### How the effective window is resolved

First match wins, and the source label each surface prints is exactly this
rung:

1. `configured (per-model)` — a `[providers.<name>.model_context_windows]`
   entry keyed by the route's exact wire model id. A hard override for that
   model only; it never rewrites another model's window.
2. `configured` — `[providers.<name>] context_window` in `config.toml`. A hard
   override for every model on the provider: nothing below it can raise or
   lower the result. Read-time aliases:
   `contextWindow`, `context_window_tokens`, `contextWindowTokens`,
   `context_length`, `contextLength`.
3. `provider-reported` — route-scoped 1M metadata a provider actually reported
   for the Kimi Code `k3` route, when it was observed within the last 24 hours.
4. `static Kimi Code safe floor` — 262,144 tokens for Kimi Code memberships,
   because 1M access is plan-gated (Allegretto and above).
5. `catalog` — the bundled route catalog (hand-curated offerings first, then
   the bundled Models.dev rows). The official `openai-codex` roster supplies
   account-specific model choices, not context-window metadata. Refresh it
   with `codewhale models --update --provider openai-codex`; Codewhale does
   not read `$CODEX_HOME` for this route.
6. `model-name hint` — an `_Nk` suffix parsed from the model name itself
   (`qwen3-32b-256k` → 256,000), vendor-agnostic. A naming convention the
   serving engine may not honor is not a fact about the route, so this rung
   sits *below* the catalog: any catalog row for the same id beats it (#5441).
7. `fallback` — the static per-provider capability table: 200,000 for
   Anthropic-wire routes, 128,000 for `openai-codex`, 8,192 for Ollama,
   otherwise Codewhale's static per-model metadata, and finally 128,000 when
   the model is unknown.

### What "(unverified)" means

The `model-name hint` and `fallback` rungs still drive real budgets — the
compaction trigger, the context meter, and the output reservation all use the
number — but they are guesses, not capabilities anyone checked. Every surface
that renders one of these windows appends `(unverified)` to its source label
(the status line, the context-pressure message, `/status`, `/config`, and the
model picker chip), so a window you did not configure and no provider reported
can never read as a verified limit (#5239, #5441). The `context_window` and
`model_context_windows` provider-table keys above are the fix: a configured
window is a hard override and renders as `configured` (or
`configured (per-model)`) with no marker.

Output ceilings follow the same rule (#5440): an Anthropic-family model the
catalog does not describe keeps the 64K Messages floor as its clamp, labeled
`unverified` (or an "assumed floor") instead of `documented`. The official
ChatGPT plan preview does not accept `max_output_tokens`; Codewhale sends no
output-token cap on that route. A local request budget is not an upstream
limit or a guarantee about the completed response length.

There is no environment variable for the context window; the provider-table
`context_window` and per-model `model_context_windows` keys are the user
knobs. They are the right ones to set when a gateway or self-hosted runtime
serves a window Codewhale's catalog does not model — per-model when only some
of a provider's routes differ, provider-wide when they all do. Codewhale will
not invent a window it cannot justify — it falls back to a conservative value,
labels it `fallback`, and marks it `(unverified)` at every surface that shows
it.

### Adjacent knobs

- `auto_compact_threshold_percent` (settings.toml; also accepted as
  `auto_compact_threshold`; `10`–`100`, default `80`): the share of the full
  route context window at which auto-compaction fires, clamped so it can never
  cross the spendable input ceiling after output reservation and headroom.
  Editable from `/config`. Raising the window without touching this raises the
  absolute compaction point along with it.
- `auto_compact` (settings.toml, on/off): turns automatic compaction off
  entirely; `/compact` and Ctrl+L stay available.
- `[compaction] summary_instructions` and
  `[compaction] retained_user_message_tokens` (config.toml): standing
  summarizer instructions and the verbatim user-message retention budget. See
  the `compaction.*` entry in the config-key reference below.
- `CODEWHALE_MAX_OUTPUT_TOKENS` (environment variable; legacy alias
  `DEEPSEEK_MAX_OUTPUT_TOKENS`): overrides the requested output cap. Without an
  override, Codewhale starts at the safe `65536` request cap and intersects it
  with any smaller documented model or route ceiling; a catalog `max_output`
  such as DeepSeek V4's 384K remains a capability ceiling, not the amount every
  response requests. Explicit overrides are preserved within the resolved
  route context window and any route output ceiling, and preflight/emergency
  budgeting reserves the same effective value that can reach the wire. A
  separately documented route input ceiling also clamps preflight and
  compaction even when the total context window is larger. A blank canonical
  variable falls through to a nonblank legacy value; a nonblank invalid or
  zero canonical value is authoritative and falls back to the safe automatic
  default instead of activating a stale legacy setting. There is no
  `max_output_tokens` key in `config.toml`.

Before compaction replaces conversation history, Codewhale durably saves the
original messages to the session's `artifacts/context-transfer-<id>.json` and,
when a model summary is produced, its handoff to the matching `.md` file.
These use the existing session artifact store and persistence redaction.
A failed write aborts compaction without replacing context. Pruning-only passes
save the original messages without making an extra model call. Pressure metadata
shows estimated input tokens and the configured trigger; it is an estimate, not
an exact promise about a provider's remaining context.

Compaction history is available through `codewhale metrics` (or `--json`) and
`audit.log` in the Codewhale home. Completed passes record their trigger,
summary/pruning path, message and estimated-token counts, effective threshold,
and summarizer token usage. Automatic refusals are recorded once per turn with
their reason. These are local diagnostics, not provider invoice totals; earlier
artifacts are not retroactively counted. A text-mode `exec` that attempts
compaction saves its owning session at turn completion so its recovery artifacts
remain discoverable. A killed process may leave artifacts without that final
session snapshot; the audit writer reports I/O failures instead of inventing data.

See [Settings File](#settings-file-persistent-ui-preferences) for the
compaction settings and [Token Quantities and
Drivers](#token-quantities-and-drivers) for what each displayed token number
actually measures.

## Profiles

You can define multiple profiles in the same file:

```toml
default_text_model = "deepseek-flash"

[providers.deepseek]
api_key = "PERSONAL_KEY"

[profiles.work.providers.deepseek]
api_key = "WORK_KEY"
base_url = "https://api.deepseek.com/beta"

[profiles.nvidia-nim]
provider = "nvidia-nim"
default_text_model = "deepseek-ai/deepseek-v4-pro"

[profiles.nvidia-nim.providers.nvidia_nim]
api_key = "NVIDIA_KEY"
base_url = "https://integrate.api.nvidia.com/v1"

[profiles.fireworks]
provider = "fireworks"
default_text_model = "accounts/fireworks/models/deepseek-v4-pro"

[profiles.siliconflow]
provider = "siliconflow"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.siliconflow.providers.siliconflow]
base_url = "https://api.siliconflow.com/v1"

[profiles.openai-compatible]
provider = "openai"

[profiles.openai-compatible.providers.openai]
base_url = "https://openai-compatible.example/v4"
model = "glm-5"

[profiles.atlascloud]
provider = "atlascloud"

[profiles.atlascloud.providers.atlascloud]
base_url = "https://api.atlascloud.ai/v1"
model = "deepseek-ai/deepseek-v4-flash"

[profiles.sglang]
provider = "sglang"

[profiles.sglang.providers.sglang]
base_url = "http://localhost:30000/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.vllm]
provider = "vllm"

[profiles.vllm.providers.vllm]
base_url = "http://localhost:8000/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.ollama]
provider = "ollama"

[profiles.ollama.providers.ollama]
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:7b" # any tag `ollama list` shows

[profiles.ollama-cloud]
provider = "ollama-cloud"

[profiles.ollama-cloud.providers.ollama_cloud]
base_url = "https://ollama.com/v1"
model = "gpt-oss:120b"
```

Select a profile with:

- CLI: `codewhale --profile work`
- Env: `DEEPSEEK_PROFILE=work`

If a profile is selected but missing, codewhale exits with an error listing available profiles.

## Legacy top-level `base_url` and `api_key`

Older releases kept DeepSeek's endpoint and key at the top of `config.toml`,
and each reader decided for itself which other routes inherited them. They
now live in provider tables only. An older file keeps working unchanged:
every load reads the top-level keys as if they were already in their table,
by one rule, per file and per `[profiles.<name>]`:

1. `provider = "custom"` with no `[providers.custom]` table: the endpoint, key
   and a copy of the model become `[providers.custom]`.
2. An endpoint on another vendor's official host (for example
   `integrate.api.nvidia.com`, `xiaomimimo.com`, `openrouter.ai`, or the
   ChatGPT Codex endpoint) belongs to that vendor's table, so a DeepSeek route
   never sends requests there.
3. Anything else belongs to `[providers.deepseek]`, which DeepSeek-CN also
   reads for its endpoint and key.

The key goes with DeepSeek (or the literal custom route). It follows the
endpoint to another vendor only when the same table sets `provider` to that
vendor, the host is that vendor's own, and the vendor's table has no key. When
no `provider` is set, a NIM host still selects `nvidia-nim` and
`api.deepseeki.com` still selects `deepseek-cn`. A `[vision_model]` without a
key of its own keeps using the top-level key. A profile's top-level
`base_url` now overrides the base file's `[providers.deepseek] base_url`
(before, the base table silently won).

When a top-level value and its table disagree, the table's `base_url` and the
top-level `api_key` are used, which is what the runtime sent before.

Loading never rewrites the file. The next save Codewhale makes (for example
`codewhale config set`, `/config ... --save`, or `auth set`) moves the keys,
keeps comments, writes a one-time credential-free copy of the old file to
`config.toml.pre-migrate.bak`, and prints one line saying what moved. A
disagreeing pair is never resolved in the file on its own; both values stay
until you choose, and `codewhale config doctor` / `codewhale doctor` report
it (sources only, never values). `codewhale config get|set|unset base_url`
(and `api_key`) address the active provider's table.

To tidy the file yourself:

```bash
codewhale config migrate --dry-run            # show what would move
codewhale config migrate                      # move it (backup first)
codewhale config migrate --prefer top-level   # resolve a conflict: keep the top-level value
codewhale config migrate --prefer table       # resolve a conflict: keep the table value
```

## Environment Variables

Most runtime environment variables override config values. API-key variables are
fallbacks after saved config and keyring credentials.

The three user-facing slots — provider, model, base URL — expose `CODEWHALE_*`
aliases. When both forms are set the `CODEWHALE_*` value wins; the
`DEEPSEEK_*` form is kept for older shells:

- `CODEWHALE_PROVIDER` (preferred) / `DEEPSEEK_PROVIDER` (legacy alias) —
  `deepseek|deepseek-anthropic|nvidia-nim|openai|atlascloud|wanjie-ark|volcengine|openrouter|xiaomi-mimo|novita|fireworks|siliconflow|arcee|siliconflow-CN|moonshot|sglang|vllm|ollama|ollama-cloud|huggingface|modelscope|together|qianfan|openai-codex|anthropic|openmodel|zai|stepfun|minimax|deepinfra|mistral`
- `CODEWHALE_MODEL` (preferred) / `DEEPSEEK_MODEL` (legacy alias) — default model for the active provider
- `CODEWHALE_BASE_URL` (preferred) / `DEEPSEEK_BASE_URL` (legacy alias) — base URL for the active provider

`CODEWHALE_BASE_URL` applies to the **active** route only. A request pinned to
another provider — a subagent or fleet child, a routed tool, the per-turn
auto-router, a picker preview — resolves its endpoint from that provider's own
`[providers.<table>]`, then its provider-scoped variable (`MOONSHOT_BASE_URL`,
`OPENAI_BASE_URL`, …), then that provider's default. It never inherits the
active session's host, and a custom route with no configured `base_url` fails
closed on a loopback placeholder rather than borrowing another provider's
endpoint. The environment writes the value into the active identity's own
table: DeepSeek-CN falls back to `[providers.deepseek]` for its endpoint and
key, but never to a DeepSeek value the environment addressed to DeepSeek alone.
A managed-config overlay that supplies or reselects the effective
route's endpoint takes the generic override away from every route.

Remaining variables:

- `DEEPSEEK_API_KEY`
- `DEEPSEEK_ANTHROPIC_BASE_URL`
- `DEEPSEEK_HTTP_HEADERS` (custom model request headers, comma-separated `name=value` pairs)
- `DEEPSEEK_DEFAULT_TEXT_MODEL` (extra legacy alias of `DEEPSEEK_MODEL`)
- `DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS` (stream idle timeout in seconds; default `900`, clamped to `1..=3600`)
- `DEEPSEEK_STREAM_OPEN_TIMEOUT_SECS` (connection setup + response-header wait in seconds; default `45`, clamped to `5..=300`; distinct from the per-chunk idle timeout; `stream.open_timeout_secs` (legacy `tui.stream_open_timeout_secs` fallback) wins when positive)
- `CODEWHALE_CACHE_MAXIMAL` (`1`/`true`/`on`/`yes`) — cache-maximal context mode (#528). When on, the Repo Working Set block materializes the **full current contents** of the top active files into the system prompt each turn (deterministic order, byte-bounded), instead of only listing their paths. The block stays byte-stable while those files are unchanged so DeepSeek's KV prefix cache keeps hitting; editing a file cache-misses from its block onward. Off by default (path list only). Byte caps default to 24 KB per file / 96 KB total.
- `NVIDIA_API_KEY` or `NVIDIA_NIM_API_KEY` (when provider is `nvidia-nim`)
- `NVIDIA_NIM_BASE_URL`, `NIM_BASE_URL`, or `NVIDIA_BASE_URL`
- `NVIDIA_NIM_MODEL`
- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`
- `ATLASCLOUD_API_KEY`
- `ATLASCLOUD_BASE_URL`
- `ATLASCLOUD_MODEL`
- `WANJIE_ARK_API_KEY`, `WANJIE_API_KEY`, or `WANJIE_MAAS_API_KEY`
- `WANJIE_ARK_BASE_URL`, `WANJIE_BASE_URL`, or `WANJIE_MAAS_BASE_URL`
- `WANJIE_ARK_MODEL`, `WANJIE_MODEL`, or `WANJIE_MAAS_MODEL`
- `VOLCENGINE_API_KEY`, `VOLCENGINE_ARK_API_KEY`, or `ARK_API_KEY`
- `VOLCENGINE_BASE_URL`, `VOLCENGINE_ARK_BASE_URL`, or `ARK_BASE_URL`
- `VOLCENGINE_MODEL` or `VOLCENGINE_ARK_MODEL`
- `OPENROUTER_API_KEY`
- `OPENROUTER_BASE_URL`
- `OPENROUTER_MODEL`
- `XIAOMI_MIMO_TOKEN_PLAN_API_KEY`, `MIMO_TOKEN_PLAN_API_KEY`, `XIAOMI_MIMO_API_KEY`, `XIAOMI_API_KEY`, or `MIMO_API_KEY`
- `XIAOMI_MIMO_BASE_URL` or `MIMO_BASE_URL`
- `XIAOMI_MIMO_MODEL` or `MIMO_MODEL`
- `XIAOMI_MIMO_MODE` or `MIMO_MODE` (`token-plan-sgp`, `token-plan-cn`,
  `token-plan-ams`, or `pay-as-you-go`)
- `NOVITA_API_KEY`
- `NOVITA_BASE_URL`
- `NOVITA_MODEL`
- `FIREWORKS_API_KEY`
- `FIREWORKS_BASE_URL`
- `FIREWORKS_MODEL`
- `HUGGINGFACE_API_KEY` or `HF_TOKEN` (`HF_TOKEN` is a fallback alias accepted when provider is `huggingface`)
- `MODELSCOPE_API_KEY`
- `HUGGINGFACE_BASE_URL` or `HF_BASE_URL`
- `HUGGINGFACE_MODEL` or `HF_MODEL`
- `SILICONFLOW_API_KEY`
- `SILICONFLOW_BASE_URL`
- `SILICONFLOW_MODEL`
- `ARCEE_API_KEY`
- `ARCEE_BASE_URL`
- `ARCEE_MODEL`
- `TOGETHER_API_KEY`
- `TOGETHER_BASE_URL`
- `TOGETHER_MODEL`
- `QIANFAN_API_KEY` or `BAIDU_QIANFAN_API_KEY`
- `QIANFAN_BASE_URL` or `BAIDU_QIANFAN_BASE_URL`
- `QIANFAN_MODEL` or `BAIDU_QIANFAN_MODEL`
- `OPENAI_CODEX_ACCESS_TOKEN` or `CODEX_ACCESS_TOKEN` (legacy process tokens; ignored by the official ChatGPT plan route)
- `OPENAI_CODEX_BASE_URL` or `CODEX_BASE_URL`
- `OPENAI_CODEX_MODEL` or `CODEX_MODEL`
- `OPENAI_CODEX_ACCOUNT_ID` or `CODEX_ACCOUNT_ID` (legacy identity hints; ignored by the official ChatGPT plan route)
- `CODEWHALE_CHATGPT_NEW_ACCOUNT=1` explicitly replaces the single selected ChatGPT registration after a validated browser sign-in
- `ANTHROPIC_API_KEY`
- `ANTHROPIC_BASE_URL`
- `ANTHROPIC_MODEL`
- `ZAI_API_KEY` or `Z_AI_API_KEY`
- `ZAI_BASE_URL` or `Z_AI_BASE_URL`
- `ZAI_MODEL` or `Z_AI_MODEL`
- `STEPFUN_API_KEY` or `STEP_API_KEY`
- `STEPFUN_BASE_URL` or `STEP_BASE_URL`
- `STEPFUN_MODEL` or `STEP_MODEL`
- `MINIMAX_API_KEY`
- `MINIMAX_BASE_URL`
- `MINIMAX_MODEL`
- `DEEPINFRA_API_KEY` or `DEEPINFRA_TOKEN`
- `DEEPINFRA_BASE_URL`
- `DEEPINFRA_MODEL`
- `MISTRAL_API_KEY`
- `MISTRAL_BASE_URL`
- `MISTRAL_MODEL`
- `MOONSHOT_API_KEY` or `KIMI_API_KEY`
- `MOONSHOT_BASE_URL` or `KIMI_BASE_URL`
- `MOONSHOT_MODEL`, `KIMI_MODEL_NAME`, or `KIMI_MODEL`
- `SGLANG_BASE_URL`
- `SGLANG_MODEL`
- `SGLANG_API_KEY` (optional; many localhost SGLang servers do not require auth)
- `VLLM_BASE_URL`
- `VLLM_MODEL`
- `VLLM_API_KEY` (optional; many localhost vLLM servers do not require auth)
- `OLLAMA_BASE_URL`
- `OLLAMA_MODEL`
- `OLLAMA_API_KEY` (optional; many localhost Ollama servers do not require auth)
- `OLLAMA_CLOUD_BASE_URL`
- `OLLAMA_CLOUD_MODEL`
- `OLLAMA_CLOUD_API_KEY` (preferred Cloud key; `OLLAMA_API_KEY` is the official fallback)
For every product-level `CODEWHALE_*` variable below, the matching legacy
`DEEPSEEK_*` name is still read as a compatibility fallback; when both are set,
the `CODEWHALE_*` value wins.

- `CODEWHALE_LOG_LEVEL` or `RUST_LOG` (`info`/`debug`/`trace` enables lightweight verbose logs)
- `CODEWHALE_SKILLS_DIR`
- `CODEWHALE_MCP_CONFIG`
- `CODEWHALE_NOTES_PATH`
- `CODEWHALE_MEMORY` (`1|on|true|yes|y|enabled` turns user memory on)
- `CODEWHALE_MEMORY_PATH`
- `CODEWHALE_TELEMETRY` / `DEEPSEEK_TELEMETRY` (legacy alias) — anonymous usage
  counting is on by default in the current 0.9.12 source, with a disclosure
  naming Codewhale and PostHog and an easy durable opt-out. Prior explicit
  declines remain off. Accepts `0|1|true|false|yes|no|on|off|enabled|
  disabled`. An explicit "off" is a **floor**: it beats `--telemetry true` and
  `telemetry = true` in config, and a value this list cannot read also resolves
  to off, because a typo in a kill switch must never resolve to "on". See
  [`TELEMETRY.md`](TELEMETRY.md).
- `CODEWHALE_TELEMETRY_ENDPOINT` / `DEEPSEEK_TELEMETRY_ENDPOINT` (legacy alias)
  — `https://`, or plain `http://` only for loopback. Overrides the config file.
  Unset selects the shipped default,
  `https://telemetry.codewhale.net/v1/telemetry`; setting it to the **empty
  string** routes batches to a local dry-run file and contacts nobody. Either
  way it only decides where a session sends — it cannot override an opt-out.
- `CODEWHALE_ALLOW_SHELL` (`1`/`true` enables)
- `CODEWHALE_APPROVAL_POLICY` (`on-request|untrusted|never`)
- `CODEWHALE_SANDBOX_MODE` (`read-only|workspace-write|danger-full-access|external-sandbox`)
- `CODEWHALE_NO_NEW_PRIVS` (`0`/`false`/`no`/`off`/`disabled` opts out) — Linux only. The
  TUI process sets the kernel's irreversible no-new-privileges flag at startup
  as defense-in-depth, which blocks `sudo`/`su`/setuid helpers for Codewhale's
  whole process tree. The flag is already skipped when the startup sandbox
  mode resolves to `danger-full-access` (#5723), so this variable is the
  explicit override in the remaining cases: set it to a falsey value before
  launching if you administer through Codewhale as a wheel-group user under a
  narrower posture and need escalation to work (#5413), or set a truthy value
  to force the flag on even under `danger-full-access`. Unset, the posture
  decides; the other startup hardening (no ptrace, no core dumps) always
  stays on.
- `CODEWHALE_MANAGED_CONFIG_PATH`
- `CODEWHALE_REQUIREMENTS_PATH`
- `CODEWHALE_MAX_SUBAGENTS` (clamped to `1..=128`)
- `CODEWHALE_TASKS_DIR` (runtime task queue/artifact storage, default
  `~/.codewhale/tasks`, with legacy `~/.deepseek/tasks` fallback when only the
  legacy directory exists)
- `CODEWHALE_RUNTIME_DIR` (override the runtime thread store root). Interactive
  sessions default to `$CODEWHALE_HOME/sessions/<session-id>/runtime` so each
  Codewhale process owns its own store (#5630). The store is single-owner: a
  second process on the **same** root fails at startup. Set this variable to
  share one store across processes, or when the runtime API server should use a
  stable non-session path. Unset, the API/server path remains
  `$CODEWHALE_HOME/tasks/runtime`. Legacy alias: `DEEPSEEK_RUNTIME_DIR`.
- `CODEWHALE_ALLOW_INSECURE_HTTP` (`1`/`true` allows non-local `http://` base URLs; default is reject)
- `CODEWHALE_FORCE_HTTP1` (`1|true|yes|on` pins the HTTP client to HTTP/1.1, disabling HTTP/2; useful on Windows or behind proxies that mishandle long-lived H2 streams)
- `CODEWHALE_HOME` (override the base data directory; defaults to `~/.codewhale`).
  If you previously exported `DEEPSEEK_HOME`, rename it to `CODEWHALE_HOME`;
  the old env var is not used for new Codewhale state paths.
- `CODEWHALE_RELEASE_BASE_URL` (release asset mirror used by `codewhale update`
  and by TUI startup update checks when `[update].update_uri` is not set, or as
  a fallback when that configured URI cannot be fetched)
- `CODEWHALE_AUTOMATIONS_DIR` (override the automations storage directory; uses
  `~/.codewhale/automations` by default, with legacy `~/.deepseek/automations`
  fallback when only the legacy directory exists)
- `NO_ANIMATIONS` (`1|true|yes|on` forces `low_motion = true` and
  `fancy_animations = false` at startup, regardless of the saved
  settings; see [`docs/ACCESSIBILITY.md`](./ACCESSIBILITY.md)).
- `SSL_CERT_FILE` — corporate-proxy / TLS-inspecting MITM users
  point this at a PEM bundle (or single DER cert) and the cert(s)
  get added alongside the platform's system trust store. Failures
  log a warning and continue — the existing system roots still
  apply.

### Instruction sources (`instructions = [...]`, #454)

Add a list of additional system-prompt sources that get
concatenated, in declared order, alongside the auto-loaded
`AGENTS.md`:

```toml
instructions = [
    "./AGENTS.md",
    "~/.codewhale/global.md",
    "~/team/agents-shared.md",
]
```

Rules:

- Paths run through `expand_path` so `~` and env vars work.
- Each file is capped at 100 KiB; oversized files are
  truncated with a `[…elided]` marker rather than skipped.
- Missing files are skipped with a tracing warning so a stale
  entry doesn't fail the launch.
- Only user-owned config, profiles, and managed config may set this array.
  Project config (`<workspace>/.codewhale/config.toml`, or legacy
  `<workspace>/.deepseek/config.toml`) ignores `instructions` so a cloned repo
  cannot choose arbitrary local files to place into the prompt.

### Hooks

Hooks are a **TUI runtime feature**. They fire from the interactive TUI and
the engine turn loop it drives; `codewhale exec`, the CLI subcommands, the
app-server / ACP surfaces, and the `workflow` tool do not fire them.

[`docs/HOOKS.md`](HOOKS.md) is the authoritative reference for all eleven hook
events — their firing points, environment variables, stdin payloads, timeout
and background semantics, and which three of them can steer Codewhale. The
sections below cover the configuration surface and the steering contracts in
more depth.

Two contract points worth reading there before writing a hook:

- `background = true` means **submitted and never awaited**. The hook still
  gets the documented stdin payload and the same timeout, but it has no exit
  code and cannot steer.
- A condition that references context its event never carries (an `exit_code`
  condition outside `tool_call_after` / `on_error`, a `mode` condition on
  `shell_env`, a tool condition on a non-tool event) is **rejected at load**,
  logged, and shown in `/hooks list`. It does not silently never match.
  Rejection is per entry, so a broken hook never drops another one that merely
  shares its `name` or is likewise unnamed.

### `/hooks` listing

Run `/hooks` (or `/hooks list`) inside the TUI to see every
configured lifecycle hook grouped by event, including each
hook's name, command preview, effective timeout, and condition. When
`[hooks].default_timeout_secs` is set it replaces every per-hook
`timeout_secs`, and the listing shows that effective value and names the
override rather than echoing the per-hook number. A
`default_timeout_secs = 0` is rejected at load — it would expire every hook
in the config immediately — so the override is ignored, per-hook
`timeout_secs` applies, the listing shows that per-hook value with no
override provenance, and the rejection appears under `configuration
problems`. The
`[hooks].enabled` flag's state is shown at the top so it's
obvious when hooks are globally suppressed, and any entry rejected
at load is listed under `configuration problems` with the reason.
Hooks are configured under `[[hooks.hooks]]` entries — see
[`docs/HOOKS.md`](HOOKS.md) for the full schema.

### Mutable `message_submit` hooks

`message_submit` hooks run before a submitted message is added to
history or sent to the model. Unlike observer-only lifecycle hooks,
non-background `message_submit` hooks can replace or block the
submitted text.

```toml
[[hooks.hooks]]
event = "message_submit"
command = "~/.codewhale/hooks/inject-context.sh"
timeout_secs = 2
continue_on_error = true
```

The hook receives JSON on stdin:

```json
{
  "event": "message_submit",
  "text": "original user text",
  "text_bytes": 18,
  "text_original_bytes": 18,
  "text_truncated": false,
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

The entire serialized document is capped at 32 KiB. Codewhale retains the
largest UTF-8-safe `text` prefix that fits after JSON escaping and bounded
metadata, and the three `text_*` fields make truncation explicit. Immediate
messages, restored queue entries, merged steers, and prior-hook replacements
all cross this same serialization boundary.

If the hook exits `0` and prints JSON with a non-empty string `text` field,
that value replaces the submitted text:

```json
{ "text": "replacement user text" }
```

Exit `0` with empty stdout, or stdout JSON without `text`, leaves
the current text unchanged. A JSON `text` field must not be empty;
`{"text":""}` is treated as invalid stdout and ignored. Exit `2`
blocks the submission before the turn starts; a structured `reason` field can
provide the bounded, redacted status message shown in the TUI. Raw stdout,
stderr, and process-error text are not copied into denial receipts.
Other non-zero exits follow the hook's `continue_on_error` setting.
Timeouts and spawn failures are also surfaced as transient TUI status
messages when `continue_on_error = true` lets submission continue.

Multiple `message_submit` hooks run in config order, and each hook
receives the text produced by the previous hook. Hooks marked
`background = true` are observer-only and cannot transform or block
the message — they still receive the same stdin payload and the same
environment, they are simply never awaited. Existing environment
variables remain available.
`shell_env` hooks keep their existing `KEY=VALUE` stdout contract;
JSON stdout contracts exist for `message_submit` (above) and
`tool_call_before` (below).

### `tool_call_before` decision hooks

`tool_call_before` hooks run before each tool call executes. In
addition to the legacy hard deny (exit code `2`, which always wins
regardless of stdout), a foreground hook may print a JSON decision on
stdout with exit code `0`:

```json
{
  "decision": "allow" | "deny" | "ask",
  "reason": "human-readable explanation (used for deny)",
  "updatedInput": { "command": "ls -la" },
  "additionalContext": "text appended to the tool result for the model"
}
```

All fields are optional. Empty stdout, non-JSON stdout, and JSON
without a `decision` field behave exactly as before (allow). An
unrecognized `decision` string logs a fixed warning without echoing the
untrusted value and is treated as allow.

- `deny` blocks the tool; the model receives a permission-denied tool
  result containing `reason`.
- `ask` forces the interactive approval prompt in Ask and Auto-Review even for
  tools that would otherwise auto-run. Full Access does not open tool-approval
  prompts, so hook `ask` does not downgrade that posture.
- `updatedInput` must be a JSON object; it replaces the tool input
  before execution. When several hooks supply it, the last hook wins.
- `additionalContext` is appended to the tool result sent back to the
  model as `[hook context] ...`. Multiple hooks' contexts are
  concatenated.

When multiple hooks match, precedence is deny > ask > allow. Hooks
marked `background = true` cannot steer tool calls — they are
submitted and never awaited, so they have no verdict to contribute.

A foreground hook that produced no verdict at all — it hit its timeout, the
process could not be started, or a strict process exited non-zero without an
explicit JSON decision — is not treated as permission. If
*that* hook is configured with `continue_on_error = false`, the outcome
denies the tool call and the denial names the hook and a bounded reason.
Strictness is read off the hooks that actually matched this call, so a
strict gate scoped to another tool cannot deny it, and a lenient hook's
timeout does not deny merely because a strict hook exists elsewhere in
config. Under the default `continue_on_error = true` the outcome is
logged and the call proceeds.

`reason` and `additionalContext` are capped (2 000 characters per field,
8 000 for the concatenated context of one call) and stripped of control
characters before they reach the TUI or the model.

Example deny hook:

```toml
[[hooks.hooks]]
event = "tool_call_before"
command = '''echo '{"decision":"deny","reason":"blocked by project policy"}' '''
condition = { type = "tool_name", name = "exec_shell" }
```

Example ask hook (force approval for every MCP tool):

```toml
[[hooks.hooks]]
event = "tool_call_before"
command = '''echo '{"decision":"ask"}' '''
condition = { type = "tool_name", name = "mcp__*" }
```

Example input rewrite:

```toml
[[hooks.hooks]]
event = "tool_call_before"
command = "~/.codewhale/hooks/clamp-shell-timeout.sh"
condition = { type = "tool_name", name = "exec_shell" }
```

where the script reads the hook context, then prints
`{"updatedInput": {...}}` with the adjusted arguments.

`tool_name` conditions support `*` globs: `mcp__*` matches every MCP
tool (e.g. `mcp__github__create_issue`) but not built-ins like
`read_file`; exact names keep matching exactly. Other regex
metacharacters in the pattern are matched literally.

### Project-local hooks

Repositories can ship policy in `<workspace>/.codewhale/hooks.toml`,
using the same shape as the `[hooks]` table (top-level fields plus
`[[hooks]]` entries). Project hooks are executable shell
configuration, so Codewhale only loads them after the workspace has
been trusted in user-owned config through the trust prompt or a
`[projects."<workspace>"] trust_level = "trusted"` entry. Session
`/trust on` mode does not enable repo-supplied hooks by itself, and
repo-local legacy markers such as `.deepseek/trusted` do not enable
project hooks. Once trusted, project hooks are appended after global
hooks from `config.toml`, so they run last and, for `updatedInput`,
win ties. A malformed trusted project file logs a warning and startup
falls back to global hooks only.

```toml
# .codewhale/hooks.toml
[[hooks]]
event = "tool_call_before"
command = '''echo '{"decision":"deny","reason":"no shell in this repo"}' '''
condition = { type = "tool_name", name = "exec_shell" }
```

### Turn-end observer hooks

`turn_end` hooks observe the end of each model turn after post-turn
state, usage totals, cost accounting, notifications, receipts, and
queue recovery have been updated. They receive JSON on stdin and are
observer-only: stdout is ignored, failures are logged as warnings, and
the hook cannot block user input, mutate the transcript, or change the
next queued follow-up.

Observer-only UI events share one 32-entry queue and two persistent workers;
the terminal loop uses non-blocking submission and does not create a thread per
event. A full queue or unavailable dispatcher drops that observer event and is
kept as an event-specific error toast, independent of ordinary agent/turn
status text.

```toml
[[hooks.hooks]]
event = "turn_end"
command = "~/.codewhale/hooks/turn-audit.sh"
timeout_secs = 2
continue_on_error = true
```

The payload includes common hook metadata plus post-turn accounting:

```json
{
  "event": "turn_end",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "created_at": "2026-07-12T10:30:00+00:00",
  "model_backed": true,
  "provider": "deepseek",
  "model": "deepseek-chat",
  "billing_surface": null,
  "turn_id": "turn_12345678",
  "status": "completed",
  "error": null,
  "duration_ms": 1834,
  "usage": {
    "input_tokens": 1200,
    "output_tokens": 180,
    "prompt_cache_hit_tokens": 900,
    "prompt_cache_miss_tokens": 300,
    "prompt_cache_write_tokens": 0,
    "reasoning_tokens": null,
    "reasoning_replay_tokens": null
  },
  "totals": {
    "session_tokens": 1380,
    "conversation_tokens": 1380,
    "input_tokens": 1200,
    "output_tokens": 180
  },
  "tool_count": 2,
  "queued_message_count": 1,
  "stop_hook_active": false
}
```

`created_at` anchors time-window pricing; `provider` and `model` identify the
effective route used for model-backed turns. `billing_surface` is an optional,
non-secret classification derived from the endpoint that actually served the
turn. Recognized StepFun routes emit `stepfun-payg` or `stepfun-plan`; the raw
base URL is never written to hook or runtime records. Runtime `TurnRecord`
exports call the same field `effective_billing_surface`, which `scorecard`
accepts as an alias. This keeps subscription quota separate from token-priced
usage. Unrecognized and custom endpoints remain `null` and unpriced.

Shell-only lifecycle completions set `model_backed` to `false` and may report a
`null` provider; offline scorecards exclude those records from model token and
cost totals. Completion-only shell, manual-compaction, and purge events that do
not have a matching `TurnStarted` retain the observer notification with a
synthetic `lifecycle_<uuid>` turn id and the time the completion was observed.

For `interrupted` or `failed` turns, `status` reflects that terminal
state and `error` carries the engine error string when one is available.
`stop_hook_active` is reserved for future re-entry protection and is
currently always `false`.

### Sub-agent lifecycle hooks

`subagent_spawn` and `subagent_complete` hooks observe sub-agent lifecycle
events. They receive bounded JSON metadata on stdin and are observer-only:
hook failures are logged as warnings and do not block sub-agent scheduling,
change prompts, or change results. For these observer events,
`continue_on_error` has no effect: later matching hooks still run even when an
earlier hook exits non-zero.

```toml
[[hooks.hooks]]
event = "subagent_complete"
command = "~/.codewhale/hooks/subagent-audit.sh"
timeout_secs = 2
continue_on_error = true
```

`subagent_spawn` receives:

```json
{
  "event": "subagent_spawn",
  "agent_id": "agent_12345678",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234,
  "prompt_preview": "bounded prompt preview",
  "prompt_truncated": false
}
```

`subagent_complete` receives the same common fields plus terminal metadata:

```json
{
  "event": "subagent_complete",
  "agent_id": "agent_12345678",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234,
  "status": "completed",
  "result_preview": "bounded result preview",
  "result_truncated": false
}
```

Previews are capped before delivery so lifecycle hooks do not receive full
sub-agent prompts, transcripts, or unbounded results. Use the transcript handle
returned by `agent` when full sub-agent details are needed.

### Running-turn input

Composer shortcuts keep the same role throughout a session:

- **Enter** sends when idle and queues a next-turn follow-up while busy. The
  behavior does not change before versus after the provider's first token.
- With an empty composer and queued follow-ups visible, **Enter** sends the
  oldest queued follow-up into the active turn now.
- **Ctrl+Enter** (or **Cmd+Enter** when the terminal forwards it) explicitly
  steers the active turn. It sends normally when idle.
- By default, **Shift+Enter**, **Alt+Enter**, and **Ctrl+J** insert a newline.
- Set `composer_multiline_mode = true` to make **Enter** insert a newline and
  **Shift+Enter** send instead. **Alt+Enter**, **Ctrl+J**, and supported
  **Ctrl+Enter** / **Cmd+Enter** behavior is unchanged.
- **Ctrl+G** and **Ctrl+S** only stash drafts; they never send or steer.

### Composer stash (`/stash`, Ctrl+G / Ctrl+S)

Press **Ctrl+G** in the composer to park the current draft to
`~/.codewhale/composer_stash.jsonl`. `/stash list` shows parked
drafts with one-line previews and timestamps; `/stash pop`
restores the most recently parked draft (LIFO); `/stash clear`
wipes the file. Capped at 200 entries; multiline drafts round-trip intact.
**Ctrl+S** remains an alias in terminals that forward it; Cursor and VS Code
reserve Ctrl+S for Save, so Ctrl+G is the portable default.

## Settings File (Persistent UI Preferences)

codewhale also stores user preferences in:

- `~/.codewhale/settings.toml` on new installs
- `~/.deepseek/settings.toml` or the legacy platform config-dir
  `deepseek/settings.toml` when an existing settings file is present

Notable settings include `auto_compact`, which uses a model-aware default-on
policy for known context windows up to the 1M-token V4 class. Automatic
compaction runs before the active model limit and carries the compacted summary
forward into the next request. The trigger defaults to
`auto_compact_threshold_percent = 80`. Users who prefer manual continuity can
persist `auto_compact = false`; manual `/compact` / Ctrl+L remains available.
You can inspect or update these from the TUI with `/settings` and `/config`
(interactive editor).

Common settings keys:

- `theme` (`system`, `terminal`, `underwater`, `underwater-retro`,
  `shoreline`, `shoreline-light`, `dark`, `light`, `grayscale`,
  `catppuccin-mocha`, `tokyo-night`, `dracula`, `gruvbox-dark`, `claude`,
  `matrix`, `solarized-light`, `uwu`; default `underwater`): `underwater` is
  the dark navy fresh-install default, `shoreline` is its warm charcoal
  alternative and `shoreline-light` the light variant, `system` follows terminal
  background detection, `dark`/`light` use the Codewhale Whale pair,
  `terminal` inherits the host terminal, `grayscale` is the low-opinion
  black/white theme, and the named community presets apply across the TUI.
  Aliases such as `whale`, `mono`, `black-white`, `tokyonight`, and `gruvbox`
  are accepted. In Whale, cobalt blue owns action/focus, seafoam owns live
  work, Signal Gold owns human decisions and the whale, coral owns warnings,
  rose owns danger, violet owns Operate, and green remains completed/verified.
  Text labels, markers, and motion policy carry the same states when color is
  unavailable; color is never the only cue.
  User-authored overlays live only at `~/.codewhale/themes/<name>.json` (or
  `$CODEWHALE_HOME/themes/<name>.json`) and are selected with
  `/theme custom:<name>`. The filename is a bounded slug, symlinks and files
  over 64 KiB are refused, colors must be `#RRGGBB`, and unknown fields fail
  validation. `/theme schema` prints the embedded JSON Schema and `/theme path`
  shows the exact directory. An overlay names one compiled `base` theme and
  changes only listed semantic colors; it cannot include or read another file.
  Open `/theme` to browse valid overlays, preview them live, and keep the
  active `custom:<name>` selector when the picker is opened without moving.
- `auto_compact` (on/off, model-aware default on for known context windows
  unless explicitly configured)
- `auto_compact_threshold_percent` (10-100, default `80`): pre-send
  auto-compaction threshold used only when `auto_compact` is enabled.
- `paste_burst_detection` (on/off, default on): fallback rapid-key paste
  detection for terminals that do not emit bracketed-paste events. This is
  independent of terminal bracketed-paste mode.
- `work_surface_placement` (`bottom`, `top`, `left`, `right`, or `off`;
  default `bottom`): places the workbar — Tasks / To-do / Workers — under the
  composer (the default bottom workbar), above the transcript, in a side
  workbar, or hides it entirely (`off`). Side choices fall back to the top
  layout on narrow terminals without changing the saved preference. Set it
  live with `/config work_surface_placement right --save` (or `left` / `top` /
  `bottom` / `off`).
- `rail_panel` (`tasks`, `agents`, `background`, `files`, `notepad`,
  `context`, `git`, `price`; default `tasks`, alias key `rail`): which panel
  the workbar shows. Panel selection is orthogonal to placement. `tasks` is
  the full live work list (to-dos, then sub-agents); `agents` narrows to the
  sub-agent rows; `background` lists background shells and automations;
  `files` lists touched files; `notepad` shows the workspace notes; `context`
  is a read-only session-facts list; `git` shows branch status; `price`
  shows cost. In every panel except `context`, rows are selectable and
  clickable and open their detail surface. `Alt+!`/`Alt+@`/`Alt+#`/`Alt+$`
  switch panels live.
- `work_surface_top_height` (2–16) and `work_surface_side_width` (26–80):
  ceilings for the top strip's height and the side workbar's width. Both are
  normally persisted by dragging the divider rather than edited by hand; the
  strip still auto-fits its content below the ceiling.
- `focus_texture` (`off`, `scrim`, or `grain`; default `off`): focus-context
  texture for modal views. `scrim` dims the already-rendered background
  outside the focused modal toward the theme surface; `grain` sprinkles
  sparse dots over blank cells there. The texture is static (no time
  component, so it is unaffected by `low_motion`), never writes over a cell
  that carries text, and preserves the 4.5:1 body-text contrast floor
  wherever both colors are resolvable. It is skipped entirely on frames
  below the ambient-life minimum size and when the focused modal already
  covers 90% or more of the frame. Set it live with
  `/config focus_texture scrim --save`.
- `mention_menu_limit` (integer, default `128`): maximum number of
  `@`-mention popup candidates retained before the composer renders the
  visible window. The visible rows still depend on terminal height.
- `mention_walk_depth` (integer, default `10`): maximum workspace depth for
  `@`-mention completion walks. Set to `0` for unlimited depth in deeply
  nested workspaces; keep the default in very large repos unless needed.
- `mention_menu_behavior` (`fuzzy`, `browser`; default `fuzzy`): controls how
  `@`-mention completions are populated. `fuzzy` searches the workspace and
  applies mention frecency. `browser` lists only the immediate children of the
  currently typed directory segment in deterministic alphabetical order.
- `show_thinking` (on/off)
- `thinking_default_expanded` (on/off, default off): renders thinking blocks
  expanded initially when `show_thinking` is enabled. Space still toggles the
  selected block, and it decides only the blocks you have not touched: a block
  you expanded or collapsed yourself keeps that state if you change this
  setting later. This is useful in SSH/tmux environments where the Space
  binding may be intercepted.
- `thinking_preview_lines` (integer, default `2`): how many body rows a
  **collapsed** completed thought still shows. `0` is header-only; `10` is
  the older dump. Live streaming preview is unchanged. Expand a block with
  Space, or set `thinking_default_expanded` to open every block.
- `help_expand_groups` (on/off, default off): start Help/shortcuts with every
  group expanded. Default folds the long tail (Grok-style); type-to-filter
  still unfolds matches.
- `pin_last_prompt` (on/off, default on): pin the last user prompt at the top
  of the transcript viewport after it scrolls off.
- `show_tool_details` (on/off)
- `inline_diffs` (`full`, `summary`, or `off`; default `full`): controls the
  inline presentation of successful structured File mutations. `full` shows a
  bounded red/green diff and semantic statistics, `summary` keeps only the
  statistics, and `off` keeps the calm changed-file outcome. All three retain
  the exact applied change in the selected File receipt's Alt/Option+V detail.
  Failure and cancellation never render a successful diff. Save the choice
  with `/config inline_diffs <mode> --save`.
- `locale` (`auto`, `en`, `ja`, `zh-Hans`, `zh-Hant`, `pt-BR`, `es-419`, `vi`,
  `ko`; default `auto`): UI chrome locale. `auto` checks `LC_ALL`,
  `LC_MESSAGES`, then `LANG`; unsupported locale selections resolve to English.
  Every shipped pack holds full `en.json` parity, so no string falls back
  to English. The runtime also exposes the resolved locale in the system
  prompt as the fallback natural language for V4 reasoning and replies when the
  latest user message is ambiguous. Clear user language still takes priority;
  Chinese turns should produce Chinese `reasoning_content` and Chinese final
  replies even when the resolved locale is English.
- `background_color` (`#RRGGBB`, `RRGGBB`, or `default`): optional main TUI
  background color applied to the root, header, transcript, and footer
  surfaces while preserving panel contrast.
- `cost_currency` (`usd`, `cny`; default `usd`): currency used by the footer,
  context panel, `/cost`, `/tokens`, and long-turn notification summaries. The
  aliases `rmb` and `yuan` normalize to `cny`.
- `default_mode` (`agent`, `plan`, or `operate`; legacy values are accepted for migration but are not live mode vocabulary)
- `launch_screen` (legacy, migration-only): this historical `on`/`off` value
  is still accepted when reading an existing settings file, but it no longer
  changes behavior and is omitted from new saves. A fresh interactive launch
  always opens Tideline Startup; only an explicit resume or an explicit
  initial prompt enters the live session directly.
- `sidebar_focus` (legacy, migration-only): the classic right sidebar this key
  configured was removed in the 0.9.4 rail unification. The key is still read
  once so old settings carry forward, then folds into the live keys:
  `pinned`/`work`/`plan`/`todos` become `rail_panel = "pinned"`,
  `agents`/`subagents` become `rail_panel = "agents"`, `context`/`session`
  become `rail_panel = "context"`, `tasks`/`auto` (the old default) become the
  `tasks` panel, `sessions` enables `sessions_rail`, and `hidden` turns the
  workbar off via `work_surface_placement = "off"`. An explicit `rail_panel`
  in the file always wins over the migrated value. Configure the workbar with
  `rail_panel` and `work_surface_placement`, not this key.
- `sessions_rail` (`on`/`off`; default `off`): show the persistent Sessions
  list in the workbar. Rows list this workspace's recent
  non-archived sessions, newest first, with the active one marked; activating a
  row opens the session picker preselected on it (`/sessions open <id>`), so
  resume keeps its single implementation. Rows are projected from cached
  session metadata — the list never reads a transcript per frame, and never
  contacts a provider.
- `session_auto_resume` (`on`/`off`; default `off`): reattach to this
  workspace's most recent session when Codewhale starts. Off by default so
  plain `codewhale` keeps starting fresh. `--resume`, `--continue`, and
  `--fresh` always take precedence. When it is on, startup still refuses to
  resume a session that is archived, fails to load, or is recorded against a
  different workspace; each of those falls back to a fresh transcript and says
  which session was skipped and why. It applies to the interactive launch only
  — `codewhale "<prompt>"` and `codewhale exec` are never silently prefixed
  with a prior conversation.
- `max_input_history` (number of submitted input history entries; cleared
  drafts are also kept locally for composer history search). Note the spelling:
  the serde field on disk is `max_input_history`
  (`crates/tui/src/settings.rs:426`, default 100). `max_history` is the key
  name accepted by `/config set` and `settings.set()` (`settings.rs:1388`), not
  a settings.toml key — writing `max_history` into the file is silently
  ignored.
- `default_model` (model name override)

`/task digest` (alias `/tasks digest`) renders the canonical Work Graph
operations and four-state To-do list as plain text, running work first. It
reads the same snapshots as the styled Work surface and owns no parallel
progress state.

Plan and Work are the everyday visible modes in the UI; Operate is an explicit
preview entry while its Workflow control surface is still being built. Switch
between them with `/mode`. For compatibility, older settings files with
`default_mode = "normal"` still load as `agent`.

Localization scope is tracked in [LOCALIZATION.md](LOCALIZATION.md). The v0.7.6
core pack covers high-visibility TUI chrome only; provider/tool schemas,
personality prompts, and full documentation remain English unless explicitly
translated later.

Readability semantics:

- Selection uses a unified style across transcript, composer menus, and modals.
- Footer hints use a dedicated semantic role (`FOOTER_HINT`) so hint text stays readable across themes.

### Token Quantities and Drivers

DeepSeek V4 prefix caching makes token labels matter. These quantities are kept
separate:

| Quantity | Meaning | Allowed to drive |
|---|---|---|
| Active request input estimate | Conservative estimate of the next request's live system prompt and transcript payload. | Header/footer context percent, auto-compaction trigger, opt-in Flash seam trigger, and emergency overflow preflight. |
| Reserved response headroom | The effective request cap plus `1024` safety tokens on every route. Normal no-override requests start at `65536`; a smaller route/provider ceiling narrows that value, and an explicit output override raises it only within the resolved route window and output ceiling. The identical cap reaches the wire and drives preflight; reasoning effort does not add a second hidden reservation. A separately published route input ceiling independently clamps the spendable input budget. | Emergency overflow budget checks only. |
| Cumulative API usage | Provider-reported input plus output tokens summed across completed API calls; multi-tool turns may count the same stable prefix more than once. | Session usage and approximate cost telemetry only. |
| Prompt cache hit/miss | Provider cache telemetry for the most recent call when available. | Cache-hit display and cost estimation only; never compaction or seam triggers. |
| Context percent | Active request input estimate divided by the model context window. | Display only; it mirrors the active-input basis used by context safeguards. |
| Cost estimate | Approximate spend from provider usage and configured DeepSeek rates. | Display only. |

For known context-window models, including 1M-class V4 models, replacement
compaction is enabled by default unless the user explicitly configures
`auto_compact = false`. It fires at the active model's compaction threshold and
replaces old history with recent user context followed by one ordinary
checkpoint message. The standing system prompt remains unchanged. Unknown model
ids remain opt-in.

### Command Migration Notes

If you are upgrading from older releases:

- Old: `/codewhale`
  New: `/links` (aliases: `/dashboard`, `/api`)
- Old: `/set model deepseek-reasoner`
  New: `/config` and edit the `model` row to `deepseek-v4-pro` or `deepseek-v4-flash`
- Old: visible `Normal` mode or `default_mode = "normal"`
  New: use `Agent` / `default_mode = "agent"`; legacy `normal` still maps to `agent`
- Old: discover `/set` in slash UX/help
  New: use `/config` for editing and `/settings` for read-only inspection

## Key Reference

### Kimi Code membership model IDs

The exact `https://api.kimi.com/coding/v1` endpoint accepts `k3`, `k3-256k`,
`kimi-for-coding`, and `kimi-for-coding-highspeed`. Use `k3-256k` for a fixed
262,144-token K3 window; use bare `k3` with `context_window = 1048576` only
when the membership plan includes the 1M entitlement. Both K3 ids use the same
reasoning contract, and all four membership ids omit generic sampling fields.

### Core keys (used by the TUI/engine)

- `provider` (string, optional): `deepseek` (default), `deepseek-anthropic`, `nvidia-nim`, `openai`, `atlascloud`, `wanjie-ark`, `volcengine`, `openrouter`, `xiaomi-mimo`, `novita`, `fireworks`, `siliconflow`, `arcee`, `siliconflow-CN`, `moonshot`, `sglang`, `vllm`, `ollama`, `ollama-cloud`, `huggingface`, `modelscope`, `together`, `qianfan`, `openai-codex`, `anthropic`, `openmodel`, `zai`, `stepfun`, `minimax`, `deepinfra`, `sakana`, `longcat`, `opencode-go`, `meta`, `mistral`, `telecomjs`, `xai`, `orcarouter`, `modelstudio-token-plan`, `google`, `edenai`, or `custom`. Legacy `deepseek-cn` configs are still accepted as an alias for `deepseek`; DeepSeek uses the same official host [`https://api.deepseek.com`](https://api-docs.deepseek.com/) worldwide. `deepseek-anthropic` targets DeepSeek's Anthropic Messages-compatible endpoint at `https://api.deepseek.com/anthropic` using `DEEPSEEK_API_KEY`; `nvidia-nim` targets NVIDIA's NIM-hosted DeepSeek endpoints through `https://integrate.api.nvidia.com/v1`; `openai` targets a generic OpenAI-compatible endpoint, defaulting to `https://api.openai.com/v1`; `atlascloud` targets AtlasCloud's OpenAI-compatible endpoint at `https://api.atlascloud.ai/v1`; `wanjie-ark` targets Wanjie Ark's OpenAI-compatible endpoint at `https://maas-openapi.wanjiedata.com/api/v1`; `volcengine` targets Volcengine Ark's OpenAI-compatible coding endpoint at `https://ark.cn-beijing.volces.com/api/coding/v3`; `openrouter` targets `https://openrouter.ai/api/v1`; `xiaomi-mimo` targets Xiaomi MiMo's OpenAI-compatible endpoint, using `https://token-plan-sgp.xiaomimimo.com/v1` by default for Token Plan keys (`tp-...`) and `https://api.xiaomimimo.com/v1` for pay-as-you-go keys. For Token Plan accounts outside the Singapore default, set `base_url` explicitly or use `mode = "token-plan-cn"` for China and `mode = "token-plan-ams"` for Europe/Amsterdam; `novita` targets `https://api.novita.ai/openai/v1`; `fireworks` targets `https://api.fireworks.ai/inference/v1`; `siliconflow` targets SiliconFlow, defaulting to `https://api.siliconflow.com/v1`; `arcee` targets Arcee AI's OpenAI-compatible endpoint at `https://api.arcee.ai/api/v1`; `siliconflow-CN` targets the SiliconFlow China regional endpoint through `[providers.siliconflow_cn]`; `moonshot` targets Moonshot/Kimi, defaulting to `https://api.moonshot.ai/v1`; `sglang` targets a self-hosted OpenAI-compatible endpoint, defaulting to `http://localhost:30000/v1`; `vllm` targets a self-hosted vLLM OpenAI-compatible endpoint, defaulting to `http://localhost:8000/v1`; `ollama` targets Ollama's OpenAI-compatible endpoint, defaulting to `http://localhost:11434/v1`; `huggingface` targets Hugging Face Inference Providers at `https://router.huggingface.co/v1`; `modelscope` targets ModelScope's OpenAI-compatible inference API at `https://api-inference.modelscope.cn/v1`; `together` targets Together AI at `https://api.together.xyz/v1`; `qianfan` targets Baidu Qianfan at `https://api.baiduqianfan.ai/v1`; `openai-codex` targets official ChatGPT plan inference with Codewhale-owned OAuth at `https://api.openai.com/v1`; `anthropic` targets Claude's native Messages API; `openmodel` targets OpenModel's Anthropic-compatible Messages API at `https://api.openmodel.ai`; `zai` targets Z.ai at `https://api.z.ai/api/coding/paas/v4`; `stepfun` targets StepFun at `https://api.stepfun.ai/v1`; `minimax` targets MiniMax at `https://api.minimax.io/v1`; `deepinfra` targets DeepInfra at `https://api.deepinfra.com/v1/openai`; `sakana` targets Sakana AI Fugu at `https://api.sakana.ai/v1`; `longcat` targets Meituan LongCat at `https://api.longcat.chat/openai/v1`; `opencode-go` targets the subscription-backed OpenCode Go model-aware route (Chat Completions, Responses, or Messages according to the documented model) at `https://opencode.ai/zen/go/v1`; `meta` targets Meta Model API; `mistral` targets Mistral AI's OpenAI-compatible endpoint at `https://api.mistral.ai/v1`; `telecomjs` targets TelecomJS TokenHub at `https://aigw.telecomjs.com/v1`; and `xai` targets xAI's API-key or OAuth route.
- `opencode-zen` (string provider value): selects the model-aware OpenCode Zen gateway through `[providers.opencode_zen]`. The default base URL is `https://opencode.ai/zen/v1`, the default model is `gpt-5.6`, and credentials come from `api_key`, `OPENCODE_ZEN_API_KEY`, or fallback `OPENCODE_API_KEY`—never ChatGPT/Codex OAuth. `OPENCODE_ZEN_BASE_URL` and `OPENCODE_ZEN_MODEL` are accepted. The selected model's wire comes from the curated Zen snapshot, then from the AI SDK package its Models.dev `opencode` row names (refresh with `codewhale models --update`): GPT, Grok, and Muse Spark use Responses; Claude and most Qwen rows use Anthropic Messages; DeepSeek, MiniMax, GLM, Kimi, `qwen3.8-max`, and the free rows use Chat Completions. Gemini, Models.dev rows marked `deprecated`, and models no loaded catalog lists fail closed because Codewhale has no proven supported wire contract for them. See the exact current model groups in [`PROVIDERS.md`](PROVIDERS.md#opencode-zen-protocol-catalog).
- `minimax-anthropic` (string provider value): selects MiniMax's Anthropic-compatible Messages route through `[providers.minimax_anthropic]`. The default Base URL is `https://api.minimax.io/anthropic`; set `https://api.minimaxi.com/anthropic` for China. Keep the `/anthropic` suffix because Codewhale appends `/v1/messages`. The route uses `MINIMAX_API_KEY` and defaults to `MiniMax-M3`; `MiniMax-M2.7` is also registered. Official M3 input modalities are text, image, and video, with adaptive or disabled thinking. M2.7 is text-only and always keeps thinking enabled.
- `api_key` (string, required for hosted providers): must be non-empty for DeepSeek/hosted providers (or set the provider API key env var). Self-hosted SGLang, vLLM, and local `ollama` can omit it. `ollama-cloud` requires a key saved for that provider or supplied by `OLLAMA_CLOUD_API_KEY`, then `OLLAMA_API_KEY`.
- `auth_mode` (string, optional provider-table key): selects a provider-specific authentication contract. Kimi Code membership uses `auth_mode = "api_key"` (or omit the field), a key created in the [Kimi Code console](https://www.kimi.com/code/console), `base_url = "https://api.kimi.com/coding/v1"`, and bare `model = "k3"` for K3. Codewhale gives that route a safe 262,144-token baseline; set `context_window = 1048576` only when the Kimi Code plan includes 1M access (Allegretto and above). `k3[1m]` is a Claude Code-only convention, not an API model ID, and Codewhale rejects it instead of silently changing the wire model or assuming an entitlement. `model = "kimi-for-coding"` remains the valid K2.7 compatibility route available to all Kimi Code members. Legacy `auth_mode = "kimi_oauth"` fails closed with API-key guidance and never probes, reads, refreshes, or rewrites `kimi_cli`/`kimi_code_cli` credential files. First-class OAuth requires Codewhale's own vendor-registered client identity and remains tracked in #4417.
- `base_url` (string, optional, `[providers.<name>]` key; see [Legacy top-level `base_url` and `api_key`](#legacy-top-level-base_url-and-api_key) for the older top-level spelling): defaults to `https://api.deepseek.com/beta` for DeepSeek's OpenAI-compatible Chat Completions API, including legacy `provider = "deepseek-cn"` configs. Other defaults are `https://api.deepseek.com/anthropic` for `deepseek-anthropic`, `https://integrate.api.nvidia.com/v1` for `nvidia-nim`, `https://api.openai.com/v1` for `openai`, `https://api.atlascloud.ai/v1` for `atlascloud`, `https://maas-openapi.wanjiedata.com/api/v1` for `wanjie-ark`, `https://ark.cn-beijing.volces.com/api/coding/v3` for `volcengine`, `https://openrouter.ai/api/v1` for `openrouter`, `https://token-plan-sgp.xiaomimimo.com/v1` for `xiaomi-mimo` when the API key starts with `tp-...` and `https://api.xiaomimimo.com/v1` otherwise, `https://api.novita.ai/openai/v1` for `novita`, `https://api.fireworks.ai/inference/v1` for `fireworks`, `https://api.siliconflow.com/v1` for `siliconflow`, `https://api.siliconflow.cn/v1` for `siliconflow-CN`, `https://api.arcee.ai/api/v1` for `arcee`, `https://api.moonshot.ai/v1` for `moonshot`, `https://api.minimax.io/v1` for `minimax`, `https://api.openmodel.ai` for `openmodel`, `https://api.z.ai/api/coding/paas/v4` for `zai`, `https://api.stepfun.ai/v1` for `stepfun`, `https://api.deepinfra.com/v1/openai` for `deepinfra`, `https://api.sakana.ai/v1` for `sakana`, `https://router.huggingface.co/v1` for `huggingface`, `https://api-inference.modelscope.cn/v1` for `modelscope`, `https://api.together.xyz/v1` for `together`, `https://api.baiduqianfan.ai/v1` for `qianfan`, `https://api.openai.com/v1` for `openai-codex`, `https://api.anthropic.com` for `anthropic`, `https://api.mistral.ai/v1` for `mistral`, `http://localhost:30000/v1` for `sglang`, `http://localhost:8000/v1` for `vllm`, `http://localhost:11434/v1` for `ollama`, and `https://ollama.com/v1` for `ollama-cloud`. Set `base_url = "https://token-plan-cn.xiaomimimo.com/v1"` for China-region Xiaomi MiMo Token Plan accounts or `base_url = "https://token-plan-ams.xiaomimimo.com/v1"` for Europe/Amsterdam accounts. Mistral-specific reasoning fields and polymorphic replay are enabled only on the documented first-party HTTPS `/v1` hosts; a custom Mistral base URL keeps generic Chat semantics. Set `https://api.deepseek.com` or `https://api.deepseek.com/v1` explicitly to opt out of DeepSeek beta features.
- `ollama-cloud` route: select `provider = "ollama-cloud"`, configure `[providers.ollama_cloud]` when overriding the default `https://ollama.com/v1` / `gpt-oss:120b` tuple, and save a key from [Ollama account settings](https://ollama.com/settings/keys) with `codewhale auth set --provider ollama-cloud`. Ambient precedence is `OLLAMA_CLOUD_API_KEY`, then `OLLAMA_API_KEY`; arbitrary Ollama model IDs pass through unchanged.
- Legacy Ollama Cloud migration: a released `provider = "ollama"` config whose normalized `[providers.ollama].base_url` is exactly `https://ollama.com/v1` is upgraded to the `ollama-cloud` runtime identity in memory. Only that exact tuple may read its old `ollama` provider table and secret slot. The config and secrets are never rewritten, and neighboring paths, HTTP downgrades, lookalike hosts, or an explicit `ollama-cloud` selection never consume the fallback.
- `telecomjs` base URL and catalog: `[providers.telecomjs]` defaults to `https://aigw.telecomjs.com/v1`; `TELECOMJS_BASE_URL` overrides it. With `TELECOMJS_API_KEY`, `/models` refreshes a key-scoped catalog without mixing rows into another provider.
- `edenai` gateway: select `provider = "edenai"`; `[providers.edenai]` defaults to `https://api.edenai.run/v3` and `deepseek/deepseek-v4-pro`. `EDENAI_API_KEY`, `EDENAI_BASE_URL`, and `EDENAI_MODEL` are accepted. Use `EDENAI_BASE_URL = "https://api.eu.edenai.run/v3"` for Eden AI's documented EU endpoint; the default `deepseek/deepseek-v4-pro` is only listed on the global catalog, so pair the EU endpoint with an EU-listed model such as `qwen/deepseek-v4-pro` via `EDENAI_MODEL` or `model`. The provider refreshes Eden AI's `/models` catalog, but leaves model-specific reasoning controls untouched because the gateway spans multiple model families.
- `codewhale` (Codewhale API): select `provider = "codewhale"`; `[providers.codewhale]` defaults to `https://api.codewhale.net/v1` and `deepseek/deepseek-v4-pro`. The credential is a Codewhale account API key (`cwc_key_…`) with the `models:infer` scope, read from `CODEWHALE_API_KEY` or the `codewhale` secret-store slot; `codewhale account api-keys create --name <name> --use` mints one and saves it locally. `CODEWHALE_API_BASE` overrides the origin and must be HTTPS except on loopback. The model catalog is the account's own authenticated `GET /v1/models`: ids are `provider/model` and each row states its wire (`chat-completions` → `/v1/chat/completions`, `anthropic-messages` → `/v1/messages`, `responses` → `/v1/responses`). Connect the underlying provider keys with `codewhale account keys set <provider>`.
- `concentrate` gateway: select `provider = "concentrate"`; `[providers.concentrate]` defaults to `https://api.concentrate.ai/v1` and `deepseek-v4-pro` over the OpenAI Responses wire. `CONCENTRATE_API_KEY`, `CONCENTRATE_BASE_URL`, and `CONCENTRATE_MODEL` are accepted. Model ids pass through verbatim (`gpt-5.6-sol`, `openai/gpt-5.6-sol`, or `concentrate/auto` for the gateway router). BYOK only; see [PROVIDERS.md](PROVIDERS.md#concentrate-notes).
- `mistral` model and reasoning contract: `[providers.mistral]` defaults to `mistral-code-latest`; `MISTRAL_MODEL` overrides it and the generic `CODEWHALE_MODEL` override wins when both are set. The current picker also lists `mistral-medium-latest`, `mistral-small-latest`, and `mistral-large-latest`. On exact first-party HTTPS `/v1` routes, Medium and Small accept only `reasoning_effort = "none" | "high"` and replay polymorphic thinking blocks. Deprecated native Magistral IDs may still be configured explicitly, remain always-reasoning, and never receive the adjustable effort field.
- `context_window` (integer, optional provider-table key): override the total context window for the active `[providers.<name>]` route when an OpenAI-compatible gateway, hosted model alias, or self-hosted runtime has a different limit than Codewhale's static model table. For example, `[providers.openai] context_window = 1000000` lets an OpenAI-compatible DashScope/Qwen route budget against a 1M-token window instead of the conservative fallback. For Kimi Code K3, keep `model = "k3"` and set `[providers.moonshot] context_window = 1048576` only when the membership plan includes 1M access; otherwise omit it to retain the 262,144-token safe baseline. The value must be greater than 0 and affects prompt context notes, compaction thresholds, context-pressure checks, and request output caps. Full resolution order, and how to see which rung produced the current window: [Context length (context window)](#context-length-context-window).
- `path_suffix` (string, optional provider-table key): override the chat-completions path for OpenAI-compatible gateways that do not serve `/v1/chat/completions`. For example, `[providers.openai] path_suffix = "/chat/completions"` sends chat requests to the unversioned base URL plus `/chat/completions`; `models` and `beta/*` requests keep their normal routing.
- `reasoning_stream_style` (string, optional provider-table key): override how streaming reasoning is separated from answer text for the active provider route. Use `separate_field` for `reasoning_content` / `reasoning` deltas, `inline_tags` for gateways that stream `<think>...</think>` inside `delta.content`, or `none` to render incoming content exactly as answer text. When unset, every Chat Completions route uses `separate_field` (Mistral's first-party route uses its typed thinking blocks); set `none` only for a gateway that streams its answer inside `reasoning_content`.
- `[providers.<name>.auth]` (table, optional): provider-scoped auth source metadata. `source = "command"` stores a command argv plus optional `timeout_ms`; `source = "secret"` stores a `secret_id`. This slice lets provider readiness, `/provider`, and doctor JSON report the auth source class without exposing command argv output or secret values; executing commands and resolving external secret material is handled by the follow-up resolver work.
- `insecure_skip_tls_verify` (bool, optional provider-table key): legacy compatibility key, disabled by default. When true on the active provider table, provider clients reject the configuration instead of skipping TLS certificate verification. Use `SSL_CERT_FILE` for corporate or private CA bundles; `codewhale doctor` reports stale uses of this setting.
- `default_text_model` (string, optional): defaults to `deepseek-flash` for DeepSeek and `deepseek-anthropic`, `gpt-5.6` for OpenAI, `grok-4.6` for xAI, `deepseek-ai/deepseek-v4-pro` for NVIDIA NIM, `deepseek-ai/deepseek-v4-flash` for AtlasCloud, `deepseek-reasoner` for Wanjie Ark, `DeepSeek-V4-Pro` for Volcengine Ark, `deepseek/deepseek-v4-pro` for OpenRouter and Novita, `mimo-v2.5-pro` for Xiaomi MiMo, `accounts/fireworks/models/deepseek-v4-pro` for Fireworks, `deepseek-ai/DeepSeek-V4-Pro` for SiliconFlow and DeepInfra, `trinity-large-thinking` for Arcee AI, `kimi-k2.7-code` for Moonshot, `MiniMax-M3` for MiniMax, `GLM-5.3` for Z.ai, `step-3.7-flash` for StepFun, `ernie-4.0-turbo-8k` for Qianfan, `fugu` for Sakana AI, `deepseek-ai/DeepSeek-V4-Pro` for SGLang/vLLM, no fixed id for local Ollama (Codewhale adopts a chat-capable tag from the live local catalog — coder or tool-capable tags first, then the largest context — and never an embedding or reranker model), and `gpt-oss:120b` for Ollama Cloud. Hugging Face and Together AI both default to `deepseek-ai/DeepSeek-V4-Pro`; `openai-codex` defaults to `gpt-5.6`; `anthropic` defaults to `claude-sonnet-4-6`; `openmodel` defaults to `deepseek-v4-flash`. Current public DeepSeek IDs include `deepseek-v4-pro` and `deepseek-flash` (V4.1 Flash, shipped as the unversioned id), both with 1M context windows, 384K max output, and thinking mode enabled by default. DeepSeek's live pricing/model page now labels the Pro backend `DeepSeek-V4-Pro-0813`; the callable API ID remains `deepseek-v4-pro`, so Codewhale does not send the backend label or the Claude Code-specific `deepseek-v4-pro[1m]` selector. DeepSeek retires `deepseek-chat` and `deepseek-reasoner` on July 24, 2026; direct first-party routes migrate both to `deepseek-v4-flash`, with omitted reasoning settings preserving their former non-thinking (`off`) and thinking (`high`) intent. Explicit `reasoning_effort` wins, and provider-owned ids on Wanjie Ark, aggregators, self-hosted runtimes, and custom endpoints are not globally rewritten. SiliconFlow retains its own mapping: `deepseek-reasoner` and `deepseek-r1` select its Pro model while `deepseek-chat` and `deepseek-v3` select Flash. Provider-specific mappings translate `deepseek-v4-pro` / `deepseek-v4-flash` to each provider's model ID where supported. OpenRouter also recognizes recent large IDs such as `arcee-ai/trinity-large-thinking`, `minimax/minimax-m3`, `minimax/minimax-m2.7`, `xiaomi/mimo-v2.5-pro`, `qwen/qwen3.6-flash`, `qwen/qwen3.6-35b-a3b`, `qwen/qwen3.6-max-preview`, `qwen/qwen3.6-27b`, `qwen/qwen3.6-plus`, `qwen/qwen3.7-max`, `google/gemma-4-31b-it`, `moonshotai/kimi-k2.7-code`, `moonshotai/kimi-k2.6`, `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`, and `nvidia/nemotron-3-ultra-550b-a55b`; direct Arcee uses bare IDs such as `trinity-large-thinking` and `trinity-large-preview`; direct Moonshot recognizes `kimi-k3`, `kimi-k2.7-code`, and `kimi-k2.6`. The exact Kimi Code endpoint recognizes bare `k3` for K3 and `kimi-for-coding` for K2.7; those membership IDs are distinct from the direct Moonshot IDs and are never rewritten across routes. Direct MiniMax recognizes `MiniMax-M3` and the documented M2.x chat model IDs; direct Z.ai recognizes `GLM-5.3` (the default), `GLM-5.2`, `GLM-5.1`, and `GLM-5-Turbo`, and OpenRouter recognizes the matching `z-ai/glm-5.1`, `z-ai/glm-5.2`, `z-ai/glm-5.3`, and `z-ai/glm-5-turbo` IDs — `GLM-5.3` has been live on the Z.ai Coding Plan since 2026-08-13; it inherits its catalog metadata from `GLM-5.2` until Z.ai publishes distinct 5.3 numbers and carries no price, and an explicit `GLM-5.2` selection keeps its own id; direct Sakana recognizes `fugu` and `fugu-ultra-20260615`; direct Xiaomi MiMo recognizes chat IDs `mimo-v2.5-pro`, `mimo-v2.5-pro-ultraspeed`, and `mimo-v2.5`, while TTS IDs are selected through `codewhale speech` / `tts`. Generic `openai`, `atlascloud`, `wanjie-ark`, `xiaomi-mimo`, `arcee`, `moonshot`, `minimax`, `openmodel`, `zai`, `stepfun`, `qianfan`, `sakana`, local Ollama, and Ollama Cloud model IDs are passed through unchanged after known aliases are normalized. OpenRouter and SiliconFlow provider configs with a custom `base_url` also preserve explicit model values, which lets OpenAI-compatible gateways accept bare model IDs. Use `/models` or `codewhale models` to discover live IDs from your configured endpoint. `CODEWHALE_MODEL` overrides this for a single process; `DEEPSEEK_MODEL` is the legacy alias.
- TelecomJS uses `deepseek-v4-pro` only as a conservative pre-refresh fallback. Once its key-scoped `/models` catalog is available, the picker uses those live rows; Codewhale omits unsupported reasoning request fields on this route.
- `reasoning_effort` (string, optional): `auto`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `ultra` (`ultracode` is an alias of `ultra`), or `max`; defaults to the configured UI tier. DeepSeek Platform receives top-level `thinking` / `reasoning_effort` fields. Ollama Cloud's OpenAI-compatible Chat Completions route preserves its documented `none` / `low` / `medium` / `high` / `max` ladder (`off` is sent as `none`; `xhigh` and `ultracode` normalize to `max`). Direct xAI `grok-4.7` and `grok-4.6` on exact `https://api.x.ai/v1` receive top-level `reasoning_effort = "low" | "medium" | "high" | "xhigh"` (the ladder comes from the bundled catalog row; `grok-4.5` maps `xhigh` to `high`, and rows without a documented effort such as `grok-4.3` get no field); Grok reasoning cannot be disabled, so `off` normalizes to `high`, `max`/`ultracode` to `xhigh`, and `auto` leaves the field omitted so xAI's documented default `high` applies. A custom xAI-compatible `base_url` does not inherit that dialect. Direct Moonshot `kimi-k3` on exact `https://api.moonshot.ai/v1` is always-thinking and receives only top-level `reasoning_effort = "low" | "high" | "max"`; `off` normalizes to `low`, and `medium` to `high`. Kimi Code membership `k3` on exact `https://api.kimi.com/coding/v1` instead receives nested `thinking.effort`, and its `off` setting also normalizes to enabled `low`. Normal dispatched `auto` uses Codewhale's auto-reasoning selector and sends a concrete route-normalized tier; only an omitted reasoning setting leaves the provider default in control. Neighboring gateways and model/endpoint combinations retain the generic Moonshot contract. OpenAI Codex normalizes stale `off` to `low` and sends `max` / `ultracode` as Responses `xhigh`. Z.ai receives documented `thinking` controls and treats enabled thinking as the GLM coding high/max lane. NVIDIA NIM receives equivalent settings through `chat_template_kwargs`.
- `verbosity` (string, optional): `normal` or `concise`. `normal` keeps the
  default conversational prompt. `concise` appends a prompt discipline block
  for direct, low-chatter output; CLI noninteractive commands (`exec` and
  `eval`) default to `concise` unless config/env/CLI overrides it.
  Override per process with `CODEWHALE_VERBOSITY` or the legacy
  `DEEPSEEK_VERBOSITY` alias.
- `telemetry` (bool, optional): anonymous usage counting, **`true` by default
  in the current 0.9.12 source**. Notice version `5` names Codewhale and PostHog
  and describes the opt-out policy; no acceptance is invented for a default
  user. Existing explicit declines remain off. An explicit `false` here is the
  durable *opt-out*: it deletes the random install id, truncates buffered and
  dry-run events, and leaves a tombstone reasserted while the setting is false.
  It is a floor: `--telemetry true` and `CODEWHALE_TELEMETRY=1` lose to it.
  Use `/settings` or `codewhale config set telemetry true` to turn counting
  back on explicitly for new sessions through the existing privacy transition. `CODEWHALE_TELEMETRY`
  (legacy alias `DEEPSEEK_TELEMETRY`) and `--telemetry false` provide a run-scoped
  kill switch that stops collection and delivery without erasing the owner's
  state. A repo-local `.codewhale/config.toml` cannot set this preference.
  `codewhale config telemetry` shows the disclosure; `codewhale config get
  telemetry` reports preference and privacy status. Full schema and opt-out
  behavior: [`TELEMETRY.md`](TELEMETRY.md).
- `telemetry_endpoint` (string, optional): where batches are POSTed. Leaving it
  unset selects the shipped default,
  **`https://telemetry.codewhale.net/v1/telemetry`** — the first-party ingest
  service described in [`TELEMETRY.md`](TELEMETRY.md), whose source is in
  `telemetry-ingest/`. This key decides only *where* a permitted session sends;
  it cannot override an opt-out. Setting it
  to the **empty string** is how you stay enabled and contact nobody: each batch
  is then written to `$CODEWHALE_HOME/telemetry/dryrun.jsonl` and no HTTP client
  is constructed at all, so you can read exactly what would have been sent. Any
  other value replaces the default outright. `https://` is required; plain
  `http://` is accepted only for loopback hosts, and there is no environment
  variable that overrides that refusal. A rejected endpoint turns telemetry off
  for the run rather than falling back to plaintext or to the default. Override
  per process with `CODEWHALE_TELEMETRY_ENDPOINT` (legacy alias
  `DEEPSEEK_TELEMETRY_ENDPOINT`), where an empty value means the same "contact
  nobody". A repo-local `.codewhale/config.toml` cannot set it.
- `allow_shell` (bool, optional): in interactive TUI Agent sessions, omitting
  this keeps shell tools available with approval prompts; setting it to `false`
  hides shell tools. Headless, durable-task, and other noninteractive profiles
  keep the conservative omitted-field default and require `allow_shell = true`
  to expose shell. Plan mode always hides shell; Full Access enables shell and
  auto-approval.
- `approval_policy` (string, optional): `on-request`, `untrusted`, or `never`. Runtime `approval_mode` editing in `/config` also accepts `on-request` and `untrusted` aliases.
- `[approval] default_selection` (string, optional): which option an approval
  card highlights when it first appears — `deny` (default) or `allow_once`.
  `deny` means a reflexive Enter on a card you have not read refuses the call.
  The highlighted row is tagged `(Enter)`; press `y` to allow once.
  Set `allow_once` to restore the pre-v0.9.6 Enter-to-approve muscle memory
  (#5293). It moves the highlight only: which calls are prompted for is still
  `approval_policy` plus the rules in `permissions.toml`.

  ```toml
  [approval]
  default_selection = "allow_once"
  ```
- `[approval] timeout_seconds` (integer, optional): bound how long an
  approval may wait — the TUI card and the Runtime API approvals the desktop
  app and web use alike. When the window elapses the call is refused
  (fail-closed) and recorded as a timeout, not as the operator's denial.
  Omitted or `0` waits indefinitely, which is the default everywhere: no
  approval is ever denied on your behalf unless you set this. Values above
  24h clamp with a warning (#6101).

  ```toml
  [approval]
  timeout_seconds = 300
  ```
- `sandbox_mode` (string, optional): `read-only`, `workspace-write`, `danger-full-access`, `external-sandbox`.
  Platform support is not identical. macOS uses Seatbelt when its runtime
  probe succeeds. Linux uses bubblewrap by default whenever `/usr/bin/bwrap`
  is installed and a probe proves it can confine a child; `prefer_bwrap =
  false` opts out and reports no OS command sandbox. Windows does not currently advertise an OS sandbox; its planned helper contract starts
  with process-tree containment only and must not be described as read-only
  filesystem isolation, workspace-write enforcement, network blocking,
  registry isolation, or AppContainer isolation until those are implemented.
- The cross-layer relationship between mode admission, hooks, registered tool
  requirements, typed rules, auto-review, repo law, human approval, and the
  execution sandbox is defined in
  [Authorization order](AUTHORIZATION_ORDER.md).
- **Read deny-list.** Every sandbox posture — `read-only` included — grants
  read access to the whole filesystem; the postures differ in what they may
  *write* and whether they may reach the network. The read deny-list narrows
  that:
  - `sandbox_read_denylist_defaults` (bool, default `true`): apply the built-in
    credential-store set — `~/.ssh`, `~/.gnupg`, cloud credential directories
    (`~/.aws`, `~/.config/gcloud`, `~/.azure`, `~/.kube`, …), `~/.netrc`,
    `~/.npmrc`, `~/.git-credentials`, macOS keychains, browser profiles,
    Codewhale's own secret stores, and `.env` files (but not `.env.example`
    and friends). Ordinary source, `Cargo.toml`, `~/.gitconfig`, `~/.cargo`,
    and `~/.npm` stay readable so builds and tests still work. Set `false` to
    restore the pre-0.9.12 full-disk-read behavior.
  - `sandbox_denied_read_paths` (list of paths): additional denied subpaths.
    `~` expands. These can never be exempted.
  - `sandbox_read_denylist_exempt` (list of paths): subtract a path from the
    *built-in defaults* when a project genuinely needs it. Deny wins over
    allow: this never reopens anything in `sandbox_denied_read_paths`.

    Exemption granularity is **whole-rule**, not per-file. An exempt path
    removes a built-in rule only when the rule's own path is at or below it,
    so exempting `~/.ssh/config` does nothing: the `~/.ssh` rule still denies
    it, because `~/.ssh` does not lie within `~/.ssh/config`. To reopen that
    one file you must exempt `~/.ssh` itself — which also reopens the private
    keys next to it. That is the documented tradeoff: there is no shipped way
    to narrow a built-in rule to "everything except one file"; copy what you
    need out of the denied tree instead. The one name-shaped rule, `.env`
    files, is exempted by *name* rather than by path: any exempt entry whose
    file name is exactly `.env` — bare `.env`, `~/.env`,
    `some/project/.env` — disables the entire `.env` filename rule, i.e.
    every `.env` and `.env.<name>` on disk rather than one project's.
    (`.env.example` and friends are never denied, so they need no exemption.)

  Enforced at two points: sandboxed shell commands (Seatbelt last-match-wins
  `deny file-read*` rules; bubblewrap masks each path) and Codewhale's own
  in-process tools, which the OS sandbox never wraps — `read_file` / `read` /
  `read_media` for contents, `list_dir` / `file_search` for *enumeration*
  (listing a denied directory, or searching one, is refused just as Seatbelt
  blocks its readdir; a name search rooted above a denied tree skips entries
  inside it). A refused read is always an explicit error, never an empty
  result, and the error names the path as the caller spelled it rather than a
  symlink target's real location.

  **This is defense-in-depth, not a security boundary.** It does not stop a
  hardlink to a denied file, a secret already copied into the workspace, an
  indirect read (`ssh-agent`, `security find-generic-password`, `aws sts …`),
  reads under `danger-full-access` shell commands, reads by MCP servers or
  other unwrapped child processes, or exfiltration of anything that *was*
  read. Keep least-privilege credentials and short-lived tokens doing the real
  work.
- `permissions.toml` (sibling file, optional): typed permission rule records
  loaded next to `config.toml`, for example `~/.codewhale/permissions.toml`.
  This active user file is the only permission-rule source today; project
  config overlays do not load a project-local `permissions.toml`. A rule's
  optional `workspace` field is its repository scope, not a second source.
  Manually authored `[[rules]]` entries accept `tool`, optional `command` or
  `path`, optional absolute `workspace`, optional `command_exact = true`, and
  optional `action = "deny" | "ask" | "allow"`; omitted `action` defaults to
  `"ask"`. `workspace` limits a rule to that repository, while
  `command_exact = true` changes a command rule from the historical
  arity-aware prefix match to a complete-command match. `deny` blocks matching
  invocations before mode-based
  approval handling, `allow` skips approval for matching invocations, and
  `ask` forces approval only in modes that can prompt. Outside the TUI
  auto-approve path, a matching `ask` rule under `approval_policy = "never"`
  is rejected because no prompt can be shown. In Full Access / auto-approval sessions,
  `ask` rules do not downgrade the session into prompting or blocking; explicit
  `deny` rules still block according to the current execution-policy logic.

  In a supported approval card, press `S` to allow the request once and append
  exact `action = "ask"` rules to this file. For eligible safe requests, choose
  **Always allow this exact rule in this repo** (shortcut `P`) to append an
  `action = "allow"` rule with the current absolute `workspace` scope.
  Remembered shell grants set `command_exact = true`, so later commands with
  extra arguments do not inherit the grant. File and patch grants retain the
  exact workspace-relative paths produced by the existing validation path.
  Supported saves are intentionally narrow:
  `exec_shell` stores the exact approved command string; `write_file` and
  `edit_file` store the exact workspace-relative file path; `apply_patch`
  stores one exact workspace-relative `path` rule per validated touched file
  from apply-patch preflight. Existing exec command matching remains
  arity-aware for manually authored prefix rules; approval-card allow grants
  use complete-command matching. File paths are normalized to the same
  workspace-relative form used by runtime matching.

  `read_file` rules can still be authored manually when you want future reads
  of a specific path to ask, allow, or deny, but the approval UI does not save
  `read_file` rules. Commands classified as requiring approval or dangerous,
  critical approval cards, and repo-law prompts cannot save allow grants and
  continue to require review.

  `/permissions` (or `/permissions list`) is the narrow rule-management
  surface. It lists each numbered rule with the active user-file source, its
  exact effective matcher (tool-wide, command prefix, exact command, or exact
  normalized path), global or repository scope, and whether that scope
  applies in the current workspace. `/config ask-rules` remains a compatibility
  entry to the same list.

  Deletion is review-gated: `/permissions remove <number>` only previews the
  selected rule and prints a confirmation command. That command carries an
  opaque token bound to the exact file bytes and rule index; if another writer
  changes `permissions.toml`, confirmation fails instead of deleting a rule
  that moved into the old position. Confirmed removal and approval-card appends
  share the adjacent `permissions.toml.lock`, preserve unrelated TOML comments
  and formatting, and atomically replace the file. The running TUI reloads the
  user ruleset without clearing session-only approvals.

  This editor intentionally does not create or rewrite rules, persist deny
  choices from approval cards, expand globs, or create broad
  directory/recursive rules. Author those supported exact/prefix records
  manually when needed.
- `[[hotbar]]` (array of tables, optional): user-owned 1-8 slot bindings for
  the TUI hotbar. Each entry has `slot`, `action`, and optional `label`.
  The Hotbar is hidden by default (#3807): omitting `hotbar` and setting
  `hotbar = []` both mean no hotbar. `/hotbar on` writes the eight default
  slots (`slash.workflow`, `slash.goal`, `slash.auto`, `mode.plan`,
  `mode.agent`, `mode.operate`, `palette.open`, `sidebar.toggle`) as explicit
  `[[hotbar]]` tables. When one or more `[[hotbar]]` tables are present, that
  list is the whole bar; missing slots stay empty. Invalid slots outside `1..=8` are skipped with a warning, duplicate
  slots use the later entry, and unknown action IDs are kept so the UI can show
  a disabled/unknown cell instead of silently deleting user config. Trusted
  user config, profiles, and managed config replace the whole list; project
  overlays cannot change hotbar bindings. Setup or wizard flows that persist
  hotbar bindings write this same schema to the resolved `~/.codewhale/config.toml`
  path, preserving legacy `~/.deepseek/config.toml` only when that fallback file
  is already the active config.

  ```toml
  [[hotbar]]
  slot = 1
  action = "mode.plan"
  label = "Plan"

  [[hotbar]]
  slot = 2
  action = "session.compact"
  ```
- `[auto_review]` (table, optional): tool-call review policy — a deterministic floor plus a model guardian tier.
  This layer sits on top of the existing permission posture; it can hold or block a
  tool call, but it is not an auto-push, auto-merge, or hosted review service.
  Block rules are checked first, then the built-in safety floor, then allow
  rules. In Ask, a safety hold opens approval; in Auto-Review, Full Access,
  or a non-interactive `never` posture it fails closed as a hard block. The
  safety floor still covers publish-like actions and destructive
  background/headless actions even if an allow rule matches.

  ```toml
  [[auto_review.allow]]
  id = "read-only-inspection"
  action_kind = "read"
  reason = "Read-only inspection is safe to run automatically."

  [[auto_review.block]]
  id = "no-release-publish"
  action_kind = "publish"
  reason = "Release and publish actions require maintainer review."
  ```

  Rule matchers are exact `tool` and/or `action_kind`. At least one matcher is
  required. `action_kind` accepts the six decision-relevant kinds `read`,
  `write`, `shell`, `external`, `publish`, and `destructive`. Invalid names
  fail config validation instead of silently broadening into another policy
  class. In block rules, the old names remain conservative compatibility
  aliases: `network`, `git`, `mcp_action`, `browser`, and `unknown` map to
  `external`; `secret` maps to `destructive`; and `mcp_read` maps to `read`.
  Retired narrow kinds in allow rules fail validation rather than widening to
  a broader class. The retired `text_contains` matcher likewise fails
  validation instead of silently broadening an old intent-dependent rule.
  Fallback holds in interactive Auto-Review escalate to one stateless guardian
  request. The request contains the exact held call and deterministic
  observations as separate JSON fields. Conversation history, skill
  instructions, attached file contents, and other expanded model context are
  excluded. The guardian does not infer user intent or compute an authorization
  score. It exposes no tools and returns a risk level, allow/deny, and a
  rationale. High or critical risk cannot auto-run even if the model says
  allow. An oversized exact call is denied rather than truncated. Exactly one
  reviewer request is made; incomplete or malformed output, timeout,
  cancellation, provider failure, or an empty rationale all fail closed. The
  deterministic floor is never model-reviewed, and headless adapters use the
  deterministic-only tier. The pinned Codex, Kimi, and DeepSeek source
  boundaries are linked from [Permission Posture](MODES.md#permission-posture).
  Reviewer outcomes emit `tool.auto_review` audit events with
  `gate = "guardian"`.

  Auto-review decisions emit `tool.auto_review` audit events with
  `gate = "deterministic"` when tool
  audit logging is enabled. Future PreToolUse/PostToolUse hooks can add
  observer input around this layer, but the configured auto-review policy is
  evaluated before a tool call is allowed to proceed.
- `managed_config_path` (string, optional): managed config file loaded after user/env config.
- `requirements_path` (string, optional): requirements file used to enforce allowed approval/sandbox values.
- `max_subagents` (int, optional): defaults to `64` and is clamped to `1..=128`.
- `subagents.*` (optional compatibility table): manual per-role model pins
  for direct and Workflow `agent` starts. An explicit saved profile wins,
  then a manual role pin, then a unique saved role pin. Conflicting tool
  `model` or `model_strength` choices are refused before admission. Unpinned
  roles allow task model/strength choices before inherited defaults.
  `[subagents.roles.<role>] model = "provider/model"` folds into the existing override
  map and wins over `[subagents.models]`, then the convenience keys. Structured
  canonical role keys win over legacy aliases. Only this structured syntax
  separates the explicit provider from the model suffix; unknown providers
  fail before admission. Bare structured model ids inherit the active provider.
  Legacy scalar/map values preserve namespaced provider-owned ids unchanged.
  Supported convenience keys are
  `default_model`, `worker_model`, `scout_model`, `planner_model`,
  `reviewer_model`, `custom_model`, `max_concurrent`, `max_admitted`,
  `launch_concurrency`, `token_budget`, `api_timeout_secs`, and
  `heartbeat_timeout_secs`. The v0.9.x keys `explorer_model`, `awaiter_model`,
  and `review_model` remain accepted as aliases. The `[subagents]
  max_concurrent` value overrides
  top-level `max_subagents` and is also clamped to `1..=128`. `[subagents]
  max_admitted` (aliases: `max_total`, `admission_limit`) is the bounded total
  of queued plus running sub-agents; it defaults to `1024`
  (`MAX_SUBAGENT_ADMISSION`, `crates/tui/src/config/subagent_limits.rs:21`,
  applied at `config.rs:6400`) so high-fanout turns can queue and drain while
  runtime launch pressure remains bounded, and is clamped to
  `max_concurrent..=1024`. `[subagents]
  launch_concurrency` sets how many direct children start at once before the
  rest queue for a launch slot; it defaults to the resolved `max_subagents` cap
  and is clamped to `1..=max_subagents` (the deprecated
  `interactive_max_launch` key is accepted as an alias, with the new key
  winning when both are set). `[subagents] token_budget` is an optional
  aggregate token ceiling for each root `agent` run and its descendants; unset
  or `0` preserves unlimited legacy behavior. `[subagents] api_timeout_secs`
  controls the per-step API timeout for sub-agent model calls and is clamped to
  `1..=3600`, with `0` or unset preserving the 600 second default; a timed-out
  attempt is retried with exponential backoff (up to 5 retries) before the
  step interrupts with a preserved checkpoint.
  `[subagents] heartbeat_timeout_secs` controls stale running agent cleanup,
  defaults to `300`, and is clamped to `30..=3600` while staying above the
  resolved API timeout. `[subagents.providers.<provider>]` accepts the same
  fanout, depth, budget, and timeout knobs (`enabled`, `max_concurrent`,
  `max_admitted`, `launch_concurrency`, `max_depth`, `token_budget`,
  `api_timeout_secs`, `heartbeat_timeout_secs`) and inherits the global
  `[subagents]` value for any key you omit. Provider keys accept canonical
  names such as `deepseek`, `zai`, `openrouter`, `anthropic`, plus convenience
  aliases such as `glm` for Z.ai and `deepseek_api` for direct DeepSeek:

  ```toml
  [subagents]
  max_concurrent = 20
  launch_concurrency = 20
  max_admitted = 200
  max_depth = 6

  [subagents.providers.deepseek]
  max_concurrent = 20
  launch_concurrency = 20
  max_admitted = 200

  [subagents.providers.glm]
  max_concurrent = 4
  launch_concurrency = 3
  max_admitted = 12
  max_depth = 2

  [subagents.providers.openrouter]
  max_concurrent = 5
  launch_concurrency = 3
  max_admitted = 20
  ```

  `/config subagents status` prints both global values and the active
  provider's resolved profile so rate-limit tuning is visible in the TUI.
  `[subagents.models]` accepts lower-case Fleet role keys such as `worker`,
  `scout`, `planner`, `reviewer`, `builder`, and `verifier`; legacy type keys
  remain accepted during v0.9.x. Values are validated
  against the active provider at spawn time; direct DeepSeek requires DeepSeek
  IDs, while OpenAI-compatible/custom provider routes pass explicit model IDs
  through to that provider. To route a child to a different provider than the
  parent session, save a Fleet/AgentProfile with explicit `provider` and
  `model` fields (including user-named custom providers such as `lm-studio`)
  and call `agent(profile: "...")`; see [SUBAGENTS.md](SUBAGENTS.md).
- `skills_dir` (string, optional): defaults to `~/.codewhale/skills` (each skill is
  a directory containing `SKILL.md`). Workspace-local `.agents/skills` or
  `./skills` are preferred when present; the runtime also discovers global
  agentskills.io-compatible `~/.agents/skills` and the broader Claude-ecosystem
  `~/.claude/skills`. First launch installs versioned bundled skills for common
  workflows including skill creation, delegation, MCP/plugin scaffolding,
  documents, presentations, spreadsheets, PDFs, and Feishu/Lark. Only
  Codewhale-owned roots (`<workspace>/.codewhale/skills` and
  `~/.codewhale/skills`) are writable install/import targets; compatible harness
  roots stay read-only. Bare `/skills` opens the Skills Manager (owned-only,
  zero network). See [SKILLS.md](SKILLS.md) for the manager, audit statuses,
  provenance markers, and mutation rules, and
  [CLAUDE_PLUGIN_COMPAT.md](CLAUDE_PLUGIN_COMPAT.md) for the supported boundary
  between portable `SKILL.md` bundles and Claude Code plugin runtimes.
- `[skills].scan_codewhale_only` (bool, default `false`): when `true`, session
  skill discovery ignores cross-tool roots such as `.claude/skills`,
  `.opencode/skills`, `.cursor/skills`, and `~/.agents/skills`. Codewhale still
  scans `<workspace>/.codewhale/skills`, `~/.codewhale/skills`, and any explicit
  `skills_dir` override. The Skills Manager can still toggle a local compatible
  audit scan independently of this runtime knob — see [SKILLS.md](SKILLS.md).
- `[skills].flat_workspace_root` (bool, default `false`): opt in to the flat
  `<workspace>/skills` compatibility root after workspace trust. Without this
  opt-in it is an audit candidate only; an explicit `skills_dir` remains an
  alternative. `scan_codewhale_only = true` excludes the flat compatibility
  root regardless of this flag, unless it is the explicit `skills_dir`.
- `[skills].registry_url` / `[skills].max_install_size_bytes` (optional): used by
  `/skills --remote`, `/skills suggest <task>`, `/skills sync`, and `/skill
  install|update`. The default manager open path does not contact the registry.
- `[verifier].enabled` (bool, default `false`): enables automatic
  claim-of-done verifier preview once that runtime trigger is active. The
  manual `run_verifiers` tool is still available when this is false.
- `mcp_config_path` (string, optional): defaults to `~/.codewhale/mcp.json`, with
  legacy `~/.deepseek/mcp.json` fallback when the Codewhale path is absent.
  Custom paths must be absolute; a relative value falls back to the user-global
  path so changing the launch directory cannot silently change the MCP pool.
  It is visible in `/config` and can be changed from the TUI. The new path is
  used immediately by `/mcp`, but rebuilding the model-visible MCP tool pool
  requires restarting the TUI.
- `notes_path` (string, optional): defaults to `~/.codewhale/notes.txt`, with
  legacy `~/.deepseek/notes.txt` fallback when the Codewhale path is absent, and
  is used by the model-visible `note` tool.
- `[memory].enabled` (bool, optional): defaults to `false`. When `true`,
  the TUI loads the user memory file into a `<user_memory>` prompt block,
  enables `# foo` quick-capture in the composer, surfaces the `/memory`
  slash command, and registers the `remember` tool. The same toggle is
  available via `DEEPSEEK_MEMORY=on`.
- `memory_path` (string, optional): anchors the native memory store. The
  configured filename is **not** the file that is written. Under the Native
  backend (the only backend) the store is re-rooted to
  `<parent-of-memory_path>/memory/global/MEMORY.md` — so the default
  `~/.codewhale/memory.md` yields `~/.codewhale/memory/global/MEMORY.md`
  (plus workspace-scoped files and a rebuildable SQLite FTS5 index). See
  [`MEMORY.md`](MEMORY.md) for the full feature surface (`# foo` composer
  prefix, `/memory` slash command, `remember` tool, opt-in toggle).
- `snapshots.*` (optional): side-git workspace snapshots for file rollback:
  - `[snapshots].enabled` (bool, default `true`)
  - `[snapshots].max_age_days` (int, default `7`)
  - snapshots live under
    `~/.codewhale/snapshots/<project_hash>/<worktree_hash>/.git`, with legacy
    `~/.deepseek/snapshots/...` fallback when only the legacy state exists, and
    never use the workspace's own `.git` directory
- `context.*` (optional):
  - `[context].project_pack` (bool, default `false`): include a deterministic
    project context pack (a large pretty-printed directory listing) in the
    stable prompt prefix (#4781). Useful for weak tool-calling models; the
    model can rebuild the same information with one `File` call.
  - The removed seam-manager keys (`enabled`, `verbatim_window_turns`,
    `l1_threshold`, `l2_threshold`, `l3_threshold`, `seam_model`) are
    ignored if an older config still carries them; they no longer load.
- `compaction.*` (optional, config.toml): how a compaction pass behaves once
  it fires. `auto_compact` / `auto_compact_threshold_percent` (settings.toml)
  still decide *when* it fires. Both keys are absent by default, and absent
  means the built-in behaviour, unchanged:
  - `[compaction].summary_instructions` (string, default empty): standing
    operator instructions appended to the summarizer prompt as a clearly
    delimited "Additional instructions from the operator" section on **every**
    pass, manual and automatic — the effort-free counterpart to a one-off
    `/compact <focus>`, which still composes after this text. Useful for
    "always list exact file paths and line numbers", "always restate open
    decisions and their trade-offs", "always write a TL;DR first". Truncated
    at 4 000 characters with a warning naming the key; whitespace-only reads
    as unset. The summarizer still runs with no system prompt and no tools —
    this suffix is the only operator-authored input it sees.
  - `[compaction].retained_user_message_tokens` (int, default `20000`, clamped
    to `2000`–`200000`; also accepted as `retained_user_message_max_tokens`):
    token budget for the recent plain user messages kept **verbatim** in the
    replacement history. Raising it keeps more of the user's own earlier
    messages instead of only whatever the lossy summary captured; the
    last-round survival contract still applies on top. The `/compact` receipt
    names the effective budget and whether operator instructions were applied,
    so you can tell the knob took effect.
- `retry.*` (optional): retry/backoff settings for API requests:
  - `[retry].enabled` (bool, default `true`)
  - `[retry].max_retries` (int, default `3`)
  - `[retry].initial_delay` (float seconds, default `1.0`)
  - `[retry].max_delay` (float seconds, default `60.0`)
  - `[retry].exponential_base` (float, default `2.0`)
  - `[retry].jitter` (bool, default `true`): randomize each backoff delay
  - `[retry].jitter_factor` (float, default `0.1` = ±10%; clamps to `0.0..=1.0`)
  - `[retry].respect_retry_after` (bool, default `true`): wait for a server
    `Retry-After` header instead of the computed backoff

  `[retry]` schedules HTTP-request retries inside the client. The stream-level
  budgets that sit above it — how often a turn re-issues a request whose
  stream failed to open or died — are the `[stream]` keys below; legacy `tui.stream_max_*` keys remain fallbacks.
- `[notifications]`: notification delivery, attention, categories and audio share one
  policy. `quiet = true`, `method = "off"`, `condition = "never"` and disabled
  categories suppress both the banner and Codewhale's selected sound.
- `notifications.method`: `auto` (default), `osc9`, `kitty`, `ghostty`, `bel`, `off`.
- `notifications.condition`: `unfocused` (default), `always`, `never`. When absent,
  the legacy `tui.notification_condition` remains the fallback. `always` also
  bypasses the duration threshold; `unfocused` requires two seconds away.
- `notifications.threshold_secs`: nonnegative integer, default `30`.
- `notifications.include_summary`: boolean, default `false`.
- `notifications.sound`: optional `off`, `whale`, `bell`, `beep`, `file`.
  A selected value controls audio across enabled categories. Absent keeps legacy
  `completion_sound` and `event_sound` choices; `off` overrides both.
- `notifications.sound_file`: custom local WAV path for `sound = "file"` or legacy
  `completion_sound = "file"`.
- `notifications.subagent_completion`: `always`, `final-only` (default), `off`.
  Covers all background work that finishes: sub-agents, background shells and
  durable tasks. `final-only` sends one notice naming everything that finished
  once no agent, workflow or durable task is still running; a running shell
  (a dev server, a watcher) never holds it back. `always` sends one per item.
- `notifications.quiet`: boolean, default `false`.
- `notifications.events`: six boolean categories, all enabled by default; see below.
- `notifications.completion_sound`: legacy completion cue, default `off`, with the
  same values as `sound`. Used only when `sound` is absent.
- `notifications.event_sound`: legacy `enabled` (default `false`), `events`
  (default `["turn-complete", "approval-needed"]`), and `quiet` (default `false`).
  `min_interval_ms` (default `2000`) applies to each category's audio in both modes.
- `tui.alternate_screen` (string, optional, default `auto`): which screen an interactive session starts on. `auto` and `always` start on the TUI-owned alternate screen; `never` starts in inline mode — a ratatui viewport the full height of the terminal with no alternate screen, so the shell's scrollback survives the session and stays scrollable after exit. `/fullscreen` and `/inline` switch it in-process; a switch that the terminal refuses rolls back and says why. Inline mode paints the whole transcript inside its viewport — nothing is written into the host scrollback while the session runs.
- `tui.mouse_capture` (bool, optional, default `true` on non-Windows terminals and on Windows Terminal/ConEmu/Cmder when the alternate screen is active; `false` on legacy Windows console and inside JetBrains JediTerm — PyCharm/IDEA/CLion/etc. — where mouse-event escapes leak into the input stream as garbled text, see #878 / #898): enable internal mouse scrolling, transcript selection, right-click context actions, and transcript scrollbar dragging. TUI-owned drag selection copies the intersected cells, removes visual wrap-column line breaks from paragraphs, and keeps selection scoped to the transcript pane; the payload is Markdown source by default, see `tui.selection_copy_markdown` below. Set this to `false` or run with `--no-mouse-capture` for raw terminal selection; set it to `true` or run with `--mouse-capture` to opt in anywhere it's defaulted off. On raw terminal selection, especially on legacy Windows console or when mouse capture is disabled, selection may cross the right workbar and include visual wraps because the terminal, not the TUI, owns the selection.
  On Linux, finishing a transcript or composer selection quietly copies text to
  PRIMARY, leaving the regular clipboard unchanged. Middle-click inside the
  composer pastes PRIMARY at the pointer without submitting it. This uses native
  X11 or Wayland data control; compositors must support PRIMARY selection. Over
  SSH without a forwarded graphical display, use your terminal's selection/paste
  gestures or `--no-mouse-capture`. Explicit Copy still uses the regular clipboard.

- `tui.selection_copy_markdown` (bool, optional, default `true`): copy TUI-owned
  transcript selections (drag release, context-menu Copy, and `Cmd+C`/`Ctrl+C`
  on an active selection) as Markdown source instead of rendered text. Every
  intersected cell serializes through the same canonical projection `Ctrl-Y`
  and `/copy` use — user and assistant cells keep their authored Markdown,
  other cells keep their full transcript form — partial intersections round out
  to whole cells, cells join with blank lines, and a toast names the copied
  cell count. Set `false` to copy the rendered text as displayed. Composer
  selections and the Linux PRIMARY auto-copy are unchanged; PRIMARY always
  carries rendered text.

- `tui.stream_chunk_timeout_secs` (int, optional, default `900`): per-SSE-chunk idle timeout for streamed model responses. Slow local or compatible servers can raise this with `/config stream_chunk_timeout_secs <seconds>` (add `--save` to write canonical `stream.chunk_timeout_secs`); `0` maps to the default and explicit values must be `1..=3600`. The legacy `DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS` env var is still honored when this key is omitted.
- `tui.osc8_links` (bool, optional, default on for macOS/Linux, off for Windows): emit OSC 8 escape sequences around URLs in transcript output so supporting terminals (iTerm2, Terminal.app 13+, Ghostty, Kitty, WezTerm, Alacritty, recent gnome-terminal/konsole) can open them with the terminal's link gesture—usually Cmd-click on macOS and Ctrl-click on Linux/Windows. Terminals without OSC 8 support render the plain label and ignore the escape. The escapes are emitted out-of-band (not inside buffer cells), so column corruption is not a concern; set `false` only for terminals that misrender the OSC 8 terminator itself. Windows legacy consoles default off; opt in with `true`.
- `tui.max_model_steps` (int, optional, default uncapped): optional model-step ceiling for one ordinary turn. Omission or `0` leaves model steps uncapped; explicit positive values are clamped to `1..=100000`. Headless `exec` and Fleet workers also have no implicit model-step ceiling; `exec --max-turns N` and positive worker budgets still apply. At ~80% of an explicit step budget the model gets one soft-landing notice; at exhaustion the turn ends `Failed` with `Maximum model steps reached before completion (limit: N)` after one bounded final-report response when needed. Cumulative wall-clock and per-stream limits remain independent. Active interactive goal turns use `goal.max_steps` instead (also uncapped by default); see the Goal loop section below.
- `tui.turn_wall_clock_secs` (int, optional, default: no limit): cumulative per-turn wall-clock budget in seconds, measured across every model step of one turn (not per request). Time blocked on a human approval is excluded. Omitted or `0` means no limit; positive values clamp to `30..=86400` (24 hours is the ceiling). When exhausted the turn stops before authorizing another billable request with a message naming the limit and the key to raise.
- `tui.stream_max_resumes` (int, optional, default `3`): how many times one turn re-issues a model request after its stream failed — the request never opened (connect failure or response-header stall), the stream died before any content, the host slept mid-stream, or the network dropped mid-stream. Every one of those paths spends this one budget, and a healthy stream resets it. `0` disables turn-level re-issues (a failed stream then fails the turn); values clamp to `0..=10`.
- `tui.stream_max_transparent_retries` (int, optional, default `2`): in-stream re-requests while nothing has streamed yet. `0` disables them; values clamp to `0..=10`.
- `tui.stream_max_errors` (int, optional, default `5`): recoverable errors tolerated within one stream before it ends. Unlike the two retry counts above, `0` does not switch anything off: like the other finite stream budgets it selects the default. Other values clamp to `1..=50`, so `1` ends the stream on its first recoverable error.
- `tui.stream_open_timeout_secs` (int, optional, default `45`): wait for a streaming request's response headers (connection setup included). A header stall on HTTP/2 retries once over HTTP/1.1 with the same wait. Omitted or `0` falls back to `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`, then the default; values clamp to `5..=300`.
- `tui.connect_timeout_secs` (int, optional, default `30`): TCP/TLS connect timeout for the model HTTP client. Omitted or `0` uses the default; values clamp to `1..=300`.
- `tui.force_http1` (bool, optional, default `false`): pin the model HTTP client to HTTP/1.1, for provider edges or proxies that mishandle long-lived HTTP/2 streams. `CODEWHALE_FORCE_HTTP1=1` does the same; either one pins.
- `tui.stream_max_content_mb` (int, optional, default `10`) and `tui.stream_max_duration_secs` (int, optional, default `1800`): per-step caps on one stream's accumulated content and wall-clock duration. `0` selects the default; values clamp to `1..=512` MB and `10..=86400` seconds.
- `transcript.prose_measure` (positive integer, optional, default absent = full width): wrap cap, in columns, for prose cells — user messages, assistant answers, and reasoning/thinking blocks — in the live transcript (#5436). Absent (or `0`) spends the full content width, consistent with tool/status cells and the #5322 wide-frame decision; the former 105-column prose rail is gone. Set a positive whole number (e.g. `prose_measure = 120` under `[transcript]`) to restore a bounded reading measure on ultrawide terminals. Narrow terminals always keep their content width — the cap clamps from above only. Tool, diff, and status cells never inherit this cap. Invalid values (negative or non-integer) are rejected at startup with a `transcript.prose_measure` config error. Resolved once per render pass, so the main transcript cache and the full-screen overlay always agree on the effective width.
- `hooks` (optional): lifecycle hooks configuration (see `config.example.toml`).
- `features.*` (optional): feature flag overrides (see below).

### Stream and transport settings

`[stream]` is the canonical table for model-stream policy and its HTTP clients.
`codewhale config dump` and `codewhale config get stream` show effective values
from the runtime resolver, including environment fallbacks and clamps; this does
not persist defaults. Set or unset individual fields with, for example,
`codewhale config set stream.open_timeout_secs 120` and
`codewhale config unset stream.open_timeout_secs`. Per-run
`--set stream.open_timeout_secs=120` uses the same validation.

| `[stream]` key | Default | Accepted behavior | Legacy `[tui]` fallback |
| --- | --- | --- | --- |
| `open_timeout_secs` | 45 | Positive values clamp to 5–300; 0 or omission falls through to the environment/default | `stream_open_timeout_secs` |
| `chunk_timeout_secs` | 900 | 0 selects default; positive values clamp to 1–3600 | `stream_chunk_timeout_secs` |
| `max_resumes` | 3 | 0 disables whole-request reissues; maximum 10 | `stream_max_resumes` |
| `max_transparent_retries` | 2 | 0 disables retries before any content; maximum 10 | `stream_max_transparent_retries` |
| `max_stream_errors` | 5 | 0 selects default; positive values clamp to 1–50 | `stream_max_errors` |
| `max_duration_secs` | 1800 | Per-stream wall clock; 0 selects default, positive values clamp to 10–86400 | `stream_max_duration_secs` |
| `max_content_mb` | 10 | Per-stream content; 0 selects default, positive values clamp to 1–512 MiB | `stream_max_content_mb` |
| `connect_timeout_secs` | 30 | TCP/TLS setup; 0 selects default, positive values clamp to 1–300 | `connect_timeout_secs` |
| `force_http1` | false | Boolean; a truthy environment pin always enables HTTP/1.1 | `force_http1` |
| `tcp_keepalive_secs` | 30 | Idle time before TCP keepalive probes; 0 disables, positive values clamp to 1–3600 | none |
| `http2_keep_alive_interval_secs` | 15 | PING interval on active HTTP/2 connections; 0 disables, positive values clamp to 1–3600 | none |
| `http2_keep_alive_timeout_secs` | 20 | PING acknowledgement deadline; 0 selects default, positive values clamp to 1–3600 | none |

For each field, an explicit canonical value wins over the legacy `[tui]` field,
including zero or false; an absent canonical field preserves the legacy value.
The existing environment precedence is unchanged: a positive configured header
wait wins over `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS` (then its `DEEPSEEK_` alias);
zero falls through to those variables. An omitted chunk timeout uses
`CODEWHALE_STREAM_IDLE_TIMEOUT_SECS` (then its `DEEPSEEK_` alias), whereas an
explicit zero uses 900. `CODEWHALE_FORCE_HTTP1` (legacy `DEEPSEEK_FORCE_HTTP1`)
is ORed with the selected config flag, so a false config flag cannot defeat a
truthy environment pin. Profile overlays merge canonical fields independently.

HTTP transport values apply to all newly constructed model, catalog and HTTP/1
fallback clients. They do not rebuild an active client, govern MCP/other network
services, or send HTTP/2 PINGs on idle pooled connections. HTTP/2 knobs have no
effect when HTTP/1.1 is pinned. The operating system controls TCP probe details;
these settings do not disable certificate validation. `[retry]` still owns the
HTTP-request backoff schedule independently of stream-level budgets.

### Workspace notes

`/note` manages a simple notes file in the current workspace at
`.codewhale/notes.md` (legacy `.deepseek/notes.md` is the fallback path when
no `.codewhale/notes.md` exists yet).
Existing `/note <text>` usage still appends a note.
The management forms are:

| Command | Action |
|---|---|
| `/note <text>` | Append a note (legacy shorthand) |
| `/note add <text>` | Append a note explicitly |
| `/note list` | List notes with temporary 1-based numbers |
| `/note show <n>` | Show the full note at number `n` |
| `/note edit <n> <text>` | Replace note `n` with new text |
| `/note remove <n>` | Delete note `n`; `rm` and `delete` are aliases |
| `/note clear` | Empty the workspace notes file |
| `/note path` | Show the resolved workspace notes path |

The numbers shown by `/note list` are not stored in the file; they are derived
from the current order each time notes are read. This keeps the file format
compatible with the existing `---`-separated notes.

### User memory

User memory is split across one top-level path setting and one opt-in
toggle table:

```toml
# Anchors the store only — actual writes go to
# ~/.codewhale/memory/global/MEMORY.md (see MEMORY.md).
memory_path = "~/.codewhale/memory.md"

[memory]
enabled = true
```

Notes:

- `memory_path` stays at the top level beside `notes_path` and
  `skills_dir`; it is not nested under `[memory]`.
- The configured path is an **anchor**: its parent directory gains
  `memory/global/MEMORY.md`, workspace-scoped files, and `index.db`.
  Pointing `memory_path` at the native layout path itself would double-nest
  (`…/memory/global/memory/global/MEMORY.md`); keep the legacy-style
  anchor filename.
- `DEEPSEEK_MEMORY_PATH` overrides the anchor path from the environment.
- `DEEPSEEK_MEMORY=on` (also `1`, `true`, `yes`, `y`, or `enabled`)
  flips the feature on without editing `config.toml`.
- The feature is inert when disabled: no file is injected, `# foo`
  falls through to normal message submission, and the model does not
  see the `remember` tool.
- See [`MEMORY.md`](MEMORY.md) for examples and the full `/memory`
  command surface.

### Goal loop (`[goal]`)

Operate-mode goals run to their completion gate with no default token, time, or
continuation ceiling (#5052). Token/time budgets, when explicitly supplied,
are telemetry only and do not stop a goal. Users who want a circuit breaker can
opt into one:

```toml
[goal]
# Optional safety backstop on automatic goal continuation passes.
# Default: 0 (unlimited). Set a positive value to opt into a ceiling.
max_continuations = 100

# Optional cancellable quiet period between successful turns. This is useful
# for coordinator goals that should poll on a cadence instead of keeping one
# provider turn open. Default: 0 (continue immediately).
continuation_delay_seconds = 300

# Optional per-turn model-step ceiling while a goal is active (#6512).
# Default: uncapped (0 or absent). Positive values clamp to 1..=100,000.
# This bounds each provider turn, never the number of continuation passes.
max_steps = 0
```

The effective delay is capped at 86,400 seconds (24 hours); use an automation
for schedules that are less frequent than once per day.

When an explicit backstop fires, the goal pauses with a status message naming
`[goal] max_continuations` and a warning is logged; resume the goal after
inspecting progress, or raise/disable the backstop.

`[goal] max_steps` governs one engine turn at a time. Like ordinary interactive
turns, omitted or `0` leaves model steps uncapped. Explicit per-invocation
ceilings — `exec --max-turns N`, child-worker caps — always win over it. At
about 80% of an explicit positive budget the model is told to land; at exhaustion
it gets one bounded final report and the turn classifies as budget-exhausted. An
unfinished goal then pauses with the BudgetLimit reason instead of re-arming
another goal turn — resume it explicitly after reviewing the report. Wall-clock
and stream protections are separate and still apply.

The delay starts only after a successful turn while an explicitly created goal
is still active. `/goal pause`, `/goal done`, `/goal blocked`, `/goal clear`,
Esc, or Ctrl+C cancels a pending continuation before another provider request
starts. Failed turns and policy/route failures never schedule another turn.
Only the numeric cadence is stored in config; no prompt, credential, or secret
is persisted for the loop.

### Reasoning-only recovery (`[reasoning_only]`)

When a reasoning model (thinking mode) finishes a response with only hidden
reasoning and no answer text or tool call, the engine can automatically
re-request the answer. Configure this behavior with the `[reasoning_only]`
table:

```toml
[reasoning_only]
# Maximum number of automatic re-requests. Default: 2.
# Set to 0 to disable automatic recovery (fail immediately).
max_reprompts = 10

# Optional custom message sent to the model on each re-request.
# When set, overrides the built-in default message.
# When unset (or commented out), the engine uses:
#   "So, what's up ? Keep running !"
reprompt_message = "Allez, répond quelque chose !"
```

This only applies when the model returns a clean `stop` finish reason with
only thinking content. An output-length stop (`length`/`max_tokens`) is never
retried, and a persistently answerless model still fails honestly after the
configured bound.

To disable the reprompt message entirely (silent retry), set it to an empty
string:

```toml
[reasoning_only]
reprompt_message = ""
```

### Notifications

Notification controls are available in the existing `/config` settings view,
from terminal commands, and through the CLI. All write the same `config.toml`
keys. Terminal changes apply immediately; add `--save` to keep them. CLI writes
apply when the next process loads its configuration.

```sh
codewhale config set notifications.sound whale
codewhale config set notifications.events.approval-needed false
codewhale config set notifications.quiet true
codewhale config get notifications
codewhale config unset notifications.quiet
```

```text
/config notifications sound whale --save
/config notifications condition unfocused --save
/config notifications quiet true
/config notifications status
```

Nested CLI edits preserve TOML types, unrelated keys and comments. Unset removes
only the selected leaf. `notifications.sound legacy` in the TUI, or CLI unset of
`notifications.sound`, restores previous sound choices. Invalid values are
rejected before file or session changes. In an active TUI profile, saved edits
update its notification table when it owns one; otherwise they update the
inherited root table. The settings detail keeps saved and current values distinct.

```toml
[notifications]
method = "auto"        # auto | osc9 | kitty | ghostty | bel | off
condition = "unfocused" # unfocused | always | never
threshold_secs = 30
include_summary = false
sound = "whale"        # optional; sound is opt-in, not enabled by default
quiet = false

[notifications.events]
turn-complete = true
subagent-terminal = true
approval-needed = true
input-needed = true
elevation-needed = true
model-notify = true
```

`quiet = true` mutes every category without changing saved choices. `method =
"off"` also stops both banner and selected audio. Disabling a category stops its
sound. Attention and duration gates apply before any sink runs. Title animation
completion is silent; only the authorized notification event can request audio.
Successful turn completion is a notification category; failed/cancelled turns do
not create a success notification.

`auto` chooses a recognized terminal protocol or the existing macOS native
fallback; unknown terminals remain unsupported and never invent a bell. `bel`
is an audio-only transport: one selected cue is dispatched, without a second
transport bell. With `sound = "off"`, that transport is silent. `osc9`, `kitty`
and `ghostty` use their terminal notification protocols; tmux passthrough is
preserved. Terminal/OS notification preferences still govern display, attribution
and any sound the host itself adds.

By default the terminal must stay unfocused for two seconds. `condition =
"always"` allows foreground notifications and bypasses the duration threshold;
`"never"` suppresses all delivery. The canonical condition takes precedence over
legacy `[tui].notification_condition`.

The bundled `whale` is a 1.55-second original whale-inspired cue, with no
third-party recording. It remains an opt-in candidate pending listening approval.
WAV playback uses a background worker: macOS `/usr/bin/afplay`, Linux `aplay`
from [ALSA utilities](https://github.com/alsa-project/alsa-utils), or Windows
`PlaySoundW`. A missing player/file or unsupported platform has no fallback bell.
Only one WAV plays at a time. A worker-start receipt is a dispatch attempt, not
proof of audible playback or OS acceptance. The existing macOS `osascript`
banner retains Script Editor attribution; this Core change does not provide a
branded native Apps banner.

#### Previous sound settings

When `notifications.sound` is absent, completion uses a non-off
`completion_sound` selection, and other events use the existing event allow-list.
These are compatibility inputs to the same audio decision, not separate playback
paths. When the global sound is selected, it takes precedence over the previous
completion/event choices. The default remains silent unless a legacy sound or
explicit `bel` transport was already selected.

```toml
[notifications.event_sound]
enabled = false
events = ["turn-complete", "approval-needed"]
min_interval_ms = 2000
quiet = false
```

Legacy event cues use one bell for completion, subagent completion, input and
model notices; approval/elevation cues use two. The per-category repeat interval
survives settings refreshes. Old unknown event names are ignored on load; new
CLI/TUI edits require names from the six categories above.

Local approval/input/elevation prompts and error receipts remain available when
external notifications are muted. Action prompts use the selected UI language
and retire when that request settles. Repeated live notices keep their first
expiry; a later routine update does not hide an unresolved warning at completion.
Optional plugin suggestion toasts and contextual tips share one session guidance
budget. `/config contextual_tips off` hides those toasts while preserving
required notices and explicit plugin review requests.

#### What a notification can contain

A desktop notification is a glance surface: on macOS it can appear on the
lock screen, and on every platform it is visible to anyone near the machine.
Codewhale therefore builds notifications from a typed payload with a fixed
per-event disclosure policy rather than from whatever text was on hand:

| Event | Shown | Never shown |
|---|---|---|
| Turn complete | status line (+ elapsed/cost when `include_summary`), preview of the assistant's reply | — |
| Sub-agent finished | status line, agent id, preview of the child's summary line | — |
| Approval needed | the tool name | the tool description, the command, the arguments |
| Input needed | "Answer the question in the terminal to continue" | the question |
| Sandbox elevation needed | the tool name and the denial reason | the command |
| `notify` tool | model-supplied title and body | — |

Every field is capped (80 characters for the status line, 120 for the
identifier, 200 for the preview), stripped of control bytes and escape
sequences, and passed through a redactor that replaces credential-shaped
strings with `[redacted]`, reduces absolute local paths to `…/basename`,
and replaces raw tool JSON with `[details hidden]`. The redactor is
deliberately over-eager: an unbroken 40-character run has no word
structure, so it is redacted even when it is not a secret.

#### macOS: why the banner says "Script Editor"

On macOS terminals that provide no notification escape of their own —
Apple Terminal, the VS Code and JetBrains embedded terminals, plain tmux
without `LC_TERMINAL` — `method = "auto"` falls back to `osascript`'s
`display notification`. That command posts on behalf of the *bundled*
host process, and `/usr/bin/osascript` is unbundled, so macOS attributes
the banner to `com.apple.ScriptEditor2`. That attribution supplies the
Script Editor icon and owns the System Settings → Notifications entry
(alert style, previews, Do Not Disturb). `display notification` has no
icon parameter, so this cannot be fixed from the notification code; it
needs Codewhale to ship a real `.app` bundle. Tracked in
[#4834](https://github.com/codewhale-hq/CodeWhale/issues/4834). In the meantime,
iTerm2, WezTerm, Ghostty, and kitty are matched first and use their own
notification protocols, and `method = "osc9"` / `"bel"` / `"off"` opt out
of the `osascript` path explicitly.

## Automations in the terminal

Open `/automation`, then choose **New automation** (`n`) or **Edit automation**
(`e`). The form edits the name, multiline prompt, schedule, model, workspace,
and enabled status. `Tab` moves between fields; `Enter` inserts a newline in
the prompt. Schedule presets include daily, weekly, hourly, once, and a custom
RRULE. Time fields accept `HH:MM`; their arrow controls change the time by
15 minutes. Weekly day buttons and model search support mouse and keyboard.

The next-run preview uses the scheduler's local time zone, shown with its UTC
offset. An enabled automation can run after **Save** (`Ctrl+S`); a paused one
has no scheduled next run. **Cancel** (`Esc`) discards the draft. Editing keeps
existing permission settings, custom schedules, and additional workspace
entries unless the corresponding supported field is explicitly changed.

Schedules are evaluated in the machine's local time zone against the wall
clock. A wall time that does not exist on a spring-forward day is skipped and
an ambiguous fall-back time fires once. Occurrences missed while Codewhale was
closed, asleep, or still running the previous occurrence are coalesced: the
next start runs one catch-up occurrence and then continues from the next
future slot, never replaying every missed slot. An occurrence never starts
while an earlier run of the same automation is still queued or running. Each
run is recorded durably with its status, timing and error; a run that needs a
tool approval has no operator to ask and fails once the approval wait expires.

Choosing a concrete model pins both the model and its exact configured
provider, including named custom routes. Later changes to the active provider
do not move that automation's pin. The default-model choice and legacy
definitions without a provider pin keep the runtime's existing default
behavior. New automation records use schema v2 and tasks use v3 so older
runtimes reject records whose provider pins they cannot preserve.

## Lifecycle Outbox (`[lifecycle_outbox]`)

The lifecycle outbox is an opt-in, machine-readable stream of session,
turn, and sub-agent lifecycle events. With a path configured, Codewhale
appends one JSON line per event to that file — for interactive TUI
sessions *and* headless `codewhale exec` runs — so a supervisor
(terminal multiplexer wrapper, automation harness, alerting setup) can
react to what happened without scraping the screen or installing per-hook
shell commands. Unset or empty `path` = the feature is **off** and
behavior is unchanged.

```toml
[lifecycle_outbox]
path = "~/.codewhale/notifications/outbox.jsonl" # unset/empty = OFF
webhook_url = ""     # optional; POSTs events as JSON when set
webhook_token = ""   # optional bearer token for webhook_url
```

### Events emitted

| Event | Kind | Fired at |
|---|---|---|
| `turn_start` | `turn.started` | a new turn begins (TUI TurnStarted; `exec` at message dispatch) |
| `turn_end` | `turn.completed` / `turn.failed` / `turn.interrupted` | turn completion, kind projected from the turn status |
| `turn_stalled` | `turn.stalled` | the stall watchdog recovers a wedged turn |
| `subagent_spawn` | `subagent.spawned` | a sub-agent is spawned |
| `subagent_complete` | `subagent.completed` | a sub-agent reaches a terminal state |
| `session_start` | `session.started` | interactive session start |
| `session_end` | `session.ended` | interactive session end |

### File contract

Each line is a `RuntimeEventEnvelope`:

```json
{"schema_version": 1, "seq": 3, "event": "turn_start", "kind": "turn.started",
 "thread_id": "…", "turn_id": "…", "item_id": null, "timestamp": "…",
 "created_at": "…", "payload": {…}}
```

- `seq` is monotonic per outbox file and recovers from the last written
  line when a new process opens the file.
- Lines are written one complete JSON line per append, serialized by an
  internal writer task and flushed before the next event; concurrent
  sessions writing the same path do not interleave bytes mid-line, but
  separate processes each continue from their own recovered `seq`, so seqs
  can repeat across processes sharing one file — prefer one file per
  process for strict uniqueness.
- Parent directories are created lazily on the first event.
- Payloads are constructed from bounded, pre-redacted fields only — never
  raw tool arguments, environment, or full transcript text. Free-form
  fields (error messages, previews) are capped at the notification limits
  (80 headline / 120 detail / 200 preview characters) and stripped of
  control bytes.

### Webhook delivery

With `webhook_url` set, every event is additionally POSTed as
`{"at": "<ISO 8601 timestamp>", "event": {…}}` with
`Authorization: Bearer <webhook_token>` when a token is configured.
Delivery is best-effort: failures are logged and dropped, never retried
into the agent loop, and a failing webhook never blocks the local file
append.

## Control Socket (`[control_socket]`)

Per-session control surface for supervised operation: with the feature
enabled, the interactive TUI binds one unix domain socket per *running*
session at `<sessions-dir>/<session-id>/control.sock` (mode `0600`;
`<sessions-dir>` is the same directory the session store uses, typically
`~/.codewhale/sessions`). The socket is removed with the session's
artifact directory, and a stale socket left by a crashed process is taken
over by the next launch. Unset or `enabled = false` = the feature is
**off** (the default) and behavior is unchanged. Unix-only; on other
platforms the key parses but no socket is bound.

```toml
[control_socket]
enabled = false # default: OFF
```

The socket speaks newline-framed JSON-RPC, one request per connection:
write one request line, read one response line, close.

```json
{"id":"1","method":"message","params":{"text":"hello"}}
{"id":"2","method":"interrupt","params":{}}
{"id":"3","method":"status","params":{}}
```

- `message` — delivers `text` as a structured user message through the
  ordinary composer dispatch path; dispatched immediately when idle,
  queued when a turn is in flight (the response's `delivery` field says
  which).
- `interrupt` — the Esc-shaped cancel of the active turn; `cancelled`
  reports whether active work was in flight.
- `status` — answers `turn_state` (`idle` / `in_progress` / `waiting`) and
  `goal` (`objective`, `status`, `paused`).

Success responses echo the request id with a `type`-tagged result;
failures carry `error.code` (`invalid_request`, `command_error`,
`timeout`, `server_unavailable`). Requests are bounded at 1 MiB per line
and a handler that does not answer within 5 s is reported as `timeout`.

## Tool Catalog

Codewhale loads a small core native tool catalog by default and leaves less
common native tools discoverable through ToolSearch. To keep specific native
tools loaded on every request, add them to `[tools].always_load`:

```toml
[tools]
always_load = ["Git", "notify"]
```

### Script tools and overrides

Scripts in `~/.codewhale/tools/` (or `[tools].plugin_dir`) that start with a
`# name:` header become model-visible tools, and `/plugin tools` lists them.
The script reads the tool's JSON input on stdin and writes a JSON
`ToolResult` (`{"content": "...", "success": true}`) on stdout.

```sh
#!/usr/bin/env sh
# name: word_count
# description: Count words in the given text
# schema: {"type":"object","properties":{"text":{"type":"string"}}}
# approval: required
```

`# approval:` takes `suggest` (the default) or `required`; either way the
tool follows the session's approval setting. A script cannot approve itself:
`approval: auto` is no longer supported, so such a script gets the default,
and the runtime log (`~/.codewhale/logs/`) and `/plugin tools` name it.

A script cannot replace a built-in tool either. A script whose `# name:` is
already registered is not loaded. `[tools.overrides]` may disable a built-in,
or add a script or command tool under a name of its own:

```toml
[tools.overrides]
"Web" = { type = "disabled" }                                   # turn a built-in off
"audited_shell" = { type = "script", path = "audit-shell.sh" }  # a new tool
"Bash" = { type = "script", path = "audit-shell.sh" }           # refused: Bash is built in
```

A `script` or `command` override keyed by a built-in is refused, and the
built-in stays active. A status line names the key once per session, and the
runtime log records it. To route a
built-in through your own wrapper, disable the built-in and register the
wrapper under a new name. An override keyed by a drop-in script's name still
replaces that script. Relative `path` values resolve against the plugin
directory.

### `request_user_input` limits

`request_user_input` asks the user a short batch of multiple-choice questions.
Both ceilings are configurable (#5949): raise `user_input_max_questions` when a
research or planning workflow legitimately needs more clarifications, lower it
when interactive triage should stay terse.

```toml
[tools]
user_input_max_questions = 6   # default 6, clamped to 1..=10
user_input_max_options = 4     # default 4, clamped to 2..=10
```

The effective values are applied in three places at once: the tool's JSON
schema (`minItems` / `maxItems`), its model-visible description, and the
payload validator. A rejected payload names the key to raise, so the model can
either resize the batch or tell the user which setting to change.

### User-input wait timeout

Questions from `request_user_input` wait until answered or canceled by default
(#6003). An omitted setting or `0` leaves the wait unbounded; a positive value
cancels the question when that many seconds pass, capped at 86,400 (24 hours).
Headless `exec` runs have no
responder, so `request_user_input` is withheld there by default:
the model reports the tool absent and finishes instead of stalling.

```toml
[tools]
user_input_timeout_seconds = 300   # opt into 5 minutes; omitted or 0 waits indefinitely; maximum 86400
```

This key governs question waits only. Approvals have their own clock,
`[approval] timeout_seconds`, which waits indefinitely unless you set it.
Wall-clock and stream protections elsewhere are unaffected.

## Feature Flags

Feature flags live under the `[features]` table and are merged across profiles.
Defaults are enabled for built-in tooling, so you only need to set entries you
want to force on or off.

```toml
[features]
shell_tool = true
subagents = true
web_search = true # enables deferred Web; the flag name is retained for config compatibility
apply_patch = true
mcp = true
exec_policy = true
code_mode = true # execute_tools composes MCP/plugin/native calls; false defers it behind tool_search
verify_tool = true # agent-callable `verify` self-critique; false removes it from the tool catalog
vision_model = false # true routes image analysis to the [vision_model] model (see above)
extension_host = false # experimental: run reviewed plugins' native code (see EXTENSIONS.md)
```

`code_mode` is on by default: `execute_tools` is advertised from the first
request and nested calls go through the same permission gate as direct calls
(see [Tool surface](TOOL_SURFACE.md#code-mode-execute_tools)). Set
`code_mode = false` to defer it behind `tool_search` again.

`extension_host` is experimental and off by default. Turning it on lets reviewed
plugins run their `native` TypeScript/JavaScript tool and slash-command code in
a separate Node (or, opt-in, Bun) process; toggling it in either direction
changes the plugin activation policy, so every plugin is reviewed again after a
restart. Its `[extension_host]` table (`runtime`, `node`, `bun`) and what the
host's sandbox does on each platform are in
[Writing an extension tool](EXTENSIONS.md).

Every flag has a row in [`docs/features.toml`](features.toml), the feature
registry; a test fails when a flag and its row disagree.

You can also override features for a single run:

- `codewhale --enable web_search`
- `codewhale --disable subagents`

Use `codewhale features list` to inspect known flags and their effective state.
The native `/config` view also includes a read-only **Experimental** section
for experimental feature flags. It shows each flag's effective enabled/disabled
state and whether that state comes from the default or a configured override.
Change feature flags in `[features]` or with `--enable` / `--disable`; the
`/config` section is an audit surface, not a stability promise. Goal and
Workflow preview rows may appear there as reserved entries until those workflows
graduate behind real gated flags.

## Web Search Provider

`web_search` uses keyless Firecrawl by default. Runtime failure or an exhausted
keyless quota degrades visibly through DuckDuckGo and Bing. China deployments
can explicitly select Baidu, Metaso, Volcengine, or a trusted SearXNG endpoint;
Codewhale does not guess geography from locale or model provider.

Configured API providers are attempted first. Runtime failure or an empty
result visibly degrades through DuckDuckGo and then Bing; the structured search
receipt records every hop. Missing configuration and network-policy denials
fail closed without sending the query to another provider.

**Provider-native search.** On routes whose provider offers its own web-search
tool (OpenAI, xAI, Anthropic, DeepSeek, Kimi and others), that search can run
ahead of the configured provider. It is a separate model call on the active
route. `[search] native` decides the order:

- unset (default): native search leads only when no search provider is
  configured; a provider chosen in `[search] provider`,
  `CODEWHALE_SEARCH_PROVIDER`, or a Tavily key wins;
- `native = true`: native search leads even when a provider is pinned;
- `native = false`: native search is never used.

The native answer is returned whole; oversized tool output spills to a session
artifact the model can page back.

**Recency and locale.** `recency` and `locale` are forwarded where the
backend's API takes them, and the search receipt reports each as honored or
ignored:

| Backend | Recency | Locale |
| --- | --- | --- |
| Firecrawl | `tbs=qdr:d/w/m/y` | `country` from the region (`de-DE` → `DE`); a bare language is ignored |
| Tavily | `time_range` | not sent (Tavily takes country names) |
| SearXNG | `time_range` | `language`, as given |
| Serply | not sent (undocumented) | `hl` language, `gl` country |

Recency is rounded up to the backend's nearest window (day, week, month,
year), so `recency = 10` searches the last month. Other backends ignore both
knobs and say so in the receipt.

For a private/internal search service that serves DuckDuckGo-compatible HTML,
keep `provider = "duckduckgo"` and set `base_url`; Codewhale appends the `q`
query parameter to that endpoint and applies network policy to its host.
Custom endpoints do not fall back to public Bing. `CODEWHALE_SEARCH_BASE_URL`
can override this per process; `DEEPSEEK_SEARCH_BASE_URL` remains accepted as
the legacy alias.

**SearXNG** ([docs](https://docs.searxng.org/dev/search_api.html)) uses the
configured instance's JSON API. Set `provider = "searxng"` and
`base_url = "https://your-searxng.example"`; Codewhale calls
`/search?q=...&format=json`. Codewhale does not use a public SearXNG instance
by default because public instances often disable JSON output or rate-limit API
traffic.

Self-host it as a separate process (Docker is fine); Codewhale never bundles or
manages the search engine itself:

- Enable JSON on the instance (`settings.yml`, `search.formats` must include
  `json`) and restart it. An HTML-only instance answers the API with HTTP 403;
  Codewhale reports that as a JSON/API-access problem on the SearXNG hop rather
  than silently returning no results.
- Bind it to loopback, or to a host and port your network policy allows. The
  instance keeps its own engine list, limiter, and limits.
- Point Codewhale at it with `[search] provider = "searxng"` and `base_url`
  (required; either the root URL or the `/search` endpoint). No instance ships
  as a default, and none is discovered automatically.
- `codewhale doctor --probe-search` sends a transport-only `HEAD` to that
  origin — no `q=`, no credentials, no redirects, no audit receipt — so a green
  probe proves reachability and network-policy admission, not that JSON is on.

Confirm the JSON API itself before assuming a Codewhale bug:

```sh
curl -sS "$BASE/search?q=codewhale&format=json" | jq '.results[0] | {title,url,score}'
```

Codewhale ranks the returned rows by `score`, highest first, and applies
`max_results` to that ranking; rows an instance reports without a usable score
keep their original relative order.

**Metaso** ([metaso.cn](https://metaso.cn)) requires a user-supplied key. Set
`METASO_API_KEY` or `[search] api_key`; Codewhale does not ship a shared key.

**Firecrawl** ([docs](https://docs.firecrawl.dev/sdks/cli)) searches Firecrawl
Cloud without a key using its bounded per-IP daily quota. Set
`FIRECRAWL_API_KEY` or `[search] api_key` for authenticated limits. Codewhale
sends no `Authorization` header in keyless mode.

**Baidu** uses Baidu AI Search at
`https://qianfan.baidubce.com/v2/ai_search/web_search`. Set
`BAIDU_SEARCH_API_KEY` or `[search] api_key`. This is a search-tool backend
only; it does not add a Baidu model provider.

**Sofya** ([sofya.co](https://sofya.co)) returns full extracted page content
rather than snippets. Set `[search] api_key` to your `ay_live_...` key, or the
`SOFYA_API_KEY` env var. This is a search-tool backend only; it does not add a
Sofya model provider.

**Serply** ([serply.io](https://serply.io)) returns Google organic results with
title, URL, and snippet. Set `[search] api_key` to your Serply key, or the
`SERPLY_API_KEY` env var. This is a search-tool backend only; it does not add a
Serply model provider.

**Tavily** ([tavily.com](https://tavily.com)) is selected automatically when a
Tavily key is present and no provider is pinned: `TAVILY_API_KEY` set, or
`[search] api_key` / `CODEWHALE_SEARCH_API_KEY` in the `tvly-` family. Doctor
reports that as `source: tavily key`. Autodetect is runtime-only — Codewhale
never writes `[search] provider` for it, and `TAVILY_API_KEY` is never merged
into `[search] api_key`. An explicit `[search] provider` or
`CODEWHALE_SEARCH_PROVIDER` always wins, so `provider = "firecrawl"` keeps
Firecrawl even with a Tavily key in the environment. Pinned `tavily` accepts
any non-empty `[search] api_key` and is configured by that key or
`TAVILY_API_KEY`; with both empty it fails closed.

```toml
[search]
provider = "firecrawl" # also duckduckgo | bing | tavily | bocha | metaso | searxng | baidu | volcengine | sofya | serply
# base_url = "https://search.example/" # optional with provider = "duckduckgo"; required with "searxng"
# api_key = "YOUR_KEY" # optional for firecrawl; required by the other API providers
# native = false # provider-native search: unset = only when no provider is configured
```

## Local Media Attachments

Use `@path/to/file` in the composer to add local text file or directory context
to the next message. Use `/attach <path>` for local image/video media paths, or
`Ctrl+V` to attach an image from a local clipboard or an explicitly forwarded
X11/Wayland clipboard. SSH terminal paste without a forwarded graphical display
is text-only; use the local terminal's paste command (`Cmd+V` on macOS or
`Ctrl+Shift+V` on Linux/Windows), and use `/attach <path>` for remote image
files. OpenSSH loopback X11 displays are detected automatically. For an
explicitly forwarded Wayland or non-loopback X11 display, set
`CODEWHALE_SSH_CLIPBOARD=graphical`; set it to `terminal` to force terminal
transfer instead of an ambient remote display. DeepSeek's public Chat
Completions API currently accepts text message
content, so media attachments are sent as explicit local path references instead
of native image/video payloads.
Attachment rows appear above the composer before submit; move to the start of
the composer, press `↑` to select an attachment row, then press `Backspace` or
`Delete` to remove it without editing the sample text by hand.

## Managed Configuration and Requirements

codewhale supports a policy layering model:

1. user config + profile + env overrides
2. managed config (if present)
3. requirements validation (if present)

By default on Unix:
- managed config: `/etc/deepseek/managed_config.toml`
- requirements: `/etc/deepseek/requirements.toml`

Requirements file shape:

```toml
allowed_approval_policies = ["on-request", "untrusted", "never"]
allowed_sandbox_modes = ["read-only", "workspace-write"]
```

If configured values violate requirements, startup fails with a descriptive error.

## Notes On `codewhale doctor`

`codewhale doctor` follows the same config resolution rules as the rest of the
TUI. That means `--config`, `CODEWHALE_CONFIG_PATH`, and the legacy
`DEEPSEEK_CONFIG_PATH` are respected, and MCP/skills
checks use the resolved `mcp_config_path` / `skills_dir` (including env overrides).

To bootstrap missing MCP/skills paths, run `codewhale setup --all`. You can
also run `codewhale setup --skills --local` to create a workspace-local
`./skills` dir.

Both plain `codewhale doctor` and `codewhale doctor --json` are structural and
offline by default. They do not check the release service, hosted provider
APIs, local provider endpoints, or MCP processes, and they do not load a
workspace credential `.env`. Use `--check-updates`,
`--probe-api`, `--probe-local`, or `--probe-mcp` to opt into the corresponding
live boundary; `--probe-local` may start a desktop-managed service such as
Ollama. Only the explicit API/local probe paths may load workspace credential
`.env` values. Live flags conflict with `--json`, so machine-readable doctor
output is always offline. Top-level keys include `version`, `paths`, `secret_backend`,
`config_path`, `config_present`, `workspace`, `api_key.source`,
`api_key.availability`, `base_url`,
`default_text_model`, `mcp`, `skills`, `tools`, `plugins`, `sandbox`,
`platform`, `api_connectivity`, and `capability`. CI consumers should rely on
`api_key.source` (`config_declared`/`env_declared`/`external_auth_declared`/
`secret_store_unprobed`/`secret_store_unavailable`/`oauth_unprobed`/
`external_consent`/`none`/`local_runtime`/`unknown`) and
`api_key.availability`
(`present`/`not_required`/`not_probed`/`unavailable`/`unknown`) rather than parsing the
human-readable `doctor` text. Source is declaration metadata, not proof that a
credential exists or works. Only a non-empty, non-sentinel literal config value
is structurally `present`; no-auth and local routes are `not_required`. Environment,
external-auth, OAuth, consent, and secret-store declarations remain `not_probed`
and cannot make structural Setup or Fleet readiness true. A secret-store sentinel
on a named/custom endpoint that is prohibited from using the shared store is
`secret_store_unavailable`/`unavailable`, while `unknown` remains reserved for
the absence of a supported structural conclusion. Exact and whitespace-wrapped
legacy sentinels are never treated as literal credentials. The structural
loader still honors safe environment routing/model/policy fields, but it never
materializes environment HTTP headers, sandbox API keys, or search API keys;
only an explicit API/local probe switches to the normal credential-loading
path. An opted-in update check also emits only typed generic failures: untrusted
release metadata and transport errors are not echoed.

If configuration loading or validation fails, `doctor --json` returns nonzero
and prints a bounded JSON error envelope with
`status = "error"` and `error.kind = "config_validation"`. It does not emit a
normal route or capability report—or the underlying possibly sensitive error—
for an invalid configuration.

MCP entries are configuration diagnostics unless an explicit MCP command is
run. `mcp.probe_scope` is `configuration`, `mcp.live_health_checked` is false,
and each server separates `checks.configuration` / `checks.command` from
`checks.process_reachable`, `checks.protocol_initialized`, and
`checks.backend_tool_health`. The latter three remain `not_checked` in doctor
output. Run `codewhale mcp validate` to explicitly start enabled servers and
verify protocol initialization/discovery; backend health still requires an
appropriate explicit tool call. Doctor reports only safe structural MCP fields:
URL userinfo/path/query/fragment and raw command arguments, environment values,
header values, and token material are omitted. Provider URLs follow the same
rule and expose only `scheme://host[:explicit-port]`.

The `capability` key contains per-provider capability info derived from
static knowledge (release docs, API guides) rather than live API probes.
Top-level sub-keys: `resolved_provider`, `resolved_model`, `context_window`,
`max_output`, `thinking_supported`, `cache_telemetry_supported`,
and `request_payload_mode`.

Use `capability.context_window` and `capability.max_output` for model-limit
checks in CI scripts; do not treat `capability.max_output` as the per-turn
request budget. Use `capability.thinking_supported` to decide whether to
configure reasoning effort.

## Setup status, clean, and extension dirs

`codewhale setup` accepts a few flags beyond the existing `--mcp`,
`--skills`, `--local`, `--all`, and `--force`:

- `--status` — print a compact one-screen status (api key, base URL, model,
  MCP/skills/tools/plugins counts, sandbox, `.env` presence). Read-only and
  network-free; safe to run in CI. If `.env` is missing and `.env.example` is
  present in the workspace, the status output points at `cp .env.example .env`.
- `--tools` — scaffold `~/.codewhale/tools/` with a `README.md` describing the
  self-describing frontmatter convention (`# name:` / `# description:` /
  `# usage:`) and an `example.sh` that follows it. The directory is
  intentionally not auto-loaded; wire individual scripts into the agent via
  MCP, hooks, or skills.
- `--plugins` — scaffold `~/.codewhale/plugins/` with a `README.md` and an
  `example/plugin.toml` plus a namespaced example Skill. Bundles are discovered
  read-only, untrusted, and disabled; review them through `/plugin` before
  enabling. v0.9.1 activates only declared Skills and MCP servers. See
  [PLUGIN_BUNDLES.md](PLUGIN_BUNDLES.md).
- `--all` now scaffolds MCP + skills + tools + plugins together.
- `--clean` — list `~/.codewhale/sessions/checkpoints/latest.json` and
  `offline_queue.json` if they exist. Legacy
  `~/.deepseek/sessions/checkpoints/` files are not scanned automatically; set
  `CODEWHALE_HOME=~/.deepseek` for a one-off legacy cleanup. Pass `--force` to
  actually remove matched files. This never touches real session history or the
  task queue.

`--status` and `--clean` are mutually exclusive with the scaffold flags.

## Why the engine strips XML/`[TOOL_CALL]` text

codewhale sends and receives tool calls only over the API tool channel
(structured `tool_use` / `tool_call` items). The streaming loop in
`crates/tui/src/core/engine.rs` recognizes a fixed set of fake-wrapper start
markers — `[TOOL_CALL]`, `<codewhale:tool_call`, `<tool_call`, `<invoke `,
`<function_calls>` — and scrubs them from visible assistant text without ever
turning them into structured tool calls. When a wrapper is stripped, the loop
emits one compact `status` notice per turn so the user can see why their
visible text shrank. Treat any change that re-enables text-based tool
execution as a regression; the protocol-recovery tests in
`crates/tui/tests/integration/protocol_recovery.rs` lock the contract.

## Model-bound redaction (`[redaction] model_bound`)

Codewhale masks credential-looking values in tool output **before it is sent
to an upstream model** — the "model boundary". A file read by a tool can
contain a configured API key, a bare provider token, or a credential-shaped
opaque string, and the model must not see those bytes. This backstop is
separate from the display/export scrubbers: it decides what the model itself
can quote back, and it is deliberately conservative (`CredentialShaped`
policy, see `crates/config/src/persistence.rs`), so ordinary code and config
stay byte-exact while keys, JWTs, bearer tokens, PEM blocks, and long opaque
runs are masked.

Turning that masking **off** is a security decision, so it is not a plain
boolean:

```toml
[redaction]
model_bound = "disabled"   # "enabled" (default) | "disabled"
```

Setting `"disabled"` only records a *request*. It takes effect only when all
of these are true:

1. You restart the interactive TUI.
2. The startup gate appears and you press `1`/`Y` on its first stage
   ("confirm and disable"). This only advances to a second, final-confirmation
   stage - the gate repeats the red warning and asks "are you really sure?".
3. On that second stage you press `1`/`Y` again. The gate is rendered with the
   same explicit-key discipline as workspace trust - `Enter` never confirms by
   reflex, and `2`/`U` on the second stage steps back.
4. Only that second confirmation persists a receipt to
   `~/.codewhale/redaction-state.json` (next to `config.toml`) and rebuilds
   the engine with masking off for the rest of this launch and future ones.

The receipt is bound to the config it was made against and is valid only
while that config still requests `"disabled"`. Setting `model_bound` back
to `"enabled"` - or rewriting `config.toml` in any way after the
confirmation - invalidates it, so requesting `"disabled"` again later
asks for a fresh confirmation on the next launch. The receipt checks both the
config contents and modification time; missing, unreadable, or malformed config
and older receipts without this binding keep masking enabled. Legacy-home
installs store the receipt beside their resolved config file.

Until a confirmation exists, the effective mode is always `"enabled"`:

- Choosing `2`/`U` ("keep masking on") leaves the config field untouched, so
  the next launch asks again. Edit the field back to `"enabled"` to stop being
  asked.
- Non-interactive entry points (`codewhale exec`, hooks, automations, headless
  agents) never confirm anything and never apply an unconfirmed request.
- Routing/classification summaries and durable goal-state text keep their own
  always-on redaction regardless of this switch; the opt-out exists so the
  model can quote file bytes for exact edits, not to relax stored state.

The config value itself is forgiving: `true`/`false`, `"on"`/`"off"`, and
`"enabled"`/`"disabled"` (any casing) all parse, with `false`/`"off"` meaning
`"disabled"`.

A confirmed opt-out still sends your configured API keys to the provider you
are already talking to. Only use it when the model must read and edit files
that contain real credentials.

### Stored sessions

The masking runs once, when tool output enters the transcript, so saved
session files (`~/.codewhale/sessions/*.json` and their checkpoints) and
Runtime API thread items (their text and tool metadata) do not store a
credential-shaped value from tool output. With the confirmed opt-out above,
tool output is stored as the model saw it.

Not yet masked: large outputs spilled to `~/.codewhale/tool_outputs`, shell
completion evidence artifacts, and tool-call *inputs* (for example a
`curl -H "Authorization: Bearer …"` command line). `doctor` and
`scrub-secrets` do not check those either.

Sessions saved by builds before this change may still hold credentials in
their tool output; nothing rewrites them automatically. `codewhale doctor`
reports them under **Stored Sessions** (it checks the newest 50 files), and
this command finds and masks them across every saved session:

```sh
codewhale sessions scrub-secrets          # report only
codewhale sessions scrub-secrets --apply  # rewrite the affected files
```

`--apply` rewrites each file under the same per-session lock a save takes,
so it never loses a concurrent save. A session that is still open can write
its in-memory copy back on its next save, so close open sessions first, and
rotate any credential that was exposed — masking a stored copy cannot un-leak it.
