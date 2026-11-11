import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { REPO_ROOT, deriveLatestPublishedRelease } from "./facts-lib.mjs";
import { readDocTopics } from "./docs-sources-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
export const RELEASE_DOCS_PATH = resolve(here, "..", "data", "agent-docs.release.json");
export const RELEASE_DOCS_INDEX_PATH = resolve(here, "..", "data", "agent-docs.release-index.json");
export const RELEASE_DOCS_FORMAT = "codewhale.release-docs.v1";

function gitShow(tag, path) {
  const result = spawnSync("git", ["-C", REPO_ROOT, "show", `${tag}:${path}`], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  return result.status === 0 ? result.stdout : null;
}

function tagResolves(tag) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag)) return false;
  return spawnSync("git", ["-C", REPO_ROOT, "rev-parse", "--verify", "--quiet", `${tag}^{commit}`], { encoding: "utf8" }).status === 0;
}

export { tagResolves };

export function deriveReleaseDocs(webDir, release) {
  if (!release || !tagResolves(release.tag)) return null;
  const topics = [];
  const omitted = [];
  for (const topic of readDocTopics(webDir)) {
    const sources = [];
    const missing = [];
    for (const path of topic.repoSource) {
      const content = gitShow(release.tag, path.split("#", 1)[0]);
      if (content === null) missing.push(path);
      else sources.push({ path, sha256: createHash("sha256").update(content).digest("hex"), content });
    }
    if (sources.length === 0) omitted.push({ id: topic.id, slug: topic.slug, missing });
    else topics.push({ id: topic.id, slug: topic.slug, sources, missing });
  }
  return { format: RELEASE_DOCS_FORMAT, tag: release.tag, version: release.version, topics, omitted };
}

export function serializeReleaseDocs(snapshot) {
  return `${JSON.stringify(snapshot, null, 2)}\n`;
}

export function releaseDocsIndex(snapshot) {
  return { tag: snapshot.tag, slugs: snapshot.topics.map((topic) => topic.slug), omitted: snapshot.omitted.map((topic) => topic.slug) };
}

export function readCommittedReleaseDocsIndex() {
  if (!existsSync(RELEASE_DOCS_INDEX_PATH)) return null;
  try { return JSON.parse(readFileSync(RELEASE_DOCS_INDEX_PATH, "utf8")); } catch { return null; }
}

export function readCommittedReleaseDocs() {
  if (!existsSync(RELEASE_DOCS_PATH)) return null;
  try { return JSON.parse(readFileSync(RELEASE_DOCS_PATH, "utf8")); } catch { return null; }
}

export function refreshReleaseDocs(webDir, release) {
  const committed = readCommittedReleaseDocs();
  const fresh = deriveReleaseDocs(webDir, release);
  if (!fresh) {
    const note = !release ? "no published release" : committed?.tag === release.tag
      ? `tag ${release.tag} is not available in this checkout, keeping the committed snapshot`
      : `tag ${release.tag} is not available in this checkout and the committed snapshot is for ${committed?.tag ?? "nothing"}`;
    if (release && committed?.tag !== release.tag) throw new Error(`[release-docs] ${note}`);
    console.warn(`[release-docs] ${note}`);
    return committed;
  }
  const text = serializeReleaseDocs(fresh);
  if (!existsSync(RELEASE_DOCS_PATH) || readFileSync(RELEASE_DOCS_PATH, "utf8") !== text) writeFileSync(RELEASE_DOCS_PATH, text);
  const indexText = `${JSON.stringify(releaseDocsIndex(fresh), null, 2)}\n`;
  if (!existsSync(RELEASE_DOCS_INDEX_PATH) || readFileSync(RELEASE_DOCS_INDEX_PATH, "utf8") !== indexText) writeFileSync(RELEASE_DOCS_INDEX_PATH, indexText);
  return fresh;
}

export function checkReleaseDocs(snapshot, release, registry, derived, index) {
  const problems = [];
  if (!release) return ["the published release is unknown"];
  if (!snapshot || snapshot.format !== RELEASE_DOCS_FORMAT || !Array.isArray(snapshot.topics) || !Array.isArray(snapshot.omitted)) {
    return ["data/agent-docs.release.json is missing or malformed; run npm run prebuild in a checkout that has the tag"];
  }
  if (snapshot.tag !== release.tag) {
    problems.push(`data/agent-docs.release.json is for ${snapshot.tag}, the published release is ${release.tag}; run npm run prebuild in a checkout that has the tag`);
  }
  if (snapshot.version !== release.version) problems.push(`snapshot version ${snapshot.version} differs from the published release ${release.version}`);
  for (const topic of snapshot.topics) {
    for (const source of topic.sources) {
      if (createHash("sha256").update(source.content).digest("hex") !== source.sha256) problems.push(`${topic.slug}: ${source.path} does not match its recorded hash`);
    }
  }
  const covered = new Set([...snapshot.topics, ...snapshot.omitted].map((topic) => topic.slug));
  for (const topic of registry) {
    if (!covered.has(topic.slug)) problems.push(`docs topic ${topic.slug} is neither in the snapshot nor recorded as omitted`);
  }
  if (index !== undefined && JSON.stringify(index) !== JSON.stringify(releaseDocsIndex(snapshot))) {
    problems.push("data/agent-docs.release-index.json does not match data/agent-docs.release.json; run npm run prebuild");
  }
  if (derived && problems.length === 0 && serializeReleaseDocs(derived) !== serializeReleaseDocs(snapshot)) {
    problems.push(`data/agent-docs.release.json differs from the documents at ${release.tag}; run npm run prebuild`);
  }
  return problems;
}

export function currentRelease() {
  return deriveLatestPublishedRelease();
}
