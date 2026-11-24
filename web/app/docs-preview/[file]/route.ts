import { DOCS_PREVIEW_ENTRIES, DOCS_PREVIEW_LANGUAGES, docsPreviewLlmsFullTxt, docsPreviewLlmsTxt, docsPreviewMarkdown, docsPreviewSearchSections } from "@/lib/docs-preview";

export const dynamic = "force-static";
export const dynamicParams = false;

const FILES: Record<string, () => { body: string; type: string }> = {
  "llms.txt": () => ({ body: docsPreviewLlmsTxt(), type: "text/plain" }),
  "llms-full.txt": () => ({ body: docsPreviewLlmsFullTxt(), type: "text/plain" }),
  ...Object.fromEntries(DOCS_PREVIEW_LANGUAGES.flatMap((language) => [
    [`search.${language}.json`, () => ({ body: JSON.stringify({ sections: docsPreviewSearchSections(language) }), type: "application/json" })],
    ...DOCS_PREVIEW_ENTRIES.map((entry) => [
      language === "en" ? `${entry.slug}.md` : `${entry.slug}.${language}.md`,
      () => ({ body: docsPreviewMarkdown(entry.slug, language), type: "text/markdown" }),
    ]),
  ])),
};

export function generateStaticParams() {
  return Object.keys(FILES).map((file) => ({ file }));
}

export async function GET(_request: Request, { params }: { params: Promise<{ file: string }> }) {
  const { file } = await params;
  const build = FILES[file];
  if (!build) return new Response("Not found", { status: 404 });
  const { body, type } = build();
  return new Response(body, { headers: { "content-type": `${type}; charset=utf-8`, "cache-control": "public, max-age=3600", "x-robots-tag": "noindex" } });
}
