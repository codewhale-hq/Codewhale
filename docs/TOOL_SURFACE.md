# Tool surface

> 阅读简体中文版：[zh_hans/TOOL_SURFACE.md](zh_hans/TOOL_SURFACE.md)。

This document describes the current model-facing tool contract. The v0.9.1
cutover that produced it is recorded in `docs/RUNTIME_SIMPLIFICATION_DESIGN.md`;
read the workspace version from `Cargo.toml`, not from this line. The registry
remains larger than the first-turn catalog so
saved transcripts can replay and uncommon capabilities can be loaded on demand.
The model should learn one canonical name for each common operation.

Implementation sources:

- `crates/tui/src/core/engine/tool_catalog.rs` owns the eager/deferred catalog.
- `crates/tui/src/tools/registry.rs` registers canonical tools and hidden aliases.
- `crates/tui/src/tools/{file,file_tool,shell}.rs` own the small foreground
  primitive behavior and schemas; the other native tools remain searchable.
- `docs/RUNTIME_SIMPLIFICATION_DESIGN.md` records the v0.9.1 cutover and receipt.

## Default-active contract

New turns start with nine eager native names plus synthetic `tool_search`:

1. `read`
2. `write`
3. `edit`
4. `file_search`
5. `bash`
6. `todo_write`
7. `task_shell_wait`
8. `load_skill`
9. `create_goal`
10. `tool_search` (synthetic, always active)

The nine native names are `DEFAULT_ACTIVE_NATIVE_TOOLS` in
`crates/tui/src/core/engine/tool_catalog.rs`, pinned by
`default_active_contract_keeps_discovery_and_core_tools_eager`. `agent`,
`workflow`, `get_goal`, and `update_goal` are deferred by default and load
via `tool_search` (`get_goal`/`update_goal` ride eager only while a goal is
active). An authority boundary may remove `agent` at the maximum child depth,
and Plan registers only the foreground shell surface (so no `task_shell_wait`),
but route size alone must not change this core vocabulary.

The core schemas deliberately stay small (deferred rows load via
`tool_search` rather than riding every turn):

| Tool | Input | Purpose |
|---|---|---|
| `read` | `path`, optional `offset`, optional `limit` | Read a bounded file window with explicit continuation or truncation notices. |
| `write` | `path`, `content` | Create or replace a file. |
| `edit` | `path`, `edits` | Apply one or more unambiguous text replacements against one original snapshot. |
| `file_search` | `query`, optional `path`, `limit`, `extensions`, `exclude` | Find files by name, path fragment or glob and return bounded, ranked matches. Respects ignore files; `exclude` replaces default exclusions. |
| `bash` | `command`, optional `timeout`, optional `description` | Run one cancellable foreground shell command and return a bounded tail. The optional description is display-only. |
| `agent` (deferred) | delegated task and optional scope/context controls | Start or inspect focused child work. |
| `workflow` (deferred) | plan/script/source_path plus run controls | Coordinate multi-agent phases with dependencies and completion checks. |
| `todo_write` | complete replacement list of `{content, status}` items | Keep optional, agent-owned progress notes for genuinely multi-step work. |
| `create_goal` | objective plus optional budget | Start the session goal the turn works toward. |
| `get_goal` (deferred unless a goal is active) | none | Read the active goal and its progress. |
| `update_goal` (deferred unless a goal is active) | terminal status | Mark the goal complete or blocked. |
| `task_shell_wait` | `task_id`, optional `wait`, `timeout_ms`, `cancel`, `gate` | Poll or stop a background shell task. Completion still arrives as a runtime event, so use it for early output, true barriers, or gate evidence. |
| `load_skill` | optional `name`, `query` | Load a named skill's `SKILL.md` body and companion-file list; omit `name` or use `name="list"` to list eligible skills, or use `query` to search names and descriptions. |
| `tool_search` | `query`, optional matching controls | Discover policy-allowed deferred tools and add selected schemas to this conversation's toolbox. |

Mode is an authority decision, not a synonym system. Plan, Work, and Operate
use the same primitive identities. Plan centrally refuses `write`, `edit`, and
`bash`; Work and Operate still pass those calls through approval, sandbox,
trusted-path, repository-law, and managed-policy gates. Full Access changes
ordinary approval behavior but does not bypass hard safety or repository law.

