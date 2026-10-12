# Codewhale product telemetry

> 阅读简体中文版：[zh_hans/TELEMETRY.md](zh_hans/TELEMETRY.md)

**Usage analytics are on by default in the current source.** The first
interactive launch gives a localized, nonblocking disclosure naming **Codewhale
and PostHog**, with a direct route to turn counting off. Notice version `5`
describes this policy; displaying it is not recorded as human acceptance.
Missing an older acceptance record does not turn a fresh installation off.
Existing explicit opt-outs remain off across upgrades, and unreadable privacy
state fails closed. Environment and CLI kill switches still take precedence.

Read the disclosure with `codewhale config telemetry`. Turn usage off in
`/settings` or run `codewhale config set telemetry false`. Turning it back on
in Settings or with `codewhale config set telemetry true` updates the existing
durable preference and privacy records for new sessions. The compatibility
command `codewhale config telemetry --accept-notice 5` remains available, but
is not required for a fresh installation. Collection never includes work content or credentials.

The PostHog sink is optional and inert until the ingest operator explicitly
configures a project token and approved regional host, and records staging
proof that the actual egress path does not forward the original client IP.
Source readiness does not establish a deployment, an activated processor,
an egress receipt, or a retention setting.

Codewhale does not collect conversations, code, prompts, files, file/repo/branch
names, model content, or credentials. It sends no per-turn or per-tool timeline.
It does send the closed, aggregate schema below: version and platform classes,
session duration/outcome, feature/error counters, and a random install id that
rotates every 90 days.

**There is now a real endpoint.** An enabled session sends its batches to the
first-party ingest service at `https://telemetry.codewhale.net/v1/telemetry`,
which is the shipped default for `telemetry_endpoint`. What that service is,
what it stores, and what it structurally cannot store is spelled out in "What
the endpoint does" below.

**To send nothing anywhere, turn telemetry off** (see "Turning it off"). To stay
enabled but contact nobody, set `telemetry_endpoint = ""`: batches are then
appended to `$CODEWHALE_HOME/telemetry/dryrun.jsonl` on your own machine, byte
for byte what the server would have received, and no HTTP client is ever
constructed. That file is how you audit this document against reality.

This document is the schema. It is not a summary of the schema: a test in
`crates/telemetry` parses the field names out of this file and asserts set
equality against the structs the serializer actually uses, so a field that is
here and not in the code — or in the code and not here — fails the build.

Local saved sessions and `codewhale metrics` remain available with telemetry
disabled and require no account or hosted reporting service. Disabling telemetry
does not disable local usage receipts.

## Turning it off

There are two off switches and they do different things. Both stop collection
completely; only one of them erases anything.

```sh
codewhale config set telemetry false     # opt out: stops collection and erases state
CODEWHALE_TELEMETRY=0 codewhale          # kill switch: stops collection, erases nothing
codewhale --telemetry false              # the same kill switch, for one command
```

**`telemetry = false` in the config file is the opt-out.** It is a floor:
`--telemetry true` and `CODEWHALE_TELEMETRY=1` both lose to it, because a
setting you can undo by accident from a wrapper script is not a setting. It
deletes the random install id, truncates every buffered event and every dry-run
record, and writes a tombstone. Appends, identity/state writes, and delivery all
share the wipe's ordering lock, so once opt-out returns no pre-opt-out write or
POST remains in flight. If any part of that wipe fails, the tombstone is still
there and the buffer is undrainable — a failed wipe fails closed. Every later
run re-asserts the same tombstone for as long as the setting stands, so it
survives. Turning telemetry back on in `/settings` records that explicit preference
in both existing privacy registers; a fresh launch can then clear the tombstone. Nothing buffered before that point is
ever sent.

**The environment variable and the flag are kill switches, not opt-outs.**
Telemetry is off for the run, nothing is written, nothing is sent — and
nothing on disk is touched or deleted. That is deliberate: a harness or agent
that sets `CODEWHALE_TELEMETRY=0` for one command must not silently discard the
install id and the dry-run records of the person who owns the machine. If you
want the erasing kind, use the config file.

`CODEWHALE_TELEMETRY` (and its `DEEPSEEK_TELEMETRY` alias) accepts
`0`, `1`, `true`, `false`, `yes`, `no`, `on`, `off`, `enabled`, `disabled`.
A value this list cannot read also resolves to off — a typo in a kill switch
must never resolve to "on".

The first-run notice is not shown when telemetry is already persistently off or
a run-scoped kill switch is active. The notice never rewrites a
`telemetry = false` you put there yourself.

