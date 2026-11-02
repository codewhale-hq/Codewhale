# Auto mode parity: Codewhale Auto-Review vs Claude Code auto mode vs Kimi Code

Status: design + gap ledger (2026-08-15). Source of truth for Codewhale
behavior is `docs/MODES.md`, `docs/AUTHORIZATION_ORDER.md`, and the code in
`crates/tui/src/tui/auto_review.rs`, `crates/engine/src/core/engine/reviewer.rs`,
and `crates/engine/src/core/engine/turn_loop.rs`. Statements about the other
two harnesses come from their public documentation as read on 2026-08-15 and
are quoted, not inferred.

## What Claude Code does (auto mode)

From `code.claude.com/docs/en/permission-modes` and `/auto-mode-config`:

- Modes: `default` (Manual), `acceptEdits`, `plan`, `auto`, `dontAsk`,
  `bypassPermissions`. "In auto mode, a second model, the classifier,
  reviews actions instead of you." On Pro/Max/Team plans "the built-in
  starting mode is auto mode." `Shift+Tab` cycles modes.
- Decision order: "1. Actions matching your allow, ask, or deny rules
  resolve immediately… 2. Read-only actions and file edits in your working
  directory are auto-approved, except writes to protected paths. 3.
  Everything else goes to the classifier… 4. If the classifier blocks,
  Claude receives the reason and tries an alternative. In most sessions the
  reason is the fixed text `Blocked by classifier`."
- "The classifier sees user messages, tool calls, and your CLAUDE.md
  content. Tool results are stripped."
- Explicit `permissions.ask` rules "always force a permission prompt, even
  in auto mode"; `permissions.deny` "blocks before the classifier is
  consulted."
- Broad allow rules (`Bash(*)`, wildcarded interpreters, `Agent`) are
  dropped on entering auto mode; narrow ones carry over;
  `autoMode.classifyAllShell` routes every shell command to the classifier.
- Trusted infrastructure and rule overrides are prose in
  `autoMode.environment/allow/soft_deny/hard_deny`; `claude auto-mode
  defaults|config|critique|reset` inspect them.
- Fallbacks: "A blocked action: Claude Code shows a notification and lists
  the action in `/permissions` under the Recently denied tab, where you can
  press `r` to retry it with a manual approval." "Repeated blocks: if the
  classifier blocks an action 3 times in a row or 20 times total, auto mode
  pauses and Claude Code resumes prompting." A mode switch during a pending
  check discards a verdict the new mode would not have requested.
- Subagents: the delegated task description is classified before spawn,
  each child action is classified with the parent's rules, and the child's
  full action history is reviewed on return (a warning is prepended when the
  review flags a concern or could not run). Messages sent to another agent
  with `SendMessage` are also classified before delivery.
- Boundaries stated in conversation ("don't push") are treated as block
  signals but are not stored as rules.
- Terminal UI conventions Hunter pointed at: agent rows show `· 1 queued`
  for a follow-up waiting on a busy subagent; the transcript shows `Message
  queued for delivery to <agent> at its next tool round.` and `Allowed by
  auto mode classifier` under a user message the classifier reviewed; the
  footer chain reads `… · esc to interrupt · ← for agents · ↓ to manage`.

## What Kimi Code does (0.34.0)

From `kimi --help` and `moonshotai.github.io/kimi-code` (llms-full):

- `--yolo` / `/yolo`: "Auto-approve regular tool calls; the agent may still
  ask questions." Plan-mode exit approval is not bypassed.
- `--auto` / `/auto`: "fully autonomous, the agent will not ask questions";
  "tool approvals are handled automatically". `--yolo` and `--auto` are
  mutually exclusive; `-p` (print mode) uses `auto` by default.
- `/permission` selects a permission mode; "always allow" rules accepted via
  `/permission` or an approval dialog propagate to every subagent; the
  `Agent` tool itself is allowed by default; each dispatch is presented as an
  approval request unless an allow rule or YOLO applies.
- `/tasks` browses background tasks (`TaskList` auto-allowed, `TaskStop`
  requires approval); Esc interrupts a turn and preserves partial output.
- Its Auto policy (per `docs/MODES.md`, at the pinned commit) applies deny
  rules and then approves; there is no model reviewer.

## What Codewhale does today (0.9.8 candidate)

- Postures (`Shift+Tab`, `/config approval_mode`): **Ask** (`suggest`),
  **Auto-Review** (`auto`), **Full Access** (`bypass`), plus `never`.
- Auto-Review = deterministic floor (configured block rules + built-in
  safety floor; allows proven-safe calls, hard-blocks publish-like and
  destructive background work) → fallback holds go to a one-shot model
  guardian (exact call + deterministic observations only; no transcript;
  90 s deadline; high/critical never auto-runs; any failure denies, fail
  closed). Repo-law holds that require a person block instead of opening a
  hidden modal. Ask rules force prompts in every posture. Full Access
  auto-approves non-bypassable registered holds instead of opening a modal.
- Every decision is written to `$CODEWHALE_HOME/audit.log`
  (`tool.auto_review` with `gate: deterministic|guardian`).
