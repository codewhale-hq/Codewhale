import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync, mkdtempSync, writeFileSync, rmSync, symlinkSync, linkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import {
  etagFor, resolveCloudFacts, verifyEnvelope, signingMessage, responseFor,
  MAX_ENVELOPE_BYTES, type CloudFactsEnvelope, type FactsCurrentRow, type CloudFactsResult,
} from "./cloud-facts";
import bundledRegistryTrust from "./cloud-facts/bundled-providers.json";
import { MAX_REGISTRY_ENVELOPE_BYTES, TRUSTED_KEYS, type TrustedKey } from "./cloud-facts/keys";
import { GET, HEAD } from "../app/api/facts/v1/[channel]/route";
import {
  activePublishingKey,
  buildEnvelope,
  buildRegistryPayload,
  emitSql,
  publishRelease,
  readBoundedFile,
  readBoundedResponse,
  revokeRelease,
  verifyEnvelope as verifyForPublisher,
} from "../scripts/facts-publish.mjs";
import { parseRustKeys, parseTsKeys } from "../scripts/check-cloud-facts.mjs";

const fixturePath = new URL("../../docs/cloud-facts/fixtures/envelope-stable-v7.json", import.meta.url);
const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as CloudFactsEnvelope;
const futureFixture = JSON.parse(readFileSync(new URL("../../docs/cloud-facts/fixtures/envelope-future-only-v8.json", import.meta.url), "utf8")) as CloudFactsEnvelope;
const TEST_KEY: TrustedKey = { keyId: "cwf-test-only", publicKey: "8+FLDW4OorUETUVks0hpQAi5Lj4wg3kjKjfYFzLbJ7U=", status: "active", role: "founder" };
const NOW = Date.parse("2026-09-07T00:00:00Z");
const basePayload = JSON.parse(Buffer.from(fixture.payload_b64, "base64").toString("utf8"));
// Ephemeral test keys exist only in memory and never enter production anchors.
const ephemeral = generateKeyPairSync("ed25519");
const EPHEMERAL_KEY: TrustedKey = { keyId: "cwf-ephemeral-test", publicKey: ephemeral.publicKey.export({ type: "spki", format: "der" }).subarray(-32).toString("base64"), status: "active", role: "founder" };

