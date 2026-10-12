# Workflow Authoring

> 阅读简体中文版：[zh_hans/WORKFLOW_AUTHORING.md](zh_hans/WORKFLOW_AUTHORING.md)。

> **Ordinary multi-agent work does not require this file.** In Operate, send
> normal messages. Small work stays direct; multiple delegated steps use a
> compact Workflow plan with dependencies, bounded scopes, and completion
> evidence. Fleet manages the same sub-agents and roles. One bounded,
> independent task can use a direct agent, with `followup` for continued work.
> Act/Agent may also use optional soft-auto launch. See
> [Automatic Workflows](AUTOMATIC_WORKFLOWS.md).

Workflow has one runtime boundary: authored source lowers to typed
Rust `WorkflowSpec`, Rust validates the IR, and the scheduler/headless worker
runtime executes leaves. Authoring languages do not get hidden authority to own
files, shell, network, providers, cancellation, or TUI state.

Compatibility launch paths on the `workflow` tool:

| Input | When to use |
|-------|-------------|
| `plan` | Structured goal / phases / children (preferred agent path) |
| `script` | Short inline JS the model owns |
| `source_path` | Checked-in `.workflow.js` / `.workflow.ts` in the workspace |

Use `agent(action="roster")` to inspect the saved Fleet models and roles before
assigning children. Native plan children accept `model` for a saved shortlist
selector, or `role`/`profile` for a saved assignment. Named Exact Fleets keep
their member routes fixed and reject per-step model overrides. Plan children
also accept `cwd`, a repository-relative working directory — required in
multi-repository workspaces so the child (and worktree isolation) resolves the
right repository, mirroring `task({cwd})`.

For a guided walkthrough from fleet task specs to Workflow authoring and
monitoring, see [fleet + Workflow Tutorial](FLEET_WORKFLOW_TUTORIAL.md).


## Access model

The Workflow script is a **coordinator only**. It has no filesystem or shell of
its own. Real work happens in sub-agents the script launches.

| Layer | What it can access |
|-------|--------------------|
| Workflow script (JS VM) | Script variables, branching/loops, `task()` / `parallel()` / `pipeline()`, `phase` / `log`, `budget` / `args`. **No** direct FS, shell, network, env, imports, clock, or randomness. |
| Workflow-spawned sub-agents | Normal tool surface (read/search/edit/write, shell, web, MCP) subject to role posture, allowlists, and parent policy. File edits for write-capable roles auto-accept under Workflow; shell / web / MCP still require parent auto-approve or fail closed. |
| Parent session | Working directory, configured tools/MCP, permission mode, sandbox/network rules. |

### Scale

- Up to **16 concurrent** live agents in one run (additional spawns wait for a slot).
- Up to **1_000 agents per run** (VM lifetime spawn cap).
- Configured `max_children` and `max_concurrent` can narrow these limits.
- Automatic launch is model-judged on scope; the host enforces only the hard `max_children` / `max_depth` ceilings.
- Plan the population the work needs and let the host queue and clamp it.
  These ceilings are enforcement, not a reason to pre-shrink a valid plan.

See the Workflow JS sandbox tests for the fail-closed host surface inventory.

## Language Choice

| Surface | Strength | Tradeoff | Stance |
|---|---|---|---|
| YAML / JSON IR | Simple, reviewable, no runtime | Verbose for generated workflows | Keep as interchange/debug format |
| JavaScript | Familiar object syntax and easy agent generation | Unsafe if executed as a general runtime | First-class authoring through declarative compile-only subset |
| TypeScript | Best editor/types story for workflow SDK | Needs stripping/typechecking if full TS is supported | Same compile-only subset for now; richer SDK later |

The default high-capability path is TypeScript/JavaScript authoring, but only as
a compile step. The compiler accepts a JSON-compatible object inside
`workflow({...})` from `.workflow.js` or `.workflow.ts`, lowers it to
`WorkflowSpec`, and runs the Rust validation gate. (Starlark authoring was a
bootstrap reference and has been removed; Workflow authoring is JS-only.)

## Contract

Accepted source shape:

```js
export default workflow({
  "id": "issue-audit-js",
  "goal": "Audit an issue fix with parallel agents",
  "nodes": [
    {
      "branch": {
        "id": "parallel-audit",
        "children": [
          { "agent": { "id": "code-audit", "prompt": "Review code", "agent_type": "review" } },
          { "agent": { "id": "test-audit", "prompt": "Review tests", "agent_type": "verifier" } }
        ]
      }
    },
    { "reduce": { "id": "summary", "inputs": ["code-audit", "test-audit"], "prompt": "Summarize" } }
  ]
});
```

Supported node wrappers: `agent`, `branch`, `sequence`, `reduce`,
`teacher_review`, `loop_until`, `cond`, and `expand`. Raw `WorkflowNode` JSON IR
with `kind` / `spec` also remains valid.

