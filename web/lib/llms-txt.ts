import releasedIndex from "../data/agent-docs.release-index.json";
import { claimAvailability, releaseView, runtimeCapabilityContext } from "./capabilities";
import { APP_COPY } from "./content/app-page";
import { INSTALL_COMMANDS, INSTALL_COPY as I, INSTALL_PAGE_COMMANDS as C } from "./content/install";
import { PRODUCT_MAP_STATE_KEY, PRODUCT_MAP_TEXT_KEYS, productMap } from "./content/product-map";
import { DOC_TOPICS, REPO_DOCS_BASE, docTopicHref, getTopic, type DocTopic } from "./docs-map";
import { BUILD_FACTS, type RepoFacts } from "./facts";
import { EN_HOME as home, fill } from "./i18n/dictionaries";
import { computerUse } from "./i18n/dictionaries/en/computer-use";
import { DISCORD_URL, REPO_RELEASES_URL, REPO_URL } from "./i18n/links";
import { SITE_NAME, SITE_URL } from "./page-meta";
import { stateLabel, surfaceState, type SurfaceContext } from "./surface-states";

export interface AgentTextInput {
  facts?: RepoFacts;
  surface?: SurfaceContext;
}

type Copy = { readonly en: string };

/**
 * First-party routes that are in the sitemap but are not a docs-map topic.
 * English canonicals only — this file is the machine-readable index.
 */