function signed(overrides: Record<string, unknown> = {}): CloudFactsEnvelope {
  const payload = { ...basePayload, ...overrides };
  const bytes = Buffer.from(JSON.stringify(payload));
  return {
    envelope: 1, channel: payload.channel, facts_version: payload.facts_version,
    schema_version: payload.schema_version, key_id: EPHEMERAL_KEY.keyId, alg: "ed25519",
    applies_to: payload.applies_to, published_at: payload.published_at,
    not_after: payload.not_after ?? null, payload_b64: bytes.toString("base64"),
    sig_b64: sign(null, signingMessage(EPHEMERAL_KEY.keyId, bytes), ephemeral.privateKey).toString("base64"),
    sigs: [], sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

function rowFrom(envelope = fixture, overrides: Partial<FactsCurrentRow> = {}): FactsCurrentRow {
  return { channel: envelope.channel, release_id: "00000000-0000-0000-0000-000000000001",
    facts_version: envelope.facts_version, schema_version: envelope.schema_version,
    envelope_version: envelope.envelope, applies_to: envelope.applies_to, key_id: envelope.key_id,
    payload_b64: envelope.payload_b64, sig_b64: envelope.sig_b64, sigs: envelope.sigs,
    payload_sha256: envelope.sha256, published_at: envelope.published_at,
    not_after: envelope.not_after ?? null, ...overrides };
}

function supabaseFetch(rows: unknown, status = 200): typeof fetch {
  return vi.fn(async () => new Response(JSON.stringify(rows), { status, headers: { "Content-Type": "application/json" } })) as typeof fetch;
}

class MemKV {
  store = new Map<string, string>();
  async get(key: string, type: "stream") {
    expect(type).toBe("stream");
    const raw = this.store.get(key);
    return raw === undefined ? null : new Response(raw).body;
  }
  async put(key: string, value: string) { this.store.set(key, value); }
}
const env = { SUPABASE_URL: "https://example.supabase.co", SUPABASE_PUBLISHABLE_KEY: "sb_publishable_test" };
const opts = { keys: [TEST_KEY], now: () => NOW };
const failing = supabaseFetch(null, 503);

function overflowingStream() {
  let pulls = 0;
  const cancel = vi.fn();
  return { cancel, pulls: () => pulls, stream: new ReadableStream<Uint8Array>({
    pull(controller) { pulls += 1; controller.enqueue(new Uint8Array(MAX_ENVELOPE_BYTES / 2 + 1)); },
    cancel,
  }) };
}

describe("cloud facts verification", () => {
  it("authenticates public fixtures, including a future-only client applicability range", async () => {
    for (const envelope of [fixture, futureFixture]) {
      expect(await verifyEnvelope(envelope, [TEST_KEY], { channel: "stable", now: NOW })).toEqual({ ok: true, keyId: TEST_KEY.keyId, mode: "verified" });
      expect(verifyForPublisher(envelope, TEST_KEY.publicKey).ok).toBe(true);
    }
    const result = await resolveCloudFacts("stable", env, { ...opts, fetchImpl: supabaseFetch([rowFrom(futureFixture)]) });
    expect(result).toMatchObject({ kind: "ok", envelope: { applies_to: ">=99.0.0", facts_version: 8 } });
  });

  it("pins a well-formed production anchor and refuses empty or retired-only trust before any reads", async () => {
    // The anchor itself is checked for shape, not for a specific key: pinning a
    // second key or rotating must not fail this test, but a malformed one must.
    // Byte-for-byte agreement with the Rust table is `check-cloud-facts.mjs`.
    expect(TRUSTED_KEYS.length).toBeGreaterThan(0);
    for (const key of TRUSTED_KEYS) {
      expect(key.keyId).toMatch(/^cwf-[A-Za-z0-9._-]+$/);
      expect(["active", "retired"]).toContain(key.status);
      // Standard base64 of a raw 32-byte Ed25519 public key.
      expect(Buffer.from(key.publicKey, "base64")).toHaveLength(32);
    }
    expect(TRUSTED_KEYS.some((key) => key.status === "active")).toBe(true);

    // The property that actually matters is unchanged: with no usable key the
    // layer fails closed *before* any network or cache read.
    const fetchImpl = vi.fn();
    const get = vi.fn();
    for (const keys of [[], [{ ...TEST_KEY, status: "retired" as const }]]) {
      expect(await verifyEnvelope(fixture, keys)).toEqual({ ok: false, reason: "no-active-keys" });
      expect(await resolveCloudFacts("stable", { ...env, CURATED_KV: { get, put: vi.fn() } }, { keys, fetchImpl })).toEqual({ kind: "unavailable", reason: "no-active-keys" });
    }
    expect(fetchImpl).not.toHaveBeenCalled();
    expect(get).not.toHaveBeenCalled();
  });

  it("rejects invalid signatures, unknown keys, retired keys and ambiguous key tables", async () => {
    expect(await verifyEnvelope({ ...fixture, sig_b64: `A${fixture.sig_b64.slice(1)}` }, [TEST_KEY])).toEqual({ ok: false, reason: "bad-signature" });
    expect(await verifyEnvelope(fixture, [EPHEMERAL_KEY])).toEqual({ ok: false, reason: "unknown-key" });
    expect(await verifyEnvelope(fixture, [{ ...TEST_KEY, status: "retired" }, EPHEMERAL_KEY])).toEqual({ ok: false, reason: "retired-key" });
    expect(await verifyEnvelope(fixture, [TEST_KEY, TEST_KEY])).toEqual({ ok: false, reason: "no-active-keys" });
  });

  it("cross-checks every unsigned metadata field with the signed payload", async () => {
    for (const change of [{ channel: "beta" }, { facts_version: 8 }, { schema_version: 2 },
      { applies_to: ">=99.0.0" }, { published_at: "2026-09-01T00:00:00Z" },
      { not_after: "2026-10-01T00:00:00Z" }, { sha256: "0".repeat(64) }]) {
      expect((await verifyEnvelope({ ...fixture, ...change }, [TEST_KEY], { channel: "stable", now: NOW })).ok).toBe(false);
      expect(verifyForPublisher({ ...fixture, ...change }, TEST_KEY.publicKey).ok).toBe(false);
    }
    expect(await verifyEnvelope(signed({ channel: "beta" }), [EPHEMERAL_KEY], { channel: "stable", now: NOW })).toEqual({ ok: false, reason: "wrong-channel" });
  });

  it("rejects signed bad versions, applicability, schema and UTC dates", async () => {
    for (const change of [{ facts_version: "7; DROP TABLE public.facts_key;" }, { facts_version: 0 },
      { facts_version: Number.MAX_SAFE_INTEGER + 1 }, { schema_version: 2 }, { applies_to: "><=3" },
      { published_at: "2026-02-30T00:00:00Z" }, { published_at: "not-a-date" }]) {
      const envelope = signed(change);
      expect((await verifyEnvelope(envelope, [EPHEMERAL_KEY], { now: NOW })).ok).toBe(false);
      expect(verifyForPublisher(envelope, EPHEMERAL_KEY.publicKey).ok).toBe(false);
    }
  });

  it("rejects future publication, expiry and reversed signed time windows", async () => {
    expect(await verifyEnvelope(fixture, [TEST_KEY], { now: Date.parse("2026-08-29T00:00:00Z") })).toEqual({ ok: false, reason: "bad-payload" });
    const expired = signed({ not_after: "2026-09-06T00:00:00Z" });
    expect(await verifyEnvelope(expired, [EPHEMERAL_KEY], { now: NOW })).toEqual({ ok: false, reason: "expired" });
    expect((await resolveCloudFacts("stable", env, { keys: [EPHEMERAL_KEY], now: () => NOW, fetchImpl: supabaseFetch([rowFrom(expired)]) })).kind).toBe("unverifiable");
    expect((await verifyEnvelope(signed({ not_after: "2026-08-29T00:00:00Z" }), [EPHEMERAL_KEY], { now: NOW })).ok).toBe(false);
    const zeroValidity = signed({ not_after: basePayload.published_at });
    expect(await verifyEnvelope(zeroValidity, [EPHEMERAL_KEY], { now: NOW })).toEqual({ ok: false, reason: "bad-payload" });
    expect(verifyForPublisher(zeroValidity, EPHEMERAL_KEY.publicKey).ok).toBe(false);
  });

  it("rejects oversized/noncanonical base64 and hostile signature shapes without throwing", async () => {
    for (const value of [null, [], {}, { ...fixture, payload_b64: "A".repeat(MAX_ENVELOPE_BYTES) },
      { ...fixture, payload_b64: `${fixture.payload_b64}\n` }, { ...fixture, sig_b64: `${fixture.sig_b64}garbage` },
      { ...fixture, sigs: "not-an-array" }, { ...fixture, sigs: Array(9).fill({ key_id: TEST_KEY.keyId, sig_b64: fixture.sig_b64 }) },
      { ...fixture, sigs: [null] }]) {
      expect((await verifyEnvelope(value, [TEST_KEY])).ok).toBe(false);
      expect(verifyForPublisher(value, TEST_KEY.publicKey).ok).toBe(false);
    }
  });

  it("reports the authenticating rotation key and changes ETag for signature-only updates", async () => {
    const rotated = { ...fixture, sigs: [{ key_id: EPHEMERAL_KEY.keyId,
      sig_b64: sign(null, signingMessage(EPHEMERAL_KEY.keyId, Buffer.from(fixture.payload_b64, "base64")), ephemeral.privateKey).toString("base64") }] };
    const keys = [{ ...TEST_KEY, status: "retired" as const }, EPHEMERAL_KEY];
    expect(await verifyEnvelope(rotated, keys, { now: NOW })).toEqual({ ok: true, keyId: EPHEMERAL_KEY.keyId, mode: "verified" });
    expect(await etagFor(rotated)).not.toBe(await etagFor(fixture));
    const result = await resolveCloudFacts("stable", env, { keys, now: () => NOW, fetchImpl: supabaseFetch([rowFrom(rotated)]) });
    expect(result).toMatchObject({ kind: "ok", keyId: EPHEMERAL_KEY.keyId });
    const request = new Request("https://example.test", { headers: { "if-none-match": await etagFor(fixture) } });
    const response = responseFor(result, request, "stable", "GET");
    expect(response.status).toBe(200);
    expect(response.headers.get("x-facts-key")).toBe(EPHEMERAL_KEY.keyId);
  });
});

describe("cloud facts transport", () => {
  it("uses one global channel query with a publishable credential and writes only a verified cache", async () => {
    const kv = new MemKV();
    const fetchImpl = supabaseFetch([rowFrom(fixture, { published_at: "2026-08-30T00:00:00+00:00" })]);
    const result = await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { ...opts, fetchImpl });
    expect(result).toMatchObject({ kind: "ok", source: "supabase", verified: "verified" });
    const [url, init] = vi.mocked(fetchImpl).mock.calls[0];
    const parsed = new URL(String(url));
    expect(parsed.origin).toBe(env.SUPABASE_URL);
    expect(parsed.pathname).toBe("/rest/v1/facts_current");
    expect(Object.fromEntries(parsed.searchParams)).toMatchObject({ channel: "eq.stable", scope: "eq.global", limit: "1" });
    expect(init?.headers).toMatchObject({ apikey: env.SUPABASE_PUBLISHABLE_KEY, Authorization: `Bearer ${env.SUPABASE_PUBLISHABLE_KEY}` });
    expect(init?.redirect).toBe("error");
    if (result.kind !== "ok") throw new Error("expected verified result");
    expect(kv.store.get("facts:cloud:stable")).toBe(result.body);
    expect(result.etag).toBe(`"${createHash("sha256").update(result.body).digest("hex")}"`);
  });

  it("rejects service/secret credentials before dispatch", async () => {
    const fetchImpl = vi.fn();
    const serviceJwt = `a.${Buffer.from(JSON.stringify({ role: "service_role" })).toString("base64url")}.b`;
    for (const key of ["sb_secret_do-not-send", serviceJwt]) {
      expect((await resolveCloudFacts("stable", { ...env, SUPABASE_PUBLISHABLE_KEY: key }, { ...opts, fetchImpl })).kind).toBe("unavailable");
    }
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("distinguishes no row and invalid channel and refuses a mismatched signed channel", async () => {
    const fetchImpl = supabaseFetch([]);
    expect((await resolveCloudFacts("Bad Slug", env, { ...opts, fetchImpl })).kind).toBe("none");
    expect(fetchImpl).not.toHaveBeenCalled();
    expect((await resolveCloudFacts("stable", env, { ...opts, fetchImpl })).kind).toBe("none");
    const beta = signed({ channel: "beta" });
    expect(await resolveCloudFacts("stable", env, { keys: [EPHEMERAL_KEY], now: () => NOW, fetchImpl: supabaseFetch([rowFrom(beta)]) })).toMatchObject({ kind: "unverifiable", reason: "wrong-channel" });
  });

  it("never caches digest/signature failures and does not amplify them through a 304", async () => {
    for (const change of [{ payload_sha256: "0".repeat(64) }, { sig_b64: `A${fixture.sig_b64.slice(1)}` }]) {
      const kv = new MemKV();
      const result = await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { ...opts, fetchImpl: supabaseFetch([rowFrom(fixture, change)]) });
      expect(["sha-mismatch", "unverifiable"]).toContain(result.kind);
      expect(kv.store.size).toBe(0);
      expect(responseFor(result, new Request("https://example.test", { headers: { "if-none-match": "*" } }), "stable", "GET").status).not.toBe(304);
    }
  });

  it("revalidates cached digest, channel, expiry and current trust after an outage", async () => {
    const kv = new MemKV();
    await kv.put("facts:cloud:stable", JSON.stringify(fixture));
    expect(await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { ...opts, fetchImpl: failing })).toMatchObject({ kind: "ok", source: "kv-stale" });
    for (const envelope of [{ ...fixture, sha256: "0".repeat(64) }, signed({ channel: "beta" }), signed({ not_after: "2026-09-06T00:00:00Z" })]) {
      await kv.put("facts:cloud:stable", JSON.stringify(envelope));
      expect((await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { keys: [TEST_KEY, EPHEMERAL_KEY], now: () => NOW, fetchImpl: failing })).kind).toBe("unavailable");
    }
    await kv.put("facts:cloud:stable", JSON.stringify(fixture));
    expect((await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { keys: [EPHEMERAL_KEY], now: () => NOW, fetchImpl: failing })).kind).toBe("unavailable");
  });

  it("never resurrects a revoked head through the stale cache after an outage", async () => {
    const kv = new MemKV();
    const ttls: unknown[] = [];
    const put = kv.put.bind(kv);
    kv.put = async (key: string, value: string, options?: { expirationTtl?: number }) => { ttls.push(options?.expirationTtl); await put(key, value); };
    expect((await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { ...opts, fetchImpl: supabaseFetch([rowFrom(fixture)]) })).kind).toBe("ok");
    // The operator revokes the head: facts_current returns no row.
    expect((await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { ...opts, fetchImpl: supabaseFetch([]) })).kind).toBe("none");
    // Supabase then goes down. The revoked envelope must not come back.
    expect(await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { ...opts, fetchImpl: failing })).toMatchObject({ kind: "unavailable" });
    // The bridge lasts no longer than a client's own 6 h staleness bound.
    expect(ttls.length).toBeGreaterThan(0);
    expect(ttls.every((ttl) => ttl === 6 * 60 * 60)).toBe(true);
  });

  it("caps and cancels PostgREST streaming bodies before JSON parsing", async () => {
    const oversized = overflowingStream();
    const fetchImpl = vi.fn(async () => new Response(oversized.stream));
    expect((await resolveCloudFacts("stable", env, { ...opts, fetchImpl })).kind).toBe("unavailable");
    expect(oversized.cancel).toHaveBeenCalledOnce();
    expect(oversized.pulls()).toBeLessThanOrEqual(3);
    const declared = overflowingStream();
    expect((await resolveCloudFacts("stable", env, { ...opts, fetchImpl: vi.fn(async () => new Response(declared.stream, { headers: { "content-length": String(MAX_ENVELOPE_BYTES + 1) } })) })).kind).toBe("unavailable");
    expect(declared.cancel).toHaveBeenCalledOnce();
  });

  it("caps KV streams and isolates malformed cache objects", async () => {
    const oversized = overflowingStream();
    const get = vi.fn(async () => oversized.stream);
    expect((await resolveCloudFacts("stable", { ...env, CURATED_KV: { get, put: vi.fn() } }, { ...opts, fetchImpl: failing })).kind).toBe("unavailable");
    expect(get).toHaveBeenCalledWith("facts:cloud:stable", "stream");
    expect(oversized.cancel).toHaveBeenCalledOnce();
    const kv = new MemKV();
    for (const bad of ["not-json", "null", JSON.stringify({ sigs: null })]) {
      await kv.put("facts:cloud:stable", bad);
      expect((await resolveCloudFacts("stable", { ...env, CURATED_KV: kv }, { ...opts, fetchImpl: failing })).kind).toBe("unavailable");
    }
  });
});

