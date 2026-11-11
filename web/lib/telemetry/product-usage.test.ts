import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { handleProductTelemetry, ingestUrl, CANONICAL_INGEST_URL } from "../../app/api/product-telemetry/route";
import {
  COUNTERS_STORAGE_KEY,
  FUNNEL_STORAGE_KEY,
  emptyFunnelCounters,
  INSTALL_ID_ROTATION_MS,
  INSTALL_STORAGE_KEY,
  NOTICE_VERSION,
  PREFERENCE_STORAGE_KEY,
  SCHEMA_VERSION,
  buildEnvelope,
  createUsageRecorder,
  emptyCounters,
  privacySignalActive,
  privacySignalHeader,
  readUsagePreference,
  resolveInstallId,
  usageCountingEnabled,
  usagePreferenceRecord,
  validateEnvelope,
  type StorageLike,
} from "./product-usage";

/** The backend's golden browser fixture, when the ingest checkout carries it. */
const GOLDEN_PATH = new URL("../../../telemetry-ingest/test/golden/browser-v3.json", import.meta.url);

const UUID = "82b77c4f-4cce-4c74-8e17-8a38ba0581ee";
const NOW = Date.parse("2026-09-04T20:00:00Z");

function memoryStorage(seed: Record<string, string> = {}): StorageLike & { data: Map<string, string> } {
  const data = new Map(Object.entries(seed));
  return {
    data,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => void data.set(key, value),
    removeItem: (key) => void data.delete(key),
  };
}

function recorderWith(storage: StorageLike, sent: string[] = [], now = () => NOW, blocked?: () => boolean) {
  const timers: (() => void)[] = [];
  const recorder = createUsageRecorder({
    surface: "website",
    appVersion: "0.9.12",
    endpoint: "/api/product-telemetry",
    storage,
    now,
    blocked,
    randomUuid: () => UUID,
    send: async (_endpoint, body) => {
      sent.push(body);
      return true;
    },
    setTimer: (callback) => {
      timers.push(callback);
      return timers.length;
    },
    clearTimer: (handle) => {
      timers[(handle as number) - 1] = () => {};
    },
  });
  return { recorder, timers, sent };
}

describe("product usage envelope", () => {
  it("matches the closed browser contract and the backend's golden fixture", () => {
    const counters = emptyCounters();
    counters.page_view = 1;
    const envelope = buildEnvelope({ counters, installId: UUID, appVersion: "0.9.12", surface: "website", now: NOW });
    expect(validateEnvelope(envelope)).toEqual({ ok: true, envelope });
    expect(SCHEMA_VERSION).toBe(5);
    expect(NOTICE_VERSION).toBe(7);
    expect(envelope.notice_version).toBe(7);
    expect(envelope).not.toHaveProperty("consent_version");
    expect(envelope.sent_at).toBe("2026-09-04T20:00:00Z");
    if (existsSync(GOLDEN_PATH)) {
      const golden = JSON.parse(readFileSync(GOLDEN_PATH, "utf8"));
      expect(golden.schema_version).toBe(3);
      expect(golden.notice_version).toBe(5);
      expect(validateEnvelope(golden).ok).toBe(true);
    }
  });

  it("rejects anything outside the closed field set", () => {
    const base = buildEnvelope({ counters: emptyCounters(), installId: UUID, appVersion: "0.9.12", surface: "website", now: NOW });
    expect(validateEnvelope({ ...base, referrer: "x" }).ok).toBe(false);
    expect(validateEnvelope({ ...base, git_sha: "abc" }).ok).toBe(false);
    expect(validateEnvelope({ ...base, surface: "tui" }).ok).toBe(false);
    expect(validateEnvelope({ ...base, notice_version: 4 }).ok).toBe(false);
    expect(validateEnvelope({ ...base, schema_version: 2 }).ok).toBe(false);
    // The retired consent field never rides along with the policy version.
    expect(validateEnvelope({ ...base, consent_version: 4 }).ok).toBe(false);
    expect(validateEnvelope({ ...base, install_id: "not-a-uuid" }).ok).toBe(false);
    expect(validateEnvelope({ ...base, events: [] }).ok).toBe(false);
    const extraCounter = structuredClone(base) as unknown as { events: [{ counters: Record<string, number> }] };
    extraCounter.events[0].counters.url = 1;
    expect(validateEnvelope(extraCounter).ok).toBe(false);
    const negative = structuredClone(base) as unknown as { events: [{ counters: Record<string, number> }] };
    negative.events[0].counters.page_view = -1;
    expect(validateEnvelope(negative).ok).toBe(false);
    // The website route only ever accepts the website surface.
    expect(validateEnvelope({ ...base, surface: "web-app" }, { surfaces: ["website"] }).ok).toBe(false);
  });
});

