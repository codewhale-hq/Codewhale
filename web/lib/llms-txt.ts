import { CHANGELOG } from "./changelog.generated";
import { GETTING_STARTED_STEPS } from "./content/getting-started";
import { DOC_TOPICS, REPO_DOCS_BASE, docTopicHref } from "./docs-map";
import { BUILD_FACTS, type RepoFacts } from "./facts";
import { computerUse } from "./i18n/dictionaries/en/computer-use";
import { DISCORD_URL, REPO_URL } from "./i18n/links";
import { IDENTITY_PHRASE, SITE_NAME, SITE_URL } from "./page-meta";

/**
 * First-party routes that are in the sitemap but are not a docs-map topic.
 * English canonicals only — this file is the machine-readable index.
 */
const EXTRA_PAGES: readonly { path: string; title: string; description: string }[] = [
  {
    path: "/product",
    title: "Product",
    description:
      "What Codewhale is and what a person gains: their own models, capable agents, and control on their own machine — with availability stated per surface.",
  },
  {
    path: "/docs",
    title: "Documentation",
    description:
      "Documentation hub: install, guide, configuration, providers, modes, tools, MCP, sandbox, Fleet, and the Runtime API.",
  },
  {
    path: "/faq",
    title: "FAQ",
    description:
      "Frequently asked questions: install, config, providers, models, modes, security, and privacy.",
  },
  {
    path: "/runtime",
    title: "Runtime & Integrations",
    description:
      "Local Runtime API over HTTP/SSE, ACP stdio over the existing Engine, MCP servers, an early VS Code companion, and a community VS Code GUI.",
  },
  {
    // The page's own English copy, so the index cannot drift from it (W01-03).
    path: "/computer-use",
    title: computerUse.title,
    description: computerUse.metaDescription,
  },
  {
    path: "/constitution",
    title: "Three layers of law",
    description:
      "Nested constitution: bundled base law, standing law (/constitution), and the repo's .codewhale/constitution.json.",
  },
  {
    path: "/roadmap",
    title: "Roadmap",
    description: "Shipped, underway, considered, and ruled-out work.",
  },
  {
    path: "/feed",
    title: "Activity",
    description: "Recent repository issues, pull requests, and releases.",
  },
  {
    path: "/digest",
    title: "Community digest",
    description: "The weekly project record, reviewed by a maintainer.",
  },
  {
    path: "/community",
    title: "Community",
    description: "Release contributors, helpers, and ways to take part.",
  },
  {
    path: "/contribute",
    title: "Contribute",
    description: "The pull-request workflow: scoped issue, fork, test the change, explain the result.",
  },
  {
    path: "/changelog",
    title: "Changelog",
    description:
      "Codewhale release record: the latest published release, the unreleased source candidate, and the notes for each version, drawn from CHANGELOG.md in the repository.",
  },
  {
    path: "/plugins",
    title: "Plugins and marketplace",
    description:
      "Preview Computer Use for selected desktop apps, and install reviewed plugins from the Codewhale catalog, a GitHub repository, or a tarball.",
  },
  {
    path: "/ratatui",
    title: "Codewhale Ratatui",
    description:
      "Explore Codewhale’s reusable Ratatui components. Compare terminal themes and widths, inspect real rendered previews, and use the Rust examples in your app.",
  },
  {
    path: "/signin",
    title: "Sign in",
    description:
      "Sign in to an existing Codewhale account. New accounts are not open; the terminal needs no account.",
  },
  {
    path: "/legal/terms",
    title: "Terms of service",
    description: "Terms that govern your use of Codewhale, a Shannon Labs product.",
  },
  {
    path: "/legal/privacy",
    title: "Privacy policy",
    description: "How Shannon Labs handles information when you use Codewhale.",
  },
];

const UNRELEASED_HEADLINE_SECTIONS = new Set(["Added", "Changed", "Fixed", "Security"]);

function unreleasedHeadlines(): string[] {
  const unreleased = CHANGELOG.find((release) => release.unreleased);
  if (!unreleased) return [];
  const headlines: string[] = [];
  for (const section of unreleased.sections) {
    if (!UNRELEASED_HEADLINE_SECTIONS.has(section.heading)) continue;
    for (const item of section.items) {
      const colon = item.indexOf(": ");
      if (colon <= 0 || colon > 140) continue;
      const headline = item.slice(0, colon).trim();
      if (/\(internal\)/i.test(headline)) continue;
      headlines.push(`${section.heading}: ${headline}`);
    }
  }
  return headlines;
}

