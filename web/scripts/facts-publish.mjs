#!/usr/bin/env node
/**
 * CodeWhale cloud facts (facts/v1) authoring tool. Zero npm dependencies.
 *
 *   node scripts/facts-publish.mjs keygen  --key-id cwf-2026-09 --out /secure/path.key
 *   node scripts/facts-publish.mjs sign    --source ../docs/cloud-facts/stable.json --channel stable \
 *                                          --key-id cwf-2026-09 [--facts-version N] [--out envelope.json]
 *   node scripts/facts-publish.mjs verify  envelope.json [--public-key <base64>]
 *   node scripts/facts-publish.mjs emit-sql envelope.json [--published-by who] [--public-key <base64>]
 *   node scripts/facts-publish.mjs publish envelope.json [--dry-run] [--published-by who]
 *   node scripts/facts-publish.mjs revoke  --channel stable --version N --reason "..." [--dry-run]
 *
 * Secrets are read ONLY from the environment at sign/publish time and are never
 * printed:
 *   CODEWHALE_FACTS_SIGNING_KEY       PEM (PKCS#8) Ed25519 private key contents
 *   CODEWHALE_FACTS_SIGNING_KEY_FILE  path to that PEM (alternative)
 *   SUPABASE_URL                      https://<ref>.supabase.co  (publish/revoke)
 *   SUPABASE_SERVICE_ROLE_KEY         service-role key (publish/revoke only; never embed)
 *
 * Signing contract (must match crates/config/src/cloud_facts/verify.rs and
 * web/lib/cloud-facts.ts): Ed25519 detached signature over
 *   "codewhale-facts/v1\0" || key_id || "\0" || payload_bytes
 * where payload_bytes is canonical JSON (sorted keys, no whitespace, UTF-8).
 * Clients verify the exact bytes carried in payload_b64; they never re-canonicalize.
 */
import { createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify, createHash } from "node:crypto";
import { readFileSync, writeFileSync, mkdirSync, openSync, closeSync, readSync, fstatSync, lstatSync, constants } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const DOMAIN = "codewhale-facts/v1\0";
export const ENVELOPE_VERSION = 1;
export const SCHEMA_VERSION = 1;
export const MAX_PAYLOAD_BYTES = 512 * 1024;
export const MAX_ENVELOPE_BYTES = 768 * 1024;
export const REGISTRY_CHANNEL = "registry";
export const REGISTRY_SCHEMA = "codewhale.registry.v1";
export const MAX_REGISTRY_PAYLOAD_BYTES = 8 * 1024 * 1024;
export const MAX_REGISTRY_ENVELOPE_BYTES = Math.floor(MAX_REGISTRY_PAYLOAD_BYTES / 3) * 4 + 64 * 1024;
const KEY_ID_RE = /^cwf-[a-z0-9-]{1,32}$/;
const CHANNEL_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
const CI_MARKERS = ["CI", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "CIRCLECI", "JENKINS_URL", "TF_BUILD"];

export function maxPayloadBytesFor(channel) {
  return channel === REGISTRY_CHANNEL ? MAX_REGISTRY_PAYLOAD_BYTES : MAX_PAYLOAD_BYTES;
}

export function maxEnvelopeBytesFor(channel) {
  return channel === REGISTRY_CHANNEL ? MAX_REGISTRY_ENVELOPE_BYTES : MAX_ENVELOPE_BYTES;
}

const here = dirname(fileURLToPath(import.meta.url));
const WEB_ROOT = resolve(here, "..");
const REPO_ROOT = resolve(WEB_ROOT, "..");

// ---------------------------------------------------------------------------
// Canonical JSON + signing primitives (exported for tests)
// ---------------------------------------------------------------------------

