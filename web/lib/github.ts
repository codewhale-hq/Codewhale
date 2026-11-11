import { OUTBOUND_TIMEOUT_MS } from "./bounded-body";
import snapshot from "../data/repo-stats-snapshot.json";
import type { FeedItem, RepoStats } from "./types";

const SNAPSHOT: unknown = snapshot;

const REPO = process.env.GITHUB_REPO ?? "codewhale-hq/CodeWhale";
const GH = "https://api.github.com";

function isProductionBuild(): boolean {
  return process.env.NEXT_PHASE === "phase-production-build";
}

function headers(token?: string): HeadersInit {
  const h: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
    "User-Agent": "codewhale-web",
  };
  if (token) h.Authorization = `Bearer ${token}`;
  return h;
}

const UNKNOWN_STATS: RepoStats = {
  stars: null,
  forks: null,
  openIssues: null,
  openPulls: null,
  contributors: null,
  fetchedAt: null,
  source: "unknown",
};

function count(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

export function readRepoStatsSnapshot(raw: unknown = SNAPSHOT): RepoStats {
  if (!raw || typeof raw !== "object") return UNKNOWN_STATS;
  const row = raw as Record<string, unknown>;
  if (typeof row.readAt !== "string" || !Number.isFinite(Date.parse(row.readAt))) return UNKNOWN_STATS;
  const stars = count(row.stars);
  if (stars === null || stars === 0) return UNKNOWN_STATS;
  return {
    stars,
    forks: count(row.forks),
    openIssues: count(row.openIssues),
    openPulls: count(row.openPulls),
    contributors: null,
    fetchedAt: row.readAt,
    source: "snapshot",
  };
}

export async function fetchRepoStats(token?: string): Promise<RepoStats> {
  if (isProductionBuild()) return readRepoStatsSnapshot();

  try {
    const [repoRes, contribRes, releaseRes] = await Promise.all([
      fetch(`${GH}/repos/${REPO}`, { headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS), next: { revalidate: 1800 } }),
      fetch(`${GH}/repos/${REPO}/contributors?per_page=1&anon=true`, {
        headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS),
        next: { revalidate: 3600 },
      }),
      fetch(`${GH}/repos/${REPO}/releases/latest`, { headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS), next: { revalidate: 3600 } }),
    ]);

    const repo = repoRes.ok ? await repoRes.json().catch(() => null) : null;
    const stars = numberField(repo, "stargazers_count");
    const forks = numberField(repo, "forks_count");
    const repoOpenCount = numberField(repo, "open_issues_count");
    const contributors = contribRes.ok ? (lastPageFromLink(contribRes.headers.get("link")) ?? null) : null;

    const prRes = await fetch(
      `${GH}/search/issues?q=${encodeURIComponent(`repo:${REPO} is:pr is:open`)}&per_page=1`,
      { headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS), next: { revalidate: 1800 } }
    );
    const prJson = prRes.ok ? ((await prRes.json().catch(() => null)) as { total_count?: unknown } | null) : null;
    const openPulls = count(prJson?.total_count);
    const openIssues = repoOpenCount !== null && openPulls !== null ? Math.max(0, repoOpenCount - openPulls) : null;

    let latestRelease: RepoStats["latestRelease"];
    if (releaseRes.ok) {
      const r = (await releaseRes.json().catch(() => null)) as { tag_name?: string; published_at?: string; html_url?: string } | null;
      if (r && typeof r.tag_name === "string" && typeof r.published_at === "string" && typeof r.html_url === "string") {
        latestRelease = { tag: r.tag_name, publishedAt: r.published_at, url: r.html_url };
      }
    }

    if (stars === null) return UNKNOWN_STATS;
    return {
      stars: stars === 0 ? null : stars,
      forks,
      openIssues,
      openPulls,
      contributors,
      latestRelease,
      fetchedAt: new Date().toISOString(),
      source: "live",
    };
  } catch {
    return UNKNOWN_STATS;
  }
}

function numberField(body: unknown, key: string): number | null {
  if (!body || typeof body !== "object") return null;
  return count((body as Record<string, unknown>)[key]);
}

