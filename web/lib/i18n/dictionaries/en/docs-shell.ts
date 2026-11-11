import type { DocsShellDict } from "../types";

/**
 * English reference dictionary for the docs shell: the portal hero in
 * `app/[locale]/docs/layout.tsx`, the hub metadata, the task/topic search,
 * the sidebar and breadcrumb chrome, the release-truth band, the contextual
 * help band that closes every docs page, the shared page-body chrome, and the
 * session recording panel.
 */
export const docsShell: DocsShellDict = {
  metaTitle: "Docs · Codewhale",
  metaDescription:
    "Install Codewhale, connect a provider, and get work done: modes and approvals, reviewing changes, workflows, sub-agents, MCP tools, hooks, the Runtime API, and troubleshooting.",
  heroTitle: "Install and use Codewhale",
  heroLead:
    "Find installation steps, model setup, tool configuration and troubleshooting. The guides include commands and examples you can run.",
  installCta: "Install Codewhale",

  releaseLabel: "Release",
  releasePublished: "Latest release {tag} · {date}",
  releaseDescribes: "These pages describe {tag}, the published release.",
  sourceLead:
    "This section describes behavior that exists in the project's source and is not in the published release. Check which version you have with `codewhale --version`.",
  releaseChangelog: "Changelog →",

  searchLabel: "Search the documentation",
  searchPlaceholder: "Search guides and reference",
  searchLoading: "Loading the documentation index. Your search stays in this browser.",
  searchUnavailable: "The full index could not load. Try again to search article text and reference commands.",
  searchRetry: "Load the index again",
  searchClear: "Clear",
  searchMatches: "{matched} of {total} entries match “{query}”",
  searchNoMatches: "Nothing matches “{query}”",
  tasksHeading: "By task",
  tasksLead: "Start from what you are trying to do.",
  topicsHeading: "By topic",
  webGuideTag: "Web guide",
  sourceDocTag: "Source doc",
  sourceDetails: "Details",
  emptyTitle: "No matching entry",
  emptyBody:
    "Try a different word — searches match English and Chinese — or browse the complete docs directory on GitHub.",
  emptyCta: "GitHub docs directory ↗",
  indexNote:
    "Web guides open on codewhale.net. Source docs open the full reference in the GitHub repository.",

  sidebarHeading: "Documentation",
  sidebarAria: "Documentation index",
  breadcrumbAria: "Breadcrumb",
  breadcrumbHome: "Home",
  breadcrumbDocs: "Docs",

  helpTitle: "Get help",
  helpLead:
    "Ask a question on Discord or report a documentation problem on GitHub.",
  helpSource: "Source: {name}",
  helpTroubleshooting: "Fix a problem",
  helpFaq: "FAQ",
  helpDiscord: "Ask on Discord ↗",
  helpIssue: "Report a docs problem ↗",

  nextHeading: "Next",
  noteLabel: "Note:",
  onThisPage: "On this page",

  mediaPendingNote:
    "There is no recording yet. When there is, it goes here with captions, a transcript, and an optional GIF download.",
  mediaPlanLink: "Recording plan and acceptance checklist ↗",
  mediaGifFallback: "GIF fallback download (no-video environments)",
  mediaTranscript: "Transcript ↗",
};