const EXTRA_PAGES: readonly { path: string; title: string; description: string }[] = [
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
    // The page's own English copy, so the index cannot drift from it (W01-03).
    path: "/computer-use",
    title: computerUse.title,
    description: computerUse.metaDescription,
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
    path: "/ratatui",
    title: "Codewhale Ratatui",
    description:
      "Explore Codewhale’s reusable Ratatui components. Compare terminal themes and widths, inspect real rendered previews, and use the Rust examples in your app.",
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

function appPage(surface: SurfaceContext): string {
  const state = (id: "desktop" | "hosted-web") => stateLabel(surfaceState(id, surface), "en");
  return `- [Desktop and browser apps](${SITE_URL}/en/app): ${APP_COPY.surfaces.desktop.en}: ${state("desktop")}. ${APP_COPY.surfaces.browser.en}: ${state("hosted-web")}.`;
}

function installLines(facts: RepoFacts): string[] {
  const release = facts.latestPublishedRelease;
  const version = release?.version ?? null;
  const ref = release?.tag ?? "main";
  const uninstallReleased = claimAvailability({ feature: "unix-uninstall" }, runtimeCapabilityContext(facts)) === "released";
  const t = (copy: Copy) => fill(copy.en, { version: version ?? "" });
  const p = (copy: Copy) => [t(copy), ""];
  const h = (copy: Copy, level = 2) => [`${"#".repeat(level)} ${t(copy)}`, ""];
  const code = (cmd: string) => ["```", cmd, "```", ""];
  const link = (copy: Copy, href: string) => [`[${t(copy)}](${href})`, ""];
  const bullets = (rows: readonly Copy[]) => [...rows.map((row) => `- ${t(row)}`), ""];
  return [
    `- macOS and Linux: \`${INSTALL_COMMANDS.shell}\``,
    `- ${t(I.windowsTitle)}: \`${INSTALL_COMMANDS.windows}\``,
    `  ${t(I.windowsNote)}`,
    `- ${t(I.npmTitle)}: \`${INSTALL_COMMANDS.npm}\``,
    `- ${t(I.cargoTitle)}: \`${INSTALL_COMMANDS.cargo}\``,
    `  ${t(I.cargoNote)}`,
    "",
    ...link(I.scriptLink, `${SITE_URL}/install.sh`),
    ...(version ? p(I.releaseLine) : []),
    ...h(I.newTitle),
    ...p(I.newLead),
    ...h(I.newMacTitle, 3),
    `1. ${t(I.newMac1)}`,
    `2. ${t(I.newMac2)}`,
    "",
    ...code(INSTALL_COMMANDS.shell),
    `3. ${t(I.newMac3)}`,
    "",
    ...h(I.newWinTitle, 3),
    `1. ${t(I.newWin1)}`,
    `2. ${t(I.newWin2)}`,
    "",
    ...code(INSTALL_COMMANDS.windows),
    `3. ${t(I.newWin3)}`,
    "",
    ...link(I.newNext, `${SITE_URL}/en/docs/first-time`),
    ...h(I.pathTitle),
    ...p(I.pathPlaces),
    ...bullets([I.pathZsh, I.pathBashLinux, I.pathBashMac, I.pathOther, I.pathFish]),
    ...p(I.pathBlockIntro),
    ...code(C.pathBlock),
    ...p(I.pathBlockAfter),
    ...code(C.pathNow),
    ...p(I.pathOptOut),
    ...code(C.noModifyPath),
    ...p(I.pathManualIntro),
    ...code(C.pathManual),
    ...h(I.verifyTitle),
    ...p(version ? I.verifyBody : I.verifyBodyUnknown),
    ...code(C.version),
    ...h(I.firstTitle),
    ...[I.firstStep1, I.firstStep2, I.firstStep3].map((row, index) => `${index + 1}. ${t(row)}`),
    "",
    ...link(I.quickstart, `${SITE_URL}/en/docs/guide`),
    ...h(I.usageTitle),
    ...p(I.usageBody),
    ...code(C.telemetryOff),
    ...p(I.usageInstaller),
    ...code(C.installTelemetryOff),
    ...link(I.usageLink, `${SITE_URL}/en/legal/privacy#usage-counting`),
    ...h(I.updateTitle),
    ...p(I.updateBody),
    ...code(C.update),
    ...h(I.removeTitle),
    ...(uninstallReleased
      ? [...p(I.removeNow), ...code(C.uninstall), ...p(I.removeFilesNow)]
      : p(version ? I.removeFilesOnly : I.removeFilesOnlyUnknown)),
    ...code(C.removeFiles),
    ...p(I.removePath),
    ...p(I.removeData),
    ...code(C.removeData),
    ...(version ? p(I.removeGuide) : []),
    ...h(I.troubleTitle),
    ...bullets([I.troubleNotFound, I.troubleQueued, I.troubleLegacy]),
    ...link(I.troubleMore, `${SITE_URL}/en/docs/troubleshooting`),
    ...h(I.channelsTitle),
    ...p(version ? I.channelsLead : I.channelsLeadUnknown),
    ...h(I.windowsTitle, 3),
    ...code(INSTALL_COMMANDS.windows),
    ...p(I.windowsVerify),
    ...p(I.windowsPackage),
    ...p(I.windowsSandbox),
    ...p(I.windowsUpdate),
    ...h(I.npmTitle, 3),
    ...code(INSTALL_COMMANDS.npm),
    ...p(I.npmBody),
    ...p(I.npmEacces),
    ...code(C.npmPrefix),
    ...p(I.npmUpdate),
    ...h(I.cargoTitle, 3),
    ...code(INSTALL_COMMANDS.cargo),
    ...p(I.cargoBody),
    ...p(I.cargoUpdate),
    ...code(C.cargoUpdate),
    ...h(I.brewTitle, 3),
    ...code(C.brewInstall),
    ...p(I.brewBody),
    ...p(I.brewUpdate),
    ...code(C.brewRemove),
    ...h(I.mirrorsTitle),
    ...p(I.mirrorsNpm),
    ...p(I.mirrorsCnb),
    ...code(C.cnbVerify),
    ...link(I.mirrorsGuide, `${REPO_URL}/blob/${ref}/docs/CNB_MIRROR.md`),
    ...link(I.fullGuide, `${REPO_URL}/blob/${ref}/docs/INSTALL.md`),
  ];
}

function mapLines(facts: RepoFacts, surface: SurfaceContext): string[] {
  return productMap({ surface, capabilities: runtimeCapabilityContext(facts) }).map((row) => {
    const keys = PRODUCT_MAP_TEXT_KEYS[row.id];
    return `- ${home[keys.name]}: ${home[PRODUCT_MAP_STATE_KEY[row.state]]}.${row.sentenceHolds ? ` ${home[keys.sentence]}` : ""}`;
  });
}

function sitePage(id: string): string {
  const topic = getTopic(id);
  if (!topic) throw new Error(`missing docs topic: ${id}`);
  return `- [${topic.label.en}](${SITE_URL}${docTopicHref(topic, "en")})`;
}

function topicDescription(topic: DocTopic): string {
  return topic.id === "install" ? I.metaDescription.en : topic.description.en;
}

function provenance(facts: RepoFacts): string {
  const read = facts.latestPublishedRelease?.readAt;
  return `Facts generated ${facts.generatedAt.slice(0, 10)}${facts.sourceRevision ? ` from source revision ${facts.sourceRevision}` : ""}${read ? `; published release read ${read.slice(0, 10)}` : ""}`;
}

function factLines(facts: RepoFacts): string[] {
  const release = facts.latestPublishedRelease;
  const lines: string[] = [];
  if (release) lines.push(`- Published release: ${release.tag} (${release.url}).`);
  if (facts.license) lines.push(`- License: ${facts.license} (${REPO_URL}/blob/main/LICENSE). Free to use; you pay your model provider directly, and local models need no key.`);
  if (facts.defaultModel) lines.push(`- Default model id: \`${facts.defaultModel}\` (DeepSeek). Every provider and model is selectable with /provider and /model.`);
  const released = releaseView(facts);
  if (released.counts) {
    lines.push(`- Built-in provider routes in ${release?.tag ?? "the published release"} (${released.counts.providerRoutes}): ${released.providers.map((provider) => provider.label).join(", ")}. Any OpenAI-compatible endpoint also works.`);
  }
  if (facts.sandboxBackends.length > 0) lines.push(`- OS command sandbox backends: ${facts.sandboxBackends.join("; ")}.`);
  lines.push(`- ${provenance(facts)}; feature registry: ${SITE_URL}/features.json.`);
  return lines;
}

export function buildInstallMarkdown(input: AgentTextInput = {}): string {
  const facts = input.facts ?? BUILD_FACTS;
  return [
    `# ${I.metaTitle.en}`, "",
    `${home.sectionStartHeading} ${home.sectionStartBody}`, "",
    ...installLines(facts),
    `${provenance(facts)}.`, "",
  ].join("\n");
}

export function buildLlmsTxt(input: AgentTextInput = {}): string {
  const facts = input.facts ?? BUILD_FACTS;
  const surface = input.surface ?? {};
  const release = facts.latestPublishedRelease;
  const lines = [
    `# ${SITE_NAME}`, "", `> ${home.heroTitle}`, "",
    fill(home.heroIntro, { brand: SITE_NAME }), "",
    release ? `The latest release is v${release.version}.` : `The latest release is unknown; see ${REPO_RELEASES_URL}.`, "",
    "## Install", "",
    ...installLines(facts),
    `## ${home.mapHeading}`, "",
    home.sectionDirectionBody, "",
    ...mapLines(facts, surface), "",
    "## Main pages", "",
    sitePage("install"),
    sitePage("guide"),
    sitePage("providers"),
    `- [Changelog](${SITE_URL}/en/changelog)`,
    `- [${home.readTheCode}](${REPO_URL})`, "",
    "## Facts", "",
    ...factLines(facts), "",
    "## Agent entry points", "",
    `- [Feature catalogue](${SITE_URL}/en/features): Source capabilities, status, surfaces and documentation.`,
    `- [Feature JSON](${SITE_URL}/features.json): The generated feature registry, including its source hash.`,
    `- [Full documentation](${SITE_URL}/llms-full.txt): Every documentation topic as plain text.`,
    `- [Runtime API](${SITE_URL}/docs-markdown/runtime-api.md): HTTP/SSE API and ACP integration.`,
    `- [MCP](${SITE_URL}/docs-markdown/mcp.md): Connect tools or run codewhale mcp-server / codewhale serve --mcp. Default server tools are read-only file_read and search.`,
    "", "## Plugins and MCP", "",
    `- [Choose an integration](${SITE_URL}/en/plugins): Skills are instructions; plugins package reviewed capabilities; MCP servers supply external tools, resources and prompts.`,
    `- [Connect MCP servers](${SITE_URL}/en/docs/mcp): Local stdio and remote Streamable HTTP, legacy SSE, environment-backed tokens and direct-server OAuth. Plugin-bundled MCP does not support OAuth.`,
    `- [Computer Use](${SITE_URL}/en/computer-use): Mac helper setup, CLI-version limitations and its separate open-source repository.`,
    `- [Public plugin catalog](https://github.com/codewhale-hq/codewhale-plugin-marketplace): Installed CLI releases bundle a dated snapshot; current catalog entries are not proof of installed versions or end-to-end qualification.`,
    "", "## Documentation", "",
  ];
  for (const topic of DOC_TOPICS.filter((entry) => releasedIndex.slugs.includes(entry.slug))) {
    lines.push(`- [${topic.label.en}](${SITE_URL}/docs-markdown/${topic.slug}.md): ${topicDescription(topic)}`);
  }
  lines.push("", "## Optional", "");
  for (const topic of DOC_TOPICS) {
    lines.push(`- [${topic.label.en} on the site](${topic.hasPage ? `${SITE_URL}${docTopicHref(topic, "en")}` : `${REPO_DOCS_BASE}/${Array.isArray(topic.repoSource) ? topic.repoSource[0] : topic.repoSource}`}): ${topicDescription(topic)}`);
  }
  lines.push(appPage(surface));
  for (const page of EXTRA_PAGES) {
    lines.push(`- [${page.title}](${SITE_URL}/en${page.path}): ${page.description}`);
  }
  lines.push(`- [GitHub](${REPO_URL})`, `- [Discord](${DISCORD_URL})`, "");
  return lines.join("\n");
}
