import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ALL_LOCALES } from "./i18n/config";
import { getChrome } from "./i18n/dictionaries";
import { NextRequest } from "next/server";
import { middleware } from "../middleware";
import { footerDocsLinks, footerLegalLinks, footerProductLinks, footerProjectLinks, navLinks, signInLink } from "./i18n/links";
import { PERMANENT_ROUTE_REDIRECTS, REDIRECTED_ROUTES } from "./route-redirects";
import { AVAILABILITY_ROWS, describeRow } from "./content/availability";
import { DESKTOP_RELEASE, desktopDownload, parseDesktopRelease, type DesktopReleaseFact } from "./desktop-release";
import { FOUNDER_SWITCHES, FOUNDER_SWITCH_VALUES } from "./founder-switches";
import { STATE_CHROME_KEY, SURFACE_IDS, SURFACE_STATES, stateLabel, surfaceCapabilities, surfaceState } from "./surface-states";
import { DesktopDownloadBlock } from "../components/desktop-download";
import HomePage from "../app/[locale]/page";
import ContributePage from "../app/[locale]/contribute/page";

const holder = vi.hoisted(() => ({ release: null as unknown }));
vi.mock("./desktop-release", async (importOriginal) => {
  const real = await importOriginal<typeof import("./desktop-release")>();
  return {
    ...real,
    get DESKTOP_RELEASE() {
      return holder.release;
    },
  };
});
vi.mock("next/navigation", () => ({ redirect: vi.fn(), notFound: vi.fn(), usePathname: () => "/en", useRouter: () => ({}) }));

const WEB = new URL("..", import.meta.url).pathname;
const LOCALES = ALL_LOCALES.map((entry) => entry.code);
const SHA = "a".repeat(64);

const fixture: DesktopReleaseFact = {
  tag: "v1.2.3",
  version: "1.2.3",
  publishedAt: "2026-12-01T10:00:00Z",
  readAt: "2026-12-01T11:00:00Z",
  url: "https://github.com/codewhale-hq/codewhale-app/releases/tag/v1.2.3",
  assets: [
    {
      name: "Codewhale-1.2.3-arm64.dmg",
      platform: "macos",
      size: 123_456_789,
      sha256: SHA,
      signed: true,
      notarized: true,
      url: "https://github.com/codewhale-hq/codewhale-app/releases/download/v1.2.3/Codewhale-1.2.3-arm64.dmg",
    },
  ],
};

type Page = (props: { params: Promise<{ locale: string }>; searchParams: Promise<Record<string, string>> }) => Promise<unknown> | unknown;
const render = async (page: Page, locale = "en") =>
  renderToStaticMarkup((await page({ params: Promise.resolve({ locale }), searchParams: Promise.resolve({}) })) as never);

beforeEach(() => {
  holder.release = null;
  vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("network down"); }));
  vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("every surface state has copy in every locale", () => {
  it.each(LOCALES)("%s", (locale) => {
    const chrome = getChrome(locale);
    for (const state of SURFACE_STATES) {
      const label = stateLabel(state, locale);
      expect(label, `${locale} ${state}`).toBeTruthy();
      expect(label).toBe(chrome[STATE_CHROME_KEY[state]]);
    }
    expect(chrome.numbersAsOf).toContain("{date}");
    expect(chrome.downloadVersion).toContain("{version}");
    expect(chrome.downloadSize).toContain("{size}");
    for (const key of ["downloadMacos", "downloadChecksum", "downloadSigned"] as const) expect(chrome[key]).toBeTruthy();
  });

  it("gives every state its own word within a locale", () => {
    for (const locale of LOCALES) {
      const labels = SURFACE_STATES.map((state) => stateLabel(state, locale));
      expect(new Set(labels).size, locale).toBe(labels.length);
    }
  });

  it("maps every state to a chrome key", () => {
    expect(Object.keys(STATE_CHROME_KEY).sort()).toEqual([...SURFACE_STATES].sort());
  });
});