A repo-local `.codewhale/config.toml` can set neither `telemetry` nor
`telemetry_endpoint`, and a workspace `.env` can set neither. Someone else's
repository cannot turn your telemetry on or aim it at a host of their choosing.

## Where it lives, and how much of your disk it uses

Everything is under `$CODEWHALE_HOME/telemetry/` (`0700`), every file `0600`:

| file | role |
|---|---|
| `buffer.jsonl` | pending events, one JSON object per line |
| `buffer.jsonl.lock` | the sibling ordering lock shared by writes, delivery, arming, and wipe |
| `dryrun.jsonl` | where batches go when the endpoint is configured empty |
| `state.json` | the last app version seen and the last flush attempt |
| `install_id.json` | the random install id and when it was minted |
| `disabled` | the tombstone; present means nothing is appended or sent |

Both `buffer.jsonl` and `dryrun.jsonl` are rings capped at 512 records or
256 KiB, whichever comes first, with the oldest dropped. The documented
footprint ceiling for the whole directory is therefore **512 KiB plus a few
hundred bytes of metadata**.

The install id is a random v4 UUID. It is never derived from your hostname,
MAC address, `machine-id`, home directory, username, or executable path — a
derived id is a device fingerprint that survives reinstall and re-identifies
you across your own opt-out. It is regenerated whenever
`$CODEWHALE_HOME/telemetry/` is cleared, which opting out does automatically,
and in any case every 90 days.

There is no factory-reset command in Codewhale, so this document does not
claim one.

## When anything is sent, and where

Nothing is collected or sent when a persistent opt-out or run-scoped kill
switch is in force, or when privacy state is unreadable. TUI and `exec` sessions have exactly one network flush point: an attempt
during shutdown, bounded at three seconds. Short CLI commands such as `config`,
`doctor`, and `auth` do not wait for that network request. They record
`session_end`, seal the event to the local buffer with a much shorter bound, and
return; a configured endpoint sends those buffered events with the next
interactive shutdown flush. An explicitly empty endpoint finalizes its local
dry-run batch immediately.

There is no startup flush, mid-session flush, per-turn flush, or per-tool-call
flush. Every network shutdown flush re-resolves your setting from disk
immediately beforehand, so `codewhale config set telemetry false` written from
another terminal stops the flush of a session that is already running.

A flush is **one `POST`** to the resolved endpoint — by default
`https://telemetry.codewhale.net/v1/telemetry`. The request carries a
`content-type: application/json` header, a
`user-agent: codewhale-telemetry/<app_version>`, and the batch body. That is
all: no cookies (the HTTP client is built without a cookie jar to disable), no
redirects (refused outright), no `Authorization` header, no custom headers, and
no query string. The response body is discarded unread; only the status class is
looked at.

`https://` is required. Plain `http://` is accepted only for a loopback host, so
you can point the client at a recorder of your own and read the wire form
directly; no environment variable overrides that refusal. An endpoint the client
refuses turns telemetry off for the run rather than falling back to a different
destination.

Any failure — DNS, connect, TLS, timeout, non-2xx — drops the batch. There is
no retry, no backoff, and no re-queue. A permanently offline machine attempts at
most once per flush point and never grows a queue.

---

## Event schema

`SCHEMA_VERSION = 3`. `notice_version = 5` identifies the disclosed default-on
policy; it does not assert human consent or that a notice was seen. The closed
v2 contract still accepts only `consent_version = 4`, retaining its original
explicit opt-in meaning. Fields cannot be mixed between versions. The unchanged
v1 contract remains first-party only and **never sent to PostHog**; it refuses
new fields, surfaces, and `product_usage` events.

 Every field is an integer, a boolean, or a **closed enum string**, except exactly three bounded strings: `app_version`, `git_sha`, `panic_site`. Each of the three has a written rule and a test pinning the rule. **There is no free-form string type in this schema, and no open-keyed map.** That is the property that makes red line 3 enforceable rather than aspirational.

### Batch envelope — sent on every POST

```jsonc
{
  "schema_version": 3,
  "notice_version": 5,
  "sent_at":     "2026-08-03T18:04:11Z",   // RFC3339 UTC, second precision
  "install_id":  "3f2a…",                  // uuid v4, rotates every 90 days
  "app_version": "0.9.12",
  "git_sha":     null,                     // non-null only for SHA-stamped builds
  "surface":     "tui",
  "os":          "macos",
  "arch":        "aarch64",
  "libc":        "none",
  "tty":         true,
  "events":      [ … ]
}
```

