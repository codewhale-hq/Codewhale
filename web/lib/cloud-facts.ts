/** Verified facts/v1 delivery. Supabase and KV are untrusted transports; only
 * active, pinned public keys authenticate the exact payload bytes. */
import { DOMAIN, MAX_PAYLOAD_BYTES, TRUSTED_KEYS, type TrustedKey } from "./cloud-facts/keys";
import { readBoundedBody } from "./bounded-body";
import { readBoundedKvJson, type KVStreamNamespace } from "./kv";

export const CHANNEL_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
export const KV_PREFIX = "facts:cloud:";
export const SUPABASE_TIMEOUT_MS = 3000;
export const MAX_ENVELOPE_BYTES = 768 * 1024;
// The last-good copy only bridges a transport outage. It lives no longer than
// a client keeps applying its own cached envelope (payload ttl_secs, 6 h by
// default, docs/CLOUD_FACTS.md), so an outage cannot extend a withdrawn
// release beyond the staleness every client already accepts.
const KV_TTL_SECS = 60 * 60 * 6;
const KEY_ID_RE = /^cwf-[a-z0-9-]{1,32}$/;
const VERSION_REQ_RE = /^(\*|(?:>=|<=|>|<|=|\^|~)?\s*\d+(\.\d+){0,2}(-[0-9A-Za-z.-]+)?(\s*,\s*(?:>=|<=|>|<|=|\^|~)?\s*\d+(\.\d+){0,2}(-[0-9A-Za-z.-]+)?)*)$/;
const MAX_SIGNATURES = 7; // Plus the primary signature: eight candidates total.
const CLOCK_SKEW_MS = 5 * 60 * 1000;

export interface FactsCurrentRow {
  channel: string;
  release_id: string;
  facts_version: number;
  schema_version: number;
  envelope_version: number;
  applies_to: string;
  key_id: string;
  payload_b64: string;
  sig_b64: string;
  sigs: { key_id: string; sig_b64: string }[] | null;
  payload_sha256: string;
  published_at: string;
  not_after: string | null;
}

export interface CloudFactsEnvelope {
  envelope: number;
  channel: string;
  facts_version: number;
  schema_version: number;
  key_id: string;
  alg: "ed25519";
  applies_to: string;
  published_at: string;
  not_after?: string | null;
  payload_b64: string;
  sig_b64: string;
  sigs: { key_id: string; sig_b64: string }[];
  sha256: string;
}

export interface CloudFactsEnv {
  SUPABASE_URL?: string;
  SUPABASE_PUBLISHABLE_KEY?: string;
  CURATED_KV?: KVStreamNamespace;
}

type Rejection = "no-active-keys" | "unknown-key" | "retired-key" | "bad-signature" |
  "bad-envelope" | "bad-payload" | "sha-mismatch" | "wrong-channel" | "expired";
export type Verification =
  | { ok: true; keyId: string; mode: "verified" }
  | { ok: false; reason: Rejection };

export type CloudFactsResult =
  | {
      kind: "ok";
      envelope: CloudFactsEnvelope;
      body: string;
      etag: string;
      source: "supabase" | "kv-stale";
      verified: "verified";
      keyId: string;
    }
  | { kind: "none" }
  | { kind: "sha-mismatch"; channel: string; factsVersion: number }
  | { kind: "unverifiable"; reason: string; channel: string; factsVersion: number }
  | { kind: "unavailable"; reason: string };

export interface ResolveOptions {
  fetchImpl?: typeof fetch;
  keys?: readonly TrustedKey[];
  timeoutMs?: number;
  now?: () => number;
}

