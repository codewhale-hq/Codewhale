#!/usr/bin/env node
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(webRoot, "..");

const FORBIDDEN = [
  /scripted[-_ ]demo/i,
  /scripted[_ ]local/i,
  /website-demo/i,
  /scripted (test |local )?model/i,
  /CODEWHALE_SIMULATOR/,
  /\(unenforced\)/,
  /names and text in them are placeholders/i,
  /taken against a test fixture/i,
];

const ROOTS = [
  "web/lib/terminal-captures",
  "web/lib/i18n/dictionaries",
  "web/lib/content",
  "web/gt-catalog",
  "web/data/desktop-media.json",
  "README.md",
  "docs/i18n",
];

function* walk(target) {
  if (!existsSync(target)) return;
  if (statSync(target).isFile()) {
    yield target;
    return;
  }
  for (const name of readdirSync(target)) {
    if (name.endsWith(".test.ts")) continue;
    yield* walk(path.join(target, name));
  }
}

const errors = [];
for (const root of ROOTS) {
  for (const file of walk(path.join(repoRoot, root))) {
    const text = readFileSync(file, "utf8");
    for (const pattern of FORBIDDEN) {
      const match = text.match(pattern);
      if (match) errors.push(`${path.relative(repoRoot, file)}: "${match[0]}"`);
    }
  }
}

const provenance = JSON.parse(readFileSync(path.join(webRoot, "lib/terminal-captures/released/provenance.json"), "utf8"));
if (provenance.model?.scripted !== false) errors.push("released capture provenance must record model.scripted = false");

if (errors.length) {
  console.error(`media honesty check failed: scripted or simulated media is named in public files:\n- ${errors.join("\n- ")}`);
  process.exit(1);
}

const latest = JSON.parse(readFileSync(path.join(webRoot, "data/latest-published-release.json"), "utf8"));
const captured = provenance.binary?.release_tag ?? null;
if (captured !== latest.tag) {
  console.warn(
    `media freshness: the home terminal frames come from ${captured ?? `an unpublished build (${provenance.binary?.version_line})`}, and the latest published release is ${latest.tag}. Re-capture them from ${latest.tag} when it is the release the site describes.`,
  );
}
console.log("media honesty check passed");
