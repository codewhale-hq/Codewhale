# Prompt-cache stability (the pinned prefix)

> 阅读简体中文版：[zh_hans/CACHE.md](zh_hans/CACHE.md)。

Provider prompt caches (DeepSeek KV cache, Anthropic `cache_control`) only pay
off when the **byte prefix** of a request matches the previous one: the system
prompt, then the tool catalog, then `messages[0..n-1]`. Any change to those
bytes invalidates the cache for every token after the first difference.

## The invariant

**After session start, the system prompt and tool catalog are frozen bytes.
History only grows. A cache miss is allowed only when we can name why.**

Concretely:

- The **header** (system prompt + tools) is composed once at session start and
  re-composed **only** on an explicit, logged header-change op. The tool loop
  performs **no** mid-loop system-prompt refresh, so an agent writing a file
  (which changes the project-context pack, a directory listing, a skills scan)
  cannot move the pinned prefix under the model's feet mid-turn.
- **History only grows.** Volatile facts the model must see (LSP diagnostics,
  steer input, subagent completions) are appended to the message list, never
  spliced into the frozen prefix. Workspace drift is delivered the same way:
  at the start of each **new user turn** (never mid-tool-loop) the engine
  recomposes the volatile contributors and, if anything differs from what the
  model last saw, appends **one** `<context_update>` user-role message with a
  bounded `+`/`-` line delta (new files in the project pack, edited AGENTS.md
  lines, added skills, memory entries, goal text) *before* the user's message.
  The header bytes stay pinned; the update is a normal append, so the prefix
  still extends. The pinned system prompt tells the model once that updates
  arrive this way. Each delta is delivered exactly once (`/cache stats` shows
  `Context updates: N`).
- Every miss is **attributable**. `PrefixStabilityManager` (`prefix_cache.rs`)
  records each change with a reason and reports it through `/cache stats`.

## What counts as a declared header change

These re-pin the prefix under a logged `change:<what>` reason (an expected,
one-request miss):

| Op | Reason |
| --- | --- |
| `/model` (SetModel) | `change:model` |
| Mode change (agent/plan/operate/yolo) | `change:mode` |
| Goal set / pause / resume / clear / status | `change:goal` |
| Mid-turn tool-surface change (deferred-tool admission/eviction, tool-search activation, runtime MCP tool arrival) | `change:tool_surface` |
| Session sync / restore (SyncSession) | `resume` |
| Session construction | `initial` |

History resets that legitimately invalidate the tail (not the header) are
logged as `reset:<what>` — `reset:compaction`, `reset:clear`.

Anything else that changes the header bytes with **no** declared reason is
**drift**: it is logged as `drift:<component>`, the original pin is **kept**
(so the same undeclared prefix keeps counting as a miss instead of quietly
becoming the new baseline), and `/cache stats` shows a `WARNING`. After the
mid-loop-refresh removal, drift should stay at zero in normal operation; a
non-zero drift count is a real bug to investigate.

## Attribution vs. the old behavior

Two earlier behaviors are rejected, matching the DeepSeek Harness design:

- **Detect-and-report + re-pin on drift.** The manager used to re-pin to the
  new prefix on every change, so `/cache stats` looked "stable" again after one
  bad step while the provider cache was already dead. It now keeps the original
  pin on undeclared drift.
- **Recompose the system prompt from disk on every tool step.** The turn loop
  used to call `refresh_system_prompt()` before every model request, including
  mid-tool-loop. That is removed. Header refreshes happen only at the declared
  edges above.

Tool-result redaction (`prepare_model_bound_request`) is content-preserving
when no secret is configured (the common case), so it does not move the prefix.
When a configured secret appears in a tool result, redacting it is a security
requirement that correctly overrides cache stability for that one message.

## Verifying the fix

`/cache stats` reports prefix stability, the pin reason, the last miss reason,
the undeclared-drift count, and the aggregate provider cache hit rate. In a
coding session, expect the first turn to be a write and every later step —
including steps after the agent writes files — to hit.

### Live end-to-end check (manual, key-gated)

With a real `DEEPSEEK_API_KEY`, run a session that makes the agent take at
least three tool steps in one turn, then open `/cache inspect`. Every request
after the first should report `prompt_cache_hit_tokens > 0`; the base static
prefix hash and the tool-catalog hash must not move between steps. If the hit
drops mid-turn, the pin reason / drift count name the cause.

## KV-cache effect note (for contributors)

Any new contributor to the session context must state its **KV-cache effect**:
does it belong in the frozen prefix (system + tools) or in append-only history?
Never splice a volatile fact (time, an instruction edit, a skill-catalog
change, a project-file change) into the prefix — append it as a user-role
message instead. A later request must be `previous ⊕ suffix` unless a logged
header change or a history reset explains the difference.

## Deferred: full reconstructability (Layer 3)

DeepSeek Harness derives every request from an append-only session log via a
pure `deriveMessages()` projection, so prefix-extension is emergent rather than
managed. Codewhale now pins the header and delivers drift as `<context_update>`
appends; the remaining step is to make the session log the single source of
truth with a pure projection (and to persist the context-update baseline with
it). That is a follow-up lane, not part of this change.


### Fork prefix inheritance

A first-level fork may reuse a parent header only when the actual captured client declares Chat Completions, its model/provider/route key and endpoint fingerprint match, the actual opaque route receipt (including credential generation and upstream vendor) matches, and a positive cache-hit receipt reports parent cache use. Unknown actual route receipts or other wire protocols, resume forks and grandchildren start cold. Only already-active policy-filtered child tools participate; deferred parent tools are not automatically activated. The child preserves every policy-filtered active tool; inherited tool order never grants a tool or replaces the current eager toolbox. Role/task framing remains in the child history, and the actual selected header is recorded in its protected transcript. The parent header is frozen when the child runtime is captured at spawn. A route replacement retires the inherited header before the next request. Directory soundings and skill bodies stay in tool-result history. This is source behavior; provider cache savings require a live receipt.


### Fork trial diagnostics

`CODEWHALE_FORK_INHERIT=off` disables fork inheritance for the process; inheritance otherwise remains enabled behind the route, protocol and hot-parent guards above. Set `CODEWHALE_FORK_TRIAL=1` before starting Codewhale to emit default-off JSON diagnostics to stderr: a classified first-fork decision and the child's first settled model response. The response reports provider input/cache-hit/cache-miss fields with missing values kept null, whether usage was reported and whether the stream failed. Component sizes measure the provider-neutral request serialization, not final HTTP wire bytes or tokens. The diagnostic contains no prompt, message, tool schema or credential text. Unknown or alternate-model wire clients remain cold and do not produce a classified decision record. These observations do not prove cache savings; compare actual provider receipts under the same binary and route. Both environment settings are process-pinned.

A matched fork may retain parent-specific header instructions while the resolved child role, task and skill instructions follow in a recorded system-role context message. Concrete routing and permissions remain independently enforced by the client and ChildAuthority. Instruction interaction quality and cache savings need live qualification; the matching gate does not prove them.
