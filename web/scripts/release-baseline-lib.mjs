import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { REPO_ROOT, countToolSpecs, parseFeatureRows } from "./facts-lib.mjs";
import { parseModelCatalog } from "../lib/model-catalog.mjs";
import { parseProviderDescriptors } from "../lib/provider-descriptors.mjs";

const here = dirname(fileURLToPath(import.meta.url));
export const BASELINE_PATH = resolve(here, "..", "data", "release-baseline.json");
export const BASELINE_FORMAT = "codewhale.release-baseline.v1";
const TOOL_DIRECTORIES = ["crates/engine/src/tools", "crates/tui/src/tools"];

function git(args) {
  const result = spawnSync("git", ["-C", REPO_ROOT, ...args], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  return result.status === 0 ? result.stdout : null;
}

export function tagResolves(tag) {
  return /^v\d+\.\d+\.\d+$/.test(tag) && git(["rev-parse", "--verify", "--quiet", `${tag}^{commit}`]) !== null;
}

function showAtTag(tag, path) {
  return git(["show", `${tag}:${path}`]);
}

function toolCountAtTag(tag) {
  for (const directory of TOOL_DIRECTORIES) {
    const listing = git(["ls-tree", "--name-only", tag, `${directory}/`]);
    const files = listing ? listing.split("\n").filter((name) => name.endsWith(".rs")) : [];
    if (files.length === 0) continue;
    const bodies = files.map((name) => showAtTag(tag, name));
    if (bodies.some((body) => body === null)) return null;
    return countToolSpecs(bodies);
  }
  return null;
}

export function deriveReleaseBaseline(release) {
  if (!release || !tagResolves(release.tag)) return null;
  const registry = showAtTag(release.tag, "docs/features.toml");
  const descriptors = parseProviderDescriptors(showAtTag(release.tag, "crates/config/assets/provider_descriptors.json"));
  const models = parseModelCatalog(showAtTag(release.tag, "crates/config/assets/models_dev.bundled.json"));
  if (!registry || !descriptors || !models) return null;
  const features = parseFeatureRows(registry).map((row) => ({
    id: row.id,
    name: row.name,
    summary: row.summary,
    status: row.status,
    since: typeof row.since === "string" ? row.since : null,
  }));
  return {
    format: BASELINE_FORMAT,
    tag: release.tag,
    version: release.version,
    features,
    providers: descriptors.providers,
    models,
    toolCount: toolCountAtTag(release.tag),
  };
}

export function readCommittedBaseline() {
  if (!existsSync(BASELINE_PATH)) return null;
  try { return JSON.parse(readFileSync(BASELINE_PATH, "utf8")); } catch { return null; }
}

export function serializeBaseline(baseline) {
  return `${JSON.stringify(baseline, null, 2)}\n`;
}

export function refreshReleaseBaseline(release) {
  const committed = readCommittedBaseline();
  const fresh = deriveReleaseBaseline(release);
  if (!fresh) {
    const note = !release ? "no published release" : committed?.tag === release.tag
      ? `tag ${release.tag} is not available in this checkout, keeping the committed baseline`
      : `tag ${release.tag} is not available in this checkout and the committed baseline is for ${committed?.tag ?? "nothing"}; capability rows and counts stay hidden`;
    console.warn(`[release-baseline] ${note}`);
    return committed;
  }
  const text = serializeBaseline(fresh);
  if (!existsSync(BASELINE_PATH) || readFileSync(BASELINE_PATH, "utf8") !== text) writeFileSync(BASELINE_PATH, text);
  return fresh;
}
