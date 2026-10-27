# DingTalk bridge (advanced self-hosted integration)

This source adapter connects a DingTalk enterprise robot using Stream mode to an
existing `codewhale serve --http` Engine. It forwards tasks, approvals, answers,
stop requests and Engine events; the Engine owns prompts, tools, permissions,
turns and conversation history. No second agent runtime is started.

Codewhale-hosted bots are the intended default product experience. This optional
operator-managed adapter is for an enterprise's own DingTalk application and
Engine; its credential and Node.js setup is not the default user onboarding flow.
It does not implement hosted installation, Codewhale account linking, enterprise
ISV authorization or a tenant credential service.

The adapter is source-ready for offline verification. No DingTalk application,
credential, production connection or deployment is configured by this repository.
Live qualification requires a separately authorized tenant and runtime.

## Dependencies and setup

Use Node.js 22 or newer. From this directory, install the lockfile with
`npm ci --ignore-scripts`. The official MIT-licensed
[`dingtalk-stream`](https://github.com/open-dingtalk/dingtalk-stream-sdk-nodejs/tree/v2.1.7-beta.1)
SDK is pinned to registry release `2.1.7-beta.1`; this is an explicit prerelease
selection, including the SDK's reconnect and disconnect lifecycle fixes. The
lockfile pins its transitive dependencies. No SDK implementation was copied.

Create an enterprise robot with Stream mode and subscribe to robot messages.
The adapter uses the SDK's outgoing connection, not a public callback server.
Follow the official [Stream protocol](https://open-dingtalk.github.io/developerpedia/docs/learn/stream/protocol/)
and [SDK example](https://github.com/open-dingtalk/dingtalk-stream-sdk-nodejs/blob/v2.1.7-beta.1/example/index.ts).
Do not enable or connect an application until its owner has approved activation.

Copy `.env.example` to `.env`, fill it locally, and use
`node --env-file=.env src/index.mjs --validate-config` to validate without opening
any connection. The normal entry point is `node --env-file=.env src/index.mjs`.
`npm run check` performs syntax checks and does not connect.

| Setting | Meaning |
| --- | --- |
| `DINGTALK_CLIENT_ID`, `DINGTALK_CLIENT_SECRET` | Enterprise application credentials |
| `DINGTALK_TENANT_ALLOWLIST` | Comma-separated exact corporate IDs; both robot and sender corporation must be listed |
| `DINGTALK_ACTOR_ALLOWLIST` | Comma-separated exact `senderCorpId:senderStaffId` pairs |
| `DINGTALK_ALLOW_GROUPS` | `false` by default; only literal `true` or `false` is accepted |
| `DINGTALK_CONVERSATION_ALLOWLIST` | Required for groups; exact `chatbotCorpId:conversationId` pairs |
| `CODEWHALE_RUNTIME_URL`, `CODEWHALE_RUNTIME_TOKEN` | Existing authenticated Engine HTTP endpoint; HTTPS or loopback HTTP |
| `CODEWHALE_MODEL` | Existing Engine model or route, defaults to `auto` |
| `CODEWHALE_WORKSPACE` | Optional workspace passed to Engine thread creation |
| `CODEWHALE_ALLOW_SHELL` | Defaults to `false`; enabling shell availability still requires Engine approval |
| `DINGTALK_STATE_PATH` | Defaults to `.state/dingtalk.json` relative to the working directory |

Tenant and actor allowlists are mandatory; there is no allow-unlisted option.
Group requests also require `isInAtList: true` on every message. Sessions are
separate for each application, bot tenant, conversation, sender tenant and staff
ID. Group responses are visible to that group, so only enable appropriate groups.
Allowlisting a user does not enable trust mode or blanket approval: both are
always disabled on this adapter's requests.

## Conversation controls

Send a text task, then use these commands in the same conversation and identity:

- `/status` reports the bound thread, active turn, retained admission and uncertain deliveries.
- `/stop` or `/interrupt` requests cancellation of that human's accepted active turn.
- `/allow APPROVAL_ID` or `/deny APPROVAL_ID` decides one live Engine approval.
- `/answer QUESTION_ID response` answers a live question. Multiple questions need one answer per line. A plain reply also answers the sole pending question for the current turn.
- `/help` shows the available controls.

Every approval and answer is checked against the current Engine pending request
and immutable recorded initiating actor. Another user's message cannot authorize
the turn. Incoming `sessionWebhook` credentials never enter the Engine prompt.

## Durability and recovery

Run one process per state file. An exclusive `.lock` file rejects concurrent
instances. State is private (`0700` directory and `0600` file), replaced through
the shared fsync-backed store. Retain it across restarts; it contains sensitive
conversation text and short-lived reply credentials. Do not commit or publish it.

The adapter durably saves an authenticated inbox entry and its message identity
before acknowledging Stream delivery. It retains 2,000 completed message keys;
queued messages and pending admissions are independently retained. The queue is
bounded, and a full queue is not acknowledged. Unsupported or unauthorized input
is acknowledged without contacting the Engine. Text-only input is supported.

Turn submission uses the shared bridge-core admission helper. When the Engine
advertises both operation idempotency and lookup, the original request and key
are retained, and recovery queries that exact key before retrying. Only an exact
404 permits the unchanged request to be submitted again. Conflicts and lookup
failures retain the receipt. Without both capabilities, an uncertain submission
is never repeated. Reconnect resumes the same Engine event cursor and accepted
turn. SDK Stream reconnect is enabled independently of Engine SSE reconnect.

Each output chunk is saved before sending and marked in flight before its one
HTTP attempt. A timeout, rejected response, restart during sending or uncertain
receipt marks that chunk uncertain; it is not resent automatically. A successful
response requires HTTP success and DingTalk `errcode: 0`. Unattempted output waits
when its session webhook expires, and a new authorized message can supply a fresh
route for the same session. Only HTTPS `oapi.dingtalk.com/robot/sendBySession`
routes with a future expiry are accepted; redirects are rejected. No proactive
message API or alternate recipient is used. SDK warning objects are suppressed
in the dedicated process to avoid exposing transient connection credentials.

After an abrupt shutdown, verify that no bridge process still owns the state
file before removing a stale `.lock`. Back up the state first. Operators must
reconcile uncertain thread creation, non-idempotent command effects, admission
without lookup support, and uncertain output against Engine/DingTalk receipts.
Do not remove retained receipts or reset the state as a retry mechanism. A
thread-creation request without a confirmed ID is deliberately not retried.

This first adapter does not support attachments, interactive cards, sharing a
single conversation between different staff identities, browsing existing Engine
threads or automatic output reconciliation. Commands interrupted during a crash
are reported uncertain rather than replayed. Response capture is capped at one
million characters with an explicit transcript notice. A host filesystem and one
process provide durability; this is not a distributed queue. Keep provider calls,
tenant messaging and production deployment separate from offline checks.
