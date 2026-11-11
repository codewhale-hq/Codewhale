import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { fetchFeed, fetchRepoStats, lastPageFromLink, loadFeed, relativeAge, relativeTime } from "./github";

// We test the pure helper functions directly.
// The async fetch functions require mocking the global fetch.

describe("production-build fallback", () => {
  it("keeps static generation offline and uses the dated snapshot and never a zero", async () => {
    const previousPhase = process.env.NEXT_PHASE;
    process.env.NEXT_PHASE = "phase-production-build";
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    try {
      expect(await fetchFeed(undefined, 10)).toEqual([]);
      // A page that is the feed needs to know nothing was asked for, so
      // the prerender does not pass as an honest empty record.
      expect(await loadFeed(undefined, 10)).toEqual({
        items: [],
        issuesStatus: "skipped",
        pullsStatus: "skipped",
      });
      const stats = await fetchRepoStats();
      expect(stats.source).toBe("snapshot");
      expect(stats.stars).toBeGreaterThan(0);
      expect(stats.contributors).toBeNull();
      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      if (previousPhase === undefined) delete process.env.NEXT_PHASE;
      else process.env.NEXT_PHASE = previousPhase;
      vi.unstubAllGlobals();
    }
  });
});

// ── relativeTime ──────────────────────────────────────────────────────

describe("relativeTime", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-01T12:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns 'just now' for less than 30 seconds ago", () => {
    expect(relativeTime("2026-06-01T11:59:45Z")).toBe("just now");
  });

  it("returns 'just now' for dates in the future", () => {
    expect(relativeTime("2026-06-02T12:00:00Z")).toBe("just now");
  });

  it("returns minutes for < 1 hour", () => {
    expect(relativeTime("2026-06-01T11:55:00Z")).toBe("5m");
    expect(relativeTime("2026-06-01T11:30:00Z")).toBe("30m");
  });

  it("returns hours for < 1 day", () => {
    expect(relativeTime("2026-06-01T09:00:00Z")).toBe("3h");
    expect(relativeTime("2026-05-31T18:00:00Z")).toBe("18h");
  });

  it("returns days for < 30 days", () => {
    expect(relativeTime("2026-05-25T12:00:00Z")).toBe("7d");
    expect(relativeTime("2026-05-03T12:00:00Z")).toBe("29d");
  });

  it("returns months for < 12 months", () => {
    expect(relativeTime("2026-03-01T12:00:00Z")).toBe("3mo");
    expect(relativeTime("2025-08-01T12:00:00Z")).toBe("10mo");
  });

  it("returns years for >= 12 months", () => {
    expect(relativeTime("2024-06-01T12:00:00Z")).toBe("2y");
    expect(relativeTime("2025-01-01T00:00:00Z")).toBe("1y");
  });
});

// ── relativeAge ───────────────────────────────────────────────────────

describe("relativeAge", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-01T12:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("reports past ages as the negative counts Intl.RelativeTimeFormat wants", () => {
    expect(relativeAge("2026-06-01T11:55:00Z")).toEqual({ value: -5, unit: "minute" });
    expect(relativeAge("2026-06-01T09:00:00Z")).toEqual({ value: -3, unit: "hour" });
    expect(relativeAge("2026-05-25T12:00:00Z")).toEqual({ value: -7, unit: "day" });
    expect(relativeAge("2026-03-01T12:00:00Z")).toEqual({ value: -3, unit: "month" });
    expect(relativeAge("2024-06-01T12:00:00Z")).toEqual({ value: -2, unit: "year" });
  });

  it("collapses sub-minute, future, and unparseable dates to the locale's 'now'", () => {
    expect(relativeAge("2026-06-01T11:59:45Z")).toEqual({ value: 0, unit: "second" });
    expect(relativeAge("2026-06-02T12:00:00Z")).toEqual({ value: 0, unit: "second" });
    expect(relativeAge("not-a-date")).toEqual({ value: 0, unit: "second" });
  });

  it("agrees with the compact English form it backs", () => {
    for (const iso of [
      "2026-06-01T11:59:45Z",
      "2026-06-01T11:30:00Z",
      "2026-05-31T18:00:00Z",
      "2026-05-03T12:00:00Z",
      "2025-08-01T12:00:00Z",
      "2025-01-01T00:00:00Z",
    ]) {
      const age = relativeAge(iso);
      const compact = relativeTime(iso);
      if (age.unit === "second") expect(compact).toBe("just now");
      else expect(compact.startsWith(String(Math.abs(age.value)))).toBe(true);
    }
  });
});

