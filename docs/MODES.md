# Modes and Permission Postures

> 阅读简体中文版：[zh_hans/MODES.md](zh_hans/MODES.md)

Codewhale has three related concepts:

- **TUI mode**: what kind of visible interaction you're in (Plan/Work/Operate).
- **Permission posture**: how aggressively the UI asks before executing tools.
- **Workflow**: named steps that coordinate sub-agents, with dependencies and
  results, available in any TUI mode.

Model selection is separate. `--model auto` and `/model auto` route each turn to
a concrete model and thinking level; they are not TUI modes and are not part of
the `Tab` cycle.

Workflow runs named steps through the same sub-agent runtime; Fleet manages
their saved roles and model assignments. A workflow adds ordering, result
handoffs, gates and a progress view. The active mode and permission posture
still control what each step may execute.

Before assigning steps, query `agent(action="roster")` for the saved Fleet
models and roles. A plan child can select a listed model with `model`, or use
the saved `role`/`profile` assignment. An Exact Fleet fixes each member's route.

## TUI Modes

Press `Tab` to complete composer menus or cycle through the visible modes
when the composer is empty: **Plan → Work → Operate → Plan**. `Tab` never sends
or queues composer text; use `Enter` to send or queue it.
Press `Shift+Tab` to cycle permission posture (Ask → Auto-Review → Full Access).
Every change of effective permission writes a `Permissions changed to …` line
in the transcript.
Press `Ctrl+T` to cycle reasoning effort.
Run `/mode` to open the mode picker, or switch directly with `/mode work`,
`/mode plan`, or `/mode operate`.

- **Plan**: design-first prompting. The stable primitive names remain familiar, but the runtime centrally refuses file mutation and shell execution. Read-only inspection and policy-allowed research, including deferred Web search/fetch, remain available. When a Plan turn successfully finishes a nonempty response, including a prose-only plan, the TUI asks how to continue: Work (Ask), Work (Auto-Review), or keep planning. Work sends the exact approved response as a normal follow-up and preserves that turn's existing To-dos, or adds one pending plan item without replacing unrelated work.
- **Work** (internally `agent`): ordinary multi-step execution. The first-turn toolbox includes `read`, `write`, `edit`, `file_search`, `bash`, `todo_write`, `load_skill`, and `task_shell_wait`, plus `create_goal` and the synthetic `tool_search`; `agent`, `workflow`, `get_goal`, and `update_goal` load through discovery (`get_goal`/`update_goal` ride eager while a goal is active). The agent calls `create_goal` when a request is a durable objective; the host never infers a goal from wording, and the contract must not tell the model otherwise. Approval, sandbox, repository law, and managed policy decide what may execute.
- **Operate**: Work at full strength, the "ultra" mode. Same tools and authority as Work; the difference is how they are used. Each substantive request becomes a goal (`create_goal`), the plan stays visible (`todo_write`), multi-part work runs through `workflow` and parallel `agent` workers, non-trivial changes get an independent reviewer before they are called done, long commands keep running in the background, and recurring or watch-type work is offered as an `automation` (created after approval). It keeps going until the goal is verified, blocked on you, or paused.

`Act` and `/mode act` remain compatibility aliases for Work. Saved settings
still normalize to the internal value `agent`.

### Tool availability by mode

| Tool family | Plan | Work | Operate |
|:---|:---:|:---:|:---:|
| `read` and policy-allowed deferred research tools | yes | yes | yes |
| `write` and `edit` | visible names; execution denied | approval- and policy-gated | same as Work |
| `bash` | visible name; execution denied | approval- and policy-gated | same as Work; delegation is preferred when parallelism or isolation helps |
| `agent` | yes, subject to child-depth authority | yes, subject to child-depth authority | yes, subject to child-depth authority |
| Deferred native, MCP, and plugin tools | discoverable through `tool_search` when policy permits | same | same |
| Paid or external-service tools | follows permission posture | follows permission posture | follows permission posture |
| Access outside the workspace root | explicit trusted paths only | only through trusted paths or trust mode | same trusted-path/trust policy as Work; fleet profiles never widen it |

Operate changes scheduling emphasis, not authority. It neither adds a
mode-specific tool denial nor bypasses the active approval, sandbox, shell,
ask-rule, repository-law, or managed-policy boundary. Plan remains the
mode-specific execution boundary for shell and write-capable tools; that
authority difference does not require a different primitive vocabulary.

### Operate loop (one screen)