export function isValidChannel(slug: string): boolean {
  return CHANNEL_RE.test(slug);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Reject noncanonical/oversized encodings before either decoder allocates. */
function b64ToBytes(value: unknown, maxBytes: number): Uint8Array {
  if (typeof value !== "string" || !value.length || value.length > 4 * Math.ceil(maxBytes / 3) ||
      (value.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(value))) {
    throw new Error("invalid-base64");
  }
  const bin = atob(value);
  if (bin.length > maxBytes || btoa(bin) !== value) throw new Error("invalid-base64");
  return Uint8Array.from(bin, (char) => char.charCodeAt(0));
}

export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as BufferSource);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function signingMessage(keyId: string, payload: Uint8Array): Uint8Array {
  const prefix = new TextEncoder().encode(`${DOMAIN}${keyId}\0`);
  const out = new Uint8Array(prefix.length + payload.length);
  out.set(prefix);
  out.set(payload, prefix.length);
  return out;
}

function hasActiveKeys(keys: readonly TrustedKey[]): boolean {
  const ids = new Set<string>();
  try {
    for (const key of keys) {
      if (!KEY_ID_RE.test(key.keyId) || ids.has(key.keyId) ||
          !["active", "retired"].includes(key.status) || b64ToBytes(key.publicKey, 32).length !== 32) return false;
      ids.add(key.keyId);
    }
    return keys.some((key) => key.status === "active");
  } catch { return false; }
}

function utcTime(value: unknown): number | null {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?Z$/.test(value)) return null;
  const time = Date.parse(value);
  // Date.parse normalizes invalid civil dates such as February 30.
  return Number.isFinite(time) && new Date(time).toISOString().slice(0, 19) === value.slice(0, 19) ? time : null;
}

function validSignature(value: unknown): boolean {
  if (!isObject(value) || typeof value.key_id !== "string" || !KEY_ID_RE.test(value.key_id)) return false;
  try { return b64ToBytes(value.sig_b64, 64).length === 64; } catch { return false; }
}

function isEnvelope(value: unknown): value is CloudFactsEnvelope {
  if (!isObject(value)) return false;
  return value.envelope === 1 && value.alg === "ed25519" && value.schema_version === 1 &&
    typeof value.channel === "string" && isValidChannel(value.channel) &&
    Number.isSafeInteger(value.facts_version) && Number(value.facts_version) > 0 &&
    typeof value.applies_to === "string" && value.applies_to.length <= 200 && VERSION_REQ_RE.test(value.applies_to) &&
    utcTime(value.published_at) !== null && (value.not_after == null || utcTime(value.not_after) !== null) &&
    typeof value.sha256 === "string" && /^[a-f0-9]{64}$/.test(value.sha256) &&
    typeof value.payload_b64 === "string" && validSignature(value) &&
    Array.isArray(value.sigs) && value.sigs.length <= MAX_SIGNATURES && value.sigs.every(validSignature);
}

function rowTimestamp(value: string): string {
  // PostgREST emits timestamptz as +00:00; the signed contract uses UTC Z.
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) return value;
  const time = Date.parse(value);
  return Number.isFinite(time) ? new Date(time).toISOString().replace(/\.000Z$/, "Z") : value;
}

export function envelopeFromRow(row: FactsCurrentRow): CloudFactsEnvelope {
  return {
    envelope: row.envelope_version,
    channel: row.channel,
    facts_version: row.facts_version,
    schema_version: row.schema_version,
    key_id: row.key_id,
    alg: "ed25519",
    applies_to: row.applies_to,
    published_at: rowTimestamp(row.published_at),
    not_after: row.not_after == null ? null : rowTimestamp(row.not_after),
    payload_b64: row.payload_b64,
    sig_b64: row.sig_b64,
    sigs: row.sigs === null ? [] : row.sigs,
    sha256: row.payload_sha256,
  };
}

/** A strong validator covers signatures and every other byte of the response. */
export async function etagFor(envelope: CloudFactsEnvelope): Promise<string> {
  return `"${await sha256Hex(new TextEncoder().encode(JSON.stringify(envelope)))}"`;
}

/** Authenticate first, then validate signed metadata. A channel serves all
 * client versions; each client evaluates the validated applicability range. */
