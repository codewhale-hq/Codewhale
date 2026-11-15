# Experimental coding evolution (draft)

This opt-in recipe extends `workflows/operate_best_of_n.workflow.js` through
Codewhale's Workflow VM, Fleet and Runtime worktrees. It references issue #7250.
It does not apply a patch, merge a branch or change production orchestration.

## What the host owns

The experimental host freezes a Git baseline, public brief, private gate and
case bundles before dispatch. It admits only bounded child tasks with a fixed
model route and scoped file tools. Linux process gates use bubblewrap with a
read-only candidate tree, a writable scratch mount and no network. The host
compares structured outputs with frozen cases; candidate claims cannot pass a
gate. An unsupported isolation backend refuses evaluation.

Every admissible candidate is sealed as a complete content-addressed tree.
The artifact includes additions, deletions, file modes and blob hashes.
Evaluation and replay materialize from that sealed tree. A later generation
starts from a verified parent artifact through a host-created Git commit.
The requested `worktreeBase` must match the applied-policy receipt before
Fleet dispatch, and the evaluator checks the resulting worktree HEAD.

The shadow genome has five genes: `planning_budget`,
`max_repair_rounds`, `extra_verification`, `candidate_strategy` and
`trace_preference`. The applied-policy receipt identifies their effective
owners. Planning passes, repair admissions, extra DEV review and feedback
format are host-controlled; candidate strategy is a prompt preference.
The genome cannot alter provider, evaluator, sandbox or write authority.
Selection preserves admissible public FAIL repair parents and PASS elites,
using observed public cases and known cost. A failed gate can therefore
ground a later code repair and policy mutation. Policy promotion is always a
proposal that requires a separate new-task evaluation.

## Reviewer order

1. Review the evaluator slice from `07b743c`: frozen identity, admission,
   sandboxed host gate, complete CAS and independent replay.
2. Review `8b450e5` and `f9b9dfc`: inherited genomes, applied-policy
   witnesses, selection and sealed parent code as the next worktree base.
3. Compare original process-gate receipts with the four-arm runner's
   `comparison.json`. Public gate, held-out replay and reviewer are
   separate decisions.

The local handoff archive records the exact build commit and binary digest,
raw provider transcripts, candidate receipts, negative pilots and rerun
commands. The PR body links the latest available public CI run.

## Reproduce the Linux isolation probe

```bash
python3 scripts/experimental-evolution-r2-sandbox-e2e.py \
  --output /absolute/path/to/new-sandbox-evidence
```

This creates a new directory and runs actual bubblewrap child processes
against synthetic host-private markers, local sockets and a delayed child.
Inspect `result.json` for `passed_count` and `case_count`. It checks the
backend independently of a provider request.

## Run real, budgeted comparison arms

Use a compiled CLI from the same source commit and an explicitly authorized
`DEEPSEEK_API_KEY` in the process environment:

```bash
python3 scripts/experimental-evolution-smoke.py \
  --codewhale /absolute/path/to/codewhale \
  --fixture examples/experimental-evolution-jsonpath \
  --output /absolute/path/to/new-evidence-directory \
  --arms evolution independent serial fixed_policy \
  --budget 6 --generations 3
```

The runner refuses to overwrite the output. It creates one frozen task
baseline and fresh per-arm clones. Each arm has a ceiling of six candidate
executions; the serial arm may stop on a task PASS. The provider, model,
brief, case bundles and write scope are shared. Read `run.json`,
`comparison.json`, receipts and original logs for the number of actual
executions, missing cost reports and held-out outcomes. A candidate ceiling
is not a token or currency cap.

For exploratory policy transfer, freeze a selected DEV genome into a JSON
file and run a different task baseline with
`--initial-policy-json /absolute/path/to/policy.json` and
`--transfer-source-evaluation-id ID`. Cross-task workers receive the
policy, never the previous task's code artifact.

## R3 policy panel and restart checks

The receipt selector in `scripts/experimental-evolution-r3-policy-panel.py`
requires two policies on the same two DEV task/replicate cells. It reads the
native evaluator receipts and complete Workflow task ledgers, including
planner, writer and DEV-review costs. A missing receipt makes that cell fail.
The selected genome is then frozen for a fresh task. Its promotion receipt
can be `NOT_QUALIFIED` even when the new task passes; no policy is installed.
Use `scripts/experimental-evolution-r3-policy-panel-e2e.py` to exercise the
selection and provenance contract without a provider request.

A completed nonfinal generation can write a checkpoint and pause. Pass its
`checkpoint-gN.json` as `resumeFrom` in a new Workflow process. Resume checks
the binary, frozen baseline/oracle/policy, prior artifact blobs, selection
history, request events and consumed budget before any new model request.
Unknown work after the checkpoint rejects resume. A prior winner is replayed
from its sealed artifact; new candidate evaluation still needs a completed
current-process Fleet worker. The native
`scripts/experimental-evolution-r3-native-resume-e2e.py` runs G1/G2, exits,
resumes G3 and checks the five identity refusals with a local scripted
provider. The native isolation, CAS, backend and cancellation E2Es in
`scripts/experimental-evolution-r3-native-*-e2e.py` use the same local route.

## Replay without provider or original worker checkout

```bash
python3 scripts/experimental-evolution-r2-offline-replay.py \
  --evaluator-dir /absolute/path/to/evaluator-bundle \
  --cases-dir /absolute/path/to/frozen-case-bundles \
  --candidate-id candidate_g0_1 \
  --output /absolute/path/to/new-replay-directory \
  --include-holdout
```

The replay verifies baseline, artifact manifest, tree and blob hashes, then
runs the same process cases in bubblewrap. The command needs neither network
nor a provider key. Use an existing PASS candidate ID from the named bundle.

## Current limits

The host enforces request, model-turn, candidate, generation, repair and wall
ceilings and records reported usage. It does not enforce a provider token or
currency cap or aggregate scratch/memory quota. Model-reported usage can be
missing. Recovery is limited to completed generation boundaries. A complete
crash-resumable multi-task policy fitness ledger, mid-request settlement and
an untouched task-family promotion panel remain outside this draft. Report
those gaps with any pilot result; do not promote a shadow policy from
task-local held-out cases alone.
