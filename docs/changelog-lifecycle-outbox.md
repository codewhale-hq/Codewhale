# Changelog — lifecycle outbox (`[lifecycle_outbox]`)

Changelog for the general lifecycle event outbox (target: upstream
PR). Feature-complete against the v0.9.9 baseline (`6f3850c3d`).

## Added

- **Config**: new `[lifecycle_outbox]` table with three optional keys:
  - `path` — JSONL outbox file. Unset/empty = feature **off**, behavior
    unchanged (the whole feature is additive and opt-in).
  - `webhook_url` — optional webhook endpoint; POSTs fire only when set.
  - `webhook_token` — optional bearer token for `webhook_url`.
  Documented in `docs/CONFIGURATION.md` and `config.example.toml`. The
  documented example default is `~/.codewhale/notifications/outbox.jsonl`;
  the config key drives the real path.
- **Writer** (`crates/hooks/src/lifecycle_outbox.rs`): appends one JSONL line
  per event to the configured path — lazy parent dirs, append+flush, single
  internal writer task serializing emits in order. Line shape is the existing
  `RuntimeEventEnvelope` (`schema_version, seq, event, kind, thread_id,
  turn_id, item_id, timestamp, created_at, payload`). `seq` is monotonic per
  outbox file and recovers from the last complete line's `seq` on open
  (bounded 64 KiB tail scan; a torn trailing line from a crash is ignored).
  Payloads are constructed from bounded, pre-redacted fields only — never
  raw tool args, environment, or transcript text — with free-form fields
  capped at the notification limits (headline ≤ 80, detail ≤ 120,
  preview ≤ 200 chars) and stripped of control bytes.
- **Webhook**: `WebhookHookSink` supports an optional bearer token and is
  wired to outbox events when `webhook_url` is set — POST `{"at", "event"}`. Delivery is
  best-effort: failures are logged and dropped, never retried into the
  agent loop, and a failing webhook never blocks the local append.

## Events emitted

| Event | Kind | Site |
|---|---|---|
| `turn_start` | `turn.started` | TUI `EngineEvent::TurnStarted`; headless `exec` at `Op::SendMessage` |
| `turn_end` | `turn.completed` / `turn.failed` / `turn.interrupted` | TUI `TurnComplete` processing; headless `exec` `TurnComplete` (kind projected from status) |
| `turn_stalled` | `turn.stalled` | `recover_stalled_runtime_turn` — the first scriptable stall signal |
| `subagent_spawn` | `subagent.spawned` | subagent observer site (fires even with no hooks configured) |
| `subagent_complete` | `subagent.completed` | subagent observer site (fires even with no hooks configured) |
| `session_start` | `session.started` | TUI session-start hook fire site |
| `session_end` | `session.ended` | TUI session-end hook fire site |

Headless `codewhale exec` coverage: `turn_start` at message dispatch and
`turn_end` at the terminal `TurnComplete` — **and** at the "engine channel
closed before a terminal receipt" path, so every emitted `turn_start` has a
matching `turn_end` and a supervisor never sees an orphaned in-progress
turn. `exec` has no TurnStarted engine event, so `turn_id` is absent there;
`thread_id` is the resumed session id when `--continue` was used, empty for
fresh runs (the session id is only minted at persistence time).

## File contract

- One JSON object per line; every line is a complete
  `RuntimeEventEnvelope`; appended and flushed per event.
- `seq` counts up per file, starting at 1 for a new file, recovering from
  the last complete line after a restart.
- Cross-process: appends use O_APPEND with the line + newline in a single
  write, so two processes sharing one file can interleave *lines* but never
  splice a line mid-record. Seq uniqueness is per process recovery, so
  sharing one file across processes can repeat seq values — use one file
  per process for strict uniqueness.

## Tests

- `crates/hooks`: append/schema shape, seq recovery across reopen, missing/
  empty file, torn trailing line, emit ordering under the writer task,
  disabled-outbox no-ops, `bounded_text` ceilings (incl. UTF-8 boundaries).
- `crates/config`: `[lifecycle_outbox]` off-by-default, webhook optional,
  full-table parse.
- `crates/tui`: TUI config parse of the table; stall-recovery emit-site
  tests (enabled outbox writes one `turn_stalled` line naming the wedged
  turn; disabled outbox writes nothing and recovery behavior is unchanged).

## Not changed

- With `[lifecycle_outbox]` unset, zero behavior change: the outbox handle
  is a disabled no-op and no file or HTTP request is ever made.
- No new runtime dependencies (JSONL append uses tokio fs; webhook reuses
  the existing `reqwest` client builder).

## Remaining / follow-ups

- Session ids for fresh headless `exec` runs are empty on `turn_start`
  (minted only when the run is persisted); a supervisor correlating runs
  can key on the process + file.
- Webhook-only configuration (url without path) parses losslessly but does
  not activate the outbox handle today — the file path is the feature gate.
  Documented; can be lifted later if webhook-only delivery is wanted.
