# @codewhale/runtime-sdk

Small JavaScript helpers and TypeScript declarations for Codewhale's local
Runtime API. The package is intentionally transport-only: it never bypasses the
Rust runtime, sandbox, approvals, provider configuration, or Runtime execution
ledger.

Version 0.10.3 wraps Fleet operations, read-only thread event streaming and
client-executed dynamic-tool result delivery.
It does **not** provide thread creation/resume, turn start/steer/interrupt,
approval, clarification or model discovery helpers. Those operations already
exist on the full Engine HTTP API; use the recipe below. This package is
distinct from the hosted model/Responses API SDK, `codewhale-sdk`.

Start the full API with `codewhale app-server --http`. Bare `app-server`,
`--stdio` and `--socket` are smaller compatibility interfaces. Their turns
require explicit `allowed_tools: []` and cannot run interactive tools or start
background goals.
Require `tool_free_turns: true` in capabilities before relying on that restriction;
older compatibility servers may ignore the allowlist field.
The supervisor supplies `CODEWHALE_RUNTIME_TOKEN`; a generated Runtime token
is not printed for discovery. Send the supplied token as a bearer credential.

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

// Creation is durable but does not launch work. Launch remains explicit.
await client.startFleetRun(created.run.id);

let cursor;
for await (const event of client.fleetEvents(created.run.id, { after: cursor })) {
  if (event.cursor) cursor = event.cursor; // persist durable cursors only
  if (event.event === "fleet.replay.cursor_unavailable") {
    // Reload getFleetRun(created.run.id), then reconnect without the old cursor.
  }
}
```

## Shared Runtime contracts

The existing thread-event declaration consumes the Runtime envelope generated
from `crates/app-server-protocol`. Image, capability and dynamic-tool types are
exported from the same generated file; the SDK does not maintain another copy.
Generate the JSON Schema and TypeScript package outputs with
`cargo run -p codewhale-app-server-protocol --features schema --bin codewhale-app-server-schema -- --write`.
Use `--check` to verify the committed schema and both TypeScript destinations.
The SDK result helper consumes these generated dynamic-tool contracts. This
does not add Codex RPC methods or turn-start helpers.

Input defaults remain optional in DynamicToolSpec and DynamicToolCallResult.
DynamicToolSpecOutput describes the serialized spec published in thread metadata;
Runtime event/capability fields describe the server's emitted representation.
Generated Json admits omitted undefined object members, while its root and array
values exclude undefined. The Native host generator keeps its existing format.

## Deliver a dynamic-tool result

Pass the exact pending call from `snapshot.pending_dynamic_tool_calls` or a
validated `tool_call.requested` journal payload, together with the result from
your registered client-side tool:

```js
await client.deliverDynamicToolResult(call, {
  success: true,
  content: [{ type: "input_text", text: output }],
}, { signal });
```

The generated `DynamicToolCallParams` supplies all three Runtime identifiers;
the helper encodes each path segment and submits once to the existing result
endpoint. It resolves with no value after the empty HTTP 202 acknowledgement.
That confirms result acceptance, not turn completion. Continue reading the
Runtime journal or thread snapshot for the terminal outcome. Unknown, expired
or already settled calls remain `RuntimeApiError` responses with their actual
HTTP status; a 404 is not inferred to mean that the endpoint is unsupported.

The helper neither executes tools nor invents results. An aborted request or
lost response leaves acceptance uncertain and does not cancel Engine work;
reload the canonical pending calls and journal before deciding what to do.
It never automatically retries a mutation. Authenticated requests and this
mutation refuse redirects, including unauthenticated use of the helper.

## Fleet Helpers

- `listFleetRuns()`
- `getFleetRun(runId)`
- `listFleetWorkers(runId)`
- `getFleetWorker(workerId)`
- `interruptWorker(workerId)`
- `stopWorker(workerId)`
- `restartWorker(workerId)`
- `stopFleetRun(runId)`
- `startFleetRun(runId)`
- `replayFleetEvents(runId, { after, limit })`
- `fleetEvents(runId, { after, limit })`
- `createFleetRun(spec)`

The Runtime implements the local managed-Fleet path. Fleet
names the roster and selected member; the Runtime owns launch authority,
durable run/worker/event state, replay, and execution. A creation request must
name its roles, define a `parallel` Workflow, and select the explicit
`this_computer` target. `another_computer` and `cloud` are contract values but
fail closed until those targets are implemented. Event cursors are opaque and
durable across Runtime restarts; if Runtime ledger compaction removes an old
cursor, replay returns a conflict so the client can reload the run projection.
Local worker IDs are generated per run; managed creation does not yet accept
caller-assigned `worker_specs` because worker controls address IDs globally.

Older runtimes that do not expose one of these endpoints produce a
`RuntimeCapabilityError` with a stable capability string instead of a generic
fetch failure.

## Full Engine API

The existing HTTP API supplies the controls missing from this package. This
example uses Node's built-in `fetch` and the same token as the client above;
it does not add a second session store, turn loop or approval authority.

```js
import { randomUUID } from "node:crypto";

const baseUrl = "http://127.0.0.1:7878";
const token = process.env.CODEWHALE_RUNTIME_TOKEN;
if (!token) throw new Error("The supervisor must supply CODEWHALE_RUNTIME_TOKEN");

