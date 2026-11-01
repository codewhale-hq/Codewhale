# `/preview-request` — see the outbound request without sending it

`/preview-request` renders a **request manifest**: a typed, redacted
description of the request the next **primary agent turn** would send. It never
sends that request, never appends to the conversation, and never writes engine,
session, or Work state. `/dryrun` and `/preview_request` are compatibility
aliases.

It is a human command. There is deliberately no model-visible tool for it.

```
/preview-request                      # session facts; route/body reported unavailable
/preview-request json                 # same manifest as JSON
/preview-request --prompt <text>      # the next turn for that prompt
/preview-request json --prompt <text> # both
/preview-request base-prompt          # exact base layer only; no runtime additions
```

### Argument grammar

```
args   := flag* [ "--prompt" <whitespace> prompt ]
flag   := json | --json | manifest | --manifest | prompt | base-prompt | --base-prompt
prompt := every remaining byte, verbatim
```

**Flags come before `--prompt`, which is terminal.** Everything after it is
prompt text — including a trailing `json`. `/preview-request --prompt fix it
json` previews the prompt *"fix it json"* as a human table; the JSON spelling
is `/preview-request json --prompt fix it`. There is exactly one reading of any
input, and an unknown argument before `--prompt` is rejected rather than
guessed at.

**The prompt keeps your bytes.** Repeated whitespace and newlines inside it
survive; exactly one whitespace codepoint separates `--prompt` from its text
and is consumed as syntax. Any additional leading whitespace, plus all
trailing whitespace and newlines, remains prompt data. The prompt is hashed
into the previewed body, so normalizing it would describe a request that
differs from the real one in the one field you typed.

`prompt` remains a compatibility alias for the ordinary protected manifest.
`base-prompt` / `--base-prompt` are an explicit, human-only disclosure mode:
they print exactly the effective base-prompt bytes and nothing else. They
cannot be combined with JSON or `--prompt`. Effective system text is never
printed because it can contain project instructions, skills, and memory.

## `--prompt` is necessary for an exact manifest, and not always sufficient

The next user message is **part of the request**. Without it there is no
next-turn body to describe, and under auto model routing there is not even a
route: the route is decided by the text you have not typed yet.

So the manifest is sectioned, and each section is either exact or typed-absent:

| Section | Exact when |
| --- | --- |
| `session` | always — posture, gates, base-prompt provenance, requested model/reasoning |
| `route` | `--prompt` was supplied, an active goal has not exhausted its token budget, a fixed route is selected, no `message_submit` hooks are configured, and the shared planner resolved that route |
| `tools` | the route is exact **and** the MCP tool state can be snapshotted without connecting |
| `body` | the tool surface and authoritative Work snapshot are exact **and** no runtime transform would rewrite the request first |

### Why a section stops being exact

| Typed reason | What a real turn would do that an inspection may not |
| --- | --- |
| `auto-route-unresolved-until-next-prompt` | decide the route from text you have not typed |
| `auto-route-classification-not-executed` | call the provider-backed Auto classifier; preview is strictly offline, so production must resolve it |
| `no-hypothetical-prompt-supplied` | send a message the manifest does not have |
| `message-submit-hooks-not-executed` | run mutable hooks that can rewrite or block the text — and therefore the route, tool policy, and body derived from it |
| `prompt-resolution-failed` | fail the same way on skill authority or file mentions |
| `route-plan-failed` | fail route resolution or preflight |
| `mcp-state-not-snapshottable` | connect MCP servers and discover tools this catalog does not contain |
| `runtime-transforms-before-send` | auto-compact, run context-overflow recovery, inject a background-shell completion, allow a running or terminal-undelivered child to complete, or flush pending LSP diagnostics before the first request |
| `work-state-not-snapshottable` | read the current graph-backed Work projection; preview never substitutes an asynchronously published, potentially stale To-do view |
| `goal-token-budget-exhausted` | stop the active goal before dispatch because durable token usage has reached the configured budget |
| `goal-state-not-snapshottable` | decide whether the active goal's terminal budget gate permits another request |
| `request-preparation-failed` | build no body at all |

