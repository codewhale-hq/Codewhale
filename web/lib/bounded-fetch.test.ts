import { FakeDraftClaimLock } from "./draft-claim-lock.fake";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchBoundedText } from "./bounded-body";
import { runDigest } from "./community-agent-tasks";
import { runLinkCheck } from "./content-watch";
import { deriveFactsFromRemote } from "./facts-drift";

afterEach(() => vi.unstubAllGlobals());

/** A peer that accepts the request and never answers until aborted. */
function hangingFetch() {
  return vi.fn((_: unknown, init?: RequestInit) => new Promise<Response>((_, reject) => {
    init?.signal?.addEventListener("abort", () => reject(init.signal!.reason), { once: true });
  }));
}

describe("bounded outbound reads", () => {
  it("gives up on a silent peer at the deadline and refuses an oversized body", async () => {
    vi.stubGlobal("fetch", hangingFetch());
    const started = Date.now();
    await expect(fetchBoundedText("https://slow.test", {}, { timeoutMs: 50 })).rejects.toThrow();
    expect(Date.now() - started).toBeLessThan(2_000);

    vi.stubGlobal("fetch", vi.fn(async () => new Response("x".repeat(64))));
    await expect(fetchBoundedText("https://big.test", {}, { maxBytes: 16 })).rejects.toThrow("payload too large");
    vi.stubGlobal("fetch", vi.fn(async () => new Response("gone", { status: 404 })));
    expect(await fetchBoundedText("https://missing.test")).toEqual({ ok: false, status: 404, text: "" });
  });

  it("puts every cron read of other hosts under a deadline", async () => {
    // Callers swallow transport errors, so record rather than assert inside.
    const seen = vi.fn(async () => new Response("{}", { status: 500 }));
    vi.stubGlobal("fetch", seen);
    await deriveFactsFromRemote();
    await runLinkCheck({ CURATED_KV: { get: async () => null, put: async () => undefined } } as never);
    const kv = { get: async () => null, put: async () => undefined, delete: async () => undefined, list: async () => ({ keys: [], list_complete: true }) };
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" } as never).catch(() => undefined);
    expect(seen.mock.calls.length).toBeGreaterThan(1);
    const unbounded = seen.mock.calls
      .filter((call) => !((call as unknown[])[1] as RequestInit | undefined)?.signal)
      .map((call) => String((call as unknown[])[0]));
    expect(unbounded).toEqual([]);
  });
});
