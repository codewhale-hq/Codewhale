# The Codewhale Agent Runtime — one durable substrate, familiar launchers

> 阅读简体中文版：[zh_hans/AGENT_RUNTIME.md](zh_hans/AGENT_RUNTIME.md)

This document explains how sub-agents, the headless `exec` path, Agent Fleet,
and Runtime relate. These concepts had drifted into *two* parallel "worker"
systems. The fix is to make the **Runtime worker run** the durable execution
primitive: fleet owns Agent identity, membership, and selection; Runtime owns
execution, authority, and lifecycle. "Sub-agent" remains useful product
vocabulary for a nested role, but it must not imply a separate execution
substrate with weaker lifecycle semantics. It also answers the open direction
question in #2972 ("how much Claude Code convergence is right?").

## The core idea

There is exactly **one** thing that runs detached Agent work: a **headless
Runtime worker** with a durable execution lifecycle. It is a model loop with
the full, authority-gated tool surface that can, in turn, delegate child work
through the same lifecycle. Everything else is a way to select, launch, or
observe that one Runtime.

```
                 ┌──────────────────────────────────────┐
                 │          headless Runtime             │
                 │ execution · authority · lifecycle     │
                 │       can spawn child workers         │
                 └───────────────┬──────────────────────┘
                                 │
                 ┌───────────────┴──────────────────────┐
                 │      one durable execution substrate  │
                 └───────┬───────────────┬──────────────┘
                         │ launches      │ launches
          ┌──────────────┴──────┐  ┌─────┴────────────────┐  ┌──────────────────────┐
          │       TUI turn       │  │   `codewhale exec`  │  │      Agent fleet        │
          │ interactive, in-proc │  │    headless CLI     │  │ identity · membership │
          │                      │  │ full tools · stream │  │     · selection       │
          └─────────────────────┘  └──────────────────────┘  └──────────┬───────────┘
                                                                          │
                                                                          └─ selects a Runtime worker
```

- A **sub-agent** is the user-facing name for a *nested assignment* with a role
  (`explore`, `reviewer`, `implement`, `test`, ... — see the canonical seven in
  `docs/SUBAGENTS.md`). It should be backed by
  the same Runtime worker lifecycle used for a fleet-selected Agent. `agent` is
  the model-facing launcher, not a second runtime.
- **`codewhale exec`** is the headless front door: usable by anyone at any time
  (CI, scripts, another agent), full tools, emits a `stream-json` event stream,
  and can spawn sub-agents. It is *the* runtime with a CLI on it.
- A **fleet-selected Agent** executes as a Runtime `codewhale exec` run. fleet
  supplies identity, membership, and selection; it does not re-implement
  execution. Runtime owns the durable ledger, scheduling/leasing/retry,
  authority, local or SSH transport, and terminal lifecycle.

So "fleet vs sub-agent" is not a choice between execution substrates. fleet
answers **who** is eligible and selected, Runtime answers **how and where** the
authorized work executes, and sub-agent remains the role/UX vocabulary for a
nested assignment.

## The cutover rule

If a detached `agent` child can fail on a one-off provider timeout with no
retry while an equivalent Runtime worker would retry and preserve ledger
evidence, then the cutover is incomplete. Treat that as a Codewhale Runtime
gap, not as normal "sub-agent behavior".

The compatibility `agent` runtime now retries transient provider header,
stream, and timeout failures with backoff before marking a worker interrupted;
when retries are exhausted it preserves a checkpoint and returns a continuation
handle. The remaining convergence work is to keep that lifecycle durable across
process restarts, remote execution, and full Runtime-ledger scheduling.

The target rule is:

- durable or long-running work goes through the Runtime worker lifecycle;
- `agent` should enqueue
  or observe a Runtime worker run instead of owning an independent
  lifecycle;
- in-process children are allowed only as a small compatibility/latency
  optimization, and they must expose the same terminal states, retry semantics,
  receipts, and inspection handles as the durable Runtime path.

In product language it is fine to say "open a sub-agent". In architecture
language that means "start a nested Runtime worker with this role", optionally
using a member selected from fleet.

## Why this shape (and why it fixes the lag)

The motivating problem: spawning many in-process sub-agents made the TUI lag,
because each child cloned a heavy runtime and rebuilt the whole tool registry,
*and* the TUI rendered a full card/transcript per child.

Surveying Claude Code, Codex, and Kimi, the thing that keeps an orchestrator
light at high fanout is **not** a process boundary — all three run sub-agents
in-process. It is **isolation + a compact event stream**:

- a child's transcript **never** flows back into the parent — the parent gets a
  result summary and a small lifecycle event stream;