function factLines(facts: RepoFacts): string[] {
  const release = facts.latestPublishedRelease;
  const lines = [
    release
      ? `- Latest published release: v${release.version}, published ${release.publishedAt.slice(0, 10)} (${release.url}). The install commands above install this release.`
      : "- Latest published release: unknown at build time; see https://github.com/codewhale-hq/CodeWhale/releases.",
  ];
  if (facts.license) lines.push(`- License: ${facts.license} (${REPO_URL}/blob/main/LICENSE). Free to use; you pay your model provider directly, and local models need no key.`);
  if (facts.defaultModel) lines.push(`- Default model id: \`${facts.defaultModel}\` (DeepSeek). Every provider and model is selectable with /provider and /model.`);
  if (facts.providers.length > 0) {
    lines.push(`- Built-in provider routes (${facts.providers.length}): ${facts.providers.map((provider) => provider.label).join(", ")}. Any OpenAI-compatible endpoint also works.`);
  }
  if (facts.sandboxBackends.length > 0) lines.push(`- OS command sandbox backends: ${facts.sandboxBackends.join("; ")}.`);
  lines.push(`- Facts generated ${facts.generatedAt.slice(0, 10)}${facts.sourceRevision ? ` from source revision ${facts.sourceRevision}` : ""}; feature registry: ${SITE_URL}/features.json.`);
  return lines;
}

export function buildLlmsTxt(): string {
  const facts = BUILD_FACTS;
  const release = facts.latestPublishedRelease;
  const headlines = unreleasedHeadlines();
  const lines = [
    `# ${SITE_NAME}`, "", `> ${IDENTITY_PHRASE}`, "",
    "Read, edit and test a project with a hosted or local model. Modes, approvals and sandbox policy control tool access.", "",
    "## Install", "",
    `- macOS and Linux: \`${GETTING_STARTED_STEPS[0].commands[0]}\``,
    "- npm (wraps the same release binaries): `npm install -g codewhale`",
    "- Cargo (crates.io; can trail GitHub Releases): `cargo install codewhale-cli --locked`",
    "- Windows x64: `winget install HunterBown.CodeWhale`",
    `- Then run \`codewhale\` from the root of a git repository. Full guide: ${SITE_URL}/en/install`, "",
    "## Facts", "",
    ...factLines(facts), "",
  ];
  if (headlines.length > 0) {
    lines.push(
      `## Unreleased on main${release ? ` (after v${release.version})` : ""}`, "",
      `Merged on main and not in a published release yet; the install commands do not include these. Changelog headlines, not the full list: ${SITE_URL}/en/changelog`, "",
      ...headlines.map((headline) => `- ${headline}`), "",
    );
  }
  lines.push(
    "## Agent entry points", "",
    `- [Feature catalogue](${SITE_URL}/en/features): Source capabilities, status, surfaces and documentation.`,
    `- [Feature JSON](${SITE_URL}/features.json): The generated feature registry, including its source hash.`,
    `- [Full documentation](${SITE_URL}/llms-full.txt): Every documentation topic as plain text.`,
    `- [Runtime API](${SITE_URL}/docs-markdown/runtime-api.md): HTTP/SSE API and ACP integration.`,
    `- [MCP](${SITE_URL}/docs-markdown/mcp.md): Connect tools or run codewhale mcp-server / codewhale serve --mcp. Default server tools are read-only file_read and search.`,
    "", "## Documentation", "",
  );
  for (const topic of DOC_TOPICS) {
    lines.push(`- [${topic.label.en}](${SITE_URL}/docs-markdown/${topic.slug}.md): ${topic.description.en}`);
  }
  lines.push("", "## Optional", "");
  for (const topic of DOC_TOPICS) {
    lines.push(`- [${topic.label.en} on the site](${topic.hasPage ? `${SITE_URL}${docTopicHref(topic, "en")}` : `${REPO_DOCS_BASE}/${Array.isArray(topic.repoSource) ? topic.repoSource[0] : topic.repoSource}`}): ${topic.description.en}`);
  }
  for (const page of EXTRA_PAGES) {
    lines.push(`- [${page.title}](${SITE_URL}/en${page.path}): ${page.description}`);
  }
  lines.push(`- [GitHub](${REPO_URL})`, `- [Discord](${DISCORD_URL})`, "");
  return lines.join("\n");
}
