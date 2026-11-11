import { OUTBOUND_TIMEOUT_MS } from "./bounded-body";
import snapshot from "../data/repo-stats-snapshot.json";
import type { RepoStats } from "./types";

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
  comments: number;
  labels: { name: string }[];
  pull_request?: unknown;
}

export interface Face {
  login: string;
  avatar: string;
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
