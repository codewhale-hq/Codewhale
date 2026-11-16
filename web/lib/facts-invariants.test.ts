import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FACTS } from "./facts.generated";
import { withBuildProvenance, isPublishedRelease, type RepoFacts } from "./facts";
import { fetchRepoStats, readRepoStatsSnapshot } from "./github";
import { buildLlmsTxt } from "./llms-txt";
import { buildSoftwareApplicationJsonLd } from "./software-application-schema";
import { AVAILABILITY_ROWS } from "./content/availability";
import { APP_COPY } from "./content/app-page";
import sitemap from "../app/sitemap";
import HomePage from "../app/[locale]/page";
import ContributePage from "../app/[locale]/contribute/page";

vi.mock("next/navigation", () => ({ redirect: vi.fn(), notFound: vi.fn(), usePathname: () => "/en", useRouter: () => ({}) }));

const snapshot = JSON.parse(readFileSync(new URL("../data/repo-stats-snapshot.json", import.meta.url), "utf8")) as { stars: number };
const published = JSON.parse(readFileSync(new URL("../data/latest-published-release.json", import.meta.url), "utf8")) as {
  tag: string; version: string; publishedAt: string; url: string; readAt: string; assets: { name: string; size: number; sha256: string | null }[];
};

type Page = (props: { params: Promise<{ locale: string }>; searchParams: Promise<Record<string, string>> }) => Promise<unknown> | unknown;
const render = async (page: Page, locale = "en") =>
  renderToStaticMarkup((await page({ params: Promise.resolve({ locale }), searchParams: Promise.resolve({}) })) as never);

function failEveryGitHubCall() {
  vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("network down"); }));
  vi.spyOn(console, "error").mockImplementation(() => {});
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe("the home page and /contribute with every GitHub call failing", () => {
  beforeEach(failEveryGitHubCall);

  it.each(["en", "zh"])("render in %s with the snapshot numbers and no zero", async (locale) => {
    for (const page of [HomePage, ContributePage]) {
      const html = await render(page as Page, locale);
      expect(html).toContain("<h1");
      const numbers = [...html.matchAll(/class="[^"]*\btabular\b[^"]*"[^>]*>([^<]*)</g)].map((match) => match[1].trim());
      expect(numbers).not.toContain("0");
    }
    const numberLocale = locale === "zh" ? "zh-CN" : "en-US";
    expect(await render(ContributePage as Page, locale)).toContain(new Intl.NumberFormat(numberLocale).format(snapshot.stars));
    expect(await render(HomePage as Page, locale)).toContain(`>${new Intl.NumberFormat(numberLocale).format(FACTS.contributorCount)}<`);
  });

  it("states the published release, not the source version, on the home page", async () => {
    const html = await render(HomePage as Page);
    expect(html).toContain(`Version ${published.version} is released, and it needs no account.`);
    if (FACTS.version !== published.version) expect(html).not.toContain(`Version ${FACTS.version} is released`);
  });
});

describe("a live number that cannot be read", () => {
  const unknown = { stars: null, forks: null, openIssues: null, openPulls: null, contributors: null, fetchedAt: null, source: "unknown" };

  it("is unknown, never 0 and never a floor, when GitHub refuses or is unreachable", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response("rate limited", { status: 403 })));
    expect(await fetchRepoStats()).toEqual(unknown);
    failEveryGitHubCall();
    expect(await fetchRepoStats()).toEqual(unknown);
  });

  it("leaves only the missing number unknown when one call fails", async () => {
    vi.stubGlobal("fetch", vi.fn(async (url: string) => {
      if (url.includes("/search/issues") || url.includes("/contributors") || url.includes("/releases/")) return new Response("{}", { status: 403 });
      return Response.json({ stargazers_count: 1234, forks_count: 56, open_issues_count: 70 });
    }));
    const stats = await fetchRepoStats();
    expect(stats).toMatchObject({ stars: 1234, forks: 56, openPulls: null, openIssues: null, contributors: null, source: "live" });
  });

  it("reads the committed snapshot on the production build path and rejects a bad snapshot", async () => {
    vi.stubEnv("NEXT_PHASE", "phase-production-build");
    const stats = await fetchRepoStats();
    expect(stats.source).toBe("snapshot");
    expect(stats.stars).toBe(snapshot.stars);
    expect(Date.parse(stats.fetchedAt ?? "")).toBeGreaterThan(0);
    for (const bad of [null, {}, { stars: 0, readAt: "2026-01-01T00:00:00Z" }, { stars: 5, readAt: "never" }]) {
      expect(readRepoStatsSnapshot(bad)).toEqual(unknown);
    }
  });
});

describe("the version a visitor gets on install", () => {
  it("is the one published-release object, in the data file, the facts and every line that names it", () => {
    expect(published.tag).toBe(`v${published.version}`);
    expect(published.assets.length).toBeGreaterThan(0);
    expect(Date.parse(published.readAt)).toBeGreaterThan(0);
    expect(FACTS.latestPublishedRelease).toEqual(published);
    expect(isPublishedRelease(FACTS.latestPublishedRelease)).toBe(true);

    const llms = buildLlmsTxt();
    expect(llms).toContain(`The latest release is v${published.version}.`);
    expect(llms).toContain(`read ${published.readAt.slice(0, 10)}`);
    expect(llms).toContain(`This installs the latest published release, v${published.version}.`);
    if (FACTS.version !== published.version) {
      expect(llms.split("\n").filter((line) => line.includes("latest published release")).join("\n")).not.toContain(`v${FACTS.version}`);
    }
    expect(buildSoftwareApplicationJsonLd(published).softwareVersion).toBe(published.version);
  });

  it("keeps the source version out of the installed-version copy", () => {
    const home = readFileSync(new URL("../app/[locale]/page.tsx", import.meta.url), "utf8");
    expect(home).toContain("fill(d.installNote, { version: release.version })");
    expect(home).not.toContain("facts.version");
  });
});

