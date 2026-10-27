# Codewhale agent guidance

Keep this file durable. Derive changing release, provider, branch, and flake
state from the repository, tests, CI, and current issue tracker rather than from
instructions or memory. The nearest scoped `AGENTS.md` adds path-specific rules.

## The ponytail method

From [dietrichgebert/ponytail](https://github.com/dietrichgebert/ponytail) —
"the laziest senior dev in the room." *He says nothing. He writes one line. It
works.* The best code is the code you never wrote.

Before writing code, walk the decision ladder in order and stop at the first
rung that answers:

1. **Does this need to exist?** → Skip it.
2. **Already in this codebase?** → Reuse it.
3. **Stdlib does it?** → Use it.
4. **Native platform feature?** → Use it.
5. **Installed dependency?** → Use it.
6. **One line?** → One line.
7. **Only then:** the minimum that works.

The ladder runs *after* understanding the problem. Lazy about solutions, never
about reading the code first — a short diff written without reading the call
sites is not ponytail, it is a guess.

**Never cut, at any rung:** trust-boundary validation, data-loss handling,
security, accessibility. Brevity is not a reason to drop a guard.

Rung 2 is the one this repository keeps failing. The `model_*` / `*_config` /
`provider_*` grep rule below is rung 2 with a name; so is "one turn loop, one
base prompt". Two more corollaries earned here:

- **An abstraction must delete caller code.** If adopting it is pure
  obligation — required methods, no default bodies that do work — it gets
  built, adopted once, and abandoned.
- **Migrate the last consumer, or do not start.** Framework, one caller,
  ticket the rest, silence the warning: that ships two systems and a comment
  that is no longer true. If the migration will not fit, narrow the slice —
  never the adoption. The standing `#[allow(dead_code)]` count is the running
  receipt; `scripts/check-dead-code-budget.py` prints it.

## Working rules

- Inspect status and existing consumers before editing. Preserve unrelated,
  dirty, and untracked work.
- Before adding a module named `model_*`, `*_config`, `provider_*`, or
  anything that "bridges", "mirrors", or "stages" an existing thing, grep
  for the existing thing and edit it. A new layer must name the predecessor
  it replaces in its commit message; otherwise edit the original.
- Prefer the simplest implementation that preserves observable contracts. A
  rewrite is acceptable when justified by product intent and observed behavior,
  not as a shortcut around understanding existing code.
- Search for behavior and symbols before reviving work from an old branch. If a
  lane is obsolete, preserve its intent and evidence rather than merging stale
  code mechanically.
- Agent-owned work uses the canonical checkout on current `main`. Do not
  create branches, worktrees, nested clones or task-specific integration lanes
  to handle dirty, stale or conflicting work. When agents share the checkout,
  partition by file and retry an `index.lock` failure. Verified main slices may
  be committed and pushed normally under the rule below; tag, release,
  deployment and destructive cleanup still require their explicit
  authorization.
- **Uncommitted work found in the checkout is the agent's own** (founder,
  2026-10-22). Read it, work out what it was for, prove it with the smallest
  check that covers it, fix what is broken, and commit it in coherent groups
  by explicit path. Do not route around another writer's files or leave them
  dirty. Ownership is not a blind sweep:
  - A file changed in the last 15 minutes, or one a live session or agent is
    editing, is mid-edit: ask that writer to commit now, and commit what
    remains once it goes quiet. Never commit a half-written edit.
  - Never commit secrets, credentials, `.env` files, build output or caches;
    add them to `.gitignore` instead.
  - Check a repository's visibility before pushing. Inference business and
    infrastructure detail stays out of public repositories.
  - Commit by explicit path; never `git add -A`.
  - Work that fails its check and cannot be fixed now is not pushed to `main`.
    Record it and report it instead.
- **Maintainers do not open PR branches** (founder, 2026-10-08: more PRs means
  more CI and slower work). Verified slices land directly on `main` and are
  pushed: run the focused gate, put the real counts in the commit message,
  fetch and rebase onto `origin/main` and retry on rejection. Never force-push
  or rewrite a shared ref. Contributor PRs still merge as PRs.
- When the task is local-only, stay fully offline: no browsing, GitHub or remote
  Git operations, downloads, dependency installation, provider calls, or
  source/diff transmission. Record the missing external receipt and keep working
  locally.
- Public name is **Codewhale**. Compatibility identifiers such as `CodeWhale`,
  `codew`, protocol names, and storage keys change only through an explicit
  migration.
