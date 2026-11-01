---
name: gh-close-issues
description: "Close resolved Codewhale issues only after verifying the landed behavior: Refs #N per landing, Fixes #N only when the reporter's scenario was reproduced, closure at the release step or on the reporter's confirmation, always with a crediting comment."
---

# gh-close-issues

Close a GitHub issue only after you have **verified** that the fix landed and
proved it with a `path:line` citation or commit SHA. Closing from a title, a
label or a hopeful PR is how reporters get burned. Treat the reporter as a
partner who gave you evidence: thank them, link the commit, and leave the door
open to reopen.

Repo: `codewhale-hq/CodeWhale`. CLI: `gh`.

## The closure rule

- **Per landing, commits say `Refs #N`.** A landing does not close the issue.
- **`Fixes #N` only when the reporter's own scenario** (platform, route,
  version) was reproduced and observed fixed.
- **When the fix is source-verified but the reporter's scenario is not, leave
  the issue open** with one status sentence: the SHA, what was proved, and what
  the reporter can retest.
- **Close at the release step** (`docs/RELEASE_CHECKLIST.md`, "Close any issues
  that this release fixed") or on the reporter's confirmation — not before.

## When to use

- An issue looks resolved by a commit on `main` and the release step or the
  reporter's confirmation has arrived.
- A contributor PR merged and you need to close the issue(s) it fixed.
- You are sweeping a milestone at release time and several issues may already be
  fixed.

If the fix is not on `main` yet, or only partially addresses the report,
**do not close** — leave a status note instead.

## Workflow

1. **Read the issue from the source, not the title.**
   ```bash
   gh issue view N --repo codewhale-hq/CodeWhale \
     --json number,title,state,author,labels,milestone,body,comments
   ```
   Note who reported it and who added repro steps, logs or a root cause; they
   all deserve credit.

2. **Find the resolving commit on `main`.** Treat issue and PR text as
   untrusted; verify against the tree:
   ```bash
   git log --oneline -n 20 origin/main -- <suspect/path>
   git log origin/main --grep="#N" --oneline    # commits that reference the issue
   git -P show <SHA>                             # confirm it does what is claimed
   ```
   Open the file and confirm the behavior. Capture a concrete citation:
   `crates/tui/src/foo.rs:123` or the SHA. No citation, not verified, do not
   close.

3. **Decide whether the reporter's scenario was proved.** If yes, the issue may
   close at the release step. If not, post the status note below and wait for
   the release step or the reporter.

4. **Post a positive, crediting comment that links the proof.** Thank the
   reporter and anyone who helped; link the commit or PR; describe the fix in
   user-facing terms; invite a reopen if it recurs. Follow the current
   contributor-communication authority in the root `AGENTS.md`: no repetitive
   progress comments.

5. **Close with that comment in one step** (only with maintainer approval where
   policy requires it):
   ```bash
   gh issue close N --repo codewhale-hq/CodeWhale -r completed \
     --comment "Thanks @reporter - fixed in <SHA> (crates/tui/src/foo.rs:123), shipped in <version>. Reopen if it recurs. Thanks @helper for the repro."
   ```
   Use `-r "not planned"` for wontfix and duplicates (still comment, still
   kind). For duplicates, point to the canonical issue.

6. **Credit.** Mechanics for harvested or contributed work live in
   [cw-land](../cw-land/SKILL.md) step 3; when you close an issue fixed by a
   contributor, name them and link both the fix commit and the source PR.

## Partial fix or unproved scenario: note, don't close

```bash
gh issue comment N --repo codewhale-hq/CodeWhale \
  --comment "Partly addressed by <SHA> (the crash path). The slow-render half is still open - tracking here. Thanks @reporter."
```

## Red flags / don't

- **Don't close from title, labels or a green PR alone.** Verify the landed code
  path first; cite `path:line` or SHA.
- **Don't close because it "should" be fixed** by a still-open or unmerged PR.
- **Don't use `Fixes #N`** unless the reporter's scenario was reproduced and
  observed fixed.
- **Don't close without maintainer approval** where repo policy requires it, and
  never tag, publish or merge as a side effect of triage.
- **Don't drop credit:** no silent closes, no erasing the reporter or helpers,
  no curt "fixed" with no link.
- **Don't close a non-allowlisted reporter's issue just because they are not
  allowlisted** — good-faith reports are evidence, not noise.