describe("today's surface states", () => {
  it("are the committed values", () => {
    expect(Object.fromEntries(SURFACE_IDS.map((id) => [id, surfaceState(id)]))).toEqual({
      terminal: "released",
      "local-browser": "released",
      vscode: "available",
      desktop: "dev",
      "hosted-web": "not_open",
      "cloud-computers": "invite_only",
      accounts: "open",
      phone: "not_public",
      "computer-use": "beta",
    });
  });

  it("read in English and Chinese as the permitted wording", () => {
    expect(stateLabel("dev", "en")).toBe("Development build");
    expect(stateLabel("not_open", "en")).toBe("Not open yet");
    expect(stateLabel("invite_only", "en")).toBe("Invite-only");
    expect(stateLabel("dev", "zh")).toBe("开发版本");
    expect(stateLabel("not_open", "zh")).toBe("暂未开放");
  });

  it("render the status of every availability row from its state", () => {
    for (const row of AVAILABILITY_ROWS) {
      const view = describeRow(row, "en");
      expect(view.status).toBe(stateLabel(surfaceState(row.id), "en"));
      expect(view.download).toBeNull();
    }
  });

  it("agree with the sign-in page, which says cloud computers are invite-only", async () => {
    const { ACCOUNT_ENTRY_COPY } = await import("./content/account-entry");
    expect(surfaceState("cloud-computers")).toBe("invite_only");
    expect(ACCOUNT_ENTRY_COPY.lede.en).toMatch(/invite-only/);
  });
});

describe("founder-set switches", () => {
  it("default to off, each with the decision that set it", () => {
    expect(FOUNDER_SWITCH_VALUES).toEqual({ cloudInvitationsOpen: false, plansPublished: false, cloudComputersBeyondInvite: false, namedVendorAgentsOverAcp: false });
    for (const entry of Object.values(FOUNDER_SWITCHES)) expect(entry.decision).toMatch(/§\d/);
  });

  it("are not read from the environment", () => {
    const source = readFileSync(join(WEB, "lib/founder-switches.ts"), "utf8");
    expect(source).not.toMatch(/process\.env|import\.meta\.env|getEnv/);
  });

  it("open a request path only for cloud computers and only when the cloud switch is on", () => {
    const on = { ...FOUNDER_SWITCH_VALUES, cloudInvitationsOpen: true };
    expect(surfaceCapabilities("cloud-computers").requestAccess).toBe(false);
    expect(surfaceCapabilities("cloud-computers", { switches: on }).requestAccess).toBe(true);
    for (const id of SURFACE_IDS.filter((candidate) => candidate !== "cloud-computers")) {
      expect(surfaceCapabilities(id, { switches: on }).requestAccess, id).toBe(false);
    }
  });

  it("never link a surface that is not open or not public", () => {
    expect(surfaceCapabilities("hosted-web").link).toBe(false);
    expect(surfaceCapabilities("phone").link).toBe(false);
    expect(surfaceCapabilities("terminal").link).toBe(true);
  });
});

describe("the desktop download", () => {
  it("is absent today, so the desktop state is a development build with no download", () => {
    expect(DESKTOP_RELEASE).toBeNull();
    expect(readFileSync(join(WEB, "data/desktop-published-release.json"), "utf8").trim()).toBe("null");
    expect(surfaceState("desktop")).toBe("dev");
    expect(surfaceCapabilities("desktop").download).toBe(false);
    expect(desktopDownload(null)).toBeNull();
  });

  it("is offered only for a signed, notarized macOS asset with a checksum", () => {
    expect(parseDesktopRelease(fixture)).toEqual(fixture);
    const asset = fixture.assets[0];
    const variants: unknown[] = [
      { ...fixture, assets: [{ ...asset, signed: false }] },
      { ...fixture, assets: [{ ...asset, notarized: false }] },
      { ...fixture, assets: [{ ...asset, sha256: null }] },
      { ...fixture, assets: [{ ...asset, sha256: "abc" }] },
      { ...fixture, assets: [{ ...asset, platform: "windows" }] },
      { ...fixture, assets: [{ ...asset, size: 0 }] },
      { ...fixture, assets: [{ ...asset, url: "http://github.com/codewhale-hq/x.dmg" }] },
      { ...fixture, assets: [{ ...asset, url: "https://example.com/x.dmg" }] },
      { ...fixture, tag: "1.2.3" },
      { ...fixture, version: "9.9.9" },
      { ...fixture, publishedAt: "never" },
      { ...fixture, assets: [] },
    ];
    for (const variant of variants) expect(desktopDownload(parseDesktopRelease(variant)), JSON.stringify(variant)).toBeNull();
  });

  it("makes the desktop state released and shows version, size and checksum", () => {
    expect(surfaceState("desktop", { desktopRelease: fixture })).toBe("released");
    const row = AVAILABILITY_ROWS.find((candidate) => candidate.id === "desktop")!;
    const view = describeRow(row, "en", fixture);
    expect(view.status).toBe("Released");
    expect(view.detail).not.toMatch(/No public download/);
    const html = renderToStaticMarkup(DesktopDownloadBlock({ download: view.download!, locale: "en" }));
    expect(html).toContain("Download for macOS");
    expect(html).toContain(fixture.assets[0].url);
    expect(html).toContain("Version 1.2.3");
    expect(html).toContain("123.5 MB");
    expect(html).toContain(SHA);
  });

  it.each(["en", "zh"])("renders on /app in %s only when the release object is present", async (locale) => {
    const { default: AppPage } = await import("../app/[locale]/app/page");
    for (const page of [AppPage]) {
      holder.release = null;
      expect(await render(page as Page, locale)).not.toContain(SHA);
      holder.release = fixture;
      const html = await render(page as Page, locale);
      expect(html).toContain(SHA);
      expect(html).toContain(getChrome(locale).downloadMacos);
      expect(html).toContain("1.2.3");
    }
  });
});