describe("usage preference", () => {
  it("is on by default, keeps every recorded opt-out, and fails closed on unreadable state", () => {
    expect(readUsagePreference(null)).toBe("default");
    expect(readUsagePreference(undefined)).toBe("default");
    expect(readUsagePreference("")).toBe("off");
    expect(usageCountingEnabled("default")).toBe(true);
    // Unreadable stored state is never replaced with the default.
    expect(readUsagePreference("{not json")).toBe("off");
    expect(readUsagePreference(JSON.stringify({ version: 4 }))).toBe("off");
    expect(readUsagePreference(JSON.stringify({ version: 4, granted: "yes" }))).toBe("off");
    // An opt-out recorded under the old opt-in policy is still an opt-out.
    expect(readUsagePreference(JSON.stringify({ version: 4, granted: false }))).toBe("off");
    expect(readUsagePreference(JSON.stringify({ version: 3, granted: false }))).toBe("off");
    expect(readUsagePreference(JSON.stringify({ version: 4, granted: true }))).toBe("on");
    expect(readUsagePreference(usagePreferenceRecord(false, NOW))).toBe("off");
    expect(readUsagePreference(usagePreferenceRecord(true, NOW))).toBe("on");
    expect(JSON.parse(usagePreferenceRecord(false, NOW))).toEqual({ version: NOTICE_VERSION, granted: false, decidedAt: "2026-09-04T20:00:00Z" });
  });

  it("counts by default without writing any preference record", async () => {
    const storage = memoryStorage();
    const { recorder, timers, sent } = recorderWith(storage);
    expect(recorder.preference()).toBe("default");
    recorder.record("page_view");
    recorder.record("page_view");
    recorder.record("docs_view");
    expect(timers).toHaveLength(1);
    timers[0]();
    await Promise.resolve();
    expect(sent).toHaveLength(1);
    const body = JSON.parse(sent[0]);
    expect(validateEnvelope(body).ok).toBe(true);
    expect(body.events[0].counters.page_view).toBe(2);
    expect(body.events[0].counters.docs_view).toBe(1);
    expect(body.install_id).toBe(UUID);
    // Discarded after the attempt; nothing was recorded as an acceptance.
    expect(recorder.pending()).toEqual(emptyCounters());
    expect(storage.data.has(COUNTERS_STORAGE_KEY)).toBe(false);
    expect(storage.data.has(PREFERENCE_STORAGE_KEY)).toBe(false);
  });

  it("counts nothing and stores nothing after an opt-out, including one recorded under the old policy", () => {
    for (const seed of [
      { [PREFERENCE_STORAGE_KEY]: JSON.stringify({ version: 4, granted: false }) },
      { [PREFERENCE_STORAGE_KEY]: usagePreferenceRecord(false, NOW) },
      { [PREFERENCE_STORAGE_KEY]: "{corrupt" },
    ]) {
      const storage = memoryStorage({ ...seed, [COUNTERS_STORAGE_KEY]: JSON.stringify({ page_view: 9 }), [INSTALL_STORAGE_KEY]: "stale" });
      const { recorder, timers, sent } = recorderWith(storage);
      expect(recorder.preference()).toBe("off");
      recorder.record("page_view");
      recorder.record("install_copy");
      expect(recorder.pending()).toEqual(emptyCounters());
      expect(storage.data.has(COUNTERS_STORAGE_KEY)).toBe(false);
      expect(storage.data.has(INSTALL_STORAGE_KEY)).toBe(false);
      expect(timers).toHaveLength(0);
      expect(sent).toHaveLength(0);
    }
  });

  it("clears queued counts and identity on opt-out, cancels pending delivery, and re-enables only deliberately", async () => {
    const storage = memoryStorage();
    const { recorder, timers, sent } = recorderWith(storage);
    recorder.record("page_view");
    await recorder.flush();
    expect(sent).toHaveLength(1);
    expect(storage.data.has(INSTALL_STORAGE_KEY)).toBe(true);
    recorder.record("download");
    expect(timers).toHaveLength(2);
    recorder.disable();
    expect(recorder.preference()).toBe("off");
    expect(recorder.pending()).toEqual(emptyCounters());
    expect(storage.data.has(INSTALL_STORAGE_KEY)).toBe(false);
    expect(storage.data.has(COUNTERS_STORAGE_KEY)).toBe(false);
    timers[1]();
    await Promise.resolve();
    expect(sent).toHaveLength(1);
    recorder.record("page_view");
    expect(recorder.pending()).toEqual(emptyCounters());
    recorder.enable();
    expect(recorder.preference()).toBe("on");
    recorder.record("page_view");
    expect(recorder.pending().page_view).toBe(1);
  });

  it("honours another tab's opt-out through sync()", () => {
    const storage = memoryStorage();
    const { recorder } = recorderWith(storage);
    recorder.record("page_view");
    expect(recorder.pending().page_view).toBe(1);
    // Another tab turns it off: the shared storage changes underneath us.
    storage.setItem(PREFERENCE_STORAGE_KEY, usagePreferenceRecord(false, NOW));
    recorder.sync();
    expect(recorder.pending()).toEqual(emptyCounters());
    expect(storage.data.has(COUNTERS_STORAGE_KEY)).toBe(false);
  });
});

