import type { DocsSubagentsDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/subagents/page.tsx`
 * ("Run agents in parallel"). Roles follow the current Fleet role table in
 * docs/SUBAGENTS.md (general/explore/planner/reviewer/implement/test/
 * advisor/custom); limits follow crates/tui/src/config/subagent_limits.rs
 * and docs/CONFIGURATION.md (`max_subagents`, `[subagents]`).
 */
export const docsSubagents: DocsSubagentsDict = {
  metaTitle: "Run agents in parallel · Codewhale Docs",
  metaDescription:
    "Let Codewhale hand independent parts of a task to sub-agents, choose their roles, keep their edits in separate worktrees, and watch them work.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Run agents in parallel",
  lede:
    "Codewhale can hand a focused piece of work to a sub-agent and keep going while it runs. Use this when a task splits into independent parts — mapping a codebase, reviewing a change, running tests — so they happen at the same time.",
  sections: [
    {
      id: "ask",
      title: "Ask for it",
      blocks: [
        {
          p: "In Work, ask Codewhale in plain words to delegate independent parts of a task. Say what you want split up:",
        },
        {
          code: `Use three explore agents in parallel: one maps the auth code,
one maps the billing code, one lists the tests that cover both.
Then summarize what would break if we changed the session format.`,
          lang: "Prompt",
        },
        {
          p: "Each sub-agent starts in the background and reports back when it finishes. Your composer stays free, and the parent turn picks up the results.",
        },
      ],
    },
    {
      id: "roles",
      title: "Pick a role",
      blocks: [
        {
          p: "A role is the stance a sub-agent takes. Name one in your request, or let Codewhale choose. A sub-agent never gets more permission than your session has.",
        },
        {
          rows: [
            ["general", "Does whatever the brief says; may edit and run commands. The default."],
            ["explore", "Read-only. Maps the relevant code fast — “find every caller of this function.”"],
            ["planner", "Designs an approach without changing anything."],
            ["reviewer", "Reads and grades a change, with a severity for each finding."],
            ["implement", "Lands one specific change with the smallest edit."],
            ["test", "Runs tests and checks, then reports pass or fail. Does not edit code."],
            ["advisor", "Short, careful second opinion on a judgment call. No commands."],
            ["custom", "Only the tools you list, for tightly limited jobs."],
          ],
          codeTerms: true,
        },
        {
          p: "To give a role a specific model every time, save it with `/fleet setup` — see [Run a workflow](/docs/fleet).",
        },
      ],
    },
    {
      id: "worktrees",
      title: "Keep parallel edits apart",
      blocks: [
        {
          p: "When two sub-agents would edit the same repository, ask for each to work in its own worktree. Codewhale creates a fresh git worktree and branch for that agent beside your repository, under `.codewhale-worktrees/`, so your checkout stays clean until you merge.",
        },
        {
          p: "A worktree is isolation, not permission: a writer still needs a writing role and the paths it may change. When a new write claim overlaps an active claim in the same checkout, the host refuses the new claimant; it does not rewind or pause the existing writer.",
        },
      ],
    },
    {
      id: "watch",
      title: "Watch and answer them",
      blocks: [
        { code: "/subagents", lang: "Codewhale" },
        {
          p: "`/subagents` (the same view as `/fleet workers`) lists the agents attached to this session and what each one is doing. Select one to read its transcript.",
        },
        {
          p: "Sub-agents follow your [approval setting](/docs/modes) and may be more restricted. In Ask, a call that needs approval appears in your session while the agent waits. Auto-Review handles tool approvals through policy and eligible independent review; blocked calls or failed reviews are refused. Check the child transcript for decisions and refusal reasons. OS and connected-service permissions may still need your action.",
        },
      ],
    },
    {
      id: "limits",
      title: "Know the limits",
      blocks: [
        {
          rows: [
            ["Running at once", "64 by default. Set `max_subagents` (up to 128) in `~/.codewhale/config.toml`."],
            ["Queued plus running", "Up to 1,024 admitted by default, configurable with `[subagents] max_admitted`. Admitted agents wait for an execution slot; launches beyond the admission limit are refused."],
            ["Nesting", "No default product depth cap. Set `[subagents] max_depth` to an owner-selected limit; child requests cannot widen an inherited restriction."],
          ],
        },
        {
          p: "These are capacity controls, not targets. Keep each agent's scope clear. Durable [Fleet runs](/docs/fleet) record work across manager exits, but recovery is explicit: reconcile stale leases, inspect the result, then choose whether to restart a worker. They do not keep executing while your machine is asleep.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/fleet",
      label: "Run a workflow",
      note: "Turn a plan you repeat into a Workflow with phases and a record of each run.",
    },
    {
      href: "/docs/work",
      label: "Track progress",
      note: "How the To-do list shows what is done, in progress, and left.",
    },
    {
      href: "/docs/review",
      label: "Review what changed",
      note: "Check what the agents changed before you keep it.",
    },
  ],
  sourceNote: "Source documents: docs/SUBAGENTS.md, docs/FLEET.md, docs/MODES.md · Update docs-map.ts when changing.",
};
