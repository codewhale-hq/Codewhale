# Contributing to Codewhale

Codewhale is what it is because of the people who used it, reported what broke,
translated it, added their providers and sent fixes. Thank you for being here.
Everyone is welcome, in English or 中文, with or without code.

All feedback is a gift to this project, including "this looked wrong" or "I got
lost here" with no fix attached. The [roadmap](docs/ROADMAP.md) says where
Codewhale is going and where help counts most.

## Where things go

| You have | Best place |
| --- | --- |
| A bug, a crash, or behavior that surprised you | [Bug report](https://github.com/codewhale-hq/CodeWhale/issues/new?template=bug_report.yml) |
| An idea or a feature you want | [Feature request](https://github.com/codewhale-hq/CodeWhale/issues/new?template=feature_request.yml), or [Ideas](https://github.com/codewhale-hq/CodeWhale/discussions/categories/ideas) to talk it through first |
| Feedback on how something looks, reads or feels | [Feedback](https://github.com/codewhale-hq/CodeWhale/issues/new?template=feedback.yml) |
| A question | [Q&A](https://github.com/codewhale-hq/CodeWhale/discussions/categories/q-a) or [Discord](https://discord.gg/37gfS3ksug) |
| A security problem | A [private advisory](https://github.com/codewhale-hq/CodeWhale/security/advisories/new), never a public issue ([policy](.github/SECURITY.md)) |
| A fix for an open issue | A pull request that links the issue |
| A translation, a provider or model, a docs fix | A pull request, no issue needed ([standing projects](#standing-projects)) |
| A new feature, already written or not | An issue first ([why](#new-features-start-as-an-issue)) |

If you are not sure, open an issue. Nothing is filed in the wrong place for
long: a maintainer moves it.

## New features start as an issue

Issues are the best place for feature requests, ideas and bugs. A pull request
is the best place for work we have already agreed on.

A feature that first appears as a pull request takes longer to land than one
that starts as an issue, even when we want it. The questions that decide a
feature are about the product, not the diff: does it belong in the engine or in
a plugin, what does it do in each permission mode, how does it show up in
receipts, what is it called in 19 languages. Answering those in a review means
rewriting code you already finished. Answering them in an issue costs you a few
sentences.

So for a new feature:

1. Open a [feature request](https://github.com/codewhale-hq/CodeWhale/issues/new?template=feature_request.yml).
   Say what you are trying to do and what gets in the way. A proposed design is
   welcome and optional.
2. Tick "I would like to open a pull request" if you want to build it.
3. A maintainer answers with the shape that fits, or with what is still
   undecided. Then the pull request is small and merges quickly.

A feature pull request that arrives without an issue is still read, and it is
never closed for that reason. We will most likely ask the same questions there
and it will take longer.

## Standing projects

These are always open, and a pull request needs no issue first:

- **Translations.** You can improve the terminal language packs, the translated
  READMEs in `docs/i18n/`, the website, and the translated docs in
  `docs/zh_hans/` and `docs/id/`. The [localization matrix](docs/LOCALIZATION.md)
  shows what each language has and how to add one, and the
  [Chinese localization group](https://github.com/codewhale-hq/CodeWhale/issues/6804)
  is recruiting.
- **Providers and models.** You can add a missing provider or a new model, or
  correct a price, a context size or a provider quirk.
  [Providers](docs/PROVIDERS.md) lists what ships, and
  `python3 scripts/check-provider-registry.py` checks your change.
- **Documentation.** You can fix a typo, an unclear step, a missing example or
  an instruction that no longer matches the product.
- **Bug fixes.** If there is no issue yet, describe the bug in the pull request
  by saying what you ran, what happened and what you expected.
- **Planned work.** You can take anything in a
  [milestone](docs/ROADMAP.md#releases) or anything labelled
  [help wanted](https://github.com/codewhale-hq/CodeWhale/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22).
  Comment on the issue to claim it so two people do not build the same thing.

Large changes to planned work are easier in layers; see
[Layered and EPIC-Sized Work](#layered-and-epic-sized-work).

### What a planned-work issue gives you

Every issue a maintainer opens for planned work, and every release tracking issue, has the same parts, so you can pick one up without asking first:

- **Status:** what is on `main` already, with commit links, and what is still missing.
- **Ways to help:** each piece open to contributors, with the files to read, the command to test it locally (usually `scripts/dev-test.sh <crate> <filter>`) and what "done" means. Pieces that need no Rust, such as trying a feature and reporting exactly what happened, are listed first. A precise report is credited like code.
- **Being built by maintainers right now:** the parts someone already holds, so you don't build the same thing twice.
- **How to take a piece:** comment to claim it, then open a pull request that says `Refs #<issue>`.

If an issue for planned work is missing one of these, say so on it. That is a bug in the issue.

## How we work with you

- We reply by naming what you reported, diagnosed or fixed.
- Your pull request merges as itself, with you as the author
  ([details](#how-your-contribution-lands)).
- You do not rebase around our changes, and a red check you did not cause is
  ours to fix.
- We credit you in the changelog and in
  [the contributors list](docs/CONTRIBUTORS.md).
- If something is not ready or not a fit, we say what is blocked and what would
  change the answer.

## Getting Started

### Prerequisites

- Rust 1.89 or later (edition 2024)
- Cargo package manager
- Git

### Setting Up Development Environment

1. Fork and clone the repository:
   ```bash
   git clone https://github.com/YOUR_USERNAME/CodeWhale.git
   cd CodeWhale
   ```

2. Build the current engine and terminal:
   ```bash
   CODEWHALE_BUILD_SHA="$(git rev-parse HEAD)" cargo build --locked -p codewhale-cli
   ```

3. Run the tests near your change (see [Fast local loop](#fast-local-loop)):
   ```bash
   scripts/dev-test.sh tui your_test_filter
   ```

4. Run with development settings:
   ```bash
   ./target/debug/codewhale --version
   ./target/debug/codewhale
   ```

### Testing the latest source

The canonical source is [`codewhale-hq/Codewhale`'s `main` branch](https://github.com/codewhale-hq/Codewhale/tree/main).
Release candidates land there after their CI gates pass, so contributors can
test and build on the same source. Tagged downloads remain the latest published
release; their version can lag the development version on `main`.

For a fresh checkout of the current source:

```bash
git clone --branch main https://github.com/codewhale-hq/Codewhale.git
cd Codewhale
CODEWHALE_BUILD_SHA="$(git rev-parse HEAD)" cargo build --release --locked -p codewhale-cli
./target/release/codewhale --version
./target/release/codewhale
```

Include the commit shown by `--version` when reporting a problem. If you work
from a fork, add the canonical repository as `upstream` and fetch `upstream/main`
before starting a change; preserve any local work when updating your branch.

## Development Workflow

### Code Style

- Run `cargo fmt` before committing to ensure consistent formatting
- Run `cargo clippy` and address all warnings
- Follow Rust naming conventions (snake_case for functions/variables, CamelCase for types)
- Add documentation comments for public APIs

### Testing

- Write tests for new functionality
- Run the tests near your change (see [Fast local loop](#fast-local-loop));
  your pull request runs the Linux workspace suite and a Windows build for
  you (see [What happens after you open a pull request](#what-happens-after-you-open-a-pull-request))
- Colocate unit tests beside the code they cover (standard Rust `#[cfg(test)]`
  modules), and add integration tests under the owning crate's `tests/`
  directory (for example `crates/tui/tests/` or `crates/state/tests/`). The
  repository root `tests/` directory is not used

### Pre-push verification

These are the Rust commands your pull request runs on Linux (the `Lint` and
`Test (ubuntu-latest)` checks). You do not need all of them before every
push: run `cargo fmt`, then check and test the crates you touched (see
[Fast local loop](#fast-local-loop)). Run the full set locally when a change
spans many crates, or let the pull request run it for you:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- \
  -D warnings \
  -A clippy::uninlined_format_args \
  -A clippy::too_many_arguments \
  -A clippy::unnecessary_map_or
cargo test --workspace --all-features --locked
```

Clippy lints test, bench and example targets too (`--all-targets`), and the
three allowances above are the only ones CI and the release lane accept.

#### Fast local loop

The full gate above is what CI enforces, but you do not need it for every
edit. `crates/tui` is a ~750k-line crate, so the loop that stays fast is
the one that avoids rebuilding it more than necessary (numbers and the
reasoning are in [`docs/BUILD_PERFORMANCE.md`](docs/BUILD_PERFORMANCE.md)):

```bash
# 1. Type-check first (seconds after the first build; no codegen, no link).
scripts/dev-cargo.sh check -p codewhale-tui

# 2. Run only the tests near your change (one crate, one filter).
scripts/dev-test.sh tui fleet_setup
# or: scripts/dev-test.sh crates/command-contract/src/elapsed.rs

# 3. Run a whole crate's unit suite. scripts/dev-test.sh uses nextest when
#    it is installed (one process per test, all cores busy, slow tests
#    named; ~100 s here vs ~270 s with libtest).
cargo install cargo-nextest --locked      # once
scripts/dev-test.sh tui
scripts/dev-cargo.sh nextest run --workspace --all-features --locked

# 4. The Linux suite your PR runs:
cargo test --workspace --all-features --locked
```

`.config/nextest.toml` already serializes the PTY suite and bounds the
integration tests that spawn the real binary, so `cargo nextest run` is
safe to use on the whole workspace (nextest does not run doctests; the
authoritative `cargo test` gate does). Tests must not depend on running in
the same process as another test (nextest gives every test its own
process); if a test needs the rustls crypto provider, install it in that
test as production does at startup.

On a machine with less than 16 GB of RAM (or when cross-compiling, e.g.
for OHOS), build one rustc at a time: `CARGO_BUILD_JOBS=1` (or `-j1`), one
crate at a time, `--lib` for tests, never `--workspace`/`--all-targets`.
The tui library needs ~6 GB for its own rustc and its unit-test build ~8 GB;
`cargo test --workspace` runs both at once. Numbers and the full recipe:
[`docs/BUILD_PERFORMANCE.md`](docs/BUILD_PERFORMANCE.md#low-memory-build-recipe-machines-with--16-gb-cross-builds).

If you work in several worktrees, do **not** share one `CARGO_TARGET_DIR`
by default: two cargos on the same target flock and serialize. Use
`scripts/dev-cargo.sh` / `scripts/dev-test.sh`, which give each workspace
its own Cargo `build-dir` (`{workspace-path-hash}` under
`${CODEWHALE_CACHE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/codewhale}`).
`CODEWHALE_DEV_CACHE=local` keeps `./target` if you want that.
`sccache` wraps rustc only when incremental compilation is already off
(`CARGO_INCREMENTAL=0` or `CODEWHALE_SCCACHE=1`) and `sccache` is on
`PATH`; a missing binary is a printed fallback, not an error. Override
the cache root with `CODEWHALE_CACHE_ROOT` — there is no machine-specific
default. A single shared `CARGO_TARGET_DIR` remains valid only for
serialized trunk work. See
[`docs/BUILD_PERFORMANCE.md`](docs/BUILD_PERFORMANCE.md).

Some checks are platform-bound or intentionally excluded from an ordinary
change. Choose them for the risk they answer rather than treating every
available suite as ritual. Visible TUI behavior is accepted in the actual
terminal at the sizes and interaction path affected by the change; the former
full-screen PTY assertion suite was removed because it froze layout and copy
while missing product quality.

- **Long-running process acceptance** should use a sealed local home, local
  fixtures, and the real binary. Record the terminal size, inputs, visible
  result, and any filesystem side effect instead of adding a full-screen
  golden.
- **OCR** (`image_ocr`) uses the macOS Vision framework or a locally
  installed `tesseract`; its platform-specific paths are
  `cfg(target_os = "macos")`-gated and depend on host tooling.
- **Seatbelt sandbox** tests are macOS-only (`cfg(target_os =
  "macos")` at the module level) and do not run elsewhere.

#### Local git hooks are optional

This repository does not install git hooks, and no hook installer
exists; CI is the enforced gate. If you want a local `pre-push` hook
that runs the commands above, add it yourself (`.git/hooks/pre-push` or
`git config core.hooksPath`). Constraints for any local hook:

- A hook must never push, tag, publish, deploy, mutate credentials, or
  rewrite the working tree (no auto-fix commits or silent file
  modification). It may only verify and report.
- To bypass your own hook for a knowingly documented reason (for
  example, pushing work-in-progress to your own fork branch), use
  `git push --no-verify` and say so in the PR description. Bypassing a
  local hook does not make the gates pass — CI still runs them, and a
  bypassed gate must never be reported as a passing one.
- Release publication (tags, GitHub Releases, crates/npm artifacts) is a
  separate, owner-approved gate. Neither local hooks nor a green local
  run authorize any publication step.

### Commit Messages

Use clear, descriptive commit messages following conventional commits:

- `feat:` New feature
- `fix:` Bug fix
- `docs:` Documentation changes
- `refactor:` Code refactoring
- `test:` Adding or updating tests
- `chore:` Maintenance tasks

Example: `feat: add doctor subcommand for system diagnostics`

**Changelog entries are written on `main` at merge time, not in PRs.** Do not
edit `CHANGELOG.md` or `crates/tui/CHANGELOG.md` on a branch: every PR
touching them re-conflicts with every other PR touching them. The release
manager writes one batched "receipts" commit per merge session, and
`./scripts/sync-changelog.sh` keeps the packaged slice in sync. A PR that
carries changelog hunks will be asked to strip them
(`git checkout origin/main -- CHANGELOG.md crates/tui/CHANGELOG.md`).

One exception is enforced by CI: a `feat:` commit whose message mentions an
issue (`#N`) must add `#N` to `CHANGELOG.md` in the same PR
(`scripts/release/check-feature-release-notes.sh`). To avoid touching the
changelog, put issue numbers in the PR description instead of in `feat:`
commit messages; the maintainer writes the entry at merge time.

**AI-assistant co-author trailers are fine.** Using an assistant is welcome and
needs no disclosure, and CI no longer rejects an auto-appended
`Co-authored-by: <some tool>` line. What we do care about is that the humans who
did the work are named — `Co-authored-by` feeds the GitHub contribution graph.
Remove an auto-appended line only if you want to:

```bash
git rebase -i origin/main   # reword each commit, delete the Co-authored-by line
```

Co-author a *person* freely; the address must be their GitHub-linked one
(`id+login@users.noreply.github.com`) or the credit does not register.

When a commit harvests code from a community PR (see "How Your Contribution
Lands" below), include a `Harvested from PR #N by @author` line in the commit
body. A maintainer then closes the referenced PR by hand with a thank-you and
a link to the commit, so the contributor gets a clear signal that their work
shipped.

## What happens after you open a pull request

Opening a pull request against `main` starts the checks below. They run on
GitHub's hosted runners with no repository secrets. GitHub may ask a
maintainer to approve the first run for a brand-new GitHub account.

| Check | What it runs | When it is red |
| --- | --- | --- |
| `Lint` (required) | `cargo fmt --check`, clippy on every target, and the fast drift checks (locales, READMEs, provider registry, dead-code and blocking-call budgets) | Run `cargo fmt --all` and the clippy command in [Pre-push verification](#pre-push-verification); the log names the file and the check |
| `Test (windows-latest)` (required) | Compiles every workspace target for Windows with warnings denied, and the installer PATH helper tests | Usually a `cfg(windows)` path or an import only Windows sees; the log has the compiler error |
| `Version drift` | Version files, the `feat:` release-note rule, release helper tests and contributor credit | The credit step only reports: a maintainer adds your credit when the PR merges |

Smaller checks cover the VS Code extension, integrations, the extension host
and plugin conversion, plus workflow lint when you change `.github/workflows`.
A documentation-only change skips the Rust builds but still reports the
required checks, so it can merge.

Your pull request does not run the test suites, the safety gate, MSRV, the npm
wrapper smoke, the slow runtime-contract and persistence-backlog measurements,
the dependency audits or the OpenHarmony build. Maintainers run those once per
release, on the exact commit being released. If that run finds a problem your
change introduced, we fix it on `main`; we will not ask you to chase it.

A red check that your change did not cause is not your job to fix. We rerun it
or fix it on `main`, and we never close a PR because CI is slow or flaky. Your
PR may be a draft, and the issue line in its description is optional.

## How Your Contribution Lands

**Your pull request merges as itself.** That is the default for every
contribution: a maintainer, or an agent working for one, reviews it and merges
it, so your commits reach `main` with you as their author.

Before merging, a maintainer may:

- ask questions or request changes in review;
- push small fixes, or a merge of current `main`, to your branch when GitHub
  allows edits by maintainers, instead of asking you to rebase around our churn;
- write the `CHANGELOG.md` entry and your credit in `docs/CONTRIBUTORS.md` on
  `main` when the PR merges. You do not edit those files in your PR.

### Fallback — harvest

Rarely, a branch cannot merge in reasonable time, for example when it is in a
fork we cannot push to and the conflicts cannot be resolved there. We will ask
on your PR first. If it still cannot merge, a maintainer may **harvest** the
useful commits into `main` instead. This is **not a rejection**: your code
landed, and your credit is kept mechanically.

- You stay the author of the harvested commit, with your GitHub-linked address
  from `.github/AUTHOR_MAP` or
  `gh api users/<login> --jq '"\(.id)+\(.login)@users.noreply.github.com"'`,
  never a raw, `.local` or old-style noreply email from your machine.
- The commit body includes `Harvested from PR #N by @your-handle`. That line is
  the contract: it is your credit and the signal that your work shipped.
- The `CHANGELOG.md` entry for the next release credits you by handle.
- A maintainer closes your PR by hand with a thank-you and a link to the commit
  on `main`.

When a maintainer closes a harvested PR by hand, the closing comment
follows this template (the pattern set on PR #2634):

```text
Closing with harvest credit, @handle — <what landed> landed via
<commit sha(s) or PR #N>. <If work remains:> The remainder is tracked
in #NNN — follow-ups welcome there.
Thank you for <one specific thing the contribution got right>.
```

Three required elements: the contributor's handle, the exact commits or
PRs where their work landed, and — when the PR contained more than what
landed — a tracking issue for the remainder. A harvested PR is never
closed with a bare "superseded".

To help your PR merge quickly as itself:

1. **Keep PRs single-purpose.** One bug fix per PR; one feature per PR.
   Don't mix a refactor with a feature.
2. **Start from current `main`.** If `main` moves while your PR is in review,
   you do not need to rebase; we resolve conflicts our changes caused.
3. **Include tests** with new behavior. A PR with tests needs fewer review
   round trips.
4. **Avoid the trust-boundary surface** without prior maintainer
   sign-off. That includes auth/credential flows, sandbox policy,
   publishing/release plumbing, and `prompts/` content. PRs that touch
   these without prior discussion are unlikely to merge directly even
   when the change is well-implemented.

## Layered and EPIC-Sized Work

Some architecture work is too large for one PR but still needs to be built in
dependent layers. For those changes, use this workflow:

1. Start with a tracking issue or EPIC when the work spans multiple PRs. Name
   the intended slices and state what each slice is not trying to close yet.
2. Keep each implementation PR focused on one behavior boundary.
3. Later layers may stay in your fork or open as draft PRs while the lower
   layer is still moving. Draft stacked PR titles or descriptions should say
   `Draft / depends on #NNNN`.
4. A dependent PR is not ready for merge review until the lower layer has
   landed, the branch has been rebased onto current `main`, and the PR targets
   `main`.
5. The PR body should identify which earlier PR it builds on, what is in scope,
   what is explicitly out of scope, which issues it references, and which local
   commands were run.
6. Use `Closes #...` only when the slice fully satisfies an issue. Use
   `Refs #...` with a short `(partial)` note when the PR advances a broad issue
   but leaves follow-up work.
7. Structured commits are fine during review. Maintainers merge the PR itself
   (merge commit, squash or rebase) with you as the author. If a layer has to
   be harvested instead, the [fallback](#fallback--harvest) rules apply.

Before asking for merge review on a layered PR, check that it is:

- rebased onto current `main`
- marked ready for review, not draft
- focused to one behavior boundary
- backed by local command evidence in the PR body
- green in its pull-request checks, or has any remaining red check clearly
  explained
- covered by round-trip or migration-preservation tests when it changes config
  or schema behavior
- referencing broad issues as partial unless it really closes them

For layered work, a useful PR description shape is:

```text
Summary:
Scope:
Not in this slice:
Builds on:
Issues:
Validation:
```

## Which branch to target

**`main`, for everything.** There is no separate staging branch. An earlier
version of this guide pointed layered refactors at `codex/v0.9.0-stewardship`;
that branch no longer exists, so please ignore any instruction you find
elsewhere to base work on it.

**We do not expect you to rebase around our churn.** If your PR conflicts only
because `main` moved while it was in review, say so and a maintainer resolves
it on your branch. If your branch is in a fork we cannot push to, we ask on the
PR first; harvesting is the last resort.

## Labels

Issues and pull requests are labelled by `.github/workflows/triage.yml`, which
only ever adds labels that already exist. Pull requests get their labels from
the paths they change and their conventional-commit title prefix; issues get
them from the title and form answers.

| Family | Meaning |
| --- | --- |
| `area:*` | The subsystem or client: `tui`, `core`, `tools`, `mcp`, `state`, `sandbox`, `plugins`, `providers`, `runtime-api`, `install`, `localization`, `ci`, `desktop`, `web`, `mobile`, `remote-control`, `subagents` |
| `os:*` | Platform-specific behaviour: `windows`, `macos`, `linux`, `freebsd`, `ohos` |
| `priority:p0` / `p1` / `p2` | Set by a maintainer; `release-blocker` still marks work that must land before a release |
| `bug`, `enhancement`, `documentation`, `question`, `cleanup`, `performance`, `security` | What kind of change it is |
| `accessibility` | Screen readers, keyboard use, contrast, motion |
| `contributor:external` | Opened by someone outside the maintainer team |
| `lang:zh` | Written in Chinese |
| `needs-triage`, `needs-info`, `agent-ready`, `agent-in-progress`, `needs-human`, `keep-open`, `pinned`, `stale` | Workflow state; other workflows read these names |

Release targeting uses milestones, not version labels.

## Working with an agent

Using an AI assistant is welcome and needs no disclosure. Codewhale is used to
work on Codewhale, and many fixes here began as a person pointing an agent at
an issue.

What makes agent-assisted work easy to merge is the same as for any other: a
specific reproduction, a small diff, the checks you ran, and a description of
the trade-off. Keep each pull request to one friction point, and put a list of
ideas for improvement in an issue.

Do not point an agent at auth, credentials, sandbox policy, publishing and
release plumbing, provider policy, telemetry, sponsorship, branding or global
prompts without a maintainer's agreement on the issue first.

Maintainers write some issues with the
[Agent task](.github/ISSUE_TEMPLATE/agent-task.yml) form so that an agent with
no other context can finish them; those carry the `agent-ready` label and are
open to anyone. Agents and maintainers follow
[docs/AGENT_ETHOS.md](docs/AGENT_ETHOS.md).

## How maintainers ship

Maintainers, and the agents working for them, land small verified slices
directly on `main` with the checks they ran in the commit message. Every push
to `main` runs a fast preflight and the full CI suite on Linux, macOS and
Windows. A release also needs a green `release-candidate.yml` run on the exact
commit being released; the sequence is in
[docs/RELEASE_RUNBOOK.md](docs/RELEASE_RUNBOOK.md).

Contributor pull requests still merge as pull requests, as described in
[How Your Contribution Lands](#how-your-contribution-lands).

## Project Structure

Codewhale is a Cargo workspace with one Engine implementation in
`crates/engine/src/core/engine/`. The public `codewhale` executable links the
TUI/runtime library; interactive sessions, noninteractive runs and the Runtime
API share that Engine.

| Path | Purpose |
| --- | --- |
| `crates/cli/` | Public command entrypoint, configuration commands and runtime dispatch |
| `crates/tui/` | Interactive terminal, Engine, tools, Runtime API and embedded local web client |
| `crates/core/`, `crates/protocol/`, `crates/state/` | Request construction, session/turn types, protocol framing and persistence |
| Other `crates/` | Shared configuration, credentials, telemetry, hooks, workflow and packaging support; see each Cargo manifest |
| `web/` | Public Next.js website and documentation; separate from the embedded Runtime web client |
| `telemetry-ingest/` | Telemetry service, schemas and service tests |
| `extensions/`, `integrations/` | Editor integration and external-service bridges |
| `npm/`, `packaging/`, `nix/` | npm wrappers/SDK and platform installation definitions |
| `computer/snapshots/` | Cloud Computer image definitions, pinned independently of the source checkout |
| `deploy/` | Deployment templates consumed by setup scripts, including Tencent Lighthouse services |
| `fleets/`, `workflows/` | Distributed Fleet definitions and workflow examples |
| `brand/` | Source artwork and generated brand variants used by the README, website and terminal |
| `docs/` | User/developer documentation, schemas, fixtures and referenced release material |
| `scripts/`, `.github/`, `.cnb.yml` | Development, validation, CI and release tooling |
| `patches/` | Vendored dependency fixes, including their licensing files |

Generated files that the product embeds or validates, such as model catalogs,
website facts and schemas, remain tracked with their generators. Platform
mirrors such as `.winget/` are retained when their packaging tools require them.
Keep local critique output, temporary verification reports and personal
operator instructions outside the tracked product tree; describe the change
and its validation in the pull request. Do not copy workspace-level operator
`AGENTS.md` or `CLAUDE.md` files into this repository.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the runtime data flow and
[the build guide](docs/BUILD_PERFORMANCE.md) for crate dependencies and local
verification.

## Writing the pull request

- Use the [pull request template](.github/PULL_REQUEST_TEMPLATE.md): what and
  why, where it came from, and how you tested it.
- Link the issue: `Closes #N` (or `Fixes` / `Resolves`) when the pull request
  finishes it, `Refs #N` for related or partial work. Never write a closing
  keyword inside a negation such as "does not close #N": GitHub closes the
  issue anyway.
- Keep it to one change. If you find related work, open an issue for it.
- If you add a layer, module or abstraction, say which one it replaces.
- Update the docs your change makes wrong.
- A draft is fine, and so is asking for direction before it is finished.

## Security

Please do **not** open a public issue for a vulnerability. See
[SECURITY.md](.github/SECURITY.md) for private reporting.

## Code of Conduct

Be respectful and inclusive. We welcome contributors of all backgrounds and
experience levels. See [CODE_OF_CONDUCT.md](.github/CODE_OF_CONDUCT.md).

## License

By contributing to Codewhale, you agree that your contributions will be
licensed under the MIT License.

## Questions?

Ask in [Discussions](https://github.com/codewhale-hq/CodeWhale/discussions) or
on [Discord](https://discord.gg/37gfS3ksug), or add the maintainer, Hunter, on
WeChat (`hunterbown`) and ask to join the Whale Brothers group. Everyone is
welcome.