const PAGES: [string, () => Promise<{ default: unknown }>][] = [
  ["", () => import("../app/[locale]/page")],
  ["app", () => import("../app/[locale]/app/page")],
  ["install", () => import("../app/[locale]/install/page")],
  ["signin", () => import("../app/[locale]/signin/page")],
  ["faq", () => import("../app/[locale]/faq/page")],
  ["features", () => import("../app/[locale]/features/page")],
  ["models", () => import("../app/[locale]/models/page")],
  ["plugins", () => import("../app/[locale]/plugins/page")],
  ["computer-use", () => import("../app/[locale]/computer-use/page")],
  ["contribute", () => import("../app/[locale]/contribute/page")],
  ["docs", () => import("../app/[locale]/docs/page")],
  ["docs/constitution", () => import("../app/[locale]/docs/constitution/page")],
];

describe("with both founder switches off, no page offers a download or a request path", () => {
  const pages = PAGES;
  const INSTALLER = /\.(?:dmg|pkg|msi|exe)(?:["?#]|$)|downloads\.codewhale\.net/i;
  const REQUEST_PATH = /request access|request an invite|request an invitation|join the waitlist|waitlist|apply for access/i;

  it.each(["en", "zh"])("holds in %s", async (locale) => {
    const downloadWords = [getChrome("en").downloadMacos, getChrome("zh").downloadMacos];
    for (const [name, load] of pages) {
      const html = await render((await load()).default as Page, locale);
      const text = html.replace(/<script[\s\S]*?<\/script>/g, " ").replace(/<[^>]+>/g, " ");
      const hrefs = [...html.matchAll(/href="([^"]*)"/g)].map((match) => match[1]);
      expect(hrefs.find((href) => INSTALLER.test(href)), `${locale}/${name} installer link`).toBeUndefined();
      expect(text.match(REQUEST_PATH)?.[0], `${locale}/${name} request path`).toBeUndefined();
      for (const word of downloadWords) expect(text, `${locale}/${name} download button`).not.toContain(word);
    }
  });
});

describe("no page links to a missing or gone route", () => {
  const ROUTE_ROOT = join(WEB, "app/[locale]");
  const REDIRECTS = new Set([...REDIRECTED_ROUTES, "signup", "terms", "privacy", "legal"]);

  function routeExists(path: string): boolean {
    const segments = path.split("/").filter(Boolean);
    if (segments.length === 0) return true;
    if (REDIRECTS.has(segments[0])) return true;
    let dir = ROUTE_ROOT;
    for (const segment of segments) {
      if (existsSync(join(dir, segment))) {
        dir = join(dir, segment);
        continue;
      }
      const dynamic = existsSync(dir) && readdirSync(dir).find((entry) => /^\[[^.\]]+\]$/.test(entry));
      if (!dynamic) return false;
      dir = join(dir, dynamic);
    }
    return existsSync(join(dir, "page.tsx"));
  }

  it.each(["en", "zh"])("answers every header and footer link in %s with a page that exists", (locale) => {
    const chrome = getChrome(locale);
    const links = [
      ...navLinks(locale, chrome, true),
      signInLink(locale, chrome),
      ...footerProductLinks(locale, chrome),
      ...footerDocsLinks(locale, chrome),
      ...footerProjectLinks(locale, chrome),
      ...footerLegalLinks(locale, chrome, "Usage"),
    ];
    for (const link of links.filter((l) => l.href.startsWith("/"))) {
      const path = link.href.replace(/#.*$/, "").replace(new RegExp(`^/${locale}`), "");
      expect(routeExists(path), `${locale} chrome link ${link.href}`).toBe(true);
      expect(REDIRECTED_ROUTES.includes(path.split("/")[1] ?? ""), `${link.href} is a redirect`).toBe(false);
    }
    for (const link of links.filter((l) => !l.href.startsWith("/"))) expect(link.href, link.label).toMatch(/^https:\/\//);
  });

  it.each(["", "/en", "/zh", "/ar"])("redirects every merged route permanently to a page that exists, under %s", (prefix) => {
    for (const [segment, target] of Object.entries(PERMANENT_ROUTE_REDIRECTS)) {
      const res = middleware(new NextRequest(new URL(`https://codewhale.net${prefix}/${segment}`), {
        headers: new Headers({ host: "codewhale.net", "accept-language": "ar" }),
      }));
      const locale = prefix.slice(1) || "ar";
      expect(res.status, `${prefix}/${segment}`).toBe(308);
      expect(res.headers.get("location"), `${prefix}/${segment}`).toBe(`https://codewhale.net/${locale}${target}`);
      expect(routeExists(target.replace(/#.*$/, "")), `${target} exists`).toBe(true);
    }
  });

  it("never links the hosted app root while its state is not open", async () => {
    expect(surfaceState("hosted-web")).toBe("not_open");
    for (const [path, load] of PAGES) {
      const page = await load();
      for (const locale of ["en", "zh"]) {
        const html = await render(page.default as Page, locale);
        const hrefs = [...html.matchAll(/href="([^"]*)"/g)].map((match) => match[1]);
        const root = hrefs.find((href) => /^https:\/\/app\.codewhale\.net\/?(?:[?#].*)?$/.test(href));
        expect(root, `${locale}/${path} links the hosted app root`).toBeUndefined();
      }
    }
  });

  it("links only to localized pages that exist", async () => {
    for (const [path, load] of PAGES) {
      const page = await load();
      const html = await render(page.default as Page, "en");
      const hrefs = [...html.matchAll(/href="(\/en(?:\/[^"?#]*)?)(?:[?#][^"]*)?"/g)].map((match) => match[1]);
      for (const href of hrefs) expect(routeExists(href.replace(/^\/en/, "")), `/en/${path} links ${href}`).toBe(true);
    }
  });
});

describe("numbers from the build-time snapshot carry the date they were read", () => {
  it.each(LOCALES)("on the home page in %s", async (locale) => {
    const snapshot = JSON.parse(readFileSync(join(WEB, "data/repo-stats-snapshot.json"), "utf8")) as { readAt: string };
    const date = new Intl.DateTimeFormat(getChrome(locale).dateLocale, { dateStyle: "medium", timeZone: "UTC" }).format(new Date(snapshot.readAt));
    const label = getChrome(locale).numbersAsOf.replace("{date}", date);
    const html = await render(HomePage as Page, locale);
    const counts = [...html.matchAll(/<p class="home-accounts tabular"[^>]*>/g)].map((match) => match[0]);
    expect(counts).toHaveLength(1);
    expect(counts[0]).toContain(`title="${label}"`);
    expect(html).toContain(`<span class="sr-only"> ${label}</span>`);
    expect(html.replace(/<span class="sr-only">[^<]*<\/span>/g, "").replace(/title="[^"]*"/g, "")).not.toContain(label);
  });

  it("on /contribute", async () => {
    expect(await render(ContributePage as Page)).toMatch(/title="As of [^"]+"/);
  });
});