describe("facts response protocol", () => {
  const req = (headers: Record<string, string> = {}) => new Request("https://codewhale.net/api/facts/v1/stable", { headers });
  async function ok(): Promise<CloudFactsResult> { return { kind: "ok", envelope: fixture, body: JSON.stringify(fixture), etag: await etagFor(fixture), source: "supabase", verified: "verified", keyId: TEST_KEY.keyId }; }

  it("serves cacheable GET, conditional 304 and bodyless successful HEAD", async () => {
    const result = await ok();
    if (result.kind !== "ok") throw new Error("fixture result");
    const get = responseFor(result, req(), "stable", "GET");
    expect(get.status).toBe(200);
    expect(get.headers.get("cache-control")).toContain("s-maxage=300");
    expect(get.headers.get("set-cookie")).toBeNull();
    expect(get.headers.get("vary")).toBeNull();
    expect(await get.json()).toEqual(fixture);
    for (const tag of [result.etag, `W/${result.etag}`, "*"]) {
      const response = responseFor(result, req({ "if-none-match": tag }), "stable", "GET");
      expect(response.status).toBe(304);
      expect(await response.text()).toBe("");
    }
    const head = responseFor(result, req(), "stable", "HEAD");
    expect(head.headers.get("content-length")).toBe(String(Buffer.byteLength(result.body)));
    expect(await head.text()).toBe("");
  });

  it("returns bodyless HEAD for every error and the invalid-channel route", async () => {
    const cases: CloudFactsResult[] = [{ kind: "none" }, { kind: "sha-mismatch", channel: "stable", factsVersion: 7 },
      { kind: "unverifiable", channel: "stable", factsVersion: 7, reason: "bad-signature" }, { kind: "unavailable", reason: "no-active-keys" }];
    for (const result of cases) {
      const response = responseFor(result, req(), "stable", "HEAD");
      expect([404, 502, 503]).toContain(response.status);
      expect(await response.text()).toBe("");
    }
    expect(await (await HEAD(req(), { params: Promise.resolve({ channel: "Bad Slug" }) })).text()).toBe("");
    const get = await GET(req(), { params: Promise.resolve({ channel: "Bad Slug" }) });
    expect(get.status).toBe(404);
  });

  it("keeps CDN freshness inside a signed expiry without stale-serving extensions", async () => {
    const envelope = signed({ not_after: new Date(Date.now() + 90_000).toISOString() });
    const result = await resolveCloudFacts("stable", env, { keys: [EPHEMERAL_KEY], fetchImpl: supabaseFetch([rowFrom(envelope)]) });
    expect(result.kind).toBe("ok");
    const response = responseFor(result, req(), "stable", "GET");
    const control = response.headers.get("cache-control")!;
    expect(control).toContain("must-revalidate");
    expect(control).not.toContain("stale-");
    expect(Number(control.match(/s-maxage=(\d+)/)?.[1])).toBeLessThanOrEqual(90);
  });
});

