# Fleet and sub-agents

> 阅读简体中文版：[zh_hans/SUBAGENTS.md](zh_hans/SUBAGENTS.md)

Fleet manages saved models and role assignments for these same sub-agents.
Use `agent` for an individual assignment and `workflow` for phases with
dependencies and completion checks. See [Workflow authoring](WORKFLOW_AUTHORING.md)
for plans that use the Fleet model shortlist.

Fleet roles are the user-facing vocabulary for delegated work: a parent
launches a focused `general`, `explore`, `planner`, `reviewer`, `implement`,
`test`, or `advisor` through `agent` and gets back an `agent_id`, declared
deliverables, and effective limits while the worker runs. The default receipt is
compact; request addressed detail when you need the transcript handle or ledger.
The internal runtime type is `FleetRole` (formerly
`SubAgentType`); the older role spellings (`worker`, `scout`, `plan`,
`review`, `builder`, `verifier`, `consultant`, `oracle`, …) remain accepted only as a persisted/deserialize
compatibility adapter during v0.9.x. New prompts and config should use fleet
names.

Child assignments run through the same `Engine::run_turn` planner, executor,
Session and approval inbox as ordinary turns. The private captured ChildGrant
narrows tool actions, shell, workspace, network and descendant depth at the final
execution boundary. The existing SubAgentManager still owns launch slots,
coordination, worker checkpoints and terminal delivery; it does not run another
model/tool loop. Work and the single bounded reporting turn share that configured
child Engine and its original deadline.

Transient provider failures use Core's dispatch and retry boundaries. An exhausted
budget or interrupted request preserves the recorded checkpoint and a continuation
handle. Each actually dispatched attempt carries its original session, route and
source identity: provider usage is billed once, then projected to the worker
ledger. A successful response without usage and an unknown request outcome retain
distinct coverage gaps; neither is priced as zero. For work that must survive
process restarts, sleep or remote execution, use Fleet or a Workflow-backed run.

Sub-agents inherit the parent's permitted tool registry, including `agent`
coordination. Spawning obeys one absolute depth ceiling: the root is depth 0,
its child is depth 1, and a child at `max_spawn_depth` cannot spawn again.
The operator default is 3, with a hard ceiling of 8. A role, saved profile, or
compatibility request can only narrow that ceiling. Recovery and transcript
forking retain the source's position and bounds; they do not buy another
generation. The removed `agent_open`/`agent_eval`/`agent_close` lifecycle tools
are absent from every registry.

Healthy children continue after an ordinary parent response. Their completion
returns through the existing Engine inbox and can wake the parent for another
normal turn. Explicit interruption or cancellation remains authoritative.
`detached: true` additionally opts a subtree out of parent-turn cancellation;
it does not remove child budgets or the headless host's deadline.

This doc covers roles and individual worker controls. Use `workflow` to coordinate
multiple assignments through the same worker runtime; see the per-role prompts in
`crates/tui/src/tools/subagent/mod.rs` (`*_AGENT_INTRO`) and the in-line
tool description.

## Role taxonomy

The `type` field on `agent` selects a fleet posture for the child
(`agent_type` is accepted as a compatibility alias). Each role is a distinct
stance toward the work — not just a different label.

## Maintainer posture

Sub-agents help Codewhale move faster, but the parent agent still owns the
maintainer decision. Use children to gather evidence, review patches, and run
verification while keeping the community posture in
[`AGENT_ETHOS.md`](AGENT_ETHOS.md): issues are open intake, PR gates are
review-load controls, and harvested work needs clear contributor credit.

When a child reviews community work, the parent should still inspect the PR
diff, linked issues, tests, and CI before merging, harvesting, closing, or
deferring it. A sub-agent's result is a working set, not a substitute for
stewardship.

| Role          | Stance                                 | Writes? | Network? | Shell posture | Typical use                                  |
|---------------|----------------------------------------|---------|----------|---------------|----------------------------------------------|
| `general`     | flexible; do whatever the parent says  | yes     | yes      | yes           | the default; multi-step tasks                |
| `explore`     | read-only; map the relevant code fast  | no      | yes      | bounded inspection | "find every call site of `Foo`; check the PR with gh" |
| `planner`     | analyse and produce a strategy         | no      | yes      | read-only probes | "design the migration; don't execute"        |
| `reviewer`    | read-and-grade with severity scores    | no      | yes      | bounded inspection | "audit this PR for bugs"                     |
| `implement`   | land a specific change with min edit   | yes     | yes      | yes           | "rewrite `bar.rs::Foo::bar` to do X"         |
| `test`        | run tests / validation, report outcome | no      | yes      | bounded verification (no writes) | "verify the diff with the bounded test checks; report PASS/FAIL" |
| `advisor`     | short-lived, high-reasoning counsel     | no      | yes      | none          | "what are we missing in this design?"        |
| `custom`      | explicit narrow tool allowlist         | inherits | inherits | inherits     | hand-picked tools on the parent's posture    |

A role's default is what the role *intends*, and the parent's effective
posture is always the ceiling (a child never widens beyond its parent).
Read-only roles withhold **workspace writes** by intent; nothing else is
taken away by default — every role keeps network reads, and `custom`
inherits the parent's write/network/shell posture and is narrowed only by
its explicit tool list or the spawning call. The focused worker's header
states the effective posture (`scout · read-only · network · read-only
shell`) from the runtime's own permission snapshot.

**Delegation moves work, never authority.** A read-only parent may delegate
to `implement`, but the child's effective write, network, shell, and tool
permissions remain within the parent's live posture. Inspection roles can use
the classified read-only shell surface and, where native enforcement is
available, the explicit read-only analysis mode described below. A different
role name or `read_only` flag cannot grant a shell tool the caller lacks.
The clamp (`ChildAuthority::clamp` in `fleet/exact.rs`) intersects every field
with the narrower side. Deny lists are unioned, so
`inherit_disallowed_tools: false` cannot drop any operator or ancestor denial.
Resuming a saved worker intersects its saved posture with the current caller's
posture again. This containment is pinned by
`a_read_only_parents_delegation_never_widens_authority` in
`crates/tui/src/fleet/exact.rs` tests.

Inside the process, the resolved authority is one object —
`ChildGrant` in `crates/tui/src/worker_profile.rs`: `files`
(none/read/write), `shell` (none/inspect/verify/full), `network`, `desktop`
(never granted to a child), the named tool `surface`, the caller's explicit
`scope`, and remaining `spawn` depth. A role is a preset over that object
(`ChildGrant::for_role`); `ChildGrant::resolve` intersects it with the
parent-derived profile. The child's tool catalog, its dispatch refusals, and
its capability envelope all read the same fields — a tool that is visible is
callable, and a tool that is denied never appears.

