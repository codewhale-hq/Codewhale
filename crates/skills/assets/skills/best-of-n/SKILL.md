---
name: best-of-n
description: Generate a small set of independent candidate solutions in worktrees, judge them against one explicit rubric, and apply the winner only after PASS verification.
metadata:
  short-description: Compare independent candidates
---

# Best of N

Use this skill when a consequential design, implementation, explanation, or
debugging task has several plausible solutions and comparison is worth the
extra model work. In Operate mode this is the preferred ensemble pattern for
high-stakes or ambiguous approaches. Do not use it for a tiny change or when
the user has already chosen the approach.

## Set The Tournament

1. Define one task, one evidence packet, and one explicit scoring rubric before
   launching candidates. Include correctness, fit to the request, simplicity,
   risk, and verification.
2. Choose `N` from 2 to 4 for a quick comparison (default 3). For an explicit
   experimental search, use the Workflow search option: 2–16 live candidates,
   with larger validated populations queued at the Workflow host's 16-worker
   concurrency gate rather than launched at once.
3. Give every candidate the same task and rubric. Add only a candidate number;
   do not steer candidates toward different conclusions unless diversity is an
   explicit part of the request.
4. Prefer a session goal (`create_goal` or active `/goal`) when the tournament
   spans more than one parent turn.

## Generate Independently

Start the candidates as parallel background `agent` workers and return agent_ids
immediately so the parent stays free. For proposals, reviews, or research, keep
them read-only:

```json
{
  "action": "start",
  "name": "candidate_1",
  "prompt": "Produce candidate 1 for the task below. Return the proposal, evidence, risks, and rubric self-score. Do not edit files.\n\n<TASK AND RUBRIC>",
  "type": "worker",
  "model_strength": "same",
  "write_authority": "read_only"
}
```

Launch the remaining candidates with the same contract, then use `agent` wait
or completion events to collect every result. Do not show one candidate another
candidate's answer before generation finishes.

When candidates must implement code, give each one:

- `type: "builder"`
- `worktree: true`
- `write_authority: "worktree_write"`
- the same bounded `write_roots` or `exact_files`

Never run parallel writers in the parent checkout. Each builder must return
the structured candidate contract (candidate id, hypothesis, paths, commands,
self-verdict, risks, and artifact references). A self-verdict is evidence to
inspect, not a hard-gate result.

Optional diversity: pin different `model` / Fleet `fleet_profile` values when
the project has multiple capable routes; otherwise keep model strength `same`.

## Judge Once

Use one read-only reviewer worker, or the parent when the result is small, to
score all candidates against the original rubric. The judge must:

- cite evidence from each candidate rather than vote by style;
- reject candidates that violate authority, scope, or verification gates;
- treat candidate-reported commands and PASS claims as untrusted until replay;
- name the winner and the decisive reasons;
- identify useful pieces worth combining, if any;
- say when the candidates are tied or all fail.

Do not ask candidates to vote for themselves. Do not silently merge incompatible
approaches into a new unreviewed solution.

## Integrate Only After PASS

For proposal-only work, return the winning answer with a compact score summary.
For code work:

1. Freeze the baseline, evaluator, hard gates, score rule, and authority before
   a larger search admits candidates. Any evaluator change starts a revision.
2. After a worker loses write authority, apply its patch to a clean baseline
   and let the runtime—not that worker—run hard gates and scoring.
3. Inspect the winning worktree diff and independently replay it on the clean
   baseline. A different read-only model may look for gaming, but deterministic
   tests remain the authority.
4. Present the verified winner for review. Applying or merging is a separate,
   explicit user action; `NONE` is valid when every candidate fails.
5. Preserve losing and failed candidate receipts as useful negative results.

The checked-in `operate_best_of_n.workflow.js` recipe supports
`strategy: "search"` for structured 2–16 candidate generation and review. It
does **not** yet turn prompt-listed commands into hidden runtime gates. Do not
advertise those gates until a runtime evaluator host consumes a frozen search
spec.

Stop early when one candidate reveals a hard constraint that invalidates the
tournament. Report the negative result rather than spending the remaining
budget to manufacture variety.