- the UI renders **counts** (`2 running / 3 done`), not a child session per
  worker;
- each worker's tool surface is built directly from a **role/capability
  profile**, not "build everything then filter".

"Headless" therefore means *the execution is not shaped like the UI* — it does
**not** mean fewer abilities. A headless worker keeps the full toolset and can
spawn sub-agents.

When the work also needs to be **durable** (survive the TUI closing, a laptop
sleeping) or **remote** (SSH), Runtime runs the worker out-of-process as
`codewhale exec`. fleet may supply the selected Agent identity, but Runtime
retains execution authority and lifecycle ownership. The heavy construction
then lives in another process entirely, so the orchestrator stays smooth
regardless of fanout, and the run survives restarts — the day-scale autonomy
goal of #3154.

## One recursion axis

A worker runs at `spawn_depth = 0` and may spawn children while
`spawn_depth + 1 ≤ max_spawn_depth`, so a budget of `N` affords `N` nested
delegation levels. Sub-agents and fleet-selected Runtime workers share **one**
axis, sourced from `codewhale_config`:

Runtime delegation has no default product depth cap. Explicit owner depth
settings still apply to descendants and cannot be widened by a child. Zero
blocks child delegation while leaving the root worker available. The internal
`u32` representation rejects overflow; it is not a suggested work budget.

The model-facing `agent` schema omits depth controls. Legacy `max_depth`,
`maxDepth` and `max_spawn_depth` inputs are parsed for compatibility but ignored
when a model requests a child; receipts identify that choice. Exact Fleet and
saved-runtime authority remains enforced.

Workflow IR structure is independent of child delegation. Its `max_depth`
default is zero (uncapped); positive operator values are honored. Parser and
VM stack/memory boundaries still validate documents before execution.

## Event vocabulary

The Runtime execution ledger persists the worker's own event stream rather than
a separate, simulated taxonomy. Compatibility APIs and types still expose this
through the `Fleet...` prefix. `codewhale exec --output-format stream-json`
emits
`{"type": "content" | "tool_use" | "tool_result" | "agent_spawned" |
"sandbox_denied" | "workflow_event" | "session_capture" | "service_released" |
"turn_usage" | "metadata" | "status" | "request_image_window" | "done" |
"error"}` lines, which map onto the Runtime ledger's compatibility type
`FleetWorkerEventPayload` (`RunningTool`, `WorkflowEvent`, `Running`,
`Completed`, `Failed`, …). `workflow_event` carries the typed
run/phase/task/gate receipt while a Workflow is in flight and is retained as a
typed `WorkflowEvent` in the Runtime execution ledger; the enclosing Runtime
worker still owns the terminal `done` or `error`. One vocabulary, two surfaces.

`session_capture` is emitted once, when the exec run persisted its transcript
as a saved session, and carries the recoverable id in exactly one place:

```json
{"type": "session_capture", "schema": "codewhale.exec-stream", "schema_version": 1,
 "content": "<redacted:…>", "saved_session_id": "01J…"}
```

- `saved_session_id` is the raw saved-session id, emitted only after a
  successful save. For local Fleet workers, the parent assigns a fresh ID and
  shares the Runtime's existing session directory. The executor advertises
  `FleetReceipt.saved_session_id` only when that exact ID was reported and its
  saved transcript can be loaded. A client with Runtime API access can then
  read the reply through `GET /v1/sessions/{id}`. SSH workers retain their
  excerpt and remote log, but do not advertise an unavailable local session
  link. An ID is a lookup key, not a substitute for Runtime authentication.
- `content` is the same redacted fingerprint the terminal `metadata.session_id`
  carries, so a captured `metadata` receipt stays safe to log on its own and
  the two events can still be correlated. `metadata.resume_command` therefore
  names this field (`codewhale exec --resume <session_capture.saved_session_id>`)
  rather than carrying the id itself.

The terminal `metadata` receipt also carries the worker's visible final answer:
`visible_final_answer_chars` is the real character count of the final
assistant reply, and `visible_final_answer_excerpt` is a bounded (4,000
characters, `...` when cut), secret-redacted excerpt of it, omitted when the
current turn produced no visible answer. Resumed turns never reuse an older
reply, and failed/interrupted receipts may carry partial current-turn text;
the receipt status remains authoritative. The Runtime executor reads the excerpt from
this receipt — never from the streamed `content` deltas, which are the run
thinking out loud — and attaches it to `Completed.summary` and, for a task
with no scorer and no file artifact, to the receipt notes as the task's
deliverable. Lifecycle event labels and worker inspection summaries show a
short excerpt; the event `payload` and the receipt keep the full excerpt.

