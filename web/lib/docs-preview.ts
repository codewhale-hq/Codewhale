import { DOCS_PREVIEW_COPY, DOCS_PREVIEW_NAV, type DocsPreviewSlug } from "./content/docs-preview";
import type { DocSearchSection } from "./doc-search-match";
import { DOCS_PREVIEW_PAGES, DOCS_PREVIEW_ROUTES } from "./docs-preview.generated";
import { fill, pickText, pickTextLocale } from "./i18n/dictionaries";
import { REPO_URL } from "./i18n/links";
import { SITE_NAME, SITE_URL } from "./page-meta";

export interface DocsPreviewChunk { kind: string; text: string; lang?: string; html?: string | null }
export interface DocsPreviewHeading { id: string; depth: number; title: string }
interface RenderedPage {
  title: string;
  lang: string;
  sourcePath: string;
  sourceHash: string;
  headings: DocsPreviewHeading[];
  chunks: DocsPreviewChunk[];
  searchSections: { id: string; title: string; text: string }[];
  markdown: string;
}

const RENDERED: Record<string, Record<string, RenderedPage>> = DOCS_PREVIEW_PAGES;

export const DOCS_PREVIEW_ENTRIES = DOCS_PREVIEW_NAV.flatMap((group) => group.pages);
export const DOCS_PREVIEW_LANGUAGES = ["en", "zh"] as const;
export const DOCS_PREVIEW_ROUTE_COUNT = DOCS_PREVIEW_ROUTES.groups.reduce((sum, group) => sum + group.routes.length, 0);

export function isDocsPreviewSlug(slug: string): slug is DocsPreviewSlug {
  return DOCS_PREVIEW_ENTRIES.some((entry) => entry.slug === slug);
}

function routesPage(language: "en" | "zh"): RenderedPage {
  const entry = DOCS_PREVIEW_ENTRIES.find((page) => page.slug === "runtime-routes")!;
  const lead = fill(pickText(DOCS_PREVIEW_COPY.routesLead, language), { count: DOCS_PREVIEW_ROUTE_COUNT, groups: DOCS_PREVIEW_ROUTES.groups.length });
  const table = (routes: readonly { path: string; methods: readonly string[] }[]) =>
    [`| ${pickText(DOCS_PREVIEW_COPY.method, language)} | ${pickText(DOCS_PREVIEW_COPY.path, language)} |`, "| --- | --- |", ...routes.map((route) => `| ${route.methods.join(" / ")} | \`${route.path}\` |`)].join("\n");
  return {
    title: pickText(entry.label, language),
    lang: language === "zh" ? "zh-Hans" : "en",
    sourcePath: DOCS_PREVIEW_ROUTES.sourcePath,
    sourceHash: DOCS_PREVIEW_ROUTES.sourceHash,
    headings: DOCS_PREVIEW_ROUTES.groups.map((group) => ({ id: group.name, depth: 2, title: group.name })),
    chunks: [],
    searchSections: [
      { id: "", title: pickText(entry.label, language), text: lead },
      ...DOCS_PREVIEW_ROUTES.groups.map((group) => ({ id: group.name, title: group.name, text: group.routes.map((route) => `${route.methods.join(" ")} ${route.path}`).join(" ") })),
    ],
    markdown: [`# ${pickText(entry.label, language)}`, "", lead, "", ...DOCS_PREVIEW_ROUTES.groups.flatMap((group) => [`## ${group.name}`, "", table(group.routes), ""])].join("\n"),
  };
}

export function getDocsPreviewPage(slug: DocsPreviewSlug, locale: string): RenderedPage & { translated: boolean } {
  const language = pickTextLocale(locale);
  if (slug === "runtime-routes") return { ...routesPage(language), translated: locale === "en" || locale === "zh" };
  const rendered = RENDERED[slug];
  const page = rendered[language] ?? rendered.en;
  return { ...page, translated: locale === "en" || (locale === "zh" && page.lang !== "en") };
}

export function docsPreviewRouteGroups() {
  return DOCS_PREVIEW_ROUTES.groups;
}

export function docsPreviewMarkdown(slug: DocsPreviewSlug, language: "en" | "zh"): string {
  const page = getDocsPreviewPage(slug, language);
  return [
    page.markdown.trimEnd(), "",
    "---", "",
    `Source: ${REPO_URL}/blob/main/${page.sourcePath}`,
    `SHA-256: ${page.sourceHash}`,
    `Page: ${SITE_URL}/${language}/docs-preview/${slug}`, "",
  ].join("\n");
}

export function docsPreviewSearchSections(language: "en" | "zh"): DocSearchSection[] {
  return DOCS_PREVIEW_ENTRIES.flatMap((entry) => {
    const page = getDocsPreviewPage(entry.slug, language);
    return page.searchSections.map((section) => ({
      href: `/docs-preview/${entry.slug}${section.id ? `#${section.id}` : ""}`,
      pageTitle: pickText(entry.label, language),
      title: section.title,
      text: section.text,
      lang: page.lang,
    }));
  });
}

export function docsPreviewLlmsTxt(): string {
  return [
    `# ${SITE_NAME} documentation preview`, "",
    `> ${DOCS_PREVIEW_COPY.lead.en}`, "",
    ...DOCS_PREVIEW_NAV.flatMap((group) => [
      `## ${group.group.en}`, "",
      ...group.pages.map((page) => `- [${page.label.en}](${SITE_URL}/docs-preview/${page.slug}.md): ${page.description.en}`), "",
    ]),
    "## Optional", "",
    `- [Every page in one file](${SITE_URL}/docs-preview/llms-full.txt)`,
    `- [Published documentation index](${SITE_URL}/llms.txt)`, "",
  ].join("\n");
}

export function docsPreviewLlmsFullTxt(): string {
  return [docsPreviewLlmsTxt(), ...DOCS_PREVIEW_ENTRIES.map((entry) => docsPreviewMarkdown(entry.slug, "en"))].join("\n\n");
}
