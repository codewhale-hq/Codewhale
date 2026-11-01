---
name: cw-dogfood
description: "Use when a Codewhale change needs proving in the real product, or when asked to build/install/dogfood the local binaries: stamped release build, atomic install, fresh-shell verification, and the manual QA that gates cannot cover."
---

# cw-dogfood

Green gates prove the code compiles and asserts. They do not prove the product
works. Freezes, route contamination, focus theft, streaming cadence, and
approval-flow regressions all live in the runtime, where no unit test looks.
This stage puts the actual binary on your PATH and makes you use it.

Stage 4 of the loop: [cw-orient](../cw-orient/SKILL.md) →
[cw-slice](../cw-slice/SKILL.md) → [cw-gates](../cw-gates/SKILL.md) →
**dogfood** → [cw-land](../cw-land/SKILL.md) → [cw-handoff](../cw-handoff/SKILL.md).

## When to use

- The change is user-visible: TUI layout, motion, streaming, model or Fleet
  selection, approvals, commands, install paths.
- Before landing a release candidate, or before claiming a runtime behavior is
  fixed.
- The user asks to "install the build", "dogfood this", or "get this on my machine".

## Workflow

1. **Gate first.** Run [cw-gates](../cw-gates/SKILL.md) to the rung the change
   deserves. Never install an ungated build.

2. **Build stamped.** Release work follows `docs/RELEASE_CHECKLIST.md`.
   Local builds are unstamped (`(dev)`), and the
   installer refuses an unstamped binary on purpose — the stamp is what proves
   the thing on your PATH is the thing you just built:
   ```bash
   CODEWHALE_BUILD_SHA=$(git rev-parse HEAD) \
     scripts/dev-cargo.sh build --release --locked -p codewhale-cli --bin codewhale
   ```

3. **Install atomically.** Use the script; do not hand-copy:
   ```bash
   scripts/release/install-dogfood.sh          # defaults to target/release
   ```
   It refuses a dirty source tree (override deliberately with
   `CODEWHALE_ALLOW_DIRTY_DOGFOOD=1`, and then say so wherever you report the
   install), verifies the binary embeds current HEAD, installs `codewhale` and
   `codew` into `~/.cargo/bin` and `~/.local/bin` (override with
   `CODEWHALE_INSTALL_DIRS`), re-signs ad-hoc on macOS, and verifies resolution
   from a fresh login shell.

   **Never `cp` over a running binary.** On Apple Silicon that poisons the
   kernel's code-signature cache for the inode, and later execs hang until
   reboot. The installer does tmp-copy plus atomic `mv` for exactly this reason.

4. **Verify from a fresh shell, not this one.** A correct `target/release`
   binary and a stale `codew` on PATH is the classic false pass:
   ```bash
   zsh -lc 'type -a codew codewhale; codew --version'
   ```
   The version string must contain the short HEAD SHA you just built.

5. **Use the product.** Run it in a real terminal and exercise what you changed.
   `crates/tui/AGENTS.md` owns sizes, motion evidence, and environment caveats;
   judge motion against `docs/MOTION_CONTRACT.md`.

   Scenarios worth exercising when they are in scope:
   - **Liveness under fanout** (#3216, #2211) — spawn six workers; typing,
     render, cancel, and the roster stay live throughout, and Esc cancels
     mid-fanout (a prompt interrupt, not a wedged burst). For the Windows
     Terminal retest path (#3289), start in plan mode, add follow-up input,
     press Esc, switch to yolo/accept flow, trigger at least two auto/Fleet
     worker spawns, and keep typing, cancel and mode switches live for several
     minutes; attach logs if the freeze reproduces.
   - **Route isolation** (#3227) — multiple terminals on distinct provider/model routes,
     zero cross-terminal contamination, no provider+model mismatch.
   - **Running-turn input** (#3203) — during a busy turn, Enter queues a
     follow-up (the preview advertises Enter send now), an empty Enter promotes
     the oldest, Ctrl+Enter steers, Shift+Enter newlines, and Ctrl+G/Ctrl+S
     only stash drafts.
   - **Approvals** — ordinary tool approval vs. repository-law approval; the
     screen must name the repository constitution where it applies, and
     decorative motion must go still when the user owns the next action.
   - **Empty, narrow, and first-run states** — compact layouts remove chrome
     before content.

6. **Headless surfaces, when the change touches them.** `codewhale exec` is the
   one-shot worker path; `codewhale app-server` is the local control/API surface.
   They must agree about routing, permissions, and event states — a disagreement
   is a runtime bug, not a QA note.
   ```bash
   scripts/release/app-server-smoke.sh
   codewhale exec --auto --output-format stream-json --model <model> "Reply PONG"
   ```
   Provider calls spend tokens. Ask before running the paid ones.

7. **Record what you saw.** Dimensions, inputs, visible state, side effects. A
   screenshot proves layout and color; only live observation or a recording
   proves motion and continuity.

## Red flags / don't

- Don't `cp` a binary over a running one. Use `install-dogfood.sh`.
- Don't verify in the shell that already has the old binary resolved.
- Don't claim animation or cadence quality from a still image.
- Don't substitute a full-screen assertion harness for looking at and using the
  product.
- Don't install from a dirty tree without saying so in the report — the version
  stamp will not show the dirt.
- Don't delete `target/dogfood/*` bundles if the lane keeps them: they are
  release evidence.
- Don't spend provider tokens on smoke runs without approval.

## Output

- The exact build command, including the `CODEWHALE_BUILD_SHA` stamp.
- The installer's destinations and its fresh-shell verification result.
- `codew --version` from a fresh login shell, with the SHA visible.
- Per scenario: terminal size, what you did, what you observed — and which
  scenarios you did not exercise.
