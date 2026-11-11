import snapshotJson from "../data/repo-stats-snapshot.json";
import exclusionsJson from "../data/contributor-exclusions.json";

const SNAPSHOT: unknown = snapshotJson;
const EXCLUSIONS_RAW: unknown = exclusionsJson;

const DAY_MS = 24 * 60 * 60 * 1000;

export const LIVE_RECORD_MAX_AGE_MS = 7 * DAY_MS;
export const LIVE_RECORD_SOURCE = "github-public-snapshot";

export interface ActivityPull {
  number: number;
  title: string;
  url: string;
  login: string;
  avatar: string;
  mergedAt: string;
  firstTime: boolean;
}

export interface ActivityPerson {
  login: string;
  avatar: string;
}

export interface ActivityMilestone {
  title: string;
  url: string;
  open: number;
  closed: number;
}

export interface ActivityReleaseCredit {
  tag: string;
  url: string;
  publishedAt: string;
  previousPublishedAt: string;
  authors: ActivityPerson[];
}

export interface RepoActivity {
  readAt: string;
  windowDays: number;
  merged: ActivityPull[];
  milestones: ActivityMilestone[];
  releases: ActivityReleaseCredit[];
}

export interface ContributorExclusions {
  confirmed: boolean;
  logins: string[];
  suffixes: string[];
}

export interface RepoActivityView {
  readAt: string;
  windowDays: number;
  pulls: ActivityPull[];
  people: ActivityPerson[];
  count: number | null;
  milestone: ActivityMilestone | null;
  milestones: ActivityMilestone[];
}

export interface LiveRecord {
  source: string;
  readAt: string | null;
  fresh: boolean;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function isDate(value: unknown): value is string {
  return typeof value === "string" && Number.isFinite(Date.parse(value));
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isHttps(value: unknown): value is string {
  return typeof value === "string" && value.startsWith("https://");
}

function parsePerson(raw: unknown): ActivityPerson | null {
  if (!isRecord(raw) || typeof raw.login !== "string" || !raw.login || !isHttps(raw.avatar)) return null;
  return { login: raw.login, avatar: raw.avatar };
}

function parsePull(raw: unknown): ActivityPull | null {
  if (!isRecord(raw)) return null;
  const person = parsePerson(raw);
  if (!person || !isDate(raw.mergedAt) || !isHttps(raw.url) || typeof raw.title !== "string") return null;
  if (typeof raw.number !== "number" || !Number.isSafeInteger(raw.number)) return null;
  return { number: raw.number, title: raw.title, url: raw.url, ...person, mergedAt: raw.mergedAt, firstTime: raw.firstTime === true };
}

export function parseExclusions(raw: unknown = EXCLUSIONS_RAW): ContributorExclusions {
  const row = isRecord(raw) ? raw : {};
  const logins = Array.isArray(row.logins)
    ? row.logins.flatMap((entry) => (isRecord(entry) && typeof entry.login === "string" ? [entry.login.toLowerCase()] : []))
    : [];
  const suffixes = Array.isArray(row.suffixes)
    ? row.suffixes.flatMap((entry) => (isRecord(entry) && typeof entry.suffix === "string" && entry.suffix ? [entry.suffix.toLowerCase()] : []))
    : [];
  return { confirmed: row.confirmed === true, logins, suffixes };
}

export function isExcluded(login: string, exclusions: ContributorExclusions): boolean {
  const lower = login.toLowerCase();
  return exclusions.logins.includes(lower) || exclusions.suffixes.some((suffix) => lower.endsWith(suffix));
}

export function readRepoActivity(raw: unknown = SNAPSHOT): RepoActivity | null {
  if (!isRecord(raw) || !isDate(raw.readAt) || !isRecord(raw.activity)) return null;
  const activity = raw.activity;
  if (!Array.isArray(activity.merged) || !Array.isArray(activity.releases)) return null;
  const windowDays = activity.windowDays;
  if (typeof windowDays !== "number" || !Number.isSafeInteger(windowDays) || windowDays <= 0) return null;

  const merged = activity.merged.map(parsePull);
  if (merged.some((pull) => pull === null)) return null;

  if (!Array.isArray(activity.milestones)) return null;
  const milestones: ActivityMilestone[] = [];
  for (const row of activity.milestones) {
    if (!isRecord(row) || typeof row.title !== "string" || !isHttps(row.url) || !isCount(row.open) || !isCount(row.closed)) return null;
    milestones.push({ title: row.title, url: row.url, open: row.open, closed: row.closed });
  }

  const releases: ActivityReleaseCredit[] = [];
  for (const entry of activity.releases) {
    if (!isRecord(entry) || typeof entry.tag !== "string" || !isHttps(entry.url) || !isDate(entry.publishedAt) || !isDate(entry.previousPublishedAt) || !Array.isArray(entry.authors)) return null;
    const authors = entry.authors.map(parsePerson);
    if (authors.some((person) => person === null)) return null;
    releases.push({ tag: entry.tag, url: entry.url, publishedAt: entry.publishedAt, previousPublishedAt: entry.previousPublishedAt, authors: authors as ActivityPerson[] });
  }

  return { readAt: raw.readAt, windowDays, merged: merged as ActivityPull[], milestones, releases };
}

export function repoActivityView(
  activity: RepoActivity | null = readRepoActivity(),
  exclusions: ContributorExclusions = parseExclusions(),
): RepoActivityView | null {
  if (!activity) return null;
  const since = Date.parse(activity.readAt) - activity.windowDays * DAY_MS;
  const pulls = activity.merged
    .filter((pull) => Date.parse(pull.mergedAt) > since && !isExcluded(pull.login, exclusions))
    .sort((a, b) => Date.parse(b.mergedAt) - Date.parse(a.mergedAt));
  const seen = new Set<string>();
  const people: ActivityPerson[] = [];
  for (const pull of pulls) {
    if (seen.has(pull.login)) continue;
    seen.add(pull.login);
    people.push({ login: pull.login, avatar: pull.avatar });
  }
  return {
    readAt: activity.readAt,
    windowDays: activity.windowDays,
    pulls,
    people,
    count: people.length > 0 ? people.length : null,
    milestone: activity.milestones[0] ?? null,
    milestones: activity.milestones,
  };
}

export function releaseAuthors(
  tag: string,
  activity: RepoActivity | null = readRepoActivity(),
  exclusions: ContributorExclusions = parseExclusions(),
): ActivityPerson[] | null {
  const release = activity?.releases.find((entry) => entry.tag === tag);
  if (!release) return null;
  const authors = release.authors.filter((person) => !isExcluded(person.login, exclusions));
  return authors.length > 0 ? authors : null;
}

export function liveRecord(raw: unknown = SNAPSHOT, now: number = Date.now()): LiveRecord {
  const readAt = isRecord(raw) && isDate(raw.readAt) ? raw.readAt : null;
  const age = readAt === null ? Infinity : now - Date.parse(readAt);
  return { source: LIVE_RECORD_SOURCE, readAt, fresh: age >= 0 && age <= LIVE_RECORD_MAX_AGE_MS };
}
