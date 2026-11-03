# Delegated coordination contract

Codewhale records the small amount of shared state that parallel work needs to
remain attributable. This is coordination metadata, not an approval system and
not a store for model reasoning or transcripts.

## Launch and write ownership

Every write-capable child persists the same `ChildLaunchManifest` used by the
runtime. Its mutation claim contains normalized repo-relative directory roots,
exact files, and named contracts. Paths that are absolute or escape with `..`
fail validation.

A prompt-only general child starts read-only. Callers that want a writer must
declare at least one `write_roots`, `exact_files`, or
`coordination_contracts` value. Codewhale does not infer a repo-wide `.` claim.
An active shared-workspace claim blocks another active owner when either tree
contains the other, exact files collide, or a named contract matches. A real
isolated worktree may proceed concurrently. Scope expansion uses
`agent action=claim` (`write_roots`, `exact_files`,
`coordination_contracts`); a collision records a bounded contention receipt and
fails before mutation without opening a permission modal.

Fleet workers follow the same rule. Write-capable Fleet tasks declare
`workspace.writable_paths` or `metadata.coordination_contracts`, and the
resolved values are persisted in their launch manifest.

### Embedding state boundary

By default, delegated control-plane state remains workspace-scoped at
`<workspace>/.codewhale/state`: the worker ledger, complete transcript
artifacts, and coordination lock share that root. An embedding host may set
`EngineConfig::subagent_state_root` to keep those files under a session-owned
root without changing child cwd, tool path authority, or the execution
workspace recorded in receipts.

Different state roots intentionally form different coordination domains. They
do not exchange write claims or contention receipts even when their execution
workspace is the same. A host choosing that layout must serialize conflicting
writes itself or give writers isolated worktrees; the state-root override is a
storage and lifecycle boundary, not cross-session write arbitration.

This record is a cooperative Codewhale coordination boundary, not an operating
system sandbox. Fleet carries a machine-readable outer cap into each worker,
rechecks structured mutation targets, rejects symlink aliases, and denies
unbounded shell, Git, code, plugin, and mutating MCP execution. Those checks
prevent one Codewhale worker from silently exceeding its declared claim; they
do not promise containment against a separate hostile process racing filesystem
paths. Use an OS sandbox or an isolated host when that adversarial boundary is
required.

Authority-bound Fleet subprocesses are explicit leaves in v0.9.1. Their MCP,
LSP, snapshot, custom-tool, plugin, shell, and nested-agent startup surfaces are
disabled so configured background executables cannot bypass the structured
mutation path. The persisted receipt reports `max_spawn_depth = 0`.

## Decisions and projected context

Coordination schema version 1 persists decision records with a stable id,
subject, proposed/accepted/superseded status, one owner, applicability scope,
concise constraints, evidence handles, version, and sequence. Only the owner
may change a decision's status. A second accepted decision for the same subject
cannot silently replace the first.

At child launch, Codewhale projects only accepted decisions whose scope matches
the child's declared paths, contracts, role, or tool capabilities. The
projection is deduplicated, limited to eight decisions and 4096 UTF-8 bytes,
and receipted by child id and decision ids. The task prompt may separately
carry at most eight explicit dependency facts and eight observable acceptance
checks. Parent transcripts, secrets, and raw reasoning are never projected.

## Neutral fan-in

Conflicting candidates remain preserved as branch, patch, or artifact handles.
The neutral owner is the nearest common Planner/manager/operator in the
persisted parent tree, falling back to the root release owner. Neither candidate
author may claim that role. Reconciliation records:

- both or all input decision ids and candidate handles;
- a retry count and a limit of at most three;
- distinct independent Reviewer and Verifier evidence handles;
- a verified, failed, or blocked verification outcome; and
- the neutral disposition and bounded evidence handles.

Retry exhaustion is a terminal, inspectable receipt, not permission to discard
either candidate. Restart/replay preserves the schema, decisions, claims,
contention, projections, and reconciliation sequence.

## Inspection

`agent action=status` exposes concise per-child claims and accepted decisions.
The bounded decision, claim, contention, projection, and reconciliation
receipts, plus deterministic hottest-path counts, reach the TUI through
`CoordinationDetailProjection`; the `agents/coordinate action=inspect` tool that
used to serve them to the model is registered for transcript replay only and is
no longer advertised in the model catalog. Metrics without an authoritative
source, such as package growth or route cost, remain explicitly null instead of
being inferred.

## One model-facing surface

`agent` is the only sub-agent tool in the model catalog. The six narrow
`agents/*` tools stay registered so a persisted transcript replays against the
same implementations, but they declare `model_visible() -> false`: they are not
sent in the initial catalog and `tool_search` cannot return them. Everything
they did is reachable through an `agent` action — `status`/`peek` for `list`,
`message`, `followup`, `interrupt`, `wait`, and `claim` for the one capability
that had no equivalent, write-scope expansion.

## Durable workspace transactions and writer ownership

The current ledger is `.codewhale/state/subagents.v3.json`: one format3
envelope around the existing agent, worker and coordination schemas. It records
writer provenance, publication sequences and a durable revision. It is the sole
current state authority. Transcripts remain their existing separate artifacts;
legacy recovery bytes are retained backups, not another writable ledger.

A short OS transaction lock serializes store initialization, migration and every
read-current/check/mutate/publish operation. Claim overlap, decisions and their
sequence/session/scope receipts are checked against that current state before
acknowledgement. Unrelated writers' records are preserved. A stale snapshot
cannot replace a newer publication from its writer, an older launch generation
cannot replace a newer one, and a terminal receipt cannot be resurrected as
Running by a checkpoint. An explicit terminal retry accepts only the same
winning outcome; authoritative newer accounting metadata is retained.

