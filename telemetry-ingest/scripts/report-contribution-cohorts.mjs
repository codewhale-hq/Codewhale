#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const run = promisify(execFile);
const REPOSITORY = "codewhale-hq/CodeWhale";
const MAX_PAGES = 100;
const MAX_ITEMS = MAX_PAGES * 100;

function date(value, field) {
  if (typeof value !== "string" || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/.test(value) || !Number.isFinite(Date.parse(value))) throw new Error(`invalid ${field}`);
  return new Date(value);
}
function isPerson(user) {
  return user && user.type === "User" && Number.isSafeInteger(user.id) && user.id > 0;
}

export function contributionCohorts(snapshot, { from = "1970-01-01T00:00:00Z", to = new Date().toISOString().slice(0, 19) + "Z" } = {}) {
  if (!snapshot || snapshot.format !== "codewhale-public-contributions-v1" || snapshot.repository?.toLowerCase() !== REPOSITORY.toLowerCase() || !["github-public-api", "synthetic"].includes(snapshot.source)) throw new Error("unsupported public contribution receipt");
  if (snapshot.coverage?.issues_complete !== true || snapshot.coverage?.pulls_complete !== true) throw new Error("incomplete GitHub pagination; earliest contribution unavailable");
  if (!Array.isArray(snapshot.issues) || !Array.isArray(snapshot.pulls) || snapshot.issues.length > MAX_ITEMS || snapshot.pulls.length > MAX_ITEMS) throw new Error("missing or oversized GitHub history");
  const start = date(from, "from"); const end = date(to, "to");
  if (start >= end) throw new Error("from must precede to");
  const issueAuthors = new Map(); const pullAuthors = new Map();
  let botOrDeleted = 0; let issueCount = 0; let mergedCount = 0;
  const issueNumbers = new Set(); const pullNumbers = new Set();
  for (const issue of snapshot.issues) {
    if (!Number.isSafeInteger(issue.number) || issue.number < 1) throw new Error("invalid issue number");
    if (issueNumbers.has(issue.number)) continue;
    issueNumbers.add(issue.number);
    if (issue.pull_request) continue;
    if (!isPerson(issue.user)) { botOrDeleted++; continue; }
    const at = date(issue.created_at, "issue created_at");
    const id = issue.user.id; issueCount++;
    if (!issueAuthors.has(id) || at < issueAuthors.get(id)) issueAuthors.set(id, at);
  }
  for (const pull of snapshot.pulls) {
    if (!Number.isSafeInteger(pull.number) || pull.number < 1) throw new Error("invalid pull number");
    if (pullNumbers.has(pull.number)) continue;
    pullNumbers.add(pull.number);
    if (!pull.merged_at) continue;
    if (!isPerson(pull.user)) { botOrDeleted++; continue; }
    const at = date(pull.merged_at, "pull merged_at");
    const id = pull.user.id; mergedCount++;
    if (!pullAuthors.has(id) || at < pullAuthors.get(id)) pullAuthors.set(id, at);
  }
  const days = new Map();
  const add = (at, key) => {
    if (at < start || at >= end) return;
    const day = at.toISOString().slice(0, 10);
    const row = days.get(day) ?? { day, first_public_issue_authors: 0, first_merged_pr_authors: 0 };
    row[key]++; days.set(day, row);
  };
  for (const at of issueAuthors.values()) add(at, "first_public_issue_authors");
  for (const at of pullAuthors.values()) add(at, "first_merged_pr_authors");
  const daily = [...days.values()].sort((a, b) => a.day.localeCompare(b.day));
  return {
    report: "codewhale_public_contribution_cohorts", repository: REPOSITORY, source: snapshot.source, synthetic: snapshot.source === "synthetic",
    coverage: { from, to, issues_complete: true, pulls_complete: true, accessible_public_history_only: true },
    input_counts: { public_issues: issueCount, merged_prs: mergedCount, excluded_bot_or_deleted_records: botOrDeleted },
    totals: { first_public_issue_authors: daily.reduce((sum, row) => sum + row.first_public_issue_authors, 0), first_merged_pr_authors: daily.reduce((sum, row) => sum + row.first_merged_pr_authors, 0) },
    days: daily,
    caveats: [
      "First means earliest in the complete accessible public repository export, not first open-source contribution anywhere. Deleted or inaccessible history cannot be recovered.",
      "First public issue authors and first merged PR authors are separate cohorts; this report does not attribute them to website visits, install IDs or runtime tasks.",
      "Stable public author IDs are used only in memory to aggregate the export. Output contains no author identities, content, account profile or attribution join.",
      "Paginated API reads are not an atomic snapshot. The export is bounded and fails rather than declaring incomplete history complete.",
    ],
  };
}

export async function exportPublicHistory({ get = githubGet, maxPages = MAX_PAGES } = {}) {
  if (!Number.isInteger(maxPages) || maxPages < 1 || maxPages > MAX_PAGES) throw new Error("maxPages must be between 1 and 100");
  const repo = await get(`repos/${REPOSITORY}`);
  if (repo.private !== false) throw new Error("repository must be verified public before exporting history");
  const paginate = async (endpoint) => {
    const records = [];
    for (let page = 1; page <= maxPages; page++) {
      const batch = await get(`${endpoint}&per_page=100&page=${page}`);
      if (!Array.isArray(batch) || batch.length > 100) throw new Error("malformed GitHub page");
      records.push(...batch);
      if (batch.length < 100) return records;
    }
    throw new Error("incomplete GitHub pagination at bounded page limit; no cohort report generated");
  };
  const issues = await paginate(`repos/${REPOSITORY}/issues?state=all&sort=created&direction=asc`);
  const pulls = await paginate(`repos/${REPOSITORY}/pulls?state=all&sort=created&direction=asc`);
  return { format: "codewhale-public-contributions-v1", repository: REPOSITORY, source: "github-public-api", coverage: { issues_complete: true, pulls_complete: true }, issues, pulls };
}

async function githubGet(endpoint) {
  try {
    const { stdout } = await run("gh", ["api", endpoint], { maxBuffer: 8 * 1024 * 1024, timeout: 30_000 });
    return JSON.parse(stdout);
  } catch {
    throw new Error("GitHub API read failed; no credentials or response content were printed");
  }
}

export async function main(argv = process.argv.slice(2)) {
  let input; let from; let to;
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i]; const value = argv[++i];
    if (!value || value.startsWith("--")) throw new Error("missing contribution report option");
    if (flag === "--input") input = value;
    else if (flag === "--from") from = value;
    else if (flag === "--to") to = value;
    else throw new Error("unknown contribution report option");
  }
  const snapshot = input ? JSON.parse(readFileSync(input, "utf8")) : await exportPublicHistory();
  return contributionCohorts(snapshot, { from, to });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then((report) => process.stdout.write(`${JSON.stringify(report, null, 2)}\n`)).catch((error) => { process.stderr.write(`report:contributions: ${error instanceof SyntaxError ? "invalid JSON receipt" : error.message}\n`); process.exitCode = 1; });
}
