# Contributing to codewhale

Thank you for your interest in contributing to codewhale! This document provides guidelines and instructions for contributing.

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
body. An auto-close workflow watches for this pattern and closes the
referenced PR with credit so the contributor gets a clear signal that
their work shipped.

## What happens after you open a pull request

Opening a pull request against `main` starts the checks below. They run on
GitHub's hosted runners with no repository secrets. GitHub may ask a
maintainer to approve the first run for a brand-new GitHub account.

| Check | What it runs | When it is red |
| --- | --- | --- |
| `Lint` (required) | `cargo fmt --check`, clippy on every target, and the fast drift checks (locales, READMEs, provider registry, dead-code and blocking-call budgets) | Run `cargo fmt --all` and the clippy command in [Pre-push verification](#pre-push-verification); the log names the file and the check |
| `Test (ubuntu-latest)` | The whole workspace suite on Linux with `cargo nextest`, plus doctests | Re-run the named test locally, for example `scripts/dev-test.sh tui <test name>` |
| `Test (windows-latest)` (required) | Compiles every workspace target for Windows with warnings denied, and the installer PATH helper tests | Usually a `cfg(windows)` path or an import only Windows sees; the log has the compiler error |
| `npm wrapper smoke (ubuntu-latest)` (required) | Builds `codewhale` and installs it through the npm wrapper | The log shows the failing install or entrypoint step |
| `Safety gate` | The sandbox, authorization and auto-review tests | Treat it as a real regression in a trust boundary |
| `Version drift` | Version files, the `feat:` release-note rule, release helper tests and contributor credit | The credit step only reports: a maintainer adds your credit when the PR merges |
| `PR closes an issue` | Your description has `Closes #N`, `Refs #N` or `No-Issue: <reason>` | Edit the description, then re-run the check |
| `DCO` | Lists commits without `Signed-off-by` | Advisory; it never blocks |
| `Contribution gate` | Labels PRs from authors not yet in `.github/APPROVED_CONTRIBUTORS` | A label, not a judgement; see [Contribution Gate](#contribution-gate) |

Smaller checks cover MSRV, the VS Code extension, integrations, the extension
host and plugin conversion, plus workflow lint when you change
`.github/workflows`. A documentation-only change skips the Rust builds but
still reports the required checks, so it can merge. The Linux test suite took
45 minutes on a recent pull request; the other checks run alongside it.

Your pull request does not run the macOS and Windows test suites, the slow
runtime-contract and persistence-backlog measurements, the dependency audits
or the OpenHarmony build. Maintainers run those once per release, on the exact
commit being released. If that run finds a problem your change introduced, we
fix it on `main`; we will not ask you to chase it.

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
fork we cannot push to and the conflicts cannot be resolved there. Then a
maintainer may **harvest** the useful commits into `main` instead. This is
**not a rejection**: your code landed, and your credit is kept mechanically.

- You stay the author of the harvested commit, with your GitHub-linked address
  from `.github/AUTHOR_MAP` or
  `gh api users/<login> --jq '"\(.id)+\(.login)@users.noreply.github.com"'`,
  never a raw, `.local` or old-style noreply email from your machine.
- The commit body includes `Harvested from PR #N by @your-handle`. That line is
  the contract: it is your credit and the signal that your work shipped.
- The `CHANGELOG.md` entry for the next release credits you by handle.
- `auto-close-harvested.yml` closes your PR with a thank-you and a link to the
  commit on `main`.

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

## Contribution Gate

Codewhale uses a maintainer-managed contribution gate for the community front
door. Maintainers and collaborators bypass this gate automatically. The gate
workflows run in dry-run mode and only add labels, so maintainers can observe
the signal before changing contributor flow.

The maintainer posture is documented in
[docs/AGENT_ETHOS.md](docs/AGENT_ETHOS.md): automation should reduce load while
keeping good-faith contributors seen, credited, and able to keep helping.

Issues are never auto-closed by the contribution gate. Unapproved external
issues receive a triage label and remain open for maintainers. Codewhale
depends on real edge cases from real users, so issue intake should stay warm
and open.

Pull requests are different because they can touch code, CI, release plumbing,
auth, sandboxing, provider policy, and other trust-boundary surfaces. The PR
gate can be switched from dry-run to enforcement when maintainers decide they
need that safety control, but it should be treated as a review-load control,
not a judgment on contributor quality. Before enabling PR enforcement, seed the
allowlist broadly enough for active external contributors who should not be
interrupted by the rollout.

The allowlist is scoped:

- `pr:username` allows pull requests.
- `issue:username` allows issues.
- `all:username` allows both.

A maintainer can approve someone by commenting `/lgtm` on a pull request for PR
access, or `/lgtmi` on an issue for issue access. The exact bare commands
`lgtm` and `lgtmi` are also accepted for compatibility, but the prefixed forms
are preferred because they are harder to trigger accidentally in ordinary review
discussion.

Approvals do not edit `main` directly. The approval workflow opens a small
allowlist update PR so the new entry is reviewable before it takes effect.

If the PR gate fires on a good contributor incorrectly, use the same approval
flow to restore them: comment `/lgtm`, merge the generated allowlist PR, then
reopen the affected pull request. If GitHub will not allow the closed PR to be
reopened, ask the contributor to resubmit after the allowlist PR is merged.

## Agent-Assisted Improvements

Codewhale is allowed to help improve Codewhale, but the contribution still has
to be shaped for human review. The recommended workflow is the recursive self-improvement prompt
in the private `codewhale-ops` repo: run it
from a fresh fork or branch, let the agent find exactly one small friction point,
and stop after one patch. DeepSeek V4 Pro is the reference path for this loop
today, but any configured provider works — the review shape matters more than
the provider.

Agents and maintainers should follow the stewardship posture in
[docs/AGENT_ETHOS.md](docs/AGENT_ETHOS.md): use automation for evidence,
verification, and narrow patches while keeping the final community decision
human-reviewed.

The useful output is not "ideas for improvement." The useful output is a
specific reproduction, a minimal diff, focused checks, and a PR description that
explains the trade-off. Do not use an agent to touch auth, credentials, sandbox
policy, publishing/release plumbing, provider policy, telemetry, sponsorship,
branding, or global prompts without prior maintainer sign-off.

## How maintainers and agents ship

Maintainers, and the agents working for them, do not open pull requests. They
land verified work directly on `main` with the local checks they ran and their
counts in the commit message. A push to `main` starts no GitHub build or test
workflow: GitHub only mirrors `main` to CNB and GitCode and closes harvested
PRs. CNB then runs its own Linux gates on the mirror and reports them on CNB,
not on GitHub. Do not write `[skip ci]`; nothing needs skipping, and it would
also skip the mirrors and the harvest auto-close.

The exhaustive gate runs once, at the final release step, from one dispatch on
the frozen candidate commit:

```bash
git fetch origin main
candidate_sha="$(git rev-parse origin/main)"
gh workflow run ci.yml --repo codewhale-hq/CodeWhale --ref main \
  -f expected_sha="${candidate_sha}"
```

It stops if `main` no longer resolves to that commit. Otherwise it runs every
CI gate on Linux, macOS and Windows, the budget measurements, the
documentation build, `cargo-deny`, the cargo and npm audits and the
OpenHarmony build. Publishing also needs a green `release-candidate.yml` run
on the same commit, which `release.yml` checks; the full sequence is in
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

## Submitting Changes

1. Create a feature branch from `main`:
   ```bash
   git checkout -b feat/your-feature
   ```

2. Make your changes and commit them

3. Run the pre-push verification commands (see
   [Pre-push verification](#pre-push-verification) above for the exact
   gate and the stricter release clippy form)

4. Push your branch and create a Pull Request

5. Describe your changes clearly in the PR description

## Pull Request Guidelines

- Use the [pull request template](.github/PULL_REQUEST_TEMPLATE.md) when opening
  a PR — what and why, the issue line, and how you tested it
- The PR description needs one issue line, checked by CI
  (`.github/workflows/pr-issue-link.yml`): `Closes #N` (or `Fixes` /
  `Resolves`) when the PR finishes the issue, `Refs #N` for related or partial
  work, or `No-Issue: <one-line reason>`. Never write a negated closing
  keyword such as "does not close #N": GitHub closes the issue anyway, so CI
  rejects it
- If you add a new layer, module, or abstraction, say which one it replaces
  or deletes
- Keep PRs focused on a single change
- Update documentation if needed
- Add tests for new functionality
- Get the pull-request checks green before requesting review, or say in the
  PR why one is red

## Shape of a Typical PR

A well-structured PR follows a consistent pattern. Recent exemplars include:

- **#386** — `/init` command: new `crates/tui/src/commands/groups/project/init.rs` module, project-type detection,
  AGENTS.md generation, command registration in `commands/mod.rs`, localization strings.
- **#389** — Inline LSP diagnostics: LSP subsystem in `crates/lsp/src/`, engine hooks in
  `crates/engine/src/core/engine/lsp_hooks.rs`, config toggle, test coverage.
- **#387** — Self-update: new `crates/cli/src/update.rs` module, CLI subcommand registration,
  HTTP download + SHA256 verification + atomic binary replacement.
- **#393** — `/share` session URL: new `crates/tui/src/commands/groups/project/share.rs`, HTML rendering,
  `gh gist create` integration, command registration.
- **#343/#346** — (v0.8.5) Runtime thread/turn timeline and durable task manager refactors.

Typically each PR touches 1–3 new files, modifies 2–5 existing files for wiring
(registries, dispatch matches, localization), and adds or updates tests. Changes
are scoped to a single feature or fix — if you discover related work that needs
doing, open a separate issue rather than expanding the PR scope.

Before submitting, run the commands in
[Pre-push verification](#pre-push-verification).

## Reporting Issues

When reporting issues, please use one of the issue templates:

- [Bug report](.github/ISSUE_TEMPLATE/bug_report.yml) — for reproducible problems
  or regressions
- [Feature request](.github/ISSUE_TEMPLATE/feature_request.yml) — for ideas and
  improvements

The forms ask for what a report needs (`codewhale --version`, OS, how you got
Codewhale, and steps to reproduce). Questions go to
[Discussions](https://github.com/codewhale-hq/CodeWhale/discussions) or
[Discord](https://discord.gg/37gfS3ksug).

## Security

If you discover a security vulnerability, please do **not** open a public issue.
See [SECURITY.md](.github/SECURITY.md) for the responsible disclosure process and
contact information.

## Code of Conduct

Be respectful and inclusive. We welcome contributors of all backgrounds and
experience levels. See [CODE_OF_CONDUCT.md](.github/CODE_OF_CONDUCT.md) for the full
code of conduct.

## License

By contributing to codewhale, you agree that your contributions will be licensed under the MIT License.

## Questions?

Feel free to open an issue for any questions about contributing.