Normal interactive/exec/Fleet/background sessions continue to share the
existing admission flock. The explicit exclusive Runtime constructor retains
its host admission policy; it is not the mechanism that conserves shared
snapshots. No manager holds the transaction lock while a child executes or
while it waits for a provider or channel capacity. Conflicting lock contention
returns a bounded retry error. Unsupported locking or failed store admission
refuses durable writes rather than silently creating an empty writable ledger.

Each writer holds its own boot lease. Other managers preserve its live records
and use its active claims during admission. An existing unlocked lease can
establish that its writer ended; a missing, invalid or unreadable lease is
uncertain and fails closed. Local task handles and a failed admission flock do
not establish that another process's work stopped. Parked and headless
WaitingForUser claim rules retain their existing lifecycle meaning; they do not
transfer a still-live writer's record ownership to a different boot.

Shared coordination mutations run through the existing manager transaction.
Failed operations do not automatically publish partial state. A deliberately
persisted contention receipt remains inspectable even though admission returns
an error. Fleet compensation is fenced by the exact store revision and the
same writer's successful publication; a stale snapshot cannot undo another
writer's work. Worker retention removes its owned records durably before
removing their transcript artifacts or evicting handles.

Taking over a proven-dead writer refreshes its exact durable worker and agent
projection and records that recovery before the requested mutation. The newer
snapshot sequence fences any older captured checkpoint, including for preflight.
Failed admission restores ownership from the last committed recovery image.
Synthetic recovery interruption is recorded as uncertainty and can be resolved
by the exact authoritative terminal outcome; it cannot authorize replay of an
admitted attempt. Unresolved recovery records remain retained. Local heartbeat
cleanup and retention act only on owned
records; transcript deletion also checks the committed retained identities.

Fleet restart preparation records the next exact generation as an unstarted
reservation. Cold recovery preserves that reservation until the matching Fleet
lease consumes it. The executor durably admits the full registered launch spec
before starting a process; an admitted or settled attempt cannot launch again
under the same generation. Current ledger cancellation can settle an unstarted
reservation only after any previous attempt has a confirmed outcome; an old
ledger attempt cannot consume its prepared successor.
An explicit controller may prepare that exact next generation for a live
foreign headless worker under the existing Fleet lease comparison, preserving
the execution owner's writer identity and every unrelated receipt. The owner
refreshes its durable generation before its next mutation; old-generation
checkpoints cannot undo the preparation.
The existing Fleet ledger and executor move through the bounded blocking owner
with the same manager-lock file. Successful completion reconciles the worker
projection after processing the executor's terminal event.

The per-worker `launch_admission_version` distinguishes the current durable
pre-spawn protocol from legacy rows. A missing legacy start timestamp or a
legacy terminal projection does not prove process teardown. Preparation retains
`prepared_previous_attempt_unsettled` until the owning executor confirms the
exact prior run, task, generation and full launch spec, or an authoritative
attempt receipt supplies that proof. A cold owner without either proof refuses
the reserved launch and retains its uncertainty. It does not invent a Failed
outcome or release a live writer's claim. Busy coordination yields without
leasing; unrelated foreign workers remain untouched.

The existing Fleet executor retains observed terminal output, the prepared
receipt and stopped host until publication is acknowledged. Final stream tails,
including usage, and the terminal receipt form one `TaskAttemptFinalized` ledger
record. An exact retry synchronizes the same candidate instead of appending
usage again. A visible record after a failed sync is not an acknowledgment.
The ledger remembers successful durability only for the same owned file and
length; appends, replacement and compaction invalidate that receipt.

Terminal completion keeps the public Running gate and its ownership until the
exact terminal result is durably recorded and parent/mailbox/host fan-in is
queued. The admitted bounded blocking owner performs that complete sequence,
so dropping its waiter cannot discard an admitted commit. A filesystem or
publication failure produces uncertainty, retains the winning pending result
and claims, and sends no successful completion. A later retry uses that same
winner. Failed live delivery after a successful commit leaves the durable
terminal fact intact and reports the delivery failure. Explicit in-memory
managers remain in-memory; their results are not durable receipts.

Current store publication propagates file-write and Unix directory-sync errors.
Opening or observing a new store revision resolves its directory durability
before acknowledging idempotent state, and exact terminal retries repeat that
barrier. Unchanged preflight does not add a directory sync.
An error after rename can still mean the new image is visible, so recovery reads
the committed image and never announces success solely from a failed waiter.
Native filesystem and power-loss qualification remain separate evidence.
Canceling a Fleet waiter retains an admitted blocking batch and its lock until
the batch returns. Dropped executor ownership uses the existing parent-death
pipe or Windows Job containment; that fallback does not create a successful
terminal receipt or claim that a remote effect was undone.

### Legacy cutover and recovery

The first current-format admission takes the legacy admission lock exclusively
only for migration, after previous holders have left. It retains the original
legacy bytes and a validated recovery copy, then leaves
`subagents.v1.json` as a private directory containing that recovery material.
The current format3 file is published under the same short transaction lock.
Interrupted cutover states are validated and resumed without replacing a later
current-format revision with the old backup. Future or malformed envelopes
cannot fall back to parsing as legacy state.

The retained directory prevents the inspected older atomic file publisher from
replacing that pathname with its stale snapshot. It does not prevent an old
binary from executing tools or changing a workspace. Concurrent arbitrary old
binaries are not a supported current coordination domain; close them before
cutover. Recovery data is not silently deleted, and migration qualification
uses isolated synthetic state rather than existing user sessions. Different
explicit state roots remain separate coordination domains as described above.
