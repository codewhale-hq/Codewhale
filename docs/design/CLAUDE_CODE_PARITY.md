# Claude Code — how its agents, workflows, plugins and skills actually work (parity reference)

Date: 2026-08-16. Author: the Claude Fable 5 session driving the v0.9.9
milestone, writing from direct operating knowledge of the Claude Code harness
(CLI/desktop/web, 2026-08). Companion to
[`WORKFLOWS_GOAL_PARITY.md`](WORKFLOWS_GOAL_PARITY.md) (Grok Build + dsh) and
[`MODES.md`](../MODES.md). This is a *mechanics* reference
for builders on #5439 (orchestration trio visibility), #5311 (plugin system /
federated marketplaces), #5324/#5123 (agent tool surface), and the one-bash
consolidation. Where Codewhale already does the same thing, it says so; where
Claude Code is simply different (not better), it says that too.

## 1. Sub-agents: the `Agent` tool

One tool, a handful of fields. What the model sees:

| field | meaning | notes |
|---|---|---|
| `prompt` | the task | required |
| `description` | 3–5 word label for the UI | required — the *only* per-spawn UX field |
| `subagent_type` | named agent definition (`Explore`, `Plan`, `general-purpose`, `code-reviewer`, `fork`, plus plugin-provided `plugin:agent`) | omit → general-purpose. `fork` inherits the parent's full context and always runs the parent model |
| `model` | optional override (`sonnet`/`opus`/`haiku`/…) | ignored for `fork`; agent-definition frontmatter normally decides |
| `isolation` | `worktree` (own git worktree, auto-cleaned if unchanged) or `remote` | no path knobs; the harness owns the worktree |

That is the whole spawn surface. There is **no** `max_steps`, `wall_time`,
`max_depth`, `thinking`, `write_roots`, `fork_context` bool,
`workspace_policy` or `write_authority` on the call. Budgets, tool
allow-lists, model tier and reasoning effort live in the **agent
definition** (`.claude/agents/<name>.md` frontmatter — `model`, `effort`,
`tools`, `description` "when to use") or in the harness. Everything the
parent needs after spawn is a lifecycle, not a knob:

- Agents run in the background; the parent gets a **completion
  notification** and must not fabricate results before it arrives.
- `SendMessage(to: <agent>)` continues an existing agent with its context
  intact; a new `Agent` call starts fresh. `ListAgents` enumerates them.
- The final report is *not* shown to the user — the parent relays what
  matters. Sub-agent self-reports are treated as working sets, not facts.
- Fork semantics: `subagent_type: "fork"` = same model, same context,
  tool output kept out of the parent context.

**Codewhale mapping.** The `agent` tool already has `action` (spawn/wait/
message/interrupt/…), `type` (8 roles), `profile`, `name`/`agent_id`/`message`,
`detached`, `worktree`, `resume_from`. That is the same shape as Claude Code's
Agent + SendMessage + ListAgents folded into one tool — fine. The delta is
that Codewhale *also* advertises ~20 budget/authority/model knobs per call.
Claude Code's answer, and the direction chosen for #5324/#5123: budgets and
authority belong to the role/profile definition, model to the operator's
session, and the per-call surface stays at ~12 fields (`action, prompt,
type, profile, name, agent_id, message, detached, worktree, write_roots,
resume_from, until`). Claude Code has no equivalent of `write_roots`; it is
kept because #5426 containment leans on it.

## 2. Orchestration: the `Workflow` tool (scripts, not prose)

A workflow is a **JavaScript script the model writes inline**, executed by the
harness deterministically; it runs in the background and the model gets a
completion notification. The API surface the script sees:

- `export const meta = { name, description, whenToUse?, phases: [{title,
  detail, model?}] }` — pure literal, required first statement.
- `agent(prompt, {label?, phase?, schema?, model?, effort?, isolation?,
  agentType?}) → Promise<string | object>` — with `schema` (JSON Schema) the
  sub-agent is *forced* to return validated structured output; returns
  `null` if the user skipped it or it died.
- `pipeline(items, stage1, stage2, …)` — per-item stages, **no barrier**
  between stages (default). `parallel([thunks])` — barrier. `phase(title)`,
  `log(msg)`, `args`, `budget {total, spent(), remaining()}` (a hard
  ceiling from a "+500k"-style user directive), `workflow(nameOrPath, args)`
  (one level of nesting).