`update_plan` remains registered only for saved-artifact compatibility and is
not model-visible. `tasks`, `Git`, `Run`, `Web`, `remember`, and other
specialized capabilities are searchable rather than first-turn ceremony.

On the TUI's default (calm) shell card the Bash description is the headline,
shown as written apart from stripped terminal escapes, collapsed whitespace and
a length bound, and the command sits on its own `$` row directly under it in
every state and at every width, including automatic runs. A card without a
description leads with the `$` command instead. The labeled card
(`show_tool_details = true` with `calm_mode = false`) keeps the command in its
header and the description as a muted `note:` row. The description never
replaces the command and is excluded from execution, exact approval keys and
Auto-Review input. The original description remains in the recorded tool input
and exports. Approval prompts, `/jobs`, the activity inspector, ACP and other
Runtime clients keep showing the command; this change adds no description row
to those surfaces.

## Local code execution

`code_execution` (Python) and `js_execution` (Node.js) use the same
permission-aware local launcher as workspace task gates and test runs. Ordinary
approval retains the effective session policy. To retry a denied exact call
with wider permissions, supply `sandbox_permissions` (`workspace-write` or
`danger-full-access`) and a nonempty `justification`; Ask mode requires explicit
one-shot user approval. The approved policy affects only that call.

Both tools read source from stdin, return stdout/stderr/return_code, and keep
the existing timeout and process-tree cleanup. They are not persistent REPLs;
tracebacks refer to stdin rather than a temporary script path. The separate
RLM/REPL kernel is not changed by this launcher integration.

Platform/backend limits of the shared launcher still apply: no available local
wrapper means workspace-write remains unenforced, while read-only is refused.
An external sandbox session cannot use these local interpreter tools and must
use its shell execution path. See [sandbox limits](SANDBOX.md).

## Deferred and dynamic tools

`Web` is conditional and deferred. It is discoverable through `tool_search`
only when the active policy and runtime backend permit it. Read-only
children retain its read-only search/fetch evidence path; read-only authority
does not mean "unable to research."

