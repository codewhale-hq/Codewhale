export const ACTIVITY_WINDOW_DAYS = 30;
export const RELEASES_CREDITED = 5;
export const SEARCH_RESULT_CAP = 1000;
export const READ_DEADLINE_MS = 20_000;
export const READ_MAX_BYTES = 2_000_000;

const DAY_MS = 24 * 60 * 60 * 1000;

export async function readCappedJson(url, { headers = {}, fetchImpl = fetch, deadlineMs = READ_DEADLINE_MS, maxBytes = READ_MAX_BYTES } = {}) {
  const response = await fetchImpl(url, { headers, signal: AbortSignal.timeout(deadlineMs) });
  if (!response.ok) throw new Error(`GitHub returned ${response.status} for ${url}`);
  const declared = Number(response.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > maxBytes) throw new Error(`response for ${url} is larger than ${maxBytes} bytes`);
  const reader = response.body?.getReader();
  if (!reader) throw new Error(`response for ${url} has no body`);
  const chunks = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel();
      throw new Error(`response for ${url} is larger than ${maxBytes} bytes`);
    }
    chunks.push(value);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

export function windowStart(readAtIso, days = ACTIVITY_WINDOW_DAYS) {
  const readAt = Date.parse(readAtIso);
  if (!Number.isFinite(readAt)) throw new Error("read date is not a date");
  return new Date(readAt - days * DAY_MS).toISOString();
}

export function mergedPullFromSearchItem(item) {
  if (!item || typeof item !== "object") return null;
  const mergedAt = item.pull_request?.merged_at;
  const login = item.user?.login;
  const avatar = item.user?.avatar_url;
  if (typeof mergedAt !== "string" || !Number.isFinite(Date.parse(mergedAt))) return null;
  if (typeof login !== "string" || !login || typeof avatar !== "string" || !avatar.startsWith("https://")) return null;
  if (!Number.isSafeInteger(item.number) || typeof item.title !== "string" || typeof item.html_url !== "string") return null;
  return {
    number: item.number,
    title: item.title,
    url: item.html_url,
    login,
    avatar,
    mergedAt,
    firstTime: item.author_association === "FIRST_TIME_CONTRIBUTOR",
  };
}

export function authorsBetween(pulls, afterIso, untilIso) {
  const after = afterIso === null ? -Infinity : Date.parse(afterIso);
  const until = Date.parse(untilIso);
  const seen = new Map();
  for (const pull of [...pulls].sort((a, b) => Date.parse(a.mergedAt) - Date.parse(b.mergedAt))) {
    const mergedAt = Date.parse(pull.mergedAt);
    if (!(mergedAt > after && mergedAt <= until)) continue;
    if (!seen.has(pull.login)) seen.set(pull.login, { login: pull.login, avatar: pull.avatar });
  }
  return [...seen.values()];
}

export function versionedMilestones(rows) {
  const candidates = [];
  for (const row of Array.isArray(rows) ? rows : []) {
    const match = typeof row?.title === "string" ? row.title.trim().match(/^v?(\d+(?:\.\d+)*)$/) : null;
    if (!match || row.state === "closed") continue;
    const open = row.open_issues;
    const closed = row.closed_issues;
    if (!Number.isSafeInteger(open) || !Number.isSafeInteger(closed) || open < 0 || closed < 0) continue;
    if (typeof row.html_url !== "string" || !row.html_url.startsWith("https://github.com/")) continue;
    candidates.push({ parts: match[1].split(".").map(Number), title: row.title.trim(), url: row.html_url, open, closed });
  }
  candidates.sort((a, b) => {
    for (let i = 0; i < Math.max(a.parts.length, b.parts.length); i += 1) {
      const diff = (a.parts[i] ?? 0) - (b.parts[i] ?? 0);
      if (diff !== 0) return diff;
    }
    return 0;
  });
  return candidates.map(({ title, url, open, closed }) => ({ title, url, open, closed }));
}

export function creditedPairs(releases) {
  const published = releases
    .filter((release) => release && !release.draft && !release.prerelease && typeof release.tag_name === "string" && Number.isFinite(Date.parse(release.published_at)))
    .sort((a, b) => Date.parse(b.published_at) - Date.parse(a.published_at));
  const pairs = [];
  for (let index = 0; index < published.length - 1 && pairs.length < RELEASES_CREDITED; index += 1) {
    pairs.push({ release: published[index], previous: published[index + 1] });
  }
  return pairs;
}

export function releaseCredits(pairs, pullsFor) {
  return pairs.map(({ release, previous }) => ({
    tag: release.tag_name,
    url: release.html_url,
    publishedAt: release.published_at,
    previousPublishedAt: previous.published_at,
    authors: authorsBetween(pullsFor(release, previous), previous.published_at, release.published_at),
  }));
}
