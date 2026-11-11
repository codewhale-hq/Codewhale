#!/usr/bin/env node
import { renameSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  ACTIVITY_WINDOW_DAYS,
  SEARCH_RESULT_CAP,
  creditedPairs,
  mergedPullFromSearchItem,
  readCappedJson,
  releaseCredits,
  versionedMilestones,
  windowStart,
} from "./repo-activity-lib.mjs";

const REPO = "codewhale-hq/CodeWhale";
const target = resolve(dirname(fileURLToPath(import.meta.url)), "..", "data", "repo-stats-snapshot.json");
const headers = { accept: "application/vnd.github+json", "user-agent": "codewhale-facts-sync" };
if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
const searchGapMs = process.env.GITHUB_TOKEN ? 2_200 : 7_000;

const read = (path) => readCappedJson(`https://api.github.com${path}`, { headers });
const count = (value) => (Number.isSafeInteger(value) && value >= 0 ? value : null);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

let lastSearch = 0;
async function searchMerged(range) {
  const query = encodeURIComponent(`repo:${REPO} is:pr is:merged merged:${range}`);
  const pulls = [];
  let seen = 0;
  for (let page = 1; ; page += 1) {
    const wait = lastSearch + searchGapMs - Date.now();
    if (wait > 0) await sleep(wait);
    lastSearch = Date.now();
    const body = await read(`/search/issues?q=${query}&per_page=100&page=${page}`);
    const total = count(body.total_count);
    if (body.incomplete_results || total === null || total > SEARCH_RESULT_CAP) {
      throw new Error(`GitHub search for merged:${range} is not complete`);
    }
    const items = Array.isArray(body.items) ? body.items : [];
    for (const item of items) {
      const pull = mergedPullFromSearchItem(item);
      if (pull) pulls.push(pull);
    }
    seen += items.length;
    if (seen >= total) return pulls;
    if (items.length === 0) throw new Error(`GitHub search for merged:${range} ended early`);
  }
}

try {
  const readAt = new Date().toISOString();
  const repo = await read(`/repos/${REPO}`);
  const openPullSearch = await read(`/search/issues?q=${encodeURIComponent(`repo:${REPO} is:pr is:open`)}&per_page=1`);
  const stars = count(repo.stargazers_count);
  const forks = count(repo.forks_count);
  const openPulls = count(openPullSearch.total_count);
  const openTotal = count(repo.open_issues_count);
  if (!stars || forks === null || openPulls === null || openTotal === null) throw new Error("GitHub returned an unusable repository record");

  const merged = (await searchMerged(`>=${windowStart(readAt)}`)).sort((a, b) => Date.parse(b.mergedAt) - Date.parse(a.mergedAt));

  const milestoneRows = await read(`/repos/${REPO}/milestones?state=open&per_page=100`);
  if (!Array.isArray(milestoneRows)) throw new Error("GitHub returned an unusable milestone list");
  const milestones = versionedMilestones(milestoneRows);

  const releaseRows = await read(`/repos/${REPO}/releases?per_page=12`);
  if (!Array.isArray(releaseRows)) throw new Error("GitHub returned an unusable release list");
  const pairs = creditedPairs(releaseRows);
  const pullsByTag = new Map();
  for (const { release, previous } of pairs) {
    pullsByTag.set(release.tag_name, await searchMerged(`${previous.published_at}..${release.published_at}`));
  }
  const releases = releaseCredits(pairs, (release) => pullsByTag.get(release.tag_name) ?? []);

  const snapshot = {
    stars,
    forks,
    openIssues: Math.max(0, openTotal - openPulls),
    openPulls,
    readAt,
    activity: { windowDays: ACTIVITY_WINDOW_DAYS, merged, milestones, releases },
  };
  const temporary = `${target}.${process.pid}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(snapshot, null, 2)}\n`);
  renameSync(temporary, target);
  console.log(`[sync-repo-stats] wrote ${stars} stars, ${merged.length} merged pull requests, ${releases.length} release credit lists, next milestone ${milestones[0]?.title ?? "none"}, read ${readAt}`);
} catch (error) {
  console.error(`[sync-repo-stats] could not read repository numbers (${error.message}); leaving the snapshot alone.`);
  process.exit(1);
}
