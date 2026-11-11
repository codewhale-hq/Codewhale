import { RELEASE_DOCS, buildTopicMarkdown } from "@/lib/release-docs";

export const dynamic = "force-static";
export const dynamicParams = false;

export function generateStaticParams() {
  return RELEASE_DOCS.topics.map((document) => ({ topic: `${document.slug}.md` }));
}

export async function GET(_request: Request, { params }: { params: Promise<{ topic: string }> }) {
  const { topic } = await params;
  const body = topic.endsWith(".md") ? buildTopicMarkdown(topic.slice(0, -3)) : null;
  if (body === null) return new Response("Not found", { status: 404 });
  return new Response(body, { headers: { "content-type": "text/markdown; charset=utf-8", "cache-control": "public, max-age=3600" } });
}