**`mcp-state-not-snapshottable` takes the body with it.** A catalog missing its
MCP contribution is not "the same request with no MCP tools" — a real turn
connects, and may send a different tool list, a different tool region, and
therefore a different body and hashes. The body inherits the tool
section's reason rather than publishing an exact hash of a request that would
never be sent. The `route` section survives: the endpoint, dialect, and wire
model do not depend on which tools are on the request.

**Detection is read-only.** Nothing is drained, received, flushed, or
compacted to find out: eligible running and terminal-undelivered children are checked, LSP blocks
are checked for emptiness, the shell manager is checked without polling or
marking completions reported, and the compaction decision is evaluated against
the borrowed hypothetical message list with its active slop gate pinned.
Inspecting the pending state does not consume it.

Without `--prompt` the route section is unavailable even on a fixed model.
That is deliberate: a route is only reported when it was resolved by the same
planner that would send the turn, for the same next message. A route that was
"probably still the current one" is exactly the kind of almost-true fact this
command exists to avoid.

An unavailable section publishes a typed reason and **no fields**. When auto
routing is unresolved there is no `provider_id`, `route_id`, `dialect`,
`endpoint_host_class`, `endpoint_fingerprint`, `wire_model`, `billing`,
`tool_surface_budget`, or `body_sha256` anywhere in the JSON — not a `null`,
not the previous turn's value. `requested_model` reads `auto`, because that is
what you actually selected.

## What runs, and what does not

With `--prompt`, the preview executes the **deterministic part of the
production path** up to (but not including) the send:

1. The prompt is resolved into model-facing content exactly as a real submit
   does — the pending active skill it would be wrapped with (**cloned**, not
   consumed), file mentions, git mentions, and the paused-command note — with
   the same error propagation. What a submit does that an inspection may not is
   run `message_submit` hooks; when any are configured the manifest says so and
   claims nothing downstream of the text.
2. For a fixed route, that content goes through the **same shared route
   planner** (`plan_turn_route`) that `spawned_dispatch_inner` uses for a real
   turn: effective provider and model, route identity resolution, preflight,
   route limits, compaction policy, and reasoning-effort normalization. Auto
   stops before this step because the planner would call a model classifier.
3. The engine projects the planned route into a throw-away client — the same
   client construction a turn installs, without installing it.
4. It rebuilds the tool catalog and narrows it with the same planner the turn
   loop uses, composes the system prompt **for that route's model and context
   window**, and appends the hypothetical user message through the same
   constructor production uses (turn metadata, route stamp, and provenance),
   then resolves an `auto` reasoning tier against
   those messages the way the turn loop does. Production sends stored history
   and nothing else — Codewhale does not re-state the To-do list on model
   steps — so the previewed outbound message list is exactly that list, and
   one estimate over stored messages plus system covers both the manifest
   number and the overflow decision.
5. It prepares the request through `CodewhaleClient::prepare_outbound_request`
   and describes the result — unless a runtime transform would rewrite it
   first, in which case the body is typed unavailable instead.

**Nothing is installed, not even briefly.** Everything a turn installs before
building its request — the command-scoped tool gate, the effective mode and
approval posture, the policy-narrowing event, the working set with the new
message observed — is passed as a value or snapshotted onto a clone. There is
no write-then-restore: a restore is not atomic across an `await`, and it does
not survive a cancellation or a panic. Terminal continuation state is read
without mutating its counters. A regression test
asserts that config, caches, session messages, model, system prompt, working
set, provider, mode, and the MCP pool are all byte-identical afterwards.

**No outbound call can happen.** With a fixed model, planning and request
preparation are local. With Auto selected, the command reports
`auto-route-classification-not-executed` and stops before the shared planner,
because resolving the route would call a model classifier. Preview never
reads or populates the classifier response cache and never changes provider
retry or rate-limit state.

**Nothing else has a side effect.** The tool-catalog build runs in a passive
mode: it never creates the MCP pool, calls `connect_all`, reloads an MCP config
source, starts a server, spawns a sub-agent runtime task, captures a fork
snapshot, or emits a UI status event. When the connected MCP state is not
already exactly what a turn would use — no pool yet, a config source changed,
or an enabled server is not connected — the `tools` section reports
`mcp-state-not-snapshottable` instead of connecting to find out.