describe("build provenance", () => {
  const base = { ...FACTS, generatedAt: "2020-01-01T00:00:00.000Z", sourceRevision: null, sourceCommittedAt: null } as RepoFacts;
  const revision = "a".repeat(40);

  it("stamps revision, commit time and generation time together", () => {
    const stamped = withBuildProvenance(base, { revision, committedAt: "2026-11-10T10:00:00Z", generatedAt: "2026-11-10T11:00:00.000Z" });
    expect(stamped).toMatchObject({ sourceRevision: revision, sourceCommittedAt: "2026-11-10T10:00:00Z", generatedAt: "2026-11-10T11:00:00.000Z" });
  });

  it.each([
    { revision, committedAt: "2026-11-10T10:00:00Z" },
    { revision, generatedAt: "2026-11-10T11:00:00.000Z" },
    { committedAt: "2026-11-10T10:00:00Z", generatedAt: "2026-11-10T11:00:00.000Z" },
    { revision: "abc", committedAt: "2026-11-10T10:00:00Z", generatedAt: "2026-11-10T11:00:00.000Z" },
  ])("never mixes a fresh stamp with the tracked file's date: %j", (stamp) => {
    expect(withBuildProvenance(base, stamp)).toBe(base);
  });
});

describe("the desktop app is not called native", () => {
  it("in the availability row and the app page, in English and Chinese", () => {
    const desktop = AVAILABILITY_ROWS.find((row) => row.id === "desktop")!;
    const copy = [desktop.detail, APP_COPY.title, APP_COPY.metadata.description].flatMap((text) => [text.en, text.zh]);
    for (const sentence of copy) expect(sentence).not.toMatch(/native|原生/i);
  });
});

describe("no price, currency amount or plan name in the public surface", () => {
  const PRICE = new RegExp([
    String.raw`[$€£¥￥]\s?\d`,
    String.raw`\b\d[\d,.]*\s?(?:USD|CNY|RMB|EUR|GBP)\b`,
    String.raw`\bper (?:month|seat|user|year)\b`,
    String.raw`/\s?(?:month|mo|seat|user|year)\b`,
    String.raw`\b(?:Free|Pro|Plus|Team|Teams|Max|Starter|Premium|Business|Enterprise|Individual)\s+(?:plan|tier|subscription)s?\b`,
    String.raw`\b(?:plan|tier)s?\s+(?:price|pricing)`,
    String.raw`\bpricing\b`,
    String.raw`\bbilled\b`,
    String.raw`\bfree trial\b`,
  ].join("|"), "i");
  const STRUCTURED_PRICE = /"(?:offers?|price|priceCurrency|priceSpecification)"|\bOffer\b/;

  const pages: [string, () => Promise<{ default: unknown }>][] = [
    ["home", () => import("../app/[locale]/page")],
    ["app", () => import("../app/[locale]/app/page")],
    ["features", () => import("../app/[locale]/features/page")],
    ["docs", () => import("../app/[locale]/docs/page")],
    ["faq", () => import("../app/[locale]/faq/page")],
    ["contribute", () => import("../app/[locale]/contribute/page")],
    ["signin", () => import("../app/[locale]/signin/page")],
    ["install", () => import("../app/[locale]/install/page")],
    ["models", () => import("../app/[locale]/models/page")],
    ["plugins", () => import("../app/[locale]/plugins/page")],
    ["computer-use", () => import("../app/[locale]/computer-use/page")],
    ["docs/constitution", () => import("../app/[locale]/docs/constitution/page")],
  ];

  beforeEach(failEveryGitHubCall);

  it.each(["en", "zh"])("holds on every public page in %s, in the visible text and in structured data", async (locale) => {
    for (const [name, load] of pages) {
      const html = await render((await load()).default as Page, locale);
      const structured = [...html.matchAll(/<script[^>]*type="application\/ld\+json"[^>]*>([\s\S]*?)<\/script>/g)].map((match) => match[1]).join("\n");
      const text = html.replace(/<script[\s\S]*?<\/script>/g, " ").replace(/<[^>]+>/g, " ");
      expect(text.match(PRICE)?.[0], `${locale}/${name} text`).toBeUndefined();
      expect(structured.match(PRICE)?.[0], `${locale}/${name} structured data`).toBeUndefined();
      expect(structured.match(STRUCTURED_PRICE)?.[0], `${locale}/${name} structured data`).toBeUndefined();
    }
  });

  it("holds for the software-application structured data and the sitemap", () => {
    expect(JSON.stringify(buildSoftwareApplicationJsonLd(published))).not.toMatch(STRUCTURED_PRICE);
    for (const entry of sitemap()) expect(entry.url, entry.url).not.toMatch(/pric|plans?\b|tier|billing|checkout|merch/i);
  });
});