- Determinism rules: `Date.now()`, `Math.random()` throw (they would break
  resume). Every run persists its script to a file; a run can be resumed by
  `{scriptPath, resumeFromRunId}` and the **longest unchanged prefix of
  agent() calls is replayed from a journal** (`journal.jsonl` records each
  agent's return value) — edit the script, resume, only the changed tail
  re-runs. Concurrency cap = min(16, CPUs−2); lifetime cap 1000 agents.
- Named/saved workflows: `.claude/workflows/<name>` resolve by name; the
  script is the artifact. Opt-in is explicit: the user must say
  "ultracode"/"use a workflow"/invoke a skill that calls it — the model may
  not spawn dozens of agents on its own judgment.
- Documented quality patterns (all in the tool's own guidance): adversarial
  verify (N refuters per finding), perspective-diverse verify, judge panel,
  loop-until-dry, multi-modal sweep, completeness critic, no silent caps.

**Codewhale mapping.** Codewhale's `workflows/*.workflow.js` + Lane Runtime
is the same idea (Grok uses Rhai; Claude Code uses JS with the API above; dsh
uses YAML presets). What Claude Code does that #5439 asks for: the *user*
sees workflows as first-class objects — `/workflows` lists live runs with
phase groups and per-agent labels, scripts are files you can open, and the
model must announce/relay. The `journal.jsonl` replay-on-resume and the
`schema`-forced structured return are the two engine features worth
copying if they are missing (check `crates/tui/src/workflow*` before
building; do not assume). The "user opts in explicitly" rule is a product
decision Codewhale should keep too: goal/workflow/auto are chosen by the
user, visibly (#5439 acceptance list), never silently.

## 3. Loops and schedules

- `/loop [interval] <prompt|/skill>` — recurring prompt on an interval; with
  no interval the model self-paces via `ScheduleWakeup(delaySeconds, noop,
  reason)`; quiet ticks are collapsed in the UI. Cron-style `CronCreate`
  exists for autonomous loops.
- `/schedule` — cloud "routines" on a cron.

**Codewhale mapping.** Goal mode + dsh-style ralph loops cover the
"keep going until done" case; the interval loop with a *visible reason
string per wake* is the piece worth adopting for goal status
(`/goal status` should show *why* it is waiting and when it wakes).

## 4. Plugins, skills, agents, hooks — the layout that makes them discoverable

- **Skills** = `SKILL.md` folders (frontmatter `name`, `description`
  "use when…"). Loaded via a `Skill` tool by exact name; slash-invocable as
  `/<name>`; scoped variants (`apps/web:deploy`) win by directory. The
  session lists every available skill with its one-line description at
  start — discoverability is a *listing*, not documentation.
- **Agents** = `.claude/agents/<name>.md` (frontmatter `model`, `effort`,
  `tools`, description "when to use"). Same listing at session start.
- **Plugins** = a marketplace entry that bundles skills + agents + MCP
  servers + hooks under a namespace (`plugin:skill`, `plugin:agent`, MCP
  tools `mcp__plugin_<plugin>_<server>__<tool>`). `/plugin` manages
  marketplaces (add/list/install/remove); plugin skills show up in the same
  listing as local ones, namespaced. Plugin MCP tools are *deferred*: names
  are visible, schemas load on demand via `ToolSearch` — the catalog stays
  small in the prompt prefix.
- **Hooks** = `settings.json` (`SessionStart`, tool-call intercept, `Stop`
  …); hook output is fed back to the model as user-level feedback.
- **CLAUDE.md / AGENTS.md** = per-repo contract, loaded verbatim; user-level
  `~/.claude/CLAUDE.md` layers under it.

**Codewhale mapping.** Codewhale has all four primitives (docs/SKILLS.md,
docs/PLUGINS.md, docs/PLUGIN_BUNDLES.md, docs/HOOKS.md, `plugin.toml`,
`/plugin marketplace …`, `.claude/skills` compat per
docs/CLAUDE_PLUGIN_COMPAT.md). #5311's real gap versus Claude Code /
kimicode is (a) **one namespaced listing** that the model *and* the user see
at session start (skills + agents + plugin-provided ones), (b) parsing
Claude/Kimi marketplace manifests so a plugin can bring agents + hooks +
MCP servers, not only a skill folder, and (c) deferred tool schemas so a
large plugin surface does not bloat the pinned prefix (docs/CACHE.md
constraint). The `.claude-plugin/plugin.json` runtime semantics Codewhale
declines to emulate (docs/CLAUDE_PLUGIN_COMPAT.md) can stay declined; the
manifest *parse* and the namespaced listing are the parity items.

## 5. Shell: one tool

Claude Code exposes exactly one shell tool, `Bash` (`command`,
`description`, `timeout` ms ≤ 600 000, `run_in_background`, an unsandbox
escape flag). Background jobs notify on exit; there is no separate
wait/interact/cancel tool family — a `Monitor` tool watches a condition,
`TaskStop` kills a background task. Interactive flags (`-i`) are refused up
front. Permission is a classifier + allow-rules over the *command string*,
not per-tool-name families.

**Codewhale mapping.** The one-bash consolidation planner already reached
the same conclusion (7 exec name families → `bash` + a small session
surface). Claude Code adds one detail worth copying: `run_in_background`
on the same tool with completion notifications, rather than a second tool
family for jobs.

## 6. What Claude Code does *not* have (Codewhale is ahead)

- No fleet ledger, no role-based authority clamps (delegation-never-widens
  is enforced by prompt + permission classifier, not by a typed envelope).
- No provider-portable model routing; the model column is one vendor.
- No `/goal` state machine with pause kinds; long-running autonomy is
  `/loop` + judgment.
- No transcript-visible tool-lifecycle contract; retired tool names simply
  vanish.

## 7. Concrete asks this note supports

1. #5324/#5123 — 12-field agent tool; budgets to roles/profiles/config
   (matches §1).
2. #5439 — `/workflow` no-arg catalog + live-run view; trio visible in the
   mode picker with "when to use" copy (matches §2/§3 product rule:
   user chooses, visibly).
3. #5311 — namespaced skill/agent/plugin listing at session start; parse
   Claude + Kimi marketplace manifests; deferred tool schemas (matches §4).
4. One-bash — `bash` + `run_in_background` + completion notifications;
   compat window for hidden legacy names ends at a version boundary
   (matches §5).
