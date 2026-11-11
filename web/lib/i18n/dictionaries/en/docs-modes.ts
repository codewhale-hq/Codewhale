import type { DocsModesDict } from "../types";

export const docsModes: DocsModesDict = {
  metaTitle: "Set modes and approvals · Codewhale Docs",
  metaDescription:
    "Choose Plan, Work or Operate and set when Codewhale asks for approval. You decide what it may do without asking.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Set modes and approvals",
  lede:
    "Plan lets you inspect and agree on an approach before making changes. Work and Operate can edit files and run commands within the permissions you choose. You decide what it may do without asking.",
  sections: [
    {
      id: "versions",
      title: "Check which version you use",
      blocks: [
        { code: "codewhale --version", lang: "Shell" },
        {
          p: "This page describes the published release named at the top of the page. Behavior that exists only in the project's source is in the labelled section at the end.",
        },
      ],
    },
    {
      id: "modes",
      title: "Choose Plan, Work or Operate",
      until: { feature: "modes", describesCurrentRow: true },
      blocks: [
        {
          rows: [
            [
              "Plan",
              "Codewhale can read the workspace and perform permitted research, but it cannot edit files or run shell commands. Research may contact external services, and session state can still be saved.",
            ],
            [
              "Work",
              "Codewhale reads, edits files and runs commands within the approval, sandbox and policy boundaries you set. It can delegate independent tasks to other agents.",
            ],
            [
              "Operate",
              "The same tools and authority as Work, with more emphasis on delegating multi-part work to parallel agents and on checking results before calling the work done. It does not bypass the approval, sandbox or policy boundaries Work uses.",
            ],
          ],
        },
        {
          p: "With an empty composer, Tab cycles Plan → Work → Operate. Open `/mode` to choose a mode, or enter one of these commands:",
        },
        { code: "/mode plan\n/mode work\n/mode operate", lang: "Codewhale" },
        {
          p: "After a successful Plan response, Codewhale asks whether to continue in Work with Ask or Auto-Review, or to keep planning. Switching to Work sends the approved plan as a follow-up.",
        },
      ],
    },
    {
      id: "approvals",
      title: "Set when Codewhale asks",
      until: { feature: "permission-postures", describesCurrentRow: true },
      blocks: [
        {
          p: "Press Shift+Tab to cycle the permission posture: Ask → Auto-Review → Full Access. A fresh install starts in Ask. The posture is separate from the mode.",
        },
        {
          rows: [
            [
              "Ask",
              "The default. Codewhale asks before a command runs. Saved approvals and policy rules still apply.",
            ],
            [
              "Auto-Review",
              "Provably safe calls run, while the deterministic safety floor blocks publishing and destructive background actions. Other held calls receive one independent model review; high-risk calls and failed reviews are denied. Codewhale may still ask you to resolve a user choice.",
            ],
            [
              "Full Access",
              "Commands can reach anything on this computer without approval prompts. Managed policy and repository law still apply.",
            ],
          ],
        },
        {
          note: "Use [Review what changed](/docs/review) to inspect edits and available restore points.",
        },
      ],
    },
    {
      id: "prompt",
      title: "Answer an approval prompt",
      blocks: [
        {
          p: "Read the proposed tool call and the options on its card before answering. Ordinary tool approval cards support these keys when the matching option is offered:",
        },
        {
          rows: [
            ["y", "Allow this call once."],
            ["a", "Choose the displayed session approval option when it is offered."],
            ["p", "Save the displayed exact repository allow rule when it is offered."],
            ["n", "Deny the call and tell Codewhale it was refused."],
            ["Esc", "Stop the current turn on its own approval card. On a child agent's card, hide the card while its approval remains pending."],
          ],
          codeTerms: true,
        },
        {
          p: "Workflow plan cards have different options, including editing or canceling the plan. Follow the labels on that card rather than the ordinary tool shortcuts above.",
        },
        {
          p: "A new approval card highlights Deny by default, so Enter refuses the call unless you changed that setting. Approval prompts wait indefinitely by default; set a timeout in your user configuration to deny unanswered prompts after 300 seconds:",
        },
        {
          code: `[approval]\ndefault_selection = "deny"\ntimeout_seconds = 300`,
          lang: "config.toml",
        },
        {
          p: "The default user configuration is `~/.codewhale/config.toml`; see [Change settings](/docs/configuration) if you use a custom configuration path. Prompt settings change how a required card behaves, rather than granting permission to tools.",
        },
      ],
    },
  ],
  sourceGroups: [
    {
      claim: { feature: "modes", describesCurrentRow: true },
      sections: [
        {
          id: "source-modes",
          title: "Plan and Work",
          blocks: [
            {
              p: "Plan and Work are the two modes. `/mode operate` still works as an alias for Work and explains that Operate was retired. Tab only completes commands, file mentions and suggestions; it never changes modes or sends text.",
            },
            { code: "/mode plan\n/mode work", lang: "Codewhale" },
            {
              p: "After a successful Plan response, Codewhale asks whether to continue in Work with Ask, Accept Edits or Auto-Review, or to keep planning.",
            },
          ],
        },
      ],
    },
    {
      claim: { feature: "permission-postures", describesCurrentRow: true },
      sections: [
        {
          id: "source-approvals",
          title: "One permission dial",
          blocks: [
            {
              p: "Shift+Tab steps one permission dial: Ask → Accept Edits → Plan → Auto-Review → Full Access → Ask. Plan is the Plan mode; every other stop uses Work.",
            },
            {
              rows: [
                [
                  "Ask",
                  "Codewhale asks before every file edit and command, including edits inside the workspace. Saved approvals and policy rules still apply.",
                ],
                [
                  "Accept Edits",
                  "Ordinary file edits inside the workspace's git work tree can run without a prompt. Commands, network and MCP actions still ask, as do sensitive-file edits, edits outside the workspace and edits in a folder without git. ACP sessions treat this setting as Ask.",
                ],
              ],
            },
            {
              note: "New installs start with Accept Edits when no posture is saved. An explicit saved choice stays in control; managed approval policy can lock the setting.",
            },
            {
              p: "The dial works while a turn is running unless another modal blocks it. A change applies at the agent's next step; a previously approved call runs only if the new posture is equal or broader. Each effective change appears in the transcript.",
            },
          ],
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/review",
      label: "Review what changed",
      note: "Inspect session changes and restore available file snapshots.",
    },
    {
      href: "/docs/sandbox",
      label: "Limit what commands can touch",
      note: "See what the operating system enforces on each platform.",
    },
    {
      href: "/docs/subagents",
      label: "Run agents in parallel",
      note: "Delegate independent tasks and follow each agent's progress.",
    },
  ],
  sourceNote: "Source: docs/MODES.md, docs/KEYBINDINGS.md and docs/CONFIGURATION.md at the published release tag; the in-source section follows main.",
};
