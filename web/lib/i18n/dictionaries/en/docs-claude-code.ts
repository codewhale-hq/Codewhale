import type { DocsPageDict } from "../types";

export const docsClaudeCode: DocsPageDict = {
  metaTitle: "Bring your settings from another coding agent · Codewhale Docs",
  metaDescription: "Keep the project instructions and skill folders you already have, and review a settings and MCP import plan before applying it.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Bring your settings from another coding agent",
  lede: "Keep compatible project instructions and skill folders while choosing your own model connection. Review settings and plugin imports before applying them.",
  sections: [
    {
      "id": "instructions",
      "title": "Keep your project instructions",
      "blocks": [
        {
          "p": "AGENTS.md is the canonical project instruction file. If a project has only `CLAUDE.md` or `.claude/instructions.md`, Codewhale reads it as a compatibility fallback; neither overrides Codewhale security controls."
        },
        {
          "p": "Plain `.claude/skills/<name>/SKILL.md` folders are discovered through the normal [skills registry](/docs/skills), with workspace trust and invocation controls."
        }
      ]
    },
    {
      "id": "settings",
      "title": "Review a settings import",
      "blocks": [
        {
          "code": "/import-claude",
          "lang": "Codewhale"
        },
        {
          "p": "The command reads settings from an existing `.claude` folder and writes a reviewable report and unapplied config bundle. MCP servers use the existing import consent flow; permissions and hooks need separate review."
        },
        {
          "note": "Secret-shaped and unknown environment values are not imported or echoed. Running the plan does not enable servers or apply permissions."
        }
      ]
    },
    {
      "id": "plugins",
      "title": "Install the supported plugin subset",
      "blocks": [
        {
          "code": "/plugin install <spec>",
          "lang": "Codewhale"
        },
        {
          "p": "The declarative `.claude-plugin/plugin.json` subset supports skills, commands, agent profiles and MCP declarations. Codewhale review hashes and explicit trust and enablement still apply."
        },
        {
          "note": "Codewhale does not run plugin runtimes from other agents. Hooks, plugin LSP declarations, build steps and TypeScript agents are outside this importer."
        },
        {
          "p": "[Read the compatibility limits](https://github.com/codewhale-hq/CodeWhale/blob/v0.10.2/docs/CLAUDE_PLUGIN_COMPAT.md). Choose credentials and endpoints in [model setup](/models)."
        }
      ]
    }
  ],
  next: [
    {
      "href": "/docs/skills",
      "label": "Manage skills",
      "note": "Inspect compatible directories before importing."
    },
    {
      "href": "/docs/auth",
      "label": "Connect a provider",
      "note": "Use your chosen hosted or local model."
    }
  ],
  sourceNote: "Source: docs/CONFIGURATION.md, docs/CLAUDE_PLUGIN_COMPAT.md and the claude-settings-import entry in docs/features.toml at the published release tag."
};
