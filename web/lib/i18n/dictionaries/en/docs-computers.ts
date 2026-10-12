import type { DocsComputersDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/computers/page.tsx`
 * ("Send a task to the cloud"). Checked against `DispatchArgs` in
 * crates/cli/src/dispatch.rs. The compute vendor is not named in user copy.
 */
export const docsComputers: DocsComputersDict = {
  metaTitle: "Send a task to the cloud · Codewhale Docs",
  metaDescription:
    "Hand a coding task to a Codewhale cloud agent through your account — quoted first, started only when you agree.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Send a task to the cloud",
  lede:
    "A cloud agent takes a task off your machine: it works on cloud compute while you keep working locally. Nothing starts until you have seen the quote and agreed.",
  sections: [
    {
      id: "status",
      title: "Know what is ready",
      blocks: [
        {
          note: "Cloud agents are a preview. A task runs through your signed-in Codewhale account, so the quote and the compute consent below apply to every task.",
        },
      ],
    },
    {
      id: "before",
      title: "Before you start",
      blocks: [
        {
          list: [
            "Sign in to your Codewhale account with `codewhale login`. Dispatch always runs through the signed-in account.",
          ],
        },
      ],
    },
    {
      id: "send",
      title: "Quote, consent, then run",
      blocks: [
        {
          code: `codewhale dispatch "fix the flaky login test and open a PR"`,
          lang: "Terminal",
        },
        {
          p: "Codewhale shows the quoted task time for your account and asks you to agree that the repository code and any files you attach run on EU compute. Nothing starts until you agree. For scripts, pass `--yes --confirm-eu-compute` to answer both up front.",
        },
        {
          p: "Use `--seconds` to set the task time, `--agent` to pick an Agent, and `--context-file` or `--file` to attach context.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "Connect a provider",
      note: "Sign in to your Codewhale account and manage keys.",
    },
    {
      href: "/docs/review",
      label: "Review what changed",
      note: "Review the agent's pull request before you merge it.",
    },
    {
      href: "/docs/fleet",
      label: "Run a workflow",
      note: "Run longer, multi-step work on your own machine instead.",
    },
  ],
  sourceNote:
    "Source documents: docs/GUIDE.md, docs/CODEWHALE_AGENT.md · Update docs-map.ts when changing.",
};