| Field | Type | Source anchor | Rule |
|---|---|---|---|
| `schema_version` | `u32` | const in `crates/telemetry/src/event.rs` | Bumped on any field add/remove/retype. Never reused. Pinned by a golden snapshot test. |
| `notice_version` | `u32` | policy constant in `crates/telemetry/src/event.rs` | Exactly `5`; identifies the disclosed opt-out policy, not a human acceptance record. |
| `sent_at` | RFC3339 | `chrono::Utc::now()` | Second precision. Per-**batch** only — events carry no timestamps at all. |
| `install_id` | uuid v4 | `crates/telemetry/src/envelope.rs` | Random, never derived, rotated every 90 days. See "Where it lives" above. |
| `app_version` | string | `env!("CARGO_PKG_VERSION")`, as at `crates/telemetry/src/lib.rs:112` | Must match `^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$`. |
| `git_sha` | string \| null | `option_env!("CODEWHALE_RELEASE_BUILD_SHA")` — a **new** rustc-env | First 12 hex chars. Emitted **only** when `codewhale_build_support::release_build_sha` saw a valid full SHA in `CODEWHALE_BUILD_SHA`, the legacy `DEEPSEEK_BUILD_SHA`, or `GITHUB_SHA`, in that precedence order. `null` for every unstamped build, with no runtime lookup of any kind. **Never** `CODEWHALE_BUILD_COMMIT` — that falls back to `git_commit` and is the builder's private HEAD. **Never** `Thread.git_sha` (`crates/state/src/lib.rs:93`) — that is the user's workspace commit and a red line, one identifier away by name. |
| `surface` | enum | explicitly set by the emitting client | `tui \| exec \| cli \| app-server \| mcp-server \| serve \| website \| web-app \| desktop \| control-plane \| vscode-extension`. Runtime metrics keep their existing collector. Browser product counts use the same closed envelope. A declared surface is not proof its client is deployed. `vscode-extension` is an extension client of this schema, deliberately generic the way `web-app` and `desktop` are: the editor's own name and version are **not** collected. Widening this whitelist changes no field, so the schema version is unchanged. |
| `os` | enum | `std::env::consts::OS`, as at `crates/cli/src/update.rs:41` | Whitelist: `linux \| macos \| windows \| freebsd \| android \| other`. |
| `arch` | enum | `std::env::consts::ARCH` | `x86_64 \| aarch64 \| other`. |
| `libc` | enum | `cfg!(target_env)` — **compile time** | `gnu \| musl \| none`. Runtime detection reads distro vendor strings; compile-time is free and leaks nothing. |
| `tty` | bool | `std::io::IsTerminal`, as at `crates/telemetry/src/envelope.rs:196` | `stdin().is_terminal() && stdout().is_terminal()`. |
| `events` | array | the drained buffer | Every element is one of the six events below and nothing else. Capped at 200 events or 64 KiB per batch; a batch that would exceed either cap leaves the remainder buffered for the next flush. |

**`os_major` is not collected.** Reading it costs unsafe FFI on two platforms plus a file parser on a third, in the one crate whose entire value is being small enough to audit — and `os`, `arch`, and `libc` are free and answer the platform question. It may be reconsidered if the stored data ever shows that the OS-version cut is what triage is missing; a hunch is not that evidence.

### Which surfaces emit

Every runtime surface uses the same `decide` and flush recheck: readable privacy
state, resolvable home, valid endpoint, and no persistent opt-out or run kill
switch. Missing preferences default on. The interactive TUI shows the disclosure;
headless commands never synthesize consent. Existing runtime events and counters
are unchanged; adding PostHog does not add a second runtime collector.

### An embedder declares the server it started

A client that starts `codewhale serve` may name itself, so its sessions are not
counted as anonymous `serve` traffic from nobody in particular. It sets
`CODEWHALE_TELEMETRY_SURFACE` to a value in the `surface` whitelist before
spawning the server; the extension client uses `vscode-extension`.

Three properties of that read are deliberate:

- **`serve` only.** The variable is inherited by descendant processes, and an
  agent's shell command runs `codewhale` descendants carrying this environment.
  Only the branch that *is* the embedder's own server consults it, so a nested
  `codewhale exec` cannot report itself as the embedder.
- **`tui` is refused**, and `serve --mcp` still reports `mcp-server`: nothing
  starts the interactive terminal UI on a user's behalf, and a server is not an
  MCP server because an embedder said so.
- **An unrecognised name is dropped, not repaired.** A value outside the
  whitelist falls back to `serve` rather than failing startup or being guessed
  at; the field is a label, and the default is the honest one.