`turn_usage` is the per-model-call usage receipt, emitted once per model
request (turn-step) when the provider reported usage for that call:

```json
{"type": "turn_usage", "schema": "codewhale.exec-stream", "schema_version": 1,
 "turn": 1, "input_tokens": 1200, "output_tokens": 180,
 "reasoning_tokens": 90, "prompt_cache_hit_tokens": 900,
 "prompt_cache_miss_tokens": 300, "prompt_cache_write_tokens": 0,
 "reasoning_replay_tokens": 40, "duration_ms": 1834}
```

- `turn` is the 1-based index of the model call within the exec run;
  `input_tokens`, `output_tokens`, and `duration_ms` are always present.
- Optional token fields are **omitted** when the provider does not report
  them — never emitted as null and never backfilled with zeros. Field names
  mirror the terminal `metadata` receipt: `prompt_cache_hit_tokens` is the
  provider's cache-read count (Anthropic `cache_read_input_tokens`),
  `prompt_cache_write_tokens` the cache-creation count
  (`cache_creation_input_tokens`). `reasoning_tokens` appears only for
  provider paths that report it (OpenAI-compatible
  `completion_tokens_details` / Responses `output_tokens_details`; Anthropic
  does not report a thinking-token count). `reasoning_replay_tokens` is a
  client-side estimate for DeepSeek V4 interleaved-thinking replays.
- `finish_reason` is the provider's stop reason for that step (for example
  `stop`, `tool_calls`, or `length`), omitted when the provider reported none.
- When a provider reports no usage at all for a call, the whole event is
  skipped for that call. Latency/convergence analysis should sum
  `turn_usage` events instead of inferring per-step tokens from wall time;
  the terminal `metadata` receipt still carries the cumulative totals.

## Convergence with Claude Code (#2972)

Codewhale should converge with Claude Code on **shape**, not on branding:

