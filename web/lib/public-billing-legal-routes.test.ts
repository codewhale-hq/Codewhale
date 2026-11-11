import { existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { redirect } from "next/navigation";
import PricingPage from "../app/[locale]/pricing/page";
import { footerLegalLinks } from "./i18n/links";
import { getChrome } from "./i18n/dictionaries";
import { formatLegalDocumentStatus, LEGAL_DOCUMENTS, PrivacyContent, TermsContent, WEBSITE_USAGE_DISCLOSURE } from "./legal-copy";
import { PrivacyContent as PinnedPrivacyContent, TermsContent as PinnedTermsContent } from "../vendor/legal-documents/legal-content";
import { USAGE_COUNTING_COPY } from "./content/usage-counting";
import TermsPage from "../app/[locale]/legal/terms/page";
import PrivacyPage from "../app/[locale]/legal/privacy/page";

const webRoot = new URL("../", import.meta.url);
vi.mock("next/navigation", () => ({ redirect: vi.fn() }));

describe("public legal routes and retired pricing", () => {
  it("ships real pages for the URLs that used to 404", () => {
    for (const path of [
      "app/[locale]/legal/terms/page.tsx",
      "app/[locale]/legal/privacy/page.tsx",
      "app/[locale]/privacy/page.tsx",
      "app/[locale]/terms/page.tsx",
    ]) {
      expect(existsSync(new URL(path, webRoot)), path).toBe(true);
    }
  });

  it("aliases /privacy and /terms onto the legal paths instead of inventing a second policy", () => {
    expect(footerLegalLinks("en", getChrome("en"), "Usage data").map((l) => l.href).slice(0, 2)).toEqual([
      "/en/legal/terms",
      "/en/legal/privacy",
    ]);
    for (const id of ["terms", "privacy"] as const) {
      expect(LEGAL_DOCUMENTS[id]).toEqual({ version: "2026-10-04", status: "effective", effectiveAt: "2026-10-04" });
      expect(formatLegalDocumentStatus(id, getChrome("en").dateLocale)).toContain("October 4, 2026");
      expect(formatLegalDocumentStatus(id, getChrome("de").dateLocale)).toContain("4. Oktober 2026");
    }
  });

  it("pins the single publication authority and fails on vendored metadata drift", () => {
    const pin = JSON.parse(readFileSync(new URL("vendor/legal-documents/PIN.json", webRoot), "utf8"));
    expect(pin.sourceRepository).toBe("codewhale-hq/codewhale-platform");
    expect(pin.sourceCommit).toBe("4cd9450aec07ed81d6c56b5ce741424fec533d6b");
    expect(pin.files.map((row: { path: string }) => row.path).sort()).toEqual(["legal-content.tsx", "legal-documents.d.ts", "legal-documents.js"]);
    expect(pin.files.find((row: { path: string }) => row.path === "legal-content.tsx")).toEqual({
      source: "apps/web/components/legal/legal-content.tsx",
      path: "legal-content.tsx",
      sha256: "0ffc3568bb9a8dea5e05c497763a342377fec3526b16fa273103909be9f9a3c1",
    });
    for (const row of pin.files) {
      const data = readFileSync(new URL(`vendor/legal-documents/${row.path}`, webRoot));
      expect(createHash("sha256").update(data).digest("hex")).toBe(row.sha256);
    }
  });

  it("uses the pinned policy components as the sole legal body", () => {
    expect(TermsContent).toBe(PinnedTermsContent);
    expect(PrivacyContent).toBe(PinnedPrivacyContent);
  });

  it("renders the complete October 4 documents in English and Chinese page chrome", async () => {
    for (const locale of ["en", "zh"]) {
      const html = renderToStaticMarkup(await TermsPage({ params: Promise.resolve({ locale }) }));
      const privacy = renderToStaticMarkup(await PrivacyPage({ params: Promise.resolve({ locale }) }));
      for (const document of [html, privacy]) {
        expect(document).toContain('data-legal-status="effective"');
        expect(document).toContain('data-legal-version="2026-10-04"');
        expect(document).toContain('data-legal-effective-at="2026-10-04"');
        expect(document).toContain(locale === "en" ? "October 4, 2026" : "2026年10月4日");
        expect(document).not.toMatch(/Not yet in effect|尚未生效|\[SELLER|\[SUPPORT/);
        expect(document).toContain("Shannon Labs, Inc.");
        expect(document).toContain('href="mailto:help@codewhale.net"');
        expect(document).toContain(`href="/${locale}/legal/terms"`);
        expect(document).toContain(`href="/${locale}/legal/privacy"`);
      }
      expect(html).toContain(renderToStaticMarkup(createElement(PinnedTermsContent)));
      expect(privacy).toContain(renderToStaticMarkup(createElement(PinnedPrivacyContent)));
      for (const id of ["purchases", "platform-fee", "no-automatic-charges", "expiry", "refunds", "governing-law"]) {
        expect(html).toContain(`id="${id}"`);
      }
      expect(privacy).toContain('id="processors"');
      expect(privacy).toContain('id="payments"');
    }
  });

  it("retains the website usage disclosure and preference control beside the canonical privacy body", async () => {
    for (const locale of ["en", "zh"] as const) {
      const html = renderToStaticMarkup(await PrivacyPage({ params: Promise.resolve({ locale }) }));
      expect(html).toContain(renderToStaticMarkup(createElement("p", null, WEBSITE_USAGE_DISCLOSURE.body)));
      expect(html).toContain(WEBSITE_USAGE_DISCLOSURE.title);
      expect(html).toContain('id="usage-counting"');
      expect(html).toContain('class="usage-counting"');
      for (const text of [USAGE_COUNTING_COPY.summary[locale], USAGE_COUNTING_COPY.choice[locale], USAGE_COUNTING_COPY.elsewhere[locale]]) {
        expect(html).toContain(renderToStaticMarkup(createElement("p", null, text)).slice(3, -4));
      }
    }
  });

  it("takes incoming pricing links to installation in the visitor's locale", async () => {
    for (const locale of ["en", "zh", "pt-BR"]) {
      await PricingPage({ params: Promise.resolve({ locale }) });
      expect(redirect).toHaveBeenLastCalledWith(`/${locale}/install`);
    }
  });
});
