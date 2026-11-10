import { FakeDraftClaimLock } from "./draft-claim-lock.fake";
import { describe, expect, it } from "vitest";
import {
  deleteDraft,
  draftKey,
  getDraft,
  hasFreshDraft,
  listDrafts,
  parseDraftKey,
  saveDraft,
  type AgentDraft,
} from "./community-agent";

class FakeKv {
  readonly draftStores = { CURATED_KV: this, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock() };
  readonly values = new Map<string, string>();
  readonly deleted: string[] = [];

  async get(key: string): Promise<string | null> {
    return this.values.get(key) ?? null;
  }

  async put(key: string, value: string): Promise<void> {
    this.values.set(key, value);
  }

  async list(options?: { prefix?: string; limit?: number }): Promise<{ keys: { name: string }[] }> {
    const prefix = options?.prefix ?? "";
    const limit = options?.limit ?? Number.POSITIVE_INFINITY;
    return {
      keys: [...this.values.keys()]
        .filter((key) => key.startsWith(prefix))
        .slice(0, limit)
        .map((name) => ({ name })),
    };
  }

  async delete(key: string): Promise<void> {
    this.deleted.push(key);
    this.values.delete(key);
  }
}

function draft(overrides: Partial<AgentDraft> = {}): AgentDraft {
  return {
    id: "42",
    type: "linkcheck",
    bodyEn: "review",
    bodyZh: "审阅",
    generatedAt: "2026-07-17T00:00:00.000Z",
    posted: false,
    ...overrides,
  };
}

describe("community-agent draft boundary", () => {
  it("uses the same canonical key for save and freshness checks", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());

    expect([...kv.values.keys()]).toEqual(["draft:linkcheck:42"]);
    await expect(hasFreshDraft(kv, "linkcheck", "42", "2026-07-16T00:00:00.000Z")).resolves.toBe(true);
    await expect(hasFreshDraft(kv, "issue", "42", "2026-07-16T00:00:00.000Z")).resolves.toBe(false);
  });

  it("uses the canonical PR review namespace for freshness checks", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft({ type: "semantic-drift" }));

    expect([...kv.values.keys()]).toEqual(["draft:semantic-drift:42"]);
    await expect(hasFreshDraft(kv, "semantic-drift", "42", "2026-07-16T00:00:00.000Z")).resolves.toBe(true);
    await expect(hasFreshDraft(kv, "pr", "42", "2026-07-16T00:00:00.000Z")).resolves.toBe(false);
  });

  it("accepts only canonical draft namespaces and bounded ids", () => {
    expect(parseDraftKey("draft:semantic-drift:123")).toEqual({ type: "semantic-drift", id: "123" });
    expect(parseDraftKey("agent:state:latest")).toBeNull();
    expect(parseDraftKey("draft:unknown:123")).toBeNull();
    expect(parseDraftKey("draft:linkcheck:bad:id")).toBeNull();
    expect(() => draftKey("linkcheck", "bad:id")).toThrow("invalid draft id");
  });

  it("rejects parseable non-drafts and key/object identity mismatches", async () => {
    const kv = new FakeKv();
    kv.values.set("draft:linkcheck:42", JSON.stringify({ generatedAt: "2026-07-17T00:00:00.000Z" }));
    kv.values.set("draft:linkcheck:43", JSON.stringify(draft()));

    await expect(getDraft(kv, "agent:state:latest")).resolves.toBeNull();
    await expect(getDraft(kv, "draft:linkcheck:42")).resolves.toBeNull();
    await expect(getDraft(kv, "draft:linkcheck:43")).resolves.toBeNull();
    await expect(listDrafts(kv)).resolves.toEqual([]);
  });

  it("never lets the draft deletion helper cross into another KV namespace", async () => {
    const kv = new FakeKv();
    kv.values.set("agent:state:latest", JSON.stringify({ state: "running" }));

    await expect(deleteDraft(kv, "agent:state:latest")).rejects.toThrow("invalid draft key");
    expect(kv.deleted).toEqual([]);
    expect(kv.values.has("agent:state:latest")).toBe(true);

    await saveDraft(kv.draftStores, draft());
    await expect(deleteDraft(kv, "draft:linkcheck:42")).resolves.toBeUndefined();
    expect(kv.deleted).toEqual(["draft:linkcheck:42"]);
  });
});
