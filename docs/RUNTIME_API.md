# Codewhale Engine Runtime API & Integration Contract

> 阅读简体中文版：[zh_hans/RUNTIME_API.md](zh_hans/RUNTIME_API.md)。

`codewhale app-server` exposes the [Codewhale Engine](ARCHITECTURE.md) through
its canonical local Runtime API and control plane.
Local SDKs, mobile/remote-control clients, and editor integrations talk to it
instead of screen-scraping terminal output. It serves the full HTTP/SSE runtime
API (`/v1/*`), a JSON-RPC control transport over stdio, and the phone-friendly
mobile page. `codewhale doctor --json` provides machine-readable health, and
`codewhale serve --acp` speaks the Agent Client Protocol over stdio for editors
such as Zed.

`codewhale serve --http` / `serve --mobile` remain as **compatibility aliases**
for `codewhale app-server --http` / `--mobile`; both launch the identical
server. New integrations should target `app-server`.

`codewhale exec` is the separate one-shot headless worker path (stream-json,
fleet worker subprocess, CI primitive). It is not part of this API, but it
shares the same runtime, provider/model resolution, permission profiles, and
event vocabulary.

This document is the stable integration contract for native workbench
applications (and other local supervisors) that embed the Codewhale Engine.

## Architecture

```
local supervisor / SDK / automation harness
        │
        ├─ codewhale app-server --http     → HTTP/SSE runtime API (/v1/*)        [canonical]
        ├─ codewhale app-server --mobile   → runtime API + mobile control page
        ├─ codewhale app-server --stdio    → JSON-RPC control transport over stdio
        ├─ codewhale app-server --socket   → same JSON-RPC over a unix domain socket (desktop daemon)
        ├─ codewhale doctor --json         → machine-readable health & capability
        ├─ codewhale serve --acp           → ACP stdio agent for editors such as Zed
        ├─ codewhale serve --mcp           → MCP stdio server
        ├─ codewhale serve --http/--mobile → legacy aliases for `app-server --http/--mobile`
        └─ codewhale exec [args]           → one-shot headless worker (stream-json)
```

The engine runs as a local-only process. All APIs bind to `localhost` by
default. No hosted relay, no provider-token custody, no secret leakage.

For the read-only record of what a thread or turn did, see
[`docs/RECEIPTS.md`](RECEIPTS.md): `codewhale receipts` on the CLI and the
`/receipt` routes under **Threads** below.

## Runtime API entrypoints

| Entry | Transport | Use |
|---|---|---|
| `codewhale web [--port 7878]` | HTTP/SSE on `127.0.0.1:7878` + embedded client | First-class loopback-only browser client; opens the default browser |
| `codewhale app-server --http` | HTTP/SSE on `127.0.0.1:7878` | Full `/v1/*` runtime API (canonical) |
| `codewhale app-server --mobile` | HTTP/SSE on loopback + `/mobile` | Runtime API + local mobile control page |
| `codewhale app-server --stdio` | JSON-RPC 2.0 over stdio | Local SDK / control probe (no listener) |
| `codewhale app-server --socket [--socket-path P]` | JSON-RPC 2.0 over a `0600` unix domain socket | Desktop daemon: multi-client, peer-uid checked, `daemon/attach` claim handshake (macOS/Linux; Windows named pipe reserved, not implemented) |
| `codewhale app-server` | HTTP on `127.0.0.1:8787` | Legacy in-process app-server (`/healthz`, `/thread`, `/app`, `/prompt`); background jobs are `GET /v1/jobs` on the Runtime API; `/prompt` and `/thread` messages execute real turns via the runtime bridge. There is no direct `/tool` route: tools run only inside Engine turns, under the Engine's tool catalog and approval posture. This legacy server does not surface approvals: its bridge forwards only text deltas and the turn's completion, and it has no decision route, so an approval-gated call waits unanswered. Drive approval-gated work through the Runtime API (`/v1/threads/*` events and `POST /v1/approvals/{approval_id}`) |
| `codewhale serve --http` / `--mobile` | same server as `app-server --http`/`--mobile` | Compatibility aliases |

`app-server --http` and `--mobile` launch the same mature runtime API server
historically reached through `serve --http` — no routes or behavior changed, so
every endpoint documented below is identical across both entrypoints. The
runtime API token is read from `--auth-token`, then `CODEWHALE_RUNTIME_TOKEN`,
then `DEEPSEEK_RUNTIME_TOKEN`; use `--insecure-no-auth` only with a loopback
bind. The `serve` compatibility aliases keep their `--insecure` flag.
The legacy in-process `codewhale app-server` also requires an explicit
`--auth-token` or `CODEWHALE_APP_SERVER_TOKEN` before binding a non-loopback
host; its generated one-time `cwapp_*` token is loopback-only.

Device tokens minted by the master through `POST /v1/auth/client-tokens`
have an immutable `intent`: `watch` (the default when omitted) or explicit
`drive`. Labels do not grant authority. Watch permits ordinary GET/HEAD reads,
Computer display and one-use display tickets; it cannot mutate Runtime state,
upgrade a protected HTTP read into a write channel, acquire/release control,
or forward display input. Drive retains the existing Runtime/control authority
but cannot mint, list or revoke device tokens. Display tickets retain the
issuing principal's intent; input still requires its current, live control lease.

`GET /v1/runtime/info` advertises `capabilities.client_token_intents: true`,
and the mint receipt returns `device_id`, `intent` and `expires_at`. A relay
grant issuer must require this capability before minting and validate that
receipt against the requested device/intent before exposing a token. Older
Engines lack enforcement and must refuse relay grants through this issuer;
an intent-like label or a successful legacy mint is insufficient. This change
does not add account/Computer ownership scopes to Engine-local device tokens.

### Workspace file suggestions

`GET /v1/workspace/files/search?query=runtime&limit=20` returns
`{"paths":["src/runtime.rs"]}` through the existing authenticated `/v1/*`
router. It searches only the server's configured workspace, not a thread's
workspace or the process's current directory. No workspace/path override is
accepted. The response contains workspace-relative file paths with `/`
separators, never file contents, absolute paths, or directories.

- `query` is a literal partial filename/path, without an `@` prefix, at most
  256 UTF-8 bytes. Missing, empty, or whitespace-only queries return an empty
  list without walking the filesystem. No match also returns an empty list.
- `limit` defaults to 20; accepted values are 1–100. Invalid limits, oversized
  queries, and unknown query parameters return HTTP 400.
- Matching reuses TUI fuzzy `@file` discovery/ranking: case-insensitive path
  prefix matches first, then substring matches, alphabetically within each
  group. This is not glob, subsequence, content, or semantic search, and does
  not apply the TUI's personal frecency boosts.
- Discovery shares the composer's ignore policy, including `.ignore` and
  `.deepseekignore`, always-discoverable AI directories, and the bounded
  hidden/gitignored local-reference fallback. The special `.agents`, `.claude`,
  `.cursor`, and `.deepseek` walks intentionally bypass ignore rules, as in
  the TUI. Ignore files are not confidentiality boundaries.
- Directory symlinks are not traversed. Files are canonicalized and filtered
  for containment in the workspace before applying the result limit; external
  and broken file symlinks are omitted. In-workspace file symlinks may appear
  by their relative names. Suggestions are a filesystem snapshot, not
  authorization to read a file later; consumers must revalidate when opening it.

Discovery runs off the async executor, with the shared default depth of 10,
at most 20,000 candidates, and a cooperative two-second discovery budget.
Results are best-effort, not an exhaustive listing; a slow filesystem operation
can finish after that budget. Each request scans anew; there is no new index or
cache. This read-only endpoint does not alter sessions or the pinned model
prompt/tool prefix.

### Workspace files and session artifacts

Native clients (the GPUI desktop's Files and Preview modules) browse and edit
the server's configured workspace through three authenticated routes. They
read and write the workspace directly; there is no second file store, cache
or index, and no path override: the workspace root is the only root.

- `GET /v1/workspace/files?path=<dir>&limit=<1-2000>` lists one directory.
  `path` is workspace-relative with `/` separators; empty or `.` is the root.
  Each entry carries `name`, `path`, `kind` (`file`, `directory`, `symlink`,
  `other`), and for files `size` and `modified` (RFC 3339). Directories sort
  first, then names case-insensitively. `limit` defaults to 200; `truncated`
  reports a cut. `.git` is never listed or served, and symlinks are listed by
  name only: they are never followed, so `path=<link>` returns 403.
- `GET /v1/workspace/files/read?path=<file>&offset=<bytes>&limit=<1-4194304>`
  returns one byte window of a regular file with `size`, `revision` (the
  SHA-256 hex of the **whole** file, not of the window), `modified`,
  `offset`, `bytes`, `truncated`, `encoding` and `content`. Text windows are
  `utf-8`; a window with a NUL byte, invalid UTF-8, or a split multi-byte
  character is `base64`. `limit` defaults to 256 KiB. Files above 16 MiB are
  refused with 413; a directory is 400; a link is 403; a missing file is 404.
- `PUT /v1/workspace/files` with `{"path", "content", "encoding"?,
  "expected_revision"?}` writes one file atomically through the same confined
  opener Fleet artifacts use. `encoding` is `utf-8` (default) or `base64`;
  bodies above 4 MiB are 413. Creating a new file requires **no**
  `expected_revision` (and creates missing parent directories inside the
  workspace); overwriting requires the `revision` from the read that the
  edit was based on, and a stale or missing one is 409 with the current
  revision in the error message so the client can re-read and merge. This is
  optimistic concurrency, not a lock: two writers racing between the check and
  the write can still interleave. The response carries `path`, `size`,
  `revision`, `created` and `written_at`; 201 for a new file, 200 otherwise.
  Writes through a link, into `.git`, or to a directory are refused.

Every path is validated before any filesystem access: absolute paths,
backslashes, `.` or `..` components are 400, and each directory on the way is
opened without following links (`O_NOFOLLOW` per component on Unix, reparse
point checks on Windows). These routes use the runtime bearer token like every
other `/v1/*` route; they do not consult the model's tool permission posture,
because the caller is the authenticated operator, not the model.

Session artifacts are the oversized tool outputs a session recorded as
`ArtifactRecord`s (`crates/tui/src/artifacts.rs`), stored under
`sessions/<id>/artifacts/`:

- `GET /v1/sessions/{id}/artifacts` lists the records a saved session carries:
  `id`, `kind`, `tool_call_id`, `tool_name`, `created_at`, `byte_size`,
  `preview` and the session-relative `path`.
- `GET /v1/sessions/{id}/artifacts/{artifact_id}?offset=&limit=` reads one
  artifact with the same window, `revision` and `encoding` contract as the
  workspace file read. A record whose stored path is absolute or leaves the
  session directory is 403; a record whose file is gone is 404.

These routes only serve what a SavedSession indexes (plus immutable image
evidence). A runtime turn's spills are read through the turn route below. An
unbound runtime thread's engine has no SavedSession index at all, so for its
spills the turn record is the only way in.

#### Turn artifacts

A turn records what it produced as typed references on its items and on the
turn itself. Nothing is scanned to build them. Each fact is recorded where the
bytes were written:
- file tools and `apply_patch` report `size`/`sha256` in `mutation.files[]`;
- spills report `artifact_digest`;
- tool media reports `sha256`.

The workspace-level half comes from the turn's own restore points: the
`pre_turn` and `post_turn` receipts in `TurnRecord.workspace_snapshots` (see
"Workspace restore points" below), diffed by tree id in the existing side
repo. No second store, snapshot or event is involved.

A reference (`TurnArtifactRef`) carries:

| Field | Meaning |
| --- | --- |
| `id` | Stable within the turn. A file's id is `file_` plus the first 32 hex digits of SHA-256(path). A spill's is `art_<call>`. Media's is `art_image_<sha256>`. |
| `kind` | `file`, `tool_output` or `media`. |
| `path` | For `file`: workspace-relative with `/` separators. For `tool_output` and `media`: session-relative (`artifacts/...`). |
| `change` | For `file` only: `created`, `updated`, `deleted` or `renamed`. A rename also has `previous_path`. |
| `size` | Byte size. Absent when the file was deleted. |
| `revision` | SHA-256 hex of the whole content. This is the value `GET /v1/workspace/files/read` reports as `revision`, and file-revert's `expected_hash` is `sha256:` + `revision`. Absent when the file was deleted, or for a delta blob over 16 MiB. |
| `content_type` | For `media`: the exact media type. |
| `session_id` | For `tool_output` and `media`: the artifact session that owns the bytes. |
| `item_id`, `tool_call_id`, `tool_name` | The tool call that wrote it. Absent for a change seen only in the workspace delta. |
| `source` | `tool_mutation`, `tool_output_spill`, `tool_media`, or `workspace_changed_during_turn`. |
| `restore_snapshot_id` | A restore point `POST /v1/threads/{id}/file-revert` accepts for this path on this thread: the `tree_id` of a receipt recorded on this turn's `workspace_snapshots`, so the thread owns it whether or not it is bound to a saved session. For a tool write it is the call's `tool` receipt; for a delta change it is the turn's `pre_turn` receipt. Absent when the turn recorded no such receipt. |
| `recorded_at` | When the reference was recorded. |

Where references appear:
- **Items.** `TurnItemRecord.artifacts` lists what one tool call produced. It
  is set when the call completes, succeeded or failed, so `item.completed` and
  `item.failed` carry it live.
- **Legacy projection.** `artifact_refs` is derived from `artifacts`. It holds
  only the workspace-relative paths of files that still exist: never spills,
  media or deleted files.
- **Turns.** `TurnRecord.artifacts` is the turn aggregate. It is computed by
  one merge and ordered most recent first.
  - Spill and media refs are always kept.
  - File refs compose in item order: created then deleted drops the file,
    created then updated stays `created`, and a rename folds its origin.
  - Once the workspace delta settles, it is authoritative for the net change,
    `size` and `revision` of every path the snapshots can see. It adds files
    no tool receipt named, such as shell and sub-agent writes. It drops an item
    path the snapshots track but that ended the turn unchanged.
  - The aggregate is capped at 1000 refs, and `workspace.truncated` /
    `workspace.omitted` report the cut.

`TurnRecord.workspace` follows the delta's lifecycle. It is `null` while the
turn runs. `turn.completed` carries it with one of these states:
- `pending`: the turn recorded both a `pre_turn` and a `post_turn` receipt,
  and their diff is still running. When it finishes, the runtime publishes
  `turn.artifacts`
  (`{turn_id, workspace, artifacts}`). That event may arrive after the next
  turn's `turn.started`, so key it by `turn_id`.
- `settled`: the delta is merged. `pre_turn_snapshot_id` and
  `post_turn_snapshot_id` are the tree ids of the pair.
- `unavailable`: no delta will come. `reason` says why:
  - `snapshots_disabled`, `workspace_too_large`, `too_many_files`,
    `unsafe_location`, `snapshot_failed`: the snapshot gates. A turn with a
    `pre_turn` receipt but no `post_turn` one keeps `pre_turn_snapshot_id`.
  - `not_captured`: the turn recorded no restore point: a compaction or purge
    operation, or a turn whose engine ended before taking one.
  - `runtime_restarted`: the process stopped before the delta settled. This is
    reconciled at startup and never recomputed.
  - `delta_failed`: the diff itself failed.

  `artifacts` still holds what the tool receipts recorded. `turn.artifacts` is
  published for every settlement outcome, so a client waiting on `pending`
  always hears back.

What the delta means, and what it cannot see:
- **It is a workspace diff, not attribution.** A delta change is everything
  that changed in the workspace while the turn ran. That includes an editor,
  another thread, or a background job writing the same workspace at the same
  time. `source: workspace_changed_during_turn` says exactly that.
- **Excluded paths are invisible to snapshots.** This covers the built-in
  excludes (for example `node_modules/`, `target/`, `dist/`, `build/`,
  `.next/`, and binary and media extensions) and the workspace's `.gitignore`. A file tool's write to
  such a path is still reported from its receipt. A shell command's write to
  one is not reported at all.
- **Shell writes have no per-call record.** A shell command's writes are
  never attributed to its item, only to the turn, and only when snapshots are
  enabled.

Routes:

- `GET /v1/threads/{id}/turns/{turn_id}/artifacts` returns
  `{thread_id, turn_id, workspace, artifacts}` from the runtime store's turn
  record. While the turn runs, `artifacts` is merged from its items on the fly
  and `workspace` is `null`. An unknown turn, or a turn of another thread, is
  404.
- `GET /v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}?offset=&limit=&revision=`
  reads one reference.
  - It uses the workspace file read's window contract (`size`, `revision`,
    `offset`, `bytes`, `truncated`, `encoding`, `content`) and adds:
    - `artifact`: the reference;
    - `source`: `workspace`, `snapshot` or `session_artifact`;
    - `current`: whether the workspace still holds these bytes, or `null`
      when that is not a question for this reference.
  - `revision` selects an intermediate revision one of the turn's items
    recorded. The default is the reference's own revision.
  - A `file` is served from the workspace when it still holds the recorded
    revision (`current: true`). Otherwise it comes from the turn's post-turn
    snapshot (`current: false`).
  - A `tool_output` or `media` reference is read under the session artifact
    root the writer used. The same confinement, image-manifest and integrity
    checks apply as for the session route.
- `GET /v1/threads/{id}/turns/{turn_id}/calls/{tool_call_id}/changes?limit=`
  returns what **one** tool call changed, read from the same two restore
  points the engine recorded around it (the call's `tool:` receipt and its
  `post-tool:` partner on this turn):

  ```json
  {
    "thread_id": "thr_1a2b3c4d", "turn_id": "turn_…", "tool_call_id": "call_…",
    "tool_name": "exec_shell",
    "state": "captured", "reason": null, "truncated": false,
    "files": [
      {
        "path": "out/result.json", "change": "created",
        "added": 12, "removed": 0, "size": 210, "revision": "<sha256 hex>",
        "restore_snapshot_id": "<pre tree id>",
        "diff": "@@ -0,0 +1,12 @@\n+{}\n", "diff_truncated": false
      }
    ]
  }
  ```

  - This is the per-call counterpart of the turn aggregate, and the only
    surface on which a **shell command's own writes** are attributable to that
    command: a command produces no `metadata.mutation`, so nothing else names
    what it wrote. The files a file tool changed are here too, read from the
    same span.
  - `change` is `created`, `updated` or `deleted` (git's `A`/`M`/`D`, with a
    type change read as `updated`; the diff runs with `--no-renames`, so a move
    is a delete plus a create). `added`/`removed` are `null` for a binary path.
  - `size` and `revision` are the span's **end**, never the work tree as it is
    now. `revision` is bare SHA-256 hex; pass `sha256:` followed by that hex as
    `file-revert`'s `expected_hash`, or `absent` for a path deleted by the call.
    Both are `null` when the path was deleted here or is too large to read.
  - `diff` is the patch between the two restore points, cut at 64 KiB on a char
    boundary (`diff_truncated` says so). It is `null` when there is nothing to
    render: a binary path, a change with no content delta, or no patch.
  - `state: "pending"` means the recorded call is queued or in progress and
    its snapshot pair is incomplete. `reason` is `null` and `files` is empty;
    read again after settlement. Once both receipts exist, the state is
    `captured`, including when the call changed nothing.
  - `state: "unavailable"` means the settled span cannot be resolved, and `reason`
    says why: `call_not_bounded` (the call took no receipt — the engine judged
    it read-only, or the turn predates receipts), `post_snapshot_missing` (the
    opening receipt exists and the closing one was lost), `pre_snapshot_missing`,
    or `snapshots_pruned` (the receipts are still on the turn, but the side repo
    no longer holds the trees they name — snapshots are pruned to the newest few
    while turn records are durable, so an older turn's span is regularly
    unrecoverable, and a workspace whose store was deleted reads the same way).
    This is **not** the same answer as an empty `files` list, which means the
    span changed nothing, and the receipt's own `changed_paths` remain readable
    on the turn record either way.
  - `limit` (default 200, max 1000) caps the list; `truncated` says it was cut,
    and how many paths were left off is not counted.
  - Reading the span runs `git diff` inside the side repo. Neither the work
    tree nor the index is touched.

