#!/usr/bin/env node
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { currentRelease, refreshReleaseDocs } from "./release-docs-lib.mjs";

const webDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const snapshot = refreshReleaseDocs(webDir, currentRelease());
if (snapshot) {
  console.log(`[release-docs] ${snapshot.tag}: ${snapshot.topics.length} topics, ${snapshot.omitted.length} omitted`);
}
