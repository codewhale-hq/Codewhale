import generated from "@/data/agent-docs.generated.json";
import { DOC_TOPICS } from "./docs-map";
import { REPO_URL } from "./i18n/links";
import { buildInstallMarkdown } from "./llms-txt";

export const AGENT_DOCS = generated;

export function buildTopicMarkdown(slug: string): string | null {
  const document = AGENT_DOCS.find((entry) => entry.slug === slug);
  const topic = DOC_TOPICS.find((entry) => entry.slug === slug);
  if (!document || !topic) return null;
  if (slug === "install") return buildInstallMarkdown();
  return [`# ${topic.label.en}`, "", topic.description.en, "", ...document.sources.flatMap((source) => [
    `Source: ${REPO_URL}/blob/main/${source.path}`,
    `SHA-256: ${source.sha256}`, "",
    source.path.split("#", 1)[0].endsWith(".json") ? `\`\`\`json\n${source.content}\n\`\`\`` : source.content,
    "",
  ])].join("\n");
}