describe("install id", () => {
  it("is a random v4 id, kept for 90 days and then rotated", () => {
    const fresh = resolveInstallId(null, NOW, () => UUID);
    expect(fresh).toEqual({ id: UUID, raw: JSON.stringify({ id: UUID, createdAt: NOW }), rotated: true });
    const kept = resolveInstallId(fresh.raw, NOW + INSTALL_ID_ROTATION_MS - 1, () => "unused");
    expect(kept.id).toBe(UUID);
    expect(kept.rotated).toBe(false);
    const rotated = resolveInstallId(fresh.raw, NOW + INSTALL_ID_ROTATION_MS, () => "9f1a2b3c-4d5e-4f60-8a1b-2c3d4e5f6a7b");
    expect(rotated.id).toBe("9f1a2b3c-4d5e-4f60-8a1b-2c3d4e5f6a7b");
    expect(rotated.rotated).toBe(true);
    expect(resolveInstallId("garbage", NOW, () => UUID).rotated).toBe(true);
  });
});

describe("same-origin forwarder", () => {
  const envelope = buildEnvelope({ counters: { ...emptyCounters(), page_view: 1 }, installId: UUID, appVersion: "0.9.12", surface: "website", now: NOW });
  const post = (body: unknown) =>
    new Request("http://localhost/api/product-telemetry", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: typeof body === "string" ? body : JSON.stringify(body),
    });

  it("is inert without the exact canonical ingest configured", async () => {
    expect(ingestUrl({})).toBeNull();
    expect(ingestUrl({ CODEWHALE_TELEMETRY_INGEST_URL: "https://example.com/v1/telemetry" })).toBeNull();
    expect(ingestUrl({ CODEWHALE_TELEMETRY_INGEST_URL: CANONICAL_INGEST_URL })).toBe(CANONICAL_INGEST_URL);
    let forwarded = 0;
    const response = await handleProductTelemetry(post(envelope), {
      ingestUrl: null,
      forward: async () => {
        forwarded += 1;
        return { ok: true };
      },
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ accepted: false, reason: "disabled" });
    expect(forwarded).toBe(0);
  });

  it("forwards only a validated website batch, and only the batch", async () => {
    const calls: { url: string; body: string }[] = [];
    const forward = async (url: string, body: string) => {
      calls.push({ url, body });
      return { ok: true };
    };
    const accepted = await handleProductTelemetry(post(envelope), { ingestUrl: CANONICAL_INGEST_URL, forward });
    expect(await accepted.json()).toEqual({ accepted: true });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(CANONICAL_INGEST_URL);
    expect(JSON.parse(calls[0].body)).toEqual(envelope);

    const rejected = await handleProductTelemetry(post({ ...envelope, surface: "web-app" }), { ingestUrl: CANONICAL_INGEST_URL, forward });
    expect(rejected.status).toBe(422);
    expect(calls).toHaveLength(1);

    const invalid = await handleProductTelemetry(post("{"), { ingestUrl: CANONICAL_INGEST_URL, forward });
    expect(invalid.status).toBe(422);

    const oversized = await handleProductTelemetry(post({ ...envelope, app_version: "0.9.12-" + "x".repeat(5000) }), { ingestUrl: CANONICAL_INGEST_URL, forward });
    expect(oversized.status).toBe(413);
    expect(calls).toHaveLength(1);
  });

  it.each([undefined, "1", "4096"])("cancels oversized chunks with declared length %s and never forwards", async (length) => {
    let cancelled = false;
    let forwarded = 0;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(2048));
        controller.enqueue(new Uint8Array(2049));
        // Deliberately leave the source open: overflow must cancel it.
      },
      cancel() { cancelled = true; },
    });
    const request = new Request("http://localhost/api/product-telemetry", {
      method: "POST", body, duplex: "half",
      headers: length === undefined ? {} : { "content-length": length },
    } as RequestInit);
    const response = await handleProductTelemetry(request, {
      ingestUrl: CANONICAL_INGEST_URL,
      forward: async () => { forwarded += 1; return { ok: true }; },
    });
    expect(response.status).toBe(413);
    expect(await response.json()).toEqual({ accepted: false, reason: "too_large" });
    expect(cancelled).toBe(true);
    expect(request.body?.locked).toBe(false);
    expect(forwarded).toBe(0);
  });

  it("bounds UTF-8 bytes rather than JS character count", async () => {
    let forwarded = 0;
    const response = await handleProductTelemetry(post("鲸".repeat(1400)), {
      ingestUrl: CANONICAL_INGEST_URL,
      forward: async () => { forwarded += 1; return { ok: true }; },
    });
    expect(response.status).toBe(413);
    expect(forwarded).toBe(0);
  });

  it("accepts an exact 4096-byte valid envelope across chunks", async () => {
    const json = JSON.stringify(envelope);
    const bytes = new TextEncoder().encode(json + " ".repeat(4096 - new TextEncoder().encode(json).byteLength));
    const body = new ReadableStream<Uint8Array>({ start(controller) {
      for (let offset = 0; offset < bytes.length; offset += 7) controller.enqueue(bytes.slice(offset, offset + 7));
      controller.close();
    } });
    let forwarded = 0;
    const response = await handleProductTelemetry(new Request("http://localhost/api/product-telemetry", {
      method: "POST", body, duplex: "half",
    } as RequestInit), {
      ingestUrl: CANONICAL_INGEST_URL,
      forward: async (_url, value) => { forwarded += 1; expect(JSON.parse(value)).toEqual(envelope); return { ok: true }; },
    });
    expect(await response.json()).toEqual({ accepted: true });
    expect(forwarded).toBe(1);
  });

  it.each(["stream_failure", "invalid_utf8", "invalid_length"])("rejects %s without forwarding", async (kind) => {
    const body = new ReadableStream<Uint8Array>({ start(controller) {
      if (kind === "stream_failure") controller.error(new Error("read failed"));
      else { controller.enqueue(new Uint8Array([0xff])); controller.close(); }
    } });
    let forwarded = 0;
    const response = await handleProductTelemetry(new Request("http://localhost/api/product-telemetry", {
      method: "POST", body, duplex: "half",
      headers: kind === "invalid_length" ? { "content-length": "no" } : {},
    } as RequestInit), {
      ingestUrl: CANONICAL_INGEST_URL,
      forward: async () => { forwarded += 1; return { ok: true }; },
    });
    expect(response.status).toBe(kind === "invalid_utf8" ? 422 : 400);
    expect(forwarded).toBe(0);
  });

  it("reports an unreachable ingest as unavailable without retrying", async () => {
    let attempts = 0;
    const response = await handleProductTelemetry(post(envelope), {
      ingestUrl: CANONICAL_INGEST_URL,
      forward: async () => {
        attempts += 1;
        throw new Error("down");
      },
    });
    expect(await response.json()).toEqual({ accepted: false, reason: "unavailable" });
    expect(attempts).toBe(1);
  });
});


