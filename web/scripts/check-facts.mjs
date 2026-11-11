#!/usr/bin/env node
/**
 * check-facts.mjs — CI drift gate for website facts.
 *
 * Re-derives mechanical facts from the current workspace (using the same
 * logic as derive-facts.mjs / facts-lib.mjs) and compares them against the
 * committed web/lib/facts.generated.ts. Exits non-zero when the committed
 * file is stale so the mismatch is caught before deploy.
 *
 * Usage:
 *   cd web && npm run check:facts
 *
 * Checked fields:
 *   version, providers, crates, sandboxBackends, defaultModel, nodeEngines,
 *   toolCount, contributorCount, license, latestPublishedRelease.
 *
 * Fields NOT checked (by design):
 *   generatedAt — always different
 *   sourceRevision/sourceCommittedAt — injected from the exact build checkout
 */
import { readFileSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { buildFacts, deriveFeatures, REPO_ROOT } from "./facts-lib.mjs";
import { deriveAgentDocs } from "./docs-sources-lib.mjs";
import { deriveReleaseBaseline, readCommittedBaseline, serializeBaseline, tagResolves } from "./release-baseline-lib.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const GENERATED_PATH = resolve(__dirname, "..", "lib", "facts.generated.ts");

// --- Helpers ---------------------------------------------------------

/**
 * Parse the committed `facts.generated.ts` into a plain object.
 * We don't `import()` the TS file (which would need ts-node); instead we
 * extract the JSON object literal from the export declaration.
 */
function parseCommittedFacts() {
  if (!existsSync(GENERATED_PATH)) {
    return { error: `not found: ${GENERATED_PATH}` };
  }
  const src = readFileSync(GENERATED_PATH, "utf-8");

  // Extract the object literal between "export const FACTS: RepoFacts = " and
  // the closing ";" (possibly preceded by "as const").
  const m = src.match(/export const FACTS\s*:\s*\w+\s*=\s*([\s\S]*?);?\s*$/);
  if (!m) {
    return { error: `could not parse FACTS export from ${GENERATED_PATH}` };
  }
  try {
    const obj = JSON.parse(m[1]);
    return { facts: obj };
  } catch (e) {
    return { error: `invalid JSON in ${GENERATED_PATH}: ${e.message}` };
  }
}

/**
 * Compare two facts objects and return a list of field-level diffs.
 */
function diffFacts(committed, fresh) {
  // Fields checked for drift. Skip generatedAt and exact-build provenance.
  const checkFields = [
    "version",
    "crates",
    "sandboxBackends",
    "providers",
    "models",
    "defaultModel",
    "nodeEngines",
    "toolCount",
    "contributorCount",
    "license",
    "latestPublishedRelease",
  ];

  const diffs = [];
  for (const field of checkFields) {
    const a = JSON.stringify(committed[field] ?? null);
    const b = JSON.stringify(fresh[field] ?? null);
    if (a !== b) {
      diffs.push({ field, committed: committed[field], fresh: fresh[field] });
    }
  }
  return diffs;
}

// --- Main -------------------------------------------------------------

const committed = parseCommittedFacts();
if (committed.error) {
  console.error(`[check-facts] ERROR: ${committed.error}`);
  process.exit(1);
}

// The shared parser refuses incomplete or malformed provider projections.
const fresh = buildFacts();
const docsPath = resolve(__dirname, "..", "data", "agent-docs.generated.json");
if (!existsSync(docsPath) || JSON.stringify(JSON.parse(readFileSync(docsPath, "utf8"))) !== JSON.stringify(deriveAgentDocs(resolve(__dirname, ".."), REPO_ROOT))) {
  console.error("[check-facts] FAIL — generated agent docs are stale; run npm run prebuild");
  process.exit(1);
}
const featuresPath = resolve(__dirname, "..", "data", "features.generated.json");
if (!existsSync(featuresPath) || JSON.stringify(JSON.parse(readFileSync(featuresPath, "utf8"))) !== JSON.stringify(deriveFeatures())) {
  console.error("[check-facts] FAIL — generated feature registry is stale; run npm run prebuild");
  process.exit(1);
}

const committedBaseline = readCommittedBaseline();
if (!committedBaseline || !fresh.latestPublishedRelease || committedBaseline.tag !== fresh.latestPublishedRelease.tag) {
  console.error(`[check-facts] FAIL — data/release-baseline.json is for ${committedBaseline?.tag ?? "nothing"}, the published release is ${fresh.latestPublishedRelease?.tag ?? "unknown"}; run npm run prebuild in a checkout that has the tag`);
  process.exit(1);
}
if (tagResolves(committedBaseline.tag)) {
  const derivedBaseline = deriveReleaseBaseline(fresh.latestPublishedRelease);
  if (!derivedBaseline || serializeBaseline(derivedBaseline) !== serializeBaseline(committedBaseline)) {
    console.error("[check-facts] FAIL — data/release-baseline.json differs from the tagged source; run npm run prebuild");
    process.exit(1);
  }
} else {
  console.warn(`[check-facts] warning: tag ${committedBaseline.tag} is not in this checkout, so the release baseline was not recomputed`);
}

// Quick sanity: critical source facts must never degrade to matching nulls.
const criticalGaps = [];
if (!fresh.version) criticalGaps.push("version");
if (fresh.providers.length === 0) criticalGaps.push("providers");
if (!fresh.latestPublishedRelease) criticalGaps.push("latestPublishedRelease");
if (criticalGaps.length > 0) {
  console.error(
    `[check-facts] FAIL — fresh derivation returned empty/missing: ${criticalGaps.join(", ")}`,
  );
  process.exit(1);
}

const diffs = diffFacts(committed.facts, fresh);

if (diffs.length === 0) {
  console.log("[check-facts] OK — committed facts.generated.ts matches workspace");
  process.exit(0);
}

console.error("[check-facts] FAIL — committed facts.generated.ts is stale");
for (const d of diffs) {
  console.error(`  ${d.field}:`);
  console.error(`    committed: ${JSON.stringify(d.committed)}`);
  console.error(`    fresh:     ${JSON.stringify(d.fresh)}`);
}

console.error(
  "\nRun `cd web && npm run prebuild` to regenerate facts.generated.ts, then commit the result.",
);
process.exit(1);
