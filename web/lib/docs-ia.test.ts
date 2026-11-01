/**
 * Information-architecture contracts: docs-map registration, sitemap and
 * hreflang preservation, navigation parity across breakpoints and locales,
 * and the accessibility hooks (skip link, labelled nav, aria-current).
 *
 * These are deterministic source/unit contracts in the same style as
 * public-copy.test.ts: they read the real sources and assert structure, so a
 * future IA change fails here first instead of drifting silently.
 */
import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import buildSitemap from "../app/sitemap";
import { DOC_TOPICS, docTopicHref, getTopic } from "./docs-map";
import { docsTopicIsCurrent } from "./docs-navigation";
import { locales } from "./i18n/config";
import { contentLocalesForPath } from "./i18n/content-locales";
import { getChrome, getHome } from "./i18n/dictionaries";
import {
  currentNavHref,
  footerLegalLinks,
  footerProductLinks,
  footerProjectLinks,
  navLinks as buildNavLinks,
  secondaryNavLinks as buildSecondaryNavLinks,
} from "./i18n/links";
import { SITE_URL } from "./page-meta";
import { siteCss } from "./site-css";
import { readCatalogue } from "./ratatui/catalogue";

const webRoot = new URL("../", import.meta.url);
const repoRoot = new URL("../../", import.meta.url);

function webText(path: string): string {
  return readFileSync(new URL(path, webRoot), "utf8");
}

const sitemapEntries = buildSitemap();
const nav = webText("components/nav.tsx");
const navLinks = webText("components/nav-links.tsx");
const mobileMenu = webText("components/mobile-menu.tsx");
const footer = webText("components/footer.tsx");
const localeLayout = webText("app/[locale]/layout.tsx");
const css = siteCss();

describe("docs-map registration", () => {
  it("registers the guide and vocabulary topics as first-party pages", () => {
    const guide = getTopic("guide");
    const vocabulary = getTopic("vocabulary");
    expect(guide?.hasPage).toBe(true);
    expect(vocabulary?.hasPage).toBe(true);
    expect(vocabulary?.category).toBe("reference");
    expect(docTopicHref(guide!, "en")).toBe("/en/docs/guide");
    expect(docTopicHref(vocabulary!, "zh")).toBe("/zh/docs/vocabulary");
    expect(docsTopicIsCurrent(vocabulary!, "en", "/en/docs/vocabulary")).toBe(true);
  });

  it("keeps every docs topic repo source on disk", () => {
    for (const topic of DOC_TOPICS) {
      const sources = Array.isArray(topic.repoSource) ? topic.repoSource : [topic.repoSource];
      for (const source of sources) {
        expect(existsSync(new URL(source, repoRoot)), `${topic.id}: ${source}`).toBe(true);
      }
    }
  });

  it("keeps topic labels and descriptions bilingual", () => {
    for (const topic of DOC_TOPICS) {
      for (const pair of [topic.label, topic.description]) {
        expect(pair.en.trim().length, `${topic.id} en`).toBeGreaterThan(0);
        expect(pair.zh.trim().length, `${topic.id} zh`).toBeGreaterThan(0);
      }
    }
  });
});