export function canonicalize(value) {
  if (value === null || typeof value !== "object") {
    if (typeof value === "number" && !Number.isFinite(value)) {
      throw new Error("non-finite number in payload");
    }
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalize).join(",")}]`;
  const keys = Object.keys(value).sort();
  const parts = [];
  for (const key of keys) {
    const v = value[key];
    if (v === undefined) continue;
    parts.push(`${JSON.stringify(key)}:${canonicalize(v)}`);
  }
  return `{${parts.join(",")}}`;
}

export function signingMessage(keyId, payloadBytes) {
  return Buffer.concat([Buffer.from(DOMAIN, "utf8"), Buffer.from(keyId, "utf8"), Buffer.from([0]), payloadBytes]);
}

export function rawPublicKeyFromKeyObject(keyObject) {
  const spki = keyObject.export({ type: "spki", format: "der" });
  // Ed25519 SPKI DER is a fixed 12-byte prefix followed by the 32-byte key.
  return spki.subarray(spki.length - 32);
}

export function publicKeyObjectFromRaw(rawB64) {
  const raw = strictBase64(rawB64, 32);
  if (raw.length !== 32) throw new Error("public key must decode to 32 bytes");
  const prefix = Buffer.from("302a300506032b6570032100", "hex");
  return createPublicKey({ key: Buffer.concat([prefix, raw]), type: "spki", format: "der" });
}

export function signPayload(privateKey, keyId, payloadBytes) {
  return sign(null, signingMessage(keyId, payloadBytes), privateKey);
}

/** Canonical base64 is checked before decoding to bound allocation. */
export function strictBase64(value, maxBytes) {
  if (typeof value !== "string" || !value.length || value.length > 4 * Math.ceil(maxBytes / 3) ||
      (value.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(value))) throw new Error("invalid base64");
  const bytes = Buffer.from(value, "base64");
  if (bytes.length > maxBytes || bytes.toString("base64") !== value) throw new Error("invalid base64");
  return bytes;
}

export function utcTime(value) {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?Z$/.test(value)) return null;
  const time = Date.parse(value);
  return Number.isFinite(time) && new Date(time).toISOString().slice(0, 19) === value.slice(0, 19) ? time : null;
}

export function verifyEnvelope(envelope, publicKeyB64) {
  const errors = [];
  if (!isPlainObject(envelope)) return { ok: false, errors: ["envelope must be an object"] };
  if (envelope.envelope !== ENVELOPE_VERSION) errors.push("unsupported envelope version");
  if (envelope.alg !== "ed25519") errors.push("unsupported signature algorithm");
  if (typeof envelope.key_id !== "string" || !KEY_ID_RE.test(envelope.key_id)) errors.push("bad key_id");
  if (envelope.schema_version !== SCHEMA_VERSION) errors.push("unsupported schema version");
  if (!Number.isSafeInteger(envelope.facts_version) || envelope.facts_version <= 0) errors.push("facts_version must be a positive safe integer");
  if (typeof envelope.channel !== "string" || !CHANNEL_RE.test(envelope.channel)) errors.push("bad channel");
  if (typeof envelope.applies_to !== "string" || envelope.applies_to.length > 200 || !VERSION_REQ_RE.test(envelope.applies_to)) errors.push("bad applies_to");
  if (utcTime(envelope.published_at) === null || (envelope.not_after != null && utcTime(envelope.not_after) === null)) errors.push("bad timestamp");
  if (typeof envelope.sha256 !== "string" || !/^[a-f0-9]{64}$/.test(envelope.sha256)) errors.push("bad sha256");
  if (!Array.isArray(envelope.sigs) || envelope.sigs.length > 7) errors.push("bad extra signatures");
  else for (const candidate of envelope.sigs) {
    if (!isPlainObject(candidate) || typeof candidate.key_id !== "string" || !KEY_ID_RE.test(candidate.key_id)) { errors.push("bad extra signature"); continue; }
    try { if (strictBase64(candidate.sig_b64, 64).length !== 64) errors.push("bad extra signature size"); }
    catch { errors.push("bad extra signature encoding"); }
  }
  if (errors.length) return { ok: false, errors };
  let payloadBytes, sig, key;
  try {
    payloadBytes = strictBase64(envelope.payload_b64, maxPayloadBytesFor(envelope.channel));
    sig = strictBase64(envelope.sig_b64, 64);
    if (sig.length !== 64) throw new Error("bad signature size");
    key = publicKeyObjectFromRaw(publicKeyB64);
  } catch { return { ok: false, errors: ["invalid payload, signature or public key encoding"] }; }
  const sha = createHash("sha256").update(payloadBytes).digest("hex");
  if (envelope.sha256 !== sha) return { ok: false, errors: ["sha256 mismatch"] };
  if (!verify(null, signingMessage(envelope.key_id, payloadBytes), key, sig)) return { ok: false, errors: ["bad signature"] };
  let payload;
  try { payload = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(payloadBytes)); }
  catch { return { ok: false, errors: ["payload is not UTF-8 JSON"] }; }
  if (envelope.channel === REGISTRY_CHANNEL) {
    errors.push(...validateRegistrySource(payload));
    if (envelope.facts_version !== payload?.revision) errors.push("outer facts_version differs from signed revision");
    if (envelope.published_at !== payload?.generated_at) errors.push("outer published_at differs from signed generated_at");
    if (envelope.applies_to !== (payload?.applies_to ?? "*")) errors.push("outer applies_to differs from signed payload");
    if ((envelope.not_after ?? null) !== (payload?.not_after ?? null)) errors.push("outer not_after differs from signed payload");
    if (payload?.not_after != null && utcTime(payload.not_after) <= utcTime(payload.generated_at)) errors.push("not_after must follow generated_at");
    return errors.length ? { ok: false, errors } : { ok: true, errors: [], payload, sha256: sha };
  }
  errors.push(...validateSource(payload));
  for (const field of ["channel", "facts_version", "applies_to", "schema_version", "published_at"]) {
    if (envelope[field] !== payload?.[field]) errors.push(`outer ${field} differs from signed payload`);
  }
  if ((envelope.not_after ?? null) !== (payload?.not_after ?? null)) errors.push("outer not_after differs from signed payload");
  if (payload?.not_after != null && utcTime(payload.not_after) <= utcTime(payload.published_at)) errors.push("not_after must follow published_at");
  return errors.length ? { ok: false, errors } : { ok: true, errors: [], payload, sha256: sha };
}

// ---------------------------------------------------------------------------
// Source validation (docs/cloud-facts/<channel>.json)
// ---------------------------------------------------------------------------

const MODEL_OPS = new Set(["upsert", "deprecate", "hide"]);
const LEVELS = new Set(["info", "warn"]);
const SURFACES = new Set(["tui", "desktop", "web"]);
const VERSION_REQ_RE = /^(\*|(?:>=|<=|>|<|=|\^|~)?\s*\d+(\.\d+){0,2}(-[0-9A-Za-z.-]+)?(\s*,\s*(?:>=|<=|>|<|=|\^|~)?\s*\d+(\.\d+){0,2}(-[0-9A-Za-z.-]+)?)*)$/;

function isPlainObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function optString(errors, where, obj, key, max = 500) {
  const v = obj[key];
  if (v === undefined || v === null) return;
  if (typeof v !== "string" || v.length > max) errors.push(`${where}.${key} must be a string (<= ${max} chars)`);
}

function optVersionReq(errors, where, obj, key = "applies_to") {
  const v = obj[key];
  if (v === undefined || v === null) return;
  if (typeof v !== "string" || v.length > 200 || !VERSION_REQ_RE.test(v.trim())) errors.push(`${where}.${key} is not a semver requirement: ${JSON.stringify(v)}`);
}

export function validateSource(source) {
  const errors = [];
  if (!isPlainObject(source)) return ["source must be an object"];
  if (source.schema_version !== undefined && source.schema_version !== SCHEMA_VERSION) {
    errors.push(`schema_version must be ${SCHEMA_VERSION}`);
  }
  if (source.channel !== undefined && !CHANNEL_RE.test(String(source.channel))) errors.push("channel slug invalid");
  if (source.facts_version !== undefined && !(Number.isSafeInteger(source.facts_version) && source.facts_version > 0)) {
    errors.push("facts_version must be a positive integer");
  }
  optVersionReq(errors, "root", source);
  optString(errors, "root", source, "not_after", 40);
  for (const field of ["published_at", "not_after"]) {
    if (source[field] != null && utcTime(source[field]) === null) errors.push(`${field} must be a valid UTC timestamp`);
  }
  const models = source.models ?? [];
  if (!Array.isArray(models)) errors.push("models must be an array");
  else {
    models.forEach((m, i) => {
      const where = `models[${i}]`;
      if (!isPlainObject(m)) return errors.push(`${where} must be an object`);
      if (typeof m.provider !== "string" || !m.provider) errors.push(`${where}.provider required`);
      if (typeof m.id !== "string" || !m.id) errors.push(`${where}.id required`);
      if (m.op !== undefined && !MODEL_OPS.has(m.op)) errors.push(`${where}.op must be one of ${[...MODEL_OPS].join("/")}`);
      for (const k of ["context_window", "max_output"]) {
        if (m[k] !== undefined && !(Number.isSafeInteger(m[k]) && m[k] > 0)) errors.push(`${where}.${k} must be a positive integer`);
      }
      if (m.pricing !== undefined) {
        if (!isPlainObject(m.pricing)) errors.push(`${where}.pricing must be an object`);
        else for (const k of Object.keys(m.pricing)) {
          if (!["input_per_m", "output_per_m", "cache_read_per_m"].includes(k)) errors.push(`${where}.pricing.${k} unknown`);
          else if (typeof m.pricing[k] !== "number" || !Number.isFinite(m.pricing[k]) || m.pricing[k] < 0) errors.push(`${where}.pricing.${k} must be a non-negative number`);
        }
      }
      if (m.reasoning !== undefined && typeof m.reasoning !== "boolean") errors.push(`${where}.reasoning must be boolean`);
      // Additive field: older clients deserialize it as false and keep provider
      // roster dominance, so an unsigned or unaware reader loses nothing.
      if (m.allow_unlisted !== undefined) {
        if (typeof m.allow_unlisted !== "boolean") errors.push(`${where}.allow_unlisted must be boolean`);
        else if (m.allow_unlisted) {
          if (m.op !== undefined && m.op !== "upsert") errors.push(`${where}.allow_unlisted requires op upsert`);
          // The client drops the assertion in a payload that cannot expire.
          if (!source.not_after) errors.push(`${where}.allow_unlisted requires a payload not_after`);
        }
      }
      optString(errors, where, m, "display_name", 120);
      optString(errors, where, m, "deprecated_at", 40);
      optString(errors, where, m, "replacement", 200);
      optString(errors, where, m, "note", 300);
      optVersionReq(errors, where, m);
    });
  }
  const defaults = source.provider_defaults ?? {};
  if (!isPlainObject(defaults)) errors.push("provider_defaults must be an object");
  else for (const [provider, d] of Object.entries(defaults)) {
    const where = `provider_defaults.${provider}`;
    if (!isPlainObject(d)) { errors.push(`${where} must be an object`); continue; }
    optString(errors, where, d, "default_model", 200);
    optString(errors, where, d, "base_url", 300);
    if (d.base_url !== undefined) {
      try {
        const url = new URL(d.base_url);
        if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash || /[\\\s]/.test(d.base_url)) throw new Error();
      } catch { errors.push(`${where}.base_url must be an unambiguous credential-free https URL`); }
    }
    optVersionReq(errors, where, d);
  }
  if (source.release !== undefined && source.release !== null) {
    const r = source.release;
    const where = "release";
    if (!isPlainObject(r)) errors.push("release must be an object");
    else {
      if (typeof r.latest !== "string" || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(r.latest)) errors.push("release.latest must be a semver version");
      if (r.yanked !== undefined && !(Array.isArray(r.yanked) && r.yanked.every((v) => typeof v === "string"))) errors.push("release.yanked must be a string array");
      optString(errors, where, r, "min_supported", 40);
      optString(errors, where, r, "notice", 300);
      optString(errors, where, r, "release_url", 300);
      optVersionReq(errors, where, r);
    }
  }
  const ann = source.announcements ?? [];
  if (!Array.isArray(ann)) errors.push("announcements must be an array");
  else {
    const seen = new Set();
    ann.forEach((a, i) => {
      const where = `announcements[${i}]`;
      if (!isPlainObject(a)) return errors.push(`${where} must be an object`);
      if (typeof a.id !== "string" || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(a.id)) errors.push(`${where}.id invalid`);
      if (seen.has(a.id)) errors.push(`${where}.id duplicated`);
      seen.add(a.id);
      if (a.level !== undefined && !LEVELS.has(a.level)) errors.push(`${where}.level must be info|warn`);
      if (typeof a.text !== "string" || !a.text.trim() || a.text.length > 200) errors.push(`${where}.text required (<= 200 chars)`);
      optString(errors, where, a, "url", 300);
      if (a.surfaces !== undefined && !(Array.isArray(a.surfaces) && a.surfaces.every((s) => SURFACES.has(s)))) errors.push(`${where}.surfaces invalid`);
      optVersionReq(errors, where, a);
      optString(errors, where, a, "starts_at", 40);
      optString(errors, where, a, "expires_at", 40);
    });
  }
  const allowed = new Set(["$schema", "_meta", "schema_version", "channel", "facts_version", "published_at", "not_after", "applies_to", "models", "provider_defaults", "release", "announcements"]);
  for (const k of Object.keys(source)) if (!allowed.has(k)) errors.push(`unknown top-level field ${k}`);
  return errors;
}

export function canonicalUtcTimestamp(value) {
  const time = utcTime(value);
  if (time === null) return null;
  return new Date(time).toISOString().replace(/\.000Z$/, "Z");
}

export function validateRegistrySource(source) {
  const errors = [];
  if (!isPlainObject(source)) return ["registry source must be an object"];
  if (source.schema !== REGISTRY_SCHEMA) errors.push(`schema must be ${REGISTRY_SCHEMA}`);
  if (!Number.isSafeInteger(source.revision) || source.revision <= 0) errors.push("revision must be a positive integer");
  if (typeof source.content_hash !== "string" || !/^sha256:[a-f0-9]{64}$/.test(source.content_hash)) {
    errors.push("content_hash must be sha256:<64 hex>");
  }
  const canonicalGenerated = canonicalUtcTimestamp(source.generated_at);
  if (canonicalGenerated === null || source.generated_at !== canonicalGenerated) {
    errors.push("generated_at must be a canonical UTC timestamp");
  }
  if (source.not_after !== undefined && source.not_after !== null) {
    const canonicalNotAfter = canonicalUtcTimestamp(source.not_after);
    if (canonicalNotAfter === null || source.not_after !== canonicalNotAfter) {
      errors.push("not_after must be a canonical UTC timestamp");
    }
  }
  if (!Array.isArray(source.providers)) errors.push("providers must be an array");
  if (!Array.isArray(source.models)) errors.push("models must be an array");
  if (!Array.isArray(source.offerings)) errors.push("offerings must be an array");
  if (!isPlainObject(source.dialects)) errors.push("dialects must be an object");
  optVersionReq(errors, "root", source);
  return errors;
}

export function buildRegistryPayload(source, { factsVersion, publishedAt, notAfter, appliesTo } = {}) {
  if (!isPlainObject(source)) throw new Error("registry source must be an object");
  const revision = factsVersion ?? source.revision;
  const generatedAt = canonicalUtcTimestamp(publishedAt ?? source.generated_at);
  if (!generatedAt) throw new Error("generated_at must be a valid UTC timestamp");
  const payload = {
    ...source,
    schema: REGISTRY_SCHEMA,
    revision,
    generated_at: generatedAt,
  };
  const resolvedAppliesTo = appliesTo ?? source.applies_to;
  if (resolvedAppliesTo !== undefined) payload.applies_to = String(resolvedAppliesTo).trim();
  const resolvedNotAfter = notAfter === undefined ? source.not_after : notAfter;
  if (resolvedNotAfter != null) {
    const canonicalExpiry = canonicalUtcTimestamp(resolvedNotAfter);
    if (!canonicalExpiry) throw new Error("not_after must be a valid UTC timestamp");
    payload.not_after = canonicalExpiry;
  } else {
    delete payload.not_after;
  }
  const errors = validateRegistrySource(payload);
  if (errors.length) throw new Error(`registry invalid:\n  - ${errors.join("\n  - ")}`);
  return payload;
}

/** Build the signed payload object (no signing) from a source file. */
export function buildPayload(source, { channel, factsVersion, publishedAt }) {
  if (channel === REGISTRY_CHANNEL) {
    return buildRegistryPayload(source, { factsVersion, publishedAt });
  }
  const errors = validateSource(source);
  if (errors.length) throw new Error(`source invalid:\n  - ${errors.join("\n  - ")}`);
  const payload = {
    schema_version: SCHEMA_VERSION,
    channel,
    facts_version: factsVersion,
    published_at: publishedAt,
    applies_to: typeof source.applies_to === "string" ? source.applies_to.trim() : "*",
    models: source.models ?? [],
    provider_defaults: source.provider_defaults ?? {},
    release: source.release ?? null,
    announcements: source.announcements ?? [],
  };
  if (source.not_after) payload.not_after = source.not_after;
  const payloadErrors = validateSource(payload);
  if (payloadErrors.length || utcTime(publishedAt) === null || !Number.isSafeInteger(factsVersion) || factsVersion <= 0 || !CHANNEL_RE.test(channel)) {
    throw new Error("invalid signed payload metadata");
  }
  return payload;
}

export function buildEnvelope({ privateKey, keyId, payload }) {
  if (!KEY_ID_RE.test(keyId)) throw new Error(`key_id must match ${KEY_ID_RE}`);
  const isRegistry = payload?.schema === REGISTRY_SCHEMA;
  const channel = isRegistry ? REGISTRY_CHANNEL : payload.channel;
  const maxPayload = maxPayloadBytesFor(channel);
  const payloadBytes = Buffer.from(canonicalize(payload), "utf8");
  if (payloadBytes.length > maxPayload) throw new Error(`payload exceeds ${maxPayload} bytes`);
  const sig = signPayload(privateKey, keyId, payloadBytes);
  const sha256 = createHash("sha256").update(payloadBytes).digest("hex");
  const envelope = {
    envelope: ENVELOPE_VERSION,
    channel,
    facts_version: isRegistry ? payload.revision : payload.facts_version,
    schema_version: isRegistry ? SCHEMA_VERSION : payload.schema_version,
    key_id: keyId,
    alg: "ed25519",
    applies_to: isRegistry ? (typeof payload.applies_to === "string" ? payload.applies_to : "*") : payload.applies_to,
    published_at: isRegistry ? payload.generated_at : payload.published_at,
    payload_b64: payloadBytes.toString("base64"),
    sig_b64: sig.toString("base64"),
    sigs: [],
    sha256,
  };
  if (payload.not_after != null) envelope.not_after = payload.not_after;
  const pub = rawPublicKeyFromKeyObject(createPublicKey(privateKey)).toString("base64");
  const check = verifyEnvelope(envelope, pub);
  if (!check.ok) throw new Error(`self-verify failed: ${check.errors.join("; ")}`);
  return envelope;
}

// ---------------------------------------------------------------------------
// CLI helpers
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg.startsWith("--")) {
      const key = arg.slice(2);
      const next = argv[i + 1];
      if (next === undefined || next.startsWith("--")) flags[key] = true;
      else { flags[key] = next; i += 1; }
    } else positional.push(arg);
  }
  return { positional, flags };
}

/** Bounded, regular, single-link file reads; no symlink or FIFO following. */
export function readBoundedFile(path, maxBytes = MAX_ENVELOPE_BYTES) {
  const before = lstatSync(path);
  if (!before.isFile() || before.nlink !== 1) throw new Error("file is not a regular single-link file");
  const fd = openSync(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0));
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.nlink !== 1 || stat.size > maxBytes || stat.ino !== before.ino || stat.dev !== before.dev) throw new Error("file is not a bounded regular single-link file");
    const bytes = Buffer.alloc(maxBytes + 1);
    let size = 0;
    while (size <= maxBytes) {
      const count = readSync(fd, bytes, size, maxBytes + 1 - size, null);
      if (!count) break;
      size += count;
    }
    if (size > maxBytes) throw new Error("file exceeds size limit");
    return bytes.subarray(0, size);
  } finally { closeSync(fd); }
}

function loadPrivateKeyFromEnv() {
  refuseUnderCi();
  let pem = process.env.CODEWHALE_FACTS_SIGNING_KEY;
  const file = process.env.CODEWHALE_FACTS_SIGNING_KEY_FILE;
  if (!pem && file) pem = readBoundedFile(file, 16 * 1024).toString("utf8");
  if (!pem) throw new Error("set CODEWHALE_FACTS_SIGNING_KEY (PEM) or CODEWHALE_FACTS_SIGNING_KEY_FILE");
  if (Buffer.byteLength(pem) > 16 * 1024) throw new Error("signing key exceeds size limit");
  const key = createPrivateKey({ key: pem, format: "pem" });
  if (key.asymmetricKeyType !== "ed25519") throw new Error("signing key must be Ed25519");
  return key;
}

export function validateTrustedKeys(keys) {
  const seen = new Set();
  for (const key of keys) {
    if (!KEY_ID_RE.test(key.keyId) || seen.has(key.keyId) || !["active", "retired"].includes(key.status) || !["founder", "automation"].includes(key.role) || strictBase64(key.publicKey, 32).length !== 32 || seen.has(key.publicKey)) throw new Error("invalid or duplicated pinned key");
    seen.add(key.keyId);
    seen.add(key.publicKey);
  }
  return keys;
}

/** Deliberately narrow syntax: a changed/unparseable table must fail the gate. */
export function parseTsKeys(text) {
  const source = text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  const tables = [...source.matchAll(/^\s*export\s+const\s+TRUSTED_KEYS\s*:\s*readonly\s+TrustedKey\[\]\s*=\s*\[([\s\S]*?)\]\s*;/gm)];
  if (tables.length !== 1) throw new Error("cannot parse exactly one TypeScript TRUSTED_KEYS table");
  const table = tables[0];
  const body = table[1].replace(/^\s*\/\/.*$/gm, "");
  const keys = [];
  const remainder = body.replace(/\{\s*keyId:\s*"([^"]+)",\s*publicKey:\s*"([^"]+)",\s*status:\s*"([^"]+)",\s*role:\s*"([^"]+)"\s*,?\s*\}/g, (_, keyId, publicKey, status, role) => {
    keys.push({ keyId, publicKey, status, role });
    return "";
  });
  if (remainder.replace(/[\s,]/g, "")) throw new Error("unparsed TypeScript TRUSTED_KEYS entry");
  return validateTrustedKeys(keys);
}

function loadTrustedKeysFromRepo() {
  const keys = parseTsKeys(readBoundedFile(resolve(WEB_ROOT, "lib/cloud-facts/keys.ts"), 64 * 1024).toString("utf8"));
  return new Map(keys.map((key) => [key.keyId, key]));
}

const AUTOMATION_CHANNEL_SUFFIX = "-models";
const AUTOMATION_MAX_VALIDITY_MS = 7 * 24 * 60 * 60 * 1000;
const AUTOMATION_MAX_MODELS = 256;
const AUTOMATION_PAYLOAD_FIELDS = new Set([
  "schema_version", "channel", "facts_version", "published_at", "not_after", "applies_to",
  "models", "provider_defaults", "release", "announcements",
]);
const AUTOMATION_MODEL_FIELDS = new Set([
  "provider", "id", "op", "context_window", "max_output", "reasoning", "display_name",
  "deprecated_at", "replacement", "note", "applies_to", "pricing_withheld",
]);

export function automationScopeViolation(facts, channel) {
  const published = utcTime(facts.published_at);
  const expires = facts.not_after == null ? null : utcTime(facts.not_after);
  if (published === null || (facts.not_after != null && expires === null)) return "timestamp is not whole-second UTC";
  if (!channel.endsWith(AUTOMATION_CHANNEL_SUFFIX) || channel.length <= AUTOMATION_CHANNEL_SUFFIX.length || !/^[a-z0-9]/.test(channel)) return `channel ${JSON.stringify(channel)} is not an automation channel`;
  const plain = (value, max) => value === undefined || (typeof value === "string" && [...value].length <= max && !/\p{Cc}/u.test(value));
  if (!plain(facts.applies_to, 200)) return "applies_to is too long or not plain";
  if (isPlainObject(facts.provider_defaults) && Object.keys(facts.provider_defaults).length > 0) return "provider defaults";
  if (facts.release != null) return "release facts";
  if (Array.isArray(facts.announcements) && facts.announcements.length > 0) return "announcements";
  const unknown = Object.keys(facts).find((field) => !AUTOMATION_PAYLOAD_FIELDS.has(field));
  if (unknown !== undefined) return `field ${JSON.stringify(unknown)} is not allowed`;
  const models = facts.models === undefined ? [] : facts.models;
  if (!Array.isArray(models)) return "models is not a list";
  if (models.length > AUTOMATION_MAX_MODELS) return `more than ${AUTOMATION_MAX_MODELS} models`;
  for (const model of models) {
    if (!isPlainObject(model) || typeof model.provider !== "string" || !model.provider || typeof model.id !== "string" || !model.id) return "malformed model entry";
    const name = `${model.provider}/${model.id}`;
    const field = Object.keys(model).find((key) => !AUTOMATION_MODEL_FIELDS.has(key));
    if (field !== undefined) return `${name}: field ${JSON.stringify(field)} is not allowed`;
    const nullField = Object.keys(model).find((key) => model[key] === null);
    if (nullField !== undefined) return `${name}: field ${JSON.stringify(nullField)} is null`;
    const op = model.op === undefined ? "upsert" : model.op;
    if (op !== "upsert" && op !== "deprecate") return `${name} is not an upsert or deprecation`;
    if (model.reasoning !== undefined && typeof model.reasoning !== "boolean") return `${name}: malformed model entry`;
    if (!plain(model.provider, 64) || !plain(model.id, 200) || !plain(model.display_name, 120) || !plain(model.deprecated_at, 40) ||
        !plain(model.replacement, 200) || !plain(model.note, 300) || !plain(model.applies_to, 200) || !plain(model.pricing_withheld, 300)) return `${name}: text too long or not plain`;
    const window = model.context_window;
    const output = model.max_output;
    const windowOk = window === undefined || (typeof window === "number" && Number.isSafeInteger(window) && window >= 1024 && window <= 20_000_000);
    const outputOk = output === undefined || (typeof output === "number" && Number.isSafeInteger(output) && output >= 1 && output <= 2_000_000 && (typeof window !== "number" || output <= window));
    if (!windowOk || !outputOk) return `${name}: limit out of range`;
  }
  const wholeSecond = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;
  if (typeof facts.published_at !== "string" || !wholeSecond.test(facts.published_at) ||
      (facts.not_after != null && (typeof facts.not_after !== "string" || !wholeSecond.test(facts.not_after)))) return "timestamp is not whole-second UTC";
  if (facts.facts_version !== published / 1000) return "facts_version is not the publication time";
  if (expires === null) return "no not_after";
  if (expires <= published || expires - published > AUTOMATION_MAX_VALIDITY_MS) return "validity outside the automation limit";
  return null;
}

const REGISTRY_PROVIDER_TRUST_FIELDS = [
  "origin", "kind", "base_url", "base_url_config_key", "env_vars", "api_key_env",
  "secret_store_slot", "config_key", "wire", "wire_policy", "auth", "aliases",
  "selector_aliases", "subagent_aliases", "credential_help", "credential_url",
  "docs_url", "discovery", "catalog_id", "catalog_source_id", "tui_wire_tag",
  "table_field", "web",
];

function sortKeysDeep(value) {
  if (Array.isArray(value)) return value.map(sortKeysDeep);
  if (!isPlainObject(value)) return value;
  const out = {};
  for (const key of Object.keys(value).sort()) {
    if (value[key] !== undefined) out[key] = sortKeysDeep(value[key]);
  }
  return out;
}

function projectProviderTrust(row) {
  const synthesized = row.origin === "descriptor" ? { ...row } : row;
  if (row.origin === "descriptor") {
    if (synthesized.wire_policy === undefined) {
      synthesized.wire_policy = typeof synthesized.wire === "string" && synthesized.wire ? synthesized.wire : "openai-compatible";
    }
    if (synthesized.auth === undefined) {
      const env = typeof synthesized.api_key_env === "string" && synthesized.api_key_env
        ? synthesized.api_key_env
        : `${String(synthesized.id).toUpperCase().replace(/-/g, "_")}_API_KEY`;
      synthesized.auth = { secret_slot: synthesized.id, env_vars: [env] };
    }
  }
  const out = {};
  for (const field of REGISTRY_PROVIDER_TRUST_FIELDS) {
    if (synthesized[field] !== undefined) out[field] = synthesized[field];
  }
  return sortKeysDeep(out);
}

export function deriveBundledRegistryTrust(descriptors) {
  const providers = {};
  for (const row of descriptors.providers || []) providers[row.id] = projectProviderTrust({ ...row, origin: "builtin" });
  for (const row of descriptors.descriptors || []) providers[row.id] = projectProviderTrust({ ...row, origin: "descriptor" });
  const compat = {};
  for (const [key, value] of Object.entries(descriptors)) {
    if (key !== "providers" && key !== "descriptors") compat[key] = value;
  }
  return sortKeysDeep({ providers, providerDescriptorsCompat: compat });
}

let cachedBundledRegistryTrust = null;
function loadBundledRegistryTrust() {
  if (!cachedBundledRegistryTrust) {
    cachedBundledRegistryTrust = readJson(resolve(WEB_ROOT, "lib/cloud-facts/bundled-providers.json"), 512 * 1024);
  }
  return cachedBundledRegistryTrust;
}

export function registryAutomationScopeViolation(facts, bundledTrust = loadBundledRegistryTrust()) {
  const published = utcTime(facts.generated_at);
  const expires = facts.not_after == null ? null : utcTime(facts.not_after);
  if (published === null || (facts.not_after != null && expires === null)) return "timestamp is not valid UTC";
  if (expires === null) return "no not_after";
  if (expires <= published || expires - published > AUTOMATION_MAX_VALIDITY_MS) return "validity outside the automation limit";
  if (!Array.isArray(facts.providers)) return "providers is not a list";
  const bundledProviders = bundledTrust?.providers ?? {};
  const bundledIds = Object.keys(bundledProviders);
  const seen = new Set();
  for (const provider of facts.providers) {
    if (!isPlainObject(provider) || typeof provider.id !== "string" || !provider.id) return "malformed provider entry";
    if (seen.has(provider.id)) return `${provider.id}: duplicate provider`;
    seen.add(provider.id);
    if (provider.origin !== "builtin" && provider.origin !== "descriptor") return `${provider.id}: unsupported provider origin`;
    if (provider.admission !== undefined) return `${provider.id}: admitted provider requires a founder signature`;
    if (!(provider.id in bundledProviders)) return `${provider.id}: unbundled provider requires a founder signature`;
    if (canonicalize(projectProviderTrust(provider)) !== canonicalize(bundledProviders[provider.id])) {
      return `${provider.id}: provider trust fields require a founder signature`;
    }
  }
  if (seen.size !== bundledIds.length) return "missing bundled provider requires a founder signature";
  if (isPlainObject(facts.engine_compat) && facts.engine_compat.provider_descriptors !== undefined) {
    if (canonicalize(facts.engine_compat.provider_descriptors) !== canonicalize(bundledTrust?.providerDescriptorsCompat ?? {})) {
      return "engine_compat.provider_descriptors requires a founder signature";
    }
  }
  return null;
}

export function activePublishingKey(envelope, keys, now = Date.now()) {
  const key = validateTrustedKeys(keys).find((key) => key.keyId === envelope.key_id && key.status === "active");
  if (!key) throw new Error("primary signing key is not pinned and active; refusing publication");
  const check = verifyEnvelope(envelope, key.publicKey);
  if (!check.ok) throw new Error(`envelope does not verify: ${check.errors.join("; ")}`);
  if (key.role !== "founder") {
    const violation = envelope.channel === REGISTRY_CHANNEL
      ? registryAutomationScopeViolation(check.payload)
      : automationScopeViolation(check.payload, envelope.channel);
    if (violation !== null) throw new Error(`payload is outside the automation key's scope: ${violation}`);
  }
  const publishedTime = envelope.channel === REGISTRY_CHANNEL
    ? utcTime(check.payload.generated_at)
    : utcTime(check.payload.published_at);
  if (!Number.isFinite(now) || publishedTime > now + 300_000 ||
      (check.payload.not_after != null && utcTime(check.payload.not_after) <= now)) throw new Error("publication timestamp is future or expired");
  return { key, check };
}

