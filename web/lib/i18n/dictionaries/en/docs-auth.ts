import type { DocsAuthDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/auth/page.tsx`
 * ("Connect a provider"). Every command and lookup order is checked against
 * docs/INSTALL.md §8, docs/PROVIDERS.md (local models), and the CLI
 * definitions in crates/cli/src/lib.rs (`auth`, `login`) and
 * crates/account-cli/src/lib.rs (`account`).
 */
export const docsAuth: DocsAuthDict = {
  metaTitle: "Connect a provider · Codewhale Docs",
  metaDescription:
    "Give Codewhale a model: save a provider key, check which key is in use, or run a local model with no key. A Codewhale account is optional.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Connect a provider",
  lede:
    "Codewhale needs a model to answer. Save a key for a hosted provider, or point Codewhale at a model running on your own machine. You pay the provider directly; no Codewhale account is involved.",
  sections: [
    {
      id: "save-key",
      title: "Save a provider key",
      blocks: [
        {
          p: "Get an API key from your provider, then save it. Codewhale asks for the key and does not echo it. DeepSeek is the default provider, so it is the example here.",
        },
        {
          code: `codewhale auth set --provider deepseek
codewhale auth status --provider deepseek`,
          lang: "Terminal",
        },
        {
          p: "`auth status` names the source in use — config file, secret store, or environment variable — and shows only the last four characters. In a script, pipe the key in with `--api-key-stdin` instead of typing it.",
        },
        {
          p: "You can also connect from inside Codewhale: press F3 (or type `/provider`), choose a provider, paste the key, then pick a model.",
        },
        {
          note: "In v0.10.0, `auth set --provider deepseek` also switches your default model to DeepSeek Pro. Run `/model` if you want the faster, cheaper model back. Connecting through F3 keeps your current model.",
        },
      ],
    },
    {
      id: "which-key",
      title: "Know which key is used",
      blocks: [
        { p: "When a key is set in more than one place, the first match in this order wins:" },
        {
          steps: [
            "`--api-key` on the command line, for one run.",
            "`api_key` in `~/.codewhale/config.toml`.",
            "The secret store written by `codewhale auth set`.",
            "The provider's environment variable, such as `DEEPSEEK_API_KEY`.",
          ],
        },
        {
          p: "Exporting a new environment variable therefore does not replace a key you saved earlier. If a rotated key keeps failing, run `auth status` to see which source is active, then save the new key or clear the stored one:",
        },
        { code: "codewhale auth clear --provider deepseek", lang: "Terminal" },
        {
          p: "On Linux the secret store is a private file (mode 0600) under `~/.codewhale/secrets/`, not an OS keyring. `codewhale doctor --probe-api` makes one test call to confirm that the key and the network both work.",
        },
      ],
    },
    {
      id: "other-providers",
      title: "Use another provider or a local model",
      blocks: [
        {
          p: "`codewhale auth list` shows every provider Codewhale knows and whether each one has a key. The pattern is the same for all of them: `codewhale auth set --provider <name>`, or that provider's environment variable.",
        },
        {
          p: "Local runners — Ollama, vLLM, and SGLang — need no key by default, and your prompts stay on your machine. Start the runner, then choose it:",
        },
        {
          code: `codewhale auth list
codewhale --provider ollama --model <model-tag>`,
          lang: "Terminal",
        },
        {
          p: "The [models page](/models) lists providers, local setups, and how to switch models mid-session.",
        },
      ],
    },
    {
      id: "account",
      title: "Codewhale accounts",
      blocks: [
        {
          p: "A provider key and a Codewhale account are different things. Codewhale never needs an account, and new accounts are not open. Provider keys stay on your machine with `codewhale auth set`.",
        },
        {
          rows: [
            ["codewhale login", "Sign in through your browser with a one-time code."],
            ["codewhale account status", "Show which account this profile is signed in to."],
            ["codewhale account logout", "Remove this profile's session."],
          ],
          codeTerms: true,
        },
        {
          p: "`codewhale login` does not accept provider keys; those always go through `codewhale auth set`. The session is kept in your operating system's credential manager when one is available, and in the private Codewhale secrets file otherwise — for example over SSH or in a container.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/guide",
      label: "Start your first task",
      note: "Open Codewhale in a project and give it something concrete to do.",
    },
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Decide whether Codewhale asks before it runs commands.",
    },
    {
      href: "/docs/trust",
      label: "See what leaves your machine",
      note: "What the provider receives, what stays local, and how to turn usage counting off.",
    },
  ],
  sourceNote:
    "Source documents: docs/INSTALL.md §8, docs/PROVIDERS.md, docs/CODEWHALE_AGENT.md · Update docs-map.ts when changing.",
};
