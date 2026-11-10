# Workflow Experimental Search

Experimental search is an option for **Workflow**, not a fourth mode and not a
second scheduler. Fleet owns the roster and deterministic member selection.
Runtime owns execution routes, concurrency, leases, and receipts. Workflow owns
the frozen order: independent generation, evaluation, selection, repair, and
verification.

The product vocabulary is Fleet (roster and selected member), Workflow (what
order), Lane (one running Workflow), Runtime (where/how/authority), and Operate
(the mode). Do not introduce
"WhaleFlow" as a current synonym.

## Current foundation

- The imperative Workflow VM admits at most 1,000 tasks per run and 64 live
  tasks at once by default. The host's per-run concurrency gate is a semaphore
  sized from `[workflow] max_concurrent` (default 64) and clamped to
  `WORKFLOW_MAX_CONCURRENT = 128` (`codewhale-workflow-js`); additional
  `task()` spawns block on that gate until a live slot frees, then route
  through fleet. A larger declared population therefore queues at the gate,
  not through fleet itself.
- There is no typed search spec in the runtime today. A Rust prototype
  (`WorkflowSearchSpec`) validated and froze the TOML shape below, but nothing
  consumed it, so it was removed in 0.10.1 (#6517) rather than left to read as
  live infrastructure. The evaluator host described below should reintroduce a
  spec together with its first real caller.
- `operate_best_of_n.workflow.js` supports `strategy: "search"` for 2–16
  structured independent candidates and one read-only reviewer. The stable
  shared instructions precede the candidate-specific suffix to favor provider
  prefix caching.
- `BranchTournament` preserves its historical cost-first default but now
  supports explicit score-first ordering. Pareto selection remains available
  in the typed Workflow core.

## Security and truth boundary

The JS starter does not own the shell or evaluator. It therefore cannot turn a
command mentioned in a prompt into a hidden, runtime-owned gate. Candidate
self-verdicts and claimed commands are untrusted. Until the evaluator host seam
lands, the starter produces generation and review evidence only.

The evaluator host must:

1. freeze a real Git baseline and evaluator before admitting candidates;
2. give every writer its own worktree and the same public evidence;
3. revoke writer authority before injecting hidden tests or scorer details;
4. apply each patch to a clean baseline, reject forbidden/test changes, then
   run hard gates before performance scoring;
5. record commands, exit codes, environment, token/cache/cost usage, artifacts,
   promotion reasons, and failures on top of fleet receipts;
6. replay the provisional winner cleanly and run an independent read-only
   adversarial review; and
7. return `NONE` when all candidates fail and never apply or merge a winner
   without a later explicit user action.

## Provider presets

The abstraction remains provider-neutral. A DeepSeek Flash preset can exploit
its automatically managed prefix cache by keeping shared instructions,
experiment rules, repository evidence, and the response contract stable, with
the candidate id last. Preliminary scouts can use lower effort while promoted
implementers/finalists use high or max effort.

Record both the requested API model id and the resolved provider version.
Provider account concurrency is not Runtime worker concurrency: the runtime keeps its
16-live-worker ceiling, handles 429 responses and keep-alives outside the
deterministic VM, and stops new admissions when the shared budget is exhausted.

## Proposed authoring shape

```toml
name = "speed-up-certificate"
objective = "Reduce runtime without changing exact results"
population = 32
rounds = [32, 8, 3, 1]
concurrency = 16
integration_policy = "review_only"

[worker]
provider = "deepseek"
model = "deepseek-v4-flash"
reasoning_effort = "high"
write_authority = "worktree_write"
write_roots = ["code"]

[budget]
max_cost_microusd = 5000000
max_tokens = 10000000

[hard_gates]
commands = [
  "PYTHONWARNINGS=error python certificate.py",
  "git diff --exit-code -- expected_result.json",
]
forbid_test_changes = true
protected_paths = ["tests", "expected_result.json"]

[score]
command = "./scripts/benchmark_candidate.sh"
direction = "minimize"
metric = "median_runtime_ms"
trials = 5
tie_breakers = ["diff_lines", "cost_microusd"]

[selection]
policy = "pareto"
retain_diversity = true
```

This shape is a design proposal: nothing parses it today and it is not a
runnable CLI promise. The next runtime slice is the evaluator host and
aggregate receipt, which must freeze the baseline, requested and resolved
model ids, public evidence hash, evaluator hash, and the complete spec before
admitting candidates; after that, the natural-language authoring layer can
safely compile a user's request into this shape.
