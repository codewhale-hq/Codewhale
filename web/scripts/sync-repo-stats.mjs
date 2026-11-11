#!/usr/bin/env node
import { renameSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = "codewhale-hq/CodeWhale";
const target = resolve(dirname(fileURLToPath(import.meta.url)), "..", "data", "repo-stats-snapshot.json");
const headers = { accept: "application/vnd.github+json", "user-agent": "codewhale-facts-sync" };
if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;

async function read(path) {
  const response = await fetch(`https://api.github.com${path}`, { headers, signal: AbortSignal.timeout(30_000) });
  if (!response.ok) throw new Error(`GitHub returned ${response.status} for ${path}`);
  return response.json();
}

const count = (value) => (Number.isSafeInteger(value) && value >= 0 ? value : null);

try {
  const repo = await read(`/repos/${REPO}`);
  const pulls = await read(`/search/issues?q=${encodeURIComponent(`repo:${REPO} is:pr is:open`)}&per_page=1`);
  const stars = count(repo.stargazers_count);
  const forks = count(repo.forks_count);
  const openPulls = count(pulls.total_count);
  const openTotal = count(repo.open_issues_count);
  if (!stars || forks === null || openPulls === null || openTotal === null) throw new Error("GitHub returned an unusable repository record");
  const snapshot = { stars, forks, openIssues: Math.max(0, openTotal - openPulls), openPulls, readAt: new Date().toISOString() };
  const temporary = `${target}.${process.pid}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(snapshot, null, 2)}\n`);
  renameSync(temporary, target);
  console.log(`[sync-repo-stats] wrote ${JSON.stringify(snapshot)}`);
} catch (error) {
  console.error(`[sync-repo-stats] could not read repository numbers (${error.message}); leaving the snapshot alone.`);
  process.exit(1);
}
