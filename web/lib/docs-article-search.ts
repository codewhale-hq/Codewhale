import {
  getDocsAuth, getDocsClaudeCode, getDocsConfiguration, getDocsFleet,
  getDocsHooks, getDocsMcp, getDocsModes, getDocsPet, getDocsReview,
  getDocsRuntimeApi, getDocsSandbox, getDocsSkills, getDocsSubagents,
  getDocsTroubleshooting, getDocsTrust, getDocsWeb, getDocsWork,
  getDocsGuide, getDocsVocabulary, pickText,
} from "./i18n/dictionaries";
import type { DocsBlock, DocsPageDict } from "./i18n/dictionaries/types";
import { GETTING_STARTED_STEPS } from "./content/getting-started";
import { TOOLS_COPY } from "./content/tools";
import {
  ADVISORY_ROLE, CONTROL_MODES, MEASUREMENT_PRINCIPLES, PERMISSION_POSTURES,
  PRODUCT_TERMS, ROUTE_IDENTITY,
} from "./content/vocabulary";

export interface DocSearchSection {
  href: string;
  pageTitle: string;
  title: string;
  text: string;
}

const ARTICLES: Record<string, (locale: string) => DocsPageDict> = {
  auth: getDocsAuth, "claude-code": getDocsClaudeCode, configuration: getDocsConfiguration,
  fleet: getDocsFleet, hooks: getDocsHooks, mcp: getDocsMcp, modes: getDocsModes,
  pet: getDocsPet, review: getDocsReview, "runtime-api": getDocsRuntimeApi,
  sandbox: getDocsSandbox, skills: getDocsSkills, subagents: getDocsSubagents,
  troubleshooting: getDocsTroubleshooting, trust: getDocsTrust, web: getDocsWeb,
  work: getDocsWork,
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
  const sections = Object.entries(ARTICLES).flatMap(([slug, getArticle]) => {
    const article = getArticle(locale);
    return [
      { id: "overview", title: article.title, text: article.lede },
      ...article.sections.map((section) => ({
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
    { href: "/docs/tools#overview", pageTitle: pickText(TOOLS_COPY.title, locale), title: pickText(TOOLS_COPY.title, locale), text: [pickText(TOOLS_COPY.lead, locale), ...TOOLS_COPY.rows.flatMap((row) => [row.name, pickText(row.detail, locale)])].join(" ") },
    { href: "/docs/tools#lsp", pageTitle: pickText(TOOLS_COPY.title, locale), title: pickText(TOOLS_COPY.lspTitle, locale), text: pickText(TOOLS_COPY.lspBody, locale) },
    { href: "/docs/tools#compatibility", pageTitle: pickText(TOOLS_COPY.title, locale), title: pickText(TOOLS_COPY.compatibilityTitle, locale), text: pickText(TOOLS_COPY.compatibility, locale) },
    { href: "/docs/vocabulary#execution-nouns", pageTitle: vocabulary.title, title: vocabulary.executionHeading, text: PRODUCT_TERMS.flatMap((row) => [row.term, pickText(row.short, locale), pickText(row.long, locale)]).join(" ") },
    { href: "/docs/vocabulary#control-nouns", pageTitle: vocabulary.title, title: vocabulary.controlHeading, text: [...CONTROL_MODES, ...PERMISSION_POSTURES].flatMap((row) => [row.term, pickText(row.description, locale)]).join(" ") },
    { href: "/docs/vocabulary#route-identity", pageTitle: vocabulary.title, title: vocabulary.routeHeading, text: ROUTE_IDENTITY.flatMap((row) => [row.term, pickText(row.description, locale)]).join(" ") },
    { href: "/docs/vocabulary#advisory-role", pageTitle: vocabulary.title, title: vocabulary.advisoryHeading, text: `${ADVISORY_ROLE.term} ${pickText(ADVISORY_ROLE.description, locale)}` },
    { href: "/docs/vocabulary#measurement", pageTitle: vocabulary.title, title: vocabulary.measurementHeading, text: MEASUREMENT_PRINCIPLES.map((row) => pickText(row, locale)).join(" ") },
  );
  return sections;
}

export function searchDocSections(sections: readonly DocSearchSection[], query: string): (DocSearchSection & { excerpt: string })[] {
  const normalized = query.trim().toLowerCase();
  if (!normalized) return [];
  return sections.filter((section) => `${section.pageTitle} ${section.title} ${section.text}`.toLowerCase().includes(normalized)).map((section) => {
    const text = section.text.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").replace(/`/g, "").replace(/\s+/g, " ");
    const position = text.toLowerCase().indexOf(normalized);
    const start = Math.max(0, position - 70);
    const end = Math.min(text.length, start + 240);
    return { ...section, excerpt: `${start ? "…" : ""}${text.slice(start, end)}${end < text.length ? "…" : ""}` };
  });
}
