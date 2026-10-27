# Automatic Workflows

> 阅读简体中文版：[zh_hans/AUTOMATIC_WORKFLOWS.md](zh_hans/AUTOMATIC_WORKFLOWS.md)。

You do **not** need to write a `.workflow.js` file to coordinate agents. Operate
handles small or tightly coupled work directly. Multi-step delegation starts
with a compact Workflow plan: named steps, dependencies, bounded scopes, and
completion checks. Workflow runs the same sub-agents that Fleet configures and
manages, passing results and evidence between dependent steps. One bounded,
independent task can use a direct background agent; follow-up work should reuse
that agent with `followup`. Act/Agent can still use the optional soft-auto
policy described below.

Related docs:

- [Workflow Authoring](WORKFLOW_AUTHORING.md) — checked-in scripts and IR
- [Fleet + Workflow Tutorial](FLEET_WORKFLOW_TUTORIAL.md) — manual fleet paths
- [Configuration](CONFIGURATION.md) — `[workflow]` knobs
- [Sandbox](SANDBOX.md) — what the Workflow VM cannot do

## Soft-auto in Act/Agent

1. **You ask naturally** — “audit every crate for unsafe,” “scout then implement,”
   “compare these two providers in parallel.”
2. **Codewhale decides in Act/Agent** — broad, independent, or staged work can
   trigger Workflow; one-file edits, simple commands, and pure Q&A do not.
3. **It tells you first** — e.g. “This looks set up for a Workflow — three scouts
   then one verifier.”
4. **Optional setup** — if one or two facts would change the plan (read-only vs
   writes, scope, child count), it opens the **`request_user_input`** modal
   (structured multiple choice, not a long free-form interview).
5. **Launch** — structured `plan` JSON (goal / phases / children) or a short
   inline script. Parallel branches use `parallel()` partial-success semantics.

In Operate, those same asks use a compact Workflow plan when they need multiple
delegated steps. The plan makes parallel work, dependency handoffs, and the
evidence needed to finish visible together. Small or tightly coupled work can
stay in the parent under the active tool and approval policy; one bounded,
independent task can use a direct agent. Continue an existing agent with
`followup` when the task remains the same. You can always type `/workflow` to
request orchestration explicitly.

## Read-only auto-start vs write approval

`[workflow]` config (see `config.example.toml`):

| Knob | Default | Meaning |
|------|---------|---------|
| `automatic` | `true` | Legacy compatibility field; currently not a launch switch. The model chooses orchestration under the active tool and approval policy. |
| `auto_start_read_only` | `true` | Read-only plans may start without a write-approval card |
| `require_approval_for_writes` | `true` | Gates the plan-approval card for writes / elevated starts |
| `max_children` / `max_concurrent` / `max_depth` | `0` / `16` / `0` | Total children and structural plan depth are uncapped at zero; positive values are owner limits. Concurrency queues excess work. Runtime delegation has no default product depth cap and remains a separate authority setting. |
| `default_token_budget` | `0` | Declared token hint retained in receipts; `0` = no hint. It does not cap spend, child lifetime, or admission. |

