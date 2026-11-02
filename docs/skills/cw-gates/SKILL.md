---
name: cw-gates
description: "Use before claiming any Codewhale change is done, green, or ready to land: the focused-to-broad verification ladder, the budget checks CI enforces, and the rules for what counts as a passing test."
---

# cw-gates

Pick the smallest evidence that answers the actual risk, then quote the real
output. This repo has been burned twice by the alternative: an exit code
mistaken for a pass, and a harness whose scoring line silently reported
unevaluated rows as green. Assertions without command output are not evidence.

Stage 3 of the loop: [cw-orient](../cw-orient/SKILL.md) →
[cw-slice](../cw-slice/SKILL.md) → **gates** →
[cw-dogfood](../cw-dogfood/SKILL.md) → [cw-land](../cw-land/SKILL.md) →
[cw-handoff](../cw-handoff/SKILL.md).

## When to use

- Before saying "done", "green", "passing", "fixed", or "ready to land".
- Before a dogfood build — never install an ungated binary.
- When asked to "run the gates" or to prove a change is safe.

For release work, follow `docs/RELEASE_CHECKLIST.md`; it adds the
version-drift gate and the exhaustive release validation on top of this ladder,
and [cw-dogfood](../cw-dogfood/SKILL.md) owns the manual TUI QA targets.

## Workflow

Climb only as far as the risk requires. Say where you stopped and what you
skipped.

Native Rust builds go through `scripts/dev-test.sh` or `scripts/dev-cargo.sh`,
which take the machine's build slot (`scripts/build-lock.py`; at most two
native builds per machine). Raw `cargo` bypasses the lock.

### Rung 1 — always, and cheap

```bash
cargo fmt --all -- --check
git diff --check
```

### Rung 2 — the area that owns the change

`scripts/dev-test.sh` maps an area or a source path to the fastest correct
invocation, and applies the isolated build-dir topology
(`docs/BUILD_PERFORMANCE.md`):

```bash
scripts/dev-test.sh --list
scripts/dev-test.sh crates/command-contract/src/elapsed.rs      # path → area + filter
scripts/dev-test.sh tui tools::                    # area + filter
scripts/dev-test.sh config
```

It uses `cargo nextest run` when nextest is on PATH (`.config/nextest.toml`);
`CODEWHALE_DEV_NEXTEST=0` forces libtest. For the TUI crate, `--lib` and
`--tests` are disjoint — choose the target that owns the behavior rather than
running both by reflex.

`scripts/dev-cargo.sh test --no-run` answers a compile question without executing unrelated
cases. `cargo test --doc` covers doc examples, and is only worth running when
those examples changed.

CI builds with `-D warnings`, so a deleted last caller (an unused import, a
variant or helper nothing constructs) is a red `main` even though a plain
build only warns. After removing or rewiring a call site, check the library
the way CI does before you push:

```bash
RUSTFLAGS="-D warnings" scripts/dev-cargo.sh check -p <crate> --lib --locked
```

Changing `RUSTFLAGS` rebuilds every dependency, so keep that value for the
whole session. The test-only half of an unused item is invisible to this
check; use `--tests` as well when you touched helpers that tests still call.

### Rung 3 — the budgets and drift checks CI enforces

Run the ones your change can move. Each fails the build in CI:

```bash
python3 scripts/check-dead-code-budget.py          # #[allow(dead_code)] ceiling
python3 scripts/check-runtime-contract-budget.py
python3 scripts/check-persistence-backlog-budget.py
python3 scripts/check-provider-registry.py         # provider registry drift
python3 scripts/check-command-crate-boundaries.py  # command-contract boundary
python3 scripts/check-command-migration-manifest.py
python3 scripts/check-tui-locale-parity.py         # touched crates/localization/locales/
sh scripts/check-tui-product-vocabulary.sh
python3 scripts/check-readme-translations.py       # touched README*.md
./scripts/release/check-versions.sh                # touched a version anywhere
```

The dead-code budget may go **down** freely; raising it needs a reviewer to be
told why. Lock in a win with `python3 scripts/check-dead-code-budget.py --update`.

### Rung 4 — cross-cutting or release risk only

```bash
scripts/dev-cargo.sh clippy --workspace --all-targets --all-features --locked -- \
  -D warnings \
  -A clippy::uninlined_format_args \
  -A clippy::too_many_arguments \
  -A clippy::unnecessary_map_or
scripts/dev-cargo.sh nextest run --workspace --all-features --locked --profile ci
scripts/dev-cargo.sh test --workspace --all-features --locked --doc
git diff --exit-code -- Cargo.lock                 # lockfile drift guard
```

`--all-targets` matters: without it, clippy never lints test code. (CI runs
the all-targets form since the v0.9.10 gate incident; run the same form
locally so there is no weaker subset.)

### Rung 5 — website, when `web/` changed

```bash
npm test && npm run check:web
```

## Claiming a test passed

`AGENTS.md` ("Claiming a test passed") owns the rules: quote the real count
line with `N > 0` for the tests that cover the change, prefer a regression
test that fails without the fix, and audit hand-rolled scorers. Two additions
for gate runs:

- Check the count **per filter group**. A green run where one required filter
  matched zero tests is a miss, not a pass.
- A focused rerun of a failing test distinguishes flake from regression in
  seconds. Do that before calling anything a flake, and root-cause anything that
  fails outside a known-flaky name — check for unisolated config-path reads or
  global timeout knobs first.

## Red flags / don't

- Don't say "tests pass" without the count line. Don't say "CI will catch it".
- Don't run the full workspace suite as ritual for a leaf change, and don't
  re-run an unchanged suite to feel more confident.
- Don't weaken a safety or data-integrity behavior to make a gate go green.
- Don't call a failure a flake without a focused rerun and a named cause.
- Don't skip the budget checks because they are "not really tests" — they are
  required CI contexts, and they encode migrations this repo has already paid for.
- Don't report a green gate as permission. A passing sweep is readiness
  evidence; landing, tagging, and publishing need their own approval.

## Output

A checklist: each command, pass/fail, and the salient line (test counts, budget
numbers, the `check-versions.sh` verdict). Name explicitly what you did **not**
run and why. If a step could not run in this environment, say so rather than
implying coverage you do not have.
