import { describe, expect, it, vi } from "vitest";
import { GET } from "../app/api/docs/search/[locale]/route";
import * as index from "./docs-article-search";
import type { DocSearchSection } from "./doc-search-match";

const context = (locale: string) => ({ params: Promise.resolve({ locale }) });

describe("locale-only documentation search delivery", () => {
  it("delivers the complete source index on demand and revalidates it without a body", async () => {
    const request = new Request("http://localhost/api/docs/search/en");
    const response = await GET(request, context("en"));
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toContain("max-age=300");
    const etag = response.headers.get("etag");
    expect(etag).toMatch(/^W\/"[0-9a-f]{64}"$/);
    const payload = await response.json() as { sections: DocSearchSection[] };
    for (const path of ["/install#", "/models#", "/plugins#", "/constitution#", "/docs/runtime-reference#", "/docs/workflow-tutorial#", "/docs/plugin-authoring#"]) {
      expect(payload.sections.some(section => section.href.startsWith(path)), path).toBe(true);
    }
    const unchanged = await GET(new Request(request.url, { headers: { "if-none-match": etag! } }), context("en"));
    expect(unchanged.status).toBe(304);
    expect(await unchanged.text()).toBe("");
    const withQuery = await GET(new Request(`${request.url}?q=private-needle-597cfed`), context("en"));
    expect(withQuery.headers.get("etag")).toBe(etag);
    expect(await withQuery.text()).not.toContain("private-needle-597cfed");
  });

  it("rejects unknown locales and fails explicitly when the source cannot build", async () => {
    const unknown = await GET(new Request("http://localhost/api/docs/search/unknown"), context("unknown"));
    expect(unknown.status).toBe(404);
    expect(unknown.headers.get("cache-control")).toBe("no-store");
    const spy = vi.spyOn(index, "getDocSearchSections").mockImplementationOnce(() => { throw new Error("private source detail"); });
    try {
      const failed = await GET(new Request("http://localhost/api/docs/search/ja"), context("ja"));
      expect(failed.status).toBe(503);
      expect(await failed.json()).toEqual({ error: "unavailable" });
      expect(failed.headers.get("cache-control")).toBe("no-store");
      expect((await GET(new Request("http://localhost/api/docs/search/ja"), context("ja"))).status).toBe(200);
    } finally { spy.mockRestore(); }
  });
});