## Scope: primary agent turns only

This manifest describes `LlmClient::create_message` /
`create_message_stream` — the model turns the agent loop runs. It does **not**
describe Codewhale's auxiliary provider calls, which have their own shapes:

| Auxiliary call | Status |
| --- | --- |
| Chat-dialect translation (`translate`) | **Not** on the prepared seam: it builds a small fixed body (no tools, temperature 0.1) directly. Out of scope. |
| Anthropic/Responses-dialect translation | Routed through `prepare_outbound_request` to avoid a second builder, but still an auxiliary call and still out of scope for this manifest. |
| FIM completion, speech, provider-native search, `/models` listing | Separate endpoints and bodies. Out of scope. |
| Auto-router classifier | A separate small turn on the router route. Out of scope and never executed by preview. |

Any claim that "every outbound request" goes through the previewed seam would
be false, and this document does not make one.

## Where the numbers come from

**The prepared outbound request.** Every primary model turn reaches the wire
through `CodewhaleClient::prepare_outbound_request`, which returns a
`PreparedOutboundRequest`: dialect, endpoint identity, canonical wire model,
the final body, and a reasoning receipt. Production dispatch sends that value;
the preview describes it. There is no second body builder.

Parity tests do not feed a captured logical request back through that builder.
They run a real production turn against an HTTP mock, parse the first body the
server actually received, canonicalize those captured bytes independently,
and compare that hash with preview. The coverage includes translated prompt
context, paused-command detach, and native Anthropic Messages shaping.

Preparation runs the full production sequence: tool-history repair and
model-bound secret redaction, protocol binding and route model re-resolution,
the dialect's own body builder with every provider-specific sanitizer and
reasoning shaper, and exact endpoint resolution.

Every production dialect is preserved end to end — nothing is projected
through Chat Completions:

| Dialect | Routes |
| --- | --- |
| `chat-completions` | DeepSeek, Moonshot/Kimi (including the Kimi Code K3 nested `thinking.effort` shape and the direct K3 fixed-sampling shape), Z.ai, xAI, OpenRouter, vLLM/Ollama/SGLang, OpenCode Zen chat routes, custom compatible endpoints |
| `anthropic-messages` | Anthropic, DeepSeek Messages, MiniMax Messages, OpenModel |
| `openai-responses` | OpenAI Codex (ChatGPT backend path), OpenCode Zen responses routes |

The manifest reports the dialect *and* the route shape
(`standard`, `deepseek-beta-strict-tools`, `kimi-code-k3`,
`direct-moonshot-k3`, `codex-responses`, `opencode-zen`,
`custom-compatible`), so you can see which builder branch actually ran.

**The engine.** The manifest is built by the engine, not the command layer,
because only the engine can rebuild the exact next-turn tool catalog, active
subset, gates, permission posture, and tool choice. The session's last tool
catalog is never consulted — it is one turn stale and stores the
pre-activation catalog.

## Streaming is reported as a wire fact

`caller_entrypoint` says which transport entry point was described
(`streaming` / `blocking`). `body_stream_field` says what the **body** says,
read off the finished JSON:

- Chat Completions streaming → `true`; Chat blocking → the field is absent
  (`null`), because the blocking body never carries it.
- Anthropic Messages → mirrors the caller.
- OpenAI Responses → **always `true`, including on the blocking entry point**,
  which opens an SSE stream and folds it into one response.

The manifest describes the body field exactly rather than inferring it from the
caller, so the Responses blocking case cannot be misreported as a non-streaming
request.

`tool_choice` is likewise read from the finished provider body, not the
logical request: Anthropic may carry an object, Responses carries its mapped
string, and DeepSeek thinking requests omit the field entirely.

## What the manifest tells you

