/**
 * facts-lib.mjs — shared derivation logic for website fact generation and
 * drift checking. Imported by both derive-facts.mjs (prebuild) and
 * check-facts.mjs (CI gate).
 *
 * Sources of truth:
 *   - <repo>/Cargo.toml                         → version, workspace crates
 *   - <repo>/crates/sandbox/src/lib.rs           → enforced sandbox markers
 * *   - <repo>/crates/config/assets/provider_descriptors.json → provider labels and default model
 *   - <repo>/npm/codewhale/package.json         → node engines
 *   - <repo>/crates/engine/src/tools/*.rs          → tool count (ToolSpec impls)
 *   - <repo>/LICENSE                            → license
 *   - <repo>/web/data/latest-published-release.json → latest published release
 */
import { parseModelCatalog } from "../lib/model-catalog.mjs";
import { parseProviderDescriptors } from "../lib/provider-descriptors.mjs";
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
// __dirname is web/scripts; REPO_ROOT is the workspace root (two levels up).
export const REPO_ROOT = resolve(__dirname, "..", "..");

function read(rel) {
  const p = join(REPO_ROOT, rel);
  if (!existsSync(p)) return null;
  return readFileSync(p, "utf-8");
}

export function deriveVersion() {
  const cargo = read("Cargo.toml");
  if (!cargo) return null;
  const m = cargo.match(/^version\s*=\s*"([^"]+)"/m);
  return m ? m[1] : null;
}

export function deriveCrates() {
  const cargo = read("Cargo.toml");
  if (!cargo) return [];
  const block = cargo.match(/members\s*=\s*\[([\s\S]*?)\]/);
  if (!block) return [];
  return [...block[1].matchAll(/"crates\/([^"]+)"/g)].map((m) => m[1]).sort();
}

export function deriveSandboxBackends() {
  const source = read("crates/sandbox/src/lib.rs");
  return source ? deriveSandboxBackendsFromSource(source) : [];
}