describe("website funnel contract and delivery", () => {
  it("matches the ingest golden and rejects legacy or malformed funnel payloads", async () => {
    const { validateBatch } = await import("../../../telemetry-ingest/src/schema");
    const counters = emptyCounters(); counters.page_view = 1;
    const funnel = emptyFunnelCounters();
    funnel.home_view = 1; funnel.install_intent = 1; funnel.docs_search = 1; funnel.docs_search_result = 1;
    const envelope = buildEnvelope({ counters, funnel, installId: UUID, appVersion: "0.9.12", surface: "website", now: NOW });
    const golden = JSON.parse(readFileSync(new URL("../../../telemetry-ingest/test/golden/browser-v5.json", import.meta.url), "utf8"));
    expect(envelope).toEqual(golden);
    expect(validateBatch(envelope).ok).toBe(true);
    expect(validateEnvelope(envelope).ok).toBe(true);
    expect(validateEnvelope({ ...envelope, schema_version: 3, notice_version: 5 }).ok).toBe(false);
    expect(validateEnvelope({ ...envelope, surface: "desktop" }).ok).toBe(false);
    for (const bad of [-1, 1.5, 4294967296, "1"]) {
      const invalid = structuredClone(golden); invalid.events[1].counters.docs_search = bad;
      expect(validateEnvelope(invalid).ok).toBe(false); expect(validateBatch(invalid).ok).toBe(false);
    }
    const unexpected = structuredClone(golden); unexpected.events[1].counters.query = "private words";
    expect(validateEnvelope(unexpected).ok).toBe(false); expect(validateBatch(unexpected).ok).toBe(false);
  });

  it.each([
    [200, { accepted: true }, "accepted"],
    [200, { accepted: false, reason: "disabled" }, "rejected"],
    [200, { accepted: false, reason: "unavailable" }, "rejected"],
    [200, { accepted: true, padding: "x".repeat(513) }, "rejected"],
    [500, { accepted: true }, "rejected"],
    [200, { other: true }, "rejected"],
  ] as const)("requires an explicit bounded acceptance receipt (%s)", async (status, payload, expected) => {
    const fetchMock = vi.fn(async (endpoint: string, init: RequestInit) => { expect(endpoint).toBe("/api/product-telemetry"); expect(init.method).toBe("POST"); return Response.json(payload, { status }); }); vi.stubGlobal("fetch", fetchMock);
    try {
      const recorder = createUsageRecorder({ surface: "website", appVersion: "0.10.3", endpoint: "/api/product-telemetry", storage: memoryStorage(), now: () => NOW, randomUuid: () => UUID, setTimer: () => 0, clearTimer: () => {} });
      recorder.recordFunnel("quickstart_confirmed"); await recorder.flush();
      expect(recorder.delivery()).toBe(expected); expect(fetchMock).toHaveBeenCalledTimes(1);
      expect(fetchMock.mock.calls[0][1]).toMatchObject({ credentials: "omit", referrerPolicy: "no-referrer" });
      expect(recorder.pendingFunnel()).toEqual(emptyFunnelCounters());
    } finally { vi.unstubAllGlobals(); }
  });

  it("fails closed on unreadable storage and a failed opt-out write", async () => {
    const unreadable = memoryStorage(); unreadable.getItem = () => { throw new Error("blocked"); };
    const sent: string[] = []; const first = recorderWith(unreadable, sent).recorder;
    expect(first.preference()).toBe("off"); first.recordFunnel("install_confirmed"); await first.flush(); expect(sent).toEqual([]);
    const storage = memoryStorage(); const write = storage.setItem;
    storage.setItem = (key, value) => { if (key === PREFERENCE_STORAGE_KEY) throw new Error("quota"); write(key, value); };
    const second = recorderWith(storage, sent).recorder;
    expect(second.disable()).toBe(false); expect(second.preference()).toBe("off");
    second.record("page_view"); second.recordFunnel("install_confirmed"); await second.flush();
    expect(sent).toEqual([]); expect(storage.data.has(INSTALL_STORAGE_KEY)).toBe(false);
  });

  it("aborts delivery and erases both queues when counting is turned off", async () => {
    const storage = memoryStorage(); let signal: AbortSignal | undefined;
    const recorder = createUsageRecorder({ surface: "website", appVersion: "0.10.3", endpoint: "/api/product-telemetry", storage, now: () => NOW, randomUuid: () => UUID, setTimer: () => 0, clearTimer: () => {}, send: async (_url, _body, abort) => {
      signal = abort; return new Promise<boolean>((resolve) => abort!.addEventListener("abort", () => resolve(false), { once: true }));
    } });
    recorder.record("install_copy"); recorder.recordFunnel("install_intent"); const delivery = recorder.flush();
    expect(signal?.aborted).toBe(false); expect(recorder.disable()).toBe(true); await delivery; expect(signal?.aborted).toBe(true);
    for (const key of [COUNTERS_STORAGE_KEY, FUNNEL_STORAGE_KEY, INSTALL_STORAGE_KEY]) expect(storage.data.has(key)).toBe(false);
    recorder.recordFunnel("quickstart_confirmed"); await recorder.flush(); expect(recorder.pendingFunnel()).toEqual(emptyFunnelCounters());
  });

  it("reschedules new interactions during delivery without retrying the previous batch", async () => {
    const timers: (() => void)[] = []; const sent: { events: { event: string; counters: Record<string, number> }[] }[] = [];
    let finish: ((accepted: boolean) => void) | undefined;
    const recorder = createUsageRecorder({ surface: "website", appVersion: "0.10.3", endpoint: "/api/product-telemetry", storage: memoryStorage(), now: () => NOW, randomUuid: () => UUID, setTimer: (callback) => { timers.push(callback); return timers.length; }, clearTimer: () => {}, send: async (_url, body) => {
      sent.push(JSON.parse(body)); return sent.length === 1 ? new Promise<boolean>((resolve) => { finish = resolve; }) : true;
    } });
    recorder.recordFunnel("install_intent"); const first = recorder.flush(); recorder.recordFunnel("docs_search"); timers.at(-1)!();
    finish!(false); await first; expect(timers).toHaveLength(3); timers.at(-1)!(); await Promise.resolve(); await Promise.resolve(); expect(sent).toHaveLength(2);
    expect(sent[0].events.find((event) => event.event === "funnel_usage")!.counters.install_intent).toBe(1);
    const second = sent[1].events.find((event) => event.event === "funnel_usage")!.counters;
    expect(second.install_intent).toBe(0); expect(second.docs_search).toBe(1);
  });
});