The session's **permission posture** applies inside every child exactly as
it applies to the parent turn: under Auto-Review the same deterministic
floor and one-shot model guardian decide a worker's held calls (never a
prompt; an unavailable guardian denies, fail closed); under Ask a held call
the role cannot delegate is raised as an approval prompt in the parent's
UI and the worker waits visibly (`waiting for user`), or is denied with the
reason on hosts that cannot prompt; Full Access applies
exactly as it does to the parent turn, so a call the person granted is not
stranded by the Auto-Review floor. Each decision nobody was prompted for is a
one-line note in that worker's transcript (visible when it is focused) and
an audit-log record. See `docs/MODES.md`.

Each role's full system prompt lives in
`crates/tui/src/tools/subagent/mod.rs` (search for
`*_AGENT_INTRO`). The prompt prefix loads automatically when the
child agent boots; the parent's assignment prompt becomes the first
turn's user message.

## Context forking

`agent` starts fresh by default: the child gets its role prompt plus the
task you pass. Use `fork_context: true` when the child should continue from
the parent's current request prefix instead. (`fork_context` is not in the
advertised schema — it stays parse-accepted for compat callers, and
auto-forking for read-only roles continues unchanged.) In fork mode the runtime keeps the
parent prefill/prompt prefix byte-identical where available, appends a
structured state snapshot, then adds the sub-agent role instructions and task
at the tail. That preserves DeepSeek prefix-cache reuse while giving the child
the context needed for continuation, review, summarization, or compaction work.

Use fresh sessions for independent exploration. Use forked sessions when the
task depends on decisions, files, todos, or plan state already in the parent
transcript.

Forked state shows the parent's To-do snapshot — the sole Work surface, written
by `todo_write`. The child's `<codewhale:fork_state>` block carries the bounded
body rendered by `crates/tui/src/todo_snapshot.rs`, so a fork continues from the
parent's real progress position rather than a paraphrase. That To-do section is
resolved when the spawn happens, so a `todo_write` earlier in the same parent
turn is included.