export async function verifyEnvelope(
  value: unknown,
  keys: readonly TrustedKey[] = TRUSTED_KEYS,
  opts: { channel?: string; now?: number } = {},
): Promise<Verification> {
  if (!hasActiveKeys(keys)) return { ok: false, reason: "no-active-keys" };
  if (!isEnvelope(value)) return { ok: false, reason: "bad-envelope" };
  const envelope = value;
  let payload: Uint8Array;
  try { payload = b64ToBytes(envelope.payload_b64, MAX_PAYLOAD_BYTES); }
  catch { return { ok: false, reason: "bad-envelope" }; }
  let keyId: string | undefined;
  let sawKnown = false;
  let sawRetired = false;
  for (const candidate of [envelope, ...envelope.sigs]) {
    const key = keys.find((key) => key.keyId === candidate.key_id);
    if (!key) continue;
    if (key.status !== "active") { sawRetired = true; continue; }
    sawKnown = true;
    try {
      const publicKey = await crypto.subtle.importKey("raw", b64ToBytes(key.publicKey, 32) as BufferSource, { name: "Ed25519" }, false, ["verify"]);
      if (await crypto.subtle.verify({ name: "Ed25519" }, publicKey, b64ToBytes(candidate.sig_b64, 64) as BufferSource, signingMessage(candidate.key_id, payload) as BufferSource)) {
        keyId = candidate.key_id;
        break;
      }
    } catch { /* An invalid candidate cannot authenticate the payload. */ }
  }
  if (!keyId) return { ok: false, reason: sawKnown ? "bad-signature" : sawRetired ? "retired-key" : "unknown-key" };
  if (await sha256Hex(payload) !== envelope.sha256) return { ok: false, reason: "sha-mismatch" };
  let facts: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(payload));
    if (!isObject(parsed)) return { ok: false, reason: "bad-payload" };
    facts = parsed;
  } catch { return { ok: false, reason: "bad-payload" }; }
  for (const field of ["channel", "facts_version", "schema_version", "applies_to"] as const) {
    if (facts[field] !== envelope[field]) return { ok: false, reason: "bad-payload" };
  }
  if (utcTime(facts.published_at) === null || utcTime(facts.published_at) !== utcTime(envelope.published_at) ||
      (facts.not_after != null && utcTime(facts.not_after) === null) ||
      (facts.not_after == null ? null : utcTime(facts.not_after)) !== (envelope.not_after == null ? null : utcTime(envelope.not_after)) ||
      (facts.models !== undefined && !Array.isArray(facts.models)) ||
      (facts.provider_defaults !== undefined && !isObject(facts.provider_defaults)) ||
      (facts.announcements !== undefined && !Array.isArray(facts.announcements)) ||
      (facts.release != null && !isObject(facts.release))) return { ok: false, reason: "bad-payload" };
  if (opts.channel !== undefined && envelope.channel !== opts.channel) return { ok: false, reason: "wrong-channel" };
  const now = opts.now ?? Date.now();
  const published = utcTime(facts.published_at)!;
  const expires = facts.not_after == null ? null : utcTime(facts.not_after);
  if (!Number.isFinite(now) || published > now + CLOCK_SKEW_MS ||
      (expires !== null && expires <= published)) return { ok: false, reason: "bad-payload" };
  if (expires !== null && now >= expires) return { ok: false, reason: "expired" };
  return { ok: true, keyId, mode: "verified" };
}

/** Only publishable keys (or legacy anon JWTs), never secret/service-role keys. */
function isPublishableKey(key: string): boolean {
  if (/^sb_publishable_[A-Za-z0-9_-]+$/.test(key)) return true;
  if (key.length > 8192) return false;
  try {
    const parts = key.split(".");
    if (parts.length !== 3) return false;
    const middle = parts[1].replace(/-/g, "+").replace(/_/g, "/");
    const payload = JSON.parse(atob(middle.padEnd(Math.ceil(middle.length / 4) * 4, "=")));
    return isObject(payload) && payload.role === "anon";
  } catch { return false; }
}

