# Workflows and goal mode — parity audit (Grok Build, DeepSeek Harness, Codewhale)

Date: 2026-08-15. Evidence: the local Grok Build source sync at
`/Volumes/VIXinSSD/CW/grokbuild` (SOURCE_REV
`e6a67a5408288c98380cd13f3b1fe1fbc01c9f1f`, `crates/codegen/xai-workflow`,
`xai-grok-shell/src/session/{goal_*,workflow/*}.rs`), the installed DeepSeek
Harness `@deepseek-ai/dsh@0.1.0-rc.6` package READMEs (`dsh-goal`,
`dsh-tool-goal`, `dsh-goal-round-driver`, `dsh-command-goal`,
`dsh-tool-workflow`, `dsh-workflow`, `dsh-tool-ralph`, `dsh-tool-todo`,
`dsh-schedule`) and `config/agent-presets/standard/agent.cordis.yml`, plus a
real Codewhale dogfood run (Ollama `qwen3:4b`, isolated home) captured under
`scratchpad/wfgoal-captures/` (`g0*-*.txt`, `w0*-*.txt`, `wf-cli-run.log`).
Public Grok Build docs found by search:
[Developers Digest guide](https://www.developersdigest.tech/blog/grok-build-developer-guide-2026)
(`grok goal "…"`, `grok goal status|pause|resume|clear`, `--max-turns`,
Arena Mode) — official docs at x.ai/cli returned 404 for the deep pages.

## What Grok Build does (from source)

- Goal mode is a state machine (`GoalTracker`): phases Idle → Planning →
  Executing; statuses `active`, `user_paused`, `back_off_paused` (run cap),
  `no_progress_paused` (verifier flagged the same gaps twice), `infra_paused`
  (turn error), `blocked`, `budget_limited`, `complete`. Unknown persisted
  statuses restore as `user_paused` — a goal never resurrects as
  self-driving after a restart. A goal planner, an adversarial "skeptic
  panel" classifier, a strategist that can restructure after stalls, a
  summarizer, and a next-step generator sit around it. `/goal status`,
  `/goal pause`, `/goal resume`, `/goal clear` are native.
- Workflows are Rhai scripts (`agent()`, `parallel()`, `phase`, `log`,
  `budget`, scratch files, `git_diff_since`) with `meta {name, description,
  whenToUse, phases}`, a call budget (`DEFAULT_AGENT_BUDGET = 128`), a
  request-hash journal that replays completed calls so an edited or resumed
  workflow does not repeat work, persisted run manifests (up to 128 restored
  runs), pause kinds `user|back_off|no_progress|verification|infra`, saved
  workflows in `.grok/workflows/*.rhai` (project + user scope, trust-checked
  paths, name == meta.name), one builtin (`deep-research`), and native
  `/workflows`, `/workflow resume <name>`, `/workflow stop <name>`,
  `/workflow review`.

## What DeepSeek Harness does (from the installed package)

- The goal domain is event-sourced session state (`goal/change` events with
  full snapshots, compare-and-set `{id, revision}`), one current goal, phases
  active/paused/blocked/complete, `defaultMaxGoalRounds = 256`. Activation
  (permission to auto-continue) is process-local and never persisted: every
  session start/fork disarms it and a human `/goal resume` rearms.
- The model-facing policy is what makes it enter goal mode readily:
  "`create_goal` may infer goal intent from a direct human request in any
  language; do not create a goal for routine single-turn work … Mark
  complete only when the objective is actually achieved. Mark blocked only
  after the same blocking condition persists for at least 3 consecutive goal
  rounds." Creation is only accepted from a direct human turn of a root
  agent — subagents and non-human producers are rejected at execution.
- The round driver queues one retained `<goal_round>` user message per round
  naming the objective and `round/maxGoalRounds`; human messages never
  consume rounds; cancellation pauses the goal so it cannot auto-restart.
- `/goal` (native command, no model turn) shows objective, phase, rounds and
  the valid next commands; `/goal <objective>` never replaces an unfinished
  goal without an explicit clear; `/goal edit|pause|resume|clear`.
- Workflows: the model writes a JavaScript script (`agent()`, `parallel()`,
  `pipeline()`, `phase`, `log`, `args`) run by a worker-thread engine;
  guidance says to use it "ONLY when the user explicitly asks for a workflow
  or for large multi-agent orchestration"; foreground collection only, no
  journaling/resume, no saved or nested workflows, no token budget
  vocabulary (all stated in the package's Known Limitations). `ralph` runs a
  fixed fresh-child loop (`maxRounds` 64 in the standard preset);
  `todo_write` is the standing plan strip; `dsh-schedule` gives durable
  reminders (`after_seconds`, `at`, `every_seconds` ≥ 5 min).

## What Codewhale does today (release candidate 533c530be + integration tip)

- `/goal <objective> [budget: N]`, `pause|resume|done|blocked|clear`,
  `declare-hunted`; state Active/Paused/Complete/Blocked with pause reasons
  `user|backoff|no_progress|usage_limit|budget_limit`, gap-fingerprint stall
  detection, completion verification (critical/advisory reviews), unlimited
  continuations by default (`[goal] max_continuations` backstop, like Grok
  Build's call cap), typed continuation prompt, thread-scoped goal
  persistence for app-server clients (`thread/goal/*`). Interrupting a turn
  keeps the goal active (design decision 2026-07-24) and only cancels the
  auto-continuation timer.
- Workflows: JS authoring lowered to a typed `WorkflowSpec` (compile-only
  subset), `task()/parallel()/pipeline()/phase()/log()/budget/args`, soft-auto
  launch policy, plan-approval cards, per-run token budgets, worktree write
  ownership, gates, per-event run journal `.codewhale/workflow-runs.jsonl`
  with restart reconciliation, live workflow panel + history card,
  `codewhale workflow run <name|--source-path> --runtime tmux|inline`.

## Dogfood findings (before this lane)

1. `/goal help` created a goal named "help" (no reserved help/status words).
2. Bare `/goal` on an empty session spent a model turn asking the model to
   invent an objective; a 4B local model answered with tool-syntax noise
   (`g02-goal-bare-nogoal.txt`).
3. `/goal` status used bracket tags (`Goal [HUNTING]: …`) and, after Esc, gave
   no hint that nothing was driving the goal any more.
4. `/workflow status` and `/workflow cancel` were routed *through the model*
   ("Call the `workflow` tool with action `status`…") — a status check cost a
   model turn and a cancel depended on a busy model obeying; the panel's
   cancel control pre-filled that same command.
5. The `[workflow]` table documented in `docs/AUTOMATIC_WORKFLOWS.md`
   (`automatic`, `auto_start_read_only`, `require_approval_for_writes`, …)
   was parsed but ignored: the tool consulted `WorkflowConfigToml::default()`
   ("Product defaults … when the tool has no live Config handle").
6. The example workflows' header says `Run: /workflow run <path>` but the
   slash command had no `run` verb (it became an objective for the model).
7. `create_goal` was gated on "only when the user explicitly asks" — the
   entry-path difference from DSH.
8. A checked-in workflow ran end to end through the CLI with the local model
   (`wf-cli-run.log`: two children, result `{"a":"hello","b":"world"}`,
   journal + report written), and the TUI status can now read it.

## Matrix

| Row | Grok Build | DeepSeek Harness | Codewhale | Verdict |
| --- | --- | --- | --- | --- |
| Authoring | Rhai script + meta; builtins; saved `.grok/workflows` | model-written JS script only | JS compile-only subset + structured `plan` + checked-in `.workflow.js`; `codewhale workflow run` | parity (different surface); saved-workflow *registry* with names is missing — checked-in files exist but no `/workflow list <saved>` |
| Triggering | manual `/workflow`, `/workflow resume` | model tool; `dsh-schedule` reminders | manual `/workflow`, soft-auto launch, `codewhale workflow run`, automations (`~/.codewhale/automations`) | parity; scheduled workflow runs deliberately not added here |
| Goal definition | `/goal`, planner phase | `/goal`, model-inferred `create_goal` | `/goal`, model `create_goal` — now inferable (this lane) | parity |
| Checkpoints / resume | journal replay of completed calls; run manifests; `/workflow resume` | none (documented limitation) | per-event journal + restart reconciliation; no replay-resume | partial (Codewhale ≥ DSH, < Grok Build) — follow-up |
| Approvals inside runs | plan-approve loop | tool approvals per child | plan-approval card gated by `[workflow]` (`require_approval_for_writes` / `auto_start_read_only`); session auto-approve still bypasses the card; writes inside a running VM step follow the VM runtime contract | parity |
| Progress / state visibility | `/workflows`, `/goal status`, tasks pane | web plan strip, `/goal` | workflow panel, work bar, `/workflow status` (now native), `/goal` (now plain, idle hint) | parity |
| Receipts / history | run manifests | session log | `.codewhale/workflow-runs.jsonl`, `/workflow status` lists journaled runs, trophy cards | parity |
| Failure handling / retry | pause kinds; strategist restructure | model judgment; ralph rounds | verifier gates, no-progress pause, blocked, `[goal] max_continuations` | parity; auto-restructure (strategist) deliberately absent |
| Cancellation | `/workflow stop <name>` native | cancel via signal | `/workflow cancel [id]` native (this lane); Esc cancels children | parity |
| Fan-out inside a workflow | `parallel()`, agent budget 128 | `parallel()/pipeline()`, no budget vocabulary | `parallel()/pipeline()`, 16 live / 1000 total, token budget | parity |
| Interrupt semantics | Ctrl+C pauses the goal | cancellation pauses the goal | Esc keeps goal active, cancels the timer, `/goal` now says how to continue | deliberately different (2026-07-24 dogfood decision) |
| Docs accuracy | — | — | fixed: `/workflow run`, `[workflow]` honored, `/goal` verbs | fixed |

## What this lane changed

- `/workflow status|runs [run_id]`, `/workflow cancel [run_id]`,
  `/workflow settings`, `/workflow help` are host answers
  (`crates/tui/src/commands/groups/core/workflow.rs`,
  `crates/engine/src/tools/workflow/mod.rs::{host_workflow_runs,
  host_cancel_workflow}`); `/workflow run <path>` launches a checked-in file
  as-is; `/config workflow` / `/config goal` explain the effective tables.
- The workflow tool reads the session `[workflow]` table for approval and
  admission decisions (`workflow_config_for`). `require_approval_for_writes`
  gates the start card only: YOLO / session auto-approve still bypasses it,
  and writes inside a running VM step stay on the VM runtime contract.
- `/goal help|?|status`, bare `/goal` prints usage on an empty session, plain
  status wording (`Goal active: … · elapsed … · continuations N · not running
  now — send a message or /goal resume to continue`).
- `create_goal` guidance now lets the model infer long-running intent from a
  direct request (DSH policy) and promises a one-line receipt instead of a
  confirmation question; the runtime shows `Goal set: "…" · /goal shows
  progress · /goal pause or /goal clear stops it` when the model (or a
  restored session) introduces a goal.

## Follow-ups (not done here)

- Journal-replay resume for workflows (`/workflow resume <run_id>`): the
  per-event journal already records every task; the missing piece is a
  request-hash cache in the driver so completed leaves return their recorded
  results on re-run (Grok Build `xai-workflow/journal.rs` is the reference).
- A saved-workflow registry (`.codewhale/workflows/*.workflow.js` with
  `meta.name`, project + user scope, trust-checked) so `/workflow run <name>`
  and `/workflow list` work by name; the CLI already resolves `workflows/`.
- Grok Build-style stall handling beyond no-progress pause (strategist
  restructure, next-step generator) — evaluate after the prefix-cache lane,
  since both add prompt sections.
- Small-context local models: the emergency compaction loop (`estimate ~3272
  tokens, budget ~1024` on an 8K Ollama route) and the phase strip showing
  "Context automatically compacting…" for the whole turn were observed but
  are outside this lane's scope (`g0*` captures).