describe("sitemap and hreflang preservation", () => {
  it("indexes every first-party docs page", () => {
    for (const topic of DOC_TOPICS) {
      if (!topic.hasPage) continue;
      const path = topic.sitePath ? `/${topic.sitePath}` : `/docs/${topic.slug}`;
      expect(
        sitemapEntries.some((entry) => entry.url === `${SITE_URL}/en${path}`),
        path,
      ).toBe(true);
    }
    expect(
      sitemapEntries.some((entry) => entry.url === `${SITE_URL}/en/docs/guide`),
    ).toBe(true);
    expect(
      sitemapEntries.some((entry) => entry.url === `${SITE_URL}/en/docs/vocabulary`),
    ).toBe(true);
  });

  it("keeps sitemap and hreflang output aligned with real translation coverage", () => {
    // Home and Computer Use have 18 locales; install and the source catalogue are English.
    // Other page bodies ship English and Chinese.
    const existingEntries = sitemapEntries.filter(
      (entry) => new URL(entry.url).pathname.split("/")[2] !== "ratatui",
    );
    expect(existingEntries).toHaveLength(112);
    expect(sitemapEntries.filter(entry => entry.url.endsWith("/install")).map(entry => entry.url))
      .toEqual([`${SITE_URL}/en/install`]);
    expect(sitemapEntries.some(entry => entry.url.endsWith("/pricing"))).toBe(false);
    for (const path of ["/product", "/plugins", "/computer-use", "/signin", "/legal/terms", "/legal/privacy"]) {
      expect(
        sitemapEntries.some((entry) => entry.url === `${SITE_URL}/en${path}`),
        path,
      ).toBe(true);
    }
    for (const [path, expectedLocales] of [
      ["/", locales],
      ["/docs/guide", ["en", "zh"]],
      ["/computer-use", locales],
      ["/install", ["en"]],
      ["/docs", ["en", "zh"]],
    ] as const) {
      const suffix = path === "/" ? "" : path;
      const entry = sitemapEntries.find(
        (candidate) => candidate.url === `${SITE_URL}/en${suffix}`,
      );
      expect(entry, path).toBeDefined();
      expect(Object.keys(entry?.alternates?.languages ?? {}), path).toEqual([
        ...expectedLocales,
      ]);
    }
    expect(sitemapEntries.every((entry) => !("lastModified" in entry))).toBe(true);
  });

  it("indexes every Ratatui preview with genuine translation alternates", () => {
    const catalogue = readCatalogue();
    const paths = ["/ratatui", ...catalogue.entries.map((entry) => `/ratatui/${encodeURIComponent(entry.name)}`)];
    const expectedUrls = paths.flatMap((path) =>
      contentLocalesForPath(path).map((locale) => `${SITE_URL}/${locale}${path}`),
    );
    const entries = sitemapEntries.filter(
      (entry) => new URL(entry.url).pathname.split("/")[2] === "ratatui",
    );
    expect(entries.map((entry) => entry.url)).toEqual(expectedUrls);
    expect(new Set(sitemapEntries.map((entry) => entry.url)).size).toBe(sitemapEntries.length);
    for (const entry of entries) {
      expect(Object.keys(entry.alternates?.languages ?? {})).toEqual(entry.url.endsWith("/ratatui") ? ["en", "zh"] : ["en"]);
    }
  });

  it("keeps the new docs pages on the shared metadata helper", () => {
    for (const route of ["guide", "vocabulary"]) {
      const page = webText(`app/[locale]/docs/${route}/page.tsx`);
      expect(page, route).toContain('import { buildPageMetadata } from "@/lib/page-meta"');
      expect(page, route).toContain(`path: "/docs/${route}"`);
    }
  });

  it("indexes sign-in and redirects account creation", () => {
    for (const path of ["/signin"]) {
      expect(
        sitemapEntries.some((entry) => entry.url === `${SITE_URL}/en${path}`),
        path,
      ).toBe(true);
      expect(existsSync(new URL(`app/[locale]${path}/page.tsx`, webRoot)), path).toBe(true);
    }
    const entry = webText("components/public-account-entry.tsx");
    expect(entry).toContain("<WhalePose");
    expect(entry).toContain("installLocally");
    expect(webText("app/[locale]/signin/page.tsx")).toContain("<PublicAccountEntry locale={locale} />");
    expect(webText("app/[locale]/signup/page.tsx")).toContain('permanentRedirect(`/${locale}/signin`)');
    expect(sitemapEntries.some((entry) => entry.url.endsWith("/signup"))).toBe(false);
    expect(sitemapEntries.some((entry) => entry.url.endsWith("/docs/computers"))).toBe(false);
    expect(webText("app/[locale]/docs/computers/page.tsx")).toContain('permanentRedirect(`/${locale}/docs/fleet`)');
    expect(nav).not.toContain("APP_LOGIN_URL");
    expect(nav).not.toContain("APP_SIGNUP_URL");
  });
});