async function runtime(path, { method = "GET", body, signal } = {}) {
  const url = new URL(path, baseUrl);
  if (url.origin !== new URL(baseUrl).origin || url.username || url.password) {
    throw new Error("Runtime requests must stay on the configured origin");
  }
  const response = await fetch(url, {
    method, signal, redirect: "error",
    headers: {
      authorization: `Bearer ${token}`,
      accept: "application/json",
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!response.ok) throw new Error(`Runtime HTTP ${response.status}: ${await response.text()}`);
  return response.status === 204 ? null : response.json();
}

const info = await runtime("/v1/runtime/info");
if (!info.capabilities.thread_create_idempotency ||
    !info.capabilities.turn_operation_idempotency ||
    !info.capabilities.turn_operation_lookup) {
  throw new Error("This client requires Runtime operation-key recovery support");
}
const providers = await runtime("/v1/providers");
const createIntent = { operation_key: randomUUID(), workspace: process.cwd(), mode: "agent" };
const thread = await runtime("/v1/threads", { method: "POST", body: createIntent });
const threadPath = `/v1/threads/${encodeURIComponent(thread.id)}`;
const snapshot = await runtime(threadPath);
```

Persist `createIntent` before sending it. Recover an uncertain creation with
the same operation key and original body; never mint a new key just because a
response was lost. A turn likewise uses a persisted `operation_key`; inspect
`GET /v1/threads/{id}/turn-operations/{operation_key}` after an uncertain turn
response. HTTP acceptance is not terminal completion. Read the canonical
snapshot, turn status, effective route metadata and events before reporting
the outcome. An aborted fetch closes the request; it does not cancel Engine
work. Use the explicit interrupt endpoint for that.

| Control | Authenticated HTTP request |
| --- | --- |
| Discover capabilities | `GET /v1/runtime/info` |
| Discover models | `GET /v1/providers`, then `GET /v1/providers/{provider}/models?limit=250`; retain `model_provider_id` for configured routes and follow `nextCursor` |
| Create/read a thread | `POST /v1/threads` / `GET /v1/threads/{id}` |
| Resume | `POST /v1/threads/{id}/resume` |
| Start a turn | `POST /v1/threads/{id}/turns` with `{ "prompt": "…", "operation_key": "…" }` |
| Steer active work | `POST /v1/threads/{id}/turns/{turn_id}/steer` with `{ "prompt": "…" }` |
| Interrupt | `POST /v1/threads/{id}/turns/{turn_id}/interrupt` |
| Decide an approval | `POST /v1/approvals/{approval_id}` with `{ "decision": "allow" }` or `{ "decision": "deny" }`; `remember` defaults to false |
| Answer clarification | `POST /v1/user-input/{id}/{request_id}` with `{ "answers": [{ "id": "question-id", "label": "selected option", "value": "answer" }] }` |

Encode each path identifier separately with `encodeURIComponent`. Decisions
must come from the user and target the exact pending request from that thread's
canonical `pending_approvals` or `pending_user_inputs`, including its turn/tool
identity. Submit them while work is active; do not queue them behind completion.
Expired or already answered IDs fail rather than settling another waiter.
Use the existing `threadEvents()` helper for SSE and its cursor/end rules below.
The snapshot's `latest_seq` is the initial subscription cursor; recover a
replay gap by reloading the snapshot, without replaying a mutation.

This table is the core client journey, not an exhaustive schema or a claim
that every planned capability is enabled. Honor `info.capabilities` and the
[Runtime API contract](../../docs/RUNTIME_API.md), including permission,
operation-key recovery and authenticated stream handling.

## Read a thread journal

`threadEvents(threadId, { sinceSeq, replayLimit, signal })` reads the existing
`GET /v1/threads/{id}/events` SSE endpoint. It never creates a thread or starts
a turn. Pass an `AbortSignal` to close the subscription. Redirects are refused,
and incomplete or oversized frames fail instead of producing partial records.

The returned `seq` and `previous_seq` belong to Runtime. Keep the last accepted
`seq` for reconnects; sequence numbers need not be consecutive. Consumers should
validate the selected thread and predecessor cursor before advancing their own
read position. Authentication uses the client constructor's existing `token`
option and stays in the Authorization header.


Consumers that present **current** activity can pass `includeProgress: true`.
The same endpoint adds `progress=true` and advertises support with
`x-codewhale-event-progress: 1`. A Runtime without that capability fails
explicitly; a historical event is not a readiness signal.

Opt-in streams include `{ event: "stream.progress", thread_id, seq, state }`,
where `state` is `replaying` or `live`. These are transport frames at the existing
journal cursor, not journal events or new sequence numbers. Initial replay and
broadcast-lag recovery are `replaying`. The stream becomes `live` only after
both durable history and the already queued live tail have been drained. A
request answered during replay therefore settles before readiness is reported.
Later lag can return the same connection to `replaying`. Consumers should stop
extending activity while replaying or disconnected and validate the thread and
cursor. Default streams retain the original event-only contract, plus the
final frame below.

### Stream end

Whenever the Runtime ends a thread stream on purpose, the last item is
`{ event: "stream.end", thread_id, reason, last_seq, retryable }`. It is not a
journal event and has no `seq`. When `retryable` is true, call `threadEvents`
again with `sinceSeq: last_seq`; otherwise stop and reload the thread snapshot.
Runtimes that send it advertise `x-codewhale-stream-end: 1`. If the iterator
ends without it, the connection was lost: resume from the last `seq` you
accepted. The full client rule is in
[`docs/RUNTIME_API.md`](../../docs/RUNTIME_API.md#ending-and-resuming-a-thread-stream).

Use `isThreadStreamEnd` to narrow iterator items in TypeScript. Journal event
names remain open-ended, so checking `event === "stream.end"` alone cannot
narrow the union:

```ts
import { isThreadStreamEnd } from "@codewhale/runtime-sdk";

for await (const item of client.threadEvents(threadId)) {
  if (isThreadStreamEnd(item)) {
    console.log(item.reason, item.last_seq, item.retryable);
  } else {
    console.log(item.seq, item.payload);
  }
}
```
