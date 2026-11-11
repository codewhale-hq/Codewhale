import { fetchRepoStats, readRepoStatsSnapshot, type Face } from "./github";
import { getEnv } from "./kv";
import { RELEASE_CONTRIBUTORS, RELEASE_HELPERS, UNRELEASED_CONTRIBUTORS } from "./release-credits";
import { isExcluded, parseExclusions, repoActivityView, type RepoActivityView } from "./repo-activity";
import type { FeedItem, RepoStats } from "./types";

export interface ProjectPulse {
  stats: RepoStats | null;
  merged: FeedItem[];
  faces: Face[];
  activity: RepoActivityView | null;
}

export async function loadProjectPulse(mergedLimit = 6, faceLimit = 60): Promise<ProjectPulse> {
  const env = await getEnv();
  const [stats] = await Promise.allSettled([fetchRepoStats(env.GITHUB_TOKEN)]);

  const exclusions = parseExclusions();
  const activity = repoActivityView();
  const merged: FeedItem[] = (activity?.pulls ?? []).slice(0, mergedLimit).map((pull) => ({
    kind: "pull",
    number: pull.number,
    title: pull.title,
    url: pull.url,
    state: "merged",
    author: pull.login,
    authorAvatar: pull.avatar,
    createdAt: pull.mergedAt,
    updatedAt: pull.mergedAt,
    eventAt: pull.mergedAt,
    comments: 0,
    labels: [],
    firstTimeContributor: pull.firstTime,
  }));

  const seen = new Set<string>();
  const faces: Face[] = [];
  const recent = (activity?.people ?? []).map((person) => ({ login: person.login, avatar: person.avatar }));
  const credited = [...UNRELEASED_CONTRIBUTORS, ...RELEASE_CONTRIBUTORS, ...RELEASE_HELPERS]
    .map((handle) => handle.replace(/^@/, ""))
    .map((login) => ({ login, avatar: `https://avatars.githubusercontent.com/${login}` }));
  for (const face of [...recent, ...credited]) {
    if (!face.login || !face.avatar || seen.has(face.login) || isExcluded(face.login, exclusions)) continue;
    seen.add(face.login);
    faces.push(face);
  }

  const live = stats.status === "fulfilled" && stats.value.stars !== null ? stats.value : null;
  const known = live ?? readRepoStatsSnapshot();
  return { stats: known.stars === null ? null : known, merged, faces: faces.slice(0, faceLimit), activity };
}