export function lastPageFromLink(link: string | null): number | undefined {
  if (!link) return undefined;

  for (const part of link.split(",")) {
    const [rawUrl, rawRel] = part.split(";").map((segment) => segment.trim());
    if (rawRel !== 'rel="last"') continue;

    const match = rawUrl.match(/^<(.+)>$/);
    if (!match) continue;

    const page = new URL(match[1]).searchParams.get("page");
    const parsed = page ? Number.parseInt(page, 10) : NaN;
    if (Number.isFinite(parsed) && parsed > 0) return parsed;
  }

  return undefined;
}

interface RawIssue {
  number: number;
  title: string;
  html_url: string;
  state: "open" | "closed";
  user: { login: string; avatar_url: string };
  created_at: string;
  updated_at: string;
  closed_at?: string | null;
  comments: number;
  labels: { name: string; color: string }[];
  pull_request?: unknown;
  draft?: boolean;
  body?: string | null;
  /**
   * GitHub's relationship verdict for the author, present on both list
   * endpoints. "FIRST_TIME_CONTRIBUTOR" is the only value we read.
   */
  author_association?: string;
}

interface RawRelease {
  tag_name: string;
  name?: string | null;
  html_url: string;
  created_at: string;
  published_at?: string | null;
  draft?: boolean;
  prerelease?: boolean;
  author?: { login: string; avatar_url: string } | null;
}

/** How many releases to pull. The tail is noise; the ticker sorts by date. */
const RELEASE_WINDOW = 5;

/** How recent a release must be to keep a reserved slot in a busy feed. */
const RELEASE_PIN_WINDOW_MS = 60 * 24 * 60 * 60 * 1000;

function firstTimer(association?: string): boolean {
  return association === "FIRST_TIME_CONTRIBUTOR";
}

/**
 * GitHub marks app accounts with a `[bot]` suffix on the login — its own
 * verdict, not our inference. The wire exists to put the people behind the
 * repository on the front page; dependency bumps and automated closes spend
 * slots that belong to them, so bot-authored issues and pulls stay off. A
 * published release is news no matter who pushed the button, so it keeps its
 * slot — with a bot publisher's byline dropped instead
 * (`author === ""` renders no by-line in components/ticker.tsx).
 */
function isBot(login: string): boolean {
  return login.endsWith("[bot]");
}

/**
 * The repository's recent life: issues, pull requests, and releases.
 *
 * Three cached GitHub calls, no per-item follow-ups. Merge state, the
 * author's handle, and GitHub's first-time-contributor verdict all arrive in
 * the list payloads we already fetch, so naming a newcomer on the homepage
 * costs nothing extra. Releases change rarely and cache for an hour;
 * unauthenticated that is ~13 requests/hour against GitHub's 60/hour/IP.
 */
export async function fetchFeed(token?: string, limit = 30): Promise<FeedItem[]> {
  return (await loadFeed(token, limit)).items;
}

/**
 * Why the feed is what it is. `fetchFeed` flattens this to a list, which is
 * right for optional chrome (the homepage ticker) but wrong for a page whose
 * whole body is the feed: there, "GitHub had nothing" and "GitHub was not
 * asked" or "GitHub refused" must render differently, or a rate limit and a
 * build-time prerender both masquerade as an honest empty record.
 *
 *  - `ok`          — that list endpoint answered; an empty list is real.
 *  - `skipped`     — static generation; nothing was fetched.
 *  - `unavailable` — that call came back non-ok (rate limit, outage);
 *                    `items` holds whatever did arrive.
 *
 * Availability is tracked per list: when exactly one endpoint refuses, the
 * other column must not be told that "the source did not answer".
 */
export type FeedLoadStatus = "ok" | "skipped" | "unavailable";

export interface FeedLoad {
  items: FeedItem[];
  issuesStatus: FeedLoadStatus;
  pullsStatus: FeedLoadStatus;
}

