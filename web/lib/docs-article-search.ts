import { getReferenceSearchSections } from "./reference-guides";
import type { DocSearchSection } from "./doc-search-match";
import {
  getDocsAuth, getDocsClaudeCode, getDocsConfiguration, getDocsFleet,
  getDocsHooks, getDocsMcp, getDocsModes, getDocsPet, getDocsReview,
  getDocsRuntimeApi, getDocsSandbox, getDocsSkills, getDocsSubagents,
  getDocsTroubleshooting, getDocsTrust, getDocsWeb, getDocsWork, getDocsFirstTime,
  getDocsGuide, getDocsVocabulary, getConstitution, getChrome, getHome, fill, pickText,
} from "./i18n/dictionaries";
import type { DocsBlock, DocsPageDict } from "./i18n/dictionaries/types";
import { GETTING_STARTED_STEPS } from "./content/getting-started";
import { INSTALL_COPY, INSTALL_COMMANDS, INSTALL_PAGE_COMMANDS } from "./content/install";
import { FACTS } from "./facts.generated";
import { MODELS_COPY, formatModelTokens, formatModelDate } from "./content/models";
import { PLUGINS_COPY, PLUGINS_COMING, pluginsComingApplies, PLUGIN_KINDS, CATALOG_FEATURED, PLUGIN_SOURCES, PLUGIN_COMMANDS, PLUGIN_COMPATIBILITY } from "./content/plugins";
import { CONNECTORS } from "./content/connectors.generated";
import { CONSTITUTION_LAYERS, CONSTITUTION_SCENES } from "./content/constitution";
import { getDocsAccount } from "./content/docs-account";
import type { LocalizedText } from "./content/vocabulary";
import { TOOLS_COPY, toolRowsFor } from "./content/tools";
import { resolveDocsPage } from "./docs-release";
import { runtimeCapabilityContext } from "./capabilities";
import { BUILD_FACTS } from "./facts";
import {
  ADVISORY_ROLE, MEASUREMENT_PRINCIPLES, PRODUCT_TERMS, ROUTE_IDENTITY, controlTermsFor,
} from "./content/vocabulary";

const ARTICLES: Record<string, (locale: string) => DocsPageDict> = {
  account: getDocsAccount, auth: getDocsAuth, "claude-code": getDocsClaudeCode, configuration: getDocsConfiguration,
  fleet: getDocsFleet, hooks: getDocsHooks, mcp: getDocsMcp, modes: getDocsModes,
  pet: getDocsPet, review: getDocsReview, "runtime-api": getDocsRuntimeApi,
  sandbox: getDocsSandbox, skills: getDocsSkills, subagents: getDocsSubagents,
  troubleshooting: getDocsTroubleshooting, trust: getDocsTrust, web: getDocsWeb,
  work: getDocsWork, "first-time": getDocsFirstTime,
};

function blockText(block: DocsBlock): string {
  if ("p" in block) return block.p;
  if ("code" in block) return block.code;
  if ("rows" in block) return block.rows.flat().join(" ");
  if ("steps" in block) return block.steps.join(" ");
  if ("list" in block) return block.list.join(" ");
  return block.note;
}