// ── fetchFeed ─────────────────────────────────────────────────────────

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

describe("fetchFeed", () => {
  const ISSUES = [
    {
      number: 4901,
      title: "Ticker shows nothing on first paint",
      html_url: "https://github.com/codewhale-hq/CodeWhale/issues/4901",
      state: "open",
      user: { login: "asto18089", avatar_url: "https://avatars/1" },
      created_at: "2026-06-01T09:00:00Z",
      updated_at: "2026-06-01T11:00:00Z",
      comments: 2,
      labels: [{ name: "bug", color: "d73a4a" }],
      author_association: "CONTRIBUTOR",
    },
    {
      number: 4880,
      title: "Fixed upstream",
      html_url: "https://github.com/codewhale-hq/CodeWhale/issues/4880",
      state: "closed",
      user: { login: "Inference1", avatar_url: "https://avatars/2" },
      created_at: "2026-03-02T09:00:00Z",
      updated_at: "2026-06-01T10:00:00Z",
      closed_at: "2026-05-30T09:00:00Z",
      comments: 0,
      labels: [],
      author_association: "NONE",
    },
    {
      // The issues endpoint returns pull requests too; they must not double up.
      number: 4899,
      title: "PR echoed by the issues endpoint",
      html_url: "https://github.com/codewhale-hq/CodeWhale/pull/4899",
      state: "open",
      user: { login: "bistack", avatar_url: "https://avatars/3" },
      created_at: "2026-06-01T08:00:00Z",
      updated_at: "2026-06-01T08:30:00Z",
      comments: 0,
      labels: [],
      pull_request: { url: "https://api.github.com/…" },
    },
  ];

  const PULLS = [
    {
      number: 4899,
      title: "fix(tui): keep the composer caret on resize",
      html_url: "https://github.com/codewhale-hq/CodeWhale/pull/4899",
      state: "closed",
      user: { login: "shenjackyuanjie", avatar_url: "https://avatars/4" },
      created_at: "2026-05-29T09:00:00Z",
      updated_at: "2026-06-01T11:30:00Z",
      closed_at: "2026-05-31T18:00:00Z",
      merged_at: "2026-05-31T18:00:00Z",
      comments: 4,
      labels: [],
      author_association: "FIRST_TIME_CONTRIBUTOR",
    },
    {
      number: 4902,
      title: "wip: sandbox notes",
      html_url: "https://github.com/codewhale-hq/CodeWhale/pull/4902",
      state: "open",
      user: { login: "h3c-hexin", avatar_url: "https://avatars/5" },
      created_at: "2026-06-01T07:00:00Z",
      updated_at: "2026-06-01T07:30:00Z",
      draft: true,
      comments: 0,
      labels: [],
      author_association: "CONTRIBUTOR",
    },
  ];

  const RELEASES = [
    {
      tag_name: "v0.9.4",
      name: "v0.9.4",
      html_url: "https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.4",
      created_at: "2026-05-28T09:00:00Z",
      published_at: "2026-05-28T12:00:00Z",
      author: { login: "Hmbown", avatar_url: "https://avatars/6" },
    },
    {
      tag_name: "v0.9.5-rc1",
      name: "v0.9.5-rc1",
      html_url: "https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.5-rc1",
      created_at: "2026-06-01T09:00:00Z",
      published_at: null,
      draft: true,
      author: { login: "Hmbown", avatar_url: "https://avatars/6" },
    },
  ];

  const requested: string[] = [];

  beforeEach(() => {
    requested.length = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        requested.push(url);
        if (url.includes("/issues?")) return json(ISSUES);
        if (url.includes("/pulls?")) return json(PULLS);
        if (url.includes("/releases?")) return json(RELEASES);
        return new Response("{}", { status: 404 });
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("covers issues, pulls, and releases in three calls — no per-item follow-ups", async () => {
    await fetchFeed(undefined, 10);
    expect(requested).toHaveLength(3);
    expect(requested.some((u) => u.includes("/issues?"))).toBe(true);
    expect(requested.some((u) => u.includes("/pulls?"))).toBe(true);
    expect(requested.some((u) => u.includes("/releases?"))).toBe(true);
  });

  it("names the contributor and dates each verb by the event, not the last comment", async () => {
    const feed = await fetchFeed(undefined, 10);

    const merged = feed.find((f) => f.number === 4899 && f.kind === "pull");
    expect(merged?.state).toBe("merged");
    expect(merged?.author).toBe("shenjackyuanjie");
    // Merged at 18:00 on the 31st; last touched at 11:30 the next day.
    expect(merged?.eventAt).toBe("2026-05-31T18:00:00Z");

    const openIssue = feed.find((f) => f.number === 4901);
    expect(openIssue?.state).toBe("open");
    expect(openIssue?.eventAt).toBe(openIssue?.createdAt);

    const closedIssue = feed.find((f) => f.number === 4880);
    expect(closedIssue?.state).toBe("closed");
    expect(closedIssue?.eventAt).toBe("2026-05-30T09:00:00Z");

    const draft = feed.find((f) => f.number === 4902);
    expect(draft?.state).toBe("draft");
  });

  it("passes GitHub's first-time-contributor verdict through verbatim", async () => {
    const feed = await fetchFeed(undefined, 10);
    expect(feed.find((f) => f.number === 4899 && f.kind === "pull")?.firstTimeContributor).toBe(
      true,
    );
    expect(feed.find((f) => f.number === 4901)?.firstTimeContributor).toBe(false);
    // "NONE" is not a first-timer — only GitHub's own FIRST_TIME_CONTRIBUTOR is.
    expect(feed.find((f) => f.number === 4880)?.firstTimeContributor).toBe(false);
  });

  it("carries published releases and drops unpublished drafts", async () => {
    const feed = await fetchFeed(undefined, 10);
    const releases = feed.filter((f) => f.kind === "release");
    expect(releases).toHaveLength(1);
    expect(releases[0].tag).toBe("v0.9.4");
    expect(releases[0].state).toBe("published");
    expect(releases[0].eventAt).toBe("2026-05-28T12:00:00Z");
    expect(feed.some((f) => f.tag === "v0.9.5-rc1")).toBe(false);
  });

  it("keeps a recent release in view when a busy window would bury it", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-01T12:00:00Z"));
    try {
      // Only two slots: pure recency would fill both with the newest threads.
      const feed = await fetchFeed(undefined, 2);
      expect(feed).toHaveLength(2);
      expect(feed.filter((f) => f.kind === "release").map((f) => f.tag)).toEqual(["v0.9.4"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not pin a stale release beside today's threads", async () => {
    vi.useFakeTimers();
    // Two years on from the last tag: a quiet quarter reads as a quiet quarter.
    vi.setSystemTime(new Date("2028-06-01T12:00:00Z"));
    try {
      const feed = await fetchFeed(undefined, 2);
      expect(feed).toHaveLength(2);
      expect(feed.some((f) => f.kind === "release")).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not double-count pull requests echoed by the issues endpoint", async () => {
    const feed = await fetchFeed(undefined, 10);
    expect(feed.filter((f) => f.number === 4899)).toHaveLength(1);
    expect(feed.filter((f) => f.kind === "issue").map((f) => f.number).sort()).toEqual([
      4880, 4901,
    ]);
  });

  it("returns an empty feed — never a placeholder — when GitHub is unreachable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("rate limited", { status: 403 })),
    );
    expect(await fetchFeed(undefined, 10)).toEqual([]);
  });

  it("reports a refused list call as unavailable, not as an empty record", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("rate limited", { status: 403 })),
    );
    expect(await loadFeed(undefined, 10)).toEqual({
      items: [],
      issuesStatus: "unavailable",
      pullsStatus: "unavailable",
    });
  });

  it("keeps what arrived and still flags the load when one list call fails", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        if (url.includes("/pulls?")) return new Response("rate limited", { status: 403 });
        if (url.includes("/issues?")) return json(ISSUES);
        return json([]);
      }),
    );
    const load = await loadFeed(undefined, 10);
    // Each column answers for itself: the issues list answered, so the
    // issues column must not be told "the source did not answer".
    expect(load.issuesStatus).toBe("ok");
    expect(load.pullsStatus).toBe("unavailable");
    expect(load.items.map((i) => i.number)).toEqual([4901, 4880]);
  });

  it("reports ok — so an empty list means empty — when every list call answered", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => json([])));
    expect(await loadFeed(undefined, 10)).toEqual({
      items: [],
      issuesStatus: "ok",
      pullsStatus: "ok",
    });
  });
});