export async function fetchCurrentRow(channel: string, env: CloudFactsEnv, opts: ResolveOptions = {}): Promise<FactsCurrentRow | null> {
  if (!isValidChannel(channel)) throw new Error("invalid-channel");
  const key = env.SUPABASE_PUBLISHABLE_KEY;
  let base: URL;
  try {
    base = new URL(env.SUPABASE_URL ?? "");
    if (base.protocol !== "https:" || base.username || base.password || base.search || base.hash || !key || !isPublishableKey(key)) throw new Error();
  } catch { throw new Error("supabase-not-configured"); }
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), opts.timeoutMs ?? SUPABASE_TIMEOUT_MS);
  try {
    const url = new URL(`${base.href.replace(/\/+$/, "")}/rest/v1/facts_current`);
    url.search = new URLSearchParams({ channel: `eq.${channel}`, scope: "eq.global", select: "channel,release_id,facts_version,schema_version,envelope_version,applies_to,key_id,payload_b64,sig_b64,sigs,payload_sha256,published_at,not_after", limit: "1" }).toString();
    const res = await (opts.fetchImpl ?? fetch)(url, {
      headers: { apikey: key!, Authorization: `Bearer ${key}`, Accept: "application/json" },
      signal: controller.signal,
      redirect: "error",
    });
    if (!res.ok) { await res.body?.cancel(); throw new Error(`supabase-http-${res.status}`); }
    const bytes = await readBoundedBody(res, MAX_ENVELOPE_BYTES);
    const rows: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
    if (!Array.isArray(rows) || rows.length > 1) throw new Error("supabase-bad-row");
    if (rows.length === 0) return null;
    if (!isObject(rows[0]) || !isEnvelope(envelopeFromRow(rows[0] as unknown as FactsCurrentRow))) throw new Error("supabase-bad-row");
    return rows[0] as unknown as FactsCurrentRow;
  } finally { clearTimeout(timer); }
}

async function retireLastGood(env: CloudFactsEnv, channel: string): Promise<void> {
  if (!env.CURATED_KV || !isEnvelope(await readBoundedKvJson(env.CURATED_KV, KV_PREFIX + channel, MAX_ENVELOPE_BYTES))) return;
  try {
    await env.CURATED_KV.put(`${KV_PREFIX}${channel}`, JSON.stringify({ retired: "no-current-head" }), { expirationTtl: KV_TTL_SECS });
  } catch { /* Best effort, like the write; the copy still expires on its TTL. */ }
}

export async function resolveCloudFacts(channel: string, env: CloudFactsEnv, opts: ResolveOptions = {}): Promise<CloudFactsResult> {
  if (!isValidChannel(channel)) return { kind: "none" };
  const keys = opts.keys ?? TRUSTED_KEYS;
  // Do not consult either transport when this release has no usable trust root.
  if (!hasActiveKeys(keys)) return { kind: "unavailable", reason: "no-active-keys" };
  let envelope: unknown;
  let source: "supabase" | "kv-stale" = "supabase";
  try {
    const row = await fetchCurrentRow(channel, env, opts);
    if (!row) {
      // The head was revoked, expired or future-dated. Retire the last-good
      // copy so a later outage cannot resurrect it through the stale path.
      await retireLastGood(env, channel);
      return { kind: "none" };
    }
    envelope = envelopeFromRow(row);
  } catch {
    source = "kv-stale";
    envelope = await readBoundedKvJson(env.CURATED_KV, KV_PREFIX + channel, MAX_ENVELOPE_BYTES);
    if (!envelope) return { kind: "unavailable", reason: "facts-transport-unavailable" };
  }
  const verification = await verifyEnvelope(envelope, keys, { channel, now: (opts.now ?? Date.now)() });
  if (!verification.ok) {
    if (source === "kv-stale" || !isEnvelope(envelope)) return { kind: "unavailable", reason: `facts-${verification.reason}` };
    if (verification.reason === "sha-mismatch") return { kind: "sha-mismatch", channel, factsVersion: envelope.facts_version };
    return { kind: "unverifiable", reason: verification.reason, channel, factsVersion: envelope.facts_version };
  }
  const verified = envelope as CloudFactsEnvelope;
  const body = JSON.stringify(verified);
  if (new TextEncoder().encode(body).length > MAX_ENVELOPE_BYTES) return { kind: "unavailable", reason: "facts-too-large" };
  if (source === "supabase" && env.CURATED_KV) {
    try { await env.CURATED_KV.put(`${KV_PREFIX}${channel}`, body, { expirationTtl: KV_TTL_SECS }); }
    catch { /* The last-good cache is best effort. */ }
  }
  return { kind: "ok", envelope: verified, body, etag: await etagFor(verified), source, verified: "verified", keyId: verification.keyId };
}

