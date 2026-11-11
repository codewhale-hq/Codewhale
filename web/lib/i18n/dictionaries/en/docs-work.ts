import type { DocsWorkDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/work/page.tsx`
 * ("Track progress"). Checked against docs/GUIDE.md (workbar), docs/MODES.md
 * (goals), docs/TOOL_SURFACE.md (the To-do list is the one progress ledger),
 * and the `/workbar`, `/goal`, and `/relay` command definitions.
 */
export const docsWork: DocsWorkDict = {
  metaTitle: "Track progress · Codewhale Docs",
  metaDescription:
    "Follow a multi-step task in the workbar: the goal, the To-do list, and sub-agents. Set a goal that lasts across turns, and hand the work to a fresh session without losing your place.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Track progress",
  lede:
    "When a task takes more than one step, Codewhale keeps a To-do list and shows it in the workbar, next to the goal and any sub-agents. You can see what is done, what is in progress, and what is left, while the transcript keeps scrolling.",
  sections: [
    {
      id: "workbar",
      title: "Read the workbar",
      blocks: [
        {
          p: "The workbar sits under the composer by default. It holds the active goal, the To-do list, and the sub-agents working for this session. Finished items stay visible as done instead of disappearing. Select a row, or press Enter on it, to open its detail.",
        },
        {
          rows: [
            ["pending", "Not started yet."],
            ["in progress", "What Codewhale is doing now — one item at a time."],
            ["completed", "Done."],
            ["cancelled", "Dropped, and kept in the list so you can see it was."],
          ],
        },
        { p: "Move it or change what it shows:" },
        { code: "/workbar left\n/workbar bottom --save\n/workbar off", lang: "Codewhale" },
        {
          p: "Positions are `bottom`, `top`, `left`, `right`, and `off`. `--save` keeps your choice for future sessions.",
        },
      ],
    },
    {
      id: "goal",
      title: "Set a goal that lasts across turns",
      blocks: [
        {
          p: "A goal keeps one objective in view until it is met. Set it yourself, or let Codewhale set one when you ask for a clear end state, such as “until the tests pass.” It then shows one line saying so, and you stay in control:",
        },
        {
          code: `/goal make the CLI tests pass on Windows budget: 200000
/goal            # show progress
/goal pause
/goal resume
/goal done
/goal clear`,
          lang: "Codewhale",
        },
        {
          p: "The optional `budget:` is a token limit for the goal. A goal does not change your mode, approval setting, or model.",
        },
      ],
    },
    {
      id: "relay",
      title: "Continue in a fresh session",
      blocks: [
        {
          p: "When a session gets long, `/relay` writes a handoff for a new thread, including the current To-do list exactly as it stands, so the next session starts from the actual state of the work rather than a summary of it. Add a focus to steer the handoff:",
        },
        { code: "/relay finish the Windows test fixes", lang: "Codewhale" },
        {
          p: "Sub-agents started with the parent's context receive the same list, so they also know what is already done.",
        },
      ],
    },
    {
      id: "one-list",
      title: "Know which list counts",
      blocks: [
        {
          p: "There is exactly one progress list: the To-do. A plan Codewhale writes out — its approach, risks, and checks — explains how it will work but does not track progress, and the workbar does not show it as a second list.",
        },
        {
          p: "The list in the workbar is for you. Codewhale sees its own updates as ordinary results in the conversation; the list is not re-sent to the model on every step.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/subagents",
      label: "Run agents in parallel",
      note: "Hand independent To-do items to sub-agents.",
    },
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Choose a mode and decide when Codewhale asks for approval.",
    },
    {
      href: "/docs/review",
      label: "Review what changed",
      note: "Check the work behind each completed item.",
    },
  ],
  sourceNote:
    "Source documents: docs/GUIDE.md, docs/MODES.md, docs/TOOL_SURFACE.md, docs/TOOL_LIFECYCLE.md · Update docs-map.ts when changing.",
};
