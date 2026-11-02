---
name: cw-land
description: "Use when turning verified Codewhale work into commits on main and pushing them, or merging a contributor PR: staging only your paths, the pre-push drift checks, preserving contributor credit, and honoring the gate artifact before merging."
---

# cw-land

Verified work still has to land without stepping on other writers, losing
contributor credit, or merging past a gate that has not actually passed. This
stage is about the boundary between "it works" and "it is in `main`" — and about
which of those steps you are allowed to take.

Stage 5 of the loop: [cw-orient](../cw-orient/SKILL.md) →
[cw-slice](../cw-slice/SKILL.md) → [cw-gates](../cw-gates/SKILL.md) →
[cw-dogfood](../cw-dogfood/SKILL.md) → **land** →
[cw-handoff](../cw-handoff/SKILL.md).

## When to use

- The change is verified and needs to become a commit on `main`.
- You are landing someone else's PR, harvesting a contributor's work, or
  resolving a conflict caused by `main` moving.
- You are about to merge something behind a required gate.

## Workflow

1. **Land on the canonical checkout's `main`.** `AGENTS.md` owns this: no
   maintainer PR branches, worktrees, nested clones or integration lanes.
   Several agents share the checkout, so partition by file, check
   `git status` before staging, stage only your paths (never `git add -A`),
   and retry a commit that fails on `index.lock`. Push verified `main`
   directly. A push is not a CI request: hosted CI runs once, at the final
   release step (`docs/RELEASE_CHECKLIST.md`), and routine slice commits carry
   `[skip ci]`. PRs are for contributors.

2. **Commit narrow and build-green.** One coherent change per commit; the tree
   builds at every commit. Put the real verification in the message — actual
   pass/fail counts, not "tests pass".

   Before every push, run the cheap drift checks that have turned a green
   `main` red most often. They take seconds:
   ```bash
   cargo fmt --all -- --check
   bash scripts/release/check-versions.sh
   ```
   The runtime-contract budget checker starts a native Cargo build by default;
   it is not a cheap drift check. Use an applicable existing measurement with
   `--receipt`, or run a new measurement through the required build-lock and
   SSD cache workflow when the changed contract or final validation needs it.
   Add the one that matches what you touched: `check-tui-locale-parity.py`
   and `check-tui-product-vocabulary.sh` (locale strings),
   `check-dead-code-budget.py` (`#[allow(dead_code)]`),
   `check-blocking-calls-budget.py` (async code),
   `check-readme-translations.py` and `check-readme-locales.sh` (READMEs),
   `node web/scripts/derive-facts.mjs && node web/scripts/check-facts.mjs`
   (public facts). Put a new CHANGELOG entry at the **top** of
   `[Unreleased]`. Open contributor PRs append at its end, and an entry of
   ours there conflicts with every one of them.

   **Record it in the same commit.** A user-visible `feat` or `fix` commit
   carries its line at the top of the matching `[Unreleased]` section (Added,
   Changed or Fixed): what changed for the user, what is still unproved,
   `(#N)`, and for reported or contributed work
   `Thanks [@handle](https://github.com/handle)`. Tests, refactors and docs
   need no line. Work by someone outside the maintainers also goes on the
   three credit surfaces in that commit: the CHANGELOG `### Contributors`
   block, `docs/CONTRIBUTORS.md`, and `UNRELEASED_CONTRIBUTORS` in
   `web/lib/release-credits.ts`. A contributor PR merged as itself gets that
   commit right after the merge. Before the push:
   ```bash
   sh scripts/sync-changelog.sh                 # regenerates crates/tui/CHANGELOG.md
   python3 scripts/check-contributor-credit.py  # since the last tag
   git log --format='%h %s' "$(git log -1 --format=%H -- CHANGELOG.md)"..HEAD | grep -E '^[0-9a-f]+ (feat|fix)'
   ```
   The last command must print nothing for commits you are landing.
   `check-feature-release-notes.sh` covers only issue-linked `feat` commits;
   fixes slip past it. A catch-up "changelog: record landed work" commit is
   the failure this step prevents, not the routine.