export async function loadFeed(token?: string, limit = 30): Promise<FeedLoad> {
  if (isProductionBuild())
    return { items: [], issuesStatus: "skipped", pullsStatus: "skipped" };

  const [issuesRes, pullsRes, releasesRes] = await Promise.all([
    fetch(
      `${GH}/repos/${REPO}/issues?state=all&per_page=${limit}&sort=updated&direction=desc`,
      { headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS), next: { revalidate: 600 } }
    ),
    fetch(
      `${GH}/repos/${REPO}/pulls?state=all&per_page=${limit}&sort=updated&direction=desc`,
      { headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS), next: { revalidate: 600 } }
    ),
    fetch(`${GH}/repos/${REPO}/releases?per_page=${RELEASE_WINDOW}`, {
      headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS),
      next: { revalidate: 3600 },
    }),
  ]);

  // Releases are a garnish on the feed; the two list calls are the record,
  // and each answers for itself.
  const issuesStatus: FeedLoadStatus = issuesRes.ok ? "ok" : "unavailable";
  const pullsStatus: FeedLoadStatus = pullsRes.ok ? "ok" : "unavailable";
  const issues = await responseArray<RawIssue>(issuesRes);
  const pulls = await responseArray<RawIssue & { merged_at?: string | null }>(pullsRes);
  const releases = await responseArray<RawRelease>(releasesRes);

  const items: FeedItem[] = [];

  for (const it of issues) {
    if (it.pull_request) continue; // GH issues endpoint returns PRs too
    if (isBot(it.user.login)) continue; // automated maintenance, not contributor life
    items.push({
      kind: "issue",
      number: it.number,
      title: it.title,
      url: it.html_url,
      state: it.state,
      author: it.user.login,
      authorAvatar: it.user.avatar_url,
      createdAt: it.created_at,
      updatedAt: it.updated_at,
      eventAt: (it.state === "closed" ? it.closed_at : it.created_at) ?? it.created_at,
      comments: it.comments,
      labels: it.labels?.map((l) => ({ name: l.name, color: l.color })) ?? [],
      body: it.body ?? undefined,
      firstTimeContributor: firstTimer(it.author_association),
    });
  }

  for (const pr of pulls) {
    if (isBot(pr.user.login)) continue; // automated maintenance, not contributor life
    let state: FeedItem["state"] = pr.state;
    let eventAt = pr.created_at;
    if (pr.merged_at) {
      state = "merged";
      eventAt = pr.merged_at;
    } else if (pr.draft) {
      state = "draft";
    } else if (pr.state === "closed") {
      eventAt = pr.closed_at ?? pr.updated_at;
    }
    items.push({
      kind: "pull",
      number: pr.number,
      title: pr.title,
      url: pr.html_url,
      state,
      author: pr.user.login,
      authorAvatar: pr.user.avatar_url,
      createdAt: pr.created_at,
      updatedAt: pr.updated_at,
      eventAt,
      comments: pr.comments,
      labels: pr.labels?.map((l) => ({ name: l.name, color: l.color })) ?? [],
      body: pr.body ?? undefined,
      firstTimeContributor: firstTimer(pr.author_association),
    });
  }

  for (const rel of releases) {
    if (rel.draft) continue; // an unpublished draft is not news
    const publishedAt = rel.published_at ?? rel.created_at;
    // A bot-published release keeps its slot but not its byline.
    const publisher =
      rel.author && !isBot(rel.author.login) ? rel.author.login : "";
    items.push({
      kind: "release",
      number: 0,
      tag: rel.tag_name,
      title: rel.name?.trim() || rel.tag_name,
      url: rel.html_url,
      state: "published",
      author: publisher,
      authorAvatar: publisher ? rel.author?.avatar_url ?? "" : "",
      createdAt: rel.created_at,
      updatedAt: publishedAt,
      eventAt: publishedAt,
      comments: 0,
      labels: [],
    });
  }

  const ordered = items.sort((a, b) => +new Date(b.updatedAt) - +new Date(a.updatedAt));
  const kept = ordered.slice(0, limit);

  // A release is the one event a busy week can bury: twenty issue comments
  // will push last week's tag out of a pure recency window. Keep the newest
  // published release in view — but only a recent one, and always carrying its
  // real date, so a quiet quarter reads as a quiet quarter instead of pinning
  // a two-year-old tag beside today's merges.
  const newestRelease = ordered.find((i) => i.kind === "release");
  const pinnable =
    newestRelease &&
    Date.now() - +new Date(newestRelease.eventAt ?? newestRelease.updatedAt) <
      RELEASE_PIN_WINDOW_MS;
  if (pinnable && kept.length === limit && !kept.some((i) => i.kind === "release")) {
    kept[kept.length - 1] = newestRelease;
  }

  return { items: kept, issuesStatus, pullsStatus };
}