An `agent` node may declare `"profile": "reviewer"` to run as a named fleet
roster profile. The name is trimmed and lowercased at compile time and must be
a single token (no whitespace, quotes, or `=`); the saved roster is resolved at
dispatch time, and explicit fields on the agent override profile defaults.

The runtime `task()` surface also accepts `cwd` for an existing repository-
relative working directory. This is required when a workflow is launched from
a multi-repository workspace and the child needs shell or file access. `cwd`
is validated by the host, does not grant mutation authority, and should be
paired with `worktree: true` when the child needs an isolated checkout.

The compiler rejects effectful constructs such as `import`, `require`, `fetch`,
`process`, `Deno`, `Bun`, `child_process`, file reads/writes, `eval`, `async`,
and `await`. This is intentionally stricter than JavaScript: workflow source is
a familiar declaration format, not a second execution runtime. The denied
effects are not denied to the run — put them in a child worker, which has
the full tool surface, and keep the script to coordination.

## Gates

A gate fires on a role's completion (`on: role_complete`) and blocks the role
named in `blocks_role` until the gate passes. That makes `blocks_role` a
**different** role from the one being gated — a gate that verifies phase 2's
work must block phase 3, never phase 2:

```js
// Correct: verify the implementer, block the next stage.
gates: [{
  id: "verify-fix",
  gate: "verify",
  on: "role_complete",
  role: "implement",
  blocks_role: "verify",
  on_fail: "escalate",
  max_retries: 1,
  require_explicit_verdict: true,
}]
```

A gate that blocks its own role can never pass, so the plan is rejected when it
is submitted, before any child is dispatched; the error names the gate and both
fields. A `blocks_role` that names a role from an **earlier** phase is not
caught there and still deadlocks the run. That failure reads
`spawn rejected: workflow gate blocks role \`implement\`: waiting for required
 gate outcome`, the run ends `Failed`, and because the blocked phase never
started, **the work its children would have done never happens and produces no
result** — the phases that already ran are the only salvageable part. Retrying
with the same plan reproduces it, so treat the message as a plan defect, not a
transient dispatch error.

Two habits keep this from costing a run:

- Read the gate block back before launch and check that every `blocks_role`
  names a role in a **later** phase than the gated `role`. With at most one
gate, `blocks_role` is never the gated role itself.
- Make each phase's result usable on its own. A localize/implement/verify split
  survives a dispatch failure in the last phase when the middle phase's
  findings were already returned, and a phase that only writes files is worth
  nothing if it never starts.

## Verification

- `cargo test -p codewhale-workflow --locked javascript`

Current example: `workflows/issue_audit.workflow.js`.

## Agent-Written fleet Workflows

The primary product flow is not "ask the user to write a script." The main
agent should decide when a task deserves workflow orchestration, draft the
Workflow source, show the plan for the current permission mode, and then let
the runtime compile and monitor it.

Workflow owns the plan: phases, branches, loops, reducers, and intermediate
results. fleet owns the durable roster, member identity, semantic role, and
saved provider/model pins or inheritance. Runtime owns tool posture, launch
concurrency, leases, heartbeats, logs, receipts, and resume/stop/restart
controls. In other words, a workflow selects fleet members and monitors their
Runtime runs; it isn't an executor, because the script has no shell or
filesystem of its own — effects live in the workers.

Workflow-to-Runtime launch validation applies a conservative default shape
before any Workflow IR is lowered to selected workers:

- up to 1,000 total worker agents per Workflow run;
- up to 16 live worker agents at once; larger populations queue (block) on the
  host's per-run concurrency gate until a live slot frees, then select through
  fleet and execute through Runtime;
- Workflow IR structural nesting no deeper than 5;
- Runtime child delegation defaults to 3 levels and has an opt-in hard ceiling
  of 8; that execution budget is independent of Workflow IR shape;
- loops require `max_iterations`;
- dynamic `expand` nodes require `max_children` and a template.

Those limits distinguish population from instantaneous launch concurrency. A
valid 1,000-agent Workflow can still drain through a smaller Runtime worker
pool. Model selection stays per member: a DeepSeek preset can suggest
`deepseek-v4-pro` for the orchestrator and `deepseek-v4-flash` for nearby
workers, but users and agents may override any slot when the task calls for it.

## Experimental search is a Workflow option

Experimental search generalizes the existing best-of-N recipe without adding a
new product mode, scheduler, or sub-agent API. The proposed search spec would
freeze the objective, baseline, model request and resolved version, public
evidence, evaluator hash, hard gates, scoring rule, budgets, write scope,
rounds, and review-only integration policy before admission; it is a design,
not shipped code.

The current JS starter supports structured generation and read-only review with
`strategy: "search"`. Runtime-owned command gates, hidden evaluation, benchmark
scoring, and clean-baseline replay are an explicit host seam still to wire; a
candidate's self-verdict must never be promoted into evaluator truth. See
[Workflow Experimental Search](WORKFLOW_EXPERIMENTAL_SEARCH.md).
