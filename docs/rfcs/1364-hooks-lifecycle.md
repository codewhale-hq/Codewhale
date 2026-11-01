# RFC: Hook Lifecycle Data Flow

**Issue:** #1364
**Status:** Implemented (all three PRs landed)
**Date:** 2026-05-28
**Reference:** the shipped contract lives in [docs/HOOKS.md](../HOOKS.md);
this RFC is kept as the design record. Where the two disagree, `docs/HOOKS.md`
is correct and this document is history.

**Scope:** everything below landed as a **TUI runtime** feature. No hook in
this RFC fires from `codewhale exec`, the CLI subcommands, the app-server /
ACP surfaces, or the `workflow` tool. The RFC's use of "Codewhale" as the
actor should be read as "the Codewhale TUI" throughout.

## 0. Implementation status

Verified against `crates/engine/src/hooks/` and its call sites:

| Item | Status | Where |
| --- | --- | --- |
| PR 1 — mutable `message_submit` | implemented | `HookExecutor::execute_message_submit_transform`, called from `dispatch_user_message_with_recovery` |
| PR 2 — `turn_end` | implemented | `HookEvent::TurnEnd`, `turn_end_payload`, fired by `execute_turn_end_observer_hook` |
| PR 3 — subagent observers | implemented | `HookEvent::SubagentSpawn` / `SubagentComplete`, fired by `execute_subagent_observer_hook` |

Two deliberate deltas from the text below, both shipped:

