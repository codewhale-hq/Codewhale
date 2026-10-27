---
name: cw-slice
description: "Use before writing code for any Codewhale feature, upgrade, or refactor: find the existing owner of the behavior, bound the change to one reviewable slice, and fix the evidence bar before you start."
---

# cw-slice

The expensive mistake in this repo is not a bad implementation — it is a second
implementation. A new `model_*`, `*_config`, `provider_*`, or "bridge" module
beside the one that already does the job ships two systems and a comment that is
no longer true. This skill is the ponytail ladder's rung 2 with commands
attached: **find the thing that already exists, then edit it.**

Stage 2 of the loop: [cw-orient](../cw-orient/SKILL.md) → **slice** →
[cw-gates](../cw-gates/SKILL.md) → [cw-dogfood](../cw-dogfood/SKILL.md) →
[cw-land](../cw-land/SKILL.md) → [cw-handoff](../cw-handoff/SKILL.md).

## When to use

- Any feature, upgrade, refactor, or "make X work like Y" request.
- Before creating a new module, trait, config struct, or command.
- When a plan or issue tells you to build something and you have not yet
  confirmed it does not already exist.

## Workflow

1. **Walk the ladder before opening an editor** (`AGENTS.md`, "The ponytail
   method" — not restated here, since a second copy is what rung 2 forbids).
   Stop at the first rung that answers. The ladder runs *after* reading the
   code, never instead of it: a short diff written without reading the call
   sites is a guess, not a small change. State the rung only when the choice
   isn't obvious from the diff itself.

2. **Grep for the predecessor.** This is the step that gets skipped and the one
   that costs the most:
   ```bash
   grep -rn "<the concept, in the words the code would use>" crates --include='*.rs' | head -40
   ls crates
   grep -rln 'model_\|_config\|provider_' crates/*/src --include='*.rs' | head -30
   ```
   Search behavior and symbols, not just filenames. If you find an owner, edit
   it. If you are still adding a new layer, its commit message must name the
   predecessor it replaces — otherwise you are editing the wrong file.

3. **Check the contracts you are about to walk into.** One of these is
   guard-tested, the rest are convention — either way, move them with their
   code, not around it:
   - One turn loop: `crates/tui/src/core/engine/turn_loop.rs`, guarded by
     `crates/core/tests/single_turn_loop.rs`. A second loop fails the guard;
     changing the shape means changing the guard with it.
   - One base prompt: `BASE_PROMPT` in `crates/tui/src/prompts/text.rs`.
   - The subagent tool is `agent`; `agent_open` / `agent_eval` /
     `agent_close` / `delegate_to_agent` are removed surfaces. If the shape
     must move, move the code and add the guard test that judges the new
     shape — the convention is not a fence around the area.
   - The system prompt + tool catalog are a session-pinned KV-cache prefix
     (`docs/CACHE.md`). Any new session-context contributor must state its cache
     effect — frozen prefix vs. append-only history. Never splice a volatile
     fact into the prefix.
   - `crates/tui/src/core/` is a module inside the TUI crate. `crates/core` is a
     different crate that runs no turns. Do not confuse them.
   - Repeatedly misidentified as dead, verify consumers before removing:
     `runtime/src/context_budget.rs`,
     `runtime/src/prompt_zones.rs`, `tui/src/tools/remember.rs`, `config/src/route/`.

4. **Read the scoped guidance for the files you will touch.** `crates/tui/AGENTS.md`
   owns the UI contracts (one owner per fact, `codewhale_palette::grammar` semantics, typed
   state enums, toast routing, `tr(locale, MessageId::...)` for user prose).
   `crates/localization/locales/AGENTS.md` owns string changes. `web/AGENTS.md` owns the
   site. `docs/MOTION_CONTRACT.md` owns motion. Design law lives in
   `docs/design/`, not in a prototype file someone left in a sibling directory.

5. **Bound the slice.** One coherent change, reviewable in one sitting, that
   leaves the tree building and green. `AGENTS.md`'s two corollaries keep
   slices honest: an abstraction must delete caller code, and a migration
   ships its last consumer or does not start.

6. **Fix the evidence bar now, not after.** Decide before writing code what will
   prove this works, and write it into your plan:
   - the focused test or existing check that covers the behavior
     (`scripts/dev-test.sh --list` maps an area to its fastest invocation);
   - whether the change is visible enough to need [cw-dogfood](../cw-dogfood/SKILL.md);
   - whether it is cross-cutting enough to need the full sweep in
     [cw-gates](../cw-gates/SKILL.md).

7. **Write the implementation, then prove it runs.** No new tests or code
   comments unless explicitly asked (`AGENTS.md`); update or delete existing
   tests the change makes wrong.

## Red flags / don't

- Don't add a module that "bridges", "mirrors", "stages", or "wraps" something
  that already exists without naming that thing in the commit message.
- Don't fork a singleton (turn loop, base prompt, delegation axis,
  lifecycle system) without moving its guard test and consumers with it.
  The repo has one of each on purpose; a silent second one is the failure
  mode, not the refactor.
- Don't write new tests unless explicitly asked, and never first. Don't add
  new code comments.
- Don't contort production code to keep a brittle assertion green. A test that
  only encodes old behavior is evidence, not a veto: change it with the code.
- Don't cut trust-boundary validation, data-loss handling, security, or
  accessibility to make a diff shorter. Brevity is never a reason to drop a guard.
- Don't leave a `#[allow(dead_code)]` behind as the cost of an incomplete
  migration — `scripts/check-dead-code-budget.py` is the running receipt.

## Output

Default shape before the first edit — compress when trivial (a one-line
change gets a one-line note, not four bullets):

- which rung of the ladder you stopped at and why;
- the existing owner you found (`path/to/file.rs:line`), or the predecessor
  your new module names;
- the bounded slice, in one sentence;
- the evidence bar you will meet, chosen in advance.