| Section | Fields |
| --- | --- |
| `session` | exact primary-agent role/lane/Fleet non-assignment, requested model (`auto` when auto), routing mode, requested reasoning, whether a hypothetical prompt was supplied, mode, approval posture, allow/deny gate sizes, base-prompt origin + bytes + SHA-256 |
| `route` | provider id + display name, named route id, typed routing source, dialect, route shape, safe endpoint host class/digest, endpoint fingerprint, **wire model**, caller entrypoint, body `stream` field, context ceiling + source (`configured`, `provider-reported`, static floor, catalog, or fallback), route input/output limits or `unknown`, typed billing |
| `tools` | active count, catalog / deferred counts, logical catalog SHA-256, surface budget, Standard-vs-Full collapse, MCP servers and MCP tools |
| `body` | reasoning resolution + wire control keys + wire effort **and the key path it came from**, `tool_choice`, system-prompt assembly + effective canonical JSON bytes/SHA-256, body/system/tool-schema/message/tool-result/attachment/framing canonical JSON sizes, per-class estimates, exact input-budget ceiling and headroom, literal wire output cap or `unknown`, provider-reported usage explicitly unavailable because no request ran, **whole-body SHA-256**, wire tool-schema SHA-256, local system/tools component SHA-256 |

Counts and estimates are extracted **dialect-aware**: a Responses body's
`instructions`/`input`, an Anthropic body's `system`/`messages`, and a Chat
body's inline `system`-role message are each read from where that dialect
actually keeps them.

### The byte classes are an exact accounting, not byte slices

`system + tool_schemas + messages + framing == body_canonical_json_bytes`, exactly, in
every dialect and on both entry points. The first three are the canonical
serializations of selected JSON values; they are not four disjoint ranges
borrowed from the body buffer. **`framing` is the algebraic remainder** after
those selected values. It includes every other top-level field and whatever
JSON structure was not already counted inside a selected array value. An
invariant test asserts the sum identity, and mutation tests check the intended
attribution. Do not use these counts to reconstruct the request bytes.

`tool_result` and `attachment` bytes are *subsets* of the message bytes,
reported for attribution and never added again.

### Headroom is measured against the input budget

`estimated_input_headroom_tokens` subtracts production's conservative
messages-plus-system estimate from this route's
**input budget ceiling** — `context_input_budget_for_route`, the same seam the
turn loop checks before it sends, which is the context window minus the output
reservation and the safety headroom. It is not the raw `context_limit_tokens`:
subtracting input from a window the route also has to fit its response into
reports headroom the turn does not have. The value goes negative when the
request would not fit, rather than clamping to zero and reading as "it fits" —
and when it does, the body is reported unavailable, because the turn loop would
run context-overflow recovery and send something else.

This production gate is distinct from the manifest's independent conservative
estimate over canonical JSON body bytes. The latter remains useful for
provider-body attribution, but it does not decide overflow or headroom.

The manifest publishes that exact ceiling as `input_budget_ceiling_tokens`.
It also keeps three different limit facts separate: the context ceiling and
its resolver source, optional route/offering input and output limits, and the
output cap literally serialized on the wire. If a route or dialect supplies
no such fact, the value is `unknown`; preview never fabricates one from a
neighboring model or an installed route.

### Reasoning controls are read wherever the dialect puts them

`reasoning_wire_effort` reads the flat `reasoning_effort`, Kimi Code's nested
`thinking.effort`, the Responses `reasoning.effort`, and the Anthropic
`output_config.effort`, and `reasoning_wire_effort_source` names which one it
came from (a compile-time constant, never a key lifted out of the body).
Reporting only the flat key made the routes that think hardest read as "no
effort sent".

`reasoning_resolution` distinguishes an `explicit` user selection from a
`route-default` control the user never asked for, and reports
`not-applicable` when the body asks for no reasoning at all. A Responses
`include` field *discloses* reasoning output rather than requesting a tier, so
`include` alone is never reported as a reasoning request.

## The whole-body hash

`body_sha256` covers the complete canonicalized wire body, not a prefix. It
changes when any of these change: max-token fields, `tool_choice`, nested
reasoning controls, provider-transformed tool schemas, attachments, stream
options, sampling parameters, or any message — including the appended
hypothetical prompt. Canonicalization sorts object keys, so builder insertion
order alone does not move the hash. Any real input change does, including
date/working-set/git metadata, runtime injections, tool discovery, prompt
settings, or routing.

`tools.active_tool_catalog_sha256` is a separate, stable hash over the current
active tool catalog **before dialect shaping** (name, description, canonical
logical schema, in order). It moves on membership, ordering, and logical schema
changes. It is a catalog identity, not a wire fact: two routes can agree here
and still send different bytes, because each dialect transforms schemas its own
way and strict mode sanitizes them further.