export function deriveSandboxBackendsFromSource(source) {
  const marker = source.match(
    /pub const PUBLIC_SANDBOX_BACKENDS\s*:\s*&\[&str\]\s*=\s*&\[([\s\S]*?)\];/,
  );
  if (!marker) return [];
  return [...marker[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

// Descriptor data owns public identities, order, labels and defaults.
const descriptorData = parseProviderDescriptors(read("crates/config/assets/provider_descriptors.json"));
export function deriveProviders() {
  if (!descriptorData) throw new Error("missing or malformed provider descriptors");
  return descriptorData.providers;
}

export function deriveDefaultModel() {
  return descriptorData?.defaultModel ?? null;
}

export function deriveNodeEngines() {
  const pkg = read("npm/codewhale/package.json");
  if (!pkg) return null;
  try {
    return JSON.parse(pkg).engines?.node ?? null;
  } catch {
    return null;
  }
}

// Public model identities, labels and source-support dates share the reviewed
// Engine catalog owner. A malformed source cannot become a partial/empty list.
export function deriveModels() {
  const models = parseModelCatalog(read("crates/config/assets/models_dev.bundled.json"));
  if (!models) throw new Error("missing or malformed reviewed model catalog");
  return models;
}

export function deriveToolCount() {
  const dir = join(REPO_ROOT, "crates/engine/src/tools");
  if (!existsSync(dir)) return null;
  let count = 0;
  for (const f of readdirSync(dir)) {
    if (!f.endsWith(".rs")) continue;
    const body = readFileSync(join(dir, f), "utf-8");
    count += (body.match(/^impl ToolSpec for /gm) ?? []).length;
  }
  return count > 0 ? count : null;
}

export function deriveContributorCount() {
  const contributors = JSON.parse(read("docs/contributors.json") ?? "null");
  const people = contributors?.people;
  if (!Array.isArray(people) || people.length === 0 || people.some((person) =>
    !person || typeof person.login !== "string" || person.login.trim().length === 0)
    || new Set(people.map((person) => person.login.toLowerCase())).size !== people.length) {
    throw new Error("missing or malformed contributor roster");
  }
  return people.length;
}

export function deriveLicense() {
  const lic = read("LICENSE");
  if (!lic) return null;
  const first = lic.split(/\r?\n/).find((l) => l.trim().length > 0);
  if (!first) return null;
  if (/^MIT License/i.test(first)) return "MIT";
  if (/Apache.*2\.0/i.test(first)) return "Apache-2.0";
  return first.trim();
}

export function deriveLatestPublishedRelease() {
  const raw = read("web/data/latest-published-release.json");
  if (!raw) return null;
  try {
    const release = JSON.parse(raw);
    if (
      typeof release.tag !== "string" ||
      typeof release.version !== "string" ||
      release.tag !== `v${release.version}` ||
      typeof release.publishedAt !== "string" ||
      !Number.isFinite(Date.parse(release.publishedAt)) ||
      typeof release.url !== "string" ||
      release.url !== `https://github.com/codewhale-hq/CodeWhale/releases/tag/${release.tag}`
    ) {
      return null;
    }
    return release;
  } catch {
    return null;
  }
}

export function deriveFeatures() {
  const source = read("docs/features.toml");
  if (!source) throw new Error("missing feature registry");
  const parsed = spawnSync("python3", ["-c", "import json, sys, tomllib; print(json.dumps(tomllib.loads(sys.stdin.read())['feature']))"],
    { input: source, encoding: "utf8", maxBuffer: 4 * 1024 * 1024 });
  if (parsed.status !== 0) throw new Error(`feature registry parse failed: ${parsed.stderr || parsed.error || parsed.status}`);
  const rows = JSON.parse(parsed.stdout);
  if (!Array.isArray(rows) || rows.length === 0) throw new Error("empty feature registry");
  const ids = new Set();
  const features = rows.map((row) => {
    if (!row || typeof row.id !== "string" || !/^[a-z0-9-]+$/.test(row.id) || ids.has(row.id)
      || typeof row.name !== "string" || typeof row.summary !== "string"
      || !["stable", "flagged", "experimental"].includes(row.status)
      || !row.surfaces || typeof row.surfaces !== "object" || Array.isArray(row.surfaces)) {
      throw new Error(`invalid feature registry row: ${row?.id ?? "unknown"}`);
    }
    ids.add(row.id);
    for (const [surface, state] of Object.entries(row.surfaces)) {
      if (!["tui", "desktop", "web", "api"].includes(surface) || !["stable", "partial", "preview", "none"].includes(state)) {
        throw new Error(`invalid feature surface: ${row.id}/${surface}`);
      }
    }
    for (const field of ["docs", "flag", "since"]) {
      if (row[field] !== undefined && typeof row[field] !== "string") throw new Error(`invalid feature ${field}: ${row.id}`);
    }
    const documentation = row.docs ?? null;
    if (documentation) {
      const path = documentation.split("#", 1)[0];
      if (path.startsWith("/") || path.includes("..") || !existsSync(join(REPO_ROOT, path))) {
        throw new Error(`missing feature document: ${row.id}/${path}`);
      }
    }
    const match = source.match(new RegExp(`^[ \\t]*id[ \\t]*=[ \\t]*["']${row.id}["']`, "m"));
    if (!match) throw new Error(`feature registry location missing: ${row.id}`);
    const line = source.slice(0, match.index).split("\n").length;
    return { id: row.id, name: row.name, summary: row.summary, status: row.status,
      surfaces: row.surfaces, since: row.since ?? null, flag: row.flag ?? null,
      docs: documentation, registryLine: line };
  });
  return { format: "codewhale.features.v1", source: { path: "docs/features.toml", sha256: createHash("sha256").update(source).digest("hex") }, features };
}

/**
 * Re-derive all mechanical facts from the current workspace. The returned
 * object is the same shape as web/lib/facts.generated.ts → RepoFacts.
 */
export function buildFacts() {
  const providers = deriveProviders();

  const facts = {
    generatedAt: new Date().toISOString(),
    // next.config.ts injects these from the exact checkout into the built
    // artifact. They stay null in the tracked snapshot to avoid a
    // self-referential generated-file diff after every commit.
    sourceRevision: null,
    sourceCommittedAt: null,
    version: deriveVersion(),
    crates: deriveCrates(),
    sandboxBackends: deriveSandboxBackends(),
    providers,
    models: deriveModels(),
    defaultModel: deriveDefaultModel(),
    nodeEngines: deriveNodeEngines(),
    toolCount: deriveToolCount(),
    contributorCount: deriveContributorCount(),
    license: deriveLicense(),
    latestPublishedRelease: deriveLatestPublishedRelease(),
  };

  return facts;
}
