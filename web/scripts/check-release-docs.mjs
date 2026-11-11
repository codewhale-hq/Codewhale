#!/usr/bin/env node
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { readDocTopics } from "./docs-sources-lib.mjs";
import { checkReleaseDocs, currentRelease, deriveReleaseDocs, readCommittedReleaseDocs, readCommittedReleaseDocsIndex, tagResolves } from "./release-docs-lib.mjs";

const webDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const release = currentRelease();
const snapshot = readCommittedReleaseDocs();
const derived = release && tagResolves(release.tag) ? deriveReleaseDocs(webDir, release) : null;
if (release && !derived) console.warn(`[check-release-docs] warning: tag ${release.tag} is not in this checkout, so the snapshot was not recomputed`);
const problems = checkReleaseDocs(snapshot, release, readDocTopics(webDir), derived, readCommittedReleaseDocsIndex());
if (problems.length > 0) {
  for (const problem of problems) console.error(`[check-release-docs] FAIL — ${problem}`);
  process.exit(1);
}
console.log(`[check-release-docs] OK — snapshot is for ${snapshot.tag}, the published release`);
