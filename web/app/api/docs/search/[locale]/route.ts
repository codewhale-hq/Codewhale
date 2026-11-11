import { getDocSearchSections } from "@/lib/docs-article-search";
import { isValidLocale } from "@/lib/i18n/config";

const responses = new Map<string, Promise<{ body: string; etag: string }>>();

export async function GET(request: Request, { params }: { params: Promise<{ locale: string }> }): Promise<Response> {
  const { locale } = await params;
  if (!isValidLocale(locale)) return Response.json({ error: "unknown_locale" }, { status: 404, headers: { "cache-control": "no-store" } });
  try {
    let pending = responses.get(locale);
    if (!pending) {
      pending = (async () => {
        const body = JSON.stringify({ sections: getDocSearchSections(locale) });
        const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(body));
        const hash = Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, "0")).join("");
        return { body, etag: `W/"${hash}"` };
      })();
      responses.set(locale, pending);
    }
    const { body, etag } = await pending;
    const headers = { "content-type": "application/json; charset=utf-8", "cache-control": "public, max-age=300, s-maxage=300", etag };
    const validators = request.headers.get("if-none-match")?.split(",").map(value => value.trim()) ?? [];
    return validators.some(value => value === "*" || value.replace(/^W\//, "") === etag.replace(/^W\//, ""))
      ? new Response(null, { status: 304, headers })
      : new Response(body, { headers });
  } catch {
    responses.delete(locale);
    return Response.json({ error: "unavailable" }, { status: 503, headers: { "cache-control": "no-store" } });
  }
}
