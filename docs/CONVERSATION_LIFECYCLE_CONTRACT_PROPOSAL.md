# Conversation lifecycle contract: archive, close, delete, retention

Status: proposal for founder ratification. Nothing here is implemented, and
this document does not authorize the Engine `DELETE /v1/threads/{id}` route.
Tracking: Linear APPS-260, Engine issue #6914. Until ratified, the platform
keeps refusing deletion of Engine-bound conversations with
`thread_runtime_deletion_unavailable`, and the Engine keeps GET/PATCH only on
`/v1/threads/{id}`.

## Decisions requested

1. Ratify the four verbs and their meanings below.
2. Choose a default for each artifact class in the retention table.
3. Choose the child-state policy (compact vs refuse) for finished subagents.
4. Choose whether a deletion that cannot finish offline is shown as Pending or
   refused up front.

## Verbs

| Verb | Meaning | Reversible | Erases content |
| --- | --- | --- | --- |
| Archive | Hide from Recent. All history, receipts and artifacts stay. | Yes | No |
| Close | End the live runtime for the conversation (stop workers, release the process lease). History stays and the conversation can be resumed. | Yes | No |
| Delete | Permanently erase the conversation's content from every store named as "erase" below. A content-free tombstone remains. | No | Yes |
| Retention | Time-limited keeping of named facts after Delete (audit, usage, billing). Never conversation content. | n/a | Expires |

Archive and Close already exist or are cheap; Delete is the only new
capability. Delete is distinct from `DELETE /v1/sessions/{id}`, which removes a
saved document and unbinds threads while preserving their turns, and is not a
conversation erase.

## Scope of a delete

A delete targets one conversation identity: (Runtime store owner, execution
scope, thread id). It never recurses into:

- other conversations, including independent forks and child chats;
- workspace files the conversation touched;
- stores shared with other conversations;
- local-only Engine history of a different account or computer. Equal raw ids
  across account and local are not a mapping.

Account-managed (bound) and private local-only conversations are different
cases. Deleting a local-only conversation is an Engine operation on that
machine. Deleting a bound one also needs the account store to hold its cleanup
identity until the Engine acknowledges.

## Retention table (founder to ratify the "Proposed default" column)

| Artifact | Proposed default | Reason |
| --- | --- | --- |
| Thread, turn, item JSON; per-thread event JSONL; goal; mail envelopes | Erase | Canonical conversation content |
| Saved session document, branch journal, checkpoints, pre-import archive, recovery material | Erase | Copies of the same content |
| Approval receipt log (`approval_receipts.jsonl`) and torn-tail recovery files | Erase | Contains tool inputs and targets |
| Turn-operation bindings and history-operation receipts | Compact to a content-free tombstone (operation id, target id, timestamp, outcome) | Needed for idempotent retry and to prevent resurrection |
| Large-output and tool artifacts owned by the conversation | Erase | Content copies |
| Exports already written by the user (files they chose to save) | Retain, detach; never touched | User-owned, outside the store; a delete must say so |
| Shared artifacts (links, published reports, mirrored copies) | Do not claim erase unless the publisher supports revoke. Show "shared copy remains" at confirmation | Cannot be reached from the Engine |
| Workspace files the agent wrote | Retain | User's work product |
| Project and learned memory derived from the conversation | Retain unless the user separately removes it; state this at confirmation | Not a transcript copy |
| Account usage, metering, billing and audit facts | Retain content-free for the declared period | Legal and billing records |
| Provider-retained inputs | Out of scope; state that provider retention follows provider terms | Not controllable by the Engine |

The confirmation dialog must name the "retain" and "shared copy remains" rows
rather than imply total erasure. Doc and UI copy never name vendors or provider
ids to customers.

## Subagent state

Subagent state lives in the workspace, under `.codewhale/state/`, in
`subagents.v1.json` (the manager's `SUBAGENT_STATE_FILE`; the lock is
`subagents.v1.lock`), loaded by `load_state` and written by
`persist_state*` in `crates/engine/src/tools/subagent/mod.rs`. A legacy
`.deepseek/state/` copy is still read for migration.

Findings that drive the policy:

- A finished child is dropped from the live handle map but its prompt, result,
  checkpoint and owner content stay in that file. Settled status is therefore
  not proof that content is gone.
- The file is a workspace-wide fleet snapshot, not per-conversation. Rows for
  other conversations share it and must survive.
- Writes are debounced and sometimes spawned on a background thread; a late
  write can re-add erased rows after a naive delete.
- After restart, `load_state` marks previously Running agents Interrupted and
  rebuilds worker origins; legacy rows without origins get a synthetic legacy
  origin and cannot be attributed to a conversation.

Proposed policy:

1. Delete refuses (`conversation_child_content_custody_unavailable`) while any
   owned child is running, queued or has an unsettled completion delivery.
   No implicit cancellation.
2. For settled children owned by the target conversation, delete compacts only
   those rows (agents, worker records, queued follow-ups, resume targets,
   coordination entries) through the existing manager, under its process lock,
   leaving unrelated rows and user files untouched.
3. Rows with legacy or ambiguous ownership are not deleted and not guessed at;
   the delete refuses as unavailable rather than claiming completion.
4. Alternative the founder may prefer: leave child state in place and state
   plainly at confirmation that delegated-work transcripts persist until the
   workspace is cleared. This is cheaper but weakens the privacy promise.

No second child store is proposed.

## Durable lifecycle and restart recovery

Delete is a lifecycle, not one transaction across processes.

1. Admission refuses: running or queued turns, pending approvals, user input or
   dynamic tools, live shell Jobs, unresolved task or automation ownership, and
   unknown ownership. Unknown ids reveal nothing and create no marker.
2. A content-free intent (operation id, target, timestamp) is persisted in the
   Engine before any erasure, reusing the existing fingerprinted operation
   records. Once recorded, failure means Pending with recovery retained, never
   success and never a rollback to writable.
3. While an intent exists, every write, admission, resume, import and recovery
   path for the target refuses. Late callbacks cannot recreate content.
4. Erasure proceeds through the table above, then the intent flips to
   Completed and keeps only the tombstone.
5. On restart the Engine reads outstanding intents first, finishes erasure
   idempotently, and answers lookup by operation id. A retry with the same
   operation key returns the recorded outcome.
6. For bound conversations the account store holds an owner-scoped cleanup
   identity (the runtime binding) until the Engine's receipt is acknowledged,
   using the existing leased background-job queue. It must not cascade the
   binding away first. Engine offline or unsupported is shown as Pending or
   unavailable, never `deleted: true`.

Clients show Pending, Completed, Refused and Unavailable distinctly. Archive
remains available while Delete is unavailable.

## What this slice does not do

No route, no schema change, no migration, no client Delete button, no account
erasure change. The Engine release-owner hold on shared source for the full
deletion packet still applies; only the self-contained `approval_log.rs`
transaction repair (commit d7cbdad22) has landed from it.

## Acceptance once ratified

Actual account API, PostgreSQL, real Engine and native and web controls:
confirm and cancel; idle, active and pending-admission targets; duplicate
delete; response loss; Engine offline; crash between intent and effect;
account or computer switch; stale replay; restart and retry; re-reads showing
the "erase" rows gone and "retain" rows intact; unrelated chats, files, Jobs
and drafts surviving. Fault injection, local runs, hosted runs and a customer
completing the task are reported as separate evidence levels.