This changes how an embedder's sessions are *labelled*. It does not change
whether anything may be collected: the server resolves that itself, from the
same durable config, the same setup state, and the same `CODEWHALE_TELEMETRY`
run-scoped kill switch. An embedder cannot enable collection where the user has
turned it off, and a durable opt-out in one client is a durable opt-out for every
client on the machine, because they share one home and one install identity.

The extension client is the worked example: it holds a single on/off setting and
passes `CODEWHALE_TELEMETRY=0` when the user turns reporting off. It contains no
payload builder, no buffer, and no transport of its own — so its sessions share
the home's `install_id` rather than minting a second one, and one installation is
one install in every count derived from that id.

Because that switch is run-scoped rather than a durable opt-out, turning it off
stops the engine this extension starts and leaves the shared buffer and identity
alone: those belong to every surface on the machine, not to one window, and
clearing them would silently change what the user's terminal sessions do. A user
who wants reporting stopped and cleared everywhere does that once, with
`codewhale config set telemetry false`.

Website and application clients use `product_usage`, with notice version `5`,
a random browser-local v4 ID rotated after 90 days, `os = other`, `arch = other`,
`libc = none`, `tty = false`, and `git_sha = null`. They do not inspect the browser
fingerprint. Their configured same-origin proxy forwards only this bounded JSON
to the first-party ingest, with no incoming cookies, headers, URLs, or identities.
The browser endpoint is inert if unconfigured. Browser opt-out clears pending
counts and local identity; actions taken while disabled are never backfilled.
Product usage settings belong in the app and runtime; marketing privacy details
and any website opt-out belong on the privacy page.

### Event: `install_or_upgrade`

Emitted once when `state.json`'s `last_version` differs from `app_version`.

```jsonc
{ "event": "install_or_upgrade", "kind": "upgrade", "previous_version": "0.9.3" }
```

| Field | Type | Source | Rule |
|---|---|---|---|
| `kind` | enum | derived | `install` (no prior record) \| `upgrade` \| `downgrade`. |
| `previous_version` | string \| null | `$CODEWHALE_HOME/telemetry/state.json` **only** | Same regex as `app_version`. Never derived from session history or config mtimes — those files have a different privacy contract. |

### Event: `session_start`

```jsonc
{ "event": "session_start", "source": "interactive" }
```

`source` is `SessionSource` (`crates/state/src/lib.rs:34-41`) stringified by `session_source_to_str` (`:1909-1917`): `interactive | resume | fork | api | unknown`.

### Event: `session_end`

The workhorse. Everything a session accumulated ships here, once.

```jsonc
{
  "event": "session_end",
  "duration_bucket": "1m_10m",
  "exit_class": "clean",
  "cold_start_bucket": "250_1000",
  "providers": ["deepseek", "custom"],
  "counters": { "turns": 14, "tool_calls": 61, "fleet_dispatch": 0, "workflow_run": 0,
                "subagent_spawn": 2, "mcp_server_connected": 0, "memory_search": 0,
                "approval_modal_shown": 0, "approval_auto_allowed": 0,
                "command_palette_open": 3 },
  "errors":   { "auth_preflight_failed": 0, "provider_http_4xx": 0, "provider_http_5xx": 1,
                "tool_denied_by_policy": 0, "tool_timeout": 0, "network_error": 0 },
  "turn_wall": { "lt_5s": 9, "5_30s": 4, "30_120s": 1, "gte_120s": 0 }
}
```

**`counters` and `errors` are `#[derive(Serialize)]` structs of named `u32` fields, not maps.** Every field is serialized including zeros. The key set is closed by the compiler: adding a counter requires editing `crates/telemetry/src/event.rs`, which is where the doc-match test lives.

**`duration_bucket`** — `chrono` delta from `app.session_started_at` (`crates/tui/src/tui/app.rs:1889`). Half-open, seconds: `lt_1m` (`d < 60`), `1m_10m` (`60 ≤ d < 600`), `10m_60m` (`600 ≤ d < 3600`), `gt_60m` (`d ≥ 3600`).

**`exit_class`** — `clean | signal | panic | error`. **Derived from an explicit `AtomicU8`, never from an exit code.** `RunTerminationReason::Canceled` maps to exit 130 (`crates/tui/src/core/runtime_contract/termination.rs:53`), the same value the signal task uses (`crates/tui/src/lib.rs:682`, 128+SIGINT), so a code-based derivation would report every Esc-cancelled turn as a signal. The atomic is set by the panic hook (`crates/tui/src/lib.rs:1582`), by the signal task (`:678-696`) before `std::process::exit`, and on the clean path from `RunTerminationReason::is_success()` (`crates/tui/src/core/runtime_contract/termination.rs:44-46`) — `error` otherwise. Do **not** use `exec_failure_exit_code` (`crates/tui/src/lib.rs:10432`): it knows only `{75, 1}` and would report an approval-required exit (3) as a generic failure.

