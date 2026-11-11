import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import DocsLayout from "@/app/[locale]/docs/layout";
import { EnglishText, FallbackRegion } from "./fallback-region";

const pathname = vi.hoisted(() => ({ value: "/ar/docs/modes" }));

vi.mock("next/navigation", () => ({
  usePathname: () => pathname.value,
  useRouter: () => ({ push: () => {}, replace: () => {}, prefetch: () => {} }),
  useSearchParams: () => new URLSearchParams(),
}));

const marker = "body text";
const attrs = 'lang="en" dir="ltr"';

async function renderDocs(locale: string, path: string): Promise<string> {
  pathname.value = path;
  const element = await DocsLayout({
    children: createElement("div", { id: "body" }, "body"),
    params: Promise.resolve({ locale }),
  });
  return renderToStaticMarkup(element);
}

describe("FallbackRegion", () => {
  it("labels an English body on a partial locale and leaves translated bodies alone", () => {
    const ar = renderToStaticMarkup(<FallbackRegion path="/changelog" locale="ar">{marker}</FallbackRegion>);
    expect(ar).toContain(attrs);
    for (const locale of ["en", "zh"]) {
      const html = renderToStaticMarkup(<FallbackRegion path="/changelog" locale={locale}>{marker}</FallbackRegion>);
      expect(html).toBe(marker);
    }
  });

  it("wraps one inline English string in its own language and direction", () => {
    expect(renderToStaticMarkup(createElement(EnglishText, null, "mode work/"))).toBe(
      `<bdi ${attrs}>mode work/</bdi>`,
    );
  });
});

describe("docs layout", () => {
  it("labels the English body and chrome for Arabic but not a translated guide body", async () => {
    const modes = await renderDocs("ar", "/ar/docs/modes");
    expect(modes).toContain(`<div class="locale-fallback" ${attrs}><div id="body">body</div></div>`);
    expect(modes.split(attrs).length - 1).toBeGreaterThanOrEqual(4);

    const guide = await renderDocs("ar", "/ar/docs/guide");
    expect(guide).toContain('<div id="body">body</div>');
    expect(guide).not.toContain(`${attrs}><div id="body">`);
    expect(guide.split(attrs).length - 1).toBeGreaterThanOrEqual(3);
  });

  it("adds no attributes for English or Chinese", async () => {
    for (const locale of ["en", "zh"]) {
      const html = await renderDocs(locale, `/${locale}/docs/modes`);
      expect(html).not.toContain(attrs);
      expect(html).not.toContain("locale-fallback");
    }
  });
});
