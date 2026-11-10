import type { DocsModesDict } from "../types";

export const docsModes: DocsModesDict = {
  metaTitle: "Set modes and approvals · Codewhale Docs",
  metaDescription:
    "Choose Plan or Work and set when Codewhale asks for approval. Compare the published v0.10.2 controls with the 0.10.3 source candidate.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Set modes and approvals",
  lede:
    "Plan lets you inspect and agree on an approach before making changes. Work can edit files and run commands within the permissions you choose.",
  sections: [
    {
      id: "versions",
      title: "Check which version you use",
      blocks: [
        { code: "codewhale --version", lang: "Shell" },
        {
          p: "The latest published release is v0.10.2. In that release, Tab with an empty composer cycles Plan → Work → Operate, and Shift+Tab cycles Ask → Auto-Review → Full Access. Operate coordinates larger tasks with the same permissions as Work; read the [v0.10.2 mode guide](https://github.com/codewhale-hq/CodeWhale/blob/v0.10.2/docs/MODES.md) for that release.",
        },
        {
          note: "The sections below describe the unpublished 0.10.3 source candidate. Installing the latest published release does not give you these new controls.",
        },
      ],
    },
    {
      id: "modes",
      title: "Choose Plan or Work in the source candidate",
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
          ],
        },
        {
          p: "Open `/mode` to choose a mode, or enter one of these commands. In the source candidate, `/mode operate` remains a compatibility alias for Work and explains that Operate was retired.",
        },
        { code: "/mode plan\n/mode work", lang: "Codewhale" },
        {
          p: "After a successful Plan response, Codewhale asks whether to continue in Work with Ask, Accept Edits or Auto-Review, or to keep planning. Switching to Work sends the approved plan as a follow-up.",
        },
      ],
    },
    {
      id: "approvals",
      title: "Set when the source candidate asks",
      blocks: [
        {
          p: "Press Shift+Tab to step one permission dial: Ask → Accept Edits → Plan → Auto-Review → Full Access → Ask. Plan is the Plan mode; every other stop uses Work. Tab only completes commands, file mentions and suggestions; it never changes modes or sends text.",
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
          note: "New source-candidate installs start with Accept Edits when no posture is saved. An explicit saved choice stays in control; managed approval policy can lock the setting. Use [Review what changed](/docs/review) to inspect edits and available restore points.",
        },
        {
          p: "The dial works while a turn is running unless another modal blocks it. A change applies at the agent's next step; a previously approved call runs only if the new posture is equal or broader. Each effective change appears in the transcript.",
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
  sourceNote: "The source-candidate controls follow docs/MODES.md, docs/KEYBINDINGS.md and docs/CONFIGURATION.md. The published-release link points to the v0.10.2 tag.",
};
