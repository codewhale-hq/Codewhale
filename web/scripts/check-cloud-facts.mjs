#!/usr/bin/env node
/** Local source, release, pinned-key parity and public-fixture gate. */
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { validateSource, verifyEnvelope, parseTsKeys, validateTrustedKeys, readBoundedFile } from "./facts-publish.mjs";

const WEB_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REPO_ROOT = resolve(WEB_ROOT, "..");
export { parseTsKeys };

export function parseRustKeys(text) {
  const source = text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  const tables = [...source.matchAll(/^\s*pub\s+const\s+TRUSTED_KEYS\s*:\s*&\s*\[TrustedKey\]\s*=\s*&\s*\[([\s\S]*?)\]\s*;/gm)];
  if (tables.length !== 1) throw new Error("cannot parse exactly one Rust TRUSTED_KEYS table");
  const table = tables[0];
  const body = table[1].replace(/^\s*\/\/.*$/gm, "");
  const keys = [];
  const remainder = body.replace(/TrustedKey\s*\{\s*key_id:\s*"([^"]+)",\s*public_key:\s*\[([^\]]*)\],\s*status:\s*KeyStatus::(Active|Retired),\s*role:\s*KeyRole::(Founder|Automation)\s*,?\s*\}/g, (_, keyId, encoded, status, role) => {
    const pieces = encoded.split(",").map((piece) => piece.trim()).filter(Boolean);
    if (pieces.length !== 32 || pieces.some((piece) => !/^(?:\d+|0x[0-9a-fA-F]+)$/.test(piece))) throw new Error("Rust public key must contain 32 literal bytes");
    const bytes = pieces.map(Number);
    if (bytes.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) throw new Error("Rust public key byte out of range");
    keys.push({ keyId, publicKey: Buffer.from(bytes).toString("base64"), status: status.toLowerCase(), role: role.toLowerCase() });
    return "";
  });
  if (remainder.replace(/[\s,]/g, "")) throw new Error("unparsed Rust TRUSTED_KEYS entry");
  return validateTrustedKeys(keys);
}

function text(path) { return readBoundedFile(path).toString("utf8"); }
function json(path) { return JSON.parse(text(path)); }

export function checkCloudFacts() {
  const failures = [];
  const source = json(resolve(REPO_ROOT, "docs/cloud-facts/stable.json"));
  for (const error of validateSource(source)) failures.push(`stable.json: ${error}`);
  if (source.channel !== "stable") failures.push("stable.json: channel must be stable");
  const latest = json(resolve(WEB_ROOT, "data/latest-published-release.json"));
  if (source.release?.latest !== latest.version) failures.push("stable.json release.latest differs from latest-published-release.json");
  if (source.release?.release_url && source.release.release_url !== latest.url) failures.push("stable.json release.release_url differs from latest-published-release.json");
  // An explicit empty table is valid and inert; parse failures are never empty.
  const rustKeys = parseRustKeys(text(resolve(REPO_ROOT, "crates/config/src/cloud_facts/keys.rs")));
  const tsKeys = parseTsKeys(text(resolve(WEB_ROOT, "lib/cloud-facts/keys.ts")));
  if (JSON.stringify(rustKeys) !== JSON.stringify(tsKeys)) failures.push("Rust and web pinned key tables diverge");
  const testOnlyPub = text(resolve(REPO_ROOT, "docs/cloud-facts/fixtures/test-only.pub")).trim();
  for (const name of ["envelope-stable-v7.json", "envelope-future-only-v8.json"]) {
    const result = verifyEnvelope(json(resolve(REPO_ROOT, "docs/cloud-facts/fixtures", name)), testOnlyPub);
    if (!result.ok) failures.push(`fixture ${name}: ${result.errors.join("; ")}`);
  }
  if ([...rustKeys, ...tsKeys].some((key) => key.publicKey === testOnlyPub || key.keyId === "cwf-test-only")) failures.push("TEST-ONLY keys must never be production trust anchors");
  return { failures, factsVersion: source.facts_version, activeKeys: tsKeys.filter((key) => key.status === "active").length };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = checkCloudFacts();
    if (result.failures.length) throw new Error(result.failures.join("\n  - "));
    console.log(`check-cloud-facts: OK (facts_version=${result.factsVersion}, ${result.activeKeys} active production keys)`);
  } catch (error) {
    console.error(`check-cloud-facts: FAIL\n  - ${error.message}`);
    process.exitCode = 1;
  }
}