function refuseUnderCi() {
  for (const marker of CI_MARKERS) {
    if (process.env[marker] && !/^(0|false|no|off)$/i.test(process.env[marker])) {
      throw new Error(`refusing to run with a secret under CI (${marker} is set); publish from the founder's machine`);
    }
  }
}

function sqlLiteral(value) {
  if (value === null || value === undefined) return "null";
  return `'${String(value).replace(/'/g, "''")}'`;
}

export function emitSql(envelope, { publishedBy = "", publicKeyB64, notes = "" }) {
  if (!publicKeyB64) throw new Error("public key required to emit the facts_key row");
  const check = verifyEnvelope(envelope, publicKeyB64);
  if (!check.ok) throw new Error(`envelope does not verify: ${check.errors.join("; ")}`);
  const payloadJson = Buffer.from(envelope.payload_b64, "base64").toString("utf8");
  return [
    "begin;",
    `insert into public.facts_key (key_id, scope, algorithm, public_key, status)`,
    `  values (${sqlLiteral(envelope.key_id)}, 'global', 'ed25519', ${sqlLiteral(publicKeyB64)}, 'active')`,
    `  on conflict (key_id) do nothing;`,
    `insert into public.facts_release (channel_id, facts_version, schema_version, envelope_version, applies_to, key_id, payload_b64, sig_b64, sigs, payload, published_at, not_after, published_by, notes)`,
    `  select c.id, ${envelope.facts_version}, ${envelope.schema_version}, ${envelope.envelope}, ${sqlLiteral(envelope.applies_to)}, ${sqlLiteral(envelope.key_id)},`,
    `         ${sqlLiteral(envelope.payload_b64)}, ${sqlLiteral(envelope.sig_b64)}, ${sqlLiteral(JSON.stringify(envelope.sigs ?? []))}::jsonb,`,
    `         ${sqlLiteral(payloadJson)}::jsonb, ${sqlLiteral(envelope.published_at)}::timestamptz, ${sqlLiteral(check.payload.not_after ?? null)}::timestamptz,`,
    `         ${sqlLiteral(publishedBy)}, ${sqlLiteral(notes)}`,
    `    from public.facts_channel c where c.scope = 'global' and c.slug = ${sqlLiteral(envelope.channel)};`,
    "commit;",
    "",
  ].join("\n");
}