describe("facts publisher boundaries", () => {
  const script = fileURLToPath(new URL("../scripts/facts-publish.mjs", import.meta.url));

  /** A PostgREST double over one channel and its releases. */
  function fakePostgrest(head: number, releases: { facts_version: number; status: string; payload_sha256: string; revoked_at?: string; revoke_reason?: string }[]) {
    const calls: string[] = [];
    const request = async (path: string, init: { method?: string; body?: Record<string, unknown> } = {}) => {
      calls.push(`${init.method ?? "GET"} ${path}`);
      if (path.startsWith("facts_channel")) return [{ id: "c1", max_facts_version: head }];
      if (path === "facts_key") return null;
      if (path === "facts_release?select=id,payload_sha256" && init.method === "POST") {
        if (Number(init.body?.facts_version) <= head) throw Object.assign(new Error("PostgREST request failed (HTTP 409)"), { status: 409 });
        head = Number(init.body?.facts_version);
        const stored = { id: "r-new", facts_version: head, status: "published", payload_sha256: String(init.body?.payload_sha256 ?? fixture.sha256) };
        releases.push(stored);
        return [{ id: stored.id, payload_sha256: stored.payload_sha256 }];
      }
      const version = Number(/facts_version=eq\.(\d+)/.exec(path)?.[1]);
      const matches = releases.filter((r) => r.facts_version === version && (!path.includes("status=eq.published") || r.status === "published"));
      if (init.method === "PATCH") for (const r of matches) Object.assign(r, init.body);
      return matches.map((r) => ({ id: `r-${r.facts_version}`, ...r }));
    };
    return { request, calls, releases };
  }

  it("revokes once, keeps the first revocation on a re-run, and refuses an unknown version", async () => {
    const pg = fakePostgrest(3, [{ facts_version: 3, status: "published", payload_sha256: "a" }]);
    expect(await revokeRelease(pg.request, { channel: "stable", version: 3, reason: "bad price", at: "2026-09-30T00:00:00Z" }))
      .toMatchObject({ revoked: 1, already_revoked: false, head: true });
    const again = await revokeRelease(pg.request, { channel: "stable", version: 3, reason: "second run", at: "2026-10-01T00:00:00Z" });
    expect(again).toMatchObject({ revoked: 0, already_revoked: true, revoked_at: "2026-09-30T00:00:00Z", revoke_reason: "bad price" });
    expect(pg.releases[0]).toMatchObject({ revoked_at: "2026-09-30T00:00:00Z", revoke_reason: "bad price" });
    await expect(revokeRelease(pg.request, { channel: "stable", version: 9, reason: "typo", at: "2026-10-01T00:00:00Z" })).rejects.toThrow("no published facts_version 9");
  });

  it("publishes exactly the envelope and treats a replay of the same bytes as already published", async () => {
    const pg = fakePostgrest(fixture.facts_version - 1, []);
    const row = { facts_version: fixture.facts_version };
    expect(await publishRelease(pg.request, fixture, row, "pub")).toMatchObject({ published: true, payload_sha256: fixture.sha256 });
    expect(await publishRelease(pg.request, fixture, row, "pub")).toMatchObject({ published: false, already_published: true, release_id: "r-new" });
    const other = { ...fixture, sha256: "f".repeat(64) };
    await expect(publishRelease(pg.request, other, row, "pub")).rejects.toThrow("high-water mark");
  });

  it("requires an active pinned primary key and cannot publish with an explicit fixture public key", () => {
    expect(() => activePublishingKey(fixture, [])).toThrow("not pinned and active");
    expect(() => activePublishingKey(fixture, [{ ...TEST_KEY, status: "retired" }])).toThrow("not pinned and active");
    expect(activePublishingKey(fixture, [TEST_KEY], NOW).check.ok).toBe(true);
    const result = spawnSync(process.execPath, [script, "publish", fileURLToPath(fixturePath), "--dry-run", "--public-key", TEST_KEY.publicKey], { encoding: "utf8" });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("publication requires the active pinned table");
  });

  it("refuses to publish authentically signed future or expired facts", () => {
    expect(() => activePublishingKey(signed({ not_after: "2026-09-06T00:00:00Z" }), [EPHEMERAL_KEY], NOW)).toThrow("future or expired");
    expect(() => activePublishingKey(signed({ published_at: "2026-09-08T00:00:00Z" }), [EPHEMERAL_KEY], NOW)).toThrow("future or expired");
  });

  it("refuses CI signing before reading a source or private credential path", () => {
    const result = spawnSync(process.execPath, [script, "sign", "--source", "/nonexistent-source-must-not-be-read"], {
      encoding: "utf8", env: { ...process.env, CI: "true", CODEWHALE_FACTS_SIGNING_KEY_FILE: "/nonexistent-key-must-not-be-read" },
    });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("refusing to run with a secret under CI");
    expect(result.stderr).not.toContain("ENOENT");
  });

  it("creates private keys exclusively without overwriting an existing file or symlink", () => {
    const dir = mkdtempSync(join(tmpdir(), "facts-key-exclusion-"));
    try {
      const target = join(dir, "existing.key");
      writeFileSync(target, "preserve-existing-file");
      const cleanEnv = { ...process.env };
      for (const marker of ["CI", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "CIRCLECI", "JENKINS_URL", "TF_BUILD"]) delete cleanEnv[marker];
      const result = spawnSync(process.execPath, [script, "keygen", "--key-id", "cwf-test-exclusive", "--out", target], { encoding: "utf8", env: cleanEnv });
      expect(result.status).toBe(1);
      expect(readFileSync(target, "utf8")).toBe("preserve-existing-file");
      if (process.platform !== "win32") {
        const link = join(dir, "linked.key");
        symlinkSync(target, link);
        expect(spawnSync(process.execPath, [script, "keygen", "--key-id", "cwf-test-exclusive", "--out", link], { encoding: "utf8", env: cleanEnv }).status).toBe(1);
        expect(readFileSync(target, "utf8")).toBe("preserve-existing-file");
      }
    } finally { rmSync(dir, { recursive: true, force: true }); }
  });

  it("rejects signed numeric SQL injection and emits escaped SQL only after verification", () => {
    const malicious = signed({ facts_version: "7; DROP TABLE public.facts_key;" });
    expect(() => emitSql(malicious, { publicKeyB64: EPHEMERAL_KEY.publicKey })).toThrow("positive safe integer");
    const sql = emitSql(fixture, { publicKeyB64: TEST_KEY.publicKey, publishedBy: "O'Hara" });
    expect(sql).toContain("select c.id, 7, 1, 1");
    expect(sql).toContain("O''Hara");
  });

  it("parses intentional empty key tables but fails closed on unknown syntax or duplicate/invalid anchors", () => {
    expect(parseTsKeys("export const TRUSTED_KEYS: readonly TrustedKey[] = [];")).toEqual([]);
    expect(parseRustKeys("pub const TRUSTED_KEYS: &[TrustedKey] = &[];")).toEqual([]);
    for (const text of ["no table", "export const TRUSTED_KEYS: readonly TrustedKey[] = [makeKey()];"]) expect(() => parseTsKeys(text)).toThrow();
    expect(() => parseRustKeys("pub const TRUSTED_KEYS: &[TrustedKey] = &[make_key()];")).toThrow();
    expect(() => parseTsKeys("// export const TRUSTED_KEYS: readonly TrustedKey[] = [];\nexport const TRUSTED_KEYS = makeKeys();")).toThrow();
    expect(() => parseRustKeys("// pub const TRUSTED_KEYS: &[TrustedKey] = &[];\npub const TRUSTED_KEYS: &[TrustedKey] = &[make_key()];")).toThrow();
    const entry = `{ keyId: "${TEST_KEY.keyId}", publicKey: "${TEST_KEY.publicKey}", status: "active", role: "founder" }`;
    expect(() => parseTsKeys(`export const TRUSTED_KEYS: readonly TrustedKey[] = [${entry}, ${entry}];`)).toThrow();
    expect(() => parseTsKeys(`export const TRUSTED_KEYS: readonly TrustedKey[] = [${entry.replace(TEST_KEY.publicKey, "bad")}];`)).toThrow();
    const rust = `pub const TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey { key_id: "${TEST_KEY.keyId}", public_key: [${[...Buffer.from(TEST_KEY.publicKey, "base64")].join(",")}], status: KeyStatus::Active, role: KeyRole::Founder }];`;
    expect(parseRustKeys(rust)).toEqual([TEST_KEY]);
    expect(parseRustKeys(rust.replace("KeyRole::Founder", "KeyRole::Automation"))).toEqual([{ ...TEST_KEY, role: "automation" }]);
    expect(() => parseRustKeys(rust.replace(", role: KeyRole::Founder", ""))).toThrow();
    expect(() => parseTsKeys(`export const TRUSTED_KEYS: readonly TrustedKey[] = [${entry.replace(', role: "founder"', "")}];`)).toThrow();
    expect(() => parseTsKeys(`export const TRUSTED_KEYS: readonly TrustedKey[] = [${entry.replace('"founder"', '"root"')}];`)).toThrow();
  });

  it("bounds publisher file/response reads and rejects symlink and hardlink inputs", async () => {
    const dir = mkdtempSync(join(tmpdir(), "facts-bounded-read-"));
    try {
      const target = join(dir, "source.json");
      writeFileSync(target, "12345");
      expect(() => readBoundedFile(target, 4)).toThrow();
      if (process.platform !== "win32") {
        const link = join(dir, "symlink.json");
        symlinkSync(target, link);
        expect(() => readBoundedFile(link)).toThrow();
        linkSync(target, join(dir, "hardlink.json"));
        expect(() => readBoundedFile(target)).toThrow();
      }
      const oversized = overflowingStream();
      await expect(readBoundedResponse(new Response(oversized.stream))).rejects.toThrow("size limit");
      expect(oversized.cancel).toHaveBeenCalledOnce();
    } finally { rmSync(dir, { recursive: true, force: true }); }
  });

  it("the local facts gate verifies both public fixtures and reports the pinned anchor count", () => {
    const checker = fileURLToPath(new URL("../scripts/check-cloud-facts.mjs", import.meta.url));
    // Derived from the table rather than hardcoded, so rotating or adding an
    // anchor does not require editing this assertion — only a gate that has
    // drifted out of step with the table will fail it.
    const active = TRUSTED_KEYS.filter((key) => key.status === "active").length;
    expect(execFileSync(process.execPath, [checker], { encoding: "utf8" })).toContain(
      `${active} active production keys`,
    );
  });
});

