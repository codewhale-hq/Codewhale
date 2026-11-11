import type { DocsFleetDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/fleet/page.tsx`
 * ("Run a workflow"). Checked against `WorkflowCommand` and `LaneArgs` in
 * crates/cli/src/lib.rs, the Lane runtime backends in
 * crates/lane/src/runtime.rs, `FleetCommand` in crates/tui/src/lib.rs, and
 * docs/FLEET_WORKFLOW_TUTORIAL.md / docs/WORKFLOW_AUTHORING.md.
 */
export const docsFleet: DocsFleetDict = {
  metaTitle: "Run a workflow · Codewhale Docs",
  metaDescription:
    "Save roles and models in a Fleet, write a repeatable Workflow, run it as a Lane you can watch and stop, and run batches of tasks with durable workers.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Run a workflow",
  lede:
    "In Work, ask Codewhale to plan a larger task and delegate independent steps to agents. Write a Workflow when you want to repeat an ordered plan with phases, parallel branches and a record of each run.",
  sections: [
    {
      id: "fleet",
      title: "Save roles in a Fleet",
      blocks: [
        {
          p: "Fleet keeps reusable roles and model choices. Connect a model first, then create a role profile inside a session:",
        },
        { code: "/fleet setup\n/fleet\n/fleet saved", lang: "Codewhale" },
        {
          p: "`/fleet setup` saves one role profile at a time. Choose its model or inherit the session route, then choose this project or your personal profiles. Review the exact file before saving. `/fleet` shows the selected team; `/fleet saved` switches among named teams that already exist. Ask for a task and use `/fleet workers` to watch its session agents.",
        },
        {
          p: "A Fleet only chooses who does the work. What a worker may read, write, or run still comes from your workspace trust, [approval setting](/docs/modes), and sandbox.",
        },
      ],
    },
    {
      id: "write",
      title: "Write a workflow",
      blocks: [
        {
          p: "A saved Workflow describes a process the agents can repeat. Create a `workflows/` folder and save this declarative example as `workflows/docs_readiness.workflow.js`. It reviews two areas in parallel, then combines the findings. Replace `src` and `docs` with directories that exist in your project:",
        },
        {
          code: `export default workflow({
  "id": "docs-readiness",
  "goal": "Review the docs and code for gaps, then summarize the next edit",
  "nodes": [
    {
      "branch": {
        "id": "parallel-review",
        "parallel": true,
        "children": [
          { "agent": { "id": "code-review", "prompt": "Inspect src/ for undocumented behavior.",
                       "agent_type": "review", "mode": "read_only", "file_scope": ["src"] } },
          { "agent": { "id": "docs-review", "prompt": "Inspect docs/ for stale or missing steps.",
                       "agent_type": "review", "mode": "read_only", "file_scope": ["docs"] } }
        ]
      }
    },
    {
      "reduce": {
        "id": "summary",
        "inputs": ["code-review", "docs-review"],
        "prompt": "Combine the findings into the safest next edit."
      }
    }
  ]
});`,
          lang: "workflows/docs_readiness.workflow.js",
        },
        {
          p: "This declarative form supports `agent`, `branch`, `sequence`, `reduce`, `loop_until`, `cond`, `expand`, and `teacher_review`. It compiles to a validated plan and rejects constructs such as `import`, `fetch`, `eval`, and `async`. Advanced imperative JavaScript can use asynchronous `task()` and `parallel()` in a sandbox, still without direct filesystem, shell or network access. Workers perform those actions under the active policy.",
        },
        {
          note: "Workflow caps how many agents are live at once, and admitted work waits for a slot. There is no default cumulative child limit or structural depth limit. Set positive `[workflow] max_children` or `max_depth` values to limit them. Runtime agent admission and delegation limits apply separately. Loops must declare `max_iterations`.",
        },
      ],
    },
    {
      id: "run",
      title: "Run it",
      blocks: [
        {
          code: `codewhale workflow run docs-readiness --runtime inline`,
          lang: "Terminal",
        },
        {
          p: "This command finds `workflows/docs_readiness.workflow.js`, validates it and starts a model-backed run, so connect your provider first. `--runtime inline` waits in this terminal. The default backend requires `tmux` installed on the host and runs in a detached session. Add `--verify` to the launch command for the configured verifier gates after a successful finish. `--fleet <name>` selects an existing named Fleet.",
        },
        {
          p: "To keep the work off your checkout, add `--worktree-repo . --branch <name>`: the run gets its own git worktree and branch.",
        },
        {
          p: "Inside a session, `/workflow <objective>` drafts a proposal without executing it. To review this saved file, use `/workflow run workflows/docs_readiness.workflow.js`. Wait for the review, then run `/workflow confirm` to start the reviewed draft. `/workflow status` reads run state, `/workflow cancel <run-id>` cancels, and `/workflows` opens the session run dashboard.",
        },
      ],
    },
    {
      id: "watch",
      title: "Watch and stop a run",
      blocks: [
        { p: "Each run is a Lane. Lanes are saved to disk, so you can check on them from any terminal:" },
        {
          code: `codewhale lane list
codewhale lane status <lane-id>
codewhale lane logs <lane-id>
codewhale lane attach <lane-id>
codewhale lane interrupt <lane-id>`,
          lang: "Terminal",
        },
        {
          p: "`lane list`, `lane status`, and `lane interrupt` accept `--json`. Inside a session, use `/lane list`, `/lane status <lane-id>`, or `/lane interrupt <lane-id>`. Logs and tmux attachment use the shell commands above. Slash reads show the last recorded state; CLI list/status reconcile backend exits. Slash interrupt first returns a queued receipt, then its final result.",
        },
      ],
    },
    {
      id: "batch",
      title: "Run a batch of tasks",
      blocks: [
        {
          p: "When you have a list of separate tasks rather than one plan, write them as a task file and run them as a Fleet run. Each task names its goal, its role, and the paths it may write. [The tutorial](/docs/workflow-tutorial) has a complete `tasks.json`.",
        },
        {
          code: `codewhale fleet init
codewhale fleet run tasks.json --check
codewhale fleet run tasks.json --max-workers 4
codewhale fleet status
codewhale fleet logs <worker-id>
codewhale fleet resume <run-id>
codewhale fleet stop --all`,
          lang: "Terminal",
        },
        {
          p: "`fleet run --check` validates without creating a run; omit `--check` to start the foreground manager. Use another terminal for status and controls while it runs. After an interruption, `fleet resume <run-id>` reconciles stale leases and reports their state; it does not restart the manager or launch queued work. To explicitly retry a worker, inspect it and use `codewhale fleet restart <worker-id>`, which runs the manager for that run. `/fleet workers` and `/subagents` show current-session agents, not these durable runs.",
        },
      ],
    },
  ],
  next: [
    { href: "/docs/workflow-tutorial", label: "Read the full Fleet tutorial", note: "Validate a complete task file and inspect recovery behavior before starting workers." },
    {
      href: "/docs/subagents",
      label: "Run agents in parallel",
      note: "Hand independent pieces of one task to sub-agents without writing a workflow.",
    },
    {
      href: "/docs/review",
      label: "Review what changed",
      note: "Check the diff a run produced and get a review before you push.",
    },
    {
      href: "/docs/vocabulary",
      label: "Product terms",
      note: "Fleet, Workflow, Lane, and Runtime, each in one sentence.",
    },
  ],
  sourceNote:
    "Source documents: docs/FLEET.md, docs/FLEET_WORKFLOW_TUTORIAL.md, docs/WORKFLOW_AUTHORING.md · Update docs-map.ts when changing.",
};
