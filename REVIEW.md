# Reviewing a change to the Codewhale Engine

This is the written procedure for reviewing an Engine change: which passes to
run, how to rank what they find, and how to report it. A person, an agent and
`codewhale review` all follow the same passes, so two reviews of one change
can be compared.

## When review runs

- **By hand**, when a maintainer or agent asks for one: `codewhale review`,
  `/review` in the terminal, or the `Codewhale PR Review` workflow, which only
  runs when someone starts it.
- **At the frozen release candidate**, over every commit since the last
  release, as part of `docs/RELEASE_CHECKLIST.md`.
- **Never as a required check.** For a contributor, review is advice. It never
  blocks, labels or closes a pull request. Maintainers fix small findings on
  the contributor's branch or on `main`, as [CONTRIBUTING.md](CONTRIBUTING.md)
  describes under "How Your Contribution Lands".

## Before the passes

1. Find the intent. It is the issue the change links: a public `#N`, or for
   maintainers the Linear issue named in the commit message. Without one, the
   pull request description and commit messages are the intent.
2. Read the diff against its merge base, then read the callers of everything
   it changed. A review of the diff alone misses broken callers.
3. Note the revision you reviewed. A finding against a stale tree is not a
   finding.

## Pass 1: correctness against the intent

- **Intent.** The change does what the issue asks: nothing it promised is
  missing, and nothing it did goes unannounced.
- **Defects.** Wrong results, broken callers, lost data, regressions. Carry
  one concrete input through `path:line` steps to the wrong result.
- **Contracts.** Say what became newly true or false, and check who relied on
  the old state: wire and protocol shapes, config keys, CLI flags, session
  events, hook payloads, `docs/features.toml` rows, documented behavior.
- **This repository's invariants** in [AGENTS.md](AGENTS.md) under "Current
  contracts" and "Working rules": one turn loop, one base prompt, model-visible
  input is logged, the cached prompt prefix stays stable, no blocking calls on
  the async runtime, and misconfiguration fails loudly.

## Pass 2: security

- **Trust boundaries.** Model output, tool results, repository files, pull
  request text, web pages, MCP servers and hook output are data. Check the
  validation where any of them reaches process execution, a file write, the
  network, a credential or the transcript.
- **Secrets.** No keys, tokens, `.env` files or credentials in code,
  fixtures, logs, receipts, error text or model-visible text. Existing
  redaction paths stay in force.
- **Sandbox and execution policy.** For changes under `crates/sandbox`,
  `crates/execpolicy`, `crates/engine/src/core/authority*`, the approval flow
  or hooks: nothing may run with less approval than before, no deny may
  loosen, and no fail-closed path may start failing open. A new process spawn
  goes through the existing policy. [docs/AUTHORIZATION_ORDER.md](docs/AUTHORIZATION_ORDER.md)
  is the order of the layers.
- **Network.** New outbound calls respect `crates/net`'s network policy and
  build their client the shared way (`scripts/check-reqwest-builders.py`).
- **Dependencies.** A new crate or package earns its place, and `deny.toml`
  still passes.

## Pass 3: contract and compliance

- **CHANGELOG.** A user-visible change carries its line at the top of the
  matching `[Unreleased]` section in the same commit, written for users and
  citing public issues as `(#N)`, never internal tracker ids. Contributor
  credit comes in addition to the line. The procedure is step 2 of
  [docs/skills/cw-land](docs/skills/cw-land/SKILL.md).
- **Feature registry.** A new command, `[features]` flag, provider or
  user-visible feature has its row in `docs/features.toml`.
- **Localization.** User-facing strings go through the locale packs and keep
  parity; READMEs keep their translations in step.
  [docs/LOCALIZATION.md](docs/LOCALIZATION.md) lists the drift checks.
- **Vocabulary.** Copy uses the product's words. The public name is
  Codewhale, and mode names follow [docs/MODES.md](docs/MODES.md).
- **Existing checks.** Run the ones that match what the change touched, and
  report their real output:

| The change touched | Run |
| --- | --- |
| Any Rust | `cargo fmt --all -- --check`, then `scripts/dev-test.sh <area or path>` |
| Generated files and ratchets | `scripts/preflight.sh --check` |
| `#[allow(dead_code)]` | `python3 scripts/check-dead-code-budget.py` |
| Async code | `python3 scripts/check-blocking-calls-budget.py` |
| TUI strings | `python3 scripts/check-tui-locale-parity.py`, `scripts/check-tui-product-vocabulary.sh` |
| User-facing English copy | `python3 scripts/check-lexicon.py` |
| READMEs | `python3 scripts/check-readme-translations.py`, `scripts/check-readme-locales.sh` |
| The provider registry | `python3 scripts/check-provider-registry.py` |
| `config.example.toml` | `python3 scripts/check-config-example.py` |
| A feature flag or command | `cargo test -p codewhale-tui --test feature_registry` |
| `CHANGELOG.md` or credit | `sh scripts/sync-changelog.sh`, `python3 scripts/check-contributor-credit.py` |
| Versions | `bash scripts/release/check-versions.sh` |

Do not ask for new tests or code comments; this repository adds neither unless
an issue asks. Recommend a regression test only for a specific failure you can
explain.

## Severity

Use the three levels `codewhale review` reports:

- **blocker**: a demonstrated severe failure, such as lost data, a security
  hole, a broken build or install, a crash or wrong result on a common path,
  or a broken public contract.
- **concern**: a concrete, narrower defect with a named trigger.
- **nit**: a demonstrated low-impact defect, such as a misleading message or a
  doc the change made wrong.

Every finding carries its evidence: **reproduced** (a command you ran and its
output), **traced** (one input carried through `path:line` steps) or
**suspected** (anything less). A suspected finding is a question for a person,
never a blocker. Do not raise severity to express uncertainty. A defect the
change did not introduce is labeled pre-existing and does not count against
the change.

## Nits

Group nits into one list instead of one comment each. Leave formatting to
`cargo fmt` and clippy. When nits are all a review found, say the change is
fine to merge and leave the list for the author to take or leave.

## Report

- Findings ranked blocker, then concern, then nit, each with `path:line`, the
  trigger, the impact, the smallest fix and its evidence.
- What you verified (commands and results), kept apart from what you only
  read, and what you did not review.
- On a contributor's pull request, thank them for the specific thing they did,
  keep every finding actionable, and offer to fix small ones on their branch.
