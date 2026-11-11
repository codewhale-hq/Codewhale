import { describe, expect, it } from "vitest";
import { applyDraftLock, type DraftLockRequest, type DraftLockStorage } from "./draft-claim-lock";
import { FakeDraftClaimLock } from "./draft-claim-lock.fake";

/** Storage that yields on every call, so unserialized callers would interleave. */
class YieldingStorage implements DraftLockStorage {
  readonly values = new Map<string, unknown>();
  private async tick() {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  async get<T>(key: string) {
    await this.tick();
    return this.values.get(key) as T | undefined;
  }
  async put<T>(key: string, value: T) {
    await this.tick();
    this.values.set(key, value);
  }
  async delete(key: string) {
    await this.tick();
    return this.values.delete(key);
  }
}

const claim = (token: string, action: "post" | "discard" = "post", leaseMs = 60_000): DraftLockRequest => ({
  op: "claim",
  token,
  action,
  leaseMs,
});

describe("DraftClaimLock", () => {
  it("grants exactly one of many concurrent claims", async () => {
    const ns = new FakeDraftClaimLock();
    const lock = ns.get(ns.idFromName("draft-claim:linkcheck:42"));
    const results = await Promise.all(
      Array.from({ length: 8 }, (_, i) => lock.act(claim(`post:${i}`)))
    );
    expect(results.filter((r) => r.ok)).toHaveLength(1);
    expect(results.filter((r) => !r.ok)).toEqual(Array(7).fill({ ok: false, holder: "post" }));
  });

  it("without the object's one-event-at-a-time gate, the same logic would double-grant", async () => {
    // Documents why the lock must run inside the Durable Object: the fake's
    // serialization is what the input gate provides in production.
    const storage = new YieldingStorage();
    const results = await Promise.all([
      applyDraftLock(storage, 0, claim("post:a")),
      applyDraftLock(storage, 0, claim("post:b")),
    ]);
    expect(results.every((r) => r.ok)).toBe(true);
  });

  it("keeps one lock per draft identity", async () => {
    const ns = new FakeDraftClaimLock();
    const a = ns.get(ns.idFromName("draft-claim:linkcheck:1"));
    const b = ns.get(ns.idFromName("draft-claim:linkcheck:2"));
    expect(await a.act(claim("post:a"))).toEqual({ ok: true });
    expect(await b.act(claim("post:b"))).toEqual({ ok: true });
    expect(await ns.get(ns.idFromName("draft-claim:linkcheck:1")).act(claim("post:c"))).toEqual({
      ok: false,
      holder: "post",
    });
  });

  it("lets a lease expire so a crashed action does not wedge the draft", async () => {
    const ns = new FakeDraftClaimLock();
    const lock = ns.get(ns.idFromName("k"));
    expect(await lock.act(claim("post:a", "post", 15 * 60_000))).toEqual({ ok: true });
    ns.now += 15 * 60_000 - 1;
    expect(await lock.act(claim("discard:b", "discard"))).toEqual({ ok: false, holder: "post" });
    ns.now += 1;
    expect(await lock.act(claim("discard:b", "discard"))).toEqual({ ok: true });
    // The expired holder can no longer release the new holder's lease.
    await lock.act({ op: "release", token: "post:a", holdMs: 0 });
    expect(await lock.act(claim("post:c"))).toEqual({ ok: false, holder: "discard" });
  });

  it("frees the draft on release, and only for the holder's own token", async () => {
    const ns = new FakeDraftClaimLock();
    const lock = ns.get(ns.idFromName("k"));
    await lock.act(claim("post:a"));
    await lock.act({ op: "release", token: "post:other", holdMs: 0 });
    expect(await lock.act(claim("post:b"))).toEqual({ ok: false, holder: "post" });
    await lock.act({ op: "release", token: "post:a", holdMs: 0 });
    expect(await lock.act(claim("post:b"))).toEqual({ ok: true });
  });

  it("holds a recorded decision for holdMs, then frees it", async () => {
    const ns = new FakeDraftClaimLock();
    const lock = ns.get(ns.idFromName("k"));
    await lock.act(claim("discard:a", "discard", 15 * 60_000));
    await lock.act({ op: "release", token: "discard:a", holdMs: 120_000 });
    ns.now += 119_999;
    expect(await lock.act(claim("post:b"))).toEqual({ ok: false, holder: "discard" });
    ns.now += 1;
    expect(await lock.act(claim("post:b"))).toEqual({ ok: true });
  });
});