```text
User message
  → small / chat / one-file?  → parent does it (Work-equivalent tools)
  → multi-step work? → goal → named steps + dependencies + completion checks
       → Workflow phases → independent sub-agents in parallel
       → collect results → check evidence → hand off to the next phase
       → missing required result? → stop dependent work and repair the step
       → one independent task? → one direct sub-agent
  → parent integrates results and reports completed, blocked and next steps
```

Lifecycle claims stay exact: dispatched ≠ settled ≠ verified.

`allow_shell` controls whether `bash` can execute; it does not rename the tool
or make mode the approval authority. Durable tasks and automation keep
conservative omitted-field defaults and receive shell authority only when their
settings explicitly grant it. Stateful terminal/background controls are
specialized deferred tools rather than fields on the small foreground `bash`
schema. Full Access changes the permission posture: the deterministic
Auto-Review floor applies in the reviewing postures and is skipped under Full
Access, while repository-policy holds remain authoritative.

Action-capable modes can discover the deferred `rlm` family through
`tool_search`; its `open`, `eval`, `configure`, and `close` actions own persistent
RLM sessions. The legacy split `rlm_*` spellings remain replay-only aliases.
Inside an RLM Python REPL, `sub_query_batch` fans out 1-16 cheap parallel child
calls pinned to `deepseek-v4-flash`.

The fast `deepseek-v4-flash` / thinking-off path is called Fin in the product
language. Fin is a seam for routing, summaries, cheap child calls, and
coordination work; it does not change approval behavior.

The orchestration controls remain available without taking over the starting
screen: `/auto` turns on Auto-Review so the agent just works, `/goal` keeps one
objective across turns, and `/workflow` prepares a repeatable ordered or
fan-out workflow. They are directly callable and searchable through the full
command palette, but are not pinned to the starter slash menu, idle welcome,
footer, or default Hotbar. A bare `/` instead opens the small task-oriented
starter set; use `/help` or the command palette for the complete inventory.

`/goal <objective>` sets a session objective with an optional token budget and
keeps active objectives visible as Work context. The agent may also create the
goal itself when a direct request describes a verifiable end state that will
take more than one turn ("until the tests pass", "make X work end to end"); it
then shows one receipt line and you can `/goal pause` or `/goal clear` it. Bare
`/goal` shows progress (state, elapsed, continuations, and how to continue when
no turn is running); with no goal and no conversation yet it prints usage.
`/goal pause` stops goal continuation without changing the objective, `/goal
resume` resumes and sends the objective back into the turn, `/goal complete`
marks it done, `/goal blocked` marks it blocked, and `/goal clear` removes it. Goal state does not change the active TUI mode,
permission posture, or model route. This remains distinct from `--model auto`, which
only controls model and thinking selection.

Workflow builds on the same separation: a goal can ask the agent to keep
working, while Workflow supplies the repeatable workflow/progress surface for
large fanout. In the UI, a Workflow run should be shown as an overlay on the
main screen, not as another mode beside Plan, Work, and Operate.

App-server clients can persist a thread-scoped goal with `thread/goal/set`, read
it with `thread/goal/get`, and clear it with `thread/goal/clear`. That persisted
record carries `active`, `paused`, `blocked`, `usage_limited`, `budget_limited`,
or `complete` status plus token/time accounting fields for clients that need
thread resume semantics.

## Mode Persistence

Choosing a mode interactively also sets the mode a fresh session starts in.
Tab/Shift+Tab cycling, the `Alt+A` / `Alt+P` / `Alt+Y` shortcuts, the hotbar's
Plan/Work/Operate actions, and `/mode` all write `default_mode` to
`~/.codewhale/settings.toml`, so switching to Operate survives a restart. The
write happens off the event loop; if it fails, the TUI says so in a warning
toast rather than reverting silently on the next launch.

Mode, thinking level, and the model picker share one serialized writer, so the
selection you made last is the one on disk — a burst of Tab presses cannot end
up persisting whichever write happened to finish last — and a mode write never
rolls back an unrelated key such as `default_model`.

Two paths deliberately do **not** rewrite the startup default: restoring a saved
session (which re-installs the mode that session was in) and a mode change
refused because a turn is in flight. The legacy `yolo` entry point installs Work
plus Full Access, and `agent` is what it persists — `yolo` is a permission
alias, never a startup mode.

Re-selecting the mode you are already in is not a no-op. After a restored
session the live mode and `default_mode` routinely disagree, so choosing the
live mode again is how you make it durable; Codewhale confirms with a
"saved as startup default" receipt rather than reporting "already in that mode".