describe("automation key scope", () => {
  const AUTOMATION_KEY: TrustedKey = { ...EPHEMERAL_KEY, role: "automation" };
  const fixture = JSON.parse(readFileSync(new URL("../../docs/cloud-facts/fixtures/automation-scope-cases.json", import.meta.url), "utf8")) as
    { now: number; cases: { name: string; channel: string; payload: Record<string, unknown>; out_of_scope: string | null }[] };
  const now = fixture.now * 1000;

  function sealed(payload: Record<string, unknown>, extra: CloudFactsEnvelope["sigs"] = []): CloudFactsEnvelope {
    const bytes = Buffer.from(JSON.stringify(payload));
    return {
      envelope: 1, channel: payload.channel as string, facts_version: payload.facts_version as number,
      schema_version: payload.schema_version as number, key_id: EPHEMERAL_KEY.keyId, alg: "ed25519",
      applies_to: payload.applies_to as string, published_at: payload.published_at as string,
      not_after: (payload.not_after as string | undefined) ?? null, payload_b64: bytes.toString("base64"),
      sig_b64: sign(null, signingMessage(EPHEMERAL_KEY.keyId, bytes), ephemeral.privateKey).toString("base64"),
      sigs: extra, sha256: createHash("sha256").update(bytes).digest("hex"),
    };
  }

  it("judges every shared case as the fixture says, in the relay and in the publish script", async () => {
    expect(fixture.cases).toHaveLength(25);
    for (const { name, channel, payload, out_of_scope } of fixture.cases) {
      const envelope = sealed(payload);
      const relay = await verifyEnvelope(envelope, [AUTOMATION_KEY], { channel, now });
      const publish = () => activePublishingKey(envelope, [AUTOMATION_KEY], now);
      if (out_of_scope === null) {
        expect(relay, name).toEqual({ ok: true, keyId: AUTOMATION_KEY.keyId, mode: "verified" });
        expect(publish().key.keyId, name).toBe(AUTOMATION_KEY.keyId);
      } else {
        expect(relay, name).toEqual({ ok: false, reason: "out-of-scope" });
        expect(publish, name).toThrow();
      }
      expect((await verifyEnvelope(envelope, [EPHEMERAL_KEY], { channel, now })).ok, name).toBe(true);
    }
  });

  it("lets a founder signature decide in either order, and never a retired one", async () => {
    const founder = generateKeyPairSync("ed25519");
    const FOUNDER_KEY: TrustedKey = { keyId: "cwf-founder-test", publicKey: founder.publicKey.export({ type: "spki", format: "der" }).subarray(-32).toString("base64"), status: "active", role: "founder" };
    const payload = fixture.cases.find((entry) => entry.name === "default model")!.payload;
    const bytes = Buffer.from(JSON.stringify(payload));
    const founderSig = { key_id: FOUNDER_KEY.keyId, sig_b64: sign(null, signingMessage(FOUNDER_KEY.keyId, bytes), founder.privateKey).toString("base64") };
    const autoFirst = sealed(payload, [founderSig]);
    const opts = { channel: "stable-models", now };
    expect(await verifyEnvelope(autoFirst, [AUTOMATION_KEY, FOUNDER_KEY], opts)).toMatchObject({ ok: true, keyId: FOUNDER_KEY.keyId });
    const founderFirst = { ...autoFirst, key_id: founderSig.key_id, sig_b64: founderSig.sig_b64, sigs: [{ key_id: autoFirst.key_id, sig_b64: autoFirst.sig_b64 }] };
    expect(await verifyEnvelope(founderFirst, [AUTOMATION_KEY, FOUNDER_KEY], opts)).toMatchObject({ ok: true, keyId: FOUNDER_KEY.keyId });
    expect(await verifyEnvelope(autoFirst, [AUTOMATION_KEY, { ...FOUNDER_KEY, status: "retired" }], opts)).toEqual({ ok: false, reason: "out-of-scope" });
  });

  it("pins no automation key in production and rejects one key under two ids", async () => {
    expect(TRUSTED_KEYS.every((key) => key.role === "founder")).toBe(true);
    const entry = (id: string, role: string) => `{ keyId: "${id}", publicKey: "${TEST_KEY.publicKey}", status: "active", role: "${role}" }`;
    expect(() => parseTsKeys(`export const TRUSTED_KEYS: readonly TrustedKey[] = [${entry("cwf-a", "founder")}, ${entry("cwf-b", "automation")}];`)).toThrow();
    expect(await verifyEnvelope(sealed(fixture.cases[0].payload), [EPHEMERAL_KEY, { ...AUTOMATION_KEY, keyId: "cwf-dup-key" }], { channel: "stable-models", now })).toEqual({ ok: false, reason: "no-active-keys" });
  });
});