**`cold_start_bucket`** — from `startup_trace::elapsed_ms()`, which reads `PROCESS_START` directly and is independent of the startup summary's buffer clear (`crates/runtime/src/startup_trace.rs:39-45`). Boundaries: `lt_250`, `250_1000`, `1000_3000`, `gte_3000`. Absent on non-TUI surfaces.

**`providers`** — sorted, deduplicated array of `ProviderKind::as_str()` (`crates/config/src/provider_kind.rs:295`, a `&'static str` from a closed enum; `Custom` yields the literal `"custom"`). **The API takes `ProviderKind` by value, never `&str`.** Do not call `ProviderKind::parse` or `parse_config_identity` (`:300`, `:330`) — those are for config-table resolution. **Do not read** `provider_identity_for_persistence()` (`crates/tui/src/tui/app.rs:5049`), `provider_id_for_persistence()` (`:5058`), `ExecStreamMeta.provider_id` (`crates/tui/src/lib.rs:10220`), or `PlannedTurnRoute.effective_provider_label` (`crates/tui/src/turn_route_plan.rs:189-193`) — all four return the customer's own `[providers.<name>]` table key when the route is Custom. This is the single most likely leak in the feature: it is one field away from the natural seam and `/status` already prints it (`crates/tui/src/commands/groups/config/status.rs:24-28`). **No model id is ever sent, for any provider** — `crates/runtime/src/safe_label.rs:11-15` documents that a model id can be a path, a URL, or a deployment id that is itself a credential.

**`counters`** — closed field set. Every bump happens at the **call site**, never inside a conditionally-entered handler:

| field | source anchor |
|---|---|
| `turns` | `run_event_loop` in `crates/tui/src/tui/ui/event_loop.rs`, immediately before it calls `execute_turn_end_observer_hook`. Never inside that hook: its first statement is `if !app.hooks.has_hooks_for_event(HookEvent::TurnEnd) { return Ok(()); }` (`crates/tui/src/tui/ui/observer_hooks.rs`), and the natural future optimization hoists that check to the call site, silently zeroing the counter for every user without hooks. |
| `tool_calls` | `execute_tool_with_lock` in `crates/tui/src/core/engine/tool_execution.rs` — surface-agnostic, fires for exec and CLI too |
| `fleet_dispatch` | `create_queued_run_with_descriptor` in `crates/tui/src/fleet/manager.rs` — the single funnel that `create_run` and `create_queued_run` both land in; counting at either caller would double-count a plain `fleet run`. |
| `workflow_run` | bumped in `WorkflowTool::execute` (`crates/tui/src/tools/workflow/mod.rs`) only after `parse_workflow_action` returns an `Ok(WorkflowAction)`, never from `input["action"]`. The JSON Schema `enum` in `WorkflowTool::input_schema` is what is published *to the model* — a declaration, not a guard; the real parse also accepts `spawn\|wait\|list\|inspect\|stop\|abort`, and its reject arm (`Invalid workflow action '…'`) embeds the model string verbatim, so a rejected action is never counted. |
| `subagent_spawn` | `apply_agent_spawned_status_and_observer` in `crates/tui/src/tui/ui/apply.rs` |
| `mcp_server_connected` | bumped when a server snapshot's `.connected` is true, in `snapshot_from_config` (`crates/tui/src/mcp.rs`); never `name`, `command_or_url`, or `error` — server names are user-chosen and routinely internal infra |
| `memory_search` | `tool_name == "memory_search"` (the tool registered in `crates/tui/src/tools/native_memory.rs`), counted at the same `execute_tool_with_lock` choke point |
| `approval_modal_shown` | the `Event::ApprovalRequired` arm of `run_event_loop` (`crates/tui/src/tui/ui/event_loop.rs`; the event is defined in `crates/tui/src/core/events.rs`) |
| `approval_auto_allowed` | `tool_ask_rule_decision_for_context` in `crates/tui/src/core/engine.rs`. Count only. Never `matched_rule`, `reason()`, the command, or argv — `auto_allow` patterns are user-authored command strings (`crates/execpolicy/src/command_safety.rs:35/309`) |
| `command_palette_open` | the palette key path in `run_event_loop` (`crates/tui/src/tui/ui/event_loop.rs`) and `handle_context_menu_action` in `crates/tui/src/tui/mouse_ui.rs` |