describe("fetchFeed — bot authors", () => {
  // The wire is contributor-forward: GitHub's own `[bot]` login marker keeps
  // automated maintenance off the strip, while a bot-published release keeps
  // its slot and drops its byline.
  const ISSUES = [
    {
      number: 101,
      title: "Automated close sweep",
      html_url: "https://github.com/codewhale-hq/CodeWhale/issues/101",
      state: "closed",
      user: { login: "github-actions[bot]", avatar_url: "https://avatars/bot1" },
      created_at: "2026-06-01T09:00:00Z",
      updated_at: "2026-06-01T11:00:00Z",
      closed_at: "2026-06-01T10:30:00Z",
      comments: 0,
      labels: [],
      author_association: "NONE",
    },
    {
      number: 102,
      title: "Real report",
      html_url: "https://github.com/codewhale-hq/CodeWhale/issues/102",
      state: "open",
      user: { login: "vFONGv", avatar_url: "https://avatars/h1" },
      created_at: "2026-06-01T08:00:00Z",
      updated_at: "2026-06-01T08:00:00Z",
      comments: 0,
      labels: [],
      author_association: "FIRST_TIME_CONTRIBUTOR",
    },
  ];

  const PULLS = [
    {
      number: 103,
      title: "chore(deps): bump serde from 1.0.219 to 1.0.220",
      html_url: "https://github.com/codewhale-hq/CodeWhale/pull/103",
      state: "closed",
      user: { login: "dependabot[bot]", avatar_url: "https://avatars/bot2" },
      created_at: "2026-05-31T09:00:00Z",
      updated_at: "2026-06-01T07:00:00Z",
      merged_at: "2026-06-01T07:00:00Z",
      comments: 0,
      labels: [],
      author_association: "CONTRIBUTOR",
    },
    {
      number: 104,
      title: "fix(tui): keep the whale rail steady",
      html_url: "https://github.com/codewhale-hq/CodeWhale/pull/104",
      state: "closed",
      user: { login: "bistack", avatar_url: "https://avatars/h2" },
      created_at: "2026-05-30T09:00:00Z",
      updated_at: "2026-06-01T06:00:00Z",
      merged_at: "2026-06-01T06:00:00Z",
      comments: 1,
      labels: [],
      author_association: "CONTRIBUTOR",
    },
  ];

  const RELEASES = [
    {
      tag_name: "v0.9.4",
      name: "v0.9.4",
      html_url: "https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.4",
      created_at: "2026-05-28T09:00:00Z",
      published_at: "2026-05-28T12:00:00Z",
      author: { login: "release-train[bot]", avatar_url: "https://avatars/bot3" },
    },
  ];

  beforeEach(() => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        if (url.includes("/issues?")) return json(ISSUES);
        if (url.includes("/pulls?")) return json(PULLS);
        if (url.includes("/releases?")) return json(RELEASES);
        return new Response("{}", { status: 404 });
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps bot-authored issues and pulls off the contributor wire", async () => {
    const feed = await fetchFeed(undefined, 10);
    expect(feed.some((f) => f.author === "github-actions[bot]")).toBe(false);
    expect(feed.some((f) => f.author === "dependabot[bot]")).toBe(false);
    expect(feed.some((f) => f.number === 102 && f.kind === "issue")).toBe(true);
    expect(feed.some((f) => f.number === 104 && f.kind === "pull")).toBe(true);
  });

  it("keeps a bot-published release in view, minus its byline", async () => {
    const feed = await fetchFeed(undefined, 10);
    const release = feed.find((f) => f.kind === "release");
    expect(release?.tag).toBe("v0.9.4");
    expect(release?.author).toBe("");
    expect(release?.authorAvatar).toBe("");
  });
});

// ── lastPageFromLink (via re-export test) ──────────────────────────────

describe("lastPageFromLink", () => {
  it("returns undefined for null input", () => {
    expect(lastPageFromLink(null)).toBeUndefined();
  });

  it("returns undefined for empty string", () => {
    expect(lastPageFromLink("")).toBeUndefined();
  });

  it("extracts page from Link header with last rel", () => {
    const link =
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=5>; rel="last"';
    expect(lastPageFromLink(link)).toBe(5);
  });

  it("extracts page from multi-part Link header", () => {
    const link = [
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=1>; rel="prev"',
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=3>; rel="last"',
    ].join(", ");
    expect(lastPageFromLink(link)).toBe(3);
  });

  it("returns undefined when no last rel present", () => {
    const link =
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=1>; rel="prev"';
    expect(lastPageFromLink(link)).toBeUndefined();
  });

  it("returns undefined for invalid URL format", () => {
    const link = "not-a-valid-link-header; rel=last";
    expect(lastPageFromLink(link)).toBeUndefined();
  });
});