async function postgrest(path, { method = "GET", body, prefer } = {}) {
  refuseUnderCi();
  const url = process.env.SUPABASE_URL;
  const key = process.env.SUPABASE_SERVICE_ROLE_KEY || process.env.SUPABASE_SECRET_KEY;
  if (!url || !key) throw new Error("SUPABASE_URL and SUPABASE_SERVICE_ROLE_KEY are required");
  const endpoint = new URL(url);
  if (endpoint.protocol !== "https:" || endpoint.username || endpoint.password || endpoint.search || endpoint.hash) throw new Error("invalid Supabase endpoint");
  const res = await fetch(`${url.replace(/\/$/, "")}/rest/v1/${path}`, {
    method,
    signal: AbortSignal.timeout(30_000),
    redirect: "error",
    headers: {
      apikey: key,
      Authorization: `Bearer ${key}`,
      "Content-Type": "application/json",
      ...(prefer ? { Prefer: prefer } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) { await res.body?.cancel(); throw Object.assign(new Error(`PostgREST request failed (HTTP ${res.status})`), { status: res.status }); }
  const text = await readBoundedResponse(res);
  return text ? JSON.parse(text) : null;
}

/**
 * Insert one signed release and prove what landed. A re-run of the same
 * envelope (the first run's reply was lost) reports the stored row instead of
 * failing on the version high-water mark; different bytes at that version
 * stay an error. `request` is postgrest() or a test double.
 */
export async function publishRelease(request, envelope, row, publicKey) {
  const channels = await request(`facts_channel?scope=eq.global&slug=eq.${encodeURIComponent(envelope.channel)}&select=id`);
  if (channels?.length !== 1) throw new Error(`channel ${envelope.channel} does not exist`);
  const channelId = channels[0].id;
  await request("facts_key", { method: "POST", body: { key_id: envelope.key_id, scope: "global", algorithm: "ed25519", public_key: publicKey, status: "active" }, prefer: "resolution=ignore-duplicates,return=minimal" });
  let inserted;
  try {
    inserted = await request("facts_release?select=id,payload_sha256", { method: "POST", body: { ...row, channel_id: channelId }, prefer: "return=representation" });
  } catch (error) {
    if (error?.status !== 409) throw error;
    const existing = await request(`facts_release?channel_id=eq.${channelId}&facts_version=eq.${envelope.facts_version}&select=id,payload_sha256,status`);
    if (existing?.length === 1 && existing[0].payload_sha256 === envelope.sha256) {
      return { published: false, already_published: true, channel: envelope.channel, facts_version: envelope.facts_version, release_id: existing[0].id, payload_sha256: existing[0].payload_sha256, status: existing[0].status };
    }
    throw new Error(`facts_version ${envelope.facts_version} is at or below channel ${envelope.channel}'s high-water mark and is not this envelope; publish a higher facts_version`);
  }
  if (inserted?.length !== 1 || inserted[0].payload_sha256 !== envelope.sha256) {
    throw new Error(`facts_release insert did not return exactly this envelope (rows: ${inserted?.length ?? 0}); inspect channel ${envelope.channel} before retrying`);
  }
  return { published: true, channel: envelope.channel, facts_version: envelope.facts_version, release_id: inserted[0].id, payload_sha256: inserted[0].payload_sha256 };
}

/**
 * Revoke one published release. Only a `published` row flips, so a re-run
 * keeps the first revocation's time and reason; a version that matches
 * nothing is an error, never a silent `revoked: 0`. `head` says whether the
 * channel now serves nothing (revoking an older version changes no delivery).
 */
export async function revokeRelease(request, { channel, version, reason, at }) {
  const channels = await request(`facts_channel?scope=eq.global&slug=eq.${encodeURIComponent(channel)}&select=id,max_facts_version`);
  if (channels?.length !== 1) throw new Error(`channel ${channel} does not exist`);
  const head = Number(channels[0].max_facts_version) === version;
  const release = `facts_release?channel_id=eq.${channels[0].id}&facts_version=eq.${version}`;
  const updated = await request(`${release}&status=eq.published`, {
    method: "PATCH",
    body: { status: "revoked", revoked_at: at, revoke_reason: reason },
    prefer: "return=representation",
  });
  if (updated?.length === 1) return { revoked: 1, already_revoked: false, head, channel, facts_version: version };
  if ((updated?.length ?? 0) > 1) throw new Error(`revoke matched ${updated.length} releases for ${channel} v${version}; inspect the channel`);
  const existing = await request(`${release}&select=status,revoked_at,revoke_reason`);
  if (existing?.length === 1 && existing[0].status === "revoked") {
    return { revoked: 0, already_revoked: true, head, channel, facts_version: version, revoked_at: existing[0].revoked_at, revoke_reason: existing[0].revoke_reason };
  }
  throw new Error(`channel ${channel} has no published facts_version ${version} to revoke`);
}

export async function readBoundedResponse(response, maxBytes = MAX_ENVELOPE_BYTES) {
  const length = response.headers.get("content-length");
  if (length !== null && (!/^\d+$/.test(length) || Number(length) > maxBytes)) {
    await response.body?.cancel();
    throw new Error("response exceeds size limit or has invalid length");
  }
  if (!response.body) return "";
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maxBytes) throw new Error("response exceeds size limit");
      chunks.push(value);
    }
  } catch (error) { try { await reader.cancel(); } catch { /* Keep rejection. */ } throw error; }
  finally { reader.releaseLock(); }
  return new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks, size));
}

