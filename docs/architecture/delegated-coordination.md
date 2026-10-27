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

## The workspace lock, and what losing it does and does not mean

The ledger lives in one file, `.codewhale/state/subagents.v1.json`, written as a
whole-document atomic replace. One per-workspace advisory flock
(`subagents.v1.lock`) decides who may *write* the file, in one of two modes:

- **Shared, warn on failure.** Interactive and `exec` sessions, `codewhale
  fleet`, `codewhale resume`/`fork` mounts and the TUI's background task runtime
  (`/task`, `task_create`, automations) take the flock shared, so several
  sessions in one folder hold it at once. Failing to get it only logs a warning.
- **Exclusive, degrade read-only.** `codewhale serve` and the other standalone
  Runtime hosts take it exclusively and refuse every durable ledger append when
  another process already holds it; they log the refusal and pick the ledger back
  up when the process restarts after that owner exits. Beside such an owner,
  shared holders cannot append.

Every shared holder may append, and each append replaces the whole file, so the
last rename wins. The ledger is loaded once at boot; agents another process
started reload as non-live, so no other process's write claims are enforced
here. A resume or fork mount only reads the ledger.

A shared holder beside an exclusive owner, or one whose acquisition failed, does
not get the flock. That is an ordinary state, not a failure:

- **It does not affect liveness.** A session that cannot write the ledger runs
  its own agents normally. Whether an agent is alive is decided by heartbeat
  evidence, never by lock ownership. (Before v0.9.4 the cleanup pass
  terminalized every running agent with no live task handle purely because this
  process lacked the flock; that coupling is gone.)
- **It does not affect reads.** The boot-time load is unconditional. Such a
  session sees the workspace's decisions and write claims even though it cannot
  append to them. Gating the load on the write flock previously left a session
  without the flock holding an empty default ledger, which it would write straight over
  the real one the moment the first session exited and the flock became
  acquirable.
- **It does mean no durable ledger appends.** Decision, claim, contention, and
  reconciliation mutations still require the flock, and so does any
  shared-workspace write-capable launch, because such a launch must be durably
  replayable before it executes. A session without the flock can therefore
  delegate only read-only and isolated-worktree work. Two shared holders can
  both launch shared-workspace writers, and neither enforces the other's claims
  (see the known gap below).

Known gap: every session holds the ledger as of its own boot, and shared holders
rewrite the whole file. Records another session appended after this one booted
can be lost when this one writes, and write claims are only enforced within one
process. Closing that needs per-session ledger segments unioned on read, so that
no two processes ever write the same file and the claim-overlap check runs
against the union. That work is not done.