While a turn is running, every change to the live route is refused — mode,
model, thinking level, and provider — no matter which surface you use. That
now includes the slash surfaces (`/mode`, `/model`, `/provider`,
`/config <key> <value>`, `/config preset`), which are reachable mid-turn. Press
Esc to interrupt first. The restart-only `default_mode` key is exempt, because
it does not touch the running turn.

Codewhale writes `settings.toml` under a lock that spans processes, and replaces
the file atomically, so a second Codewhale instance on the same home directory
cannot lose your selection or read a half-written file. At exit, queued writes
are flushed before the terminal is restored; anything that failed is printed on
the way out instead of disappearing with the alternate screen.

## Compatibility Notes

- Older settings files with `default_mode = "normal"` still load as `agent`; saving rewrites the normalized value.

## Escape Key Behavior

`Esc` is a cancel stack, not a mode switch.

- Close slash menus or transient UI first.
- Cancel the active request if a turn is running.
- Discard a queued draft if the composer is empty.
- Clear the current input if text is present.
- Otherwise it is a no-op.

## Permission Posture

Permission posture controls tool approval and whether a turn may pause for a
missing user decision. It is one layer of the full
[authorization order](AUTHORIZATION_ORDER.md), not a bypass for tool admission,
repository law, or sandbox enforcement. Cycle it with `Shift+Tab`, or edit it
at runtime:

```text
/config
# edit the approval_mode row to: suggest | auto | never
```

Legacy note: `/set approval_mode ...` was retired in favor of `/config`.

- `suggest` (**Ask**, default): tool approvals may interrupt, and Codewhale asks
  when an unresolved user choice materially changes authority, cost, scope, or
  outcome.
- `auto` (**Auto-Review**): reviews tool calls automatically. Deliberate user
  questions remain available in interactive sessions through `request_user_input`;
  a question parks the turn until answered, canceled, or its configured timeout
  expires. Headless `exec` withholds that tool because it has no responder.
  Tool safety holds remain separate from user questions. Two layers decide approvals. The
  **deterministic floor** (configured block rules plus the built-in safety
  floor) allows proven-safe calls and hard-blocks publish-like actions and
  destructive background/headless work; it is never model-reviewed. Fallback
  holds — calls the deterministic engine could not prove safe — escalate to a
  one-shot **model guardian** (v0.9.8) that returns risk, allow/deny, and a
  rationale. The guardian sees the exact held call and deterministic
  observations in separate JSON fields; conversation history, skill
  instructions, attachments, and expanded model context are excluded. It does
  not infer user intent or compute a generic user-intent score.
  High or critical risk cannot auto-run even if the model says allow. It has no
  tools, remembers no rules, and denies rather than truncates an oversized
  exact call. Exactly one reviewer request is made; incomplete or malformed
  output, timeout, cancellation, or provider failure fails closed. Repeated
  denials stop the turn: after three guardian denials in a row, or ten among
  the turn's last fifty reviews, the turn ends as interrupted with a status
  line, and calls still in that batch are refused without another review.
  Only explicit denials count (not timeouts or failures), an allowed review
  resets the run of three, the count starts fresh with each turn, and a child
  agent counts its own turns. Headless
  adapters use the deterministic-only tier. Repo-law
  holds that explicitly require a person block in Auto-Review rather than
  opening a hidden approval modal.