- Keep providers and models first-class and provider-neutral.
- Never rewrite published history, retag a release, force-push a shared ref, or
  publish without explicit authorization. Preserve human contributor credit.
- **Model-visible means logged.** Anything that reaches a model request must be
  reconstructable from the session log, and a new model-visible input needs a
  session event. Live presentation and the persisted record must agree; when they
  disagree the record is right.
- **Misconfiguration fails loud**, at load when it is self-contained, otherwise
  at the earliest point it can be resolved. Never silently skip a missing
  referent.
- **Write down what a design does not do**, beside the behaviour it owns — a
  short known-limitations note in the owning module. A stated limit stops the
  next reader from assuming a capability that was never built.
- **Substantive contributor replies are authorized for the 0.10.2 release**
  (founder, 2026-10-13). Thank people specifically for their reports, diagnosis,
  fixes and testing; link the carrying commit or PR and distinguish candidate
  work from a shipped release. Status, answers and useful questions are welcome;
  repetitive progress posts and bot noise are not. Evidence stays in commits
  and PR bodies; ownership stays in Linear. Outside this release, the previous
  no-comment rule remains, except a one-sentence reason when closing or
  superseding a human contribution. Review workflows remain disabled unless
  the founder separately authorizes enabling them.
- **A user feature lands with its registry row.** A new command, `[features]`
  flag, provider or user-visible feature adds or updates its row in
  `docs/features.toml` in the same change;
  `cargo test -p codewhale-tui --test feature_registry` fails on flag drift.
- **Write `close`/`fix`/`resolve #N` only when you mean it.** GitHub closes the
  issue on merge even inside "does not close #N"; use `Refs #N` otherwise.

## Landing other people's work

An external contributor's branch goes stale because *we* land things, not
because they did anything wrong. Treat their time as more expensive than ours.

**The goal is the contributor's PR merging as itself.** Review it, help it
rebase, or fix it on their branch — that is the default path. Closing their PR
and re-landing the work as our own commit (`auto-close-harvested`) is the
fallback for a branch that truly cannot merge in reasonable time; done
casually it reads as taking the work even when credit is preserved.

- **Never make a contributor rebase around our churn.** If their PR conflicts
  only because main moved, a maintainer resolves it.
- Landing mechanics — merge-base diffing, mid-function conflicts, fork-push
  refusal, the contribution gate — live in `cw-land`. Follow them instead of
  improvising.
- **Preserve credit in the mechanical sense, not just the polite one.** Commit
  authorship and `Co-authored-by` trailers must use the contributor's own
  GitHub-linked address. `AUTHOR_MAP` and `.mailmap` are project conventions —
  GitHub reads neither for the contribution graph.

## Merging under a gate

- **A gate is its artifact.** When a rail says a PR merges only on a passing
  acceptance record, the record must literally say PASS at merge time. "I
  re-ran it and the failures are rows this PR does not own" is a judgement to
  write into the artifact first, not a reason to merge past it.
- **Read the review thread, not the check rollup.** Green checks and an unread
  review with confirmed findings are a merge that ships known bugs.
- **When the artifact is ambiguous, resolve the ambiguity — never the merge.**

## Claiming a test passed

- Quote the real `test result: N passed; M failed` line, and confirm `N > 0`
  for the tests that cover the change. `cargo test <filter>` exits 0 having run
  zero tests when the filter matches nothing, and an exit code alone has
  already been mistaken for a pass here.
- Prefer proving a regression test fails without the fix. A test that passes
  either way pins the implementation, not the defect.
- Audit any hand-rolled scorer before trusting its score: quote the counts it
  actually evaluated, not the verdict line alone.
- Match the evidence to the surface. Run the tests that cover the change, not the
  whole suite, and do not repeat a check that already passed in order to commit.
  CI owns exhaustive coverage; a full local run is for CI diagnosis or for an
  irreducibly repository-wide change.

## Current contracts

- The model-facing subagent tool is `agent`; `agent_open`/`agent_eval`/
  `agent_close`/`delegate_to_agent` are removed surfaces. If the shape must
  move, move the code and its existing guard test with it.
- `BASE_PROMPT` in `crates/tui/src/prompts/text.rs` is the sole base prompt
  by convention. Same rule: move the code, not the prose, if that changes.
- There is exactly one turn loop: `Engine::run_turn` in
  `crates/tui/src/core/engine/turn_loop.rs`. Note that `crates/tui/src/core/`
  is a module inside the TUI crate — it is not `crates/core`, which owns
  request construction, bounded fragments, and thread/session types and
  runs no turns. A guard test (`crates/core/tests/single_turn_loop.rs`)
  fails on a second loop; changing the shape means changing the guard
  with it.