**The list is shown once, at that spawn, and never re-sent.** No sub-agent
request re-states a To-do list, and neither does a parent request. Each agent
keeps its own private list (#4810); what it knows about that list comes from its
own `todo_write` calls (each sends the whole list) and the short receipts they
returned, which are ordinary messages in its own transcript. A worker therefore cannot read or write a parent's or a
sibling's list, and a forked child cannot mutate the snapshot it was handed or
keep reading later parent changes.

That same private list is what the child's in-transcript card shows. A
delegate card renders a bounded projection of **its own** agent's To-do — the
settled/total count, the in-progress item always included, up to three rows, and an
explicit `… +N more` when the bound elides the rest — built by
`card_todo_projection` from the same snapshot, priority order, and sanitizer the
model-facing body uses. A card only ever consumes an envelope whose `agent_id`
matches it, so a parent's list never appears under a child and no sibling's list
appears under another. An agent that has stated no work shows no To-do rows at
all rather than a placeholder task, and a terminal card keeps the last snapshot
its agent actually published. Fanout cards stay a dot grid and do not show child
To-do: with many workers behind one card there is no truthful place to hang a
single list. A child To-do appears only when the runtime already represents
that child as its own delegate card.

The durable Runtime ledger (projected through fleet task status) still owns
lifecycle state. `update_plan` is no
longer reachable by a model: `model_visible()` returns `false`
(`crates/tui/src/tools/plan.rs:408-413`), so it is filtered out of the API tool
list and never appears to a child. It survives only to replay older transcripts.
Strategy that used to go there now goes in the response body, and lifecycle
state goes in `todo_write`.

## Worktree isolation

For parallel edit lanes, launch the child with `worktree: true`. Codewhale
creates a fresh git worktree and branch for that child, runs the child from the
isolated checkout, and reports the resulting workspace/branch in the returned
session projection and worker record. By default the branch is
`codex/agent-<name>-<id>` and the checkout lives beside the parent repo under
`.codewhale-worktrees/`, so the parent checkout stays clean.

Isolation is not write authority. A prompt-only start with no role/profile or
write declaration remains read-only, and read-only roles need no write scope.
Explicitly selected write-capable roles such as `general` and `implement`
inherit the parent's write ceiling and default to the workspace
(`write_roots: ["."]`) unless narrowed. Prefer explicit, disjoint `exact_files`
or `write_roots` for parallel work; `coordination_contracts` can reserve named
shared contracts. If only `deliverables` supplies a writer's scope, those files
become the exact-file scope.

`write_authority` is optional typed narrowing: `read_only` admits no write
scope, `workspace_write` uses the shared checkout, and `worktree_write`
requires actual worktree isolation. Incompatible role/scope declarations fail
before admission. Active overlapping shared claims fail before mutation; a
real isolated worktree may proceed in parallel. A `custom` role requires
explicit write-capable authority to claim writes; otherwise it starts
read-only.

Read-only is not file-only. A normally write-capable agent narrowed with
`write_authority: "read_only"` keeps the existing classifier-bounded inspection
shell when its parent permits it: Git history/status, search, and allowed `gh`
log reads, not arbitrary commands or test programs. Workflow read-only steps
likewise use the effective role's tools instead of imposing a second File-only
list; a `test` role retains its bounded verification interface. Explicit tool
allowlists, `deny_all_tools`, parent denials, network limits, and mutation checks
still apply. A parent without shell access cannot delegate it.

Optional fields:

- `worktree_branch`: exact branch to create.
- `worktree_base`: git ref to branch from; defaults to `HEAD`.
- `worktree_path`: exact checkout path. Relative and absolute paths must stay
  under the default sibling `.codewhale-worktrees/<repo>/` root (symlinks are
  resolved before the check). Any worktree request keeps the approval card,
  even for a read-only role.

`cwd` may be combined with `worktree`: the requested directory becomes the
discovery anchor the repo root (and the new checkout) is resolved from
(`prepare_child_workspace`). Without `worktree`, `cwd` remains the manual
escape hatch for an already-created directory inside the parent workspace.

### File deliverables and edit claims

Put required files in `deliverables`; keep the human outcome in
`expected_artifact`. For example, call `agent` with:

```json
{
  "action": "start",
  "type": "implement",
  "prompt": "Summarize the local routing evidence in reports/routing.md.",
  "exact_files": ["reports/routing.md"],
  "deliverables": ["reports/routing.md"],
  "expected_artifact": "A concise report with source references and open gaps"
}
```

At most 16 repo-relative file paths are accepted. Absolute paths, traversal,
repository metadata paths, and symlink traversal are refused. Completion checks
each file against the admitted scope and reports its path, status, and byte
count where available. The terminal statuses are `present`, `missing`, `empty`,
`not_file`, `out_of_scope`, `invalid_path`, and `unreadable`.
`present` means a nonempty regular file exists; it does not prove the report is
correct or that tests passed.

A missing or invalid required file sets `verification.status` to
`deliverable_missing` with the individual verdicts. Successful file checks can
produce `deliverables_present`; they do not turn a child self-report into an
independent quality gate. The completion notice includes the actual verdicts,
including when a worker fails or exhausts a budget.

Edit claims are checked separately against the spawn-time git HEAD and dirty
file contents. Explicit changed-file declarations can produce
`claim_mismatch` when a claimed file did not change, or when a successful
bounded write receipt changed a file the child did not declare. A peer's
change inside a worker's broad scope is not enough to attribute that write to
the worker. `path:LINE` and `path:LINE-LINE` evidence citations, including
sentence punctuation and Markdown links, never count as edit claims.

### Read-only shell commands

Scout, reviewer and planner agents, agents narrowed with
`write_authority: "read_only"`, and durable Fleet workers with a read-only
shell grant all judge `bash` calls by the same read-only grammar:

- inspection programs: `ls`, `pwd`, `cat`, `head`, `tail`, `wc`, `which`,
  `stat`, `file`, `du`, `df`, `grep`, `rg`, `fd`, `find` without `-exec` or
  `-delete`, and `sed -n <range>p`;
- `git status`, `log`, `diff`, `show`, `ls-files`, `blame` and `grep`,
  optionally after `-C <dir>` or `--no-pager`;
- the text filters `sort`, `uniq`, `cut`, `tr` and `comm`, and literal
  `echo`/`printf`;
- with a network grant, `gh` issue/pr/release/repo/run/workflow view or list
  reads.

`npm view` is refused even with a network grant: npm configuration can select
executable helpers, so package metadata reads are outside this bounded grammar.

Admitted commands can be joined with `|`, `&&`, `||` and `;`, for example
`git diff HEAD && echo '=== FILES ===' && ls -la`. A leading `cd <dir> &&`
sets the working directory, and that directory must be inside the workspace.
The only redirects are `2>/dev/null`, `>/dev/null` and `2>&1`. Quoted text is
data, so `rg 'a && b' src` is one search. Other redirects, `$` or backtick
expansion, subshells, backgrounding, inline environment assignments, and any
other program (such as `python`, `awk`, `jq` or `cargo`) are refused. Options
and path operands are still checked, and each `gh` or `npm` read needs the
network grant wherever it appears in the command.

A refused command comes back to the agent as an error result that names the
rule, for example
`[shell.readonly.command] program: `touch` is not a read-only inspection command`,
followed by what the agent can do instead. The agent keeps working, and
repeated refusals without progress end it as failed rather than completed.

### Reading beside a writer

Read-only tools and classifier-approved shell reads can run while a peer owns
a shared write claim. For arbitrary analysis code, call `bash` with explicit
`read_only: true`:

```json
{
  "action": "run",
  "read_only": true,
  "command": "python3 -c \"import sqlite3; db = sqlite3.connect('file:cache/index.db?mode=ro', uri=True); print(db.execute('SELECT name FROM sqlite_schema').fetchall())\""
}
```

This mode requires native filesystem read-only isolation and denies network
access. It accepts only foreground `run` with `command`, optional `cwd`, and
`timeout_ms`. Background or interactive modes, stdin, sandbox escalation, and
external execution backends are incompatible. If native enforcement is absent
or cannot be prepared, the call refuses before executing the command; the flag
never falls back to trusting a promise that the code only reads. Existing role,
tool, and ancestor policy restrictions still apply.

For a write refusal outside your own scope, `agent(action="claim", ...)` can
add permitted paths to your claim. It cannot take a live peer's claim. Wait for
that peer, choose disjoint bounded writes, or use a separate worktree for code
that needs writes. `action="release"` only clears claims whose owners are no
longer live; it is not a way to unlock another running worker's files.

## Delegation briefs

The parent should pass a compact brief instead of a loose paragraph. Use the
structured `dependencies` and `acceptance` arrays for bounded prerequisite facts
and observable checks; keep the focused objective in `prompt`. Do not copy raw
parent reasoning or an unbounded transcript.

Default shape for the brief — a plain sentence beats the template when the
delegation is trivial:

```
QUESTION:
SCOPE:
ALREADY_KNOWN:
EFFORT: quick | medium | thorough
STOP_CONDITION:
OUTPUT: VERDICT, EVIDENCE, GAPS, NEXT
```

`scout` briefs default to quick, read-only investigation (no writes, but
network reach and the bounded verification surface are available for real
scouting). A quick scout usually needs only a handful of calls: orient,
search, read the decisive lines, and return — but stop at decisive evidence,
not at a number. There is no per-agent call cap to save; the runtime budgets
depth and concurrency, not curiosity. Do not repeat `ALREADY_KNOWN` work
unless evidence contradicts it. Builder and repair-style briefs should use
checkpoints before scope expansion or after repeated failures.

Good delegation prompt examples:

```text
QUESTION: Does PR #N introduce release-risk behavior around provider routing?
SCOPE: PR #N diff, linked issue, provider routing tests, docs/PROVIDERS.md.
ALREADY_KNOWN: Branch is <release-branch>; workspace version is <live-version>.
EFFORT: medium
STOP_CONDITION: Return once you have either one BLOCKER/MAJOR issue or enough evidence for no MAJOR+ issues.
OUTPUT: VERDICT, EVIDENCE with file:line refs or PR refs, GAPS, NEXT.
```

```text
QUESTION: Where is the child-agent prompt assembled?
SCOPE: crates/tui/src/prompts*, crates/tui/src/tools/subagent/*.
ALREADY_KNOWN: The model-facing launcher is only `agent`; do not look for removed lifecycle tools.
EFFORT: quick
STOP_CONDITION: Stop after identifying the prompt source files and the function that wraps assignment text.
OUTPUT: VERDICT, EVIDENCE, GAPS, NEXT.
```

```text
QUESTION: Is the focused prompt/subagent test filter valid, and what fails if not?
SCOPE: cargo test -p codewhale-tui --lib --locked prompt; subagent filter if needed.
ALREADY_KNOWN: Do not fix failures; capture exact command, exit code, and first relevant assertion.
EFFORT: medium
STOP_CONDITION: Stop after one clean PASS or one reproducible failing assertion with command evidence.
OUTPUT: VERDICT, EVIDENCE, GAPS, NEXT.
```

### When to pick which role

- **`general`** — when the task is "do this whole thing", not "go
  look", "design", or "verify". This is the right default; reach for
  a more specific role only when the posture matters.
- **`explore`** — when the parent needs evidence before deciding what
  to do next. Scouts are cheap and fast; open 2–3 in parallel
  for independent regions.
  They should orient first: confirm the project root, read relevant
  `AGENTS.md`/`README.md` guidance in unfamiliar trees, search only the
  likely scope, and return `path:line-range` evidence instead of a narrative
  tour. The role name to use is `explore`.
- **`planner`** — when the parent has an objective but no executable
  decomposition. Planners write artifacts (`todo_write` items,
  strategy in the response body) but don't carry them out.
- **`reviewer`** — when there's already a change and the parent wants
  it graded. Reviewers run under read-only posture, so the runtime
  refuses patch attempts — describe the fix in the finding and the
  parent dispatches a builder when the verdict is "fix it".
- **`implement`** — when the change is already specified and just
  needs to land. Builders stay tightly scoped: minimum edit, no
  drive-by refactoring, run a quick verification before handing back.
- **`test`** — when the parent needs an authoritative pass/fail
  on the test suite or other validation. The verifier posture never
  writes — the runtime refuses fix attempts — so capture the failing
  assertion + stack and put fix candidates under RISKS for the parent
  to dispatch. Shell is clamped to the bounded built-in verification
  surface: Run tests/verifiers (pass `cwd` when the checks live in a
  subdirectory), Git fetch for remote refs, Git merge_tree for merge
  results. The write ceiling is read-only and unbounded shell forms
  are refused (#5186). A refused probe is reported to the parent,
  never worked around (#6298).
- **`advisor`** — when the operator wants a high-leverage second opinion
  before cheaper execution continues. Consultants read enough to ground a
  recommendation; their grant carries no writes and no shell, so the
  runtime refuses both. `oracle` and
  `consultant` remain accepted only when loading older requests or persisted
  records; new prompts, receipts, and UI use `advisor`.
- **`custom`** — only when the parent needs to constrain the tool
  set explicitly. Pass the allowlist via the `allowed_tools` field
  on legacy/internal sub-agent records; the model-facing `agent` tool keeps the
  public schema intentionally small.

### Aliases

The model can spell each role multiple ways:

| Canonical     | Aliases                                                          |
|---------------|------------------------------------------------------------------|
| `general`     | `worker`, `default`, `general-purpose`, `general_purpose`         |
| `explore`     | `scout`, `explorer`, `exploration`                               |
| `planner`     | `plan`, `planning`, `awaiter`                                    |
| `reviewer`    | `review`, `code-review`, `code_review`                           |
| `implement`   | `builder`, `implementer`, `implementation`                       |
| `test`        | `verifier`, `verify`, `verification`, `validator`, `tester`       |
| `advisor`     | `consultant`, `oracle` (compatibility input only)                 |
| `custom`      | (none; explicit `allowed_tools` array required)                  |

All matching is case-insensitive. Unknown values produce a typed
error listing the accepted set, so the model can self-correct on
the next turn.

## Concurrency cap

Up to **64** sub-agents run concurrently by default (`DEFAULT_MAX_SUBAGENTS`),
configurable via `[subagents].max_concurrent` in `~/.codewhale/config.toml` up to
the hard ceiling of **128** (`MAX_SUBAGENTS`). The session admits a bounded
queue of up to **1024** running plus queued sub-agents by default
(`MAX_SUBAGENT_ADMISSION`, `crates/tui/src/config/subagent_limits.rs:21`), so a turn can
request broad fan-out and let the manager drain it without creating an
unbounded population.

By default every admitted child may start immediately — there is no artificial
throttle beyond the rate-limit governor described below. Request the fan-out the work actually needs and let the runtime
queue and drain it; the caps above are enforcement, not a reason to
pre-refuse valid work. If you want gentler fan-out, lower `[subagents].launch_concurrency`
(how many direct children start at once); children beyond that limit **queue**
for a launch slot rather than bursting. `launch_concurrency` defaults to the
resolved `max_subagents` cap. (The pre-v0.8.61 `interactive_max_launch` key is
still accepted as a deprecated alias; the new key wins when both are set.)

High-fanout Workflows can tune that bounded population with `[subagents]
max_admitted` (aliases: `max_total`, `admission_limit`). That total ceiling
counts both **running** and **queued** agents, while `launch_concurrency` keeps
instantaneous execution bounded. Completed / failed / cancelled records persist
for inspection but don't occupy an admission slot. Agents that lost their
`task_handle` (e.g. across a process restart) also don't count against the cap.

### Rate-limit governor

The one automatic throttle is the rate-limit governor. It watches provider
rate limits (HTTP 429) across a 60-second window. After repeated limits it
shrinks the number of launch slots; under a sustained burst it pauses new
launches entirely. Steady successes add slots back one at a time. It never
interrupts an agent that is already running, and quota exhaustion is not
treated as a throttle.

While the governor is holding launches back, it says so in two places:

- a queued agent's row gives the reason, for example
  `launch slots throttled to 4/8 after 2 provider rate limit(s) in the last 60s`
  or `launches paused after 4 provider rate limit(s) in the last 60s`, and the
  time its wall budget ends;
- `GET /v1/agent-runs` returns a `governor` object next to `runs`, with
  `launch_slots`, `max_launch_slots`, `paused`, `recent_rate_limits`, and a
  `status` line while launches are held back. It describes launches made by the
  runtime serving the request (Fleet runs).

Known limitation: the `/subagents` register header does not show the governor
line yet; the TUI receives agent lists from the Engine without governor state.

Provider profiles let one config stay aggressive for direct API routes while
keeping subscription or aggregator routes gentle. Every key under
`[subagents.providers.<provider>]` inherits from `[subagents]` when omitted.
Provider keys accept canonical names such as `deepseek`, `zai`, `openrouter`,
and aliases such as `glm` for Z.ai:

```toml
[subagents]
# Global fallback for providers without a profile.
max_concurrent = 20
launch_concurrency = 20
max_admitted = 200
# Operator-selected Runtime delegation depth. The default is 3; this explicit
# value opts in above the default but remains below the hard ceiling of 8.
max_depth = 6
# Omitted or zero model-step budget is unbounded. Set a positive value only
# when an operator deliberately wants a per-child cap.
default_max_steps = 0
default_wall_time_secs = 1800

[subagents.providers.deepseek]
# Direct API key with room to fan out.
max_concurrent = 20
launch_concurrency = 20
max_admitted = 200

[subagents.providers.glm]
# Z.ai / GLM subscription-style route: keep pressure tight.
max_concurrent = 4
launch_concurrency = 3
max_admitted = 12
max_depth = 2
api_timeout_secs = 180
heartbeat_timeout_secs = 240

[subagents.providers.openrouter]
max_concurrent = 5
launch_concurrency = 3
max_admitted = 20

[subagents.providers.anthropic]
max_concurrent = 3
launch_concurrency = 2
max_admitted = 12
```

Use `/config subagents status` to see both the global values and the active
provider's resolved fanout, depth, and timeout profile.

## Advertised agent-tool fields

The model-facing `agent` schema exposes these controls:

| Purpose | Fields |
| --- | --- |
| Launch and route | `action`, `prompt`, `type`, `profile`, `name`, `model`, `model_strength`, `thinking` |
| Scope and outputs | `worktree`, `cwd`, `write_authority`, `write_roots`, `exact_files`, `coordination_contracts`, `deliverables`, `expected_artifact` |
| Narrow run limits | `max_steps`, `wall_time_secs` |
| Coordinate and recover | `agent_id`, `agent_ids`, `all_parked`, `message`, `until`, `detached`, `resume_from` |
| Inspect | `detail`, `offset`, `limit` |

`start` requires `prompt`. `message` requires a target and message;
`followup` requires a message and exactly one target form: `agent_id`/`name`,
`agent_ids`, or `all_parked: true`. `peek`, `interrupt`, and `cancel` require a
target. `claim` requires scope entries. These action requirements are validated
before execution.

`agent(action="roster")` reports each built-in role's resolved provider, model,
reasoning effort, known route limits and capability provenance. It uses the
same resolver as execution. An explicit saved profile wins first, followed by
a manual role pin in the current configuration, then a unique saved member
pinning that semantic role. Conflicting task `model` or `model_strength` choices
fail before admission. For an unpinned role, per-task `model` precedes
`model_strength`, then inherited role defaults and the session route.
When a Pod is selected, the `models` rows list its exact routes in saved order.
Use a listed `provider/model` selector for a task on an unpinned role; the session
model remains allowed. Off-list choices fail with the allowed routes, and a bare
model shared by multiple providers requires an exact selector. Without selected
models, current-provider overrides and `model_strength` retain their behavior;
foreign-provider requests fail. These choices do not change child authority.

The `profiles` rows expose saved members from the existing selected Fleet or
trusted config/personal/workspace/plugin layers, with bounded identities and the
same route/cost evidence. `profile="bug-hunter"` loads that member's instructions,
role, provider/model pin and depth limit. Conflicting type or model requests are
refused; explicit `thinking` overrides the saved tier. Missing providers, revoked
plugin authority and disabled project profiles fail before child admission.
Discovery never creates a profile or enrolls a model. These identity choices use
the existing child lifecycle; a saved profile alone does not create a continuing
Bot conversation or a computer lease.

Cost classes describe current uncached text input/output rates, not the total
price of a future task. Missing or routing-dependent prices remain unknown;
subscription/local routes are labelled not money metered. Discovery makes no
provider request and reports reachability as unverified.

**Parse-accepted but unadvertised (compat).** Other inputs remain accepted
for saved transcripts, ACP/MCP clients, fleet execution data, and
internal/operator compatibility. Runtime validates and intersects them with
live policy:

- delegation compatibility: `max_depth`, `maxDepth`, or `max_spawn_depth`;
  values are restricted to 0 through the Runtime hard ceiling of 8 and only
  narrow the inherited absolute ceiling. Model-facing calls inherit depth
  from the operator and selected profile.
- workspace/isolation: `workspace_policy`, `fork_context`,
  `worktree_path`, `worktree_branch`, `worktree_base`
- spawn contract: `deliberate`, `dependencies`, `acceptance`, `allowed_tools`
- lifecycle extras: `timeout_secs` (wait), `reason` (interrupt),
  `include_archived` (status)

Compatibility input is not a way to widen inherited authority or remove a
finite budget.

## Child budgets (steps, wall time, tokens)

`max_steps` and `wall_time_secs` are optional per-call limits.
Each can only narrow the applicable role, operator, parent, and saved-run
limits; the one exception is that `wall_time_secs` may raise the built-in
1800-second default (see below). Omission inherits those limits; explicit zero, null, negative, or
out-of-range values are rejected by the tool parser (schema minimum is 1).
Fleet file task-specs use a different convention — there, omitted-or-zero
means unbounded; see `docs/FLEET.md`.

`max_steps` counts model turns and accepts 1 through 2000. All roles default
to no model-turn cap unless an operator or ancestor supplies one; the internal
zero representation for that default never cancels a finite inherited cap.
`wall_time_secs` accepts 1 through 86400. Omitted, it defaults to 1800
seconds. An explicit value may go above that built-in default, for long
unattended work; an operator-configured `default_wall_time_secs` is both the
default and a ceiling, and role, parent, and saved-run deadlines still only
narrow. It covers model requests and tools. The effective absolute deadline
is persisted, and saved again when a queued agent launches, so a later
continuation is bounded by the deadline the agent actually worked to.

The work clock starts when the agent gets a launch slot. An agent that waits
in the launch queue waits at most `wall_time_secs`; if no slot opens in that
time it fails with a `never started` reason and zero steps, instead of being
reported as a run that used up its budget. An agent that does get a slot
after waiting receives its full `wall_time_secs` from that moment, still
bounded by any parent, saved-run or source deadline. So a parent can wait up
to about twice `wall_time_secs` for a queued agent. The queued row names the
reason for the wait and the time the agent stops waiting. If agents often
wait long, start fewer at once.

For example, a focused review can request:

```json
{
  "action": "start",
  "type": "reviewer",
  "prompt": "Review the parser diff and report concrete regressions.",
  "max_steps": 12,
  "wall_time_secs": 300
}
```

The receipt's `effective_limits` is authoritative; a request for 300 seconds
cannot extend a parent's earlier deadline. A continuation keeps the source's
remaining steps, original deadline, and token history. A new ID, role, or
`resume_from` fork cannot reset those bounds.

### Token accounting and partial results

Token budgets were retired in 0.9.14: token usage is tracked, never
enforced — runs are no longer stopped by token accounting. Legacy input that
still carries `token_budget` parses and is ignored; `max_steps` and
`wall_time_secs` remain the narrowable per-call limits.

The governor uses provider-reported input plus output tokens, not a local
estimate presented as a bill. Request output is capped to the remaining
allowance. Unknown prompt usage and requests already in flight can overshoot;
receipts retain the full reported usage. Missing usage remains unknown.
Worker records distinguish the worker's own token totals from shared
`budget_spent_tokens` and `budget_remaining_tokens`; do not sum a shared
pool once for every descendant.

The worker reserves room for one final report inside these limits: up to 10%
of a token allowance (at most 8192 tokens, only when at least 1024 can be
reserved), one turn when the step cap permits at least two, and up to 10% of
wall time (at most 10 seconds). Ordinary task execution stops before using
that reserve. Shared scopes hold back one token reserve for the scope;
reporting workers atomically claim remaining headroom so siblings cannot
independently reuse it. Continuation never refunds measured usage or resets
the original deadline.

The final reporting turn uses the worker's existing resolved provider and
model, with tools disabled and at most 1024 output tokens. It consolidates
bounded assistant notes and tool results into findings, evidence, produced
files, unfinished work and next steps. Estimated input cost counts against
its allowance. Provider transport retries remain inside the one logical
turn and its original wall-time deadline; no worker summary retry loop is
added. Token estimates are not billing receipts: unknown provider input and
requests already in flight can still overshoot, and actual usage is recorded.

The outcome stays `BudgetExhausted`, even when a useful report is obtained,
with the specific cause, checkpoint, measured usage and normal deliverable
verdicts. If the allowance is too small or already spent, earlier bounded
usage is unknown, the provider fails, or time expires, the worker returns
recorded partial text and says why a model report was unavailable.
Known missing response usage and attempts interrupted by timeout or cancellation
stay recorded across continuations and shared siblings; later known usage remains a
subtotal and cannot restore reporting headroom in that bounded scope.
Cancellation wins over reporting. Missing usage stays unknown. Exhausted
scopes reject further spawns or continuations; a partial report is not
successful completion.

## Per-role models (#3018)

Children can run on a different model than the parent. Structured role pins,
the legacy model map, and convenience keys feed one override map. Structured
`[subagents.roles.<role>]` entries win over `[subagents.models]`, which wins over
the convenience keys. Keys are case-insensitive; within the structured table,
a canonical role key wins over its legacy alias:

```toml
[subagents]
default_model  = "deepseek-v4-flash"   # fallback for every role
worker_model   = "deepseek-v4-pro"     # worker
scout_model    = "deepseek-v4-flash"   # scout
planner_model  = "deepseek-v4-flash"   # planner
reviewer_model = "deepseek-v4-pro"     # reviewer
custom_model   = "deepseek-v4-pro"     # custom

[subagents.models]
# Free-form role → model map; any role alias accepted by agent works.
builder = "deepseek-v4-pro"

[subagents.roles.reviewer]
model = "deepseek/deepseek-v4-pro"
```

These are manual pins for direct and Workflow `agent` starts. A task may restate
the same model or exact provider/model pair, but cannot change the pin with
`model` or `model_strength`. An explicit saved profile takes precedence over a
manual role pin. A type-only start also selects a unique saved role pin when
there is no manual override; ambiguous saved roles fail instead of choosing one.
Durable Fleet runs retain their selected member's frozen route.

A structured role pin may list approved replacement routes:

```toml
[subagents.roles.reviewer]
model = "xai/grok-4.6"
replacements = ["deepseek/deepseek-v4-pro"]
```

When the pinned route refuses the agent's **first** request (exhausted quota,
rejected credentials or authorization, or an unavailable model), the agent
retries that same request on the next listed route, keeping its role,
permissions, tools, scope and budgets. Listing a route authorizes sending the
agent's task to that provider, so each entry must name `provider/model`; at
most three are allowed and each is tried once. The route receipt records the
effective route, `route_source = "role.replacement"`, and a note with the
original route, the reason and the attempt. Replacement never happens after
the agent has run a tool, never for content-policy, context-length or
invalid-request errors, never for Codewhale's own permission denials, and never
for exact Fleet members or task-level `model` choices, which stay exact.

Structured role pins accept `provider/model`, preserving the configured provider's
exact identity and the complete model suffix. Unknown providers, empty pairs,
and cross-provider `auto` choices fail before admission. A bare structured model
inherits the session provider. For a namespaced model, qualify it explicitly,
for example `openrouter/deepseek/deepseek-v4-pro`. Legacy scalar and
`[subagents.models]` values keep their full provider-owned id, including slashes;
they do not change providers.

The v0.9.x convenience keys `explorer_model`, `awaiter_model`, and
`review_model` remain accepted as deprecated aliases so existing config files
do not break.

Model ids may be **any model the active provider accepts** — validation is
provider-aware and happens at spawn time, not load time. On the official
DeepSeek API only DeepSeek ids are accepted; every other provider passes the
id through to the provider API, which is the authority. A non-DeepSeek
example:

```toml
provider = "moonshot"
model = "kimi-k2.7-code"

[subagents]
worker_model = "kimi-k2.6"
```

Model ids are validated the same way when applied to a child route; an invalid
id on the official DeepSeek API fails the spawn with the accepted-id list
instead of an opaque provider 400.

With `/model auto`, sub-agent routing is provider-aware too: providers with a
known big/cheap pair (DeepSeek, and the hosted DeepSeek routes on NVIDIA NIM,
OpenRouter, Novita, SiliconFlow, SGLang, vLLM) route between that pair;
providers without a known cheap tier (e.g. Ollama, Moonshot) skip the
network router and keep children on the session model.

## Per-profile provider routes (#3965)

`[subagents.models]` changes the child model within the active provider. A slash
in that legacy input does not grant another provider. To pin a different provider,
use a structured `[subagents.roles.<role>]` declaration as above, or use a
fleet/AgentProfile and select it with `profile` or its unique saved role.
The profile's explicit `provider` +
`model` fields win over the parent session route; omitting `provider` preserves
the existing inherit behavior.

Example: keep the parent session on DeepSeek, but run a formatter child on a
local LM Studio OpenAI-compatible endpoint:

```toml
# ~/.codewhale/config.toml or workspace config
provider = "deepseek"

[providers.deepseek]
api_key = "YOUR_DEEPSEEK_KEY"

[providers.lm-studio]
kind = "openai-compatible"
base_url = "http://127.0.0.1:1234/v1"
api_key = "lm-studio"
model = "qwen-2.5-7b"
```

```toml
# .codewhale/agents/local-formatter.toml
id = "local-formatter"
role_hint = "formatter"
provider = "lm-studio"
model = "qwen-2.5-7b"
reasoning_effort = "off"

[instructions]
text = "Use small, local edits. Keep formatting changes mechanical."
```

Then call `agent(profile: "local-formatter", prompt: "...")`. In-process
children build a client for `lm-studio`; fleet workers forward
`--provider lm-studio` to `codewhale exec`, which resolves the same
`[providers.lm-studio]` table. Unknown or unconfigured provider ids fail the
spawn rather than silently falling back to the parent provider.

## Per-step API timeout (#1806, #1808)

Each sub-agent step wraps its DeepSeek `create_message` call in a
per-step timeout so a single stuck request can't pin the parent's
completion wakeup channel indefinitely. The default is `600` seconds.
A timed-out attempt is retried with exponential backoff (up to 5
retries) before the step interrupts with a preserved checkpoint.
Long-thinking children that legitimately exceed that, for example
heavy plan or review work behind `agent`, can extend the timeout in
`~/.codewhale/config.toml`:

```toml
[subagents]
api_timeout_secs = 900  # 15 minutes; clamped to 1..=3600
```

Values are clamped to `1..=3600`. `0` and `unset` keep the `600`
second default.

## Stale-agent heartbeat (#2614)

Running agents also track manager-visible progress. If a child stops emitting
progress for the heartbeat window, the manager auto-cancels it, releases its
sub-agent slot, and keeps the cancelled record inspectable through the returned
transcript handle and persisted worker record. The default is 5 minutes
(resolved to at least 30 seconds above `api_timeout_secs`, so 630 seconds
with the 600-second default API timeout):

```toml
[subagents]
heartbeat_timeout_secs = 300  # clamped to 30..=3600
```

The effective heartbeat is kept at least 30 seconds above
`api_timeout_secs`, so a configured long model request is not cancelled before
its own request timeout can fire.

## Lifecycle

Each opened session produces a record that progresses through:

```
Pending → Running → (Completed | Failed(reason) | Cancelled | Interrupted(reason) | BudgetExhausted)
```

An explicit interrupt, exhausted provider retries, or recovery of an orphaned
running record can leave an `Interrupted` worker with a checkpoint. Inspect
`needs_continuation` and the recorded reason; use `followup` for continuable
work. `BudgetExhausted` includes the specific token, step, or wall-time cause;
continuation cannot replenish an exhausted allowance.

`wait` observes workers. A timeout returns current outcomes and never parks,
cancels, or resumes them. `until: "completion"` returns when one child settles;
`until: "all"` joins the workers running when that call starts;
`until: "activity"` can return on progress. A later spawn is not silently added to an
earlier join.

An ordinary parent response leaves healthy children running. The same Engine
turn loop consumes their completion notices and can continue the parent.
Headless `codewhale exec` defers a successful final receipt until its existing
Engine reports no live children and no queued child completions. Its original
wall-clock deadline still bounds that settlement, including autonomous parent
turns. Cancellation, deadline exhaustion, a fatal event, or a lost Engine
channel stops settlement and returns the appropriate interrupted or failed
receipt with recorded partial usage. It does not report successful child
completion merely because the parent's first response ended.

### Session boundaries (#405)

Each `SubAgentManager` instance assigns itself a fresh `session_boot_id` on
construction. Every new session stamps the agent with that id; the workspace
state file records it for restart recovery.

Work-bar/status projections focus on current-session agents by default.
Prior-session agents that are not still running are treated as archived records
so the model does not mistake stale work for live work. This is a
*prior-session* rule only: agents that finished in the CURRENT session keep
their work-bar rows for the rest of the session (quiet completion), and their
details still open from those rows.

Records that loaded from a pre-#405 persisted state file (no
`session_boot_id` field) classify as prior-session because the
manager can't match them to the current boot.

## Run receipts, follow-up, and takeover

Each compatibility sub-agent has a persisted worker record in
`.codewhale/state/subagents.v1.json`. The record is the current run-ledger
slice for sub-agent lanes until those lanes are backed directly by the fleet
ledger: it stores `run_id`, objective, role/model,
workspace/branch, lifecycle events, artifact refs, follow-up target, takeover
target, usage provenance, and verification provenance.

The normal parent flow is to keep working and consume the completion event.
Default start and status receipts are compact; full snapshots and worker
records are diagnostic detail, not repeated in every response.

### Continue an existing worker

`message` queues a note without waking the child. `followup` wakes a running
child or resumes a continuable checkpoint:

```json
{"action":"followup","agent_id":"child-previous-id","message":"Continue the assignment using the recorded evidence."}
```

Use the returned `agent_id` for subsequent waits and messages. The receipt's
`from` and `to` identify the original target and its current continuation.
The original receipt is retained. Retrying through an old ID follows the
persisted continuation chain and does not create a duplicate worker. If the
current successor is running, the follow-up is delivered there; if it has
already settled and cannot continue, the response says no message was
delivered. Duplicate workers are prevented, but repeated messages to a running
worker are still repeated messages.

For a batch, choose exactly one target form:

```json
{"action":"followup","agent_ids":["child-a","child-b"],"message":"Continue the remaining checks."}
```

```json
{"action":"followup","all_parked":true,"message":"Continue the parked assignments."}
```

Explicit batches accept up to 32 distinct IDs. `all_parked` selects parked
children you control and refuses more than 32 so you can choose explicit
batches. Bulk responses return separate `results` and `errors`; a failing
target does not roll back a successful continuation. Parent/descendant control
checks apply to both the addressed record and its current successor.

Use `start` with `resume_from` only to create a separate worker from a settled
child's transcript, for example to assign a new review. Each such start is a
new worker. Missing, running, or cross-workspace sources are refused; the
source's authority and budget bounds still apply. This is distinct from
continuing parked work with `followup`.

### Compact status and full transcript retrieval

Unscoped `agent(action="status")` returns a session-scoped page bounded to
8 KiB. `offset` and `limit` page the roster; the default and maximum limit is
20. Follow `next_offset`, since the byte bound can return fewer rows than
requested. The model-facing roster wire uses one stable `columns` header and
an array of values per entry in `agents`; pair each row with the header instead
of reading it as an object. `null` means absent or unreported, and a measured
zero stays numeric `0`. The header is present even on an empty page.
Rows include worker and parent IDs, current depth, state,
elapsed time, own token total, recent activity, pending input, and continuation
lineage (`resumed_from` / `resumed_as`). Verification includes the verdict,
nonempty deliverable counts and a short warning when needed. Names, steps,
routes, effective limits (including maximum depth), and token breakdowns remain
available in the unchanged object projection when addressing one `agent_id`.
Aggregate usage counts each worker's own reported tokens once and reports its
coverage. Completion receipts additionally report measured descendant usage,
deduplicate continuation lineage, and distinguish unknown usage from zero. A worker's
`has_unreported_usage` and the descendant/subtree `unreported_usage_workers`
counts identify missing responses even when later responses provide a measured subtotal.

Request one worker's detail when investigating a failure:

```json
{"action":"status","agent_id":"child-a","detail":true,"offset":0,"limit":20}
```

Addressed `peek` also accepts `detail: true`. Detail remains bounded to 32 KiB;
message/event archives and deliverable verdicts are paged, and omission fields
identify truncated detail. Use the returned typed `transcript_handle` with
`handle_read` for the complete retained transcript. The handle's lookup
coordinates are preserved even when diagnostic prose is omitted. Unscoped
`detail: true` does not expand the entire roster into transcripts.

Artifacts are symbolic refs. Treat `result_summary` as a child self-report and
inspect the specific `verification.status` and its evidence before relying on
it. `usage.status` remains `unknown` until provider usage is reported, then
becomes `reported` or `budget_exhausted` for a spent token scope. Neither a
file's `present` verdict nor a completed lifecycle state proves a test gate.

## Output contract

Non-scout sub-agents end with five Markdown headings, in this order:

```
### SUMMARY    one paragraph; what you did and what happened
### EVIDENCE   path:line-range citations and key findings; one bullet each
### CHANGES    files modified, with one-line descriptions; "None." if read-only
### RISKS      what could go wrong / what the parent should double-check
### BLOCKERS   what stopped you; "None." if you finished cleanly
```

Use `### HEADING` lines, with `EVIDENCE` before `CHANGES`. List edited
repo-relative file paths under `### CHANGES`; blank lines before the bullets
are allowed. Begin each bullet with its file path, followed by a description;
quote paths containing spaces or literal trailing punctuation. The verifier
also accepts older explicit `CHANGES:`,
`Changed files:`, and `Files changed:` declarations. Evidence citations and
paths under `RISKS` are not declarations of edits. The five-heading prompt
contract is `SUBAGENT_OUTPUT_FORMAT` in
`crates/tui/src/prompts/text.rs`. `prompt_documents_structured_subagent_briefs`
in `crates/tui/src/prompts.rs` asserts every heading against it.

Scouts are the carve-out (#5189 F5): they end with `### SUMMARY` and
`### EVIDENCE` only (`SUBAGENT_SCOUT_OUTPUT_FORMAT` in
`crates/tui/src/prompts/text.rs`). `FleetRole::system_prompt` in
`crates/tui/src/tools/subagent/mod.rs` injects the scout contract for
`FleetRole::Scout` and the five-heading contract for every other role. A
subagent test pins that scouts contain `## Output contract (scout)` and do
not contain `### BLOCKERS`.

The parent reads `EVIDENCE` as a working set for the next turn, so
scouts and reviewers should be precise here.

## Memory and the `remember` tool (#489)

Sub-agents share the parent's native memory store when memory is enabled
(`[memory] enabled = true` or `DEEPSEEK_MEMORY=on`). They can
append durable notes via the `remember` tool — handy for a
scout that discovers a project convention worth carrying across
sessions, or a verifier that learns "this test is flaky".

`remember` takes a `scope` of `global` or `workspace`
(`crates/tui/src/tools/remember.rs`, schema enum plus scope handling) and
writes through `NativeMemoryStore` to `~/.codewhale/memory/global/MEMORY.md`
or `~/.codewhale/memory/workspace/<id>/MEMORY.md`. Writes do not go through
the standard write-approval flow. The legacy single-file `memory.md` path was
removed in v0.9.4; see `docs/MEMORY.md` for the full layout.

## Implementation notes

- Source: `crates/tui/src/tools/subagent/mod.rs`.
- Persisted state: `<workspace>/.codewhale/state/subagents.v1.json`. Schema
  version `1` (forward-compatible — new optional fields use
  `#[serde(default)]`).
- Settled records normally expire after `COMPLETED_AGENT_RETENTION`
  (default 1h), with a normal retained-record target of 256. Running /
  starting / waiting workers and the continuation identities and budget
  lineage needed by live work are preserved. Cleanup cannot discard an old
  ID while its continuation is still active or erase usage history needed
  to enforce an active scope.
- `SubAgentRuntime::background_runtime()` starts from `child_runtime()` but
  replaces the turn-scoped child token with a fresh cancellation token, so
  parent turn cancellation does not stop detached background sessions.
- The `is_running` check ignores agents whose `task_handle` is
  `None`; this avoids counting persisted-but-detached records
  toward the concurrency cap (#509).
- `SharedSubAgentManager` is `Arc<RwLock<...>>` — read paths use
  read locks so `/agents` and the workbar projection don't block
  the main loop during multi-agent fan-out (#510).

Personal profiles use the same format at
`$CODEWHALE_HOME/agents/<id>.toml` (normally `~/.codewhale/agents/`). For example:

```toml
# ~/.codewhale/agents/reasoner.toml
base_role = "explore"
provider = "openrouter"
model = "qwen/qwen3.7-plus"
reasoning_effort = "high"

[permissions]
allow_shell = false
trust = false
```

Select it with `agent(action: "start", profile: "reasoner", prompt: "...")`.
The provider must also be configured in `config.toml`. The receipt names the
resolved profile, its personal/project origin, provider/model and effective
reasoning effort. Effort is normalized to the selected model's supported tiers;
an explicit `thinking` request overrides the saved preference.

`allow_shell` and `trust` belong under `[permissions]`, not at the top level.
A profile cannot grant `allow_shell = true`, `trust = true`, or disable approval.
Use the appropriate `base_role` for the task; the parent session's live policy
remains the authority ceiling. These profile fields are not a way to grant
additional access.

A malformed, unreadable or duplicate profile now causes an explicit selection
error, including when its name matches a built-in role. It never silently
substitutes a lower roster layer. Repair the file and retry; profiles are reloaded
for each launch. `agent(action: "roster")` reports affected profile identities and
paths in `profile_load_issues` without exposing parser excerpts. Other valid
profiles remain available, and a valid project override still wins over a broken
personal definition. Fleet run creation performs the same check before storing
a run or launching workers.
