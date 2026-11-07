import type { DocsModesDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/modes/page.tsx`
 * ("Set modes and approvals"). Checked against docs/MODES.md (modes,
 * postures, persistence), docs/INSTALL.md §10 (approval keys, Ask applies
 * workspace edits), and docs/CONFIGURATION.md (`[approval]`).
 */
export const docsModes: DocsModesDict = {
  metaTitle: "Set modes and approvals · Codewhale Docs",
  metaDescription:
    "Choose Plan, Work, or Operate for the kind of work, and Ask, Auto-Review, or Full Access for how often Codewhale stops to ask you.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Set modes and approvals",
  lede:
    "Two separate controls decide what Codewhale does on its own. The mode sets the kind of work — look, change, or coordinate. The approval setting decides when it stops to ask you. Change either one from the keyboard at any time between turns.",
  sections: [
    {
      id: "modes",
      title: "Pick a mode",
      blocks: [
        {
          rows: [
            [
              "Plan",
              "Look and plan only. Codewhale can read the workspace and research, but it cannot edit files or run shell commands. Use it to understand a codebase or agree on an approach first.",
            ],
            [
              "Work",
              "Everyday coding. Codewhale reads, edits, and runs commands, within the approval setting you chose. This is the default.",
            ],
            [
              "Operate",
              "Larger goals. Same permissions as Work, but Codewhale plans named steps, hands independent steps to sub-agents in parallel, and checks each result before it reports done.",
            ],
          ],
        },
        {
          p: "Press Tab with an empty composer to cycle Plan → Work → Operate, or type `/mode` to open the picker. You can also switch directly:",
        },
        { code: "/mode plan\n/mode work\n/mode operate", lang: "Codewhale" },
        {
          p: "The mode you pick becomes the mode your next session starts in. Codewhale refuses mode changes while a turn is running; press Esc to stop the turn first.",
        },
      ],
    },
    {
      id: "approvals",
      title: "Choose when it asks",
      blocks: [
        {
          p: "Press Shift+Tab to cycle Ask → Auto-Review → Full Access. Plan is always read-only, whatever you pick here.",
        },
        {
          rows: [
            [
              "Ask",
              "The default. File edits inside the workspace are applied and shown to you as a diff. Shell commands and other consequential tools stop for your approval.",
            ],
            [
              "Auto-Review",
              "Reviews tool actions automatically, but may still ask you to resolve a choice. Calls that are provably safe run. Publishing and destructive background actions are always blocked. Anything else gets one independent model review; high-risk calls and failed reviews are denied, not run.",
            ],
            [
              "Full Access",
              "No approval prompts. Repository rules and managed policy still block what they block. Use it only in a workspace you trust.",
            ],
          ],
        },
        {
          note: "Ask applies workspace file edits without asking. Commit or stash anything you care about before you start, and use [Review what changed](/docs/review) to check or roll back edits.",
        },
      ],
    },
    {
      id: "prompt",
      title: "Answer an approval prompt",
      blocks: [
        {
          p: "When Codewhale stops at an approval, it shows the exact command. Answer with one key:",
        },
        {
          rows: [
            ["y", "Allow this call once."],
            ["a", "Allow it for the rest of this session."],
            ["n", "Deny it. Codewhale is told the call was refused."],
            ["Esc", "Stop the whole turn."],
          ],
          codeTerms: true,
        },
        {
          p: "The highlighted option on a new prompt is Deny, so pressing Enter without reading refuses the call. To change that, or to deny prompts that wait too long, set it in `~/.codewhale/config.toml`:",
        },
        {
          code: `[approval]
default_selection = "allow_once"   # default: "deny"
timeout_seconds = 300              # default: wait indefinitely`,
          lang: "config.toml",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/review",
      label: "Review what changed",
      note: "See the diff for a session and roll files back to an earlier turn.",
    },
    {
      href: "/docs/sandbox",
      label: "Limit what commands can touch",
      note: "An approval is not a sandbox. See what the operating system enforces on each platform.",
    },
    {
      href: "/docs/subagents",
      label: "Run agents in parallel",
      note: "What Operate does with independent steps, and how to watch it.",
    },
  ],
  sourceNote: "Source documents: docs/MODES.md, docs/INSTALL.md §10, docs/CONFIGURATION.md · Update docs-map.ts when changing.",
};
