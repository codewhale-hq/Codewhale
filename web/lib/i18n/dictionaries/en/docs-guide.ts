import type { DocsGuideDict } from "../types";

/**
 * English reference dictionary for `/docs/guide` ("Start your first task").
 * The four steps themselves live in `web/lib/content/getting-started.ts`,
 * shared with the homepage.
 */
export const docsGuide: DocsGuideDict = {
  metaTitle: "Start your first task · Codewhale Docs",
  metaDescription:
    "Install Codewhale, connect a model, and give it a task in your project. Add a Fleet later if you want several models and roles.",
  bodyClassName: "text-ink-soft leading-relaxed",
  overviewTitle: "Start your first task",
  overviewLead:
    "You need a terminal, an existing project folder, and a provider key or a running local model; replace /path/to/your/project with your folder. Install Codewhale, connect a model, and check its explanation of a real file before the optional Fleet setup.",
  sessionTitle: "Watch a real session",
  sessionLead: "Follow a task from the first request to the finished result.",
  nextTitle: "Next",
  sourceNote:
    "Source documents: docs/GUIDE.md, docs/INSTALL.md, docs/KEYBINDINGS.md · Update docs-map.ts when changing.",
};
