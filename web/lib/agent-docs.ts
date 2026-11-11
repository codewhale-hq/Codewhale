import generated from "@/data/agent-docs.generated.json";
import { REFERENCE_GUIDE_COPY } from "./content/reference-guides";
import { DOC_TOPICS } from "./docs-map";
import { BUILD_FACTS } from "./facts";
import { fill } from "./i18n/dictionaries";
import { REPO_URL } from "./i18n/links";
import { buildInstallMarkdown } from "./llms-txt";

export const AGENT_DOCS = generated;

export function buildTopicMarkdown(slug: string): string | null {
  const document = AGENT_DOCS.find((entry) => entry.slug === slug);
  const topic = DOC_TOPICS.find((entry) => entry.slug === slug);
  if (!document || !topic) return null;
  if (slug === "install") return buildInstallMarkdown();
  const tag = BUILD_FACTS.latestPublishedRelease?.tag;
  return [`# ${topic.label.en}`, "", topic.description.en, "", ...(tag ? [
    fill(REFERENCE_GUIDE_COPY.scope.en, { tag }), "",
    `Repository at ${tag}: ${REPO_URL}/tree/${tag}`, "",
  ] : []), ...document.sources.flatMap((source) => [
    `Source: ${REPO_URL}/blob/main/${source.path}`,
    `SHA-256: ${source.sha256}`, "",
    source.path.split("#", 1)[0].endsWith(".json") ? `\`\`\`json\n${source.content}\n\`\`\`` : source.content,
    "",
  ])].join("\n");
}