export interface Face {
  login: string;
  avatar: string;
}

export async function fetchFaces(token?: string, limit = 100): Promise<Face[]> {
  if (isProductionBuild()) return [];
  const res = await fetch(`${GH}/repos/${REPO}/contributors?per_page=${limit}`, {
    headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS),
    next: { revalidate: 3600 },
  });
  const people = await responseArray<{ login?: string; avatar_url?: string; type?: string }>(res);
  return people
    .filter((p) => p.login && p.avatar_url && p.type !== "Bot" && !isBot(p.login))
    .map((p) => ({ login: p.login as string, avatar: p.avatar_url as string }));
}

export interface MilestoneProgress {
  number: number;
  open: number;
  closed: number;
}

export async function fetchMilestones(token?: string): Promise<MilestoneProgress[]> {
  if (isProductionBuild()) return [];
  const res = await fetch(`${GH}/repos/${REPO}/milestones?state=open&per_page=30`, {
    headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS),
    next: { revalidate: 1800 },
  });
  const rows = await responseArray<{ number?: number; open_issues?: number; closed_issues?: number }>(res);
  return rows
    .filter((m) => typeof m.number === "number")
    .map((m) => ({ number: m.number as number, open: m.open_issues ?? 0, closed: m.closed_issues ?? 0 }));
}

export interface OpenIssue {
  number: number;
  title: string;
  url: string;
  comments: number;
  labels: string[];
}

export async function fetchLabelled(label: string, token?: string, limit = 6): Promise<OpenIssue[]> {
  if (isProductionBuild()) return [];
  const res = await fetch(
    `${GH}/repos/${REPO}/issues?state=open&labels=${encodeURIComponent(label)}&per_page=${limit}&sort=updated&direction=desc`,
    { headers: headers(token), signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS), next: { revalidate: 1800 } },
  );
  const rows = await responseArray<RawIssue>(res);
  return rows
    .filter((i) => !i.pull_request)
    .map((i) => ({
      number: i.number,
      title: i.title,
      url: i.html_url,
      comments: i.comments,
      labels: (i.labels ?? []).map((l) => l.name).filter((name) => name !== label),
    }));
}

async function responseArray<T>(res: Response): Promise<T[]> {
  if (!res.ok) return [];
  const body = await res.json().catch(() => null);
  return Array.isArray(body) ? (body as T[]) : [];
}

/** Compact star-count label, e.g. 39312 → "39.3k". */
export function formatStars(n: number): string {
  if (n >= 1000) {
    return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
  }
  return String(n);
}

/**
 * An age expressed the way `Intl.RelativeTimeFormat` wants it: a negative
 * count and a unit. Past ages are negative; anything under a minute (and any
 * unparseable or future date) is `0 seconds`, which `numeric: "auto"` renders
 * as the locale's own "now".
 *
 * This exists so a surface can print an age in the reader's language without
 * a hand-translated abbreviation table per locale — CLDR already has one, and
 * the masthead already formats its date the same way off `chrome.dateLocale`.
 */
export interface RelativeAge {
  value: number;
  unit: "second" | "minute" | "hour" | "day" | "month" | "year";
}

export function relativeAge(iso: string): RelativeAge {
  const then = +new Date(iso);
  if (!Number.isFinite(then)) return { value: 0, unit: "second" };

  const mins = Math.round((Date.now() - then) / 60000);
  if (mins < 1) return { value: 0, unit: "second" };
  if (mins < 60) return { value: -mins, unit: "minute" };
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return { value: -hrs, unit: "hour" };
  const days = Math.round(hrs / 24);
  if (days < 30) return { value: -days, unit: "day" };
  const months = Math.round(days / 30);
  if (months < 12) return { value: -months, unit: "month" };
  return { value: -Math.round(months / 12), unit: "year" };
}

const AGE_SUFFIX: Record<RelativeAge["unit"], string> = {
  second: "",
  minute: "m",
  hour: "h",
  day: "d",
  month: "mo",
  year: "y",
};

/** Compact English age, e.g. "5m", "3h", "2y". Same thresholds as `relativeAge`. */
export function relativeTime(iso: string): string {
  const age = relativeAge(iso);
  if (age.unit === "second") return "just now";
  return `${Math.abs(age.value)}${AGE_SUFFIX[age.unit]}`;
}