describe("browser privacy signals", () => {
  it("reads Global Privacy Control and Do Not Track, including the legacy properties", () => {
    expect(privacySignalActive({}, {})).toBe(false);
    expect(privacySignalActive({ globalPrivacyControl: false, doNotTrack: "0" }, { doNotTrack: "unspecified" })).toBe(false);
    expect(privacySignalActive({ globalPrivacyControl: true }, {})).toBe(true);
    expect(privacySignalActive({ doNotTrack: "1" }, {})).toBe(true);
    expect(privacySignalActive({}, { doNotTrack: "1" })).toBe(true);
    expect(privacySignalActive({}, { doNotTrack: "yes" })).toBe(true);
    expect(privacySignalActive({ msDoNotTrack: "1" }, {})).toBe(true);
    expect(privacySignalActive(null, null)).toBe(false);
  });

  it("reads the same two signals from request headers", () => {
    expect(privacySignalHeader(new Headers())).toBe(false);
    expect(privacySignalHeader(new Headers({ "sec-gpc": "0", dnt: "0" }))).toBe(false);
    expect(privacySignalHeader(new Headers({ "Sec-GPC": "1" }))).toBe(true);
    expect(privacySignalHeader(new Headers({ DNT: "1" }))).toBe(true);
  });

  it("counts nothing, reads and writes no install id, and sends nothing while a signal is present", async () => {
    const storage = memoryStorage();
    const reads: string[] = [];
    const watched: StorageLike = {
      getItem: (key) => (reads.push(key), storage.getItem(key)),
      setItem: storage.setItem,
      removeItem: storage.removeItem,
    };
    const { recorder, timers, sent } = recorderWith(watched, [], () => NOW, () => true);
    recorder.record("page_view");
    recorder.record("install_copy");
    recorder.recordFunnel("star_exit");
    await recorder.flush();
    expect(reads).toEqual([]);
    expect(storage.data.size).toBe(0);
    expect(sent).toEqual([]);
    expect(timers).toEqual([]);
    expect(recorder.pending()).toEqual(emptyCounters());
    expect(recorder.pendingFunnel()).toEqual(emptyFunnelCounters());
  });

  it("clears counts and the install id from an earlier visit but keeps the person's own choice", () => {
    const choice = usagePreferenceRecord(true, NOW);
    const storage = memoryStorage({
      [COUNTERS_STORAGE_KEY]: JSON.stringify({ ...emptyCounters(), page_view: 3 }),
      [FUNNEL_STORAGE_KEY]: JSON.stringify({ ...emptyFunnelCounters(), home_view: 1 }),
      [INSTALL_STORAGE_KEY]: JSON.stringify({ id: UUID, createdAt: NOW }),
      [PREFERENCE_STORAGE_KEY]: choice,
    });
    recorderWith(storage, [], () => NOW, () => true);
    expect([...storage.data.keys()]).toEqual([PREFERENCE_STORAGE_KEY]);
    expect(storage.data.get(PREFERENCE_STORAGE_KEY)).toBe(choice);
  });

  it("drops what was queued when a signal appears mid-session, and counts as before without one", async () => {
    let signal = false;
    const storage = memoryStorage();
    const { recorder, sent } = recorderWith(storage, [], () => NOW, () => signal);
    recorder.record("page_view");
    expect(recorder.pending().page_view).toBe(1);
    signal = true;
    await recorder.flush();
    expect(sent).toEqual([]);
    expect(storage.data.size).toBe(0);
    signal = false;
    recorder.record("page_view");
    await recorder.flush();
    expect(sent).toHaveLength(1);
    expect(storage.data.has(INSTALL_STORAGE_KEY)).toBe(true);
  });
});

