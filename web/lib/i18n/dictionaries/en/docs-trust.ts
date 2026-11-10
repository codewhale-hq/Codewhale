import type { DocsTrustDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/trust/page.tsx`
 * ("See what leaves your machine"). Every claim traces to
 * docs/public-surface-facts.json (`trust`), docs/TELEMETRY.md, and
 * docs/SANDBOX.md. The security contact is code-owned in the page.
 */
export const docsTrust: DocsTrustDict = {
  metaTitle: "See what leaves your machine · Codewhale Docs",
  metaDescription:
    "What stays local, what a model provider receives, what usage counting sends and how to turn it off, where the audit log lives, and how to report a vulnerability.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "See what leaves your machine",
  lede:
    "Codewhale runs on your computer and talks to the model provider you choose. This page lists what goes where, what the anonymous usage count contains, and how to switch it off — as built today.",
  sections: [
    {
      "id": "preview-request",
      "title": "See the request before sending",
      "blocks": [
        {
          "p": "/preview-request shows a redacted manifest of the next primary turn. This human command never sends a request or writes conversation or Work state."
        },
        {
          "code": "/preview-request\n/preview-request json\n/preview-request json --prompt explain this project",
          "lang": "Codewhale"
        },
        {
          "note": "Without --prompt, route and body are unavailable. A configured classifier, hooks or pending runtime work can also prevent an exact preview; the manifest states why."
        },
        {
          "p": "[Read the request preview contract](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/PREVIEW_REQUEST.md)"
        }
      ]
    },
    {
      id: "boundaries",
      title: "Where your work goes",
      blocks: [
        {
          rows: [
            ["Stays on your machine", "The runtime, your workspace, session history, snapshots, and the audit log."],
            ["Goes to your provider", "The context each turn needs — your messages, the files and tool results Codewhale reads for that turn — goes directly to the provider you selected. There is no Codewhale relay in between."],
            ["Stays local entirely", "With a model you run yourself, inference never leaves your machine."],
            ["Needs no account", "Installing and running Codewhale locally needs no Codewhale account."],
            ["Plan mode", "Cannot edit files or run shell commands. Research it is allowed to do may still contact outside services."],
          ],
        },
        {
          p: "Tools you add can send data elsewhere: an MCP server, a web search, or a hook runs with your permissions and reaches whatever it reaches. Add only ones you trust.",
        },
      ],
    },
    {
      id: "telemetry",
      title: "Know what usage counting sends",
      blocks: [
        {
          p: "Codewhale counts anonymous usage by default and says so the first time you launch it, naming Codewhale and PostHog. Here is exactly what it covers:",
        },
        {
          rows: [
            ["Never sent", "Prompts, responses, code, diffs, file contents, file or repository or branch names, paths, model ids, MCP server names, API keys or tokens, error message text, keystrokes, or any per-turn or per-tool timeline."],
            ["Sent while on", "Version and platform classes, session length and outcome, feature and error counts as fixed categories, and a random install id that changes every 90 days."],
            ["Where it goes", "`https://telemetry.codewhale.net/v1/telemetry`, a first-party service whose source is in the repository. It stores no IP address, country, or location, keeps no request logs, and holds data for three months."],
            ["PostHog", "Forwarding to PostHog happens only if the service operator configures it separately; it carries the same fields and nothing more."],
          ],
        },
      ],
    },
    {
      id: "turn-off",
      title: "Turn usage counting off",
      blocks: [
        {
          code: `codewhale config set telemetry false   # stop, and erase the local id and buffer
CODEWHALE_TELEMETRY=0 codewhale        # stop for this process, erase nothing`,
          lang: "Terminal",
        },
        {
          p: "The config setting is the lasting choice: later versions keep it, and a command-line flag or environment variable cannot turn it back on. You can also switch it in `/settings`. Turning it off deletes what was kept on your machine; rows already sent are keyed only to the random id you just erased, and expire with the three-month window.",
        },
        {
          p: "To see exactly what would be sent without sending anything, set `telemetry_endpoint = \"\"` in `~/.codewhale/config.toml`. Each batch is then written to `$CODEWHALE_HOME/telemetry/dryrun.jsonl` on your machine, byte for byte, and no network connection is made.",
        },
      ],
    },
    {
      id: "audit",
      title: "Check the local audit log",
      blocks: [
        {
          p: "Credential, approval, and elevation events are appended to `$CODEWHALE_HOME/audit.log` (by default `~/.codewhale/audit.log`). Writing is best-effort: if a write fails, the failure is logged rather than hidden. The log never leaves your machine.",
        },
      ],
    },
    {
      id: "report",
      title: "Report a vulnerability",
      blocks: [
        {
          p: "Email security reports to the address below instead of opening a public issue. Include your Codewhale version (`codewhale --version`) and steps to reproduce if you have them.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/sandbox",
      label: "Limit what commands can touch",
      note: "What the operating-system sandbox enforces on each platform.",
    },
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Decide what Codewhale may do without asking.",
    },
    {
      href: "/docs/auth",
      label: "Connect a provider",
      note: "Choose who receives your turns — or keep them local with a local model.",
    },
  ],
  sourceNote:
    "Source documents: docs/public-surface-facts.json (trust), docs/TELEMETRY.md, docs/SANDBOX.md · Update docs-map.ts when changing.",
};