**`errors`** — closed field set. Every value is a **variant discriminant**, never `err.to_string()`:

| field | source anchor |
|---|---|
| `auth_preflight_failed` | discriminant of `CredentialReadiness` (`crates/workflow/src/fleet_preflight.rs:37-58`) / `ProviderAuthClass` (`crates/tui/src/provider_readiness.rs:32`). Discriminant only — `Missing { detail }` carries free text |
| `provider_http_4xx` | `status.as_u16() / 100 == 4`, captured at `crates/tui/src/client/chat.rs:595` and `:673` **before** the `bail!`. One row per field, because the doc-match test reads this table field for field |
| `provider_http_5xx` | `status.as_u16() / 100 == 5`, same capture points |
| `tool_denied_by_policy` | the `permission_denied` arm of the 8-variant match at `crates/tui/src/core/engine/tool_execution.rs:512-531` |
| `tool_timeout` | the `timeout` arm, same match |
| `network_error` | `retry_reason_label_and_human()`'s `&'static str` half, `crates/tui/src/client.rs:2659` |

Why discriminants and nothing else: `ToolError::PathEscape`'s `Display` *is* an absolute path (`crates/tools/src/lib.rs:61`); `fim.rs:48-50`'s `Display` *is* a literal source fragment the model emitted; `secrets/src/lib.rs:50`'s `Display` carries the secret store's absolute path; every `LlmError` variant carries the raw provider HTTP body verbatim (`crates/tui/src/llm_client/mod.rs:327`), and a 400 from a content filter routinely echoes the prompt.

**`turn_wall`** — a per-session histogram of counts, never per-turn events. `lt_5s`, `5_30s`, `30_120s`, `gte_120s`. Recorded by `observe_turn_secs` next to the `turns` bump in `run_event_loop`, which already has the turn duration in hand.

### Event: `panic`

Appended **synchronously** by the panic hook, because a `session_end` may never be written.

```jsonc
{ "event": "panic", "site": "crates/tui/src/lib.rs:1582:5" }
```

`site` comes from `panic_info.location()` (`crates/tui/src/lib.rs:1597-1600`) or `Location::caller()` (`crates/tui/src/utils.rs:523`). **Allowlist reduction, not optional:** emit verbatim only if `file()` starts with `crates/`; otherwise emit the literal `"<dep>"`. Must match `^crates/[A-Za-z0-9_/.-]+\.rs:\d+:\d+$` or `^<dep>$`. There is no `--remap-path-prefix` in this repo (no `.cargo/config.toml`; `Cargo.toml:69-74` sets only `lto`/`strip`/`codegen-units`), so a panic inside a registry dependency yields `/Users/<builder>/.cargo/registry/src/…/ratatui-0.29.0/src/…` — the **build machine's username**, shipped from every user's binary.

**The panic message is never sent.** The hook at `crates/tui/src/lib.rs:1590-1596` builds `msg` from the payload; telemetry must not read it. A slicing panic embeds the entire string being sliced, and this tree slices user and model text in dozens of places.

### Event: `product_usage`

One aggregate of explicit product interactions, with every field a saturating
`u32`, including zero values. No page paths, URLs, referrers, search terms,
account/session IDs, text, or action timestamps are permitted. Clients flush
aggregate counts on page exit or when hidden, rather than each action.

```jsonc
{
  "event": "product_usage",
  "counters": {
    "page_view": 1,
    "docs_view": 0,
    "install_copy": 0,
    "download": 0,
    "signup": 0,
    "login": 0,
    "session_create": 0,
    "session_resume": 0,
    "turn_submit": 0,
    "turn_complete": 0,
    "settings_open": 0,
    "integration_connect": 0,
    "error_shown": 0
  }
}
```

`page_view` counts a page view and `docs_view` a documentation view; `install_copy`
and `download` count installation actions. `signup` and `login` count successful
authentication actions, never identity. `session_create`, `session_resume`,
`turn_submit`, and `turn_complete` count their named product actions.
`settings_open`, `integration_connect`, and `error_shown` are counts only:
never the selected setting, integration name, error body, or work content.
An event's availability depends on the emitting client; zero is not proof a
feature was exercised or available.

### Event: `operations_summary`

Anonymous service health aggregates from the existing control-plane signal ledger.
Exactly these six saturating `u32` fields are accepted, only on `control-plane`.
This requires an explicit operator delivery/processor configuration, independently
of the end-user analytics choice. Each batch uses a fresh random v4 `install_id`;
there is no user/account/session join. No per-request rows, routes, request
digests, IDs, error text, traces, or timing series are permitted. `requests` and
`errors` count requests and failures; `duration_ms_total` and `duration_ms_max`
are aggregate latency values; `probes` and `probes_failed` count health probes.

```jsonc
{
  "event": "operations_summary",
  "requests": 10,
  "errors": 1,
  "duration_ms_total": 1500,
  "duration_ms_max": 300,
  "probes": 2,
  "probes_failed": 0
}
```

### What the endpoint does — a shipping gate, not a footnote

This section was a gate on configuring any non-loopback endpoint. The endpoint is now configured by default, so this is a description of a service that exists rather than a promise about one that might.

**What it is.** `https://telemetry.codewhale.net/v1/telemetry` — a Cloudflare Worker named `codewhale-telemetry-ingest`, whose complete source is in this repository at [`telemetry-ingest/`](../telemetry-ingest/). It is the single schema and storage authority; browser applications may use a same-origin proxy that forwards only the body. There is no telemetry queue or additional runtime collector. It is write-only: nothing in the Worker can read back what was stored, and querying happens out of band through Cloudflare's SQL API with the owner's token. The hostname is deliberately self-describing, so anyone inspecting their own network traffic can tell what it is from the name alone.

