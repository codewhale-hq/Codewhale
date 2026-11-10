# Experimental evolution search

This opt-in prototype extends the checked-in `operate_best_of_n.workflow.js`
recipe. It uses the existing Workflow VM, Fleet task dispatch, Runtime worktrees
and subagent receipts. The original checkout and production settings are not
modified by a run.

## Reviewer path

1. Review the first commit independently: the host freezes an external JSON
   manifest before any worker starts, admits bounded worktree candidates, and
   replays each patch on a fresh worktree from the frozen Git SHA. Public and
   held-out process gates execute from the host. Only a clean winner replay
   can reach read-only review.
2. Review the second commit for orchestration: generation zero has independent
   candidates; generation one chooses parents with observed public-gate
   failures and sends their bounded feedback, patch excerpt and receipt path
   to fresh workers. The two operators are targeted assertion repair and an
   alternative implementation strategy. Duplicate patches are recorded.
3. Review the shadow policy and benchmark separately. The policy only changes
   candidate instructions. The host validates its typed values and limits;
   it cannot change provider identity, evaluator gates, permissions or global
   settings. A selected policy remains a proposal for human review.

## Run one real comparison

From a build of this exact branch, with a valid `DEEPSEEK_API_KEY` exported in
the process environment:

```bash
python3 scripts/experimental-evolution-smoke.py \
  --codewhale /absolute/path/to/codewhale \
  --output /absolute/path/to/new-evidence-directory \
  --budget 4
```

The script refuses to overwrite an existing evidence directory. It creates a
fixed Git baseline of the task in `examples/experimental-evolution-smoke/`,
then fresh clones for the single, independent Best-of-N, and evolutionary
arms. It holds the provider, model, task contract and write roots constant.
Best-of-N and evolution each have a maximum of four worker executions;
evolution uses two workers in each of two generations. The single arm has one.
Run logs, frozen manifests, patches, gate logs, receipts and `comparison.json`
are preserved under the output directory. `heldout_replay_pass` is computed
only from a real replay receipt with a passing held-out gate. Inspect the
per-arm run JSON and receipts for actual execution counts and token/cost usage.

The task's stub is an intentionally failing baseline. Its public gate command
is:

```bash
cd examples/experimental-evolution-smoke/task
python3 ../public_gate.py
```

The second process-level task exercises atomic JSON Patch operations, pointer
decoding, array moves and structured errors. Run it with a separate evidence
directory:

```bash
python3 scripts/experimental-evolution-smoke.py \
  --codewhale /absolute/path/to/codewhale \
  --fixture examples/experimental-evolution-patch \
  --output /absolute/path/to/new-patch-evidence-directory \
  --budget 4
```

Its 38-case public gate and six-case held-out gate are distinct frozen
artifacts. Keep each task's `comparison.json` with its own baseline SHA and
receipts; do not pool runs with different fixture hashes.

The versioned `examples/experimental-evolution-patch-strict` fixture adds a
strict request/operation shape and validates no-op moves against missing
sources. It has 46 public process cases and eight held-out cases. It was
created after the first two smoke tasks showed no first-generation public
failures; use `--fixture examples/experimental-evolution-patch-strict` and a
third output directory. Do not reinterpret the earlier frozen runs under this
revised contract.

The frozen task does not expose the held-out gate to worker prompts. Mutation
uses the public failure excerpt and parent receipt; the held-out gate runs
only for the provisional winner's independent replay.

## Evidence and promotion

Each manifest records baseline SHA, objective and fixture digests, evaluator
and gate artifact digests, provider/model route, allowed and protected paths,
budgets, and the initial shadow policy. Each candidate receipt records the
patch hash, lineage, policy, route, usage when reported, gate command and exit
status, observed test count, and artifact paths. `UNKNOWN` and missing usage
stay distinct from zero. The workflow returns a proposal with `NONE` if no
candidate survives replay and review; it never applies a patch or production
policy. Reviewers can stop after the evaluator commit, revert any experimental
commit independently, and discard task-owned worktrees/evidence after review.

This is an experimental smoke task. The planned 20–30 task paired benchmark
and three-seed uncertainty analysis should be recorded as separate evidence
when run; a single smoke result is reported as one task, one seed.