Elevated work (writes, shell beyond read-only, network, secrets, worktrees, high
budget) surfaces an approval card with goal, child summary, capability flags,
and budget before launch (#4126) when `require_approval_for_writes` is on.
That flag only gates the card. Session-level auto-approve (YOLO / Full Access /
`bypass`) still skips it, the same as other ordinary `Required` tools.
Writes inside a running VM `task()` step are the VM runtime contract
(sandbox, `writeAuthority`, parent tool policy) — this flag does not re-ask
for each child write.

Worktree isolation and write ownership are separate. A write-capable `task()`
(`type: "implementer"`, or `writeAuthority: "workspace_write"` /
`"worktree_write"`) may declare repo-relative `writeRoots`, `exactFiles`, or
`coordinationContracts`; with none, the spawn boundary claims its
`deliverables`, or else the workspace root (`.`), exactly as a plain Agent
spawn does. The coordination ledger refuses a second live writer whose claim
overlaps, so when a script fans out parallel writers in one checkout it should
give each one disjoint `writeRoots` or `exactFiles`. Read-only roles cannot
declare write authority. `worktree: true` selects isolation but does not
silently grant mutation authority. A prompt-only general task is read-only.
`dependencies` and `acceptance` carry bounded child-specific prerequisites and
observable completion checks; they are not a parent-transcript copy.

When a workflow runs from a workspace containing multiple repositories, a
child that needs shell or file access must set `cwd` to the repository-relative
directory it should use. The host validates that the directory exists inside
the parent workspace before dispatch. Use `worktree: true` for isolated writes;
`cwd` selects an existing checkout and does not grant write authority or
isolation by itself.

## Controlling a run

`/workflow status [run_id]`, `/workflow cancel [run_id]`, and `/workflow
settings` are answered by Codewhale itself from the run journal and the live
run state — they never spend a model turn, so a status check is free and a
cancel lands even while the model is busy. `/workflow cancel` with no id stops
the only running workflow.

Starting work is review-first. `/workflow <objective>` and bare `/workflow`
ask the model for a bounded, tool-less proposal; `/workflow run
<path/to/x.workflow.js>` prepares a review of that exact checked-in source.
Neither form executes anything. After reviewing the proposal, run `/workflow
confirm` to launch the latest reviewed draft. The `[workflow]` table above is
read from your `config.toml` for every launch decision (auto-start,
write-approval card, child limits); `/workflow settings` prints the effective
values with what each one does. Reloading `config.toml` refreshes that table
for both settings and the workflow tool.

`/workflows` opens the run dashboard: every run this workspace's journal
keeps for the session — running and finished — newest first. Each row shows
the status token, the run's label, elapsed time, child count, and latest
progress; `Enter` opens the detail pane (run id, phases, the child roster
with per-child state, recent progress, and the error/result summary). `x`
cancels the selected running run through the same host path as `/workflow
cancel`, `r` re-reads the journal, and `Esc` closes. The dashboard never
launches anything — orchestration authority stays with `/workflow`.

## What you see while it runs

- **Workflow panel** — phases, children, status, budget
- **Compact history card** — one calm row that expands for detail
- **One artifact per delegated unit** — no duplicate “delegate + tool card”
- **Typed child identity** — labels/roles; no “unknown child” in the default UI

Cancel stops the run and child agents. Completed activity can persist across the
session (and across restarts when configured).

## Sandbox guarantees

The Workflow JS VM has **no** filesystem, shell, network, env, imports, clock, or
randomness. Allowed host calls: `task`, `parallel`, `pipeline`, `phase`, `log`,
`budget`, `args`. Real work happens in sub-agents / fleet under normal tool and
approval policy. See [Sandbox](SANDBOX.md).

## Synthesis and compatibility

- Prefer `responseSchema` on read-only children that must return structured fields.
  Write-capable children retain their change and verification evidence contract.
- Ordinary failed parallel slots become `null` (partial success); filter them
  before synthesizing one operator-facing summary. A `responseSchema` mismatch
  is a contract failure and intentionally fails the run instead of being
  silently converted to `null`.
- A `null` slot is no longer anonymous. `parallel()` and `pipeline()` attach a
  non-enumerable `errors` array to the result — `[{ index, kind, message }]`,
  ordered by index — so a synthesizer can say *why* a slot is missing. The
  array's own contents and JSON encoding are unchanged.
- `kind` is one of `admission`, `budget`, `cancelled`, `agent`, `schema`,
  `driver` (assigned by the host where the failure happened) or `script` (the
  script threw it). Read it from the thrown `Error`'s `.kind`; it is never
  inferred from message text, so a child's own prose cannot forge a kind.
- `opts.mode` selects the contract: `settled` (default — today's behavior),
  `fail-fast` (reject the whole fan-out with the first non-fatal slot error),
  or `partial` (resolve every non-cancellation failure to
  `{ __taskError: { index, kind, message } }`). An unrecognized mode throws
  rather than quietly reading as `settled`.
- A run whose every task failed is recorded as **failed**, not as a partial
  success, even when the script itself returned a value.
- Native plans settle ordinary failures in singleton, sequential, and parallel
  steps. Independent work continues; a failed or missing required result skips
  its dependents without launching them. The receipt retains surviving results
  and the skip reason. Partial loss is `degraded`; no surviving work is `failed`.
  Cancellation and explicit schema-contract failures still stop the run.
- Legacy Workflow token budgets survive only for parse/receipt compatibility and
  are absent from the advertised tool/plan schema. They are not caps. Aggregate token
  accounting is unavailable: `budget.metered` is `false`, and `budget.spent()`
  and `budget.remaining()` return `null`. `budget.total` retains the declared
  hint. Actual reported provider usage remains in child/run receipts.
- Model-authored child step, time, output and delegation-depth limits are not
  runtime authority. Legacy fields remain parse-compatible but are ignored;
  direct launch receipts identify ignored fields. Generated Workflow tasks omit
  legacy token/step/time hints entirely. Configure actual owner limits through
  the runtime's operator, role/Fleet, parent or saved-run controls. Stop remains
  authoritative. Continue until the outcome is complete or a real blocker needs a handback.
- Compatibility paths remain: `script`, `source_path` (checked-in
  `.workflow.js` / `.workflow.ts`), and structured `plan`.

## Choosing orchestration

The model can use direct tools instead of a Workflow for:

- One-file edits and tiny one-step asks  
- Simple commands / factual questions  
- Highly interactive design conversations  
- Risky writes without a clear decomposition  
- Plans that would exceed `max_children` / `max_depth` (refused before launch)

These are planning choices, not task-size cutoffs. Use the decomposition the
work needs; only the configured structural and authority checks refuse launch.

## Example scenarios (#4131)

Checked-in example workflows cover four automatic-Workflow scenarios:

1. Read-only repo audit  
2. Staged bug fix with worktree implementer + verifier  
3. Partial failure and synthesis  
4. Cancellation mid-run  

Fixtures: [`docs/examples/dogfood-automatic/`](examples/dogfood-automatic/).
Panel regression tests use the `dogfood_` prefix in
`crates/tui/src/tui/widgets/workflow_panel.rs`.