**What it stores.** Everything in this document and nothing else, in Workers Analytics Engine — one row per event. The validator in `telemetry-ingest/src/schema.ts` is a **closed** field set: an unknown key anywhere in a batch rejects the whole batch with `400`. A future client bug that starts attaching a path, a prompt, or a provider table name gets refused by the server rather than quietly stored. `telemetry-ingest/test/schema-doc.test.ts` parses the field names and enum spellings back out of *this file* and asserts set equality against the validator, and `telemetry-ingest/test/ingest.test.ts` posts the Rust client's own pinned golden batch and asserts it is accepted byte for byte — so this document, the client, and the endpoint cannot drift apart without a red test.

Batches are **IP-stripped at ingest**. No IP is stored, logged, or joined to `install_id` — and that is structural rather than a setting anyone could flip. An Analytics Engine row is exactly `_sample_interval`, `blob1`–`blob20`, `dataset`, `double1`–`double20`, `index1`, and `timestamp`. Every one of those columns is written by the Worker's own `writeDataPoint` call; there is no implicit column, so **there is no IP, country, or geo column** — no slot one could occupy even if the code wanted it there. And the code cannot want it:

- The handler reads exactly **two** request headers — `content-type` and `content-length`. Nothing else, ever.
- It never touches the request's `cf` property, so country, colo, city, region, ASN, timezone, and coordinates are never in scope.
- The function that builds every stored row cannot see the request at all; its input type is the validated batch body.
- Nothing logs. `invocation_logs` is off in `wrangler.jsonc` — Cloudflare describes those as "enriched with information available to Cloudflare in the context of the invocation", which is exactly the class of automatic per-request record this service will not keep — and there is no `console.*` call in the source.
- Rate limiting is keyed on `install_id` from the validated body, never on a network address. An IP-keyed limiter would mean this Worker handles IPs. It is the weaker limiter and it is the right trade.
- `telemetry-ingest/test/no-ip.test.ts` reads the shipped source as text and fails the build if any address or geo name appears, if the set of headers read grows past two, if a `console.*` call is added, or if a `Response` is ever constructed with a body.

**Retention: three months.** That is Cloudflare's fixed window for Analytics Engine and it is not configurable, so it is a ceiling rather than a policy — there is no setting that could make it longer.

**Optional PostHog processor.** After first-party storage, an explicitly
configured ingest may forward validated schema-v3 / notice-v5 batches and
legacy schema-v2 / consent-v4 batches to
PostHog's [batch capture API](https://posthog.com/docs/api/capture). The only
permitted origins are `https://us.i.posthog.com` and `https://eu.i.posthog.com`.
Legacy v1 batches cannot reach that path. PostHog receives the same bounded
envelope and event fields, the batch timestamp as its event timestamp, and
`codewhale:<install_id>` as its anonymous `distinct_id`. Events are named
`codewhale_install_or_upgrade`, `codewhale_session_start`,
`codewhale_session_end`, `codewhale_panic`, `codewhale_product_usage`, or
`codewhale_operations_summary`.
`$process_person_profile = false`, `$geoip_disable = true`, and `$ip = null`
are fixed transport controls. No request metadata, profile identification,
SDK, autocapture, session replay, advertising, or work content is forwarded.
A fresh server request has no user cookies or authorization headers, cannot
follow redirects, and is limited to 1.5 seconds. There are no retries or logs;
a failure does not change a successful first-party ingest response.

