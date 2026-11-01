#!/usr/bin/env node
/**
 * check-locales.mjs — CI gate against website dictionary drift (#3091).
 *
 * web/lib/i18n/dictionaries/en/ is the reference. Every other locale
 * directory must define chrome.ts and home.ts with exactly the same
 * top-level keys, and every `{token}` template placeholder in the English
 * values must survive translation (call sites interpolate with `fill()`,
 * so a dropped token renders literal braces on the page).
 *
 * This is the dependency-free half of the gate; web/lib/i18n/dictionaries.test.ts
 * covers the same contract through the real module imports.
 *
 * Exits non-zero on any parity violation.
 */
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const DICT_DIR = join(ROOT, "lib", "i18n", "dictionaries");
const REFERENCE = "en";
const FILES = ["chrome.ts", "home.ts"];
// Per-page dictionaries (#5337): optional per locale. English is the
// required reference; a locale that ships the file is held to the same
// key/token parity, and a locale without it falls back to English at
// lookup time — so absence is a valid state, never a failure.
const OPTIONAL_FILES = [
  "features.ts",
  "enterprise.ts",
  "docs-skills.ts",
  "docs-claude-code.ts",
  "docs-guide.ts",
  "docs-shell.ts",
  "docs-hooks.ts",
  "docs-troubleshooting.ts",
  "docs-configuration.ts",
  "docs-fleet.ts",
  "docs-mcp.ts",
  "docs-modes.ts",
  "docs-runtime-api.ts",
  "docs-sandbox.ts",
  "docs-subagents.ts",
  "docs-web.ts",
  "docs-auth.ts",
  "docs-trust.ts",
  "docs-work.ts",
  "docs-pet.ts",
  "docs-review.ts",
  "docs-vocabulary.ts",
  "states.ts",
  "changelog.ts",
  "legal-terms.ts",
  "legal-privacy.ts",
  "digest.ts",
  "feed.ts",
  "community.ts",
  "faq.ts",
  "roadmap.ts",
  "contribute.ts",
  "constitution.ts",
  "runtime.ts",
  "computer-use.ts",
];

/** Top-level keys of the exported object literal (two-space indented `key:`). */
function extractKeys(source) {
  return new Set(
    [...source.matchAll(/^ {2}(\w+):/gm)].map((m) => m[1]),
  );
}

function extractTokens(source) {
  const tokens = new Map();
  // Per-key token sets: walk `key: "…"` and array entries line by line.
  let current = null;
  for (const line of source.split("\n")) {
    const keyMatch = line.match(/^ {2}(\w+):/);
    if (keyMatch) current = keyMatch[1];
    for (const t of line.matchAll(/\{(\w+)\}/g)) {
      if (!current) continue;
      if (!tokens.has(current)) tokens.set(current, new Set());
      tokens.get(current).add(t[1]);
    }
  }
  return tokens;
}

let failed = false;
const fail = (msg) => {
  console.error(`[check-locales] FAIL — ${msg}`);
  failed = true;
};

const locales = readdirSync(DICT_DIR, { withFileTypes: true })
  .filter((d) => d.isDirectory() && d.name !== REFERENCE)
  .map((d) => d.name)
  .sort();

console.log(`[check-locales] reference ${REFERENCE}: ${FILES.join(", ")}`);
console.log(`[check-locales] locale dirs: ${locales.join(", ")}`);

/** Compare one locale file against the English reference (parity + tokens). */
function checkLocaleFile(locale, file, refKeys, refTokens) {
  const path = join(DICT_DIR, locale, file);
  const source = readFileSync(path, "utf8");
  const keys = extractKeys(source);
  const missing = [...refKeys].filter((k) => !keys.has(k));
  const extra = [...keys].filter((k) => !refKeys.has(k));
  if (missing.length) fail(`${locale}/${file}: missing keys: ${missing.join(", ")}`);
  if (extra.length) fail(`${locale}/${file}: keys the reference lacks: ${extra.join(", ")}`);

  const tokens = extractTokens(source);
  for (const [key, refSet] of refTokens) {
    const got = tokens.get(key) ?? new Set();
    const dropped = [...refSet].filter((t) => !got.has(t));
    if (dropped.length) {
      fail(`${locale}/${file}: ${key} dropped template token(s): ${dropped.join(", ")}`);
    }
  }
  if (!missing.length && !extra.length) {
    console.log(`[check-locales] ${locale}/${file}: ${keys.size}/${refKeys.size} keys — complete`);
  }
}

for (const file of [...FILES, ...OPTIONAL_FILES]) {
  const required = FILES.includes(file);
  const refPath = join(DICT_DIR, REFERENCE, file);
  if (!existsSync(refPath)) {
    fail(`${REFERENCE}/${file}: reference file missing`);
    continue;
  }
  const refSource = readFileSync(refPath, "utf8");
  const refKeys = extractKeys(refSource);
  const refTokens = extractTokens(refSource);

  for (const locale of locales) {
    if (!existsSync(join(DICT_DIR, locale, file))) {
      if (required) {
        fail(`${locale}/${file}: missing (reference has ${refKeys.size} keys)`);
      } else {
        console.log(`[check-locales] ${locale}/${file}: absent — falls back to English`);
      }
      continue;
    }
    checkLocaleFile(locale, file, refKeys, refTokens);
  }
}

if (failed) {
  console.error("[check-locales] FAIL");
  process.exit(1);
}
console.log("[check-locales] PASS");
