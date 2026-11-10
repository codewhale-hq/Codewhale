/**
 * roadmap-feed.ts — fetch the live roadmap from GitHub.
 *
 *   "Shipped"    ← last 8 published Releases on codewhale-hq/CodeWhale
 *   "Underway"   ← open issues with label `roadmap:underway`
 *   "Considered" ← open issues with label `roadmap:considered`
 *   "Ruled out"  ← issues (open or closed) with label `roadmap:ruled-out`
 *
 * Cached in CURATED_KV under `roadmap:feed` with a 30-minute TTL so the
 * roadmap page renders fast and the GH rate limit never matters.
 *
 * Categories that come back empty fall through to the page's static items —
 * the maintainer can adopt label-driven roadmap incrementally.
 */
import { OUTBOUND_TIMEOUT_MS } from "./bounded-body";
import { truncateChars } from "./truncate";

const REPO = process.env.GITHUB_REPO ?? "codewhale-hq/CodeWhale";
const KV_KEY = "roadmap:feed";
const KV_TTL = 60 * 30;

export interface RoadmapItem {
  title: string;
  note: string;
  href?: string;
  number?: number;
}

export interface RoadmapFeed {
  generatedAt: string;
  shipped: RoadmapItem[];
  underway: RoadmapItem[];
  considered: RoadmapItem[];
  ruledOut: RoadmapItem[];
}

interface KVNamespace {
  get(k: string): Promise<string | null>;
  put(k: string, v: string, o?: { expirationTtl?: number }): Promise<void>;
}

async function gh<T>(url: string, ghToken?: string): Promise<T | null> {
  if (process.env.NEXT_PHASE === "phase-production-build") return null;

  const headers: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "User-Agent": "codewhale-web-roadmap",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  if (ghToken) headers["Authorization"] = `Bearer ${ghToken}`;
  try {
    const r = await fetch(url, { headers, signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS) });
    if (!r.ok) return null;
    return (await r.json()) as T;
  } catch {
    return null;
  }
}

interface GhRelease { tag_name: string; name: string | null; body: string | null; html_url: string; prerelease: boolean; draft: boolean }
interface GhIssue { number: number; title: string; html_url: string; body: string | null; state: string; pull_request?: unknown }

function summarizeReleaseBody(body: string | null): string {
  if (!body) return "";
  const lines = body.split(/\r?\n/).map((l) => l.trim());
  // Every release body opens with the same blockquote boilerplate and install
  // steps, so the document's first prose line is not release-specific. Prefer
  // the release's own "What's in vX.Y.Z" section when the template provides
  // one, and never summarize a code fence, heading, blockquote, or separator.
  const whatsIn = lines.findIndex((l) => /^#+\s+what's\s+in\b/i.test(l));
  const scoped = whatsIn >= 0 ? lines.slice(whatsIn + 1) : lines;
  let fenced = false;
  let candidate = "";
  for (const line of scoped) {
    if (line.startsWith("```")) {
      fenced = !fenced;
      continue;
    }
    if (fenced || line.length <= 8) continue;
    if (line.startsWith("#") || line.startsWith("---") || line.startsWith(">")) continue;
    candidate = line;
    break;
  }
  if (!candidate) return "";
  // Strip bullets and links, then cap length without splitting a character
  const stripped = candidate.replace(/^[*\-•]\s+/, "").replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").trim();
  return truncateChars(stripped, 140, 137);
}

function summarizeIssueBody(body: string | null): string {
  if (!body) return "";
  // Issue bodies are often very long; take the first non-empty paragraph (up to ~140 chars)
  const para = body.split(/\r?\n\r?\n/).map((p) => p.trim()).find((p) => p.length > 0) ?? "";
  const stripped = para
    .replace(/^[#>*\-\s]+/, "")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
  return truncateChars(stripped, 140, 137);
}

async function fetchByLabel(label: string, ghToken?: string, state: "open" | "closed" | "all" = "open"): Promise<RoadmapItem[]> {
  const url = `https://api.github.com/repos/${REPO}/issues?state=${state}&labels=${encodeURIComponent(label)}&per_page=10&sort=updated`;
  const issues = await gh<GhIssue[]>(url, ghToken);
  if (!issues) return [];
  return issues
    .filter((i) => !i.pull_request) // skip PRs
    .map((i) => ({
      title: i.title,
      note: summarizeIssueBody(i.body) || `Issue #${i.number}`,
      href: i.html_url,
      number: i.number,
    }));
}

export async function fetchRoadmap(ghToken?: string): Promise<RoadmapFeed> {
  const [releases, underway, considered, ruledOut] = await Promise.all([
    gh<GhRelease[]>(`https://api.github.com/repos/${REPO}/releases?per_page=8`, ghToken),
    fetchByLabel("roadmap:underway", ghToken, "open"),
    fetchByLabel("roadmap:considered", ghToken, "open"),
    fetchByLabel("roadmap:ruled-out", ghToken, "all"),
  ]);

  const shipped: RoadmapItem[] = releases
    ? releases
    .filter((r) => !r.draft)
    .map((r) => ({
      title: r.name?.trim() || r.tag_name,
      note: summarizeReleaseBody(r.body) || r.tag_name,
      href: r.html_url,
    }))
    : [];

  return {
    generatedAt: new Date().toISOString(),
    shipped,
    underway,
    considered,
    ruledOut,
  };
}

export async function getCachedRoadmap(kv: KVNamespace | undefined, ghToken: string | undefined): Promise<RoadmapFeed | null> {
  try {
    if (kv) {
      const cached = await kv.get(KV_KEY);
      if (cached) {
        const parsed = JSON.parse(cached) as RoadmapFeed;
        return { ...parsed, shipped: parsed.shipped ?? [] };
      }
    }
    const fresh = await fetchRoadmap(ghToken);
    const hasLive = fresh.shipped.length + fresh.underway.length + fresh.considered.length + fresh.ruledOut.length > 0;
    if (kv && hasLive) {
      await kv.put(KV_KEY, JSON.stringify(fresh), { expirationTtl: KV_TTL });
    }
    return fresh;
  } catch {
    return null;
  }
}
