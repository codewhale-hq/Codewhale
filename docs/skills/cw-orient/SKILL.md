---
name: cw-orient
description: "Use at the start of any Codewhale work session, or when unsure whether the canonical checkout is on current `main` and what is dirty in it: establish live repo truth before reading a plan or editing a file."
---

# cw-orient

Open every session on live truth, not on a handoff note, a directory name, or
what was true last time. Several agents share this one checkout on `main`; the
cheapest bug to avoid is committing a live writer's half-written edit.

This is stage 1 of the loop: **orient → [cw-slice](../cw-slice/SKILL.md) →
[cw-gates](../cw-gates/SKILL.md) → [cw-dogfood](../cw-dogfood/SKILL.md) →
[cw-land](../cw-land/SKILL.md) → [cw-handoff](../cw-handoff/SKILL.md)**.

## When to use

- Starting a session, resuming one, or taking over from another agent.
- A handoff, issue, or plan tells you the state of the repo — verify before
  trusting it.
- You are about to edit and are not certain what is dirty or who is writing.

## Workflow

1. **Locate yourself.** Checkout, branch, dirt, and how far you are from the
   remote:
   ```bash
   git rev-parse --show-toplevel && git branch --show-current
   git status --short --branch
   git log --oneline --decorate -10
   ```
   Agent work happens on `main`. A detached HEAD or another branch here is a
   state to reconcile (preserve dirt and stranded commits, compare with
   `origin/main`, return to `main`) before editing; see `AGENTS.md` § Working
   rules.

2. **Look for stranded work, not lanes.** `git worktree list` and recent
   branches tell you what may hold unlanded commits. Inventory them; do not
   start work in one, and do not create one.
   ```bash
   git worktree list
   git branch --sort=-committerdate --format='%(committerdate:short) %(refname:short)' | head -20
   ```

3. **Dirt in this checkout is yours** (`AGENTS.md`, founder 2026-10-22).
   ```bash
   git status --porcelain
   git diff --stat
   ```
   Check mtimes: a file changed in the last 15 minutes, or one a live session
   is editing, is mid-edit — ask that writer to commit, and commit the rest
   yourself in coherent verified groups ([cw-land](../cw-land/SKILL.md)). Never
   stash, `checkout --`, reset or clean it.

4. **Fix which guidance applies.** The nearest scoped `AGENTS.md` wins over the
   root one, and it is where the per-area rules actually live:
   ```bash
   find . -name AGENTS.md -not -path './tmp/*' -not -path '*/node_modules/*' -not -path './target/*'
   ```
   Today: root `AGENTS.md`, `crates/tui/AGENTS.md`,
   `crates/localization/locales/AGENTS.md`, `web/AGENTS.md`. Read the one that owns the
   files you are about to touch.

5. **Establish version truth from source, not memory.**
   ```bash
   grep -m1 '^version' Cargo.toml
   ./scripts/release/check-versions.sh
   ```
   `check-versions.sh` is the drift gate across the workspace version, npm,
   `Cargo.lock`, the changelog, and the README. If it disagrees with what you
   were told, believe the script.

6. **Only if the task is about the community queue** — issues, PRs, harvesting,
   credit — pull the live queue with the `gh-*` skills in this directory rather
   than assuming from memory. When the task is local-only, stay offline and
   record the missing external receipt instead.

## Red flags / don't

- Don't infer the current state from a directory name, a stale handoff, or a
  `.md` file's confident prose. All three have been wrong here.
- Don't treat a plan document as current state. Plans describe intent; `git`
  describes reality.
- Don't clean, stash, reset, or `git checkout --` dirt in the checkout; read
  it, prove it, and commit it, or hand it to the live writer.
- Don't create a branch, worktree or clone to escape dirt or a conflict.
- Don't skip step 4 because the root `AGENTS.md` "probably covers it". The
  scoped files carry the rules that actually get violated.
- Don't fetch, browse, or hit GitHub when the task was scoped local-only.

## Output

Open with a short status block. Skip fields that are empty or normal — a
clean tree on `main` with no stranded work is one line, not five:

- checkout path, branch (or detached HEAD), base commit;
- dirty files and whether a live writer owns them;
- stranded branches or checkouts holding unlanded commits;
- workspace version and whether `check-versions.sh` agrees;
- which `AGENTS.md` files govern this change.