`body.tool_schema_wire_sha256` is the hash of the `tools` region **as the
provider receives it**. `body.local_system_tools_component_sha256` combines
that digest with the final wire system-region digest as a local comparison
fingerprint. It is not a provider cache key, does not claim those regions are
adjacent, and carries no route-specific cache-semantics guarantee. It is
omitted when the tool surface is not exactly known.

## Disclosure boundary

The manifest is a fixed set of counts, hashes, enums, and short provenance
labels. It has no field that can hold free-form request text. It cannot
contain:

- the system prompt, project instructions, memory, or skill content;
- message content, tool-result bodies, or attachment payloads;
- credentials, `Authorization` headers, or query strings;
- URL paths (which can themselves carry a deployment secret);
- absolute workspace or home paths.

**Identifiers are not trusted either.** A custom route id and a model id are
user-authored text that can be an absolute path, a URL, a URL path, or a
deployment id that is itself a credential. Every such value crosses an
allowlist boundary (`crate::safe_label`) before it is printed: an
generic identifier is published verbatim only when it contains no slash. A
slash-bearing model id additionally has to match an exact entry in the active
local model catalog; a vendor-looking prefix is not sufficient. Anything else
is replaced by a stable `sha256:<12 hex>` fingerprint. Two previews of the same
hostile id still compare equal; the id itself is never shown.

**Error text is not trusted either, and is not scrubbed — it is
allowlisted.** Preflight, MCP, prompt-resolution, and request-preparation
failures all interpolate host text, and that text routinely carries a route id,
a server name in quotes, an endpoint whose *path* is the secret, or a raw
credential. Every whitespace-separated token has to earn its place:

- a token containing a control character is dropped;
- a URL keeps only `scheme://host[:port]`, and only when both are themselves
  ordinary — path, query, fragment, and userinfo are never published, and a
  token-shaped "host" makes the whole token opaque rather than half-published;
- anything path-shaped (POSIX absolute, `~/`, Windows drive, or containing a
  backslash) collapses to `<path-redacted>`;
- a token carrying `"`, `'`, or a backtick is replaced wholesale — quoted spans
  are where hostile identifiers hide;
- everything else must be a short ordinary word (ASCII alphanumerics plus
  `-`, `_`, `.`, bounded in length, rejected if it looks token- or key-shaped),
  with only sentence punctuation allowed at its edges.

Anything else becomes `<redacted>`, runs of redactions collapse, and the result
is truncated. Ordinary diagnostic sentences survive intact; hostile ones become
a generic shape.

Endpoints are published two ways only: a bounded host class (`http loopback`
or `https remote sha256:<12 hex>`) and a SHA-256 fingerprint of the full URL
for "same endpoint?" comparisons. A remote authority is always digested — it
may be a credential-shaped tenant subdomain — and paths, IDNs, userinfo,
queries, and fragments are never shown.

**This is an inspectability slice, not a request-body export.** A regression
test asserts that no manifest field carries a serialized message array or tool
schema.

## Estimates are estimates

Every token number is an **offline estimate** (~4 bytes/token plus a 5%
conservative margin), never a provider-authoritative count. Use them to
compare requests to each other, not to predict a bill. Byte counts, hashes,
and counts are exact.

Tool-result and attachment estimates are *subsets* of the message estimate and
are reported for attribution; they are not added again into the total.

## Base-prompt provenance (#3928)

The protected manifest distinguishes three things without printing effective
system text. The explicit `/preview-request base-prompt` mode separately prints
only the exact effective base-prompt bytes:

- **Origin** — where the base-prompt bytes came from:
  - `bundled in this codewhale-engine build (BASE_PROMPT, compiled in)`
  - `config-directory override installed at startup (prompts/constitution.md,
    opt-in enabled)`
- **Assembly** — how the effective prompt was built on top of that base:
  `base prompt only`, `base prompt + configured static layers`, or
  `base prompt + configured layers + runtime/session additions`.
- **Effective hash** — the SHA-256 of the system region of the *prepared
  request*: the prompt in its final wire form, not an independently
  recomposed string.

