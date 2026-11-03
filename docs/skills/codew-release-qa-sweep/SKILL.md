---
name: codew-release-qa-sweep
description: "Use before claiming Codewhale release work is done: a pointer to docs/RELEASE_CHECKLIST.md for the gates and to cw-dogfood for the manual QA targets."
---

# Codewhale Release QA Sweep

No sweep, no "done". This skill owns nothing itself; it points at the two places
that do.

- **Gates and the release cut:** follow `docs/RELEASE_CHECKLIST.md` from the
  canonical checkout on `main` (`docs/RELEASE_RUNBOOK.md` for the tools behind
  it). Native builds go through `scripts/dev-cargo.sh` / `scripts/dev-test.sh`
  ([cw-gates](../cw-gates/SKILL.md)). Hosted CI runs once, at the final release
  step, against the frozen candidate commit.
- **Manual QA targets:** [cw-dogfood](../cw-dogfood/SKILL.md) step 5. Unit and
  build gates do not cover the live TUI.

## Regressions the manual QA must cover

Exercise each in a real terminal and record what you saw. The scenarios are
described in cw-dogfood; the refs are the reports that motivated them.

- Six-worker fanout liveness, including Esc mid-fanout: #3216, #2211, and the
  Windows Terminal retest path #3289.
- Multi-terminal route isolation: #3227.
- Running-turn input contract: #3203.

## Reporting

Each command, pass or fail, and the salient output line (test counts, the
`--version` string, the `check-versions.sh` verdict). For manual QA, what you
observed per target, citing the ref. Name what you skipped (for example, no
display for TUI QA) rather than implying coverage you do not have.

## Red flags / don't

- Don't claim "done", "passing" or "merge-ready" without command output.
- Don't skip the manual targets because the build is green.
- Don't tag, publish, create a GitHub Release, push artifacts or merge or close
  a PR or issue without approval. A green sweep is readiness evidence, not
  permission.
- Credit mechanics for contributor work: [cw-land](../cw-land/SKILL.md) step 3.