The durable `github`, `automation`, and `rlm` action families are also deferred
by default. `rlm` owns `open`, `eval`, `configure`, and `close` actions for a
persistent local Python session (a subprocess with a scrubbed environment, not
an OS sandbox). Inline ```` ```repl ```` fences in a reply run in the same kind of
kernel only when `code_execution` is on the turn's surface (never in Plan mode),
only when the fence opens its own line, and only after `code_execution`'s
approval under the session posture. Feature-gated native tools may be added to
the active or deferred catalog only when their implementation and host
dependencies are available.

MCP tools are dynamic. Successfully connected servers register names such as
`mcp_<server>_<tool>` from `~/.codewhale/mcp.json`; a failed or disabled server
must not be presented as available. MCP and plugin tools are deferred unless a
user explicitly names them in `[tools].always_load`.

For an unstarted configured server, an MCP-focused `tool_search` first
performs bounded discovery under the current turn's server/tool ceiling.
It searches real server-provided schemas after connection, rather than
inventing `mcp_*` definitions. A search inside `execute_tools` describes
those schemas without activating them; a direct search uses the existing
bounded activation cache. Ordinary unrelated searches leave optional servers
unstarted. The CLI's standalone MCP inspection commands own a separate pool.

### Code mode (`execute_tools`)

`execute_tools` is engine-injected, alongside the synthetic interpreter tools.
It runs a JavaScript program whose only host surface is
`await tools.call(name, args)`, and it is the default way to compose several
tool calls — MCP and plugin tools included — without round-tripping every
intermediate result through the conversation. It is hidden from Plan mode and
refused under a worker authority envelope.

- **One gate.** In a session turn every nested call is sent back to the turn
  loop and planned exactly like a direct call: deny/allow lists, preparation
  (MCP `readOnlyHint`/`destructiveHint`), `tool_call_before` hooks, ask-rules,
  Auto-Review, repository law, and the Computer Use consent refusal. MCP calls
  run through the session MCP pool. Approving the program grants nothing, so
  `execute_tools` itself is auto-approved. If the permission posture changes
  while a program runs, its remaining nested calls are refused (an approved
  call survives only an equal or broader posture, as for a direct call) and
  the model retries them under the new posture.
- **Approvals suspend the program.** A nested call that needs approval raises
  the normal approval card (named `execute_tools program call: ...`) and the
  program waits; allow resumes it, deny fails only that nested call as an
  exception the program can catch. Time spent waiting on a decision does not
  count against the program's run deadline, which is the turn's remaining
  wall clock.
- **Receipts.** The result lists every nested call with its decision (`auto`,
  `approved`, `denied`, `refused`) and status (`ok`, `failed`, `refused`,
  `in_flight`). A program that hits its deadline still returns the receipt;
  `in_flight` calls were cancelled and may have partially run. Each nested
  result is `{content, metadata, truncated}`; an oversized result keeps that
  shape, and `truncated` names the original size and the spillover file with
  the full output.
- **Discovery without re-pinning.** Inside a program,
  `tools.call('tool_search', {query})` returns matching deferred tools with
  their input schemas and does not activate them, so the request's tool array
  and the session-pinned prefix do not change.
- **Stays direct:** `agent`, `workflow`, `request_user_input`, nested
  `execute_tools`, interactive shells, sandbox escalation, Computer Use
  consent and scripts, and MCP sign-in (`mcp_<server>_authenticate`).

Code mode is on by default (`[features] code_mode = true`), which makes
`execute_tools` eager from the first request; direct tools and `tool_search`
stay available either way. Set `code_mode = false` (or run with
`--disable code_mode`) to go back to deferring `execute_tools` behind
`tool_search`. The flag is session configuration, so the prompt prefix stays
stable within a session.

Sub-agents get `execute_tools` on the same terms, unless their role is limited
to the read-only evidence tools or an explicit tool list leaves it out. A
child's nested call is planned by the child's own gate, so its grant and its
approval path apply as for its direct calls, and it is admitted at execution
against the child's write-scope claim and live peers. A child's program also
stops at the child's tool timeout (time waiting on approvals is not counted)
or its work deadline, and still returns the receipt. A program in which no
nested call ran does not count as progress for the child's permission-denial
guard, but the guard does not yet count a refused nested call as a denial
either; only the child's step and time budgets end a loop of denied calls
retried inside programs. Without an engine turn
(a direct unit call), a program keeps the conservative profile: read-only,
auto-approved native calls only, no MCP.

### Conversation toolbox cache

A successful search activation is remembered by name for the current
conversation. The cache is bounded at eight deferred names and 16 KiB of
serialized schemas. To make room it evicts tools that were never called first
(oldest activation first), then the least recently called; a tool called in the
current or previous turn is never evicted. When such tools already fill the
cache, a search still activates its best match over the bound and holds the
rest; a later activation restores the bound once those tools go unused. Every
entry is revalidated against the current catalog and policy before it is
advertised again. A session sync clears it. The cache cannot resurrect a
removed, denied, or newly-eager tool.

When a search's activation pushes a previously activated tool out of the
cache, that search's result lists it under `evicted_tool_references`, so the
model is told the tool is no longer callable rather than finding it missing
from the next request. A held match is listed under
`unavailable_tool_references`. Both carry `tool_search_input`, an exact regex
query for that one name that loads it again. Only tools that actually left the
active set are listed; a tool that became eager stays callable and is not
reported. Known limit: a tool dropped by the per-turn revalidation (removed,
denied, or no longer deferred), or evicted to make room for a direct first call
to a deferred tool, is still dropped silently, because there is no search
result to carry the notice.

Each subagent gets its own policy-filtered deferred catalog, always-present
`tool_search`, and bounded activation cache. Forked messages and instructions
remain in context, but the child cache starts empty and discovers tools locally;
neither forked context nor a cache can become a discovery allowlist. A child can
still search every tool its own authority permits, including Web search/fetch
for read-only research roles.

## Inspect the model-client request tool payload

Run `/tools` after a model turn to inspect a bounded projection of the exact
tool field in the latest prepared model-client request. `/tools json` emits the
same evidence as bounded machine-readable JSON. Both formats open in a pager;
they are not copied into transcript history. `/tool-studio` remains a human-
command compatibility alias; it is not a model tool.

The snapshot distinguishes an absent tool field from a present empty array. It
reports the exact model-client tool JSON byte count and SHA-256 digest only when
measurement fits the one-MiB inspection bound; larger payloads stay unavailable.
Provider adapters may transform, sanitize, or omit those fields while building
a provider-specific wire body, so `/tools` marks provider delivery and the wire
payload unavailable. Capture and rendering are bounded: retained schemas,
descriptions, caller lists, catalog rows, turn IDs, and payload measurement all
carry explicit truncation, omission, or unavailable receipts. The snapshot stays
in memory only for the current session and is replaced on each prepared request.

Provider, model, approval, registry provenance, and runtime capability metadata
are not fields in the request tool schema. `/tools` therefore reports them as
unavailable instead of joining against mutable state or inferring values. Use
the separate route and permission receipts for those facts.

## Modes and permission postures

Modes and permission postures are separate controls:

- **Plan** keeps the stable primitive vocabulary but centrally refuses shell
  execution and file mutation.
- **Work** is ordinary interactive execution.
- **Operate** uses the same direct-tool authority as Work. Small work stays
  direct; multi-step delegation uses a compact Workflow plan with dependencies,
  bounded scopes, and completion evidence. Fleet manages the same sub-agents
  and roles. One bounded, independent task can use a direct agent; `followup`
  reuses that agent for continued work.
- **Ask**, **Auto-Review**, and **Full Access** control approval behavior within
  an action-capable mode. They never widen Plan into write or shell access.

See `docs/MODES.md` for the full mode and posture contract.

## Compatibility names

The model-facing contract is the lowercase core above. Saved v0.9.x
transcripts and protocol clients may still call exact hidden compatibility
names such as `File`, `Bash`, and the older single-operation file names. Those
names never enter a new model catalog or `tool_search` result.

Compatibility is execution compatibility, not fuzzy aliasing: an exact legacy
call must reach the handler for its legacy schema. It must not be rewritten
into a small lowercase primitive whose input shape is different. Unknown or
retired names still fail closed instead of guessing a destination.

Specialized native families such as `Git`, `Run`, and `Web` are not aliases for
the lowercase core. They remain real, policy-filtered deferred tools and are
loaded through `tool_search` when needed.

## Long-running work

`bash` runs one cancellable foreground command. It does not carry background,
TTY, wait, interact, or cancel action fields. Stateful process and terminal
control stays out of the shell schema: `task_shell_wait` is eager for the
background jobs `bash` starts, while interactive terminal control remains
deferred behind `tool_search`; neither enlarges the first-turn shell schema.

Use `tasks` when the work itself needs a durable lifecycle, structured gates,
artifacts, replayable timelines, or a stable task id. Large tool results should
remain behind bounded handles or artifacts instead of being copied wholesale
into the parent transcript.

## Parallel fan-out

The sub-agent capacity source of truth is
`crates/tui/src/config/subagent_limits.rs`:

- default configured concurrency: **64**;
- maximum configured concurrency: **128**;
- maximum admitted running-plus-queued work: **1024**.

These are capacity ceilings, not advice to dispatch every available slot. A
manager should use the smallest useful fan-out, preserve a single owner for
fan-in, and verify worker receipts before reporting combined completion.

RLM child-query batching is a different, cheaper cost class. Its
`sub_query_batch` helper accepts 1–16 one-shot children inside a live `rlm`
session; it is not a substitute for tool-carrying `agent` workers.

## Human inspection: `/tools` (`/tool-studio`)

`/tools` renders a **read-only, bounded human projection** of the tool field of
the request that was prepared for one `(turn, step)`. It is not a second
registry and not an execution surface.

**The seam.** The snapshot is built in `crates/tui/src/core/engine/turn_loop.rs`
immediately after `MessageRequest` is constructed, from `request.tools` — the
same value the model client is handed. The engine resolves the surrounding
per-turn data once in `engine.rs` (`ToolSurfaceContext`: flattened registry
facts, the MCP pool's own server attribution, the engine-injected catalog names,
and the resolved model client's receipt) and passes it as plain data, so the
per-step seam never re-locks the MCP pool or holds a tool object.

**Turn and step identity.** The tool set can differ between steps of a turn, so
each snapshot is stamped with turn id and step and each seam emits its own. The
TUI keeps only the latest (`SessionState.last_tool_request_snapshot`). Before
the first seam there is no snapshot and `/tools` says so rather than rebuilding
a registry in the UI.

Two kinds of fact are kept apart:

- **Wire facts** come from the prepared request: name, description, schema,
  `defer_loading` / `strict` / `allowed_callers` / `cache_control`, byte
  accounting, and the catalog digest.
- **Surface facts** come from the `ToolSurfaceContext`: provenance
  (`builtin` / `plugin` / `mcp` / `synthetic` / `unknown`), MCP server identity,
  declared capabilities, declared approval requirement, and model visibility.

Contract:

- **One digest.** `active_tool_catalog_sha256`
  (`crates/tui/src/core/engine/preview.rs`) is the single definition of the
  active-tool-catalog hash. The request manifest publishes it as
  `ToolSurfaceFacts::active_tool_catalog_sha256` and `/tools` reports the same
  value for the same prepared request; neither surface keeps a hash of its own.
- **Nothing is guessed.** MCP server identity is shown only when the real pool
  attributed that exact model tool name. `McpPool::mcp_model_tool_name` is the
  single definition shared by the model catalog and the human attribution, and
  an ambiguous name (two servers colliding on one model name) resolves to no
  server. Synthetic provenance comes from
  `default_synthetic_catalog_tool_names`, which is asserted against the engine's
  own `is_synthetic_catalog_tool` predicate. A transmitted tool with no registry
  entry reports `capabilities: unknown`, never "none".
- **Provider availability follows the resolved client.** It comes from
  `Engine::tool_surface_provider_receipt`, never from "a tool registry exists".
  With no client the receipt is `unavailable` even when the registry is full.
- **Unknown shrinks, it does not vanish.** `unavailable_for_this_request` always
  contains `provider_wire_payload`: nothing on this path observes what the
  provider adapter finally transmits. It additionally contains `provider` and
  `model` without a resolved client, and `provenance` / `capabilities` /
  `approval` when no surface context was captured.
- **Absent stays distinct from empty.** A request with no tools field is not a
  request with an empty tools array; an unresolved field is `unknown` with a
  reason, not a default.
- **Bounded.** Rendering is capped by tool count (32), name, description, schema
  bytes, allowed-caller count, and a payload measurement bound, each with an
  explicit truncation or omission receipt. Registered tools that this request
  does *not* carry are reported as a bounded name list plus an exact count
  rather than expanding the projection.
- **Inert.** The snapshot lives beside the transcript, never in
  `session.messages`, so it cannot enter a model request or perturb the
  provider's prefix cache. It never executes a tool, never reads credentials,
  never reorders the catalog, and is never registered as a model-callable tool.
- **Delivery is never claimed.** The capture happens before connection setup, so
  `delivery_status` stays `unknown`.

## Release verification

Do not infer the public surface from handler function names. Verify the model
catalog and alias visibility at the exact candidate SHA:

```bash
python3 scripts/measure-runtime-contract.py
cargo test -p codewhale-tui --lib --locked core::engine::tests::default_active_contract_keeps_discovery_and_core_tools_eager -- --exact
cargo test -p codewhale-tui --lib --locked tools::file_tool::tests::primitive_schemas_are_separate_and_small_contract_shaped -- --exact
cargo test -p codewhale-tui --lib --locked tools::shell::tests::lowercase_bash_schema_is_small_contract -- --exact
cargo test --locked -p codewhale-tui --lib core::engine::tests::print_mode_tool_catalog_metrics -- --ignored --exact --nocapture
```

Check the test names against the source before trusting a green run: `cargo test`
exits 0 with "0 passed; N filtered out" when a filter matches nothing, so a
misspelled filter is indistinguishable from a pass. Each `--exact` command
above must report `1 passed` (the ignored metrics test reports `1 passed`
only because `--ignored` selects it); `0 passed` means the filter matched
nothing and the check did not run.

The provider-free receipt must report the ten default-active names listed
above.
above. A separate repository-wide tool count may include deferred, dynamic,
feature-gated, and compatibility-only registrations; it is not the number of
tools placed in the first-turn model catalog.


### Source navigation receipts

Source reads may include a bounded pod/symbol/link footer. Edits retain their unified diff in metadata and can name affected declarations and confirmed same-directory callers. File-name search and grep report matched directory pods; project maps can report bounded key-file symbols. Sounding content uses the same denied-read and credential exclusions plus protected regular-file handles. Caller content scans at most32 deterministic candidates at256KiB each; candidates outside that budget are reported as unscanned, never as additional known callers. Key-file content is capped at8MiB. Enumeration itself and platform I/O latency are not wall-clock bounded.