| Status | When |
| --- | --- |
| 404 | Unknown thread or turn, a turn of another thread, or a `tool_call_id` this turn has no item or receipt for. |
| 400 | `limit` outside `1..=1000`. |
| 500 | A runtime item record could not be read or parsed, or an operational failure occurred reading the snapshot repository; this is not evidence that the call was unbounded or snapshots were pruned. |

The artifact routes answer:

| Status | When |
| --- | --- |
| 404 | Unknown thread, turn or artifact id, or a `revision` this turn never recorded. |
| 409 | A file's recorded revision is in neither the workspace nor the snapshot store (snapshots are pruned after 50 per workspace or 7 days), or a session artifact's bytes no longer hash to the recorded revision. The message names the current revision. |
| 410 | The turn deleted the file (restore it with `file-revert` and `restore_snapshot_id`), or a session artifact's bytes were pruned. |
| 413 | Content over 16 MiB. |
| 403 | A symlink, or a reference that leaves its root. |

Fleet receipt artifacts keep their own route
(`GET /v1/fleet/runs/{run_id}/receipts/{task_id}/evidence`).

### Runtime and account identity

`GET /v1/runtime/info` reports `codewhale_version` plus the full 40-character
`codewhale_commit` embedded by the shared CLI/TUI build. A source archive that
cannot provide an exact commit reports `unknown`, allowing compatibility
clients to fail closed rather than accepting an ambiguous binary pair.

The same response advertises `capabilities.account_session: true` and
`capabilities.turn_operation_idempotency: true`. A client must require the
latter before relying on `operation_key`; do not infer support from a 2xx turn
response because an older tolerant reader may ignore an unknown request field.
`capabilities.turn_operation_lookup: true` separately advertises the read-only
operation lookup below; clients must require it before relying on GET-based
recovery of a lost turn response.
The response also includes a token-free account receipt:

```json
{
  "account": {
    "schema_version": 1,
    "state": "authenticated",
    "api_base": "https://api.codewhale.net",
    "account_id": "acct_...",
    "session_id": "session_...",
    "scopes": [],
    "expires_at": "2026-08-01T20:00:00Z"
  }
}
```

The Runtime reads this receipt from the exact profile- and API-origin-scoped
secure record written by `codewhale account login`; it does not run a second
login flow. States are `signed_out`, `authenticated`, `offline_cached`,
`expired`, or `revoked`. Scopes are copied only from explicit stored session
grants and are never inferred from account identity. Access/refresh tokens,
email, provider profile, and provider credentials are never returned.
`account_id` and `session_id` are included only for a request authorized with
the Runtime token (or an explicitly insecure loopback server); the public
bootstrap response remains usable but reports `signed_out`. Signed-out local
Work remains supported and never allocates cloud compute implicitly.

The `--stdio` control transport is newline-delimited JSON-RPC 2.0. Probe it
without spending model tokens:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"healthz"}' \
  '{"jsonrpc":"2.0","id":2,"method":"capabilities"}' \
  '{"jsonrpc":"2.0","id":3,"method":"shutdown"}' \
  | codewhale app-server --stdio