describe("forwarder and browser privacy signals", () => {
  const envelope = buildEnvelope({ counters: { ...emptyCounters(), page_view: 1 }, installId: UUID, appVersion: "0.9.12", surface: "website", now: NOW });

  it.each([["Sec-GPC"], ["DNT"]])("answers %s: 1 as disabled before reading the body and forwards nothing", async (header) => {
    let bodyRead = false;
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        bodyRead = true;
        controller.enqueue(new TextEncoder().encode(JSON.stringify(envelope)));
        controller.close();
      },
    }, { highWaterMark: 0 });
    const request = new Request("http://localhost/api/product-telemetry", {
      method: "POST",
      headers: { "content-type": "application/json", [header]: "1" },
      body,
      duplex: "half",
    } as RequestInit);
    let forwarded = 0;
    const response = await handleProductTelemetry(request, {
      ingestUrl: CANONICAL_INGEST_URL,
      forward: async () => {
        forwarded += 1;
        return { ok: true };
      },
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ accepted: false, reason: "disabled" });
    expect(bodyRead).toBe(false);
    expect(forwarded).toBe(0);
  });

  it("still forwards a valid batch when the signals are absent or zero", async () => {
    for (const headers of [{} as Record<string, string>, { "sec-gpc": "0", dnt: "0" }]) {
      let forwarded = 0;
      const response = await handleProductTelemetry(
        new Request("http://localhost/api/product-telemetry", {
          method: "POST",
          headers: { "content-type": "application/json", ...headers },
          body: JSON.stringify(envelope),
        }),
        { ingestUrl: CANONICAL_INGEST_URL, forward: async () => ((forwarded += 1), { ok: true }) },
      );
      expect(await response.json()).toEqual({ accepted: true });
      expect(forwarded).toBe(1);
    }
  });
});