describe("registry channel relay and publisher", () => {
  const AUTOMATION_KEY: TrustedKey = { ...EPHEMERAL_KEY, role: "automation" };
  const REG_NOW = Date.parse("2026-11-17T19:00:00Z");

  function bundledProvidersList(): Record<string, unknown>[] {
    return Object.entries(bundledRegistryTrust.providers).map(([id, trust], position) => ({
      id,
      ...trust,
      position,
      label: id,
    }));
  }

  function sampleRegistry(overrides: Record<string, unknown> = {}): Record<string, unknown> {
    const providers = bundledProvidersList();
    return {
      schema: "codewhale.registry.v1",
      revision: 12,
      content_hash: `sha256:${"a".repeat(64)}`,
      generated_at: "2026-11-17T18:00:00Z",
      source: { engine: { repository: "codewhale", commit: "b".repeat(40) }, revision_seq: 12, revision_kind: "engine_import" },
      counts: { providers: providers.length, models: 1, offerings: 1 },
      providers,
      models: [{ id: "deepseek/deepseek-v4-flash", catalog: { name: "DeepSeek Flash" } }],
      offerings: [{ provider: "deepseek", id: "deepseek-flash", model: "deepseek/deepseek-v4-flash" }],
      dialects: { default_profile: "standard", custom_allowed_profiles: ["standard"], profiles: [], bindings: [] },
      engine_compat: { provider_descriptors: bundledRegistryTrust.providerDescriptorsCompat },
      ...overrides,
    };
  }

  it("verifies and serves founder-signed registry envelopes, including >512 KiB payloads and admitted providers", async () => {
    expect(MAX_REGISTRY_ENVELOPE_BYTES).toBe(11_250_344);
    const largePadding = "x".repeat(540 * 1024);
    const payload = buildRegistryPayload(sampleRegistry({
      generated_at: "2026-11-17T18:00:00.000Z",
      providers: [
        ...bundledProvidersList(),
        {
          id: "acme",
          origin: "descriptor",
          position: 99,
          label: "Acme",
          wire_policy: "openai-compatible",
          base_url: "https://api.acme.example/v1",
          auth: { secret_slot: "acme", env_vars: ["ACME_API_KEY"] },
          admission: { id: "radm_1", reviewed_by: "founder", reviewed_at: "2026-11-17T18:00:00Z", founder_signature: "required" },
        },
      ],
      engine_compat: { provider_descriptors: bundledRegistryTrust.providerDescriptorsCompat, pad: largePadding },
    }));
    expect(payload.generated_at).toBe("2026-11-17T18:00:00Z");
    const envelope = buildEnvelope({ privateKey: ephemeral.privateKey, keyId: EPHEMERAL_KEY.keyId, payload });
    expect(envelope.channel).toBe("registry");
    expect(envelope.facts_version).toBe(12);
    expect(envelope.published_at).toBe("2026-11-17T18:00:00Z");

    expect(await verifyEnvelope(envelope, [EPHEMERAL_KEY], { channel: "registry", now: REG_NOW })).toEqual({
      ok: true,
      keyId: EPHEMERAL_KEY.keyId,
      mode: "verified",
    });
    expect(verifyForPublisher(envelope, EPHEMERAL_KEY.publicKey).ok).toBe(true);
    expect(activePublishingKey(envelope, [EPHEMERAL_KEY], REG_NOW).key.keyId).toBe(EPHEMERAL_KEY.keyId);
    expect(emitSql(envelope, { publishedBy: "founder", publicKeyB64: EPHEMERAL_KEY.publicKey })).toContain("and c.slug = 'registry'");

    const releaseRow = {
      facts_version: envelope.facts_version,
      schema_version: envelope.schema_version,
      envelope_version: envelope.envelope,
      applies_to: envelope.applies_to,
      key_id: envelope.key_id,
      payload_b64: envelope.payload_b64,
      sig_b64: envelope.sig_b64,
      sigs: envelope.sigs,
      payload,
      published_at: envelope.published_at,
      not_after: envelope.not_after ?? null,
    };
    expect(Buffer.byteLength(JSON.stringify(releaseRow))).toBeGreaterThan(MAX_ENVELOPE_BYTES);
    const seenPaths: string[] = [];
    const postgrestOverHttp = async (path: string, init: { method?: string; body?: Record<string, unknown>; prefer?: string } = {}) => {
      seenPaths.push(`${init.method ?? "GET"} ${path}`);
      let bodyObj: unknown = null;
      if (path.startsWith("facts_channel")) bodyObj = [{ id: "c-reg" }];
      else if (path === "facts_key") bodyObj = null;
      else if (path === "facts_release?select=id,payload_sha256") bodyObj = [{ id: "r-reg-12", payload_sha256: envelope.sha256 }];
      else if (path === "facts_release") bodyObj = [{ id: "r-reg-12", payload_sha256: envelope.sha256, ...releaseRow }];
      const res = new Response(bodyObj === null ? "" : JSON.stringify(bodyObj));
      const text = await readBoundedResponse(res);
      return text ? JSON.parse(text) : null;
    };
    const published = await publishRelease(postgrestOverHttp, envelope, releaseRow, EPHEMERAL_KEY.publicKey);
    expect(published).toEqual({
      published: true,
      channel: "registry",
      facts_version: 12,
      release_id: "r-reg-12",
      payload_sha256: envelope.sha256,
    });
    expect(seenPaths).toContain("POST facts_release?select=id,payload_sha256");

    const kv = new MemKV();
    const resolved = await resolveCloudFacts("registry", { ...env, CURATED_KV: kv }, {
      keys: [EPHEMERAL_KEY],
      now: () => REG_NOW,
      fetchImpl: supabaseFetch([rowFrom(envelope, { published_at: "2026-11-17T18:00:00+00:00" })]),
    });
    expect(resolved).toMatchObject({ kind: "ok", keyId: EPHEMERAL_KEY.keyId, envelope: { channel: "registry", facts_version: 12, published_at: "2026-11-17T18:00:00Z" } });
    const httpRes = responseFor(resolved, new Request("https://codewhale.dev/api/facts/v1/registry"), "registry", "GET");
    expect(httpRes.status).toBe(200);
    expect(httpRes.headers.get("X-Facts-Channel")).toBe("registry");
    expect(httpRes.headers.get("X-Facts-Version")).toBe("12");
  });

  it("permits automation-signed model/offering registry updates with bounded not_after and refuses unbundled or mutated providers", async () => {
    const providersWithNamingUpdate = bundledProvidersList().map((row) =>
      row.id === "deepseek" ? { ...row, models: { ids: "strict", aliases: { "deepseek-next": "deepseek-chat" } } } : row,
    );
    const modelOnlyPayload = buildRegistryPayload(sampleRegistry({ providers: providersWithNamingUpdate }), {
      notAfter: "2026-11-20T18:00:00Z",
    });
    const modelOnlyEnvelope = buildEnvelope({ privateKey: ephemeral.privateKey, keyId: EPHEMERAL_KEY.keyId, payload: modelOnlyPayload });
    expect(await verifyEnvelope(modelOnlyEnvelope, [AUTOMATION_KEY], { channel: "registry", now: REG_NOW })).toEqual({
      ok: true,
      keyId: AUTOMATION_KEY.keyId,
      mode: "verified",
    });
    expect(activePublishingKey(modelOnlyEnvelope, [AUTOMATION_KEY], REG_NOW).key.keyId).toBe(AUTOMATION_KEY.keyId);

    const noExpiry = buildEnvelope({ privateKey: ephemeral.privateKey, keyId: EPHEMERAL_KEY.keyId, payload: buildRegistryPayload(sampleRegistry()) });
    expect(await verifyEnvelope(noExpiry, [AUTOMATION_KEY], { channel: "registry", now: REG_NOW })).toEqual({ ok: false, reason: "out-of-scope" });
    expect(() => activePublishingKey(noExpiry, [AUTOMATION_KEY], REG_NOW)).toThrow(/no not_after/);

    const tooLong = buildEnvelope({
      privateKey: ephemeral.privateKey,
      keyId: EPHEMERAL_KEY.keyId,
      payload: buildRegistryPayload(sampleRegistry(), { notAfter: "2026-12-01T18:00:00Z" }),
    });
    expect(await verifyEnvelope(tooLong, [AUTOMATION_KEY], { channel: "registry", now: REG_NOW })).toEqual({ ok: false, reason: "out-of-scope" });

    for (const badRegistry of [
      sampleRegistry({
        providers: [
          ...bundledProvidersList(),
          {
            id: "acme",
            origin: "descriptor",
            position: 99,
            label: "Acme",
            wire_policy: "openai-compatible",
            base_url: "https://api.acme.example/v1",
            auth: { secret_slot: "acme", env_vars: ["ACME_API_KEY"] },
          },
        ],
      }),
      sampleRegistry({
        providers: bundledProvidersList().map((row) =>
          row.id === "openai" ? { ...row, base_url: "https://evil.example/v1" } : row,
        ),
      }),
      sampleRegistry({
        providers: bundledProvidersList().slice(1),
      }),
      sampleRegistry({
        engine_compat: { provider_descriptors: { ...bundledRegistryTrust.providerDescriptorsCompat, injected: true } },
      }),
    ]) {
      const badEnvelope = buildEnvelope({
        privateKey: ephemeral.privateKey,
        keyId: EPHEMERAL_KEY.keyId,
        payload: buildRegistryPayload(badRegistry, { notAfter: "2026-11-20T18:00:00Z" }),
      });
      expect(await verifyEnvelope(badEnvelope, [AUTOMATION_KEY], { channel: "registry", now: REG_NOW })).toEqual({
        ok: false,
        reason: "out-of-scope",
      });
      expect(() => activePublishingKey(badEnvelope, [AUTOMATION_KEY], REG_NOW)).toThrow(/founder signature/);
    }
  });

  it("rejects tampered outer metadata, channel confusion, and oversized stable payloads", async () => {
    const payload = buildRegistryPayload(sampleRegistry(), { notAfter: "2026-11-20T18:00:00Z", appliesTo: ">=0.10.3" });
    const envelope = buildEnvelope({ privateKey: ephemeral.privateKey, keyId: EPHEMERAL_KEY.keyId, payload });
    expect(envelope.applies_to).toBe(">=0.10.3");
    expect(await verifyEnvelope(envelope, [EPHEMERAL_KEY], { channel: "registry", now: REG_NOW })).toEqual({
      ok: true,
      keyId: EPHEMERAL_KEY.keyId,
      mode: "verified",
    });
    expect(await verifyEnvelope(envelope, [EPHEMERAL_KEY], { channel: "stable", now: REG_NOW })).toEqual({ ok: false, reason: "wrong-channel" });
    for (const tampered of [
      { facts_version: 13 },
      { published_at: "2026-11-17T18:00:01Z" },
      { applies_to: "*" },
      { applies_to: ">=99.0.0" },
      { not_after: null },
      { not_after: "2026-11-21T18:00:00Z" },
    ]) {
      expect((await verifyEnvelope({ ...envelope, ...tampered }, [EPHEMERAL_KEY], { channel: "registry", now: REG_NOW })).ok).toBe(false);
      expect(verifyForPublisher({ ...envelope, ...tampered }, EPHEMERAL_KEY.publicKey).ok).toBe(false);
    }
  });
});
