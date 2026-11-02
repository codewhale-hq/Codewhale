# Post-0.9.1: thin TUI over core + stream consolidation

**Status:** seams landed in v0.9.1; full split deferred.

## What shipped in 0.9.1 (seams only)

New visual / capability systems stay in focused modules (do not grow the
monoliths without necessity):

| System | Module |
|--------|--------|
| Ambient ocean life | `tui/ambient_life.rs` |
| Hot tail | `tui/hot_tail.rs` |
| Hover aura | `tui/hover_hit.rs` + `tui/hover_layer.rs` |
| Git status cache | `tui/git_status.rs` |
| Worktree manager UI | `tui/worktree_manager.rs` |
| Phase rail | `tui/phase_strip.rs` |
| Stream entry seam | `client/stream_entry.rs` |

Business logic must not land in `tui/ui.rs` / `tui/app.rs` / `tui/widgets/mod.rs` unless it
is pure view wiring.

## StreamFn consolidation (landed post-0.9.1)

`client/stream_entry.rs` is the shared open-path seam, and all three
streaming adapters open through it:

- HTTP policy (`DualWithH1Fallback` / `Http1Only`, env pin via
  `CODEWHALE_FORCE_HTTP1`)
- dual/H1-twin client selection (`client_for_policy`)
- bounded response-header wait (`stream_open_timeout`, env override
  `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`)
- one shared open function (`open_sse_response`): only a proven transient
  connection failure before dispatch can select the HTTP/1.1 twin; a header
  stall or interrupted response remains unknown and cannot reissue inference
- typed H1 retry classification (`is_stream_open_transport_failure`)
- idle-timeout message format (`idle_timeout_message`, with
  bytes/age/last-chunk diagnostics)

Wire-protocol request construction and stream decoding remain at the
adapter edge (`chat.rs`, `anthropic.rs`, `responses.rs`): each adapter
builds its own endpoint URL, headers, auth, and body inside the attempt
closure it hands to `open_sse_response`. The shared streaming retry loop
(`send_retry_loop`) wraps stream opening; rate-limit backoff stays outside each
response-header timer. It permits only ordinary rate limits or proven transient
pre-dispatch connection failures. Both inference clients disable automatic redirects and reqwest
protocol retries. Read-only catalog GETs select their bounded retry policy at
their actual method owner.

Remaining follow-up: collapsing further toward a piagent-style single
StreamFn (shared decode loop) is still deferred.

## Thin TUI over core (north star)

`tui/ui.rs` / `tui/app.rs` / `tui/widgets/mod.rs` remain large. Post-0.9.1 priority:

1. Extract tool / git / github / session / workflow / MCP routing out of the TUI
   crate into a core/data layer (kimi-code `agent-core` / piagent package shape).
2. Keep the TUI a projection of state + input routing.
3. Prefer new modules over adding to the three monoliths.

## Reserved vocabulary: "Operation" (not shipped, not a rename)

**Workflow keeps its name.** `/workflow`, `codewhale workflow`, the `workflow`
tool, the `[workflow]` config table, `*.workflow.js` sources, and the public
vocabulary below are unchanged in v0.9.2, and no `/operation` or
`codewhale operation` alias ships.

The public vocabulary stays:

- **Fleet** = who does the work.
- **Workflow** = what order the work follows.
- **Lane** = one running Workflow.
- **Runtime** = where and how a Lane executes.
- **Operate** = the current mode (unchanged).

"Operation" is **reserved for a future, distinct concept** — a continuous
objective or organization-level control loop that coordinates *multiple Fleets
and multiple Workflows over time*. It is a layer above a Workflow, not another
name for one. Nothing implements this today; do not advertise it, add config
keys for it, or introduce it as a synonym for Workflow. If it is ever built, it
must earn the noun by being that coordinating layer.

## Optional deferred

- Full live global model subscriptions (refresh on every open + `r` / Ctrl+R is
  the practical path; continuous live feed if unstable stays deferred).
- YOLO mode is gone from product UI; `mode_yolo` remains only as legacy theme
  palette data.