```

`capabilities` returns the advertised method families (`thread/*`, `app/*`,
`prompt/*`) and the full method list; `thread/capabilities`,
`app/capabilities`, and `prompt/capabilities` scope it per family. The method
set is pinned by a drift test in `crates/app-server/src/lib.rs`, so SDK and
local integration clients can rely on it not changing silently.

### Daemon socket: `codewhale app-server --socket`

The desktop shell (DESKTOP-APP-BRIEF §2) attaches to a long-lived daemon over
a unix domain socket. The wire is the `--stdio` transport verbatim — the same
newline-delimited JSON-RPC 2.0 methods, dispatched by the same code — with one
handshake in front of it.

> Note (2026-09-14): the Tauri desktop shell named above is retiring under the
> 2026-09-14 product-client transition, and the DESKTOP-APP-BRIEF reference is
> a dangling pointer (that brief does not exist in this repo). The GPUI client
> in the private `codehwhale-gpui` repo is the successor daemon consumer over
> this HTTP runtime API; the socket protocol described here is unchanged.

**Endpoint.** `--socket-path` if given; else `$CODEWHALE_HOME/run/daemon.sock`
when `CODEWHALE_HOME` is set (an explicit home is an isolation boundary); else
`$XDG_RUNTIME_DIR/codewhale/daemon.sock`; else
`~/Library/Application Support/codewhale/daemon.sock` on macOS or
`~/.codewhale/run/daemon.sock` elsewhere. The directory is created `0700`, the
socket is `0600`, and every accepted peer must present the daemon's own uid.
On start, a socket file nobody answers on is removed; a live one makes the new
daemon exit with `a live listener already answers on <path>; refusing to
replace it`; a non-socket file at the path is never touched. On Windows `--socket` fails with a typed
`UnsupportedPlatform` error naming the reserved pipe `\\.\pipe\codewhale-daemon`
— there is no silent TCP fallback. The daemon prints
`codewhale daemon: listening on <path>` to stderr once it is accepting.

**Handshake.** The first request on a connection must be `daemon/attach`
(`healthz` is also allowed beforehand, so a shell can probe liveness). Every
other method is refused with `-32010 attach_required` until then.

```json
{"jsonrpc":"2.0","id":1,"method":"daemon/attach","params":{
  "client":{"name":"codewhale-desktop","version":"1.2.3","pid":4242},
  "mode":"claim",
  "expect_daemon_version":"0.9.11"}}
```

`mode` is `"claim"` (this client spawned the daemon and manages its lifetime)
or `"attach"` (default: a guest that found a healthy daemon). A claim while
another connection owns the daemon fails with `-32011 daemon_already_claimed`
(`data.owner` names the holder) and the client should retry with `attach`.
`expect_daemon_version`, when present, must equal the daemon's crate version
or the attach fails with `-32013 daemon_version_skew` (the bundle-skew guard).
The reply reports the granted `role` (`owner` / `attached`), the daemon's
`pid`, `version`, `socket_path`, and `uptime_ms`, the current `owner`, and the
live `connections` count. A second `daemon/attach` on an attached connection
is `-32014 already_attached`.

**Capabilities.** On this transport `capabilities.methods` is the pinned stdio
set plus `daemon/attach` (second entry, after `healthz`); `transport` reads
`unix-socket`. `shutdown` is advertised to every connection because the method
exists, but only the owner may call it (below).

**Ownership.** Only the owner may `shutdown`; a guest's `shutdown` is refused
with `-32012 not_daemon_owner` and does not interrupt anyone's turn. When the
owner disconnects the slot frees, so a relaunched shell re-claims the daemon it
left running. The owner's `shutdown` stops the listener, closes every
connection, and removes the socket file. Journal replay from a client's
last-seen `seq` is not part of this transport yet.

### Interrupting a turn

`thread/message` streams until the turn reaches a terminal state, which can
take minutes. The read loop keeps polling stdin while a turn streams, so a
client can send:

```json
{"jsonrpc":"2.0","id":9,"method":"thread/interrupt","params":{"thread_id":"thr_..."}}
```

and the runtime is asked to interrupt that turn
(`POST /v1/threads/{id}/turns/{turn_id}/interrupt`). The reply carries
`interrupted: false` when no turn is streaming for that thread — this is not
an error, just nothing to stop. The interrupted `thread/message` then fails
with a `turn interrupted` error, and its reply is written before the
interrupt's own reply, since the turn owns the writer until it unwinds.

`shutdown` sent during a live turn also interrupts first: it needs the same
bridge that the turn holds, so without that it would wait for the very turn
it was meant to stop. Other requests that arrive mid-turn are queued and run
in order once the turn finishes.

### Running a prompt

`prompt/request` and `prompt/run` (byte-identical aliases) and the legacy
HTTP `POST /prompt` all execute a **real turn** on the runtime, through the
same bridge `thread/message` uses. There is no local fallback: nothing else
in the app-server can produce model output, so a prompt either runs or fails.

- `params.prompt` is required and must be non-empty (`-32602` otherwise).
- `params.thread_id` is optional. With one, the prompt runs on that thread and
  its history. Without one, the runtime gets a fresh thread for that single
  turn; the mapping is dropped when the turn ends, so a one-shot prompt is not
  addressable by `thread/interrupt`. Use `thread/message` when you need to be
  able to interrupt.
- `params.model` selects the model only when the call is the one that creates
  the runtime thread; an existing thread keeps the model it was created with.
- The response carries what the model actually said: `output` is the
  concatenated `agent_message` text, `model` is the model the runtime reports
  for the thread that ran it, and `events` are the real
  `response_start`/`response_delta`/`response_end` frames. Over stdio the same
  frames are also streamed to stdout while the turn runs, exactly as for
  `thread/message`.
- If the runtime cannot be reached, the call fails with `-32005`
  (`runtime_unavailable`) on stdio, or HTTP `503` with
  `{"error":{"code":"runtime_unavailable", ...}}` on `POST /prompt`. Failures
  are never shaped like a successful `PromptResponse`.

`POST /thread` with a `Message` body behaves the same way — it runs the turn
and replies `status: "completed"` with the streamed frames in `events` — where
it previously replied `accepted` without doing anything.

### Thread ids and restarts

`thread/message`, `thread/request` messages, and HTTP `POST /thread` messages
take a thread id from `thread/create` (or `thread/fork`). An id that was never
created fails with `-32004` (`thread_not_found`) on stdio, or HTTP `404` on
`/thread`, before any runtime thread is started. `/prompt`, `prompt/request`,
and `prompt/run` are different: their optional `thread_id` is any key the
caller chooses, and a new key starts a new conversation.

The authenticated canonical owner keeps the full saved conversation graph,
its selected branch and the thread/session binding. Compatibility controls use
that owner; the old SQLite history remains a protected, read-only import source.
Import compares the complete source graph and selected leaf before publishing
the canonical alias. A failed alias publication retains the source and the
actual canonical result so recovery can report what completed.

Existing validated legacy goals are imported into the owner goal store. Active
goals are paused during import; historical data never starts a provider call.
Source goal fields participate in the same protected source comparison.

Saved-session forks preserve the complete journal, including inactive branches,
and copy a validated local session-goal sidecar into the new saved session.
An active local goal is copied as paused; the source remains unchanged.
That sidecar is separate from the public Runtime thread goal. A native Runtime
thread fork does not automatically inherit the public thread goal.

`thread/create`, `thread/start`, `thread/resume` and `thread/fork` carry a
client-generated `operation_key`. Capture one key for each user intent before
sending it, retain it after an uncertain response, and reuse it when recovering
that same intent. Create carries the key in `metadata.operation_key`; Start,
Resume and Fork carry it in `operation_key`. Two intentional forks use different
keys. Recovery checks
the original operation in the existing owner store; it never creates another
thread merely because the response was lost or the source later grew.

`POST /v1/thread-history/operations/lookup` is read-only. Its closed request
contains `version: 1`, `operation_key`, `expected_data_dir`,
`expected_execution_scope` and `workspace`; the response is `absent`, `pending`
or `committed`. A pending or committed response includes the retained receipt
and its exact action/source `association`.

`POST /v1/thread-history/operations/recover` accepts that lookup request in
`operation` and the expected `association`. It explicitly finishes an already
prepared target under the same owner, after checking the saved document, full
graph, workspace, checkpoint and action/source identity. It does not rebuild
the original intent from a source that may already have changed. An unprepared
target stays pending; a changed or unverifiable target refuses completion.
Repeat recovery with the same key to observe the same committed result.

The selected workspace comes from the acknowledged owner or the explicitly
admitted request. History and historical receipts cannot supply permissions,
credentials, endpoints or a different owner. A missing, changed, busy or
incompatible bound store is an explicit failure. A missing canonical target
does not start an empty replacement conversation.

`codewhale thread resume` and `codewhale thread fork` perform the durable
owner control and print its committed thread, session and operation receipt.
They do not launch an interactive frontend.

Global `--workspace` (also `--cd`), `--profile` and `--config` select an explicit
control scope. Relative paths are captured before attachment; the client
authenticates the owner, then admits that scope against the same owner receipt
and its captured worker setting. An incompatible profile or config, missing
scope facts, or a changed owner fails explicitly. Without these options, the
acknowledged owner supplies the workspace. Thread listing remains store-wide.

For a fresh `thread resume` or `thread fork`, global `--provider`, `--model`,
`--approval-policy` and `--sandbox-mode` supply proposals to the existing owner
decoder and permission checks. The corresponding `--set` keys are `provider`,
`model`, `default_text_model`, `approval_policy` and `sandbox_mode`. Credentials
and endpoints stay with the owner: `--api-key`, `--base-url` and other per-run
settings are refused for these controls. Configure and authenticate the owning
Runtime before using them. With a retained `--operation-key`, newly supplied
model, provider, policy or sandbox proposals are refused; recovery observes
the original admitted intent.

Interactive `codewhale resume` and `codewhale fork` use the same canonical
history operation. An inactive local owner is shut down and joined before the
existing TUI acquires the saved-session lease and store. A live owner or an
uncertain handoff refuses attachment. `--operation-key <KEY>` recovers the
original outcome, including completing a verified prepared target; an
unprepared or unverifiable result preserves uncertainty.

### Changing config

`app/config/set` and `app/config/unset` write the change to the config file
before replying: the `--config` path, or the default `config.toml` used by the
canonical owner when no `--config` is given. They change user settings that
outlive the app-server, not just this session. The change is applied to the
file as it is on disk, so edits saved by other processes are kept. If the file
cannot be read, parsed, or written, the reply is `ok: false` and nothing
changes; a file that does not parse is not rewritten, so fix it by hand (or
`app/config/reload` after fixing it). Over HTTP `/app`, a rejected key or value
is `400` and a read or write failure is `500`.

### Answering a clarification question

When a headless turn calls `request_user_input`, the runtime emits a
`user_input.required` event carrying a `request_id`. Reply on the runtime API:

```
POST /v1/user-input/{thread_id}/{request_id}
```

The app-server control transport cannot accept that reply.
`app/request` with `SubmitUserInput` returns `ok: false` and
`error: "user_input_reply_unsupported"`. This is a property of the transport,
not an omission: while a turn is streaming, the stdio loop executes only
`thread/interrupt` and queues everything else, so an answer sent there would
wait on the very turn that is waiting for it.

## SDK contract

The app-server exists so an external SDK can answer — without scraping TUI
output — *what route ran, which provider/model/reasoning/permission profile was
effective, what events happened, how many tokens were used, and how the run
finished.* The durable Thread/Turn/Item data model already carries most of
this; the table maps each integration need to where a local client reads it.

| Integration need | Where it comes from | Status |
|---|---|---|
| Route / effective model / billing surface | `TurnRecord` + thread `model`; per-run `--provider`/`--model` overrides | available |
| Permission / sandbox / approval profile | thread `auto_approve`, sandbox + approval policy; `TurnRecord.permission_posture` + `TurnRecord.mode` for how *that* run was governed (the thread's own `mode` may have been switched since) | available |
| Run / thread / turn IDs | `thread_id`, `turn_id`, SSE event envelope | available |
| Event stream | `GET /v1/threads/{id}/events` (replay + live SSE) | available |
| Turn status / terminal classification | `TurnRecord.status` + error summary | available |
| Token usage | `TurnRecord.usage`; aggregate via `GET /v1/usage` | available |
| Action receipt (files, commands, web/MCP calls, agents, approvals and who decided, failures) | `GET /v1/threads/{id}/receipt`, `GET /v1/threads/{id}/turns/{turn_id}/receipt` | available ([RECEIPTS.md](RECEIPTS.md)) |

For one-shot/headless automation, prefer `codewhale exec` with explicit
`--provider <id> --model <id>` so a failure identifies the exact provider/model
pair. Use `app-server` when a local integration needs to start, resume, steer,
or interrupt turns, list models/capabilities, follow the event stream, or read
usage. Both paths share the same runtime, so route-effective model resolution
and the event vocabulary match.

### Release smoke

`scripts/release/app-server-smoke.sh` is the committed pre-release check:

```bash
scripts/release/app-server-smoke.sh                 # stdio health/capabilities probe (no tokens)
scripts/release/app-server-smoke.sh --matrix        # + print the configured provider/model matrix
scripts/release/app-server-smoke.sh --matrix --real # + exec a cheap sentinel per provider
```

The stdio probe runs against a throwaway config, so it never reads real keys.
The matrix discovers configured providers from `codewhale auth list`, skips
unconfigured providers, and maps a provider to a cheap sentinel model only when
it has a built-in cheap default. That built-in set is deliberately conservative
(currently `deepseek`, `zai`, `moonshot`, and `openai`); every other provider —
including `arcee`, `openrouter`, `xiaomi-mimo`, and `openai-codex` — is left
unmapped on purpose and must be given a model per run via `SMOKE_MODEL_<SLUG>`
rather than a guessed default (#3205). Any configured-but-unmapped provider
fails loudly in `--real` mode. `auth list` reports presence flags only and exec
output is passed through a redactor, so secrets are never printed. The parser is
covered by `scripts/release/app-server-smoke.test.sh` against a fake `codewhale`
binary.

## ACP stdio adapter: `codewhale serve --acp`

ACP JSON-RPC over newline-delimited stdio is a transport over the existing
RuntimeThreadManager and Engine. It has no provider/tool round loop, executable
registry, prompt composer, or conversation writer of its own. The server loads
the selected config/profile and plugin discovery once; each prompt uses a
canonical thread, Core turn, event timeline, approval waiter, and full Engine
session snapshot.

The editor surface supports `initialize`, `session/new`, `session/list`,
`session/load` (including durable ID prefixes), `session/prompt`, `session/cancel`,
model discovery/selection, and the advertised mode/model config options.
`session/new` creates a durable bare UUID and empty checkpoint without calling a
provider. At most 64 idle transport bindings are retained; eviction keeps the
durable conversation. Resume deduplicates against the existing thread binding.
Full histories, tool pairs, signatures, media and partial-effect receipts are
saved under the same checkpoint guard and session write lease used by HTTP.
ACP display text may be shortened; the saved history is the full Core snapshot.

The trusted local ACP profile narrows Core to file/search/git/patch and admitted
foreground shell tools. Shell requires both editor terminal support and operator
`allow_shell`; a requested external sandbox that is unavailable removes shell.
MCP, dynamic tools, task/PTY/background shell, interpreter, subagent and RLM
lifecycles are unavailable on this transport. Built-in overrides remove the
whole compatibility alias family. Final dispatch rechecks the profile, so a
fabricated alias, hook rewrite or omitted catalog entry cannot bypass it. Plan
remains read-only under Full Access. Full Access and the ordinary approval
posture are server-owned read-only options; the editor cannot widen them.

Tool updates begin `pending`. `in_progress` means Core reached final dispatch;
`completed`/`failed` and typed image blocks come from the actual Core result.
`session/request_permission` correlates a private JSON-RPC ID to the same
Runtime-minted pending approval and Core execution ID. Only an exact live
`allow-once` response can release that waiter; wrong IDs are ignored, invalid
options deny, and cancellation withdraws it. ACP grants neither remembered
permissions nor Native capabilities. Core typed rules, strict hooks, repo law,
Headless Auto-Review and hard floors remain in force, including the absence of a
workspace-write carve-out; a guardian-only decision refuses on this host.

Replay uses the existing bounded event reader and sequence deduplication. A
replay gap, closed owner or terminal store fault is an explicit error, never a
re-execution. Input is serviced between replayed events; each transport write has a 30-second
deadline. Cancel, EOF and writer
failure interrupt only this transport's claimed Core turn; settlement uses its
actual terminal receipt and preserves any completed effects. An unconfirmed
cancellation does not fabricate success. `stopReason` is `end_turn`, `cancelled`,
or typed `max_turn_requests`; Core failure remains an error. Prompts use at most
50 model steps (a lower configured limit still wins) and Core's bounded final
report response; this profile does not dispatch autonomous goal continuations.

ACP presently claims its own exclusive canonical Runtime owner. If another
process holds that store, startup refuses. A saved conversation bound to another
Runtime store also refuses; authenticated cross-process owner attachment remains
unqualified and ACP does not copy a live conversation into a random store. This
does not expose every `/v1/*` steering, job or control method to an ACP editor.
Use `codewhale app-server --http` for that full runtime API.


## Capability endpoint: `codewhale doctor --json`

Returns a JSON object describing the current installation's readiness state.
Suitable for health-check polling from a macOS workbench. This command is
strictly structural and offline: it does not load workspace credential
`.env` files, inspect credential environment values, open secret/OAuth files,
probe an OS keyring, contact providers, or start MCP processes.

```bash
codewhale doctor --json
```

### Response schema (key fields)

| Field | Type | Description |
|---|---|---|
| `version` | string | Installed version (e.g. `"0.8.9"`) |
| `config_path` | string | Resolved config file path |
| `config_present` | bool | Whether the config file exists |
| `paths` | object | Canonical config, settings, state, sessions, logs, automations, and secrets paths |
| `secret_backend` | object | Metadata-only file-store shape, or literal `unknown` / `not_probed` for system and unsupported backends |
| `workspace` | string | Default workspace directory |
| `legacy_state.primary_root` | string | Primary Codewhale state root inspected for known state paths |
| `legacy_state.legacy_root` | string | Legacy `.deepseek` state root inspected for known state paths |
| `legacy_state.needs_attention` | bool | Whether known `~/.deepseek` state paths need review or the read-only session recovery diagnostic found missing destination filenames / could not complete |
| `legacy_state.legacy_only_count` | number | Count of known state paths present only under the legacy root |
| `legacy_state.dual_present_count` | number | Count of known state paths present under both primary and legacy roots |
| `legacy_state.entries` | array | Per-path migration status: `{name, primary_present, legacy_present, status}` |
| `legacy_state.session_recovery.status` | string | `isolated`, `no_legacy_sessions`, `migration_pending`, `migration_incomplete`, `migration_complete`, or `scan_failed` |
| `legacy_state.session_recovery.read_only` | bool | Always true; doctor never invokes session migration or modifies either session directory |
| `legacy_state.session_recovery.chat_contents_read` | bool | Always false; comparison is based only on top-level `.json` filenames and filesystem metadata |
| `legacy_state.session_recovery.checkpoint_internals_scanned` | bool | Always false; `sessions/checkpoints/` and all other directories are skipped |
| `legacy_state.session_recovery.recoverable_files` | array | Bounded sample of up to 100 missing destination filenames with source and destination paths; no chat payloads |
| `legacy_state.session_recovery.recoverable_file_count` | number | Total missing destination filename count, including entries beyond the bounded sample |
| `legacy_state.session_recovery.recoverable_files_truncated` | bool | Whether more than 100 recoverable filenames were found |
| `legacy_state.session_recovery.recovery_command` | string or null | `codewhale sessions` when additive automatic recovery is available; null for isolated, complete, empty, or failed scans |
| `api_key.source` | string | Structural source state: `config_declared`, `env_declared`, `external_auth_declared`, `secret_store_unprobed`, `secret_store_unavailable`, `oauth_unprobed`, `external_consent`, `none`, `local_runtime`, or `unknown`; declarations are not availability proof |
| `api_key.availability` | string | Literal `present`, `not_required`, `not_probed`, `unavailable`, or `unknown`; only `present` and `not_required` certify structural Setup/Fleet credential readiness |
| `base_url` | string | Provider URL authority only (`scheme://host[:explicit-port]`); userinfo, path, query, and fragment are omitted |
| `default_text_model` | string | Default model |
| `memory.enabled` | bool | Whether the memory feature is on |
| `memory.path` | string | Path to memory file |
| `memory.file_present` | bool | Whether memory file exists |
| `mcp.config_path` | string | MCP config file path |
| `mcp.present` | bool | Whether MCP config exists |
| `mcp.probe_scope` | string | `configuration`; doctor does not start MCP servers |
| `mcp.live_health_checked` | bool | Always false for doctor JSON |
| `mcp.servers` | array | Per-server structural result and counts plus separate `checks`; URL userinfo/path/query/fragment and command argv, environment, header, and token values are never emitted, and all live stages are `not_checked` |
| `skills.selected` | string | Resolved skills directory |
| `skills.global.path` / `.present` / `.count` | — | Codewhale global skills dir (`~/.codewhale/skills`, with legacy `~/.deepseek/skills` support) |
| `skills.agents.path` / `.present` / `.count` | — | Workspace `.agents/skills/` dir |
| `skills.agents_global.path` / `.present` / `.count` | — | agentskills.io global skills dir (`~/.agents/skills`) |
| `skills.local.path` / `.present` / `.count` | — | `skills/` dir |
| `skills.opencode.path` / `.present` / `.count` | — | `.opencode/skills/` dir |
| `skills.claude.path` / `.present` / `.count` | — | `.claude/skills/` dir |
| `tools.path` / `.present` / `.count` | — | Global tools directory |
| `plugins.path` / `.present` / `.count` | — | Global plugins directory |
| `sandbox.available` | bool | Whether sandbox is supported on this OS |
| `sandbox.kind` | string or null | Sandbox kind (e.g. `"macos_seatbelt"`) |
| `storage.spillover.path` / `.present` / `.count` | — | Tool output spillover dir |
| `storage.stash.path` / `.present` / `.count` | — | Composer stash |

### Example

```json
{
  "version": "0.8.9",
  "config_path": "/Users/you/.codewhale/config.toml",
  "config_present": true,
  "workspace": "/Users/you/projects/codewhale-tui",
  "api_key": {
    "source": "secret_store_unprobed",
    "availability": "not_probed"
  },
  "base_url": "https://api.deepseek.com",
  "default_text_model": "deepseek-v4-pro",
  "memory": {
    "enabled": false,
    "path": "/Users/you/.codewhale/memory.md",
    "file_present": true
  },
  "mcp": {
    "config_path": "/Users/you/.codewhale/mcp.json",
    "present": true,
    "servers": [
      {"name": "filesystem", "enabled": true, "transport": "stdio", "args_count": 2, "env_count": 0, "status": "ok"}
    ]
  },
  "sandbox": {
    "available": true,
    "kind": "macos_seatbelt"
  }
}
```

## HTTP/SSE runtime API: `codewhale app-server --http`

```bash
codewhale app-server --http [--host 127.0.0.1] [--port 7878] [--workers 2] [--auth-token TOKEN] [--insecure-no-auth]
codewhale app-server --mobile [--host 127.0.0.1] [--port 7878] [--auth-token TOKEN]
codewhale app-server --mobile --host ::1 [--port 7878] [--insecure-no-auth]
codewhale web [--port 7878]

# Compatibility aliases — identical server, serve flag names:
codewhale serve --http   [...] [--insecure]
codewhale serve --mobile [...] [--insecure]
```

Defaults: host `127.0.0.1`, port `7878`, 2 workers (clamped 1–8).

The server binds to `localhost` by default. Configuration is via CLI flags —
there is no `[app_server]` config section.

`/v1/*` routes require a bearer token unless `codewhale app-server` is started
with `--insecure-no-auth` on a loopback bind such as `127.0.0.1`. Mobile mode
is loopback-only: non-loopback hosts are rejected until Runtime has a TLS or
verified-overlay transport boundary. The `codewhale serve` compatibility aliases
use `--insecure` for the same loopback escape hatch.
Pass `--auth-token TOKEN` or set `CODEWHALE_RUNTIME_TOKEN=TOKEN` before starting
the server; `DEEPSEEK_RUNTIME_TOKEN` remains a compatibility alias. If neither
is set, the process generates a Runtime token for that process and does **not**
print it. `/health`, `/v1/runtime/info`, and an enabled static client shell
remain public; Runtime mutations and thread data stay behind `/v1/*`
authentication. `/mobile` returns 404 when mobile mode is disabled and serves
the unchanged static shell when it is enabled.

Authenticated clients can provide the token as `Authorization: Bearer TOKEN`,
`X-Codewhale-Runtime-Token: TOKEN`, the legacy
`X-DeepSeek-Runtime-Token: TOKEN`. Query-string and raw Runtime-token cookie
authentication are not supported.

### Local browser client

`codewhale web` starts the canonical Runtime API on `127.0.0.1`, serves
dependency-free assets embedded in the binary, prints a single-use launch URL,
and asks the operating system to open that URL in the default browser. If the
browser does not open, the printed URL remains usable for ten minutes. The
command cannot bind to a non-loopback host and cannot run with Runtime auth
disabled.

The browser-launch URL contains a random, short-lived, one-time bootstrap
capability, never the Runtime token. A loopback request exchanges that
capability for a
`codewhale_web_session=…; HttpOnly; SameSite=Strict; Path=/` cookie backed by a
single process-local server session that expires 12 hours after the server
process starts, consumes the capability immediately, and redirects to `/`.
Reused, expired, malformed, or
non-loopback bootstrap attempts fail closed. The Runtime bearer token is not
placed in rendered HTML, browser storage, logs, URL queries/fragments, or
browser-launch arguments. The one-time bootstrap capability is printed in the
local terminal and transits the OS browser launcher's argument list. A same-user
process could race the browser to the exchange, which is why the capability is
single-use, loopback-only, and expires after ten minutes — and why a same-user
attacker has strictly easier local avenues than this race.
Web fetches require the session cookie plus an origin-scoped request proof;
streams use a fresh single-use ticket. The initial redirect carries the proof
in a fragment, which the client removes and saves in origin-scoped
`sessionStorage`. On reload or in a second tab, an authenticated `GET /` also
embeds the proof in a meta tag when `Sec-Fetch-Site` is `same-origin` or `none`
(direct navigation). The page uses `no-store`, disallows framing, and grants no
cross-origin read access. This lets a new tab recover without reusing the
bootstrap URL, including when storage is unavailable. Clients without Fetch
Metadata can only reuse the fragment or their existing stored proof; recovery
does not extend the server session or replace an expired cookie.
Cross-origin Fetch Metadata or a mismatched Origin is rejected on web API
requests. Explicit bearer and Runtime-token header clients keep their existing
behavior. Transient stream-ticket failures retry with capped backoff; HTTP
401/403 stops ticket retries until a fresh session is opened.

The embedded client provides a responsive thread/search rail, Runtime-owned
session facts, transcript and tool receipts, and a bottom composer. It can
create, select, rename, and archive threads; choose a provider and model for a
new thread without changing Runtime defaults; start or steer turns; interrupt
work; resolve approvals; and answer Runtime user-input requests. Selection
loads `GET /v1/threads/{id}` first, then opens the replayable event stream with
`since_seq=latest_seq`; reconnection advances from the newest accepted sequence
and drops duplicates or events from a stale selection. The thread detail
snapshot includes `pending_approvals`, `pending_user_inputs`, and
`pending_dynamic_tool_calls`; clients must hydrate those fields before
subscribing so a reload cannot strand work whose request event is at or before
`latest_seq`. Resolution is also published as `approval.decided`,
`user_input.answered`, `user_input.canceled`, `tool_call.resolved`,
`tool_call.canceled`, or `tool_call.timeout` for already-connected clients.

An existing thread's model, mode, permission posture, workspace, and branch are
display-only in this client. Files/Changes, PTY/terminal, preview, artifacts,
provider login or global-default switching, Fleet creation, and
undo/retry/restore controls are not built into this page. The native desktop
client covers them through the workspace-file, turn-artifact, terminal and
workspace-restore routes documented here.

### Mobile control page

`codewhale serve --mobile` starts the same HTTP/SSE runtime API and serves a
phone-friendly control page at `/mobile`. It binds only to loopback
(`127.0.0.1` or `::1`); a non-loopback host is rejected because this Runtime
surface does not yet provide TLS or a verified overlay transport. The static
HTML page contains no Runtime bearer and is not itself token-gated. When
Runtime auth is enabled, the CLI prints a short-lived, single-use loopback
bootstrap URL. That capability creates a 30-minute, process-local
`Max-Age=1800; HttpOnly; SameSite=Strict` mobile session cookie plus origin-scoped browser
proofs. A sibling port that receives the host-scoped cookie cannot use it by
itself. The page can also exchange an explicitly entered bearer once, then
clears it rather than storing it in browser storage or a cookie. EventSource
connections use separate short-lived, single-use stream tickets.

The mobile page can list/create threads, send prompts, follow live SSE events,
steer or interrupt an active turn, and resolve normal tool approvals through
`POST /v1/approvals/{approval_id}`. It is a local-only convenience surface;
do not expose it directly to another device or public network until Runtime has
a TLS or verified transport boundary.

### Endpoints

**Health**
- `GET /health`

**Sessions** (durable session manager)
- `GET /v1/sessions?limit=50&search=<fuzzy>&include_archived=false&archived_only=false&workspace=<path>&sort=recent|name|size`
- `GET /v1/sessions/summary?…` (same query params; projected row shape)
- `GET /v1/sessions/{id}` (add `?peek=true&entries=12` for a bounded, redacted
  read-only peek instead of the full transcript). The full response carries
  `turn_outcomes` when a turn ended `Failed`: `{ status, error, ended_at,
  after_message_count }` per failure, oldest first, bounded to 64, with the
  error text the transcript showed and secrets redacted
- `PATCH /v1/sessions/{id}` (`{ "title"?: string, "archived"?: bool }`)
- `DELETE /v1/sessions/{id}`
- `POST /v1/sessions/{id}/resume-thread` returns the open thread that already
  holds the whole saved session (`200`), or seeds a new thread from it (`201`)
  when none does, including when the session grew after that thread opened it.
- `GET /v1/sessions/{id}/artifacts` and `GET /v1/sessions/{id}/artifacts/{artifact_id}?offset=&limit=`
  (see workspace files and session artifacts above; runtime-turn spills are
  read through `GET /v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}`)
- `POST /v1/sessions` (`{ "thread_id": string, "title"?: string }`) exports a
  thread as a saved session. It is idempotent: it writes only the document
  whose id is derived from the thread, creating it the first time (`201`) and
  updating it after (`200`), so a retry never makes a duplicate. A session the
  thread was resumed from is left unchanged.
- `PUT /v1/sessions` (`{ "thread_id"?: string, "session_id"?: string }`) saves
  a thread's live conversation. Naming a `session_id` that another thread is
  bound to returns `409 Conflict`.
- `GET /v1/sessions/repair` returns the last session-store repair summary, or
  `null` if none has run

Sessions and threads answer the same `include_archived` / `archived_only` pair
with the same meaning, and `search` is the same fuzzy match (title, id,
workspace — substring, then subsequence) the TUI session picker and the workbar
Sessions list use. All three surfaces run one projection
(`crates/tui/src/session_projection.rs`), so a listing cannot differ between
the terminal and the dashboard.

`GET /v1/sessions/summary` returns rows that are field-compatible with
`GET /v1/threads/summary` — `id`, `title`, `preview`, `model`, `mode`,
`workspace`, `archived`, `updated_at` — plus `message_count`, `total_tokens`,
`created_at`, `parent_session_id`, and `is_current`. One caveat stated plainly:
`preview` is the session's recorded **title**, not its last message. Session
metadata does not store a last message, and reading every transcript to
synthesise one would make a list view an unbounded read. Full transcript
preview lives in the TUI session picker, which reads one selected session.

`PATCH /v1/sessions/{id}` renames and/or archives a saved session and returns a
lifecycle receipt shaped like the thread patch receipt:

```json
{
  "session": { "id": "…", "title": "Renamed", "archived": true, "…": "…" },
  "changes": { "title": "Renamed", "archived": true }
}
```

`changes` lists only what actually moved, so a no-op patch is distinguishable
from an applied one. Archiving is durable and reversible: an archived session
stays on disk and stays loadable, disappears from default listings, and is
never chosen by `--continue` or by auto-resume. The route is the same writer
the TUI picker (`e`) and `/sessions archive <id>` use — there is no second
archive notion.

While a session is open in an interactive Codewhale process, that process holds
the authoritative copy in memory and rewrites the whole document on its next
autosave. `PATCH`, `PUT` and `DELETE` therefore fail closed on it with
`409 Conflict` rather than writing something that would be silently reverted.
Change it in the terminal instead. The open process holds a lock on the
session (`sessions/.late-usage/<id>.live`), so this holds whether the request
reaches the API inside that process or a separate `codewhale serve`.

The session store is repaired in the background at each launch and each
`codewhale serve` start. The repair gives a "Recovered:" session to each thread
in a Runtime store that no session is bound to. It unbinds threads whose session
document is gone; they then load from their own turns. It moves unreadable
documents, empty unbound stores, and old artifact directories that no session
names to `sessions/.set-aside/<run>/`, and writes a `MANIFEST.jsonl` there.
Nothing is deleted. `GET /v1/sessions/repair` and `codewhale doctor` report the
last run; `codewhale doctor --repair-sessions [--dry-run]` runs one on demand.

`GET /v1/sessions/{id}?peek=true` returns a bounded, redacted, read-only view
instead of the transcript: at most 12 entries of at most 400 characters each
(`&entries=N` lowers the budget, never raises it past the cap), tool calls and
results summarised to a name and a size rather than inlined, and
credential-shaped substrings masked. `omitted_before` reports how many earlier
messages were dropped. The payload carries `"live": false` and deliberately has
no turn status, `running`, or `active` field — a saved session is a recording,
and live state comes only from a resumed thread's SSE stream.

**Threads** (durable runtime data model)
- `GET /v1/threads?limit=50&include_archived=false&archived_only=false`
- `GET /v1/threads/summary?limit=50&search=<optional>&include_archived=false&archived_only=false&thread_ids=<id>,<id>`
- `GET /v1/threads/running`
- `GET /v1/goals?limit=50&cursor=<opaque>&status=<status>` — read-only index of
  the per-thread goal records across threads, newest update first. Returns
  `{"goals": [ThreadGoal...], "next_cursor": "<opaque>"}`; `next_cursor` is
  absent on the last page. `limit` is 1-200 (default 50); `status` is one of
  `active`, `paused`, `blocked`, `usage_limited`, `budget_limited`, `complete`;
  goals whose thread no longer exists are omitted; a bad `status` or `cursor`, or a cursor issued under a different `status`, is `400`. Same auth as every other `/v1` read.
  Goal creation, completion and removal stay on `/v1/threads/{id}/goal`.
- `GET /v1/threads/{id}/notices`
- `DELETE /v1/threads/{id}/notices/{notice_id}`
- `POST /v1/threads`
- `GET /v1/threads/{id}`
- `PATCH /v1/threads/{id}` (see body shape below)
- `POST /v1/threads/{id}/resume`
- `POST /v1/threads/{id}/fork`
- `GET /v1/threads/{id}/receipt` — what the thread did, one entry per action
  (read-only; shape in [RECEIPTS.md](RECEIPTS.md))
- `GET /v1/threads/{id}/turns/{turn_id}/receipt` — the same, for one turn;
  `404` for an unknown thread or a turn that is not this thread's

`thread_ids` is a comma-separated list of up to 200 IDs (each at most 128 bytes).
The route filters exact IDs before applying `limit`, preserving newest-first
order. This is a selection filter, not an ownership or authorization check;
the Runtime bearer token remains the access boundary.

`POST /v1/threads` accepts optional execution defaults in addition to the
provider, model, workspace, and permission fields:

```json
{
  "model_provider": "openai-codex",
  "model": "gpt-5.6",
  "reasoning_effort": "high",
  "allowed_tools": ["read_file", "search"]
}
```

`reasoning_effort` uses the canonical Runtime vocabulary (`auto`, `off`,
`low`, `medium`, `high`, `xhigh`, `ultra`, or `max`; documented compatibility
aliases are accepted and persisted canonically). `allowed_tools` is a
model-visible allowlist. Omitting it keeps the normal configured catalog; an
explicit empty array (`"allowed_tools": []`) exposes no tools to the model.
Both fields are additive: older thread records and clients that omit them
retain their previous behavior.

`GET /v1/threads/summary` is the read-only summary surface used by the VS Code
Agent View. `search` matches thread `id`, `title`, and `model` (and, when the
title is unset, the latest turn's input summary — the displayed title). It
does not scan turn or item bodies: `preview` is filled only after a match, so
a dashboard keystroke is not a whole-store read per thread. Each item includes
`id`, `title`, `preview`, `model`, `mode`, `archived`, `updated_at`,
`latest_turn_id`, `latest_turn_status`, plus workspace metadata:

```json
{
  "id": "thread_...",
  "title": "Implement MCP status count",
  "preview": "The TUI footer should count project MCP servers...",
  "model": "deepseek-v4-pro",
  "mode": "agent",
  "branch": "feature/runtime-api",
  "head": "abc1234",
  "dirty": false,
  "workspace": "/Users/you/projects/codewhale",
  "archived": false,
  "updated_at": "2026-06-06T05:43:00Z",
  "latest_turn_id": "turn_...",
  "latest_turn_status": "completed"
}
```

`branch` is resolved from the thread workspace at request time and may be
`null` when the workspace is not a Git repository or the branch cannot be read.
`head` is the current short Git commit for that workspace when available.
`dirty` is true when the workspace has staged, unstaged, or untracked changes.
`workspace` is included so editor clients can show when an agent lane is working
outside the current VS Code folder.

Thread forks are sibling runtime threads, not an in-place tree projection.
`thread.forked` events include `source_thread_id`; internal backtrack-aware
forks may also include `backtrack_depth_from_tail` and `dropped_turn_id`, and
a fork anchored to a named turn (`/fork-at-turn`) reports them with the depth
resolved from that turn, and names the first user turn it dropped (not the
anchor, which a named-turn fork keeps, and not a prompt-less turn such as a
manual compaction that sits between them).
Thread list and summary responses remain flat in v0.8.40, so clients that need
a graph should reconstruct it from events instead of assuming list order is a
complete tree.

`GET /v1/threads/running` is the running-work accounting surface
(#6180): threads with at least one queued or in-progress turn, each with
`thread_id`, `model`, `title`, and `active_turns` (`turn_id` + `status`).
Background-capable clients use it for quit/background decisions — one call,
no inference from latest-turn status. Archive state is ignored (archiving
has no quiescence gate); an empty array means no owned work is live.

`GET /v1/threads/{id}/notices` is the per-thread active-notice surface
(#6180): the TUI-visible conditions a watch-only client must surface —
`subagent-terminal` (a child settled), `elevation-needed` (a tool call is
blocked on elevation), `model-notify` (the model asked the user to come
back) — each with `turn_id` and a `subject` id for targeting. Notices are
in-memory session state, bounded to 32 per thread (oldest evicted), and
never persisted. Clearing: elevation auto-clears when its tool call
completes; terminal/notify clear on `DELETE .../notices/{notice_id}`
(204, unknown ids 404). Unknown threads 404 on both endpoints.

`archived_only=true` returns archived threads only (mutually overrides
`include_archived`). Default behavior is unchanged: `include_archived=false`
and `archived_only=false` returns active threads. Added in v0.8.10 (#563).

`PATCH /v1/threads/{id}` body — every field is optional, missing means
"no change". At least one field must be present. `title` and `system_prompt`
accept an empty string to clear a previously-set value. Added in v0.8.10 (#562):

```json
{
  "archived": true,
  "allow_shell": false,
  "trust_mode": false,
  "auto_approve": false,
  "model": "deepseek-v4-pro",
  "mode": "agent",
  "title": "User-set thread title",
  "system_prompt": "You are a useful assistant.",
  "model_provider": "custom",
  "model_provider_id": "lm-studio"
}
```

`model_provider` switches the provider the thread's future turns use. It
takes a built-in kind (`deepseek`, `xai`, ...) or a configured route name, as
`/provider` does. `model_provider_id` names one exact `[providers.<id>]` table
and wins over a route name. The target route is resolved and its client
preflighted before anything is saved, so an unknown or credential-less
provider is refused and nothing changes. Without `model`, the thread takes the
new provider's default model; an `auto` thread stays `auto`. The loaded
engine and conversation history are kept, and the next turn installs the new
route.

A change of effective permission adds a completed `status` item to the thread's
latest turn and publishes a `thread.permissions_changed` event (`from`, `to`,
`source`).

**Turns** (within a thread)
- `POST /v1/threads/{id}/turns`
- `POST /v1/threads/{id}/turns/{turn_id}/steer` - inject guidance into the running turn. The response is a receipt for what actually happened, not for what was attempted; see [Steer delivery](#steer-delivery).
- `POST /v1/threads/{id}/turns/{turn_id}/interrupt`
- `GET /v1/threads/{id}/turns/{turn_id}/artifacts` - what the turn produced: typed references plus the workspace-delta state. See [Turn artifacts](#turn-artifacts).
- `GET /v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}?offset=&limit=&revision=` - read one reference from the workspace, the post-turn snapshot, or the session artifact directory.
- `POST /v1/threads/{id}/compact` (manual compaction)
- `POST /v1/threads/{id}/undo` - fork the thread with the last N turns removed (`{"depth": N}`, default 0 = last turn only); returns the forked thread plus `original_user_text` so a GUI can pre-populate the input box
- `POST /v1/threads/{id}/fork-at-turn` - fork at one named user turn (`{"turn_id": "turn_…"}`, as `GET /v1/threads/{id}` reports it). The fork *keeps* that turn and every turn before it, and drops the turns after it; naming the last turn therefore keeps the whole conversation. The receipt is `/undo`'s (`thread`, `original_user_text`, `original_user_images`), carrying the *first dropped* user turn's prompt — what was asked next, even when a prompt-less turn such as a manual `/compact` sits between — so a client can put it back in the composer for editing. The source thread, its session document and the workspace are untouched, and there is no file rollback: a fork is a sibling conversation, and rewinding the workspace would rewind the branch left behind with it. Clients should name the turn instead of computing a `depth` — the transcript they render and the turn list this cuts are not the same list (steers, image-only prompts and injected handoffs each sit on one side only), and a client-side count that is off by one forks the wrong prefix while answering `201`. `400` when the turn is not a user turn of that thread.
- `POST /v1/threads/{id}/patch-undo` - rolls back the files the dropped turns changed, then the same fork (`{"depth": N}`); returns `patch_result` (`files_restored`, `summary`, `snapshot_label`) alongside the forked thread. See [Workspace restore endpoints](#workspace-restore-endpoints) for ownership, the trust, admission and abort rules, and the `error.code` values of a refusal.
- `POST /v1/threads/{id}/file-revert` - restore exactly one file from one restore point the thread owns (`{"path", "snapshot_id", "expected_hash"}`); never forks the conversation. See [Workspace restore endpoints](#workspace-restore-endpoints).
- `POST /v1/threads/{id}/retry` - fork with the last N turns removed and immediately start a new turn (`{"depth": N, "prompt": "..."}`; `prompt` overrides the original user text, which is re-used when omitted)

`POST /v1/threads/{id}/turns` accepts the same optional
`reasoning_effort` and `allowed_tools` fields as per-turn overrides:

```json
{
  "prompt": "Review this change without running tools.",
  "operation_key": "cwc-request-01J7Y6Q9W4",
  "reasoning_effort": "max",
  "allowed_tools": []
}
```

The same `model_provider` / `model_provider_id` fields on a turn route that
one turn through another provider. The saved thread keeps its provider.
Without `model`, the turn uses that provider's default model (an `auto` thread
stays `auto`). The override is always preflighted and is part of the
`operation_key` fingerprint.

Account-owned model turns require `capabilities.account_model_owner: true` before
submission. Set `account_model_owner` to the signed-in account ID, both
`model_provider` and `model_provider_id` to `codewhale`, and `model` to the exact
`provider/model` account route. The Runtime requires current model access for
that same secure account session and uses only that account credential; missing,
expired or revoked access is refused. Local keys and other configured routes are
not substitutes. A supplied profile constitution must belong to the same account.
The owner participates in the durable operation fingerprint; replay of an
already completed exact operation returns its receipt before fresh credential
admission. The account gateway resolves the selected provider's current saved
API key at use. Ordinary turns and saved Chat defaults retain their existing
behavior. The Runtime Chat relay carries the same assertion as `accountModelOwner`.

Resolution is deterministic: a turn override wins over the thread default,
which wins over the Runtime's normal configuration. For tools, reaching normal
configuration means the ordinary configured catalog; `[]` is never treated as
missing. Reasoning is normalized only after the exact provider/model route is
resolved, and `auto` remains a per-prompt reasoning decision even when the
thread uses a fixed model. The request still enters the existing
`Op::SendMessage` path and the single `Engine::run_turn` loop.

Image input uses the same turn path: `"images": [{"mime": "image/png",
"dataBase64": "..."}]`. Clients must first observe
`capabilities.turn_image_inputs: true` in `/v1/runtime/info` (or the isolated
Runtime Chat relay catalog). Older HTTP runtimes ignore unknown fields, so a
successful text response is not evidence that an attachment was accepted.
The field is omitted when empty. It is also accepted by app-server
`thread/message`, `thread/request` messages, and prompt requests; that bridge
checks the underlying Runtime capability before forwarding image bytes.
Legacy remote Work commands do not support images and explicitly refuse them.

New inline images require a named model whose exact resolved route reports
`image_input: "supported"`; Auto and unknown/unsupported image routes are
refused before classifier or provider dispatch. This does not change the
existing trusted-local attachment behavior for routes with unknown capability.
A nonempty prompt is required. Inputs are limited to 10 images, 4 MiB decoded
bytes per image, 5 MiB total, and an 8 MiB JSON body. PNG, JPEG, GIF and WebP
must have matching MIME, canonical padded base64 and valid bounded image
content: at most 8192 pixels per dimension, 33,554,432 pixels total and 64 MiB
decoder allocation. The Runtime does not fetch paths or URLs from this field.
Malformed images refuse the whole turn; callers can retain the draft for
correction. Relay command polling uses an 8 MiB response budget; the sender
must paginate by serialized bytes without advancing past unserved commands.

Accepted image bytes and order are retained in the existing turn records and
reconstructed after restart, import and fork. Retry retains those images even
when its optional `prompt` changes the text; undo responses include
`original_user_images` when present. Image-bearing records require schema v3,
which older readers refuse. Text-only records and operation fingerprints retain
their prior representation. Validated stored local images retain the existing
5 MiB per-image ceiling and prior aggregate/count semantics on import/retry;
this internal storage authority does not
relax exact model or permission checks. Image bytes, MIME and order participate in request
identity, so changing an image under the same operation key conflicts.
Compaction can summarize older context; retaining the original attachment does
not promise that every later model request includes it. Image pixels are not
subject to text-secret redaction.

`operation_key` is an optional idempotency key for clients that may lose an
HTTP response after the Runtime accepted a turn. It is scoped to the current
Runtime store and thread, may contain at most 128 UTF-8 bytes, and may not be
empty or contain surrounding whitespace or control characters. Omitting it
preserves the legacy create-a-new-turn behavior.

The first accepted request durably binds a SHA-256 fingerprint of the key to
the Runtime turn id and a canonical request fingerprint before sending the
existing `Op::SendMessage`. An exact retry returns that original turn in the
normal `{ "thread": ..., "turn": ... }` response and emits no second engine
operation, item, or lifecycle sequence. Reusing the key on the same thread
with a different provider/model, prompt, reasoning policy, tool allowlist or
dynamic-tool schema, environment, or permission policy fails closed with
`409 Conflict`. The same caller key may be used independently on another
thread.

Only the scoped key fingerprint, request fingerprint, thread id, and turn id
are stored in the Runtime's private turn-operation index. The raw key is never
persisted or logged, and request bodies, credentials, and attachments are not
copied into that index. Existing thread/turn persistence remains the source of
the returned turn after a process restart.

**Exact accepted-turn lookup**

`GET /v1/threads/{id}/turn-operations/{operation_key}` uses the same Runtime
authentication as turn submission. URL-encode each path segment. It returns
`200 OK` with the existing bare `TurnRecord` (the `turn` object in the POST
response), identified by that exact thread and operation key. It does not use
the thread's latest turn or require the original request body or current route
settings to match.

- `404 Not Found`: no binding exists for that thread/key, or persisted identities
  do not match. These cases share a generic response.
- `409 Conflict`: admission holds the operation claim, or its durable binding
  is incomplete. Retry the lookup; this response does not authorize another turn.
- `400 Bad Request`: the thread ID or operation key is malformed. The key uses
  the same 128-byte and whitespace/control-character rules as POST.
- `500 Internal Server Error`: storage or the existing claim lock cannot be
  checked safely. This is not evidence that the operation is absent.

The lookup holds a shared read lock on the existing operation claim while
reading the binding and turn. It creates no files, starts no engine, emits no
events, and performs no replay or recovery. Normal Runtime startup may recover
an incomplete admission before a later lookup, but GET itself never does so.

**Approvals**
- `POST /v1/approvals/{approval_id}` with body
  `{ "decision": "allow" | "deny", "remember": false }`
- `GET /v1/approvals?limit=` (1-500, default 100) is the read-only history:
  decided and still-pending approvals across all sessions, newest ask first.

Each history row carries `approval_id`, `tool_name`, `outcome`, `asked_at` and
`decided_at` (null while pending), plus optional `decided_by` (`user`,
`session_rule`, `posture`, `host`), `target` and `summary`. `outcome` is one of
`allowed_once`, `denied`, `timeout`, `cancelled`, `unavailable`,
`retry_with_policy`, or `pending`. `standing_rule` (`allow` or `deny`) is present
only when a remembered session rule or grant answered rather than a person
(`decided_by: session_rule`); `outcome` is unchanged. `target` is the host of a
URL or a workspace-relative path (absolute paths outside the workspace are
reduced to `.../<basename>`); `summary` is a one-line description of what was
asked. Both are redacted conservatively when the ask is recorded: a shell
command keeps only the program name and plain arguments, and any quoted,
`KEY=VALUE`, credential-like or unrecognised argument replaces all arguments
with `[args redacted]`. Whitespace is collapsed and text is capped at 120
characters. Rows recorded before these fields existed
omit `target` and `summary`.

`approval_id` is minted by the Runtime, not by the model or the provider. It is
an opaque `approval_<32 hex>` capability, unique per prompt, bound to the thread
that raised it, and single-use: the Runtime removes it when the decision is
delivered, when the prompt times out, or when the turn abandons it. Clients echo
the value they were given and must not construct, derive, or guess one.

It is deliberately **not** the provider's tool-call ID. Providers restart their
call-ID counters per response, so two threads can gate calls whose raw IDs are
byte-equal; keying approvals by that value let one thread's decision settle
another thread's call. The endpoint therefore performs one exact match on the
minted ID and has no fallback: a raw tool-call ID, an expired ID, or a replayed
ID that has already been settled all return `404` and reach no engine. A `404`
means the capability is not pending — it is not evidence about how the approval
was resolved; read `approval.decided` for that.

The raw provider call ID travels separately as `tool_call_id` on
`pending_approvals[]` and on the approval events. It is a correlator for
attaching a prompt to the tool row it gates, and never accepted as a decision.
Each thread-detail `pending_approvals[]` entry is
`{ "id", "turn_id", "tool_name", "description", "intent_summary"?, "tool_call_id"?, "summary"? }`,
where `id` is the capability above. `summary` (also on `approval.required`) is
a one-line description of the gated call built from the tool name and its
arguments only, never from model text ("Search the web for 'espresso'",
"Write notes/espresso.md"); paths inside the workspace are workspace-relative.
Clients show it first and keep the raw arguments behind it. For task and
automation create/update, `summary` also names the requested trust mode,
shell, auto-approve, mode and workspace.

`"remember": true` on an `allow` records a **session grant** for that tool and
argument class (the approval grouping key: a shell command family for a
simple, known command such as `git status` whose only options are value-free
ones like `-s` or `--porcelain` — a compound, wrapper, interpreter or
unrecognised command, one with any other option, or one whose arguments are
what runs or is installed (`go run`, `make`, `git bisect`, package installs)
is granted as its full normalized command, and a
shell interact or wait call as the exact call — a patch's file set, a `fetch_url` host, an MCP tool, a `web.run` action kind — for
`open`, the hosts it opened). Computer Use consent and `app_script` calls, and
any tool without a class, are granted for the exact call only. A grant never
changes the thread's permission posture. Later matching calls on the thread are
approved without a prompt: they still emit `approval.required`, then
`approval.decided` with `"auto": true` and the `grant_id`. Creating a grant
emits `approval.grant_added` with `{ "grant": { "grant_id", "tool_name",
"scope", "summary", "granted_at" } }`; thread detail lists live grants in
`approval_grants[]`. `DELETE /v1/threads/{id}/approval-grants/{grant_id}`
revokes one (emitting `approval.grant_revoked`); the next matching call
prompts again. Archiving or deleting the thread ends all of its grants
(archiving emits `approval.grant_revoked` for each; unarchiving does not
restore them). Grants live in memory for the Runtime process: a restart
forgets them, and a forced (non-bypassable) prompt is never answered by one.

**User input**
- `POST /v1/user-input/{thread_id}/{input_id}` with body
  `{ "answers": [{ "id": "question-id", "label": "Choice", "value": "Choice" }] }`

Submitted values are delivered to the active model turn but are deliberately
excluded from durable Runtime items and events. The settled tool item contains
only a neutral receipt and a machine-readable `response_redacted` marker. The
Runtime accepts only an exact pending `(thread_id, input_id)` request; an
unknown, concurrently settling, or already settled id returns 404 and is never
placed in the engine mailbox. It commits the secret-free
`user_input.answered` receipt before removing the snapshot-authoritative prompt
or delivering the answer to the engine. That settlement runs independently of
the HTTP connection, so disconnecting after submission cannot leave a prompt
half accepted. Terminal-turn cancellation follows the same receipt-before-
removal ordering through `user_input.canceled`.

**Client-executed dynamic tools**
- `POST /v1/threads/{thread_id}/turns/{turn_id}/tool-calls/{call_id}/result`

The thread and turn in the result route must match the pending call. A call is
settled at most once; wrong-route and duplicate results return 404. Terminal
lifecycle events carry identifiers and status only, never tool result content.
The Runtime commits the terminal lifecycle event before making a submitted
result available to the model. Result delivery, timeout, and terminal-turn
cancellation race through one settlement owner, so exactly one of these events
is durable for a call:

- `tool_call.requested` — the typed client-executed call became pending;
- `tool_call.resolved` — a result was durably accepted by the Runtime
  (`result_accepted: true`; `success` is result metadata, but result content is
  excluded);
- `tool_call.timeout` — no result won before the bounded wait expired;
- `tool_call.canceled` — the turn terminated before a submitted result won.

HTTP `202 Accepted` and `tool_call.resolved` share that durable-acceptance
meaning. Neither claims that the model consumed the result: a concurrent turn
shutdown may close the model receiver after acceptance. Once the Runtime has
accepted the result, that call is terminal and a duplicate result returns 404.

**Events** (SSE replay + live stream)
- `GET /v1/threads/{id}/events?since_seq=<u64>&replay_limit=<n>&progress=true`

Cursors:

- `since_seq` is the per-thread cursor: events with `seq > since_seq` are sent.
  Omitted (and no `Last-Event-ID`), the stream starts from the beginning of the
  thread's history.
- Every journal frame carries `id: <seq>`, so a browser `EventSource` can
  resume through the `Last-Event-ID` header it sends on reconnect. An explicit
  `since_seq` wins over the header, so a deliberate replay from `0` is never
  overridden by a stale id. A header value that is not a decimal integer is
  ignored.
- `replay_limit` (at most 4096) returns only the newest tail of the requested
  history; `previous_seq` on the first returned event advances past exactly the
  omitted history.

Durable history parsing runs off the async server workers and reaches SSE in
bounded batches of at most 256 events through a backpressured channel. Broadcast
delivery is only a wake-up optimization: a lagged receiver opens the same
bounded durable replay from its last accepted cursor.

`progress=true` adds `stream.progress` transport frames
(`{schema_version, event, kind, thread_id, seq, state}`, `state` is
`replaying` or `live`) at the current cursor and advertises them with
`x-codewhale-event-progress: 1`. The stream reports `live` only after durable
history and the already-queued live tail are both drained; broadcast-lag
recovery returns it to `replaying`. Progress frames never carry a new sequence
number.

Failures before the stream opens are ordinary HTTP errors with the JSON error
body, never SSE:

| Status | When |
| --- | --- |
| `401` / `403` | Missing or wrong Runtime credential |
| `404` | Unknown thread |
| `400` | `replay_limit` above 4096 |
| `500` | The durable history could not be opened (including a replay worker crash before the first cursor) |

Once the response is `200`, every end the server chooses is a final
`stream.end` frame; see [Ending and resuming a thread
stream](#ending-and-resuming-a-thread-stream).

**Snapshots** (side-git restore point listing + restore)
- `GET /v1/snapshots?limit=20`
- `POST /v1/snapshots/{id}/restore`

`/v1/snapshots` lists recent side-git restore points for the runtime workspace.
`limit` defaults to `20` and must be between `1` and `100`. `POST
/v1/snapshots/{id}/restore` restores workspace files from the snapshot and
returns `{"restored": "<snapshot-id>"}`. It is the direct operator surface for
the server's own workspace (the same action as the TUI's `/restore <N>`): it is
gated by the Runtime API bearer token, not by any thread's trust flag, and it
is refused with `409` while a turn is active in an overlapping workspace (see
below). A `pre-restore:` safety snapshot is taken first.

```json
[
  {
    "id": "snap_...",
    "label": "post-turn:1",
    "timestamp": 1780730580
  }
]
```

### Workspace restore endpoints

Three routes mutate workspace files from side-git snapshots. They share one
admission rule and one safety net, and they differ in scope and trust.

| Route | Scope | Trust | Forks the thread |
| --- | --- | --- | --- |
| `POST /v1/snapshots/{id}/restore` | whole server workspace | bearer token only (operator action) | no |
| `POST /v1/threads/{id}/patch-undo` | the files the dropped turns changed | thread `trust_mode` or `auto_approve` when files would change | yes |
| `POST /v1/threads/{id}/file-revert` | exactly one regular file | thread `trust_mode` or `auto_approve`, always | no |

**Admission.** A restore reserves the same admission the Runtime uses for
config reloads and session checkpoints, so no new turn starts and no saved
history changes while files are being rewritten. If any thread already has an
active turn in the same workspace, a nested checkout of it, or a parent of it,
the request is refused with `409` and the message `already has an active turn`.
The reservation is owned by the worker performing the Git mutation, so a client
that disconnects mid-request cannot release it early; the operation completes
or fails as a whole. Concurrent restores serialize. The reservation is
runtime-wide: while a restore's safety snapshot and checkout run, new turns,
steering, compaction and user-input delivery on every thread wait for it to
finish, so a large workspace can add seconds of latency elsewhere during a
restore. A thread whose workspace directory is not available (unmounted
volume, disconnected share, missing directory) is refused with `409` rather
than treated as having nothing to restore.

**Ownership.** A thread owns exactly the workspace restore points recorded on
its own turns. While a turn runs, the engine reports each snapshot it takes —
`pre_turn` before the turn, `tool` before and `post_tool` after each tool call
that may write (every call that is not read-only: file tools, shell commands,
programs, write-capable MCP tools), `post_turn` when it ends (always before
the turn settles) — and the Runtime appends it to the turn record's
`workspace_snapshots`, in order:

```json
"workspace_snapshots": [
  { "kind": "pre_turn", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d" },
  { "kind": "tool", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d", "tool_call_id": "call_…", "write_paths": ["src/lib.rs"], "changed_paths": [] },
  { "kind": "post_tool", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d", "tool_call_id": "call_…", "changed_paths": ["src/lib.rs"] },
  { "kind": "post_turn", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d", "changed_paths": [] }
]
```

`changed_paths` lists the workspace-relative paths whose content changed since
the turn's previous receipt — what happened in the span the receipt closes; it
is absent on `pre_turn` and when it could not be computed (a snapshot in
between failed). `write_paths` is set on the `tool` receipt of a file tool
(`write_file`, `edit_file`, `apply_patch`) to the paths the call declared, as
it named them; a tool without it (a shell command) may write any path. On a
user shell turn the `pre_turn` receipt carries the command's `tool_call_id`,
since the command runs from it to `post_turn`.

Each receipt is also published as a `turn.workspace_snapshot` event (payload:
the receipt). The engine runs every Runtime thread under the thread's own id,
across restarts and engine eviction, so `session_id` is the thread id for turns
the thread ran itself; it does not follow the thread's saved-session binding
(`PUT`/`POST /v1/sessions`, resume), which only names a document. A fork clones
its source's turn records and so owns the restore points of the turns it
inherited. Another thread's, or a TUI session's, snapshots in the same
workspace are never candidates. `tree_id` is the durable identity: a prune
rebuilds the side repo and rewrites every commit id but keeps each tree, and a
restore point resolves only to a stored snapshot with the same tree, session
tag and kind. The count prune after each snapshot keeps the newest 50
snapshots plus the newest 50 turn boundaries (`pre-turn:`/`post-turn:`), so a
turn with more tool calls than that, or a burst from another thread, never
pushes out a recent turn's own restore points. Turns recorded before receipts existed, turns imported by
`resume-thread`, and turns run with snapshots off or unavailable have none.

**Safety net.** Every restore first records a `pre-restore:<target>` snapshot
of the current workspace. That label is never a `/undo`, `patch-undo` or
`file-revert` candidate, so the net does not change what later undos select.
For `file-revert` the backup is mandatory: if it cannot be written, or the
requested file is excluded from it (for example by `.gitignore`), the request
fails and nothing is changed.

**`patch-undo`.** Undoes whole turns. For each dropped turn's `pre_turn` →
`post_turn` window, the paths that differ between the two snapshots must all be
the turn's own: changed only in the span of one of the turn's tool calls (a
`tool` → `post_tool` span, or a shell turn's whole window), and, inside a file
tool's span, a path that call declared. A path that changed while none of the
turn's tools could have written it — another thread, an editor, a background
process — is someone else's change, and the undo is refused rather than revert
it. Each of the turn's paths goes back to its content before the first dropped
turn that changed it, and nothing else is touched, so later work by the user or
another thread in the same workspace survives. Then the conversation forks
exactly as `/undo` does.

Snapshots never hold paths excluded by the workspace's `.gitignore` files or
the built-in snapshot exclusions (`node_modules/`, `target/`, `dist/`, build
caches, binary artifacts), nor paths outside the workspace. A dropped file-tool
call that declared such a path is refused with `path_not_snapshotted`, because
no snapshot can put it back. A shell command declares no paths: what it writes
under an excluded path (build output, dependency installs) is outside what
`patch-undo` restores and is not reported. `201` means either files were restored (`files_restored: true`, one
`<action> <path>` line per file in `summary`, `snapshot_label` naming the
pre-turn snapshot) or there was provably nothing to restore
(`files_restored: false`): every dropped turn ran here without a tool call,
changed no file, or its files are already back at their pre-turn content.
Anything that cannot be restored aborts the whole undo with `409`, nothing is
changed and no fork is published; `error.code` says why:

| `error.code` | Meaning |
| --- | --- |
| `restore_point_unavailable` | a dropped turn that may have changed files has no complete recorded restore point (older record, imported by `resume-thread`, snapshots off, or the snapshot failed) |
| `restore_point_pruned` | the restore point is no longer in the snapshot store |
| `path_not_snapshotted` | a dropped file-tool call wrote a path snapshots do not hold (ignored, built-in exclusion, or outside the workspace) |
| `workspace_changed_since_turn` | a path the turns changed was changed afterwards (or between two dropped turns), a path changed while a dropped turn ran but outside its own tool calls, or a path that is not a regular file |
| `restore_requires_trust` | there is something to restore and the thread is not in trusted mode or Full Access |
| `workspace_unavailable` | the workspace directory is not available |

For the first four a client can offer the conversation-only
`POST /v1/threads/{id}/undo` instead, and `file-revert` for individual files.
Snapshot repository, listing or comparison failures abort with `500` and also
preserve the conversation, so a turn is never dropped while its file changes
stay on disk. Depth and history are validated before any file changes. If the
fork cannot be persisted after files were restored, the response is a `500`
that names the restored snapshot; the original thread still holds the turn and
the `pre-restore:` snapshot holds the previous files.

**`file-revert`.** Request body:

```json
{
  "path": "src/lib.rs",
  "snapshot_id": "3f2a…40-or-64 hex…",
  "expected_hash": "sha256:<64 lowercase hex digits>"
}
```

- `path`: workspace-relative, or absolute inside the thread workspace. The
  name is literal (brackets, spaces and glob characters are filename bytes;
  Git runs with `--literal-pathspecs`). It must name a regular file: directories,
  symlinks anywhere in the path, and `.git` components are `400`.
- `snapshot_id`: the exact `tool` or `pre_turn` restore point of the change
  the user selected, from the thread's own turn records: a receipt's
  `snapshot_id` or `tree_id` (for a tool call, the receipt whose
  `tool_call_id` matches), or the current commit id `GET /v1/snapshots` lists
  for it. It must be a restore point recorded on one of this thread's turns;
  the server never picks "the newest snapshot that differs", because an
  unrelated newer snapshot can erase later user edits while leaving the tool's
  change.
- `expected_hash`: `sha256:` of the current file bytes the client displayed,
  or `absent` when the client saw the file as deleted. It is checked before the
  safety backup and again immediately before the mutation.

Responses:

- `200 {"path", "action", "snapshot_id", "snapshot_label"}` — `action` is
  `modified`, `recreated` (file was missing) or `removed` (the snapshot does
  not contain the file, so the file the tool created is deleted; its parent
  directories are left in place).
- `400`: malformed `snapshot_id`/`expected_hash`, path outside the workspace,
  or a path that is not a regular file on either side.
- `404`: unknown thread.
- `409`: thread not in trusted mode or Full Access; active turn in an
  overlapping workspace; workspace directory not available; snapshot unknown,
  pruned, not recorded on this thread's turns (another thread's or a TUI
  session's), or not a restore point (refresh the change record); file already
  matches the
  snapshot (nothing to revert); or the file changed after the reviewed
  `expected_hash` (refresh and review again). Nothing is changed in any of
  these cases.
- `422`: missing or mistyped body fields.
- `500`: Git or filesystem failure; a failure after the safety snapshot names
  that snapshot so the previous bytes can be recovered with
  `POST /v1/snapshots/{id}/restore` or `/restore`.

Capability probe: `GET` on the route returns `405` where the endpoint exists
and `404` on an older engine; clients treat any non-`404` as available and
degrade with an explanation otherwise.

**Compatibility stream** (one-shot, backwards-compatible)
- `POST /v1/stream`

**Tasks** (durable background work)
- `GET /v1/tasks`
- `POST /v1/tasks`
- `GET /v1/tasks/{id}`
- `POST /v1/tasks/{id}/cancel`

**Automations** (scheduled recurring work)
- `GET /v1/automations`
- `POST /v1/automations`
- `GET /v1/automations/{id}`
- `PATCH /v1/automations/{id}`
- `DELETE /v1/automations/{id}`
- `POST /v1/automations/{id}/run`
- `POST /v1/automations/{id}/pause`
- `POST /v1/automations/{id}/resume`
- `GET /v1/automations/{id}/runs?limit=20`

Create and update requests accept an optional `model`. When present, each
scheduled or manually triggered run uses that model; omitting it keeps the
runtime's default task model.

**Automation mutation preconditions.** `GET /v1/runtime/info` advertises
`capabilities.automation_mutation_preconditions: true`; require it before
relying on the behavior below (older Engines ignore both inputs).

- Every `AutomationRecord` carries an integer `revision`: `1` on create, `+1`
  on each `PATCH`, `pause` and `resume`; legacy records read as `0`. Scheduler
  bookkeeping (`next_run_at`, `last_run_at`) does not change it.
- `expected_revision` is an optional precondition on `PATCH`, `run`, `pause`
  and `resume`: a JSON body field on `PATCH`, or the `?expected_revision=N`
  query parameter on any of the four. On mismatch nothing is written and the
  response is `409` with
  `{"error":{"code":"revision_conflict","current_revision":N,...}}`. Omitting
  it behaves as before.
- `Idempotency-Key` (1-255 visible ASCII characters) is optional on `POST
  /v1/automations`, `PATCH`, `run`, `pause` and `resume`. The first successful
  response (2xx) is persisted in `idempotency.json` beside the automation
  store, bounded to 1024 entries and a 24 hour TTL. A retry with the same key
  and identical request returns the stored status and body with
  `Idempotency-Replayed: true` and performs no second create or run. The same
  key with a different request is `422` `idempotency_key_reuse`. Failed
  attempts are not stored, so they can be retried. Keys are scoped to the
  route and automation id. If the record cannot be persisted after the
  mutation committed, the success response is still returned (the failure is
  logged), so a retry of that request is not deduplicated. An unparseable
  `idempotency.json` is renamed to `idempotency.corrupt-<timestamp>.json` and
  that request fails with `500`; later requests start from an empty store.
  `DELETE` is not covered.

**Operate** (always-on named operation; same `OperateRecord` as CWC
`20de981` / PR #284)

- `GET /v1/operate` — current operation + plan board
- `POST /v1/operate` — create (`direction`, optional `burnRate`)
- `PATCH /v1/operate` — steer direction, `burnRate`, or `leadPlan`
- `PUT /v1/operate/plan` — set `leadPlan` (`{ slices: [...] }`)
- `POST /v1/operate/keepalive` — observe spend / burn; never stops
- `POST /v1/operate/cancel` — explicit cancel (`/v1/operate/stop` aliases);
  also pauses the `cw-operate` keepalive so nothing keeps spending after cancel
- `POST /v1/operate/auto-merge/check` — call landed
  `scripts/check-auto-merge.py --repo --pr --agent` (does not merge)

The operation record (`current.json`) is persisted under a cross-process
file lock with atomic temp+rename writes; every PATCH / keepalive / plan
save reloads the latest state inside the lock, so concurrent saves merge
instead of losing writes. A `PATCH` that changes `direction` invalidates
the recorded `leadPlan` (workers stop executing superseded slices) and
pulls the keepalive lead run forward to re-plan. `POST /v1/operate`
installs the hourly `cw-operate` keepalive and kicks its first lead-plan
run immediately instead of waiting out the first recurrence; credentials
resolve through the normal Z.ai provider resolution (config, `api_key_env`,
secret store, or provider env vars — blank values count as missing).

`burnRate` is `{ "kind": "usd_per_hour", "amountUsdPerHour": number }`,
a positive number, or `null` (unbounded). Status is
`planning | running | idle_blocked | cancelled`. Pace
(`unbounded | hold | throttle | widen`) is not a status: over-target
throttles, under-target widens, no wallet-cap stop. Idle-blocked only
for empty direction, awaiting lead plan, missing credentials, or a
human gate. Auto-merge is `scripts/check-auto-merge.py --repo … --pr …
--agent …` from codewhale-ops `origin/main` (exit 0), then
`scripts/auto-merge-pr.py`. Do not invent a second checker.

**Introspection**
- `GET /v1/workspace/status`
- `GET /v1/workspace/files/search?query=<partial>&limit=<1-100>` (see workspace file suggestions above)
- `GET /v1/workspace/files?path=<dir>&limit=<1-2000>`, `GET /v1/workspace/files/read?path=<file>&offset=&limit=`
  and `PUT /v1/workspace/files` (see workspace files and session artifacts above)
- `GET /v1/skills`
- `GET /v1/skills/{name}` — one skill's routing metadata (`source`,
  `invocation`, `aliases`, `bundled_tier`, `enabled`) plus its full
  `SKILL.md` body, so a client can compose an activation instruction for its
  own next turn the way TUI's `/skill <name>` does. `404` for a name no
  discovery root holds; `403` for a plugin snapshot whose authority is no
  longer current; native rows whose file has since been deleted also `404`
  rather than serving the stale body. Advertised as
  `capabilities.skill_detail` on `GET /v1/runtime/info`, which is the source a
  client should use rather than probing this path: a `404` here means "no such
  skill" and is indistinguishable from "no such route".
- `GET /v1/apps/mcp/servers`
- `GET /v1/apps/mcp/tools?server=<optional>`

Skill activation toggles are persisted under a cross-process transaction lock.
Each mutation reloads and merges the latest exact-name state before an atomic
write, and `GET /v1/skills` refreshes that shared state so another Codewhale
process's successful toggle is visible without restarting the Runtime API.

Skill rows on `GET /v1/skills` carry `invocation`, `aliases`, and
`bundled_tier` alongside the fields they always carried, so a client can build
a picker, autocomplete, or activation gate without a second request per row.

**Usage** (token/cost aggregation across threads)
- `GET /v1/usage?since=<rfc3339>&until=<rfc3339>&group_by=<day|model|provider|thread>`

`since` / `until` are inclusive RFC 3339 timestamps and may be omitted (no
bound). `group_by` defaults to `day`. Buckets are sorted by ascending key.
Empty time ranges produce empty `buckets` (never a 404). Cost is computed via
the model→pricing map; turns whose model has no pricing entry contribute
tokens but `0.0` cost. Added in v0.8.10 (#564).

```json
{
  "since": "2026-04-01T00:00:00Z",
  "until": "2026-04-30T23:59:59Z",
  "group_by": "day",
  "totals": {
    "input_tokens": 12345,
    "output_tokens": 6789,
    "cached_tokens": 0,
    "reasoning_tokens": 0,
    "cost_usd": 0.012,
    "turns": 42
  },
  "buckets": [
    {
      "key": "2026-04-30",
      "input_tokens": 1234,
      "output_tokens": 678,
      "cached_tokens": 0,
      "reasoning_tokens": 0,
      "cost_usd": 0.001,
      "turns": 3
    }
  ]
}
```

### Native client routes (GPUI desktop)

These families serve the GPUI desktop client over the same bearer-token
transport. They reuse the runtime's existing authorities — the engine's shell
manager, the durable thread store, the workspace confinement layer, the
config's credential plumbing — and add no second runtime, session store,
scheduler, or credential store.

**Terminal sessions** (the persistent Engine-owned shell)

The jobs family above runs one command per job. A terminal pane needs the
*other* authority: the stateful PTY-backed shell the agent's own terminal
tools drive, which keeps cwd and environment across inputs. These routes
attach to that session and never create one — a name with no live session is
`404`, because conjuring a shell from an HTTP request would give the client a
terminal the Engine does not know about. Input is attributable by route:
`input` is the client's writer, `terminal_send` is the agent's.

- `GET /v1/terminal/{name}/output?cursor=<bytes>&max_bytes=<1-64KiB>&format=
  <base64|text>` — the resumable byte stream. `{name, offset, next_cursor,
  total, dropped, encoding, data, running, exit_code}`: pass `next_cursor`
  back to continue; reads never consume, so several clients may hold
  independent cursors; `dropped` reports bytes the 512 KiB ring discarded,
  and a cursor past `total` is answered from `total` rather than echoed back
- `POST /v1/terminal/{name}/input` — `{ "data", "encoding"? }`, `base64` by
  default (exact bytes) or `text` for UTF-8 → `{ "name", "written" }`
- `POST /v1/terminal/{name}/resize` — `{ "rows", "cols" }` → the kernel
  window the child draws for
- `POST /v1/terminal/{name}/kill` — end the shell; observe the exit through
  `output` (`running` / `exit_code`) rather than the acknowledgement

`GET /v1/runtime/info` advertises `terminal_stream`, `terminal_input`,
`terminal_resize` and `terminal_kill`. All four are `false` on Windows and OpenHarmony builds
today: the owner is Unix-only, those routes answer `501`, and a client
should gate its terminal controls on these flags rather than discovering it
from a failed request. Known limitations, stated because a reader would
otherwise assume them: there is no `wait_ms` long poll (poll the cursor),
scrollback dropped by the ring is gone with the process, a restarted Engine
reports no session rather than pretending to reattach, and the
`@codewhale/runtime-sdk` package has no terminal client wrapper yet — the raw
routes are the contract for now.

**Jobs** (operator-scoped shell jobs; the terminal surface)
- `GET /v1/jobs` — every live and known-stale job across all threads
- `GET /v1/threads/{id}/jobs` — jobs owned by one thread's manager:
  model-launched, subagent-launched, and client-launched together
- `POST /v1/threads/{id}/jobs` — `{ "command", "cwd"?, "timeout_ms"?,
  "tty"?, "env"? }` → `201 { "job" }`; runs as a background shell under the
  thread's projected sandbox policy. `tty: true` merges stderr into stdout
  and gives the command a terminal (required for interactive programs);
  background jobs are never killed at `timeout_ms`. A relative `cwd`
  resolves against the thread workspace. Outside trust mode `cwd` must stay
  inside it after symlinks resolve (`403` otherwise): unlike the shell tool,
  this route does not follow `workspace_follow_symlinks` or `/trust add`
  roots, so a symlink leading out of the workspace is refused. The job runs
  in the resolved directory that was checked (a later symlink retarget does
  not move it); a `cwd` that resolves to a non-UTF-8 path is a `400`
- `GET /v1/threads/{id}/jobs/{job_id}` — one job's status + metadata
- `GET /v1/threads/{id}/jobs/{job_id}/output?stream=<stdout|stderr>&cursor=
  <bytes>&max_bytes=<1-512KiB>&wait_ms=<0-30s>&format=<base64|text>` — the
  resumable byte stream. `{job_id, stream, offset, next_cursor, total,
  dropped, encoding, data, status, exit_code, done}`: pass `next_cursor`
  back to continue; `wait_ms` long-polls for new bytes on a running job;
  `done` means a terminal status and nothing left past the cursor
- `POST /v1/threads/{id}/jobs/{job_id}/stdin` — `{ "data", "encoding"?,
  "close"? }`: `data` is UTF-8 text by default or `base64`, `close: true`
  sends EOF; works for PTY and piped jobs → `204`
- `POST /v1/threads/{id}/jobs/{job_id}/kill` — bounded SIGTERM → SIGKILL
  escalation on the process group → `{ "job", "result" }` with the final
  snapshot

Reads are non-consuming: several clients may hold independent cursors, and
polling never steals output from the engine's own delta consumer. The
buffer is bounded with exact drop accounting — a reader whose `cursor`
falls behind the retained window gets `offset` past it and `dropped > 0`,
and must re-anchor. Evicted jobs keep a tail snapshot which the output
route serves as the final retained window. Jobs are scoped to the thread
that created them and are killed when that thread is removed; the engine's
background commands use the same per-thread manager, so `GET /v1/jobs` is
also how a client sees model-spawned work.

**Commands** (typed command catalog, APPS-28)
- `GET /v1/commands` — `{commands: [...]}`: every registered slash command,
  builtin and user, as the TUI's own registry holds it. Per entry: `name`,
  `aliases`, `summary` and `usage` (English source text — localizing is the
  client's surface), `subcommands` (the literal verbs the usage line
  declares), `takes_arguments`, `kind` (`builtin` registered code, or `user`
  expanding a stored template), `binding` (`host` runs locally and never
  reaches the model; `prompt` expands into the request the model sees),
  `discovery` (`primary` / `advanced` / `compatibility`, builtins only),
  `hidden` for rows the product does not advertise, and `shadowed_by` /
  `shadowed_aliases` where a user command has taken a builtin's spelling.

  Each entry also carries the composer argument shape, computed the way the
  TUI composer computes it, so a client does not re-derive it from `usage`
  (#6230):
  - `requires_argument` — the usage line mentions any argument, required or
    optional.
  - `requires_required_argument` — the usage line has a `<required>` argument
    outside every `[optional]` group.
  - `composer_wants_trailing_space` — accepting the command leaves a trailing
    space for its arguments.
  - `palette_runs_directly` — the palette runs the command on selection
    instead of pasting it into the composer.
  - `show_in_empty_discovery` — the command is listed when the slash menu
    opens with no filter text.

  User commands derive these from `takes_arguments`: their arguments are
  never required, a template that takes arguments waits in the composer and
  one that does not runs directly, and a `hidden` template stays out of empty
  discovery.

  The same registry the TUI palette reads, so a desktop palette can be
  checked against it instead of drifting from it. Two rules a client must
  respect: a `binding: "host"` row is never submitted as a model prompt, and
  a user command shadowing a builtin name wins that spelling.

**Hooks**
- `GET /v1/hooks[?thread_id=...]` — `{workspace, enabled, hooks: [...],
  problems: [...]}`: the hook set Runtime API threads run for that workspace
  (the server workspace, or the named thread's). Per entry: `name`, `event`,
  `command` (credential-shaped values masked; every URL keeps only its scheme and host), `background`, `timeout_secs`,
  and `source` (`global` user config, `plugin` reviewed plugin, `project`
  trusted and approved `.codewhale/hooks.toml`). `problems` lists hooks
  rejected or warned about at load, one line each.

  Every Runtime thread builds its engine with this set: `tool_call_before`
  can deny a call, `shell_env` applies to shell tools, and `tool_call_after`
  and `on_error` (for a failed tool) fire as observers, as they do in the TUI.
  Clients read this route instead of keeping a hook table of their own.

**Context** (per-thread context pressure, APPS-90)
- `GET /v1/threads/{id}/context` — `input_tokens` (the conservative live
  estimate the visible meter uses), `billed_input_tokens` (last
  provider-counted prompt size when one exists), `window_tokens`,
  `output_cap_tokens`, `input_budget_ceiling`, `available_input_tokens`,
  `compaction_trigger_tokens`, `usage_percent` and `pressure`. Served by the
  live engine via `Op::GetContextBudget`. Every numeric field is nullable —
  a route that cannot express a bounded window reports `null` rather than an
  invented number — and `live: false` marks responses where the engine
  could not be loaded and only the store-recorded route's static window
  resolved.

**Git** (workspace repository operations, APPS-106)
- `GET /v1/git` — status detail: `git_repo`, `branch`, `head`
  (abbreviated, for display), `head_oid`, `index_token`, `revision`,
  `ahead`/`behind`, counts, per-file porcelain `files[]`
  (`{path, index, worktree, staged, status, old_path?, rev}`), `branches`,
  `remotes`. `files[].path` and `old_path` are workspace-relative, the same
  frame the write routes take; in a workspace that is a subdirectory of its
  repository, rows outside the workspace are not listed (the counts stay
  repository-wide). A rename out of the workspace appears as its source
  deletion. Normal Git filters and untracked settings apply. Untracked
  directories stay collapsed; only a row naming the workspace itself is
  expanded to individually addressable files. The precondition tokens are
  opaque:
  - `head_oid` — the full HEAD commit id; `null` on an unborn branch or when
    HEAD cannot be read
  - `index_token` — the whole index (mode, blob, stage and path of every
    entry, repository-wide). A stat-only refresh by `git status` does not
    change it. Status remains best-effort on repository read failures;
    unavailable tokens are `null`, and broken HEADs cannot satisfy guards
  - `files[].rev` — one row: its index entries plus the working-tree state
    of every file it covers (including a rename's in-workspace source).
    Content tokens (`c-…`) include file bytes, the executable bit on Unix,
    symlink targets, and submodule HEAD/status. Submodule dirty contents
    are summarized by porcelain status, not recursively hashed; ordinary
    stage/discard do not write those contents. A read hashes at most
    64 MiB / 4,096 files; rows past that budget or containing a file over
    16 MiB carry a display-only size-and-mtime token (`s-…`). Those tokens
    cannot guard writes. Unreadable paths, paths beneath symlinked
    directories, special files and broken nested repositories have `rev: null`;
    other rows retain their tokens. If a broken tracked submodule aborts
    porcelain, ordinary rows are recovered without submodule recursion and
    the unreadable submodule is shown with `status: "unknown"`, `worktree: "?"`
  - `revision` — the whole tree: `head_oid`, `index_token`, every row's
    `rev`, including the working-tree state of rows outside a subdirectory
    workspace. `null` when any row is stat-only or unreadable, a repository
    read is incomplete, or untracked paths are hidden; whole-tree guarded
    writes are unavailable in that case. Clients must not silently omit a guard
- `GET /v1/changes` — the same porcelain `files[]` projection plus
  `head_oid`, `index_token` and `revision`, minus repo chrome
  (branches/remotes): one authority, so the change list can never disagree
  with the status read
- `GET /v1/diff?path=` — one file's unified `diff` against `base` (`HEAD`,
  or the empty tree on an unborn branch — which reads staged adds as new
  files). Covers staged+unstaged in one patch; `truncated` reports the
  512 KiB cap. An untracked file answers `untracked: true` with an empty
  diff — the client reads the file itself rather than mistaking it for
  unchanged
- `GET /v1/workspace/diff?limit=` — whole-tree patch (default 256 KiB,
  max 4 MiB) plus a complete `--numstat` `files[]` inventory
  (`{path, added, deleted}`) so every changed row renders even when the
  patch is truncated
- `GET /v1/git/graph?limit=` — bounded commit rows (`id`, `short`,
  `parents`, `author`, `timestamp`, `refs`, `subject`); an unborn branch is
  an empty graph, not an error
- `POST /v1/git/stage` `{ "paths": [...] }` or `{ "all": true }`;
  `POST /v1/git/unstage` same; `POST /v1/git/discard` `{ "paths": [...] }`
  (tracked paths only — no `all`, an untracked path fails closed);
  `POST /v1/git/commit` `{ "message", "all"? }`; stage, unstage, discard
  and commit also take an optional `expect` (below); `POST /v1/git/push`
  `{ "remote"?, "set_upstream"? }` (`remote`, or `origin` when only
  `set_upstream` is given, must name a configured remote);
  `POST /v1/git/branch`
  `{ "name", "create"? }`

Diffs and precondition token reads run through the hardened review command
(filters, fsmonitor, hooks, lazy fetches and replace-objects neutralized).
Porcelain status uses normal Git, matching the workspace counts and filters;
writes run through the non-interactive command path
(`GIT_TERMINAL_PROMPT=0`, BatchMode ssh) so a credential or host-key prompt
can never hang a request. Path lists are workspace-relative under the same
confinement as the file routes (traversal → 400, `.git` → 403), passed after
`--` under `--literal-pathspecs`, so `src/*` names a file called `*` and is
never a glob. (Unstage of the whole tree uses the `:/` root pathspec.)
Mutations answer `{ok, output, status, current}`: the refreshed status and
the full `GET /v1/git` detail, so a client re-reads nothing after an
operation and can chain the next write from fresh tokens. A workspace that
is not a repository answers `404`.

*Preconditions.* Stage, unstage, discard and commit accept
`expect: { head?, index?, revision?, files? }`, built from the last
`GET /v1/git`. Each present field is checked and an absent one is not;
`head: null` means "HEAD must still be unborn". Without `expect` (or with
`expect: {}`) a write behaves exactly as before. Recommended use:

| Operation | `expect` |
| --- | --- |
| stage / unstage `paths` | `{head, files: {path: rev}}` |
| stage / unstage `all` | `{head, revision}` |
| discard (always — it destroys edits) | `{head, files: {path: rev}}` |
| commit | `{head, index}` |
| commit `all` | `{head, revision}` |

Malformed preconditions answer `400` before anything runs: `head` must be a
40- or 64-hex id or `null`; `index` and `revision` 64 hex (an explicit
`revision: null` is refused); `files` values a content-safe `c-` rev (`s-`
tokens answer `400`). `files` keys
(workspace-relative; `dir/` and `dir` are the same key)
must name exactly the requested paths, so no path is left unguarded by
accident. `files` is refused with `all: true` (use `revision`) and on
commit. An unknown key inside `expect` is rejected like any other unknown
field. When the repository no longer matches, the route writes nothing and
answers `409`:

```json
{ "error": { "message": "The repository changed since it was read (HEAD moved; src/a.rs changed). Nothing was written; refresh and review again.",
             "status": 409, "code": "git_state_changed" },
  "stale": ["head", "files"], "stale_paths": ["src/a.rs"],
  "current": { "...": "the GET /v1/git detail" } }
```

`stale` lists the components that moved (`head`, `index`, `files`,
`revision`); `stale_paths` the `files` keys whose `rev` changed. A client
re-renders from `current`, keeps the user's selection, and asks again.

Stage, unstage, discard, commit and branch from one runtime are serialized,
so a check and its write are atomic with respect to that runtime's other
windows; a second concurrent write answers `409` with
`error.code: "git_busy"` rather than queueing behind a long commit hook.
Push is not serialized: it only moves a remote ref and can wait on the
network for up to 120 s. The lock does not cover processes outside this
runtime — a terminal, an editor, or Codewhale's own agent tools — which can
still change the repository in the moment between the check and git taking
`index.lock`; a truly concurrent git write then fails on git's own
`index.lock` (a `400` carrying git's message). A compare-and-swap commit via
`commit-tree` and `update-ref` would close that window but skip the
repository's hooks, which a Review-sheet commit must run, so it is not used.

**Diagnostics** (read-only logs, crashes, process — APPS-103)
- `GET /v1/logs` → `{sources: [{dir, files: [{name, size, modified}]}]}` —
  the runtime's log directory plus `audit.log[.1]` from the codewhale home,
  newest first, capped
- `GET /v1/logs/{name}?offset=<bytes>&limit=<bytes>&tail=<bytes>` →
  `{name, size, modified, offset, bytes, truncated, encoding, content}` —
  one bounded window; `tail` reads from the end and is mutually exclusive
  with `offset`; `truncated` means bytes remain after the returned window
  (a tail read at EOF is `false`), `encoding` is `utf-8` or `base64`
- `GET /v1/crashes`, `GET /v1/crashes/{name}` — the same list/read contract
  over the crash-dump directories (`~/.codewhale/crashes`, legacy
  `~/.deepseek/crashes` merged)
- `GET /v1/process` → `{pid, version, commit, started_at, uptime_seconds,
  executable, rss_bytes}` — `rss_bytes` only where the platform reports it
  (Linux `/proc`); absent rather than fabricated elsewhere

These routes package what already exists on disk for a client-side export;
there is no telemetry upload route and no second log store. Names are
basename-validated (no separators, no `..`), listings are capped, reads are
bounded windows, and symlinks are never followed — a client bundles the
files itself.

**Targets and remote posture** (APPS-50)
- `GET /v1/targets` → `{targets: [self], remote: {supported: true,
  attach: "client", probe: "POST /v1/remote/connect"}, ssh: {…},
  cloud: {…}}` — this runtime's own record as the attachable target plus
  per-surface ownership; the runtime keeps no persistent target registry,
  so `POST /v1/targets` and `POST /v1/targets/switch` answer
  `501 Not Implemented` — target selection is client-owned and a switch
  must never move a running task server-side
- `GET /v1/remote` → `{bind_host, port, loopback_only, reachable_from_lan,
  auth_required, mobile, tls}` — this listener's reachability posture.
  `tls` is always `false`: the API has no TLS terminator, so non-loopback
  reachability assumes a verified overlay (VPN/mesh), never plain LAN trust
- `POST /v1/remote/connect` `{ "endpoint": "http://host:port" }` — probes a
  candidate remote's unauthenticated `GET /v1/runtime/info` (origin only;
  any pasted path is discarded). Answers `{ok, remote: {endpoint,
  runtime_api_version, codewhale_version, auth_required, …}, attach:
  "client"}` on success, and `{ok: false, reason: "unreachable" |
  "not a Codewhale runtime" | …}` as data on failure. URLs carrying
  credentials are refused with 400 — the remote's token is configured
  client-side, and a connect route that forwarded one would be an
  exfiltration primitive
- `GET /v1/ssh`, `GET /v1/cloud` → `{supported: false, owner:
  "codewhale-control-plane", reason}`; `POST /v1/ssh/connect` and
  `POST /v1/cloud/attach` → `501`: SSH workspace provisioning and hosted
  cloud computers belong to the Apps control plane (ASCII Box for Managed
  Computer), not to a second authority inside Core

A remote Codewhale is a `serve --http` runtime with a token — that is the
whole attach model. These routes describe and probe it; they never execute
a remote request on the local machine.

**LSP** (workspace language intelligence, APPS-93)
- `GET /v1/lsp` — capability: `enabled`, supported `languages` with their
  server commands, `custom_languages`, operations, poll and diagnostic caps
- `GET /v1/diagnostics?path=` — file diagnostics
- `GET /v1/definition?path=&line=&character=` (1-based)
- `GET /v1/references?path=&line=&character=` (1-based)
- `GET /v1/symbols?path=&query=` — empty query returns document symbols

One lazily-built workspace-level `LspManager` serves these; engine threads
keep their own per-thread managers for the post-edit hook, and a server that
never serves an LSP route never spawns a language server. `path` is
workspace-relative under the same confinement as the file routes. Normal
absence is data: no language server, a disabled `[lsp]` config, or a timeout
answers `200` with `ok: false` and a machine-readable `reason`
(`no_server`, `lsp_disabled`, `lsp_error`); malformed input is a 400 and a
missing file a 404.

**Voice** (host dictation, APPS-98)
- `GET /v1/voice` — capability: `available`, detected `recorder` command,
  resolved `asr` `{kind, model}`, `modes`, `send_phrases`,
  `max_record_seconds`
- `POST /v1/voice/dictate` — record then transcribe → `{ ok, text }`
- `POST /v1/voice/send` — same capture with the "send it" / 发送/發送
  suffix contract: `send: true` tells the client to submit (empty `text`
  with `send: true` means submit the client's current draft)
- `POST /v1/voice/control` `{ "composer": "draft text" }` — assisted
  dictation that shows the model the composer text; `assisted: false` in the
  response means a free ASR backend (local whisper/Groq) handled the audio
  and the composer context was never seen

The runtime owns the host microphone and the ASR dispatch — the same
implementation the TUI's `/voice` commands run, headless. Recording is one
blocking capture per host (requests serialize; the loser gets
`ok:false`/`no_speech`, not a fought-over device). Provider ASR resolves its
key lazily so local-whisper and Groq paths work without provider auth.
Interim and final transcription use the selected ASR backend. If local whisper
or Groq fails, the error stays on that backend: the runtime never retries the
recording or composer text with the active model provider. Select provider ASR
explicitly to use that route.
Failure is data: `no_recorder`, `no_speech`, `no_provider_auth`,
`transcription_failed`. `CODEWHALE_DISABLE_VOICE=1` is an operator
kill-switch — a headless `serve --http` host reports `available: false` and
every dictate call fails closed.

## Provider and model selection

These three routes are how a GUI renders a model picker whose contents are true
for *this* runtime instead of guessed from a version snapshot. They were
undocumented until 2026-08-04, which cost a desktop integration a day: the
client probed `/v1/models`, `/v1/runtime/models`, and `/v1/runtime/providers`
(all correctly 404) and concluded the capability did not exist.

### `GET /v1/providers`

```json
{
  "current": "modelstudio-token-plan",
  "providers": [
    {
      "id": "modelstudio-token-plan",
      "model_provider_id": "modelstudio-token-plan",
      "display_name": "Alibaba Cloud Model Studio",
      "default_model": "qwen3.8-max",
      "has_model_catalog": true,
      "credentialState": "configured"
    }
  ]
}
```

`current` is the active generic provider id. Only the active entry carries an
exact identity: an active built-in normally repeats its canonical id in
`model_provider_id`, while an active named custom route has `current` set to
`custom` and the exact configured key there (for example `lm-studio`). Other
entries have a null exact id; a null id on the active `custom` entry identifies
the released legacy root-level custom route. Preserve both fields from the
selected entry and send a non-null exact id back as `POST /v1/threads`'s
`model_provider_id`; dropping a named custom id would collapse the selection to
the legacy root custom route. `credentialState` is a stable, non-secret
projection of the Runtime's existing structural credential classification:

- `configured`: credential material is structurally available;
- `login_required`: the route needs login or a usable login capability;
- `missing`: an API-style credential is unavailable;
- `no_auth`: the route explicitly disables credential use;
- `local`: the exact route is local and keyless;
- `legacy`: the compatibility route cannot be classified more precisely.

For an active named custom provider, this state is calculated from the exact
route named by `model_provider_id`, not from a generic custom-provider default.
It deliberately collapses saved-key and imported-token details into
`configured`, and login/consent-source details into `login_required`.

The response never includes endpoint URLs, credential environment-variable
names, filesystem paths, credential values, consent-source details, or token
metadata. `credentialState` is not a provider canary: `configured` does not
prove endpoint reachability, credential validity, model entitlement, or a
successful request. The models route below is also only a selection catalog; a
non-empty list does not prove that the route can currently serve a request.

### `GET /v1/providers/{id}/models`

```json
{
  "provider": "deepseek",
  "models": [
    {
      "id": "deepseek-v4-flash-vision-exp",
      "image_input": "supported",
      "reasoning_effort": "unknown",
      "reasoning_effort_levels": [],
      "reasoning_effort_source": null
    }
  ]
}
```

For an exact configured route, supply `?model_provider_id=vision-work` and
require the response to echo that same `model_provider_id`. The Runtime resolves
that identity under the requested provider kind before reading model support.
Unknown or mismatched identities return `400`. Named pagination cursors bind the
configuration identity, endpoint and catalog snapshot; changing any of those
requires restarting pagination. Omitting the query preserves the legacy catalog
projection and omits the identity echo.

The catalog for one provider. Returns `400` for an unknown id, and for the
legacy `deepseek-cn` alias, which has no provider metadata — use `deepseek`.
An empty `models` array means the Runtime has no discoverable or configured
model ids for that provider; it does not report credential presence.

The ids returned here are exactly the values accepted by `POST /v1/threads`'s
`model` field and by the switch route below. `image_input` is the exact resolved
provider/model route's capability state: `supported`, `unsupported`, or
`unknown`. Keep `unknown` unknown rather than inferring from the model name or
wire protocol. `supported` describes the model route; it does not mean a given
client implements an image-upload control.

`reasoning_effort` uses the same three capability states and describes whether
the exact model's metadata publishes a selectable effort ladder.
`reasoning_effort_levels` contains only canonical, recognized active effort levels
from that metadata. Off and provider synonyms such as none are excluded: the
Apps/Chat protocol treats off as omission, which does not prove support for an
explicit provider disable command. A model capable of reasoning may still have
an unknown active effort ladder.
Codex levels are also excluded when native compatibility would change their
wire value (currently minimal and auto). This projection does not change native
compatibility behavior or advertise a tier the Runtime cannot send unchanged.
No levels are inferred from a provider-wide default or a familiar model name
on a custom endpoint. `reasoning_effort_source` identifies `catalog`,
`codex_cli_cache`, or `codex_app_server`; missing, stale, and unrecognized model
metadata stays unknown. Codex roster metadata describes the external CLI's
roster, not proof that a separately configured Runtime credential belongs to
the same account or that an authentication boundary is approved.

Pass `?model_provider_id=<exact configured id>` when selecting a named route.
The Runtime validates the provider kind and exact identity together, returns
`model_provider_id` alongside that route's model list, and leaves the active
configuration unchanged. An empty or unknown requested identity, or a mismatched kind, returns
`400`; it never falls back to another named route.

The Runtime Chat relay publishes the same effort fields in camelCase
(`reasoningEffort`, `reasoningEffortLevels`, `reasoningEffortSource`). These
model facts do not enable tool execution or establish account entitlement.

For a thread-scoped choice, send the provider fields from the selected entry
alongside the selected model. Omit `model_provider_id` when it is null:

```json
{
  "model_provider": "custom",
  "model_provider_id": "lm-studio",
  "model": "local-vision-model"
}
```

This creates one thread on the exact named custom route without changing the
Runtime's provider or model defaults.

### `PUT /v1/providers/{id}/key` — write-only credential

```json
// request
{ "key": "sk-…" }

// response
{ "provider": "openai-codex", "stored": true, "backend": "keychain",
  "credentialState": "configured", "configPath": "/…/config.toml" }
```

Stores a provider API key through the same transactional write as
`codewhale auth set --provider <id> --api-key-stdin`: the secret store under
the provider write lock, plus the `[providers.<id>] auth_mode` metadata
marker persisted to the config document and mirrored into the live runtime
config so `GET /v1/providers` reports the new state immediately. `backend`
names which secret backend holds the key and `configPath` which config
document carries the marker (the user-global file when the ambient config
is workspace-scoped).

The key is never returned — there is no read route for credential material,
and neither the key nor its length appears in the response, errors, or
logs; the response carries only the readiness projection
(`credentialState`). An unknown provider id, the `deepseek-cn` legacy
alias, an empty key, a key over 4 KiB, or one containing control characters
is `400`. `credentialState: "local"` after a successful write is honest
output for a keyless local route: the key is stored, but the route
classifies as not needing one.

### `DELETE /v1/providers/{id}/key` — clear a Codewhale-owned credential

```json
// response
{ "provider": "openai", "cleared": true, "credentialState": "missing" }
```

Clears the credential through the same shared owner as
`codewhale auth clear`: the config document is snapshotted and restored if
its save fails, the secret store is only touched once that save has landed,
and the cleared markers are mirrored into the live runtime config so
`GET /v1/providers` reports `missing` on the next read rather than after a
restart.

Clearing an already-clear route returns `cleared: true` — a client retrying
a revoke must not be told something went wrong. If the config entry is
cleared but the secret backend refuses the delete, the route answers `500`
and names the slot: reporting success while the key is still in the keyring
would be a lie about a security action.

### Credential ownership: `credentialSource` and `credentialWritable`

Both credential verbs refuse a route whose credential Codewhale does not
own, and `GET /v1/providers` carries the same classification so a client can
disable its control *before* submitting instead of failing late:

| `credentialSource` | `credentialWritable` | Meaning |
| --- | --- | --- |
| `secret_store` | `true` | Codewhale's own durable backend. The only writable source. |
| `config` | `false` | A literal key in a config file, which still wins at request time. |
| `external_auth` | `false` | An active external consent (OAuth) owns the credential. |
| `none` | `false` | The route sends no credential, or has no credential slot. |

When `credentialWritable` is `false`, `credentialWritableReason` carries
user-facing copy naming the owner, and both `PUT` and `DELETE` answer `409`
with that same reason. The classification is structural: it reads declared
auth mode, consent state and the *kind* of any configured `api_key` value,
and never resolves a secret, an environment value, or an auth command. It is
a class and never a value, a path, or an environment variable name.

### `POST /v1/providers/{id}/switch`

```json
// request  (model is optional; omit to take the provider default)
{ "model": "qwen3.8-max" }

// response
{ "provider": "modelstudio-token-plan", "model": "qwen3.8-max",
  "message": "…", "persisted": true }
```

**Use this rather than simulating a switch with repeated `POST /v1/config`
writes plus a reload.** Provider and model move together here, the change is
validated against the provider's catalog before it is applied, and `persisted`
reports whether it was written to config or applied to the live session only.
Rejects an unknown provider id and the `deepseek-cn` alias with `400`.

## Runtime data model

The runtime uses a durable Thread/Turn/Item lifecycle.

- **ThreadRecord** — `id`, `created_at`, `updated_at`, `model`,
  `model_provider` (generic kind), `model_provider_id` (optional exact configured
  route), `workspace`, `mode`, `task_id`, `system_prompt`, `latest_turn_id`,
  `latest_response_bookmark`, `archived`
- **TurnRecord** — `id`, `thread_id`, `status` (`queued|in_progress|completed|
  failed|interrupted|canceled`), `effective_provider`, `effective_model`,
  `effective_billing_surface`, timestamps, duration, usage, error summary,
  `artifacts` and `workspace` (see [Turn artifacts](#turn-artifacts))
- **TurnItemRecord** — `id`, `turn_id`, `kind` (`user_message|agent_message|
  tool_call|file_change|command_execution|context_compaction|status|error`),
  lifecycle `status`, `metadata`, `artifacts` and the legacy
  `artifact_refs` projection

Events are append-only with a global monotonic `seq` for replay/resume.

`effective_billing_surface` is a non-secret classification derived from the
endpoint that served the turn. Recognized StepFun routes use `stepfun-payg` or
`stepfun-plan`; unknown and custom endpoints leave it unset. The raw base URL is
not persisted in `TurnRecord`.

### Restart semantics

- If the process restarts while a turn or item is `queued` or `in_progress`,
  the recovered record is marked `interrupted` with an `"Interrupted by
  process restart"` error.
- The trailing newline is an event append's commit marker. On startup, a final
  JSONL fragment without that delimiter is truncated and fsynced even when its
  bytes form valid JSON; it is an uncommitted append, and its already-reserved
  sequence number is not reused. Newline-terminated malformed records are not
  identifiable crash debris and continue to fail closed during replay.
- If a terminal turn record reached disk but its terminal event sequence did
  not, the first async read reconciles any unresolved dynamic calls as
  `tool_call.canceled` and then emits one `turn.completed`. Existing terminal
  call and turn receipts are detected and never duplicated.
- Turn-operation bindings survive restart. Retrying the same `operation_key`
  and request returns the original recovered turn (including an `interrupted`
  turn recovered from an in-progress process exit); mismatched reuse remains a
  conflict. A crash-created binding that never acquired a turn is discarded at
  startup because engine submission happens only after both records are
  durable.
- Task execution performs its own recovery on top of the same persisted
  thread/turn store.

### Approval model

- The `auto_approve` flag applies to the runtime approval bridge and engine
  tool context. When enabled for a thread/turn/task, approval-required tools
  are auto-approved in the non-interactive runtime path, shell safety checks
  run in auto-approved mode, and spawned sub-agents inherit that setting.
- When omitted, `auto_approve` defaults to `false`.
- [Authorization order](AUTHORIZATION_ORDER.md) describes where typed rules,
  registered tool requirements, safety floors, repository law, approval
  transport, and sandbox enforcement sit relative to one another.

### SSE event stream

The SSE event payload shape for `/v1/threads/{id}/events`:

```json
{
  "schema_version": 1,
  "seq": 42,
  "previous_seq": 38,
  "event": "item.delta",
  "kind": "item.delta",
  "thread_id": "thr_1234abcd",
  "turn_id": "turn_5678efgh",
  "item_id": "item_90ab12cd",
  "timestamp": "2026-02-11T20:18:49.123Z",
  "created_at": "2026-02-11T20:18:49.123Z",
  "payload": {
    "delta": "partial output",
    "kind": "agent_message"
  }
}
```

Compatibility notes:

- `schema_version` is the HTTP/SSE envelope schema version. It is independent of
  the runtime store schema used for persisted thread/turn/event records.
- `event` remains the SSE event name in existing clients; it is preserved as-is.
- `kind` mirrors `event` in the stable envelope for typed clients.
- `seq` is allocated globally across all Runtime threads. Consequently, gaps
  between a thread's events are normal when other threads interleave. On this
  per-thread SSE stream, `previous_seq` is the sequence of the last event
  delivered for this thread (or the requested replay cursor for the first
  event); clients detect loss by comparing it with their accepted per-thread
  cursor, not by requiring `seq == previous_seq + 1`. Sequence allocation is
  also not rewound after an append is transactionally rolled back, so a retry
  can intentionally skip an unused value without implying a missing event.
- `thread.started`, `turn.started`, and `turn.completed` are emitted as SSE event
  names exactly as before.
- `timestamp` remains the canonical event time for schema version 1. `created_at`
  is an equivalent alias for clients that use `created_at` naming elsewhere; do
  not require both fields to be present.

### Ending and resuming a thread stream

Whenever the server ends a `/v1/threads/{id}/events` stream that already
returned `200`, the last frame is `stream.end`, sent exactly once and always
(it does not need `progress=true`):

```
event: stream.end
data: {"schema_version":1,"event":"stream.end","kind":"stream.end","thread_id":"thr_1234abcd","reason":"replay_failed","last_seq":42,"retryable":true}
```

- It is a transport frame, not a journal event: it has **no `seq`** and **no
  SSE `id:`**. Clients that acknowledge on `seq` skip it, and a browser's
  `Last-Event-ID` stays on the last real event.
- `last_seq` is the stream's cursor when it ended: the last journal `seq`
  delivered on this connection, or the effective start cursor if none was
  (after a `replay_limit` tail, that is already past the omitted history). It
  is exactly the `since_seq` that resumes with no loss and no repeats.
- `retryable` says whether resuming from `last_seq` can succeed. Key the client
  on it, not on the reason list. Treat an unknown `reason` by its `retryable`.
- There is no free-text message. The underlying error is in the Runtime log,
  and it can contain store paths.

| `reason` | Meaning | `retryable` |
| --- | --- | --- |
| `replay_failed` | The durable history read that feeds the opening replay failed, including a replay worker crash after the first cursor | `true` |
| `catch_up_failed` | After broadcast lag, the durable re-read from the stream's cursor could not be opened or failed | `true` |
| `runtime_shutdown` | The Runtime API server is stopping (SIGINT, SIGTERM, or SIGHUP; Ctrl+C or Ctrl+Break on Windows). Open streams get this frame within a bounded drain window before the process exits | `true` |

Every `200` response carries `x-codewhale-stream-end: 1`. With that header, an
EOF **without** `stream.end` means the connection or the Runtime process died
without the server choosing to end the stream: a network or proxy drop, a
crash, or a kill that allows no drain (for example `SIGKILL`). A Runtime that
predates this frame sends no header; there EOF stays ambiguous, so treat it as
connection loss.

Client resume rule:

1. Keep `cursor` = the `seq` of the last journal frame you accepted. Ignore
   frames with `seq <= cursor`. If a frame's `previous_seq` is not your
   `cursor`, you missed events: reload the thread snapshot rather than trust
   local state.
2. On `stream.end` with `retryable: true`, reconnect with
   `since_seq = last_seq` after a bounded backoff, and tell the user what the
   Runtime said (for example, "Runtime is shutting down — reconnecting").
   With `retryable: false`, stop, show the reason, and fall back to the
   snapshot.
3. On EOF or a transport error without `stream.end`, reconnect with
   `since_seq = cursor` after a bounded backoff and show a connection problem,
   not a Runtime error.
4. A `401`/`403` or `404` before the stream opens is terminal (a credential or
   thread problem). Retry `5xx` with backoff.
5. Never reconnect from `0` to "start over": replay is idempotent only by
   cursor.

Fleet streams (`/v1/fleet/runs/{run_id}/events`) keep their own end frames,
`fleet.stream.error {retryable}` and `fleet.replay.cursor_unavailable`. They
differ from `stream.end`: they carry no cursor, because a Fleet client resumes
from the opaque `cursor` of the last Fleet event it accepted.

### Steer delivery

Putting a steer into the engine's mailbox is not the same as the model reading
it. The engine discards a steer whose turn has already moved on, and an
interrupted or failed turn drops whatever it had queued. The API reports the
engine's real verdict rather than the attempt:

- The item is persisted `queued` when the steer is accepted into the mailbox.
- **Delivered.** The engine committed the text into the turn's record: the item
  becomes `completed`, `steer_count` rises, and `turn.steered` + `item.completed`
  are emitted. `POST .../steer` returns `200` with that turn.
- **Not delivered.** The turn moved on, was interrupted, or failed first: the
  item becomes `canceled`, `steer_count` does not rise, and `turn.steer_dropped`
  is emitted carrying `input`, `reason`, and the settled `item`. `POST .../steer`
  returns `409`, so a client can keep the user's text and resend it rather than
  clearing a composer over guidance that was never seen.
- **Still pending.** A steer sent while the engine is inside a long tool call
  cannot settle until that call returns, and the request does not hang for it.
  After a short wait `POST .../steer` returns `200` with the item still `queued`;
  the eventual `turn.steered` or `turn.steer_dropped` event carries the verdict.

A client that treats `200` as "the model saw it" is therefore wrong in the third
case: read the item's status, or wait for the event.

Common event names: `thread.started`, `thread.forked`, `turn.started`,
`turn.lifecycle`, `turn.steered`, `turn.steer_dropped`, `turn.interrupt_requested`,
`turn.completed`, `turn.artifacts`, `item.started`, `item.delta`, `item.completed`,
`item.failed`, `item.interrupted`, `approval.required`, `approval.decided`,
`approval.timeout`, `user_input.required`, `user_input.answered`,
`user_input.canceled`, `tool_call.requested`, `tool_call.resolved`,
`tool_call.timeout`, `tool_call.canceled`, `sandbox.denied`,
`turn.workspace_snapshot`, `runtime.store_failure`.

`runtime.store_failure` is the runtime reporting a fault in the operator's own
on-disk state: a thread, turn, or item record under the session's runtime
store could not be read, parsed, or written. The payload carries `operation`
(`read` | `parse` | `write`), `record_kind` (`thread` | `turn` | `item`),
`record_id`, `path`, the full `error` chain, the root-cause `reason`, a
`next_action` (which file to move aside, or where to check free space and
permissions), and a one-line `message`. When `terminal` is `true`, the turn's
own record is unreadable or unwritable and no `turn.completed` will follow;
clients waiting on that turn should treat it as failed.

Agent-message and reasoning deltas are materialized into the item projection
before their corresponding `item.delta` event is sequenced. To avoid an fsync
for every provider fragment, adjacent deltas are coalesced to configured bounds
of at most 32 ms or approximately 16 KiB before publication (an indivisible
upstream chunk can itself exceed the byte target). A process crash inside that
unpublished window can lose the recent suffix; no durable event claims that
suffix existed. Once an `item.delta` is durable, snapshots at or beyond its
cursor include the same materialized prefix.

`approval.required` events may include a `matched_rule` string when an
execution-policy rule caused the prompt. This field is explanatory metadata for
clients and does not grant or persist permissions.

`approval.required`, `approval.decided`, and `approval.timeout` carry two
distinct identifiers. `approval_id` is the Runtime-minted, single-use capability
described under **Approvals** — the only value `POST /v1/approvals/{id}` accepts
— and `approval.required` also repeats it in the legacy `id` field for older
clients. `tool_call_id` is the provider's raw tool-call ID, present for
correlation only. Automatically resolved prompts (thread `auto_approve`, and the
Auto-Review posture, which never opens a modal) mint an `approval_id` as well, so
the field has one meaning on every path; those IDs register no waiter and are
inert against the endpoint. Clients must never treat `tool_call_id` as an
approval capability or assume it is unique across threads.

The thread event stream forwards these payloads intact. The compatibility turn
stream carries `approval_id`, its `id` alias and `tool_call_id`; the pending
snapshot carries the same capability and correlator so reconnecting clients can
attach an approval prompt to its tool row. Resolutions carry their resolution
flags through that projection too: `timeout: true` marks the deny produced when
the configured decision budget expires, and `cancelled: true` marks a deny
forced by a turn interrupt or turn teardown where no user selection
was made — clients should clear the pending prompt rather than report a
refusal.

## Security boundary

- **Localhost by default**. The server binds to `127.0.0.1` by default.
  `--mobile` is also loopback-only and rejects a non-loopback host until a TLS
  or verified-overlay transport boundary exists. The runtime does not provide
  user isolation or TLS.
- **Optional token guard**. `--auth-token` or `DEEPSEEK_RUNTIME_TOKEN`
  requires a matching bearer token for `/v1/*` routes. This is a local
  convenience guard, not a replacement for TLS, VPN, or a trusted reverse
  proxy on public networks.
- **No provider-token custody**. The server never returns the API key. The
  `api_key.source` capability field reports `env`, `config`, or `missing` —
  never the key itself.
- **No hosted relay**. The app-server is a local process under the user's
  control. There is no cloud component.
- **Capability responses** never leak secrets, file contents, or session
  message bodies. They report *metadata*: presence, counts, status flags.

### CORS allow-list

The runtime API ships with a built-in dev-origin allow-list:
`http://localhost:3000`, `http://127.0.0.1:3000`, `http://localhost:1420`,
`http://127.0.0.1:1420`, `tauri://localhost`. To add additional origins (e.g.
when developing a UI on Vite's default `:5173`), use any of:

- CLI flag (repeatable): `codewhale serve --http --cors-origin http://localhost:5173`
- Env var (comma-separated): `DEEPSEEK_CORS_ORIGINS="http://localhost:5173,http://localhost:8080"`
- Config (`~/.codewhale/config.toml`):
  ```toml
  [runtime_api]
  cors_origins = ["http://localhost:5173"]
  ```

User-supplied origins **stack on top of** the built-in defaults; they do not
replace them. Wildcard origins are not supported — the explicit allow-list
model is preserved. Cross-origin preflights advertise only `Authorization`,
`Content-Type`, `Accept`, `X-Codewhale-Runtime-Token`, and the compatibility
`X-DeepSeek-Runtime-Token` request header; custom request headers are not
allowed. Added in v0.8.10 (#561), tightened in v0.9.1 (#4454).

## Managed Fleet Runtime and SDK helpers

The Runtime SDK lives in `npm/runtime-sdk` and is exposed as
the `@codewhale/runtime-sdk` workspace package. It is deliberately thin: every
helper calls the local Rust Runtime API and therefore cannot bypass Codewhale's
sandbox, approval prompts, provider configuration, or fleet ledger authority.

```js
import { createRuntimeClient } from "@codewhale/runtime-sdk";

const client = createRuntimeClient({
  baseUrl: "http://127.0.0.1:7878",
  token: process.env.CODEWHALE_RUNTIME_TOKEN,
});

const created = await client.createFleetRun({
  target: "this_computer",
  roles: [{ name: "reviewer" }, { name: "verifier" }],
  workflow: {
    id: "release-check",
    kind: "parallel",
    tasks: [
      { id: "review", name: "Review", instructions: "Review locally.", worker: { role: "reviewer" } },
      { id: "verify", name: "Verify", instructions: "Verify locally.", worker: { role: "verifier" } },
    ],
  },
});

// POST /runs only prepares durable work. This call crosses the launch gate.
await client.startFleetRun(created.run.id);

let cursor;
for await (const event of client.fleetEvents(created.run.id, { after: cursor })) {
  if (event.cursor) cursor = event.cursor;
  if (event.event === "fleet.replay.cursor_unavailable") {
    // Reload getFleetRun(created.run.id), then reconnect without the old cursor.
  }
}
```

The managed path is deliberately two-step. `POST /v1/fleet/runs` validates and
persists the run and queue without starting a worker. A separate authenticated
`POST /start` activates it and schedules the executor driver; its `202` response
reports `leased: 0` because the driver performs all leasing after it owns the
run. Creation requires named roles, one task owner per role, a `parallel`
Workflow, and an explicit Runtime target. v0.9.4 executes
only `this_computer`; `another_computer` and `cloud` return `501` rather than
silently executing locally. Worker IDs are generated per run; caller-assigned
`worker_specs` return `501` until custom workers can be given collision-free
managed identities. Parallel tasks with overlapping effective write roots are
rejected before the run is journaled. Managed `security_policy` overrides also
fail closed until that document can be enforced end to end; executable
authority comes from each named role's tool posture and bounded task workspace
scope.

Fleet helpers cover this HTTP surface:

| Helper | Runtime API route |
|---|---|
| `createFleetRun(spec)` | `POST /v1/fleet/runs` |
| `startFleetRun(runId)` | `POST /v1/fleet/runs/{run_id}/start` |
| `listFleetRuns()` | `GET /v1/fleet/runs` |
| `getFleetRun(runId)` | `GET /v1/fleet/runs/{run_id}` |
| `listFleetWorkers(runId)` | `GET /v1/fleet/runs/{run_id}/workers` |
| `getFleetWorker(workerId)` | `GET /v1/fleet/workers/{worker_id}` |
| `interruptWorker(workerId)` | `POST /v1/fleet/workers/{worker_id}/interrupt` |
| `stopWorker(workerId)` | `POST /v1/fleet/workers/{worker_id}/stop` |
| `restartWorker(workerId)` | `POST /v1/fleet/workers/{worker_id}/restart` |
| `stopFleetRun(runId)` | `POST /v1/fleet/runs/{run_id}/stop` |
| `replayFleetEvents(runId, options)` | `GET /v1/fleet/runs/{run_id}/events/replay` |
| `fleetEvents(runId, options)` | `GET /v1/fleet/runs/{run_id}/events` (SSE) |

`stopWorker` durably cancels that worker's active task and leaves the rest of
the Fleet running. `interruptWorker` is the compatibility name for the same
attempt-fenced cancellation transition. `stopFleetRun` cancels every queued or
active task and marks the whole run cancelled.

Replay covers aggregate run/task transitions and privacy-bounded individual
worker transitions. Event bodies omit prompts, tool call IDs, completion text,
artifact paths/checksums, and cancellation identities; bounded failure reasons
pass through secret redaction. `cursor` is opaque and stable across ordinary
appends and Runtime restarts. Clients reconnect with `after=<cursor>`. A fresh
request returns a bounded newest tail and marks `history_truncated` when older
history exists. Ledger compaction can remove an old cursor; the JSON endpoint
then returns `409`, while the SSE endpoint emits
`fleet.replay.cursor_unavailable`, so the client reloads the current run
projection instead of accepting a silent gap.

`GET /v1/runtime/info` advertises `fleet_run_create`, `fleet_run_start`,
`fleet_event_replay`, `fleet_event_stream`, and `fleet_local_target`. Older
runtimes without a requested route still produce a typed SDK
`RuntimeCapabilityError`.

Verification:

```bash
npm test --workspace @codewhale/runtime-sdk
```

## Agent Run Receipts

Sub-agent lanes persist compact run receipts in
`.codewhale/state/subagents.v1.json`. The Runtime API exposes those receipts as
a read-only inspection surface:

| Operation | Endpoint |
|---|---|
| List persisted agent runs | `GET /v1/agent-runs` |
| Inspect one run | `GET /v1/agent-runs/{run_id}` |
| Stop one run | `POST /v1/agent-runs/{run_id}/cancel` |

The response is the same worker-record shape surfaced by `agent` receipts:
`spec.run_id`, `actor_kind`, lifecycle `status`, bounded `events`,
`follow_up`, `takeover`, `artifacts`, `usage`, and `verification`. `run_id`
falls back to the worker id for older records, and `{run_id}` may be either the
run id or the worker id.

These endpoints do not start or steer sub-agents. The API surface exists so
app/editor/headless clients can inspect the same handoff receipts that the TUI
and parent model see, and stop a run they are showing.

`POST /v1/agent-runs/{run_id}/cancel` takes no body. It stops the run through
the same session-scoped path as the TUI's stop and the `agent/cancel` tool:
descendants stop with it, and a write-scoped child's changed files are named in
its result rather than dropped. It answers with the worker record:

- `200` when the record is terminal (stopping an already-finished run is a
  no-op that returns its receipt);
- `202` when the owning engine accepted the stop but has not recorded the
  terminal receipt within a few seconds; poll `GET /v1/agent-runs/{run_id}`;
- `404` for an unknown run;
- `409` when the run belongs to a session this runtime is not hosting (for
  example a separate terminal session); stop it from that session.

## Session lifecycle (native UI supervision)

| Operation | Endpoint |
|---|---|
| List sessions | `GET /v1/sessions` |
| List session summaries | `GET /v1/sessions/summary` |
| Get session | `GET /v1/sessions/{id}` |
| Rename / archive session | `PATCH /v1/sessions/{id}` |
| Delete session | `DELETE /v1/sessions/{id}` |
| Session store repair summary | `GET /v1/sessions/repair` |
| Resume into thread | `POST /v1/sessions/{id}/resume-thread` |
| Create thread | `POST /v1/threads` |
| List threads | `GET /v1/threads` |
| Attach to events | `GET /v1/threads/{id}/events?since_seq=0` |
| Send message | `POST /v1/threads/{id}/turns` |
| Steer | `POST /v1/threads/{id}/turns/{turn_id}/steer` |
| Interrupt | `POST /v1/threads/{id}/turns/{turn_id}/interrupt` |
| Compact | `POST /v1/threads/{id}/compact` |

## Compatibility tests

Contract snapshots live in `crates/protocol/tests/`. Run:

```bash
cargo test -p codewhale-protocol --test parity_protocol --locked
```

This validates that the app-server's event schema hasn't drifted from the
documented contract. CI runs this on every push to `main` and on release tags.

The app-server stdio control surface has its own drift guard — the advertised
`capabilities` method set is pinned in `crates/app-server/src/lib.rs`:

```bash
cargo test -p codewhale-app-server capabilities
```

Before a release, run the headless smoke (stdio probe + optional provider
matrix, no secrets leaked):

```bash
scripts/release/app-server-smoke.sh --matrix        # dry-run plan
bash scripts/release/app-server-smoke.test.sh       # parser self-test (fake binary)
```

## Profile constitution

`profile_constitution` in runtime capabilities enables profile snapshots on
`POST /v1/threads/{id}/turns`. The optional `profile_constitution` field contains
`{accountId, revision, constitution}`. `constitution` is exactly
`{schemaVersion: 1, detail, initiative, collaboration, notes}`; choices are
`brief|balanced|detailed`, `check|judgment|moving`, and `direct|critical|coach`.
Notes are limited to 4,000 Unicode characters. Invalid data is refused.

The authenticated account transport supplies the snapshot, which participates in
the turn's replay identity. The Engine renders it through its existing personal
constitution renderer and records it in native session history. The snapshot is
unchanged across provider retries and compaction. A new snapshot fully replaces
earlier personal preferences. Permissions and approval policy are unaffected.
Internal follow-ups and RLM child calls inherit the admitted preferences;
they do not re-read the host operator's account in the middle of that work.

Without a supplied snapshot, the Engine reads the signed-in profile from the
configured account service at turn admission. When that Engine-loaded profile is
unavailable or invalid, the turn uses the signed-out local constitution and shows
a notice once per session; an invalid host-supplied `profile_constitution` snapshot still fails
the turn, and `GET /v1/constitution` still reports the error. An account without saved
preferences uses an explicit default snapshot; a signed-out account uses the
existing local constitution. Hosted transports always supply the owning
account snapshot, including defaults, so local preferences cannot leak between
accounts.

`GET /v1/constitution` reads the next-turn profile and its model guidance. It is
not a receipt that an active turn adopted the edit. `POST /v1/constitution/preview`
accepts a constitution document and returns `{modelGuidance, saved:false}` without
saving anything. Both routes use normal Runtime authorization. Older runtimes
must be upgraded before account transports submit profile-bearing turns.
