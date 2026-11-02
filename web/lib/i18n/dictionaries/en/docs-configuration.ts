import type { DocsConfigurationDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/configuration/page.tsx`
 * ("Change settings"). Checked against `ConfigCommand` and the `--set`
 * runtime overrides in crates/cli/src/lib.rs, and docs/CONFIGURATION.md
 * (config path, `/config audit`, per-project overlay, legacy paths).
 */
export const docsConfiguration: DocsConfigurationDict = {
  metaTitle: "Change settings · Codewhale Docs",
  metaDescription:
    "Find Codewhale's config file, change a setting from the session or the shell, try one for a single run, and see what a repository is allowed to override.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Change settings",
  lede:
    "Most settings can be changed without opening a file: from the session with `/config`, or from the shell with `codewhale config`. This page shows where settings live, how to change them, and what a repository can and cannot change for you.",
  sections: [
    {
      id: "file",
      title: "Find the config file",
      blocks: [
        {
          p: "Your settings live in `~/.codewhale/config.toml`. Print the exact path, or open the file in your editor:",
        },
        { code: "codewhale config path\ncodewhale config edit", lang: "Terminal" },
        {
          p: "To use a different file, pass `--config <path>` or set `CODEWHALE_CONFIG_PATH`; the flag wins when both are set. Codewhale was once called DeepSeek-TUI: if only an old `~/.deepseek/` folder exists, it is still read, and new settings are always written to `~/.codewhale/`.",
        },
      ],
    },
    {
      id: "change",
      title: "Change a setting",
      blocks: [
        {
          p: "Inside a session, `/config` opens the settings editor and `/settings` opens the settings screen. `/config audit` lists which settings can change in this session, which can be saved, and which only take effect after a restart — check it before editing by hand.",
        },
        { p: "From the shell:" },
        {
          code: `codewhale config get tools
codewhale config set telemetry false
codewhale config unset telemetry
codewhale config dump       # the settings in effect, secrets redacted
codewhale config doctor     # unknown keys, empty secrets, malformed URLs`,
          lang: "Terminal",
        },
        {
          p: "`config set` handles its named keys; for anything else it tells you which TOML table to edit instead of guessing.",
        },
      ],
    },
    {
      id: "one-run",
      title: "Try a setting for one run",
      blocks: [
        {
          p: "`--set KEY=VALUE` changes a setting for one launch and saves nothing. It accepts `provider`, `model`, `verbosity`, `approval_policy`, `sandbox_mode`, and `telemetry`, and can be repeated.",
        },
        {
          code: 'codewhale --set model=deepseek-flash --set sandbox_mode=read-only',
          lang: "Terminal",
        },
      ],
    },
    {
      id: "project",
      title: "Share settings with a repository",
      blocks: [
        {
          p: "A repository can include `.codewhale/config.toml` to suggest settings to everyone who works in it. Only a few keys are honored, and safety settings can only get stricter:",
        },
        {
          rows: [
            ["model", "The default model for this repository."],
            ["reasoning_effort", "For example `\"high\"` for a complex codebase."],
            ["approval_policy, sandbox_mode", "Only values stricter than yours."],
            ["allow_shell", "`false` turns shell commands off; `true` is ignored."],
            ["max_subagents", "Fewer parallel sub-agents, from 1 to 128."],
            ["notes_path", "Keep notes in the repository."],
          ],
          codeTerms: true,
        },
        {
          p: "Keys, endpoints, provider choice, MCP servers, hooks, skills, and extra instruction files always come from your own config, so a cloned repository cannot point Codewhale at its own server or files. Start with `--no-project-config` to ignore a repository's file for one launch.",
        },
        {
          p: "Project instructions — how an agent should work in this repository — belong in `AGENTS.md` instead. Run `/init` to create one.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "Connect a provider",
      note: "Save a key and see which one Codewhale is using.",
    },
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "The settings most people change first.",
    },
    {
      href: "/docs/hooks",
      label: "Run commands on events",
      note: "Add your own scripts to the `[hooks]` table.",
    },
  ],
  sourceNote:
    "Source documents: docs/CONFIGURATION.md, docs/LEGACY_PATHS.md · Update docs-map.ts when changing.",
};
