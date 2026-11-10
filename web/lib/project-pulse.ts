import { fetchFaces, fetchRepoStats, loadFeed, type Face } from "./github";
import { getEnv } from "./kv";
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
  for (const face of [...recent, ...(people.status === "fulfilled" ? people.value : [])]) {
    if (!face.login || !face.avatar || seen.has(face.login)) continue;
    seen.add(face.login);
    faces.push(face);
  }

  const live = stats.status === "fulfilled" && stats.value.stars > 0 ? stats.value : null;
  return { stats: live, merged, faces: faces.slice(0, faceLimit) };
}