function readJson(path, maxBytes = MAX_REGISTRY_ENVELOPE_BYTES) {
  return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(readBoundedFile(path, maxBytes)));
}

function nowIso() {
  return new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
}

async function main(argv) {
  const { positional, flags } = parseArgs(argv);
  const cmd = positional[0];
  if (!cmd || flags.help) {
    console.log(readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n").slice(1, 26).join("\n"));
    return 0;
  }
  if (cmd === "keygen") {
    const keyId = String(flags["key-id"] ?? "");
    if (!KEY_ID_RE.test(keyId)) throw new Error("--key-id must match cwf-[a-z0-9-]{1,32}");
    const out = flags.out ? resolve(String(flags.out)) : null;
    if (!out) throw new Error("--out <path> is required (write the private key OUTSIDE any repository)");
    refuseUnderCi();
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    mkdirSync(dirname(out), { recursive: true, mode: 0o700 });
    const fd = openSync(out, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o600);
    try { writeFileSync(fd, privateKey.export({ type: "pkcs8", format: "pem" })); }
    finally { closeSync(fd); }
    const raw = rawPublicKeyFromKeyObject(publicKey);
    console.log(JSON.stringify({
      key_id: keyId,
      algorithm: "ed25519",
      public_key_b64: raw.toString("base64"),
      public_key_bytes: [...raw],
      private_key_file: out,
      note: "Private key written with mode 0600. Move it into custody (password manager); never commit it.",
    }, null, 2));
    return 0;
  }
  if (cmd === "sign") {
    refuseUnderCi();
    const sourcePath = resolve(String(flags.source ?? resolve(REPO_ROOT, "docs/cloud-facts/stable.json")));
    const source = readJson(sourcePath);
    const defaultChannel = source?.schema === REGISTRY_SCHEMA ? REGISTRY_CHANNEL : (source.channel ?? "stable");
    const channel = String(flags.channel ?? defaultChannel);
    if (!CHANNEL_RE.test(channel)) throw new Error("bad channel slug");
    const factsVersion = Number(flags["facts-version"] ?? (channel === REGISTRY_CHANNEL ? source.revision : source.facts_version));
    if (!Number.isSafeInteger(factsVersion) || factsVersion <= 0) throw new Error("--facts-version (or source.facts_version) must be a positive integer");
    const keyId = String(flags["key-id"] ?? "");
    const privateKey = loadPrivateKeyFromEnv();
    const publishedAt = String(flags["published-at"] ?? (channel === REGISTRY_CHANNEL && source.generated_at ? canonicalUtcTimestamp(source.generated_at) : nowIso()));
    const payload = channel === REGISTRY_CHANNEL
      ? buildRegistryPayload(source, { factsVersion, publishedAt, notAfter: flags["not-after"] })
      : buildPayload(source, { channel, factsVersion, publishedAt });
    const envelope = buildEnvelope({ privateKey, keyId, payload });
    const text = `${JSON.stringify(envelope, null, 2)}\n`;
    if (flags.out) {
      writeFileSync(resolve(String(flags.out)), text);
      console.error(`wrote ${flags.out} (channel=${channel} facts_version=${factsVersion} key_id=${keyId} sha256=${envelope.sha256})`);
    } else process.stdout.write(text);
    return 0;
  }
  if (cmd === "verify") {
    const envelope = readJson(resolve(String(positional[1] ?? "")));
    let pub = flags["public-key"];
    let trusted;
    if (!pub) {
      trusted = loadTrustedKeysFromRepo().get(envelope.key_id);
      if (!trusted || trusted.status !== "active") throw new Error("key is not pinned and active; use --public-key only for explicit offline verification");
      pub = trusted.publicKey;
    }
    const result = verifyEnvelope(envelope, String(pub));
    if (result.ok && trusted && trusted.role !== "founder") {
      const violation = envelope.channel === REGISTRY_CHANNEL
        ? registryAutomationScopeViolation(result.payload)
        : automationScopeViolation(result.payload, envelope.channel);
      if (violation !== null) {
        result.ok = false;
        result.errors = [`payload is outside the automation key's scope: ${violation}`];
      }
    }
    console.log(JSON.stringify({ ok: result.ok, errors: result.errors, channel: envelope.channel, facts_version: envelope.facts_version, key_id: envelope.key_id, sha256: result.sha256 ?? null }, null, 2));
    return result.ok ? 0 : 1;
  }
  if (cmd === "emit-sql") {
    const envelope = readJson(resolve(String(positional[1] ?? "")));
    let pub = flags["public-key"];
    if (!pub) pub = activePublishingKey(envelope, [...loadTrustedKeysFromRepo().values()]).key.publicKey;
    process.stdout.write(emitSql(envelope, { publishedBy: String(flags["published-by"] ?? ""), publicKeyB64: pub ? String(pub) : undefined, notes: String(flags.notes ?? "") }));
    return 0;
  }
  if (cmd === "publish") {
    const envelope = readJson(resolve(String(positional[1] ?? "")));
    if (flags["public-key"] !== undefined) throw new Error("--public-key is only for offline verify/emit-sql; publication requires the active pinned table");
    const { key, check } = activePublishingKey(envelope, [...loadTrustedKeysFromRepo().values()]);
    const pub = key.publicKey;
    const row = {
      facts_version: envelope.facts_version,
      schema_version: envelope.schema_version,
      envelope_version: envelope.envelope,
      applies_to: envelope.applies_to,
      key_id: envelope.key_id,
      payload_b64: envelope.payload_b64,
      sig_b64: envelope.sig_b64,
      sigs: envelope.sigs ?? [],
      payload: check.payload,
      published_at: envelope.published_at,
      not_after: check.payload.not_after ?? null,
      published_by: String(flags["published-by"] ?? ""),
      notes: String(flags.notes ?? ""),
    };
    if (flags["dry-run"]) {
      console.log(JSON.stringify({ dry_run: true, channel: envelope.channel, facts_key: { key_id: envelope.key_id, public_key: pub }, facts_release: { ...row, payload_b64: `<${envelope.payload_b64.length} chars>` } }, null, 2));
      return 0;
    }
    console.log(JSON.stringify(await publishRelease(postgrest, envelope, row, pub), null, 2));
    return 0;
  }
  if (cmd === "revoke") {
    const channel = String(flags.channel ?? "");
    const version = Number(flags.version);
    const reason = String(flags.reason ?? "");
    if (!CHANNEL_RE.test(channel) || !Number.isSafeInteger(version) || version <= 0 || !reason) throw new Error("--channel, --version and --reason are required");
    if (flags["dry-run"]) {
      console.log(JSON.stringify({ dry_run: true, channel, facts_version: version, status: "revoked", revoke_reason: reason }, null, 2));
      return 0;
    }
    console.log(JSON.stringify(await revokeRelease(postgrest, { channel, version, reason, at: nowIso() }), null, 2));
    return 0;
  }
  throw new Error(`unknown command ${cmd}`);
}

const invokedDirectly = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) {
  main(process.argv.slice(2)).then((code) => process.exit(code)).catch((err) => {
    console.error(`facts-publish: ${err.message}`);
    process.exit(1);
  });
}
