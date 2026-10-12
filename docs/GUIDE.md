# Codewhale User Guide

This guide is for your first hour with Codewhale. It explains the main
workflow, the important safety controls, and where to go next when you need a
complete reference.

Codewhale has deeper reference documents for installation, configuration,
providers, modes, keybindings, tools, and operations. Use this page as a guided
walkthrough, then follow the "Next" links when you need every option.

## 1. Welcome to Codewhale

Codewhale is a terminal coding agent. You run it from a workspace, give it a
task, and it can use structured tools to inspect files, run commands, edit
code, and report back with evidence.

The important difference from a normal chat model is that Codewhale is built
around a harness:

- It keeps the active workspace and session visible.
- It routes each turn through explicit modes and approval rules.
- It shows tool calls in the transcript instead of hiding the work.
- It can preserve sessions, fork conversations, and continue later.
- It can run sub-agents for focused background work.

You can use Codewhale for small questions:

```text
Explain the authentication flow in this repository.
```

You can also use it for multi-step work:

```text
Find the failing validation path, propose a fix, and wait for my approval
before editing files.
```

For a new repository, start conservatively. Ask Codewhale to explore and plan
before asking it to change files. That gives you a reviewable path and makes it
easier to catch wrong assumptions early.

Next: [ARCHITECTURE.md](ARCHITECTURE.md) explains the internal harness and
runtime model.

## 2. First Launch