- **`tool_call_before` also became mutable** (#3026), which this RFC listed as a
  non-goal for PR 1. It carries its own stdout JSON contract
  (`decision` / `reason` / `updatedInput` / `additionalContext`) plus the legacy
  exit-code-`2` hard deny. `message_submit` is therefore no longer the only
  event with a stdout contract; `shell_env`'s `KEY=VALUE` contract also predates
  both.
- **`turn_end` observers are not gated by `continue_on_error`.** The structured
  observer path runs every matching hook regardless, because an observer that
  fails must not suppress the observers behind it.

Still unimplemented from the text below: nothing in scope. `stop_hook_active` is
emitted as `false` and the re-entry protection it reserves room for has not been
built.

Four contract points hardened after this RFC, all documented in
`docs/HOOKS.md`:

- **One session identity.** The hook session id is minted once per TUI launch
  and survives a workspace switch or a trust decision that adds project hooks.
  `tool_call_before` reports that id rather than the engine's own session id,
  and `mode_change` carries it at all (it previously carried no session id).
- **Timeouts bind background hooks too.** A background hook is supervised on
  its own thread; on expiry its process group is killed and reaped.
- **`background` describes scheduling, not capability.** A background hook
  receives the same stdin payload and environment as the foreground form; it
  is simply never awaited, so its `HookResult` is flagged as a submission and
  carries no exit code.
- **Conditions that can never match are rejected at load** rather than left
  silently inert — per entry, so a broken hook does not take a same-named or
  likewise-unnamed sibling with it — and a `tool_call_before` gate that returns
  no verdict does not read as permission when *that gate* declared
  `continue_on_error = false`. Strictness rides on the hook result, so it
  applies only to the hooks whose conditions matched the call in question.

## 1. Problem

Codewhale already has lifecycle hooks and MCP support, but the current hook
surface is mostly observer-only. This blocks portable extensions that need to
participate in the agent data flow:

- memory/context injection before a user message reaches the model
- post-turn background analysis that prepares context for the next turn
- sub-agent lifecycle visibility for orchestration and audit extensions

The current `message_submit` event fires before dispatch, but its output is
ignored. `TurnComplete`, `AgentSpawned`, and `AgentComplete` exist internally,
but they are not exposed as configurable hook events.

## 2. PR split

This issue should be implemented as three PRs. Each PR should be independently
reviewable and should leave the hook system in a useful state.

### PR 1: Mutable `message_submit`

Add a structured hook execution path for `message_submit` that can transform or
block the user's submitted text before it is sent to the engine.

Scope:

- keep the existing `[[hooks.hooks]]` config shape
- pass a JSON payload to the hook on stdin
- interpret stdout JSON containing `text` as the replacement user text
- treat exit code `2` as an intentional block
- run multiple submit hooks serially in config order
- keep existing env vars for compatibility
- keep `shell_env` stdout parsing unchanged

Non-goals:

- no tool argument mutation
- no global stdout JSON semantics for all hook events
- no transcript or model response mutation

### PR 2: `turn_end`

Expose the existing turn completion lifecycle as a hook event.

Scope:

- add `HookEvent::TurnEnd` with event name `turn_end`
- fire from the UI's `EngineEvent::TurnComplete` branch after core app state,
  usage, cost, notifications, and receipt state have been updated
- pass turn metadata on stdin as JSON
- make failures non-blocking and warn-only
- include a `stop_hook_active` field in the payload, initially `false`, so the
  contract can support re-entry protection later

Non-goals:

- no change to turn status
- no blocking of user input
- no transcript mutation from `turn_end`

Implementation note for the v0.9 branch: the narrow #2578 harvest uses the
shared structured observer path introduced for sub-agent lifecycle hooks. It
fires before queued follow-up dispatch, after queue-recovery state is known, so
the payload can report the queued-message count without letting a hook change
what gets sent next. Stdout is ignored for `turn_end`; only `message_submit`
has a stdout mutation contract.

### PR 3: Subagent lifecycle observer hooks

Expose subagent start and completion as observer-only hook events.

Scope:

- add `HookEvent::SubagentSpawn` with event name `subagent_spawn`
- add `HookEvent::SubagentComplete` with event name `subagent_complete`
- fire from the existing `AgentSpawned` and `AgentComplete` UI branches
- pass subagent metadata on stdin as JSON
- make failures non-blocking and warn-only

Non-goals:

- no subagent spawn gating in the first version
- no subagent prompt/result mutation
- no changes to subagent scheduling

## 3. PR 1 detailed plan

### 3.1 Contract

Configuration:

```toml
[[hooks.hooks]]
event = "message_submit"
command = "~/.deepseek/hooks/inject-memory.sh"
timeout_secs = 2
continue_on_error = true
```

Input payload on stdin:

```json
{
  "event": "message_submit",
  "text": "original user text",
  "session_id": "sess_xxxx",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

Output payload on stdout:

```json
{ "text": "replacement user text" }
```

Rules:

- exit `0` with stdout JSON containing `text: string` replaces the current text
- exit `0` with empty stdout leaves the current text unchanged
- exit `0` with JSON that does not contain `text` leaves the current text
  unchanged
- exit `2` blocks submission before the message is appended to history or sent
  to the engine
- other non-zero exits follow `continue_on_error`
  - `true`: warn, keep the current text, continue later hooks
  - `false`: stop later hooks and block submission with an error message
- `background = true` on `message_submit` remains observer-only and cannot
  transform or block submission

Multiple hooks:

- hooks run in config order
- each hook receives the latest transformed text
- the final transformed text is the only text used by file mention expansion,
  skill wrapping, auto routing, history, and `api_messages`

### 3.2 Implementation steps

1. Add structured submit outcome types in `crates/engine/src/hooks.rs`:

```rust
pub enum MessageSubmitOutcome {
    Unchanged,
    Replaced(String),
    Blocked { reason: String },
}
```

2. Add a stdin-capable sync executor:

```rust
fn execute_sync_with_stdin(
    &self,
    hook: &Hook,
    env_vars: &HashMap<String, String>,
    stdin_json: &serde_json::Value,
) -> HookResult
```

This should reuse the existing timeout, working directory, stdout, stderr, and
error handling behavior from `execute_sync`.

3. Add a `message_submit` transform entrypoint:

```rust
pub fn execute_message_submit_transform(
    &self,
    context: &HookContext,
    original_text: &str,
) -> MessageSubmitOutcome
```

This method should:

- filter configured `MessageSubmit` hooks through existing condition matching
- build a JSON payload for each hook using the current text
- run non-background hooks through `execute_sync_with_stdin`
- run background hooks with the existing observer-only path
- parse stdout JSON only for non-background hooks
- return the final text or a block result

4. Apply the transformed message in `dispatch_user_message`:

- run the transform before `last_submitted_prompt`, file mentions, history, and
  `api_messages`
- create a local mutable `QueuedMessage` or replacement display text
- if blocked, show a status message or toast and return without dispatch

5. Update `/hooks events`:

- keep `message_submit` listed
- update description to say it can transform or block user text

6. Update user-facing docs:

- document the stdin/stdout contract
- document exit code `2`
- document that `shell_env` still uses `KEY=VALUE` stdout

### 3.3 Test plan

Unit tests in `crates/engine/src/hooks.rs`:

- parses stdout `{"text":"changed"}` as replacement
- empty stdout means unchanged
- JSON without `text` means unchanged
- malformed stdout means unchanged with warning semantics
- exit `2` maps to blocked
- multiple hooks apply transforms in order
- background `message_submit` hook cannot transform
- `continue_on_error = false` blocks on non-zero failure

TUI integration or focused dispatch tests:

- transformed text is written to `api_messages`
- transformed text is written to visible history
- transformed text is used by file mention expansion
- blocked submit does not append user history
- blocked submit does not push an API message
- blocked submit leaves loading state false

Manual smoke test:

1. Add a config hook that prepends `[hooked] ` to every submitted message.
2. Submit `hello`.
3. Verify the transcript and model input use `[hooked] hello`.
4. Replace the hook with one that exits `2`.
5. Submit `hello`.
6. Verify no turn starts and the TUI shows the block reason.

## 4. Shared payload conventions

All new structured hook payloads should include:

- `event`
- `session_id`
- `workspace`
- `mode`
- `model`

Event-specific payloads should add only fields that are stable and useful for
extension authors. Avoid leaking secrets, full tool outputs, or unbounded
transcript content in the first version.

## 5. Compatibility

- Existing hook config remains valid.
- Existing observer-only hooks keep working.
- Existing env vars remain available.
- `shell_env` keeps its existing stdout `KEY=VALUE` contract.
- Structured stdout is interpreted only by `message_submit` in PR 1. Structured
  observer hooks such as `turn_end`, `subagent_spawn`, and `subagent_complete`
  receive JSON on stdin, but their stdout is ignored by the caller.

## 6. Review checkpoints

PR 1 should be accepted only if:

- submit mutation is covered by tests
- submit blocking is covered by tests
- the unchanged path preserves current behavior
- `shell_env` tests still prove the old stdout contract
- the docs clearly mark `message_submit` as the only mutable hook

PR 2 should be accepted only if:

- `turn_end` fires after `TurnComplete` app state updates
- failure is warn-only
- payload contains status and usage

PR 3 should be accepted only if:

- subagent hooks are observer-only
- failures do not affect subagent lifecycle
- payloads do not include unbounded or secret data