In a live session the assembly is normally
`base prompt + configured layers + runtime/session additions`, because the
environment block, project context, skills, and memory are appended after the
constitution. Codewhale does not claim the configured constitution is the
effective base prompt, and no diagnostic cites a source-tree path that does not
exist on an installed binary.

## Tool-surface labels

`standard_and_full_surfaces_collapsed` is **derived, not asserted**: the
surface shaper is run over the actual current catalog under both the Standard
and the Full budget and the results are compared. Today it reports `true`
— the two budgets produce the same catalog — and the manifest says so in
plain words rather than implying a difference. The day the shaper narrows
Standard differently from Full, the field flips on its own with no copy edit.

Any benchmark claim about one surface offering "more tools" has to show a
different `active_tool_catalog_sha256` first.

## Comparing routes before you spend anything

`/preview-request` makes a provider-free fixed-route A/B possible: switch route,
preview with the same prompt, compare.

1. Select the route (`/model`, `/provider`, or your profile) — do not send a
   turn.
2. Run `/preview-request json --prompt "<the same text each time>"` and save
   the output, e.g. `glm-5.2.json`.
3. Repeat for each route.
4. Diff the manifests.

What to read in the diff:

- **`route.dialect` / `route.route_shape`** — two routes on different dialects
  are sending structurally different requests, not the same request to a
  different host.
- **`route.wire_model`** — the id that actually goes on the wire. Router
  entries frequently differ from the label you selected.
- **`tools.active_tool_count` / `body.tool_schema_wire_sha256`** — identical
  wire hashes mean identical tool bytes, whatever the surface label says. Use
  `tools.active_tool_catalog_sha256` only to compare *logical* catalogs; two
  dialects can agree there and still send different schemas.
- **`body.estimates.tool_schemas` vs `route.context_limit_tokens`** — the fixed
  overhead each route pays before the conversation starts, against the window
  it has.
- **`body.reasoning_wire_control_keys` / `body.reasoning_wire_effort` /
  `body.reasoning_resolution`** — whether the route is actually being asked to
  think, in which dialect, and whether that came from you or from auto
  routing. Two routes with different effective effort are not comparable.
- **`body.body_sha256`** — the whole request. If it is unchanged, nothing about
  the outbound bytes changed.
- **`body.local_system_tools_component_sha256`** — whether the two locally
  measured wire components changed. It does not prove provider cache reuse or
  invalidation.
- **`route.billing`** — subscription quota and metered API routes are not
  cost-comparable, and `unknown` means cost reporting fails closed.

Each manifest is exact for the snapshot it describes. Repeated previews are
identical only when every contributing input is identical: route, current
date, git and working-set metadata, prompt settings, tool/MCP
state, session history, and pending runtime transforms. Previewing does not
mutate session history or the response cache, but no cross-call byte-stability
claim is made. Auto is intentionally unavailable and cannot be used for this
comparison until production resolves a concrete route.

`schema_version` is bumped whenever a field is renamed or removed, so scripted
consumers can detect an incompatible manifest instead of silently reading
`null`. The current shape is `9`: v8's `work-state-not-snapshottable`
unavailable reason is gone, because no request carries a To-do block for a
snapshot to fail on. The active goal token-budget terminal gate remains an
explicit fail-closed dependency of an exact outbound request.

## What is still approximate

Auto routing is not approximated: its provider-backed classifier is never run
by preview, and the route-dependent sections are typed unavailable instead.

Working-set drift used to be listed here. It no longer applies: a real submit
calls `working_set.observe_user_message` before it builds `<turn_meta>`, and
the preview now performs that same observation on a **clone** of the working
set and builds the block from that snapshot. Same bytes, no session write.

Everything else that used to be "approximate" is now typed. If a runtime
transform would change the request, the body section says so and publishes no
bytes at all, rather than publishing numbers that are nearly right.

## Credit

The `dryrun` concept — preview the next request from the real request-building
seam rather than a hand-rolled summary — was harvested from PR #1099 by
[@GTC2080](https://github.com/GTC2080) (TaoMu). No code from that PR is
reused; the implementation here is written against Codewhale's current
multi-dialect client.