export function getDocSearchSections(locale: string): DocSearchSection[] {
  const context = runtimeCapabilityContext(BUILD_FACTS);
  const tools = toolRowsFor(context);
  const sections = Object.entries(ARTICLES).flatMap(([slug, getArticle]) => {
    const article = getArticle(locale);
    return [
      { id: "overview", title: article.title, text: article.lede },
      ...resolveDocsPage(article, context).sections.map((section) => ({
        id: section.id, title: section.title, text: section.blocks.map(blockText).join(" "),
      })),
    ].map((section) => ({
      href: `/docs/${slug}#${section.id}`, pageTitle: article.title,
      title: section.title, text: section.text,
    }));
  });
  const guide = getDocsGuide(locale);
  sections.push(...GETTING_STARTED_STEPS.map((step) => ({
    href: `/docs/guide#${step.id}`, pageTitle: guide.overviewTitle,
    title: pickText(step.title, locale),
    text: [pickText(step.body, locale), ...step.commands, step.prompt ? pickText(step.prompt, locale) : ""].join(" "),
  })));
  const vocabulary = getDocsVocabulary(locale);
  sections.push(
    { href: "/docs/tools#overview", pageTitle: pickText(TOOLS_COPY.title, locale), title: pickText(TOOLS_COPY.title, locale), text: [pickText(tools.released ? TOOLS_COPY.lead : TOOLS_COPY.leadSource, locale), ...tools.body.flatMap((row) => [row.name, pickText(row.detail, locale)])].join(" ") },
    { href: "/docs/tools#lsp", pageTitle: pickText(TOOLS_COPY.title, locale), title: pickText(TOOLS_COPY.lspTitle, locale), text: pickText(TOOLS_COPY.lspBody, locale) },
    { href: "/docs/tools#compatibility", pageTitle: pickText(TOOLS_COPY.title, locale), title: pickText(TOOLS_COPY.compatibilityTitle, locale), text: pickText(TOOLS_COPY.compatibility, locale) },
    { href: "/docs/vocabulary#execution-nouns", pageTitle: vocabulary.title, title: vocabulary.executionHeading, text: PRODUCT_TERMS.flatMap((row) => [row.term, pickText(row.short, locale), pickText(row.long, locale)]).join(" ") },
    { href: "/docs/vocabulary#control-nouns", pageTitle: vocabulary.title, title: vocabulary.controlHeading, text: controlTermsFor(context).body.flatMap((row) => [row.term, pickText(row.description, locale)]).join(" ") },
    { href: "/docs/vocabulary#route-identity", pageTitle: vocabulary.title, title: vocabulary.routeHeading, text: ROUTE_IDENTITY.flatMap((row) => [row.term, pickText(row.description, locale)]).join(" ") },
    { href: "/docs/vocabulary#advisory-role", pageTitle: vocabulary.title, title: vocabulary.advisoryHeading, text: `${ADVISORY_ROLE.term} ${pickText(ADVISORY_ROLE.description, locale)}` },
    { href: "/docs/vocabulary#measurement", pageTitle: vocabulary.title, title: vocabulary.measurementHeading, text: MEASUREMENT_PRINCIPLES.map((row) => pickText(row, locale)).join(" ") },
  );
  sections.push(...getCustomDocSearchSections(locale), ...getReferenceSearchSections(locale));
  return sections;
}

