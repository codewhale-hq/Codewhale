import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { authorsBetween, creditedPairs, releaseCredits } from "../scripts/repo-activity-lib.mjs";
import ContributePage from "../app/[locale]/contribute/page";
import HomePage from "../app/[locale]/page";
import { readAsOf } from "../components/project-pulse";
import { GET as factsRoute } from "../app/api/facts/route";
import { ALL_LOCALES } from "./i18n/config";
import { getContribute } from "./i18n/dictionaries";
import { pluralCategoriesFor, pluralCategory, pluralText } from "./i18n/plural";
import {
  LIVE_RECORD_MAX_AGE_MS,
  isExcluded,
  liveRecord,
  parseExclusions,
  readRepoActivity,
  releaseAuthors,
  repoActivityView,
} from "./repo-activity";

vi.mock("next/navigation", () => ({ redirect: vi.fn(), notFound: vi.fn(), usePathname: () => "/en", useRouter: () => ({}) }));

const webRoot = new URL("../", import.meta.url);
const snapshot = JSON.parse(readFileSync(new URL("data/repo-stats-snapshot.json", webRoot), "utf8")) as Record<string, unknown>;
const exclusionsFile = JSON.parse(readFileSync(new URL("data/contributor-exclusions.json", webRoot), "utf8")) as {
  confirmed: boolean;
  logins: { login: string; reason: string }[];
  suffixes: { suffix: string; reason: string }[];
};

const avatar = (login: string) => `https://avatars.githubusercontent.com/${login}`;
const pull = (number: number, login: string, mergedAt: string) => ({
  number,
  title: `change ${number}`,
  url: `https://github.com/example/repo/pull/${number}`,
  login,
  avatar: avatar(login),
  mergedAt,
  firstTime: false,
});

type Page = (props: { params: Promise<{ locale: string }> }) => Promise<unknown> | unknown;
const render = async (page: Page, locale: string) =>
  renderToStaticMarkup((await page({ params: Promise.resolve({ locale }) })) as never);

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("pages with every GitHub call failing", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("network down"); }));
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  it.each(["en", "zh", "ar"])("render the home page and /contribute in %s with no zero and no empty number", async (locale) => {
    for (const page of [HomePage, ContributePage]) {
      const html = await render(page as Page, locale);
      expect(html).toContain("<h1");
      const numbers = [...html.matchAll(/class="[^"]*\btabular\b[^"]*"[^>]*>([^<]*)</g)].map((match) => match[1].trim());
      expect(numbers.length).toBeGreaterThan(0);
      for (const number of numbers) {
        expect(number).not.toBe("");
        expect(number).not.toBe("0");
      }
      expect(html).not.toContain("api.github.com");
    }
  });

  it("shows the merged people, their faces and the milestones block on /contribute", async () => {
    const html = await render(ContributePage as Page, "en");
    const view = repoActivityView();
    expect(view?.count).toBeGreaterThan(0);
    expect(html).toContain(`${view?.count} GitHub accounts had pull requests merged in the last 30 days.`);
    expect(html).toContain('id="milestones"');
    const person = view?.people[0];
    expect(html).toContain(`alt="${person?.login}"`);
    expect(html).toContain(view!.pulls[0].url);
  });

  it("announces the read date of the count and the milestone progress to assistive technology", async () => {
    const html = await render(ContributePage as Page, "en");
    const asOf = readAsOf(String(snapshot.readAt), "en");
    expect(asOf).toBeTruthy();
    expect(html).toMatch(new RegExp(`<p class="page-meta tabular" title="${asOf}">[^<]+<span class="sr-only"> ${asOf}</span></p>`));
    const progress = html.match(/<span class="tide-progress"[\s\S]*?<\/dd>/g) ?? [];
    expect(progress.length).toBeGreaterThan(0);
    for (const block of progress) {
      expect(block).toContain(`title="${asOf}"`);
      expect(block).toContain(`<span class="sr-only">, ${asOf}</span></span></dd>`);
    }
    expect(html).not.toMatch(/<p[^>]*aria-label=/);
  });
});

