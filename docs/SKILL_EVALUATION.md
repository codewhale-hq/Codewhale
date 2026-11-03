# Evaluating skill changes

NVIDIA's [NeMo Skills](https://github.com/NVIDIA-NeMo/Skills) primarily supports
synthetic data generation, model training, and model evaluation. The closer
fit for Codewhale is NeMo Platform's
[Agent Optimizer](https://docs.nvidia.com/nemo-platform/latest/documentation/agents/optimize-agents/run-the-agent-optimizer):
its `nemo agents optimize-skills` flow evaluates a baseline, edits a configured
skills directory, reevaluates, and accepts improvements against the baseline.
It requires its platform services, agent/provider configuration, and evaluation
setup; it is not a drop-in Codewhale runtime.

Use that experiment design around Codewhale's existing Engine. Keep provider,
model, tool definitions, permissions, fixtures, and task prompts fixed while
changing one skill. Start with `debug`, `handoff`, and `verify`, which affect
task completion and continuity directly. Preserve the baseline and candidate
skill hashes and all execution receipts.

| Task family | Required behavior | Rejection condition |
| --- | --- | --- |
| Debug a failing command | Reproduce, locate the cause, make a bounded repair, verify the reported failure | Silently skips verification or reports an unrun check as passed |
| Continue a long task | Retain objective, corrections, authorization limits, working files, and running task handles | Repeats completed work, loses a constraint, or asks the user to manage automatic compaction |
| Review a plugin update | Inspect changed content and capabilities; use existing trust and enablement controls | Catalog text or a previous trust decision grants authority to new bytes |
| Verify a release candidate | Tie evidence to the tested source and binary, and distinguish local from provider/CI proof | Claims release readiness from stale or partial evidence |

Measure task success first, then tool errors, unnecessary user interruptions,
input/output tokens, provider-reported cache hits and misses, elapsed time,
and cost at the recorded route price. Preserve the stable system/tool prefix
within a trial; never infer cache hits from prompt length or shared wording.
Reject every candidate that violates authorization, loses user data, or
regresses an existing acceptance case, regardless of token savings.

Separate development tasks from held-out tasks. Select edits using only the
development set, then evaluate the frozen candidate against the untouched
holdout with repeated paired runs. Promote only an improvement that survives
those runs and the normal source gates. Keep the previous version for rollback.

Codewhale's `crates/app-server/src/eval.rs` provides deterministic tool-loop checks;
it does not measure model skill quality. Existing Engine, skill-discovery,
plugin-lifecycle, and compaction survival tests are prerequisite checks. A
real provider comparison is a separate, bounded-cost experiment. No model
quality or cost improvement is established merely by editing these files,
running offline tests, or reading NeMo's documentation.
