import { AGENT_DOCS } from "./agent-docs";
import { REFERENCE_GUIDES } from "./content/reference-guides";
import { pickText } from "./i18n/dictionaries";
import { buildInstallGuide } from "../scripts/install-guide-lib.mjs";

export function getReferenceGuide(slug: string) {
  const spec = REFERENCE_GUIDES.find((entry) => entry.slug === slug);
  if (!spec) throw new Error(`Unknown reference guide: ${slug}`);
  const document = AGENT_DOCS.find((entry) => entry.slug === slug);
  const source = document?.sources.find((entry) => entry.path === spec.repoSource);
  if (!source) throw new Error(`Missing canonical reference source: ${spec.repoSource}`);
  const sources = document!.sources;
  const body = sources.map((entry, index) => `${index ? "## Registered HTTP paths\n\n" : ""}${entry.content.replace(/^# [^\n]+\n/, "")}`).join("\n\n");
  const guide = buildInstallGuide(body, { sourcePath: spec.repoSource.replace(/^docs\//, ""), tableLabel: "Reference table" });
  return { spec, source, sources, guide };
}

export function getReferenceSearchSections(locale: string) {
  return REFERENCE_GUIDES.flatMap((spec) => {
    const { guide } = getReferenceGuide(spec.slug);
    return [
      { id: "overview", title: pickText(spec.title, locale), text: `${pickText(spec.lead, locale)} ${guide.preamble}` },
      ...guide.searchSections,
    ].map((section) => ({
      href: `/docs/${spec.slug}#${section.id}`,
      pageTitle: pickText(spec.title, locale),
      title: section.title,
      text: section.text,
      lang: "en",
    }));
  });
}
