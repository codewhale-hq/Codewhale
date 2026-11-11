import type { DocsPageDict } from "../types";

export const docsSkills: DocsPageDict = {
  metaTitle: "Manage skills · Codewhale Docs",
  metaDescription: "Find, install and trust SKILL.md instruction packages. Control user and model invocation and import compatible skill folders from other coding agents.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Manage reusable skills",
  lede: "Skills are SKILL.md instruction packages. Discover compatible folders, then review the source and trust before using a skill.",
  sections: [
    {
      "id": "manager",
      "title": "Open the skills manager",
      "blocks": [
        {
          "code": "/skills\n/skills manage\n/skills inspect\n/skill <name>",
          "lang": "Codewhale"
        },
        {
          "p": "The manager opens without network access and shows owned roots. Turn on compatible-root scanning to inspect external folders; importing targets a Codewhale-owned root."
        },
        { note: "`/skill <name>` activates instructions for the next turn; activation alone is not a model reply or proof that the model followed them. The plugin tutorial includes a greeting marker you can check after explicitly sending a message."
        }
      ]
    },
    {
      "id": "install",
      "title": "Install into an owned folder",
      "blocks": [
        {
          "p": "Write and share a skill with the [two-file plugin tutorial](/docs/plugin-authoring)."
        },
        {
          "code": "/skill install --project <spec>\n/skill install --global <spec>\n/skill trust --project <name>",
          "lang": "Codewhale"
        },
        {
          "p": "Owned roots are .codewhale/skills in your project and ~/.codewhale/skills globally. Compatible .agents/skills and .claude/skills directories are read-only import sources."
        },
        {
          "note": "Project skills load only after you trust that workspace. A configured external skills_dir stays read-only; choosing a folder does not grant authority to mutate it."
        }
      ]
    },
    {
      "id": "invocation",
      "title": "Choose who can load a skill",
      "blocks": [
        {
          "p": "Frontmatter disable-model-invocation: true removes a skill from model discovery. user-invocable: false hides it from user menus and refuses explicit activation; setting both disables both paths."
        },
        {
          "p": "A skill body enters context when loaded. The compact catalogue contains bounded names, aliases and invocation metadata."
        }
      ]
    }
  ],
  next: [
    {
      "href": "/docs/claude-code",
      "label": "Bring your settings from another coding agent",
      "note": "Bring compatible skills and review a settings import."
    },
    {
      "href": "/plugins",
      "label": "Add plugins",
      "note": "Install supported bundles through review and trust."
    }
  ],
  sourceNote: "Source: docs/SKILLS.md and docs/CLAUDE_PLUGIN_COMPAT.md."
};
