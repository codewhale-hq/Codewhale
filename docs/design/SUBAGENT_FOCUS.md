# Subagent focus — one child, one conversation, one composer target

Status: implemented on `work/v098-agent-focus-20260815` (TUI + runtime
contract), on by default, no settings key. Its acceptance surface is the actual
terminal: focus a worker, send a follow-up, confirm that the composer target and
transcript agree, and return to the main conversation. Historical automated PTY
captures were removed because they froze layout geometry instead of protecting
the runtime contract.

## What Claude Code does (the reference behaviour)

Observed on Claude Code's terminal UI while it ran several forked subagents:

- Below the composer, an **agent list**: one row per agent — `main` first,
  then each child — shaped `<status dot> <kind> <live activity summary>
  <elapsed> · ↓ <tokens>` (e.g. `○ fork  Running fleet_setup unit tests
  25m 23s · ↓ 377.0k tokens`). The selected row carries a `❯` at the far left
  and a filled `●` dot; the others show `○`.
- The footer hint chain reads `esc to interrupt · ← for agents · ↓ to manage`
  in the same ` · ` dot-chain style as the rest of the shell.
- Selecting an agent switches the visible transcript to **that agent's full
  conversation**, scrollable like the main one.
- The composer grows a **chip on its top-right border** naming the targeted
  agent (its short description). Sending continues that agent with its context
  intact (`SendMessage`), whether it is still running or already stopped —
  a stopped agent is resumed on its own fork.
- Sending to a busy agent adds a trailing ` · 1 queued` counter (accent
  colour) on its row until the agent takes the message at its next turn
  boundary; the transcript keeps a small receipt line under the user's message
  (`Message queued for delivery to <agent> at its next tool round.`), and, when
  the permission classifier allowed something, a second line names it.
- `↓ to manage` opens a manager for the agents (stop, inspect, return).

## What Codewhale does now (default, no configuration)

Codewhale already had the row grammar (`crates/tui/src/tui/work_surface/`,
Agents panel: `<mark> <role> <status> <objective> … <elapsed> · ↓ <tokens>`)
and a bounded per-agent transcript pager. This lane replaces the pager with a
**focus** model and adds the composer target and the follow-up contract.

### Surfaces

| Surface | Behaviour |
| --- | --- |
| Agent list | The rail's Agents panel (`Alt+W`, `Alt+2`/`Alt+@`, or `←` on an empty composer). While a worker is focused every row gets a two-cell gutter and the focused row shows `❯` there. A running worker with follow-ups it has not yet taken shows ` · N queued` in the accent colour. |
| Focus | Enter on a row / click / Enter in `/agents` **focuses** the worker: its full transcript (durable artifact first, resident tail otherwise) is rendered with the ordinary history cells in the conversation area, headed by one line `● <name> · <status word>`. PageUp/PageDown/wheel scroll it exactly like the main transcript. `⌥V`/`Alt+V` opens that worker's Agent Details. |
| Composer | Chip `→ <name>` on the top-right border; empty hint `Message <name> · Esc returns to main`. Esc on an empty composer returns to the main conversation (rail selection is kept). |
| Footer | While workers exist the hint chain gains `← for agents · ↓ to manage` (ASCII-safe: `<- for agents · v to manage`), also on the settled `✓ done` strip. Words are `MessageId::FooterHintForAgents` / `FooterHintToManage`. |
| Manage | `↓` on an empty composer (or `/agents`) opens the register: `↑/↓ select · Enter focus · X stop · R refresh · F roster/setup · Esc close`. |
| Receipts | Sending while focused writes one system line in the **main** transcript — `Queued for <name>` — and echoes the message in the focused view until the child's own transcript carries it. The delivery outcome arrives as one line in the focused view: `Queued for <name>: it reads the message at its next round.`, `<name> had finished; continued on a new fork (<target>) …`, or `Could not deliver to <name>: <reason>`. Approval decisions keep Codewhale's existing approval receipts (Ask / Auto-Review / Full Access wording); no separate "classifier" line is invented. |

### Runtime contract (real work, not UI illusion)

- `Op::FollowUpSubAgent { agent_id, text }` (TUI → engine) →
  `SubAgentManager::continue_child_from_user`:
  - **Running** child: text goes to its live input channel and is folded into
    its next model round (`followup_child`). The manager counts it in
    `queued_follow_up_counts()` until the loop takes it (`SubAgentInput::
    mark_taken`), which is what the rail's ` · N queued` shows via
    `Event::AgentList { queued_follow_ups }`.
  - **Interrupted or Completed** child with a continuable checkpoint: resumed
    on a new agent id from the checkpoint plus the follow-up
    (`resume_from_checkpoint_with_policy(InterruptedOrCompleted)`); the
    terminal record stays an immutable receipt and `resume_targets` links the
    fork. Focus follows the fork. The model-facing `agents/followup` keeps its
    interrupted-only contract.
  - Retrying on an id whose `resume_targets` successor already exists: a
    Running successor takes the text on its live channel; a settled Completed
    successor answers from its existing result; a settled Failed / Cancelled /
    BudgetExhausted successor reports the follow-up as undelivered, naming the
    status.
  - **Failed / Cancelled / BudgetExhausted**: refused with the exact reason.
- `Event::SubAgentFollowUp { agent_id, outcome }` carries the receipt back.
- The engine builds the resume runtime from the installed session route
  (`Engine::off_turn_subagent_runtime`), so a continued fork inherits the
  session's provider, model, permissions posture, and denied tools.

### Keys

| Key | Where | Effect |
| --- | --- | --- |
| `←` | empty composer, workers exist | enter the agent list (rail Agents panel; `/agents` register when the rail is off) |
| `↓` | empty composer, workers exist | open the manage register |
| `↑`/`↓`, Enter | rail (focused) or register | select / focus a worker |
| `X` | register | stop the selected worker |
| Esc | empty composer while focused | back to the main conversation |
| `⌥V` / `Alt+V` | while focused | that worker's Agent Details |
| PageUp/PageDown, wheel | while focused | scroll the worker's transcript |

Tab is untouched: it never changes the message target.

### How this is the default

Nothing to enable. Any session with children gets the hints, the list, focus,
and follow-ups. The one-agent-one-destination rule from v0.9.7 still holds:
every activation of an agent row lands on the same place — now the in-place
focus rather than a modal pager. Rail placement/panel settings are unchanged
(`rail_panel`, `work_surface_*`); a rail set to `off` still reaches everything
through `←`/`↓` and `/agents`.

### Hook points left for parallel lanes

- Whale role badges: rail rows and the focus banner render the worker name
  through `agent_focus::agent_display_label`; a badge can be prefixed there
  without touching the focus logic.
- Compact tier (< 60 cols): the strip collapses; keep the focus banner and chip
  to one line each and inspect the current product when this surface changes.

### Not done / follow-ups

- No token counter for continued forks beyond what the runtime already
  reports per worker (no invented numbers).
- The focused view re-reads the child's transcript at ~400 ms; a push-based
  refresh from `SubAgentMailbox` events would be cheaper on very long chats.
- Re-focusing a worker after a session restart works from the durable
  artifact, but focus itself is not persisted in the session snapshot.