3. **Preserve credit mechanically, not just politely.** GitHub reads neither
   `.github/AUTHOR_MAP` nor `.mailmap` for the contribution graph, so
   authorship must use the contributor's own GitHub-linked address; those
   files are project conventions on top. When a contributor's work lands as
   our commit, the contributor is its **author**
   and it carries the harvest line (`AGENTS.md`, "Harvested contributor
   credit"):
   ```text
   git commit --author="Name <ID+handle@users.noreply.github.com>" ...

   Harvested from PR #N by @handle

   Co-authored-by: <maintainer or agent identity>
   ```
   GitHub's contributor list counts authors only, and
   `check-contributor-credit.py` flags a harvested handle who is not credited.
   Resolve the address from `.github/AUTHOR_MAP`, else
   `gh api users/<handle> --jq '"\(.id)+\(.login)@users.noreply.github.com"'`.
   A clean cherry-pick keeps the author by itself. This step is the single
   owner of credit mechanics; other skills link here.
   The `Harvested from` line is what lets `auto-close-harvested.yml` close
   their PR with credit. Canonical human identities live in
   `.github/AUTHOR_MAP`.

   Whether a bot or agent also appears in a trailer no longer matters — the CI
   check that policed trailer identities was removed because it rejected
   ordinary agent commits. Give humans their credit; don't spend time scrubbing
   tool trailers.

4. **Landing someone else's work: their time is more expensive than ours.**
   - Never make a contributor rebase around our churn. If their PR conflicts
     only because `main` moved, a maintainer resolves it.
   - Read their diff against the **merge base** first, so you know exactly what
     they added, then re-apply that — rather than hand-merging two large sides
     and hoping:
     ```bash
     git diff $(git merge-base main <pr-head>)..<pr-head>
     ```
   - **Conflicts that split mid-function do not resolve by keeping both sides.**
     Git's markers can land inside a body, so a both-sides resolution produces
     unbalanced braces that look plausible and do not compile. Default: take
     one side whole, then re-insert the other side's additions at their
     original anchor; when the conflict doesn't fit that pattern, resolve it
     however is correct and let the compiler judge.
   - Resolve on their branch without creating a local branch. Build the merge
     commit with `git merge-tree --write-tree origin/main <pr-head>` and
     `git commit-tree <tree> -p <pr-head> -p origin/main`, then push it with
     `git push <fork-url> "${sha}:refs/heads/<their-branch>"`. In zsh, write
     `${sha}`, not `$sha:refs`, because `:r` is a history modifier.
     `maintainerCanModify` does not guarantee that the push is accepted. When
     the fork refuses it, ask on the PR. Harvesting is the last resort.
   - **Check the contribution gate before assuming a PR is stalled.** An
     unlisted author's workflow runs sit at `action_required` and never start,
     so the PR looks abandoned when nobody has actually looked at it. Approve
     the runs, then fix the cause: add them to `.github/APPROVED_CONTRIBUTORS`
     (`all:username`), or comment `/lgtm` (PR scope) or `/lgtmi` (issue scope).

5. **Merging under a gate.** `AGENTS.md` ("Merging under a gate") owns this:
   the artifact must literally say PASS, the review thread outranks the check
   rollup, and ambiguity blocks the merge — never the reverse. Before you
   merge, list the open PRs whose base is the PR's source branch
   (`gh pr list --base <branch>`) and retarget them first. Otherwise they
   close when that branch goes away.

## Red flags / don't

- Don't tag, create a release, publish, or deploy without explicit
  authorization. Pushing verified `main` is allowed; it is none of those.
- Don't rewrite published history, retag a release, or force-push a shared ref.
- Don't commit `AGENTS.md` / `CLAUDE.md` operator controls that live outside the
  product repository into a public repo.
- Don't stage another writer's dirty files to get a clean commit.
- Don't merge on a green rollup alone when a review thread has open findings.
- Don't harvest or close from a PR title or label — review the code, tests,
  comments, and checks.
- Don't add another legacy call site for convenience once a replacement
  architecture is adopted. Declared migrations are one-way.
- Don't leave new enforcement live: keep it dry-run/advisory unless approved.

## Output

- Commit SHAs, the paths committed, the drift checks run, and the resulting
  `origin/main` SHA.
- The credit trailers applied and to whom.
- The CHANGELOG line and credit-surface entries added, or why none was owed.
- The Linear/GitHub status written after the push, or why none was owed.
- The gate artifact's literal verdict at merge time, if a gate applies.
- Exactly which public actions you took, and which you deliberately did not.