function getCustomDocSearchSections(locale: string): DocSearchSection[] {
  const t = (copy: LocalizedText) => fill(pickText(copy, locale), {
    version: FACTS.version ?? "—", model: FACTS.defaultModel ?? "—",
  });
  const section = (path: string, id: string, pageTitle: string, title: string, text: string[]): DocSearchSection => ({
    href: `${path}#${id}`, pageTitle, title, text: text.join(" "),
  });
  const installTitle = getHome(locale).sectionStartHeading;
  const modelTitle = t(MODELS_COPY.modelsTitle);
  const pluginTitle = t(PLUGINS_COPY.title);
  const constitution = getConstitution(locale);
  const sections = [
    section("/install", "install-overview", installTitle, installTitle, [
      getHome(locale).sectionStartBody, ...Object.values(INSTALL_COMMANDS), t(INSTALL_COPY.windowsNote),
      t(INSTALL_COPY.cargoNote), t(INSTALL_COPY.scriptLink),
    ]),
    section("/install", "install-path", installTitle, t(INSTALL_COPY.pathTitle), [
      t(INSTALL_COPY.pathPlaces), t(INSTALL_COPY.pathBlockAfter), t(INSTALL_COPY.pathOptOut), t(INSTALL_COPY.pathManualIntro),
      INSTALL_PAGE_COMMANDS.pathBlock, INSTALL_PAGE_COMMANDS.pathNow, INSTALL_PAGE_COMMANDS.noModifyPath,
    ]),
    section("/install", "install-verify", installTitle, t(INSTALL_COPY.verifyTitle), [
      t(INSTALL_COPY.verifyBodyUnknown), INSTALL_PAGE_COMMANDS.version,
    ]),
    section("/install", "install-first-task", installTitle, t(INSTALL_COPY.firstTitle), [
      t(INSTALL_COPY.firstStep1), t(INSTALL_COPY.firstStep2), t(INSTALL_COPY.firstStep3),
    ]),
    section("/install", "install-usage", installTitle, t(INSTALL_COPY.usageTitle), [
      t(INSTALL_COPY.usageBody), t(INSTALL_COPY.usageInstaller), INSTALL_PAGE_COMMANDS.installTelemetryOff,
    ]),
    section("/install", "install-update", installTitle, t(INSTALL_COPY.updateTitle), [t(INSTALL_COPY.updateBody)]),
    section("/install", "install-remove", installTitle, t(INSTALL_COPY.removeTitle), [
      t(INSTALL_COPY.removePath), t(INSTALL_COPY.removeData), INSTALL_PAGE_COMMANDS.removeFiles, INSTALL_PAGE_COMMANDS.removeData,
    ]),
    section("/install", "install-troubleshooting", installTitle, t(INSTALL_COPY.troubleTitle), [
      t(INSTALL_COPY.troubleNotFound), t(INSTALL_COPY.troubleQueued), t(INSTALL_COPY.troubleLegacy),
    ]),
    section("/install", "install-channels", installTitle, t(INSTALL_COPY.channelsTitle), [
      t(INSTALL_COPY.windowsPackage), t(INSTALL_COPY.windowsSandbox), t(INSTALL_COPY.npmBody), t(INSTALL_COPY.cargoBody),
      t(INSTALL_COPY.brewBody), INSTALL_PAGE_COMMANDS.brewInstall, INSTALL_PAGE_COMMANDS.npmUpdate,
    ]),
    section("/install", "mirrors", installTitle, t(INSTALL_COPY.mirrorsTitle), [
      t(INSTALL_COPY.mirrorsNpm), t(INSTALL_COPY.mirrorsCnb), INSTALL_PAGE_COMMANDS.cnbVerify,
    ]),
    section("/models", "models-overview", modelTitle, t(MODELS_COPY.title), [t(MODELS_COPY.lead)]),
    section("/models", "models-setup", modelTitle, t(MODELS_COPY.setupTitle), [
      t(MODELS_COPY.setupLead), t(MODELS_COPY.providerDocs),
      ...MODELS_COPY.steps.flatMap(step => [t(step.title), t(step.detail).replaceAll("`", "")]),
      t(MODELS_COPY.routeDeepseekTitle), t(MODELS_COPY.routeDeepseekBody), "DEEPSEEK_API_KEY",
      t(MODELS_COPY.routeFirstPartyTitle), t(MODELS_COPY.routeFirstPartyBody),
      t(MODELS_COPY.routeCompatTitle), t(MODELS_COPY.routeCompatBody), t(MODELS_COPY.routeCompatConfig).replaceAll("`", ""),
    ]),
    section("/models", "models-title", modelTitle, t(MODELS_COPY.modelsTitle), [
      t(MODELS_COPY.modelsLead), t(MODELS_COPY.colModel), t(MODELS_COPY.colContext), t(MODELS_COPY.colAdded),
      ...FACTS.models.flatMap(model => [model.id, model.provider ?? "", model.reasoning ? t(MODELS_COPY.reasoning) : "", formatModelTokens(model.contextWindow), formatModelDate(model.addedAt, getChrome(locale).dateLocale)]),
    ]),
    section("/models", "providers-title", modelTitle, t(MODELS_COPY.listTitle), [
      t(MODELS_COPY.provider), t(MODELS_COPY.id), t(MODELS_COPY.credential),
      ...FACTS.providers.flatMap(provider => [provider.label, provider.id, provider.env]),
      t(MODELS_COPY.missing), t(MODELS_COPY.request),
    ]),
    section("/plugins", "plugins-overview", pluginTitle, pluginTitle, [
      t(PLUGINS_COPY.lead), t(PLUGINS_COPY.marketplaceCta), t(PLUGINS_COPY.docsCta),
    ]),
    section("/plugins", "kinds-title", pluginTitle, t(PLUGINS_COPY.kindsTitle), [
      ...PLUGIN_KINDS.flatMap(kind => [t(kind.title), t(kind.detail)]),
      t(PLUGINS_COPY.originBefore) + t(PLUGINS_COPY.originName) + t(PLUGINS_COPY.originAfter), t(PLUGINS_COPY.dshStatus), t(PLUGINS_COPY.dshLink),
    ]),
    ...(pluginsComingApplies(runtimeCapabilityContext(BUILD_FACTS).release) ? [section("/plugins", "coming-title", pluginTitle, t(PLUGINS_COPY.comingTitle), [
      t(PLUGINS_COPY.comingLead), ...PLUGINS_COMING.flatMap(item => [t(item.title), t(item.detail)]),
    ])] : []),
    section("/plugins", "connectors-title", pluginTitle, t(PLUGINS_COPY.connectorsTitle), [
      t(PLUGINS_COPY.connectorsLead), ...CONNECTORS.map(connector => connector.name), t(PLUGINS_COPY.connectorsNote),
      t(PLUGINS_COPY.connectorsGoogle), t(PLUGINS_COPY.connectorsLink),
    ]),
    section("/plugins", "catalog-title", pluginTitle, t(PLUGINS_COPY.catalogTitle), [
      t(PLUGINS_COPY.catalogLead), ...CATALOG_FEATURED.flatMap(entry => [entry.title, t(entry.detail), `/plugin marketplace install codewhale ${entry.name}`]),
    ]),
    section("/plugins", "safe-title", pluginTitle, t(PLUGINS_COPY.safeTitle), [
      t(PLUGINS_COPY.safeLead), ...PLUGINS_COPY.safeSteps.flatMap(step => [t(step.title), step.command, t(step.detail)]),
      t(PLUGINS_COPY.safeHashes), t(PLUGINS_COPY.sourcesLead), PLUGIN_SOURCES,
    ]),
    section("/plugins", "reference-title", pluginTitle, t(PLUGINS_COPY.referenceTitle), [
      ...PLUGIN_COMMANDS, ...PLUGIN_COMPATIBILITY.flatMap(format => [t(format.title), t(format.detail)]),
    ]),
    section("/plugins", "contribute-title", pluginTitle, t(PLUGINS_COPY.contributeTitle), [
      t(PLUGINS_COPY.contributeLead), t(PLUGINS_COPY.docsCta), t(PLUGINS_COPY.contributeLink),
    ]),
    section("/constitution", "constitution-overview", constitution.title, constitution.title, [
      constitution.kicker, constitution.titleAside, constitution.lede, constitution.since, constitution.sinceBody,
    ]),
    section("/constitution", "constitution-rank", constitution.title, constitution.rankTitle, [
      constitution.rankScope, ...CONSTITUTION_LAYERS.flatMap(layer => [t(layer.name), t(layer.path), t(layer.body)]),
      constitution.boundaryTitle, constitution.boundaryBody,
    ]),
    section("/constitution", "constitution-trace", constitution.title, constitution.traceTitle, [
      constitution.traceScope, constitution.illustration,
      ...CONSTITUTION_SCENES.flatMap(scene => [t(scene.context), scene.trace, ...scene.cites, t(scene.decision)]),
    ]),
    section("/constitution", "constitution-next", constitution.title, constitution.configuration, [constitution.install]),
  ];
  return sections;
}