describe("no browser call to GitHub", () => {
  const sources: string[] = [];
  for (const dir of ["components", "app", "lib"]) {
    const walk = (path: string) => {
      for (const name of readdirSync(path)) {
        const full = join(path, name);
        if (statSync(full).isDirectory()) walk(full);
        else if (/\.(ts|tsx)$/.test(name) && !name.endsWith(".test.ts") && !name.endsWith(".test.tsx")) sources.push(full);
      }
    };
    walk(new URL(`${dir}/`, webRoot).pathname);
  }

  it("keeps GitHub API URLs out of every client component", () => {
    const clientSources = sources.filter((path) => /^\s*["']use client["']/m.test(readFileSync(path, "utf8")));
    expect(clientSources.length).toBeGreaterThan(0);
    for (const path of clientSources) {
      const source = readFileSync(path, "utf8");
      expect(source, path).not.toContain("api.github.com");
      expect(source, path).not.toMatch(/\/repos\/codewhale-hq/);
    }
  });

  it("has no GitHub feed route left", () => {
    expect(() => statSync(new URL("app/api/github/feed/route.ts", webRoot))).toThrow();
  });
});

describe("the contributor exclusion list", () => {
  it("is public data with a reason for every entry and starts unconfirmed", () => {
    expect(exclusionsFile.confirmed).toBe(false);
    for (const entry of [...exclusionsFile.logins, ...exclusionsFile.suffixes]) expect(entry.reason.length).toBeGreaterThan(10);
  });

  it("removes bots from the count and the people", () => {
    const fixture = {
      readAt: "2026-11-11T00:00:00Z",
      windowDays: 30,
      merged: [
        pull(1, "alice", "2026-11-10T00:00:00Z"),
        pull(2, "dependabot[bot]", "2026-11-09T00:00:00Z"),
        pull(3, "github-actions[bot]", "2026-11-09T00:00:00Z"),
        pull(4, "some-new-app[bot]", "2026-11-08T00:00:00Z"),
        pull(5, "bob", "2026-11-07T00:00:00Z"),
        pull(6, "alice", "2026-11-06T00:00:00Z"),
        pull(7, "old", "2026-09-01T00:00:00Z"),
      ],
      milestones: [],
      releases: [],
    };
    const activity = readRepoActivity({ readAt: fixture.readAt, activity: fixture });
    expect(activity).not.toBeNull();
    const view = repoActivityView(activity, parseExclusions(exclusionsFile));
    expect(view?.count).toBe(2);
    expect(view?.people.map((person) => person.login)).toEqual(["alice", "bob"]);
    expect(view?.pulls.map((item) => item.number)).toEqual([1, 5, 6]);
    expect(repoActivityView(activity, parseExclusions({}))?.count).toBe(5);
    expect(isExcluded("Dependabot[bot]", parseExclusions(exclusionsFile))).toBe(true);
    expect(isExcluded("alice", parseExclusions(exclusionsFile))).toBe(false);
  });

  it("flips the noun from accounts to people with one switch", () => {
    const t = getContribute("en");
    expect(pluralText(t.mergedAccountsCount, 3, "en")).toBe("3 GitHub accounts had pull requests merged in the last 30 days.");
    expect(pluralText(t.mergedPeopleCount, 3, "en")).toBe("3 people had pull requests merged in the last 30 days.");
    expect(parseExclusions({ confirmed: true }).confirmed).toBe(true);
    expect(parseExclusions(exclusionsFile).confirmed).toBe(false);
  });
});

describe("the committed snapshot", () => {
  it("reads as activity with merged pull requests, milestones and release credits", () => {
    const activity = readRepoActivity();
    expect(activity?.merged.length).toBeGreaterThan(0);
    expect(activity?.milestones.length).toBeGreaterThan(0);
    expect(activity?.releases.length).toBeGreaterThan(0);
    for (const release of activity!.releases) expect(releaseAuthors(release.tag)?.length).toBeGreaterThan(0);
  });

  it("is unknown, never a partial, when any part is malformed", () => {
    expect(readRepoActivity(null)).toBeNull();
    expect(readRepoActivity({ readAt: snapshot.readAt })).toBeNull();
    const activity = snapshot.activity as { merged: unknown[] };
    expect(readRepoActivity({ ...snapshot, activity: { ...activity, merged: [...activity.merged, { number: 1 }] } })).toBeNull();
    expect(readRepoActivity({ ...snapshot, readAt: "not a date" })).toBeNull();
  });
});

describe("release-scoped authors", () => {
  const releases = [
    { tag_name: "v2", html_url: "https://github.com/example/repo/releases/tag/v2", published_at: "2026-02-10T12:00:00Z" },
    { tag_name: "v1", html_url: "https://github.com/example/repo/releases/tag/v1", published_at: "2026-02-01T12:00:00Z" },
    { tag_name: "v0", html_url: "https://github.com/example/repo/releases/tag/v0", published_at: "2026-01-15T12:00:00Z" },
  ];
  const pulls = [
    pull(1, "before", "2026-01-20T00:00:00Z"),
    pull(2, "at-previous-tag", "2026-02-01T12:00:00Z"),
    pull(3, "just-after-previous", "2026-02-01T12:00:01Z"),
    pull(4, "middle", "2026-02-05T00:00:00Z"),
    pull(5, "middle", "2026-02-06T00:00:00Z"),
    pull(6, "at-tag", "2026-02-10T12:00:00Z"),
    pull(7, "after", "2026-02-10T12:00:01Z"),
  ];

  it("returns exactly the authors merged after the previous tag and up to this one", () => {
    expect(authorsBetween(pulls, "2026-02-01T12:00:00Z", "2026-02-10T12:00:00Z").map((person) => person.login)).toEqual([
      "just-after-previous",
      "middle",
      "at-tag",
    ]);
    const credits = releaseCredits(creditedPairs(releases), () => pulls);
    expect(credits.map((credit) => credit.tag)).toEqual(["v2", "v1"]);
    expect(credits[0].authors.map((person) => person.login)).toEqual(["just-after-previous", "middle", "at-tag"]);
    expect(credits[1].authors.map((person) => person.login)).toEqual(["before", "at-previous-tag"]);
  });

  it("ignores drafts and prereleases when choosing the previous tag", () => {
    const withDraft = [{ ...releases[1], tag_name: "v1-rc", draft: true }, ...releases];
    expect(creditedPairs(withDraft).map((pair) => `${pair.release.tag_name}<-${pair.previous.tag_name}`)).toEqual(["v2<-v1", "v1<-v0"]);
  });
});

describe("the live record", () => {
  const readAt = "2026-11-11T00:00:00.000Z";
  const at = Date.parse(readAt);

  it("is fresh for seven days and stale after", () => {
    expect(liveRecord({ readAt }, at).fresh).toBe(true);
    expect(liveRecord({ readAt }, at + LIVE_RECORD_MAX_AGE_MS).fresh).toBe(true);
    expect(liveRecord({ readAt }, at + LIVE_RECORD_MAX_AGE_MS + 1).fresh).toBe(false);
    expect(liveRecord({ readAt }, at - 1).fresh).toBe(false);
    expect(liveRecord({ readAt: "nope" }, at)).toMatchObject({ readAt: null, fresh: false });
    expect(liveRecord(null, at)).toMatchObject({ readAt: null, fresh: false });
  });

  it("is served by /api/facts", async () => {
    const response = await factsRoute();
    const body = (await response.json()) as { liveRecord: { source: string; readAt: string; fresh: boolean } };
    expect(body.liveRecord.readAt).toBe(snapshot.readAt);
    expect(body.liveRecord.source).toBeTruthy();
    expect(typeof body.liveRecord.fresh).toBe("boolean");
  });
});

describe("plural-aware count sentences", () => {
  const counts = [0, 1, 2, 3, 5, 11, 100];

  it("picks the right category in Arabic, Russian and English", () => {
    expect(counts.map((n) => pluralCategory("ar", n))).toEqual(["zero", "one", "two", "few", "few", "many", "other"]);
    expect(counts.map((n) => pluralCategory("ru", n))).toEqual(["many", "one", "few", "few", "many", "many", "many"]);
    expect(counts.map((n) => pluralCategory("en", n))).toEqual(["other", "one", "other", "other", "other", "other", "other"]);
  });

  it("fills the keyed form for each count and keeps the number in it", () => {
    for (const locale of ["ar", "ru", "en"]) {
      const forms = getContribute(locale).mergedAccountsCount;
      for (const n of counts) {
        const text = pluralText(forms, n, locale, "en-US");
        expect(text).toContain(String(n));
        expect(text).not.toContain("{n}");
        expect(text).toBe((forms[pluralCategory(locale, n)] ?? forms.other).replace("{n}", String(n)));
      }
    }
    expect(pluralText(getContribute("ru").mergedAccountsCount, 2, "ru", "en-US")).toContain("2 аккаунтов");
    expect(pluralText(getContribute("ru").mergedAccountsCount, 21, "ru", "en-US")).toContain("21 аккаунта");
    expect(pluralText(getContribute("en").mergedAccountsCount, 1, "en")).toBe("1 GitHub account had a pull request merged in the last 30 days.");
  });

  it("falls back to other when a locale lacks a form", () => {
    expect(pluralText({ other: "{n} things" }, 1, "en")).toBe("1 things");
    expect(pluralText({ other: "{n} things" }, 3, "xx-invalid-locale-!", "en")).toBe("3 things");
  });

  it("gives every routed locale every category it needs, with {n} in each", () => {
    for (const locale of ALL_LOCALES.map((entry) => entry.code)) {
      const t = getContribute(locale);
      for (const forms of [t.mergedAccountsCount, t.mergedPeopleCount]) {
        for (const category of pluralCategoriesFor(locale)) {
          expect(forms[category], `${locale} ${category}`).toContain("{n}");
        }
      }
    }
    expect(pluralCategoriesFor("ar")).toEqual(["zero", "one", "two", "few", "many", "other"]);
  });
});