The PostHog project's retention and privacy settings are separate deployment
configuration and must be reviewed before activation. The Analytics Engine
three-month limit does not establish a PostHog retention period.

**The edge egress path also requires staging proof.** [Cloudflare documents](https://developers.cloudflare.com/fundamentals/reference/http-headers/#cf-connecting-ip-in-worker-subrequests)
that Worker subrequests to non-Cloudflare zones can receive platform-added
client-IP headers. Fresh JavaScript headers and local fetch tests cannot prove
those headers are removed. `POSTHOG_IP_SAFE_EGRESS_VERIFIED="true"` is an explicit
operator prerequisite alongside the host and token, and must remain unset
until a receipt for the actual deployment and regional destination confirms
no original client IP in any received header, including `CF-Connecting-IP`,
`X-Forwarded-For`, and `X-Real-IP`. The flag does not strip platform headers.
Requalify a changed egress path; if it cannot pass, keep the sink disabled or
run only its delivery in a detached first-party context. No such live proof
is established by this source change.

**Every response is a bare status with an empty body** — `204` accepted, `400` schema violation, `404`/`405` wrong path or method, `413` oversized, `415` wrong content type, `429` rate-limited, `500` internal. The endpoint cannot echo back what it received or what it holds, and because the client drops the batch on anything that is not 2xx, a rejection is invisible to you by construction and a server error can never surface as a client-visible failure.

`install_id` rotates client-side every 90 days (`rotated_at` in `install_id.json`), so no single identifier spans a long history. This costs longitudinal accuracy and the docs say so: **no count derived from `install_id` is a user count.** It is a lower bound on distinct machine-installs in a window, and it undercounts a returning user across a rotation.

**Turning it off deletes what was kept locally, not what was already sent.** `codewhale config set telemetry false` erases the install id, the buffer, and the dry-run records on your machine, and stops anything further. Rows already accepted by the endpoint are keyed only by a rotating random id that is now gone; first-party rows age out with the three-month window. If PostHog processing is activated, already-delivered processor records follow that project’s configured retention. The client has no remote deletion API and does not claim that local opt-out erases delivered records.

### What the owner reads back — observed active installs

The one product metric derived from this data is **observed active installs**: the number of distinct rotating anonymous install ids that produced a `session_start` event on a UTC day. That is the whole definition. It is not a count of people, not a count of accounts, and not a count of total installs — the id is per installation, rotates every 90 days, and is deleted on opt-out, so no number derived from it can be any of those things.

The exact owner command is checked into the repository, so the routine query is reviewable code rather than SQL pasted from a chat:

```sh
cd telemetry-ingest
CF_ACCOUNT_ID=... CF_API_TOKEN=... npm run report:active-installs
```

It prints the daily series, a 7-day trend over complete UTC days, the freshness of the newest ingested event, and — with the numbers, in both text and `--json` output — the coverage caveats: clients older than the telemetry feature, opted-out installs, and non-emitting environments (kill switches, fleet workers, offline shutdowns, dropped flushes) are invisible, so every count is a lower bound; and because ids rotate, week-over-week comparisons are not a retention metric. The report's read path is itself tested: the install id appears only inside an aggregate, no payload column is selected, and the wording can never drift into calling the result users. (`npm run report:dau` remains as a compatibility alias for the same report.)

### What is never collected — the public red-line list

Prompts; completions; tool arguments; diffs; patches; file contents; filenames; absolute or relative paths; git remotes; repo names; branch names; workspace commit SHAs; memory entries; chat history; API keys, tokens, cookies, or `Authorization` headers (including any boolean asserting a key exists); model ids of any kind; custom provider table names; MCP server names, commands, or URLs; approval rule text; error message bodies; panic message text; per-event timestamps; keystrokes; clipboard; screenshots; microphone; camera; location; and any third-party ad or analytics SDK — there are none in the runtime binary and none may be added.

Two named traps for the implementer. `crates/state/src/lib.rs` persists `git_sha`, `git_branch`, `git_origin_url`, `cwd`, and `path` on the threads table (`:93, :399, :653`): a payload builder that accepts a `Thread` or `ThreadMeta` and derives `Serialize` breaches the contract in one line. **Never derive `Serialize` over an existing state type** — build every telemetry struct from scratch with explicit fields. And `crates/core/src/lib.rs:1389-1398` is the one place in the tree where the word `telemetry` sits inside a JSON object next to `prompt`, `base_url`, and `has_api_key`. It is the object someone will copy. Do not.

---