describe("navigation parity and accessibility", () => {
  it("keeps desktop and mobile navigation on one shared link set", () => {
    // Both surfaces consume the same `links` prop from nav.tsx — assert the
    // wiring rather than duplicating the arrays.
    expect(nav).toContain("<NavLinks links={links} primaryAria={chrome.navPrimaryAria} />");
    expect(nav).toContain("links={links}");
    expect(mobileMenu).toContain("[...links, ...moreLinks].map");
    expect(navLinks).toContain("links.map");
    // One generator feeds both surfaces — no per-locale hardcoded arrays.
    expect(nav).toContain("navLinks(locale, chrome)");
    expect(nav).not.toMatch(/const (EN|ZH)_LINKS/);
    // The primary strip replaces the compact menu at lg, with icon-only
    // controls leaving room for translated labels (see nav-hit-target.test).
    // Wrapping is the escape valve for a translated strip that outgrows the
    // row; the row gap stays tight so a second row does not double the
    // sticky header's height.
    expect(navLinks).toContain(
      'className="hidden lg:flex min-w-0 shrink items-center gap-x-5 gap-y-1 flex-wrap"',
    );
    // One label per destination: no companion labels on either surface.
    expect(navLinks).not.toContain("secondary");
    expect(mobileMenu).not.toContain("secondary");
    expect(mobileMenu).toContain('className="nav-icon-button lg:hidden"');
    expect(nav).toContain("paper-install-cta hidden lg:inline-flex");
    // A fixed descendant of the blurred sticky header uses the header as its
    // containing block and collapses. The open sheet must live at body scope.
    expect(mobileMenu).toContain('import { createPortal } from "react-dom"');
    expect(mobileMenu).toContain("createPortal(<div");
    expect(mobileMenu).toContain("document.body");
    expect(mobileMenu).toContain("element.inert = true");
    expect(mobileMenu).toContain('if (e.key !== "Tab") return');
    expect(mobileMenu).toContain('window.matchMedia("(min-width: 1024px)")');
    expect(mobileMenu).toContain("if (event.matches) closeImmediately()");
    // Locale handlers are shared so a regional tag cannot nest
    // (`/ja/pt-BR/...`); the theme control is site-wide, never route-gated.
    expect(webText("components/locale-switcher.tsx")).toContain("replacePathLocale(pathname, code)");
    expect(webText("components/theme-toggle.tsx")).not.toContain("isDocsPath");
    expect(webText("middleware.ts")).toContain("pathLocale(pathname)");
  });

  it("keeps nav link paths in exact locale-swap parity for every routed locale", () => {
    // The hardcoded /en/ and /zh/ arrays are gone; assert the generated set
    // directly, across every routed locale rather than only two of them.
    const reference = buildNavLinks("en", getChrome("en")).map((l) =>
      l.href.replace(/^\/en\//, ""),
    );
    expect(reference).toEqual(["product", "features", "models", "plugins", "docs"]);
    const moreReference = buildSecondaryNavLinks("en", getChrome("en")).map((l) =>
      l.href.replace(/^\/en\//, ""),
    );
    expect(moreReference).toEqual(["docs/guide", "install", "ratatui", "faq", "community", "contribute"]);
    for (const locale of locales) {
      const links = buildNavLinks(locale, getChrome(locale));
      expect(
        links.map((l) => l.href.replace(new RegExp(`^/${locale}/`), "")),
        `${locale} nav routes`,
      ).toEqual(reference);
      const more = buildSecondaryNavLinks(locale, getChrome(locale));
      expect(
        more.map((l) => l.href.replace(new RegExp(`^/${locale}/`), "")),
        `${locale} secondary nav routes`,
      ).toEqual(moreReference);
      for (const link of [...links, ...more]) {
        expect(link.href.startsWith(`/${locale}/`), `${locale} ${link.href}`).toBe(true);
        expect(link.label.trim().length, `${locale} empty nav label`).toBeGreaterThan(0);
      }
    }
  });

  it("keeps footer link paths in exact locale-swap parity for every routed locale", () => {
    const reference = footerProductLinks("en", getChrome("en")).map((l) =>
      l.href.replace(/^\/en\//, ""),
    );
    expect(reference).toContain("product");
    expect(reference).toContain("docs/guide");
    expect(reference).toContain("faq");
    for (const locale of locales) {
      const product = footerProductLinks(locale, getChrome(locale));
      expect(
        product.map((l) => l.href.replace(new RegExp(`^/${locale}/`), "")),
        `${locale} footer product routes`,
      ).toEqual(reference);
      const project = footerProjectLinks(locale, getChrome(locale));
      expect(project.map((l) => l.href), `${locale} footer project routes`).toEqual([
        "https://github.com/codewhale-hq/CodeWhale",
        "https://github.com/codewhale-hq/CodeWhale/issues",
        "https://discord.gg/37gfS3ksug",
        `/${locale}/contribute`,
        "https://github.com/codewhale-hq/CodeWhale/blob/main/LICENSE",
      ]);
      const legal = footerLegalLinks(locale, getChrome(locale));
      expect(legal.map((l) => l.href), `${locale} footer legal`).toEqual([
        `/${locale}/legal/terms`,
        `/${locale}/legal/privacy`,
      ]);
      // Labels come from the dictionary, not hardcoded English, so every
      // routed locale renders the footer legal links in its own language.
      expect(legal.map((l) => l.label), `${locale} footer legal labels`).toEqual([
        getChrome(locale).footerTerms,
        getChrome(locale).footerPrivacy,
      ]);
    }
  });

  it("labels the primary nav and marks the current page accessibly", () => {
    expect(navLinks).toContain("aria-label={primaryAria}");
    expect(getChrome("en").navPrimaryAria).toBe("Primary");
    expect(getChrome("zh").navPrimaryAria).toBe("主导航");
    expect(navLinks).toContain('aria-current={isActive ? "page" : undefined}');
    expect(mobileMenu).toContain('aria-current={isActive ? "page" : undefined}');
    expect(mobileMenu).toContain('aria-expanded={open}');
    expect(mobileMenu).toContain('aria-controls="mobile-menu"');
    expect(mobileMenu).toContain('role="dialog"');
  });

  it("marks exactly one nav link as the current page on a nested route", () => {
    // `/xx/docs` and `/xx/docs/guide` are both nav links, so the plain
    // prefix test both surfaces used marked two links `aria-current="page"`
    // on the guide route — and drew the nav underline under both.
    for (const locale of locales) {
      const links = [
        ...buildNavLinks(locale, getChrome(locale)),
        ...buildSecondaryNavLinks(locale, getChrome(locale)),
      ];
      const guide = `/${locale}/docs/guide`;
      const naive = links.filter(
        (l) => guide === l.href || guide.startsWith(`${l.href}/`),
      );
      expect(naive.length, `${locale} ancestor+page collision`).toBeGreaterThan(1);
      expect(currentNavHref(links, guide), `${locale} current nav link`).toBe(guide);
      // A route that is not itself a nav link still resolves to its section.
      expect(
        currentNavHref(links, `/${locale}/docs/configuration`),
        `${locale} section fallback`,
      ).toBe(`/${locale}/docs`);
      expect(currentNavHref(links, `/${locale}`), `${locale} home`).toBeNull();
    }
    // Both surfaces resolve the current page through the shared helper
    // rather than repeating the prefix test that collided.
    expect(navLinks).toContain("currentNavHref(links, pathname)");
    expect(mobileMenu).toContain("currentNavHref([...links, ...moreLinks], pathname)");
    expect(navLinks).not.toContain("pathname.startsWith(");
    expect(mobileMenu).not.toContain("pathname.startsWith(");
  });

  it("ships a keyboard-reachable skip link to the main landmark", () => {
    expect(localeLayout).toContain('href="#main-content"');
    expect(localeLayout).toContain('className="skip-link"');
    expect(localeLayout).toContain('<main id="main-content">');
    expect(css).toContain(".skip-link:focus-visible");
  });

  it("keeps the docs trail on the docs-map registry", () => {
    const docsLayout = webText("app/[locale]/docs/layout.tsx");
    const docsMap = webText("lib/docs-map.ts");
    expect(docsMap).toContain("sidebar, breadcrumbs, and drift/parity checks");
    expect(docsMap).toContain("export const DOC_CATEGORY_LABELS");
    expect(docsLayout).toContain("<DocsBreadcrumb locale={locale} />");
    expect(css).toContain(".docs-breadcrumb");
  });

  it("keeps responsive breakpoints for the getting-started steps", () => {
    // 4-up grid by default, 2-up at the tablet breakpoint, 1-up on phones —
    // the same responsive ladder as the existing workflow steps.
    expect(css).toMatch(/\.gs-steps\s*\{[^}]*repeat\(4, minmax\(0, 1fr\)\)/);
    expect(css).toMatch(
      /@media \(max-width: 760px\)[\s\S]*?\.gs-steps\s*\{[^}]*repeat\(2, minmax\(0, 1fr\)\)/,
    );
    expect(css).toMatch(
      /@media \(max-width: 520px\)[\s\S]*?\.gs-steps\s*\{\s*grid-template-columns: 1fr/,
    );
  });

  it("keeps the footer discovery links alongside the pinned legal links", () => {
    // The link sets moved to lib/i18n/links.ts, so assert the rendered
    // contract for en AND zh rather than scraping literals out of the TSX.
    for (const locale of ["en", "zh"]) {
      const product = footerProductLinks(locale, getChrome(locale)).map((l) => l.href);
      expect(product, `${locale} footer product`).toContain(`/${locale}/docs/guide`);
      expect(product, `${locale} footer product`).toContain(`/${locale}/faq`);
    }
    const license = footerProjectLinks("en", getChrome("en")).at(-1);
    expect(license).toEqual({
      label: "MIT license",
      href: "https://github.com/codewhale-hq/CodeWhale/blob/main/LICENSE",
    });
    // zh gets the footer legal labels from its dictionary, not English.
    expect(footerLegalLinks("zh", getChrome("zh")).map((l) => l.label)).toEqual([
      "服务条款",
      "隐私政策",
    ]);
    expect(footer).toContain("footerProductLinks(locale, chrome)");
    expect(footer).toContain("footerProjectLinks(locale, chrome)");
    expect(footer).toContain("footerLegalLinks(locale, chrome)");
  });
});

describe("homepage integration", () => {
  const homepage = webText("app/[locale]/page.tsx");

  it("renders the shared getting-started path on the homepage", () => {
    expect(homepage).toContain('import { GettingStartedSteps } from "@/components/getting-started-steps"');
    expect(homepage).toContain("<GettingStartedSteps locale={locale} />");
    expect(homepage).toContain("product-start");
    expect(homepage).toContain("/docs/guide");
    expect(homepage).toContain("/docs/vocabulary");
  });

  it("keeps the published release separate from the source candidate", () => {
    expect(homepage).toContain("facts.latestPublishedRelease");
    // The unreleased-source label is the EN dictionary value the page renders
    // (plain "Unreleased", per docs/design/WEB_VOICE.md).
    expect(homepage).toContain("d.sourceCandidate");
    expect(getHome("en").sourceCandidate).toBe("Unreleased");
  });
});