- Children inherit the parent posture; an explicit Full Access handoff
  keeps the child from prompting.
- `Esc` is a cancel stack (footer advertises it while working).

## Parity matrix

| Row | Claude Code auto | Kimi Code auto/yolo | Codewhale Auto-Review | Status |
| --- | --- | --- | --- | --- |
| What runs without asking | reads + working-dir edits by rule; rest via classifier | yolo: regular tool calls; auto: everything, no questions | proven-safe by deterministic floor; fallback holds via guardian | **parity** (different mechanism, same outcome class) |
| What always asks / never auto-runs | explicit `ask` rules; protected paths; org-`ask` connectors; `requiresUserInteraction` MCP | plan-mode exit | ask rules; safety-floor holds needing a person (denied, not hidden); high/critical guardian risk; repo law | **parity** — Codewhale denies rather than prompts in Auto-Review, by design (no hidden modal) |
| Denial UX | notification + `Blocked by classifier` reason to the model; `/permissions` → Recently denied, `r` retries | approval dialog / denial | tool error carries the reason to the model; **now** a one-line transcript receipt | **partial** → transcript receipt added here; recently-denied ledger + retry is a follow-up |
| Decision receipts | `Allowed by auto mode classifier` under classified messages | none documented | audit log only → **now** transcript notes for guardian allow/deny/unavailable, deterministic blocks, and held-without-pausing | **closed in this lane** |
| Allow/deny lists | `permissions.allow/ask/deny` + prose `autoMode.*` | `/permission` always-allow rules | `permissions.toml` ask rules (`/permissions list/remove`), configured block rules, execpolicy | **partial**: prose trusted-infrastructure config is deliberately absent (guardian sees only the exact call); `/permissions` now explains the posture and where receipts go |
| Escalation | classifier reasons; retry via `/permissions` | n/a | guardian returns rationale + "do not work around" | **parity** |
| Sandbox | sandbox network requests classified per host/port | n/a | sandbox modes incl. `external-sandbox`; DSH-grounded contract | **deliberately different** (sandbox is a separate layer, not the reviewer's job) |
| Interrupt | `esc to interrupt` in footer | Esc interrupts | `Esc` cancel stack; footer `Esc to interrupt` (now localized) | **parity** |
| Queued messages to a busy child | `· 1 queued`, delivery receipt, message classified | n/a | owned by the subagent-focus lane (`work/v098-agent-focus-20260815`) | **in progress (other lane)** |
| Subagent visibility / manage | agent rows, `← for agents · ↓ to manage`, `/tasks` | `/tasks` | `/fleet workers` (`/subagents`), work bar; rail + hints owned by the focus lane | **partial (other lane)** |
| Auto pause after repeated blocks | 3 in a row / 20 total pauses auto mode | n/a | none | **missing** — follow-up (see plan) |
| Child task classified before spawn / reviewed on return | yes | dispatch shown as approval | child inherits posture; no return review | **missing** — follow-up |
| Conversation boundaries as block signals | yes (not durable) | n/a | guardian never sees the transcript by design | **deliberately different** (durable rules only) |

## Changes landed in this lane

- `Event::ToolGateDecision` (engine → hosts) with `ToolGate`,
  `ToolGateVerdict`, and `bounded_gate_reason` (control/bidi stripped,
  ≤220 chars). Emitted for guardian Allow/Deny/Unavailable and deterministic
  Blocks. Proven-safe deterministic allows stay silent, like rule-based
  auto-approvals elsewhere.
- TUI: `crates/tui/src/tui/gate_receipts.rs` renders one localized line per
  decision (`Auto-Review allowed '<tool>' (<risk> risk, model guardian):
  <reason>` / `… denied …` / `… could not review … denied, fail closed` /
  `… blocked … (deterministic policy)`). Receipts are held until the tool's
  card completes so they land under the card (a mid-run insert splits the
  tool run); leftovers flush at turn end. The Auto-Review "held without
  pausing" case is now a localized transcript note as well as a status.
- `/permissions` output ends with the active posture and what it decides
  alone vs never, plus the audit-log path.
- Footer `Esc to interrupt` is localized (`FooterHintEscInterrupt`).
- 12 new `MessageId`s translated in all 15 shipped locale packs.

## Follow-up plan (not in this lane)

1. **Recently-denied ledger + retry**: keep the last N `ToolGateDecision`
   denials per session; `/permissions denied` lists them; a retry action
   re-issues the exact call under Ask (a person decides). Safety argument:
   retry never bypasses the deterministic floor or repo law; it only converts
   a guardian/hold denial into a visible prompt.
2. **Auto-Review pause after repeated denials** (3 in a row / 20 total, like
   Claude Code): switch the session to Ask with a status receipt; resuming is
   explicit. Thresholds constant, not configurable, until measured.
3. **Child dispatch review**: run the deterministic floor over an `agent`
   task description before spawn (publish-like/destructive intent → hold),
   and append the child's gate receipts to its return summary. No new
   classifier: reuse the guardian with the same exact-call contract.
4. **Message-to-child review**: when the focus lane lands follow-ups to a
   busy child, route them through the same guardian only if they contain
   tool-shaped instructions; otherwise deliver.