The LLM reviewer is closest to OpenAI Codex's experimental Auto-Review at
commit [`6fc6b9d6d2580d62622fc9884b5f5707f6505a5e`](https://github.com/openai/codex/tree/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e).
Codex's [guardian entry point](https://github.com/openai/codex/blob/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e/codex-rs/core/src/guardian/mod.rs)
reconstructs conversation context and runs a dedicated review session.
Codewhale deliberately adopts only the exact-action structured decision,
90-second deadline, fail-closed result, and per-turn denial circuit breaker. It
does not copy Codex's transcript reconstruction, user-authorization score,
reviewer tools, retries, persistent review session, or denial ledger.

Kimi Code at commit
[`1414d4602898f406e540b23342cb18db23ff9efc`](https://github.com/MoonshotAI/kimi-code/tree/1414d4602898f406e540b23342cb18db23ff9efc)
also has no LLM reviewer. Its ordered
[permission policy](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/permissionPolicyService.ts)
applies explicit deny rules and then its
[Auto policy](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/policies/auto-mode-approve.ts)
returns `approve` directly. Codewhale uses the deterministic floor and guardian
described above, while keeping deliberate user questions available.

The sandbox and escalation baseline is grounded in DeepSeek Harness
`0.1.0-rc.5` at
commit [`47f943859bef60e4160492346772ded9b24f765a`](https://github.com/deepseek-ai/deepseek-harness/tree/47f943859bef60e4160492346772ded9b24f765a):
its [sandbox contract](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/sandbox.md)
defines per-call `read-only`, `workspace-write`, and `danger-full-access`
boundaries and forbids silent unconfined fallback; its
[approval contract](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/approval.md)
grants only `allowed-once` and fails closed on rejection, cancellation, or an
unavailable answerer; and its
[sandbox result contract](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/shell/bash-sandbox/README.md)
tells the model to retry a denied command exactly once with the narrowest wider
mode plus a justification. DeepSeek Harness does not add an LLM reviewer to
that path. Codewhale's autonomous posture adds only the single stateless
guardian request described above; deterministic hard blocks remain
non-bypassable in the reviewing postures.
- `bypass` (**Full Access**): ordinary tool calls do not show approval prompts,
  while deliberate user questions remain available. The deterministic
  Auto-Review floor is skipped — publish-like commands, destructive
  background/headless work, and the Windows npm-launcher platform hold do not
  strand a call the person already granted. Non-bypassable registered holds
  auto-approve instead of opening a contradictory modal. Repository-law and
  managed-policy holds fail closed as hard blocks instead of contradicting
  Full Access with an approval modal.
- `never`: blocks any tool that is not considered safe/read-only; deliberate
  user questions remain available.

The effective posture and its question discipline are projected into every
turn from the same runtime authority that gates tools. A mode/posture change is
therefore visible to the next turn. Untrusted runtime-generated input is
narrowed before metadata is built and cannot invent approval authority. An
explicit Full Access sub-agent handoff preserves the parent's standing posture
so ordinary child work does not begin prompting again.

### Children (sub-agents and fleet workers)

Children inherit the session posture faithfully rather than a bare
auto-approve bit:

- **Auto-Review**: a worker's held call goes through the same deterministic
  policy (proven-safe calls run; publish-like and destructive background work
  is hard-blocked) and, for holds it cannot prove safe, the same one-shot
  model guardian using the child's own session client. No prompt is ever
  opened for a child; an unavailable guardian denies, fail closed.
- **Ask**: a call the role may delegate runs. A held call is raised as an
  approval prompt in the parent's UI (`agent:<id>:approval:<n>`) when the
  host is an interactive TUI; the worker waits visibly (`waiting for user`)
  and the person's answer is routed back to it, whether the parent turn is
  idle or itself awaiting an approval. Hosts that cannot prompt deny with the
  reason.
- **Full Access**: calls run, including destructive detached work — the
  Auto-Review floor is skipped exactly as it is for the parent turn. The
  execution envelope still applies and never widens.

Role posture and the execution envelope are checked before and after this
gate and never widen. Every decision a person did not make at a prompt is
written to the audit log and to the child's transcript as a one-line note
(`Auto-Review allowed 'bash' (low risk, model guardian): …`), visible when
the worker is focused.

## Small-Screen Status Behavior

When terminal height is constrained, the status area compacts first so header/chat/composer/footer remain visible:

- Loading and queued status rows are budgeted by available height.
- Queued previews collapse to compact summaries when full previews do not fit.
- `/queue` workflows remain available; compact status only affects rendering density.

## Workspace Boundary and Trust Mode

By default, file tools are restricted to the `--workspace` directory. Enable trust mode to allow file access outside the workspace:

```text
/trust on
```

Bare `/trust` (like `/trust status`) only *reports* the current setting — it
does not enable anything. Use `/trust off` to restrict access again.

Full Access enables trust mode automatically.

## MCP Behavior

MCP tools are exposed as `mcp_<server>_<tool>` and use the same approval flow as
built-in tools. Read-only MCP helpers may auto-run in Ask and Auto-Review when
policy permits; MCP tools with possible side effects require approval. Full
Access does not bypass hard policy holds.

A tool's own MCP annotations count only as far as their source is trusted. A
tool from a reviewed, enabled plugin that declares `readOnlyHint: true` runs
like the built-in read helpers, with no prompt; the same claim from any other
server is ignored. A tool that declares `destructiveHint: true` never gets
that relaxation, even from a reviewed plugin, and its approval card says the
server marked it destructive. Full Access still runs it without a prompt, like
any other tool that would ask.

See `MCP.md`.

## Related CLI Flags

Run `codewhale --help` for the canonical list. Common flags:

- `-p, --prompt <TEXT>`: one-shot prompt mode (prints and exits)
- `codewhale exec --auto --output-format stream-json <PROMPT>`: run the tool-backed non-interactive agent and emit one JSON object per line for harnesses and backend wrappers. `--auto` enables Full Access by default (no Codewhale shell sandbox, trust mode on); add `--sandbox workspace-write` to keep the shell sandbox and file-tool workspace restrictions, with trust mode off. Any `--sandbox` other than `danger-full-access` (or the equivalent `sandbox_mode` in config) keeps trust mode off; `external-sandbox` relies on your own isolation. Exec runs that offer tools are saved in every output format. Exit codes: `0` on success, `1` for genuine task/agent failures, `75` (`EX_TEMPFAIL`) when the turn ended on a retryable infrastructure failure (provider/transport `network`/`timeout` after all in-session retries) so harnesses can tell a retryable infra exit apart from a task failure; the terminal stream `metadata` event's `error_category` carries the same classification
- Put exec flags before the prompt. A prompt starting with `-` needs `--` first, for example `codewhale exec -- "- explain this option"`. A lone `-` is literal text. Known exec long flags (including `--flag=value`) after the prompt are rejected unless they follow an explicit `--`, which marks them as literal prompt text.
- `codewhale exec --prompt-file <PATH>` / `cat prompt.txt | codewhale exec --prompt-file -`: read the prompt from a file or stdin instead of argv, for prompts past the OS per-argument limit (~128 KiB on Linux). Conflicts with a positional prompt, and a positional `-` is literal prompt text; `--prompt-file -` is refused with `--parent-death-watch`, which owns stdin
- `codewhale exec --resume <ID|PREFIX> <PROMPT>` / `--session-id <ID|PREFIX>`: continue a saved session non-interactively
- `codewhale exec --continue <PROMPT>`: continue the most recent saved session for this workspace non-interactively
- `codewhale fork <ID|PREFIX>` / `codewhale fork --last`: copy a saved session into a new sibling session; forked sessions retain additive parent-session metadata and show that lineage in session listings
- `--model <MODEL>`: when using the `codewhale` facade, forward a DeepSeek model override to the TUI
- `--workspace <DIR>`: workspace root for file tools
- `-r, --resume <ID|PREFIX|latest>`: resume a saved session
- `-c, --continue`: resume the most recent session in this workspace
- `--max-subagents <N>`: clamp to `1..=128`
- `--mouse-capture` / `--no-mouse-capture`: opt in or out of internal mouse scrolling, transcript selection, right-click context actions, and transcript scrollbar dragging. Mouse capture is enabled by default on non-Windows terminals and on Windows Terminal/ConEmu/Cmder so drag selection copies only transcript text, removes visual wrap-column line breaks from paragraphs, and stays scoped to the transcript pane; hold Shift while dragging or use `--no-mouse-capture` for raw terminal selection. It defaults off on legacy Windows console (CMD without `WT_SESSION` / `ConEmuPID`) and inside JetBrains JediTerm — PyCharm/IDEA/CLion/etc. — where the terminal advertises mouse support but forwards SGR mouse events as raw text (#878, #898). Use `--mouse-capture` to opt in anywhere it's defaulted off. Raw terminal selection may cross the right workbar and include visual wraps because the terminal, not the TUI, owns the selection.
- `--profile <NAME>`: select config profile
- `--config <PATH>`: config file path
- `-v, --verbose`: verbose logging

## Branching and Rollback

Codewhale has three related but intentionally separate recovery paths:

- `codewhale fork <ID>` creates a new saved session from an existing saved
  conversation and records the source session id. This is the safe way to
  explore a different answer path without overwriting the original session.
- Esc-Esc backtrack rewinds the live transcript to a previous user prompt and
  restores that prompt into the composer for editing. It does not change
  files; `/restore` rolls the folder back to a chosen restore point and needs
  `/trust on` or Full Access.
- `/restore` and the `revert_turn` tool restore workspace files from side-git
  snapshots. `/restore list [N]` lists more snapshot options before choosing a
  rollback point. They do not rewrite conversation history.

A Pi-style in-file tree browser is a larger UI/data-model project. v0.8.40
ships the bounded fork/backtrack primitives and explicit lineage metadata.