export async function cloudFactsSummary(env: CloudFactsEnv, channel = "stable"): Promise<
  { channel: string; factsVersion: number; publishedAt: string; keyId: string; source: string } | null
> {
  try {
    const result = await resolveCloudFacts(channel, env);
    if (result.kind !== "ok") return null;
    return { channel: result.envelope.channel, factsVersion: result.envelope.facts_version,
      publishedAt: result.envelope.published_at, keyId: result.keyId, source: result.source };
  } catch { return null; }
}

const CACHE_CONTROL = "public, max-age=300, s-maxage=300, stale-while-revalidate=3600, stale-if-error=604800";

function cacheControl(envelope: CloudFactsEnvelope): string {
  if (envelope.not_after == null) return CACHE_CONTROL;
  const seconds = Math.max(0, Math.min(300, Math.floor((utcTime(envelope.not_after)! - Date.now()) / 1000)));
  // A CDN must not extend a signed deadline through stale serving directives.
  return `public, max-age=${seconds}, s-maxage=${seconds}, must-revalidate`;
}

function baseHeaders(): Record<string, string> {
  return {
    "Content-Type": "application/json; charset=utf-8",
    "Access-Control-Allow-Origin": "*",
    "X-Content-Type-Options": "nosniff",
  };
}

function errorResponse(status: number, body: Record<string, unknown>, method: "GET" | "HEAD", extra: Record<string, string> = {}): Response {
  return new Response(method === "HEAD" ? null : JSON.stringify(body), {
    status,
    headers: { ...baseHeaders(), "Cache-Control": "no-store", ...extra },
  });
}

function etagMatches(ifNoneMatch: string | null, etag: string): boolean {
  if (!ifNoneMatch) return false;
  return ifNoneMatch
    .split(",")
    .map((v) => v.trim().replace(/^W\//, ""))
    .some((v) => v === etag || v === "*");
}

export function responseFor(result: CloudFactsResult, req: Request, channel: string, method: "GET" | "HEAD"): Response {
  switch (result.kind) {
    case "none":
      return errorResponse(404, { error: "no-facts", channel }, method, { "Cache-Control": "public, max-age=60" });
    case "sha-mismatch":
      return errorResponse(502, { error: "facts-digest-mismatch", channel, factsVersion: result.factsVersion }, method);
    case "unverifiable":
      return errorResponse(503, { error: "facts-unverifiable", reason: result.reason, channel, factsVersion: result.factsVersion }, method, { "Retry-After": "600" });
    case "unavailable":
      return errorResponse(503, { error: "facts-unavailable", channel }, method, { "Retry-After": "600" });
    case "ok": {
      const headers: Record<string, string> = {
        ...baseHeaders(),
        "Cache-Control": cacheControl(result.envelope),
        ETag: result.etag,
        "X-Facts-Channel": result.envelope.channel,
        "X-Facts-Version": String(result.envelope.facts_version),
        "X-Facts-Source": result.source,
        "X-Facts-Verified": result.verified,
        "X-Facts-Key": result.keyId,
      };
      if (etagMatches(req.headers.get("if-none-match"), result.etag)) {
        return new Response(null, { status: 304, headers });
      }
      headers["Content-Length"] = String(new TextEncoder().encode(result.body).length);
      return new Response(method === "HEAD" ? null : result.body, { status: 200, headers });
    }
  }
}
