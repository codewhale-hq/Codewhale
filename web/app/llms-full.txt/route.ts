import { RELEASE_DOCS, buildTopicMarkdown } from "@/lib/release-docs";
import { buildLlmsTxt } from "@/lib/llms-txt";

export const dynamic = "force-static";

export function GET() {
  const body = [buildLlmsTxt(), ...RELEASE_DOCS.topics.map((document) => buildTopicMarkdown(document.slug))].join("\n\n");
  return new Response(body, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "public, max-age=3600" } });
}