For a new macOS or Linux installation, use the official GitHub release.
The installer verifies the release checksums and provides the same runtime
under the `codewhale` and `codew` command names:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Windows users should choose the matching installer or archive from
[GitHub Releases](https://github.com/codewhale-hq/CodeWhale/releases/latest).
For an existing direct install, use `codewhale update --check`, then
`codewhale update`. npm and Cargo remain secondary packaging routes; Cargo
also supports source builds where a compatible prebuilt is unavailable.
For occupied directories, package-managed installs, and PATH setup, follow
[the installation and migration guide](INSTALL.md#recommended-official-github-releases).
Android/Termux uses its own [preview archive or source-build path](INSTALL.md#android--termux-arm64).

Docker is also available when you want an isolated runtime. The published
image is currently 0.10.0; to test current `main`, [build it from source](DOCKER.md#building-locally).

```bash
docker volume create codewhale-home
docker run --rm -it \
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \
  -v codewhale-home:/home/codewhale/.codewhale \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/hmbown/codewhale:latest
```

Once the install directory is on PATH, launch Codewhale from the repository or
directory you want it to work in:

```bash
codewhale
```

For the default GitHub installer destination, you can use
`"$HOME/.local/bin/codewhale"` until that directory is on PATH.

On first launch, Codewhale opens the composer, even with no key or route.
With no key or route, workspace trust is asked once a provider is connected;
otherwise only when the folder requires a decision. A provider is asked for
when you send your first message: the message stays in the box while you sign
in or use your own key, and a local model is offered only when a local Ollama
server answers. The provider step also has an explicit offline route.

Everything optional stays available after that. Use `/setup` for the
progressive setup and repair guide, `/settings` for the full typed editor, and
`/constitution` when you want your own constitution on top of the bundled one.
The localized telemetry choice appears only after the workspace is ready and
does not block the composer.

DeepSeek is the default provider. If you want to configure its key before or
after the first launch, the most direct setup path is:

```bash
codewhale auth set --provider deepseek
```

You can also provide a key through the environment:

```bash
export DEEPSEEK_API_KEY="your-key"
codewhale
```

New Codewhale config is stored under `~/.codewhale/config.toml`. Legacy
`~/.deepseek/config.toml` files are still supported for users migrating from
the old name.

Use `/constitution` to review or change standing guidance. After setup, run a
doctor check:

```bash
codewhale doctor
```

Use the JSON form when you need a machine-readable report for an issue:

```bash
codewhale doctor --json
```

Both forms are offline by default. They report structural configuration and
literal unknown/not-probed credential states without loading workspace `.env`
credentials, opening secret/OAuth files, probing a keyring, contacting a
provider, or starting MCP servers. Use `--check-updates`, `--probe-api`,
`--probe-local`, or `--probe-mcp` only when you intentionally want that live
boundary. JSON remains offline and does not accept live flags.

JSON reports credential `source` separately from literal `availability`.
Configured environment, external-auth, OAuth, consent, and secret-store sources
remain `not_probed`; their declaration alone does not make Setup or fleet ready.
Only a structurally present literal config value, or a route where credentials
are not required, certifies offline readiness. A legacy secret-store sentinel on
a route that cannot use the shared store is reported separately as
`secret_store_unavailable`/`unavailable`, not as eligible or merely unknown.

Both `doctor` and `doctor --json` also include a session-recovery diagnostic
that compares legacy session filenames against the current store and reports
one of `isolated`, `no_legacy_sessions`, `migration_pending`,
`migration_incomplete`, `migration_complete`, or `scan_failed`; it never reads
session contents. Use `migration_pending` or `migration_incomplete` as your
cue to finish moving sessions from `~/.deepseek` to `~/.codewhale`, the same
legacy-path migration described above. Setting an explicit `CODEWHALE_HOME`
suppresses this ambient inspection.

Next: [INSTALL.md](INSTALL.md) covers platform-specific install paths,
[CONFIGURATION.md](CONFIGURATION.md) covers config resolution, and
[PROVIDERS.md](PROVIDERS.md) covers provider IDs and credentials.

## 3. Your First Task

Start with a read-only task in a real workspace:

```text
Map the repository structure and tell me where the CLI entrypoint lives.
```

Then ask for a focused plan:

```text
I want to add a small validation for empty config values. Inspect the relevant
code and propose the smallest safe change before editing anything.
```

When you are ready for edits, be specific about the acceptance criteria:

```text
Implement the validation you proposed. Keep the change scoped to config
parsing, add or update the narrowest test, and run the relevant check.
```

Good first prompts include four details:

- The outcome you want.
- The files, feature, or behavior you care about.
- What is out of scope.
- What verification should count as done.

For example:

```text
Fix the broken provider error message in the config loader. Do not change the
provider registry. Add a regression test and run only the config crate tests.
```

If you are not sure where the bug is, say that:

```text
Investigate why `codewhale doctor` reports the wrong provider. Do not edit
files yet. Return the likely cause, evidence, and a proposed patch plan.
```

Codewhale works best when you let investigation and implementation happen in
separate steps for unfamiliar code. For small, well-understood changes, a
single implementation request is fine.

Next: [MODES.md](MODES.md) explains when to use Plan, Work, and Operate.

## 4. Understanding the Interface

The interactive TUI has a few stable regions:

- Header: current session, active model, mode, and high-level status.
- Transcript: the conversation, tool calls, command output summaries, and
  model responses.
- Composer: where you type prompts, slash commands, and file mentions.
- Workbar: the strip under the composer (or an optional side workbar) that
  holds the active goal, the to-do list, and sub-agents. Rows stay for the
  whole session — finished work reads as done rather than disappearing — and
  clicking a row (or pressing `Enter` on it) opens its detail.
- Status and footer areas: live activity, queued follow-ups, and short command
  hints.

When the model asks a question (`request_user_input`), a bottom sheet opens
over the transcript rather than a centered overlay. The conversation stays
visible above it. Use `PageUp`/`PageDown`, `Home`/`End`, or modified `↑`/`↓`
(`Ctrl`, `Alt`, or `Shift`) to review the transcript while the sheet stays open.
The mouse wheel scrolls the transcript above the sheet and the question content
over the sheet itself. Moving the highlight or typing brings that content back
into view after wheel browsing. Use `↑`/`↓` to move, `Enter` to confirm, `←`/`h`
to go back to a previous question, and `Esc` to cancel the whole request.
Every question offers an "Other" row for a custom response; that text stays on
screen while you type. Keys for the sheet are in [KEYBINDINGS.md](KEYBINDINGS.md).

The bottom chrome is configurable. Run `/statusline` to choose what is
visible, or set `[tui].status_items` in `config.toml`. Each key owns exactly
one thing on screen: `mode` is the posture bar's plan/act/operate chip, and
`model`, `context_percent`, `cost`, `balance` (prepaid providers only:
DeepSeek, DeepSeekCN, OpenRouter, SiliconFlow), `cache`, `tokens` and
`ttft`, `output_rate`, `workspace` and `git_branch` are segments of the metrics line below it. Omit
`status_items` to keep the built-in default; set it to `[]` to strip the
metrics line down to the help hint.

`workspace` and `git_branch` are opt-in. The workspace chip shows the folder
name; linked worktrees include its parent to distinguish repeated names. The
branch chip shows the current branch or a short detached HEAD SHA, with `(wt)`
for linked worktrees. Both keep the last 24 display columns when long. Git
metadata refreshes in the background on the existing 15-second cadence and
when a refresh is requested; unavailable Git data removes the branch chip.
These identify the active session workspace. The full path remains in `/status`.

`context_percent` is on by default and shows `ctx NN%` at every fullness —
0.9.12 went silent below 50% and left most of a session with no context
signal at all. The reading keeps its warning colour from 80% up.

The keys `status`, `agents`, `reasoning_replay`, `prefix_stability`,
`last_tool_elapsed` and `rate_limit` were retired in 0.9.13:
they drove nothing. Old configuration files still load — the retired keys are
ignored with a warning in the log.

`status_items` composes the rows; two size presets decide how much of each
row paints. `[tui].posture_bar` and `[tui].metrics_line` each take `full`,
`compact`, or `hidden`. The posture bar defaults to `full` so active controls
stay visible; the metrics line defaults to `compact` to keep selected performance
readings while removing secondary counts and help. These are also settable at runtime with
`/config posture_bar compact`. TOML values must be lowercase; `/config`
accepts either case. `compact` is the row after its first shed
rungs: the posture bar keeps its permission and mode chips — and the cap
warning, which is advice, not decoration — and drops the clocks, counts and
hint; the metrics line keeps the route, the context reading, the cost and
the balance, plus selected TTFT and output rate when space allows, and drops
secondary counts and the help hint. `hidden` gives the
row back to the transcript. A small tmux pane can hide both rows without
touching what `/statusline` composes.

Both `ttft` and `output_rate` are on by default and work in full or compact
rows. `/statusline` lets you toggle them separately; Space previews, Enter saves,
and Esc restores your previous settings. Legacy `session_metrics` still enables
both readings. The pair shows: `ttft 1.5s` — the mean time to first streamed token — and `120 avg tok/s`,
the session's provider-reported output tokens divided by the measured request
seconds for those same calls. The rate includes connection setup, time to first
token and pauses within a response, and excludes tools and idle time between
calls. It measures effective request throughput, not decoder speed. Streaming
and non-streaming calls follow the same rule; receipts without individual
request timing are excluded from both tokens and time. While a request runs,
the last measured average stays visible. Both readings use the same
accumulators `/status` prints in full. Missing evidence is omitted rather than
estimated. On narrow rows the pair sheds before cost and context.

Every file read, command, and edit appears in the transcript as it happens.
`/receipts` lists what the session did, one line per action; see
[What Codewhale records](#what-codewhale-records). If a command fails, use the
visible failure output as part of your next instruction instead of starting
over.

The composer accepts normal prompts and slash commands. Type `/` to discover
available commands. Use file mentions when you want the model to focus on a
specific file or directory instead of searching broadly.

The workbar is useful when a turn spans multiple steps. It keeps the goal,
the to-do list, and agent state visible while the transcript continues to
grow — including after the work settles, so you can still open what happened.

Keyboard shortcuts vary by context, terminal, and platform. This guide avoids
duplicating the full shortcut catalog so it does not drift from the TUI.

Next: [KEYBINDINGS.md](KEYBINDINGS.md) is the complete shortcut reference.

## 5. Modes

Codewhale has three visible TUI modes:

| Mode | Use it for | Default posture |
| --- | --- | --- |
| Plan | Exploration, design, and review before changes | Read-only investigation |
| Work | Normal multi-step coding work | Tool use with approval gates |
| Operate | Direct work plus parallel or background coordination | Tools follow the active posture; delegate when useful |

Switch modes from the TUI with the mode picker:

```text
/mode
```

Or switch directly:

```text
/mode plan
/mode work
/mode operate
```

Plan mode is the safest place to start in an unfamiliar repository. It is for
inspection and decision-making, not file edits.
When a Plan turn successfully finishes a nonempty response, Codewhale asks
how to continue, including when the plan is plain text. **Work (Ask)** and
**Work (Auto-Review)** switch to Work with that permission and send the exact
approved response as the next message. Existing To-dos from that turn are
preserved; a prose-only plan adds one pending item without replacing unrelated
work. If Codewhale cannot prepare the work state, it stays in Plan without
starting the task. **Keep planning**, or `Esc`, stays in Plan. You can also
type feedback there to have the plan revised. The question appears only when
the composer is empty and no other view is open; `/mode work` always works.

Work mode is the default for most contribution work. It lets Codewhale read,
run checks, and edit files while keeping risky actions behind approval gates.

Operate keeps that direct tool surface and its approval, sandbox, shell,
ask-rule, and repository protections. Small or tightly coupled work stays
direct. Multi-step delegation uses a compact Workflow plan with dependencies,
bounded scopes, and completion evidence passed between steps. Fleet configures
and manages those same sub-agents and their roles. One bounded, independent
task can use a direct agent; continued work reuses it through `followup`.
Heavy work can also run on a cloud computer with `codewhale dispatch "<task>"`,
which goes through the signed-in account with a quote and EU-compute consent.

For trusted workspaces where you intentionally want actions to proceed without
approval prompts, select the Full Access permission posture with `Shift+Tab`.
Do not use Full Access in a repository you do not trust.

Modes are separate from model routing. `Tab` cycles visible modes when the
composer is idle, while `/model auto` controls model and thinking selection for
turns.

You can also change approval behavior from `/config` by editing the approval
mode. Use this only when you understand how it changes tool execution.

Next: [MODES.md](MODES.md) has the full mode, approval, and trust-mode
reference.

## 6. Slash Commands

Slash commands are typed into the composer. They are useful when you want to
change Codewhale state directly instead of asking the model in natural
language.

Common commands for first-time users:

| Command | Use |
| --- | --- |
| `/mode` | Open the mode picker or switch with `/mode agent` |
| `/model` | Select a model or use `/model auto` |
| `/provider` | Pick the active API provider |
| `/fleet` | Open the selected fleet's member roster |
| `/fleet saved` | Pick or switch among named saved fleets |
| `/goal` | Set a persistent objective the agent works toward across turns; bare `/goal` shows progress |
| `/workflow` | Orchestrate the current work as a Workflow; `status`, `cancel`, `settings` answer without a model turn |
| `/workflows` | Open the live Workflow run dashboard: every run this workspace's journal keeps, with phases, children, progress, and host-side cancel |
| `/config` | Edit runtime and provider settings |
| `/statusline` | Choose which footer status chips are visible |
| `/receipts` | List what this session did: files changed, commands run, web and MCP calls, agents, approvals and who gave them, failures |
| `/compact` | Summarize long context to recover token budget |
| `/copy` | Copy the last completed assistant response to the clipboard |
| `/review` | Ask for a structured review workflow |
| `/memory` | Inspect or manage memory when enabled |
| `/mcp` | Configure or inspect MCP server integration |
| `/plugin` | Review and manage disabled-by-default local plugin bundles |
| `/rc` | Hand this exact session to the signed-in Codewhale web app |

Toolbox commands stay searchable when you type them directly: `/models`
fetches live endpoint IDs, `/modeldb` opens the bundled model reference, and
`/rlm` loads a file or block of text into a working context that stays
available for the rest of the session.

Use `/provider` when you want to switch away from the default DeepSeek route.
Provider IDs, environment variables, model defaults, and capability notes are
kept in the provider registry document.

Soft-auto multi-agent work: [AUTOMATIC_WORKFLOWS.md](AUTOMATIC_WORKFLOWS.md).

Posting Codewhale PR reviews as a bot identity:
[GITHUB_APP.md](GITHUB_APP.md).

Next for durable multi-worker work: [FLEET_WORKFLOW_TUTORIAL.md](FLEET_WORKFLOW_TUTORIAL.md)
walks through fleet task specs, monitoring, and Workflow authoring.

Fleet is the public noun for the durable roster. `codewhale fleet …` is
the command and `/fleet` the slash command. The Fleet name is
shared by what has to stay stable across versions: the durable ledger
`.codewhale/fleet.jsonl`, saved rosters `fleets/<name>.toml`, the `[fleet]`
config table, and the `codewhale workflow run --fleet` flag.

Use `/model auto` when you want Codewhale to choose the model and thinking
level per turn. When the DeepSeek routing model is available, Auto may select
any runnable provider/model pair in the redacted inventory. That classification
sends the latest request (capped at 4,000 characters) plus a bounded summary of
up to six recent context rows (900 characters each) to
`DeepSeek / deepseek-v4-flash`. Credentials, endpoints, and provider error text
are not included in the inventory. Without that router, Auto uses a local,
provider-aware heuristic and sends no routing request. If a classifier attempt
fails validation or errors, Auto falls back to that heuristic while retaining
the attempted classifier data path in the turn receipt.

The `/model` picker states which data path is available and shows the last
resolved route. `Ctrl+O` opens the reasoning detail for the selected or current
turn; `Ctrl+Alt+O` (or `/turn inspect`) opens the whole-turn Turn Inspector,
whose model-route section records the concrete provider/model, strong/fast pair,
selected tier, selection scope, route reason, and whether the classifier received
routing context. Use a
fixed model when you need repeatable comparisons, a strict provider boundary,
or no classification request.

Use `/compact` when a session gets long and the model starts carrying too much
history. Compaction trades raw transcript detail for a concise working summary.

This guide intentionally does not list every command. The command surface
changes more often than the onboarding flow, and the TUI command palette is the
source of truth while you are inside a session.

Next: [CONFIGURATION.md](CONFIGURATION.md) covers runtime settings and
[MCP.md](MCP.md) covers Model Context Protocol integration.
[PLUGIN_BUNDLES.md](PLUGIN_BUNDLES.md) covers the disabled-by-default bundle
inventory, capability review, and namespaced Skill/MCP activation boundary.

## 7. Working with Tools

Codewhale tools are structured actions. Instead of only producing prose, the
model can call tools to inspect and change the workspace.

Examples of tool-backed work include:

- Reading a file before explaining it.
- Searching for call sites before proposing a refactor.
- Running a focused test command.
- Applying a small patch.
- Opening a sub-agent for parallel investigation.

Tool use is governed by mode, approvals, and sandbox policy. The exact behavior
depends on the current mode and config, but the basic rule is simple: start in
Plan for read-only exploration, use Work for normal changes, and reserve Full
Access for trusted automation.

The workspace boundary matters. Codewhale is expected to work in the directory
you launched it from or the workspace you configured. Be explicit when a task
should stay inside a repo:

```text
Only inspect and edit files under this repository. Do not touch parent
directories or global config.
```

When a command needs network, writes outside the workspace, or a risky shell
operation, expect an approval prompt unless you have configured more permissive
behavior.

Good tool instructions are concrete:

```text
Run the narrowest test that covers this parser change. If it fails, report the
failure and stop before broadening the test scope.
```

Avoid asking for broad cleanup during a focused fix. Smaller tool scopes make
the transcript easier to review and the final diff easier to merge.

Next: [TOOL_SURFACE.md](TOOL_SURFACE.md) lists the tool surface and
[SANDBOX.md](SANDBOX.md) explains sandbox behavior.

### What Codewhale records

Codewhale keeps these records on your machine, under `~/.codewhale/`:

- **The session.** `sessions/<id>.json` holds the full conversation,
  including every tool call and its result text. App and `codewhale serve`
  threads keep each call as a turn item under `tasks/runtime/`, with its
  input, status, start and end time, and structured result.
- **Approvals.** `sessions/<id>/approval_receipts.jsonl` records every
  approval Codewhale asked for, the decision, and who made it: you, a
  session rule, or the active posture. App threads also record each decision
  in their event log. A call that ran without asking (Full Access, an allow
  rule, a remembered grant) has no approval record; the posture each turn
  ran under is saved with the turn.
- **Undo points.** Workspace snapshots let you go back. `/undo` takes back
  your last request in one step: every file it changed, plus the request and
  its reply. It works in every access mode and lists the files it put back.
  If one of those files changed after the request, `/undo` changes nothing
  and says which file. If a request was cut off before Codewhale recorded
  where its changes end, `/undo` says so; `/undo force` undoes it anyway,
  including edits made to those files since it started. `/restore` rolls the
  whole folder back to a point you pick, overwriting later edits, so it
  needs `/trust on` or Full Access. Esc Esc rewinds the conversation only;
  `/undo` afterwards puts the files back.
- **Security events.** `audit.log` records credential changes, hook
  environment key names, compaction passes, the terminal's approval
  routing, and Auto-Review verdicts. It is not a list of what a session did.

To see what a session did, run `/receipts`, or from a shell:

```bash
codewhale receipts --last
codewhale receipts <session-id> --format json
```

A receipt says what it cannot show. A terminal session's receipt lists the
files a command changed from each turn's workspace snapshots; a Runtime
thread's lists only file tools. Terminal sessions do not save a passing
command's exit code or how long each call took. A call Codewhale blocked
before it started (Auto-Review, a policy, invalid input) is listed as
blocked, with the reason, and is not counted as run. [RECEIPTS.md](RECEIPTS.md) has the full contract.

## 8. Sub-agents and Parallel Work

Sub-agents are background child agents. The parent session gives a child a
focused task, receives an agent id, and can continue working while the child
runs.

The main orchestration tool is:

- `agent`: start a focused child with a task and role. The child runs in the
  background and returns a compact receipt plus transcript handle.

You normally do not need to call these tools directly. Ask for parallel work in
plain language:

```text
Open one read-only explorer for the config crate and another for the TUI
provider picker. Have both return file references and risks before we plan the
fix.
```

Useful roles include:

| Role | Good for |
| --- | --- |
| `general` | Multi-step tasks; the default when no role is specified |
| `explore` | Read-only code mapping |
| `plan` | Design and migration planning |
| `review` | Bug-focused review of an existing change |
| `implementer` | A tightly specified edit |
| `verifier` | Running checks and reporting pass/fail evidence |

Sub-agents are most useful when work can be separated cleanly. Do not use them
for tiny edits, and do not ask multiple agents to write the same files at the
same time.

### How long work stays coherent

Work that spans many turns does not rely on an ever-growing chat transcript.
This is ordinary Agent behavior — there is nothing to turn on and no separate
workflow to learn:

- A working context stays loaded for the session. Large source material and the
  durable transcript are held as data the agent can search and slice, and useful
  variables and imports survive across turns.
- Workflow composes independent `task(...)` calls and parallel fan-out.
- `agent` messages and follow-ups coordinate active children directly.
- Goals retain the durable objective across the work.

`/rlm <file-or-text>` points that working context at a specific file or block
of text. The historic action-shaped `rlm` tool remains registered only so older
sessions replay, and is deliberately not taught to new model turns.

Codewhale can also keep a small project-local ledger at
`.codewhale/harness/state.json`: evidence-backed prompt notes, reusable child
briefs, and skill-routing hints. Later turns receive it as untrusted
supplemental guidance, never as authority or executable instructions. Reading it
is automatic; adding or removing an entry goes through the normal approval
receipt. It is separate from personal memory, and it must never hold secrets,
scratch transcripts, or unverified claims.

Next: [SUBAGENTS.md](SUBAGENTS.md) covers roles, lifecycle, concurrency, and
output contracts.

## 9. Skills

Skills are reusable instruction packs. A skill is usually a `SKILL.md` file
that teaches Codewhale how to perform a recurring workflow, use a tool family,
or follow a project convention.

Use skills when a task has a repeatable process:

- Reviewing a specific kind of PR.
- Working with a document or spreadsheet format.
- Following a team release checklist.
- Using a project-specific memory or wiki workflow.

Inside the TUI, `/skill <name>` activates a skill when one is available, and
bare `/skills` opens the Skills Manager (owned-only inventory, no network). Use
`/skills <prefix>`, `/skills inspect`, `/skills --remote`, `/skills suggest <task>`,
or `/skills sync` for the text/registry paths. Suggestions rank the remote
catalog but never install or activate anything. The command palette can also
surface skill entries alongside normal slash commands.

Good skills are narrow. They should tell the model what workflow to follow,
what evidence to collect, and what to avoid. They should not hide credentials
or replace normal repository documentation.

If a repository has its own instructions, treat them as part of the active
work. Read the local guidance before editing, and keep any contribution within
the repository's conventions.

Next: see [SKILLS.md](SKILLS.md) for the manager, ownership, and provenance
rules; [CLAUDE_PLUGIN_COMPAT.md](CLAUDE_PLUGIN_COMPAT.md) for Claude Code
skill/plugin compatibility; and [CONFIGURATION.md](CONFIGURATION.md) for config
paths and project authority.

## 10. Getting Help

Start with doctor output:

```bash
codewhale doctor
```

Use JSON when filing a detailed issue:

```bash
codewhale doctor --json
```

For authentication problems, use the structural source state to identify what
is declared. Doctor deliberately does not inspect environment, secret-store,
keyring, or OAuth token values. When a live check is appropriate, opt in with
`codewhale doctor --probe-api` (or `--probe-local` for a local endpoint).

For provider problems, confirm the active provider and model:

```text
/provider
/model
```

For long or confusing sessions, use `/compact` to reduce context pressure, or
start a fresh session in the same workspace and summarize what you need.

When reporting an issue, include:

- Codewhale version.
- Install method.
- Operating system and terminal.
- Provider and model.
- The exact command or prompt.
- Relevant doctor output.
- Whether the problem happens in a fresh workspace.

Do not paste API keys, private source code, or secrets into a public issue.

Next: [OPERATIONS_RUNBOOK.md](OPERATIONS_RUNBOOK.md) has operational triage and
recovery steps.

## FAQ

### Is Codewhale only for DeepSeek?

DeepSeek is the default and first-class route, but Codewhale also supports
other hosted and local OpenAI-compatible providers. Use `/provider` or
`codewhale --provider <id>` to choose a provider. Keep the provider registry
open when configuring a non-default route.

### Which mode should I use first?

Use Plan for unfamiliar code, Work for normal implementation, and Full Access
only for trusted repositories where automatic execution is acceptable.

### Why does Codewhale ask before running commands?

Approvals are part of the safety model. Shell commands, paid tools, writes, and
actions outside the expected workspace can have side effects. Approval prompts
let you keep control while still letting the model do useful work.

### How do I run a Python file on macOS?

Open Terminal in the folder that contains the file and run:

```bash
python3 your_file.py
```

If macOS says `python3` is missing, install Python from
[python.org](https://www.python.org/downloads/macos/) or with Homebrew:

```bash
brew install python
```

Inside Codewhale, ask the agent to inspect the file and run it with
`python3 your_file.py`. If the script needs packages, install them in a virtual
environment first:

```bash
python3 -m venv .venv
source .venv/bin/activate
python3 -m pip install -r requirements.txt
python3 your_file.py
```

### Where is my config stored?

New Codewhale config uses `~/.codewhale/config.toml`. Legacy
`~/.deepseek/config.toml` remains supported for compatibility. Project overlays
can also affect behavior when a workspace config exists.

### How do I keep costs predictable?

Use `/model auto` for routing, choose a fixed model when you need a strict
profile, and compact long sessions. For larger tasks, ask Codewhale to plan
before implementing so you do not spend tokens on the wrong path.

### How do I continue previous work?

Codewhale saves sessions. Use the session picker or resume/continue CLI paths
documented in the README and modes guide. For a risky experiment, fork the
session before changing direction.

The `/sessions` picker starts scoped to the current workspace so resumes stay
attached to the project you opened. Press `a` in the picker to show sessions
from every workspace, or run `codewhale sessions` to list all saved sessions
with last-updated timestamps before resuming a specific id.

To archive the durable record and its artifacts, run:

```sh
codewhale sessions export <id-or-unique-prefix> --output session.tar.xz
```

The archive contains `session.json`, a portable `container.json`, a manifest,
and regular files under `artifacts/`. Use `--skip-artifacts` for the record
only, `--compression 0` through `9` to choose the xz preset (default `6`),
and `--force` to replace an existing output. Store the archive outside the
session store. Symlinks are skipped; linked artifact roots, hard links,
nonportable filenames, and trees exceeding 64 directory levels or 100,000
entries fail the export without replacing the destination.

Unlike the sanitized Markdown `/export`, these archives retain unredacted
session content, including system prompts, thinking, tool calls and results,
journal branches, and approval receipts. Extract `session.json` and open it
with `/load` in the TUI; `/resume` imports the conversation only. Extracted
artifacts remain separate files and are not installed into the artifact store
by `/load`. Pause writes before archiving if every artifact must reflect the
same instant; growing files are bounded to their recorded size and shrinking
files abort the export.

To continue the exact running session from the web app, type `/rc` or launch
with `codewhale rc`. Approve the one-time code in the system browser. While the
lease is active, the browser owns new prompts and approvals and the terminal is
a readable safety surface. Once connected, the banner and a transcript note
show the live session link (`https://app.codewhale.net/session?run=…`);
`/rc open` opens it in your browser and `/rc link` prints it. `/rc status`
shows ownership, `/rc stop` returns it to the terminal, and interrupt remains
available. A dropped connection keeps local input locked until the last web
lease expires so two controllers never race. Every folder you enroll from one
terminal shares a single stable device id, so the web app lists one computer
per machine rather than one per session.

> Note (2026-09-14): the hosted web app at app.codewhale.net sunsets in phases
> under the 2026-09-14 product-client decision; the native GPUI desktop app
> (private `codehwhale-gpui` repo, phase map in `docs/TRANSITION.md`) is the
> successor surface. `/rc` keeps working against the web app while it remains
> live.

### What should I do when the model gets confused?

Stop and restate the goal, constraints, and current evidence. If the transcript
is long, use `/compact` or start a fresh session with a short handoff. If the
problem is operational, run `codewhale doctor` and inspect the reported config
and provider state.

### Should I put project rules in prompts or files?

Use repository files for durable project rules and prompts for turn-specific
intent. If a workflow repeats across projects, consider turning it into a
skill.

### Can Codewhale edit files outside the current repository?

That depends on workspace boundaries, sandbox settings, trust mode, and
approval policy. For contribution work, keep instructions scoped to the current
repository unless you intentionally need something else.

### Where should I go after this guide?

Read the focused reference for the thing you are changing. For most users, the
next pages are install, configuration, providers, modes, keybindings, tools,
and sub-agents.

Next: [INSTALL.md](INSTALL.md), [CONFIGURATION.md](CONFIGURATION.md),
[PROVIDERS.md](PROVIDERS.md), [MODES.md](MODES.md), and
[TOOL_SURFACE.md](TOOL_SURFACE.md).

## Live Terminal dock

Open `/workbar terminal` to watch the same workspace PTY sessions the model
uses. Click the dock or press `Alt+W` to give it input focus. While focused,
typing, Enter, Tab and Ctrl+C reach the selected shell. Esc returns focus to
the composer; the output remains visible. Click a session or use `Alt+Down` /
`Alt+Up` to switch. `Ctrl+N` starts your own fresh shell without replacing the
model’s session. Mouse-wheel and PageUp/PageDown review the retained output.
The active permission posture and filesystem scope govern fresh shells and
refuse reuse of an unsandboxed shell after the scope narrows.

The dock shows the latest 64 KiB, reports omitted bytes, and never consumes the
model’s output cursor. A reset or lost session requires explicit selection;
input cannot silently follow a reused session name. PTYs are currently supported
on macOS and Linux. This is an ANSI-styled output view: cursor-addressed programs,
alternate screens and replay after process restart are outside its scope. Paste
is direct terminal input and may execute commands containing newlines; control
characters and pastes over 64 KiB are refused. While a process uses canonical
line input, batches of 1 KiB or more are refused before any bytes are sent:
the terminal can discard oversized input. Transfer long content through a file.