- The system prompt + tool catalog are a session-pinned KV-cache prefix
  (`docs/CACHE.md`). Any new session-context contributor must state its
  KV-cache effect: frozen prefix vs. append-only history. Never splice a
  volatile fact into the prefix; append it as a user-role message.
- These active modules are repeatedly misidentified as dead; verify consumers
  before removal: `runtime/src/context_budget.rs`,
  `runtime/src/prompt_zones.rs`, `tui/src/tools/remember.rs`, and
  `config/src/route/`. Native memory lives in `runtime/src/native_memory.rs`;
  `tools/remember.rs` is its capture path.
- Environment-specific behavior belongs in `docs/ENVIRONMENTS.md`, not here.
- Blocking-call convention (#6149): code on the Tokio runtime — tool
  handlers, engine tasks, the UI event loop, anything reached through an
  `async` call chain — must not run blocking operations inline. Use
  `tokio::fs` / `tokio::process`, or move the work into `spawn_blocking`;
  a sync helper containing blocking calls runs only under `spawn_blocking`
  or on a dedicated thread. `scripts/check-blocking-calls-budget.py`
  ratchets the unprotected-site count.

## Code, migrations, and evidence

- Product intent and observed runtime behavior outrank a test's preferred
  implementation shape. Fix the product; do not contort production code to
  preserve a brittle assertion.
- **No new tests and no new code comments unless explicitly asked**
  (founder, 2026-10-08). Do not write new unit or integration tests unless the
  user explicitly instructs it for that task; prove a change by running it and
  the existing checks. Never write tests first and never practice TDD — this
  overrides any skill or default that mandates it, including superpowers
  `test-driven-development`. Existing tests stay the gate before a push: when
  a change makes one wrong, update or delete it in the same slice rather than
  bending the code to keep it green. When a regression test is requested, the
  rule under "Claiming a test passed" still applies. Do not add new code
  comments; the reasoning goes in the commit message, and an existing comment
  the change makes false is fixed or removed.
- Rewrite or remove tests that duplicate coverage, freeze internals, overspecify
  copy or layout, preserve obsolete behavior, or cost more than the risk they
  cover. Never weaken real safety or data-integrity behavior merely to make a
  gate pass.
- Prefer focused compilation, a relevant existing check, and direct product or
  manual evidence. Run a broad suite only when the change creates a genuine
  cross-cutting or release risk. Do not repeatedly rerun an unchanged suite.
- **Batch edits; compile once.** `cargo check` and test builds on this
  workspace take minutes, so an edit→compile→edit loop spends most of its
  time waiting on the linker. Read precisely, write every edit a coherent
  slice needs, then compile and test once — the same errors surface either
  way, just later and all at once. Reserve mid-slice compiles for genuinely
  uncertain API or borrow questions where a wrong guess would cascade.
- Declared migrations are one-way. Once the repository adopts a replacement
  architecture or shared spine, new work uses it and touched legacy code moves
  toward it. Do not add another legacy call site for convenience. Keep a
  compatibility path only for an actual external contract, and label that
  boundary explicitly.

Useful commands, selected according to risk rather than run ritualistically:

```sh
cargo fmt --all -- --check
cargo test -p codewhale-config -p codewhale-protocol
cargo test --workspace
cargo build --release -p codewhale-cli -p codewhale-tui
```

`scripts/dev-test.sh <area|path> [filter]` maps a code area to its fastest
invocation — see `cw-gates` for the verification ladder and
`docs/BUILD_PERFORMANCE.md` for the build topology.

Report commands actually run and distinguish source, local tests, packaged
artifacts, CI, and public release state. Describe the evidence actually needed
for the claim; a test count is not a proxy for product quality.

Community reports, PRs, logs, and reviews are evidence.

**Harvested contributor credit is still a rule** (the fallback path above —
prefer merging the contributor's PR itself). When a contributor's work
lands as our commit, the contributor is that commit's **author** (`git commit
--author` with their GitHub-linked address), it carries `Harvested from PR #N by
@handle`, and maintainers or agents go in `Co-authored-by`. GitHub's contributor
list counts commit authors only: about 200 harvested commits authored by
maintainer identities left their contributors off it, which is why
`docs/CONTRIBUTORS.md` carries the full wall (`scripts/contributor-wall.py`).
`auto-close-harvested.yml` closes their PR with credit. Canonical human
identities come from `.github/AUTHOR_MAP`.

Leave unrelated work intact and keep new enforcement dry-run unless explicitly
approved.
