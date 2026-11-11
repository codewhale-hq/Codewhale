import { fetchFaces, fetchRepoStats, loadFeed, readRepoStatsSnapshot, type Face } from "./github";
import { getEnv } from "./kv";
import { RELEASE_CONTRIBUTORS, RELEASE_HELPERS, UNRELEASED_CONTRIBUTORS } from "./release-credits";
import type { FeedItem, RepoStats } from "./types";

export interface ProjectPulse {
  stats: RepoStats | null;
  merged: FeedItem[];
  faces: Face[];
}

export async function loadProjectPulse(mergedLimit = 6, faceLimit = 60): Promise<ProjectPulse> {
  const env = await getEnv();
  const token = env.GITHUB_TOKEN;
  const [stats, feed, people] = await Promise.allSettled([
    fetchRepoStats(token),
    loadFeed(token, 50),
    fetchFaces(token, 100),
  ]);

  const items = feed.status === "fulfilled" ? feed.value.items : [];
  const merged = items
    .filter((item) => item.kind === "pull" && item.state === "merged")
    .sort((a, b) => +new Date(b.eventAt ?? b.updatedAt) - +new Date(a.eventAt ?? a.updatedAt))
    .slice(0, mergedLimit);

  const seen = new Set<string>();
  const faces: Face[] = [];
  const recent = merged.map((item) => ({ login: item.author, avatar: item.authorAvatar }));
  const credited = [...UNRELEASED_CONTRIBUTORS, ...RELEASE_CONTRIBUTORS, ...RELEASE_HELPERS]
    .map((handle) => handle.replace(/^@/, ""))
    .map((login) => ({ login, avatar: `https://avatars.githubusercontent.com/${login}` }));
  for (const face of [...recent, ...(people.status === "fulfilled" ? people.value : []), ...credited]) {
    if (!face.login || !face.avatar || seen.has(face.login)) continue;
    seen.add(face.login);
    faces.push(face);
  }

  const live = stats.status === "fulfilled" && stats.value.stars !== null ? stats.value : null;
  const known = live ?? readRepoStatsSnapshot();
  return { stats: known.stars === null ? null : known, merged, faces: faces.slice(0, faceLimit) };
}
