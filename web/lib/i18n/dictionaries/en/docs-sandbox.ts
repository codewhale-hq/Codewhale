import type { DocsSandboxDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/sandbox/page.tsx`
 * ("Limit what commands can touch"). Checked against docs/SANDBOX.md — which
 * describes only behavior wired into the command execution path — and
 * docs/CONFIGURATION.md (`sandbox_mode`, `prefer_bwrap`).
 */
export const docsSandbox: DocsSandboxDict = {
  metaTitle: "Limit what commands can touch · Codewhale Docs",
  metaDescription:
    "See which operating-system sandbox wraps shell commands on macOS, Linux, and Windows, turn it on where it is optional, and choose how much a command may write.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Limit what commands can touch",
  lede:
    "Approving a command decides whether it runs. A sandbox decides what it can reach once it does. Codewhale uses the operating system's sandbox where one is available and tells you plainly when there is none.",
  sections: [
    {
      "id": "execution-policy",
      "title": "Inspect execution policy rules",
      "blocks": [
        {
          "code": "/permissions",
          "lang": "Codewhale"
        },
        {
          "p": "Typed permissions select User, Agent and BuiltinDefault rules in that order. Within one source layer, deny wins over ask and allow; specificity breaks ties."
        },
        {
          "p": "Hard denied command prefixes cannot be overridden by a typed allow. An allow never removes later safety checks, repository restrictions or sandbox limits."
        },
        {
          "p": "[Read the authorization order and matching rules](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/AUTHORIZATION_ORDER.md)"
        }
      ]
    },
    {
      id: "platforms",
      title: "Check what your platform provides",
      blocks: [
        {
          rows: [
            ["macOS", "Seatbelt, automatically, when its startup check succeeds. Commands get broad read access, writes limited by the sandbox mode, and network only when the mode allows it."],
            ["Linux", "Bubblewrap, automatically, once it is installed and verified working (opt out below). Without it, commands run with no OS sandbox, and Codewhale shows the workspace limit as unenforced."],
            ["Windows", "No OS sandbox. Codewhale shows the workspace limit as unenforced, and your approval setting and Windows permissions still apply."],
            ["External service", "With `sandbox_backend = \"opensandbox\"`, shell commands run on an OpenSandbox-compatible service you configure; its isolation is that service's to guarantee."],
          ],
        },
        { p: "Ask Codewhale which one it found:" },
        { code: "codewhale doctor\ncodewhale setup --status", lang: "Terminal" },
        {
          p: "Both report the sandbox that is actually available after your settings are applied. Codewhale never counts source code that is not wired in as a sandbox.",
        },
      ],
    },
    {
      id: "linux",
      title: "Enable (or turn off) the Linux sandbox",
      blocks: [
        { p: "Install bubblewrap — Codewhale uses it by default. To run Linux commands unwrapped instead, opt out with one line in `~/.codewhale/config.toml`:" },
        {
          code: "sudo apt install bubblewrap",
          lang: "Debian / Ubuntu — Terminal",
        },
        {
          code: "prefer_bwrap = false",
          lang: "~/.codewhale/config.toml — optional opt-out",
        },
        {
          p: "Codewhale uses `/usr/bin/bwrap` only when that file exists, is executable, and a probe shows it can create its sandbox namespaces — an installed-but-blocked bwrap is treated as absent, not used. Commands then see a read-only view of the system, write only where the sandbox mode allows, and have no network unless the mode enables it.",
        },
      ],
    },
    {
      id: "mode",
      title: "Choose how much a command may write",
      blocks: [
        { code: 'sandbox_mode = "workspace-write"', lang: "config.toml" },
        {
          rows: [
            ["read-only", "Commands can read but not write."],
            ["workspace-write", "Commands can write inside the workspace and temporary folders, and nowhere else."],
            ["danger-full-access", "No OS sandbox. Use only on a machine or container you are prepared to lose."],
            ["external-sandbox", "You are already running inside isolation, so Codewhale adds none of its own."],
          ],
          codeTerms: true,
        },
        {
          p: "The first two are enforced only where a sandbox is available — on Linux without bubblewrap, and on Windows, they are settings without an OS wrapper behind them. A repository's own config can make the mode stricter, never looser. For one headless run, pass `--sandbox <mode>` before the prompt to `codewhale exec`. By default, `--auto` enables Full Access: tools run without asking, shell commands have no Codewhale sandbox, and trust mode lets file tools write outside the workspace. Any `--sandbox` other than `danger-full-access` (or the equivalent `sandbox_mode` in config) keeps trust mode off. Use `--sandbox workspace-write` for the shell sandbox, where available, and file-tool workspace restrictions; `external-sandbox` relies on your own isolation.",
        },
      ],
    },
    {
      id: "limits",
      title: "Know the limits",
      blocks: [
        {
          list: [
            "Availability is checked before a command starts, but the sandbox can still fail at launch because of host policy or container restrictions.",
            "A “Permission denied” from a command is not proof that the sandbox blocked it. Codewhale labels a denial as the sandbox's only when the sandbox itself reported it.",
            "No sandbox protects against kernel vulnerabilities or every kind of resource exhaustion.",
          ],
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Decide which commands stop for your approval.",
    },
    {
      href: "/docs/trust",
      label: "See what leaves your machine",
      note: "What a provider receives, what stays local, and what telemetry sends.",
    },
    {
      href: "/docs/configuration",
      label: "Change settings",
      note: "Where these keys live and what a repository may override.",
    },
  ],
  sourceNote: "Source documents: docs/SANDBOX.md, docs/CONFIGURATION.md · Update docs-map.ts when changing.",
};
