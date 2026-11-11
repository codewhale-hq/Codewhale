import generated from "@/data/agent-docs.generated.json";
import released from "@/data/agent-docs.release.json";
import { DOC_TOPICS } from "./docs-map";
import { REPO_URL } from "./i18n/links";
import { buildInstallMarkdown } from "./llms-txt";

export const AGENT_DOCS = generated;
export const RELEASE_DOCS = released;

export function buildTopicMarkdown(slug: string): string | null {
  const document = RELEASE_DOCS.topics.find((entry) => entry.slug === slug);
  const topic = DOC_TOPICS.find((entry) => entry.slug === slug);
  if (!document || !topic) return null;
  if (slug === "install") return buildInstallMarkdown();
  const { tag } = RELEASE_DOCS;
  return [
    `# ${topic.label.en}`, "", topic.description.en, "",
    `The document${document.sources.length > 1 ? "s" : ""} below ${document.sources.length > 1 ? "are" : "is"} the repository's file${document.sources.length > 1 ? "s" : ""} as ${document.sources.length > 1 ? "they were" : "it was"} at the published release ${tag}.`, "",
    ...document.missing.flatMap((path) => [`${path} does not exist at ${tag}, so this topic has no such document for the published release.`, ""]),
    ...document.sources.flatMap((source) => [
      `Source: ${REPO_URL}/blob/${tag}/${source.path}`,
      `SHA-256: ${source.sha256}`, "",
      source.path.split("#", 1)[0].endsWith(".json") ? `\`\`\`json\n${source.content}\n\`\`\`` : source.content,
      "",
    ]),
  ].join("\n");
}
