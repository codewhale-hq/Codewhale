---
name: handoff
description: "Write a compact, decision-ready handoff so the next session (or the user) can continue without reconstructing the current one. Use when the session is ending, context is running low, the user asks for a handoff / 'pass the baton' / 'hand off', or a long-running operation needs a durable state checkpoint."
invocation: model+user
---

# Handoff

> Write a compact, decision-ready handoff so the next session (or the user)
> can continue without reconstructing the current one. Use when the session is
> ending, context is running low, the user asks for a handoff / "pass the
> baton" / "hand off", or a long-running operation needs a durable state
> checkpoint. The goal: the durable artifact survives, the context does not
> need to.

Invocation: `model+user`

## When to use

- The user says "handoff", "hand off", "pass the baton", "takeover prompt",
  "write me a handoff", or the session is about to end / compact.
- A long operation (multi-turn, multi-workstream) has state that must survive
  context loss: commits, branches, PRs, CI, blockers, decisions.
- You are switching to a fresh session and want the new session to start from
  evidence instead of reconstructing the old one.

## What to do

1. **Gather the truth from tools, not memory.** Run/collect:
   - `git branch --show-current`, `git status --short`, `git log --oneline
     origin/<default>..HEAD` (what is local-only), `git log --oneline -5`
     (recent context).
   - Live remote state where relevant (`gh pr list --state open`,
     `gh pr checks <n>`, `gh run list`) — only what the user's operation
     actually depends on; do not pad the handoff with a full GitHub dump.
   - Any in-flight work: dirty files, uncommitted slices, partial worktrees,
     running background jobs/workers, queued CI.
2. **Write a compact markdown handoff** (aim under ~60 lines; the user may
   also ask for a "short text-only" variant — then aim under ~15 lines):

   ```markdown
   # Handoff — <operation/session name> — <date>
   - **State:** <one-line: what is done vs in-flight vs blocked>
   - **Landed/committed:** <exact SHAs + one-line what>
   - **Branches/PRs:** <names + states; which are ours vs community>
   - **CI:** <what is green, what is waiting, what is broken>
   - **Blockers:** <exact blocker + what would unblock>
   - **Decisions made:** <the WHY that a fresh session must not re-litigate>
   - **Next step:** <the single next action, one line>
   - **Continuation records:** <pointers to partial work that must be
     preserved (worktrees, uncommitted files, receipts)>
   ```

3. **Persist it.** Always write `.codewhale/handoff.md` in the workspace — that
   is the only path the runtime reads back. On the next session's first turn it
   is injected as the "## Previous Session Relay" block
   (`HANDOFF_RELATIVE_PATH`, `crates/engine/src/prompts.rs:85`; loader at
   `prompts.rs:301-315`). A handoff written anywhere else is never picked up,
   so the next session starts cold no matter how good the note is.

   Optionally also write a human-discoverable copy:
   - If the workspace has an ops/notes convention (e.g. `codewhale-ops/notes/`
     with a living handoff file), update the living handoff's dated facts and
     snapshot, or create `<topic>-handoff-<date>.md` next to it.
   - Otherwise the repo root as `HANDOFF.md` or
     `docs/handoff/<topic>-<date>.md`. These are for people; nothing in the
     runtime reads them. Never overwrite someone else's uncommitted handoff
     without reading it first.
4. **Clear the way for the new session ("clears context").** A skill cannot
   delete the current context, but it can make the context disposable:
   - Ensure nothing is left only in memory: dirty work is either committed
     (WIP is fine with a real body), stashed with a note, or recorded in the
     handoff with its exact location.
   - Kill or record background work that would outlive the session
     (background jobs, sub-agents) — record what is still running and its
     task id.
   - Close with the one-line "next step" so the fresh session has an
     unambiguous first action.
5. **Deliver.** Give the user the compact handoff text in your reply
   (the persisted file is the durable copy; the reply is the readable one).

## Constraints

- Facts only from tool output; never invent SHAs, check states, or blockers.
- Keep claims narrower than evidence: distinguish landed/committed, verified
  locally, CI-verified, and pending.
- Preserve other people's uncommitted work: read before touching, archive
  before overwriting.
- The handoff is orientation, not law: tell the next session to refresh
  live state before acting on it.
- If the user asks for a "short text-only" handoff, give exactly that in the
  reply and skip the full markdown file unless asked.