- **Adopt**: a headless runtime with a real CLI/SDK front door; sub-agents as
  isolated runs that return summaries (not transcripts); a compact, event-driven
  fanout projection; capability/role tool profiles; the skills ecosystem
  (#2743); structured run receipts.
- **Keep distinct**: Codewhale branding and first-class DeepSeek/GLM/MiniMax/
  multi-provider support; the local-first **Agent fleet** as the identity,
  membership, and selection layer; durable local/SSH execution and authority in
  Runtime; Workflow as the ordering overlay.
- **Do not** fork execution semantics per surface. The TUI, `agent`,
  `exec`, and the Runtime API must all drive the *same* Runtime and observe the
  *same* event stream. fleet selections are passed to that Runtime rather than
  creating a second execution path — divergence there is what produced the
  "two moving targets" this document exists to prevent.

The litmus test for any new agent surface: *does it launch and observe the one
runtime, or does it invent a second one?* Only the former is allowed.

## Historical note: what remained after v0.9.0

Archived roadmap snapshot — live state is the issue tracker, not this list.
Refreshed 2026-08-17 from a full audit of the older 0.9-era documents. Those
plans are evidence, not a second source of truth. v0.9.0 consolidated the
underwater shell, message-first Operate, permission postures, the wired
Workflow engine and durable run journal, Lane CLI/runtime, setup with
`operate_ready`, constitution rebalance, and ProviderLake/Models.dev. The
remaining work belongs to later releases:

1. **Rebrand completion** — the `deepseek`/`deepseek-tui` binary shims and
   shim release assets were removed in v0.9.0; the remaining obligation is the
   Homebrew `codewhale` formula rollout (`docs/REBRAND.md`).
2. **Operate as a value stream** — a control-board surface over the underwater
   shell (WIP, queue age, bottleneck); phase history (#4039); Workrooms Phase 2
   (#3209/#3210) as the inbox substrate;
   receipt reconciliation.
3. **Flow control** — real WIP limits and visible queues (#4015, #4016),
   reconciled with the shipped 16-concurrent/1k-run access model (#4292).
4. **fleet identity and Runtime/Workflow convergence residuals** — live
   tmux/verifier-gate dogfood closing #4175/#4177/#4178/#4179; fleet consuming
   canonical AgentProfiles and selecting members while Runtime owns execution;
   Conductor/topology (#4010, #4012) as stretch.
5. **TTC design implementation** (design doc in `codewhale-ops`) — approved and now unblocked after v0.9.0.
6. **HarnessProfile completion** — the status/UX display lane
   (`docs/rfcs/HARNESS_PROFILE_CUTLINE.md`).
7. **File decomposition, landed** — the v0.9.0-era offenders were split out:
   `main.rs` is now a thin stub and `ui.rs` has been decomposed into focused
   modules under `crates/tui/src/tui/` (~3.9k lines today; the
   `docs/rfcs/FILE_DECOMPOSITION_0_9_0.md` figures are the 0.9.0-era snapshot).
   The remaining work is the "thin TUI over core" north star tracked in
   `POST_0_9_1_SEAMS.md`.

Explicitly deferred by their own documents: external workflow memory (boundary
only), automatic harness evolution, hosted workrooms, `constitution_modules`
(needs sign-off), permission profiles (#3211, needs design), and plan-ceiling
probing (needs a product decision).

## Public launch contract for an external harness (#4641)

An external evaluation harness (for example a future Verifiers v1 built-in
harness) embeds Codewhale by launching the public `codewhale exec` front door
against an interception endpoint it owns. Codewhale owns only its **launch
contract**; the harness owns interception, traces, model-call timing, token
accounting, retries, rollout limits, and runtime orchestration. Do not add a
harness runtime, trace parser, or receipt schema to Codewhale.

A reproducible headless launch uses only existing generic surfaces:

- an explicit temporary config that names the route and the credential
  **environment variable**, never the secret itself:

  ```toml
  provider = "openai"

  [providers.openai]
  base_url = ""            # the harness fills in its interception endpoint
  model = ""               # the harness fills in the target model
  api_key_env = "VF_CODEWHALE_API_KEY"
  ```

- `CODEWHALE_HOME` set to a fresh per-run directory;
- `CODEWHALE_SECRET_BACKEND=file`;
- `CODEWHALE_MCP_CONFIG` pointing to a generated per-run MCP JSON file that
  contains only the task servers the harness supplies
  (`{"mcpServers":{"task-tools":{"url":""}}}`; the `mcpServers` alias and
  URL-based Streamable HTTP / SSE transports already exist);
- `CODEWHALE_MEMORY=false` and `CODEWHALE_TELEMETRY=false`. The 0.9.12 source
  defaults usage counting on with an opt-out. Every sealed harness explicitly
  sets the run-scoped kill switch so a test cannot collect or send from a
  fresh or reused home. Ordinary enabled sessions send aggregate counts to an endpoint
  (`https://telemetry.codewhale.net/v1/telemetry`, the shipped default) rather
  than to a local file. It is a hard floor — an explicit "off" in the
  environment beats `--telemetry true` and `telemetry = true` in config. Set
  `CODEWHALE_TELEMETRY_ENDPOINT=` (empty) instead if a harness wants an enabled
  home to keep buffering locally without contacting anything. See
  [`docs/TELEMETRY.md`](TELEMETRY.md);
- `CODEWHALE_ALLOW_INSECURE_HTTP=1` **only** when the harness supplies a
  trusted `http://` interception endpoint (container/tunnel endpoints are not
  always loopback);
- `--append-system-prompt` and `--disallowed-tools` when the caller supplies
  them.

The interception secret stays in the child environment (resolved through the
route's `api_key_env`); it is never written into argv, the route config, logs,
the `stream-json` stream, or any generated file.

The exact argument order is:

```sh
codewhale \
  --config .vf-codewhale/config.toml \
  --workspace . \
  --no-project-config \
  --skip-onboarding \
  exec \
  --auto \
  --sandbox danger-full-access \
  --output-format stream-json \
  -- "<task prompt>"
```

`--no-project-config` must appear **before** the subcommand (like
`--skip-onboarding`). The public dispatcher parses it and forwards it ahead of
the TUI subcommand; `Exec` then skips the workspace-specific
`[workspace]`/`[projects]` user-config overlay so the config surface depends
only on the explicit `--config`. `crates/tui/tests/integration/verifiers_harness_contract.rs`
is the provider-free acceptance lock for this contract.

### Future upstream checklist (out of scope here — do not run)

Actually adding Codewhale as a built-in harness lives in the external Verifiers
repository; the public, immutable Codewhale GitHub Releases with checksum
manifests it needs have existed since v0.9.1 (latest published release is
v0.9.13, published 2026-09-14; the workspace source version is 0.9.13).
That upstream change is expected to be limited to a new
`verifiers/v1/harnesses/codewhale/` package plus its test-matrix and docs
registration, with `CodewhaleHarnessConfig` pinning the target release,
`setup()` downloading and verifying the released archive, and `launch()`
writing the temporary route/MCP files above and calling `runtime.run_program(...)`.

Holdouts, explicitly **not** performed by this contract work: tagging,
publishing, or creating a Codewhale release; opening or submitting the upstream
Verifiers PR; running its credentialed E2E matrix; or claiming
runtime/architecture support before the exact released archive has run in that
upstream runtime.
