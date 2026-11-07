import { fetchBoundedText } from "./bounded-body";
import type { PublishedReleaseFact } from "./facts";

const REPOSITORIES = {
  terminal: "codewhale-hq/CodeWhale",
  computerUse: "codewhale-hq/codewhale-cu-plugin",
} as const;

export async function fetchLatestPublishedRelease(
  token?: string,
  channel: keyof typeof REPOSITORIES = "terminal",
): Promise<PublishedReleaseFact | null> {
  if (process.env.NEXT_PHASE === "phase-production-build") return null;
  const repo = REPOSITORIES[channel];
  try {
    const response = await fetchBoundedText(
      `https://api.github.com/repos/${repo}/releases/latest`,
      {
        headers: {
          Accept: "application/vnd.github+json",
          "User-Agent": "codewhale-web",
          "X-GitHub-Api-Version": "2022-11-28",
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
        },
        next: { revalidate: 300 },
      },
      { timeoutMs: 5000, maxBytes: 128 * 1024 },
    );
    if (!response.ok) return null;
    const release = JSON.parse(response.text) as Record<string, unknown> | null;
    if (!release || release.draft !== false || release.prerelease !== false
      || typeof release.tag_name !== "string" || !/^v\d+\.\d+\.\d+$/.test(release.tag_name)
      || typeof release.html_url !== "string"
      || typeof release.published_at !== "string"
      || !Number.isFinite(Date.parse(release.published_at))) return null;
    const url = `https://github.com/${repo}/releases/tag/${release.tag_name}`;
    if (release.html_url.toLowerCase() !== url.toLowerCase()) return null;
    return {
      tag: release.tag_name,
      version: release.tag_name.slice(1),
      publishedAt: release.published_at,
      url,
    };
  } catch {
    return null;
  }
}
