---
name: contributor-onboarding
description: Help a new contributor get productive on this checkout - inspect sync state against main, build, run the repository's exact verification gate, and produce a local what's-new digest. Never fetches, pulls, or modifies a dirty tree on its own. Explicit-only.
invocation: explicit-only
---

# Contributor Onboarding

Requested by @JayBeest in issue #4227: a first-run path for a contributor who
has cloned the repo and wants to know *am I current, does it build, does it
pass, and what changed while I was away* — without a wall of prose and without
anything touching their working tree behind their back.

## Invocation

Explicit-only. Loading this skill is **not** authority to fetch, pull, rebase,
push, or write files. Every network or mutating step below is a separate action
the contributor must ask for after reading the plan.

## Non-goals

- Do not run `git fetch`, `git pull`, `git rebase`, or `git checkout` on your
  own initiative. Report state; propose the command; wait.
- Do not stash, discard, reset, or commit a dirty tree. Ever.
- Do not call a model provider. Every step here is a local command with a
  deterministic result. The digest is built from files and git output, not
  generated prose.
- Do not claim a gate passed that you did not run, and do not summarize a
  build you did not observe.
- Do not privilege any provider. Codewhale is provider-neutral; a dogfood run
  uses whatever route the contributor already configured, or none.

## Workflow

### 1. Inspect (read-only, always safe)

Run these and report the results verbatim. Nothing here writes:

```
git rev-parse --abbrev-ref HEAD
git status --porcelain
git rev-list --left-right --count origin/main...HEAD
```

Report three facts plainly:

- **Branch** the contributor is on.
- **Tree state**: clean, or the count and paths of dirty entries.
- **Sync state**: `N behind, M ahead` of `origin/main`, or **unavailable** when
  `origin/main` is missing or has never been fetched. Unavailable is a real
  answer — say it rather than guessing zero.

### 2. Sync — propose, never perform

If behind, print the exact commands and stop:

```
git fetch origin
git rebase origin/main     # or: git merge origin/main
```

**If the tree is dirty, do not propose a sync at all.** Print a recovery plan
first, in this order, and let the contributor choose:

1. `git stash push -u -m "wip before sync"` then sync, then `git stash pop`
2. Commit the work on a branch, then sync
3. Stay behind and continue — being behind is not an error

### 3. Build

```
cargo build --release -p codewhale-cli -p codewhale-tui
```

Report the exit status and the first error if it fails. A build failure ends
the run: do not proceed to the gate and do not report gate results.

### 4. Verification gate — the repository's exact CI command

Run what CI runs, not a paraphrase of it:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-features --locked -- \
  -D warnings \
  -A clippy::uninlined_format_args \
  -A clippy::too_many_arguments \
  -A clippy::unnecessary_map_or \
  -A clippy::collapsible_if \
  -A clippy::assertions_on_constants
cargo test --workspace
```

These are copied from `.github/workflows/ci.yml`. If that file changes, this
list is stale — read the workflow and say so rather than running a command CI
no longer uses.

Known suite papercut: `run_verifiers_background_*` is flaky under full-suite
parallelism and passes in isolation. Attribute it to the known flake, not to
the contributor's change.

### 5. What's new — deterministic local digest

Built only from files already on disk. No network, no model:

```
git log --oneline -n 20 origin/main
```

plus the topmost released section of `CHANGELOG.md`.

Rules:

- Cap the digest at **20 commits and 40 lines** of changelog. State the cap
  when you hit it; do not silently truncate.
- If `origin/main` is unavailable, digest `HEAD` instead and label it as such.
- Quote what the files say. Do not summarize, rank, or editorialize — the
  point is that two contributors on the same commit get the same digest.

### 6. Dogfood — optional, staged, confirmed

Only after the gate has actually passed, and only if the contributor asks.
Print the plan and require an explicit yes before running anything:

```
./target/release/codewhale exec --help
```

This is a provider-free smoke check: it exercises the built binary without
sending a request anywhere. Anything beyond it — an actual `codewhale exec`
turn — needs the contributor's own configured route and their explicit
go-ahead. Never select a provider for them and never fall back to a default
one.

## Reporting

End with a compact status table: branch, tree, sync, build, gate, digest,
dogfood. Use `not run` for anything skipped and `unavailable` for anything the
environment could not determine. Never write `passed` from inference.

## Credit

Requested by @JayBeest (#4227). Preserve that attribution in the changelog
entry and in the commit body of any change that lands from this skill.
