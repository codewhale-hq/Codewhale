import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";

vi.mock("./community-agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./community-agent")>();
  return { ...actual, agentChat: vi.fn() };
});

import { FakeDraftClaimLock } from "./draft-claim-lock.fake";
import { runLinkCheck, runSemanticDrift, watchDraftId } from "./content-watch";
import { agentChat, deleteDraft, draftStorageKey, markDraftResolved, type AgentDraft } from "./community-agent";

const agentChatMock = agentChat as Mock;

function createFakeKV() {
  const store = new Map<string, string>();
  return {
    store,
    lock: new FakeDraftClaimLock(),
    async get(key: string): Promise<string | null> {
      return store.get(key) ?? null;
    },
    async put(key: string, value: string): Promise<void> {
      store.set(key, value);
    },
    async list(opts?: { prefix?: string; limit?: number }): Promise<{ keys: { name: string }[] }> {
      const prefix = opts?.prefix ?? "";
      const limit = opts?.limit ?? 100;
      return {
        keys: [...store.keys()]
          .filter((k) => k.startsWith(prefix))
          .slice(0, limit)
          .map((name) => ({ name })),
      };
    },
    async delete(key: string): Promise<void> {
      store.delete(key);
    },
  };
}

type FakeKV = ReturnType<typeof createFakeKV>;

function draftEntries(kv: FakeKV): [string, AgentDraft][] {
  return [...kv.store.entries()]
    .filter(([key]) => key.startsWith("draft:"))
    .map(([key, value]) => [key, JSON.parse(value) as AgentDraft] as [string, AgentDraft]);
}

function okResponse(body = ""): Response {
  return new Response(body, { status: 200 });
}

describe("runLinkCheck draft identity", () => {
  let kv: FakeKV;

  beforeEach(() => {
    kv = createFakeKV();
    // Break exactly one target; everything else answers 200 to HEAD.
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        return url === "https://buymeacoffee.com/hmbown"
          ? new Response("broken", { status: 500 })
          : okResponse();
      })
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("creates a linkcheck draft under the canonical key on first run", async () => {
    const result = await runLinkCheck({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: kv.lock });

    expect(result.ok).toBe(true);
    expect(result.broken).toBe(1);

    const drafts = draftEntries(kv);
    expect(drafts).toHaveLength(1);
    const [key, draft] = drafts[0];
    expect(draft.type).toBe("linkcheck");
    expect(draft.targetUrl).toBe("https://buymeacoffee.com/hmbown");
    expect(key).toBe(draftStorageKey(draft));
    expect(key.startsWith("draft:linkcheck:")).toBe(true);
    expect(draft.id.length).toBeLessThanOrEqual(80);
  });

  it("creates no duplicate when the same breakage is seen again", async () => {
    await runLinkCheck({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: kv.lock });
    const second = await runLinkCheck({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: kv.lock });

    expect(second.broken).toBe(1); // still broken…
    expect(draftEntries(kv)).toHaveLength(1); // …but not re-drafted
  });
});

describe("runSemanticDrift draft identity", () => {
  let kv: FakeKV;

  const env = () => ({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: kv.lock, DEEPSEEK_API_KEY: "test-key" });

  function mockSources() {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const { hostname } = new URL(String(input));
        if (hostname === "api.github.com") return new Response("[]", { status: 200 });
        if (hostname === "raw.githubusercontent.com") return okResponse("# Changelog\n- things");
        return okResponse("<html><body>site copy</body></html>");
      })
    );
  }

  function mockDrifts(drifts: unknown) {
    agentChatMock.mockResolvedValue({
      content: JSON.stringify({ drifts }),
      usage: { input: 0, output: 0 },
    });
  }

  const finding = {
    page: "homepage",
    claim: "Codewhale supports three modes",
    evidence: "CHANGELOG: modes renamed in 0.9.0",
    suggested_replacement: "Codewhale supports Plan / Work / Operate",
  };

  beforeEach(() => {
    kv = createFakeKV();
    agentChatMock.mockReset();
    mockSources();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("creates a semantic-drift draft under the canonical key on first run", async () => {
    mockDrifts([finding]);
    const result = await runSemanticDrift(env());

    expect(result).toEqual({ ok: true, drafted: 1 });
    const drafts = draftEntries(kv);
    expect(drafts).toHaveLength(1);
    const [key, draft] = drafts[0];
    expect(draft.type).toBe("semantic-drift");
    expect(key).toBe(draftStorageKey(draft));
    expect(key.startsWith("draft:semantic-drift:")).toBe(true);
  });

  it("creates no duplicate when the same finding is reported again", async () => {
    mockDrifts([finding]);
    await runSemanticDrift(env());
    const second = await runSemanticDrift(env());

    expect(second).toEqual({ ok: true, drafted: 0 });
    expect(draftEntries(kv)).toHaveLength(1);
  });

  it("does not count a finding the maintainer already discarded as drafted", async () => {
    mockDrifts([finding]);
    await runSemanticDrift(env());
    const [[key, draft]] = draftEntries(kv);
    await markDraftResolved(kv, "semantic-drift", draft.id, "discarded");
    await deleteDraft(kv, key);

    const second = await runSemanticDrift(env());

    expect(second).toEqual({ ok: true, drafted: 0 });
    expect(draftEntries(kv)).toHaveLength(0);
  });

  it("creates a new draft when the finding's evidence changes", async () => {
    mockDrifts([finding]);
    await runSemanticDrift(env());

    mockDrifts([{ ...finding, evidence: "CHANGELOG: modes renamed in 0.9.1" }]);
    const second = await runSemanticDrift(env());

    expect(second).toEqual({ ok: true, drafted: 1 });
    expect(draftEntries(kv)).toHaveLength(2);
  });

  it("does not collide findings whose truncated slug prefixes are identical", async () => {
    const sharedPrefix = "a".repeat(120);
    const first = { ...finding, claim: `${sharedPrefix}-first-variant` };
    const second = { ...finding, claim: `${sharedPrefix}-second-variant` };
    mockDrifts([first, second]);

    const result = await runSemanticDrift(env());

    expect(result).toEqual({ ok: true, drafted: 2 });
    const drafts = draftEntries(kv);
    expect(drafts).toHaveLength(2);
    const keys = drafts.map(([key]) => key);
    expect(new Set(keys).size).toBe(2);
    for (const [, draft] of drafts) {
      expect(draft.id.length).toBeLessThanOrEqual(80);
    }
  });

  it("bounds excessive model output and skips malformed entries", async () => {
    const flood = Array.from({ length: 500 }, (_, i) => ({
      ...finding,
      claim: `claim number ${i}`,
    }));
    const malformed = [
      { page: "evil-page", claim: "x", evidence: "y", suggested_replacement: "z" },
      { page: "homepage", claim: "", evidence: "y", suggested_replacement: "z" },
      { page: "homepage", claim: "x", evidence: 42, suggested_replacement: "z" },
      "not-an-object",
      null,
    ];
    mockDrifts([...malformed, ...flood]);

    const result = await runSemanticDrift(env());

    expect(result.ok).toBe(true);
    expect(result.drafted).toBe(10); // MAX_DRIFT_DRAFTS_PER_RUN
    expect(draftEntries(kv)).toHaveLength(10);
  });

  it("treats a non-array drifts payload as empty", async () => {
    mockDrifts(undefined);
    const result = await runSemanticDrift(env());
    expect(result).toEqual({ ok: true, drafted: 0 });
  });
});

describe("watchDraftId", () => {
  it("is deterministic for the same identity", async () => {
    const a = await watchDraftId("some-slug", "identity");
    const b = await watchDraftId("some-slug", "identity");
    expect(a).toBe(b);
  });

  it("separates identities that share a long slug prefix", async () => {
    const prefix = `https://example.com/${"a".repeat(120)}`;
    const a = await watchDraftId(prefix, `linkcheck\n${prefix}?x=1`);
    const b = await watchDraftId(prefix, `linkcheck\n${prefix}?x=2`);
    expect(a).not.toBe(b);
    expect(a.length).toBeLessThanOrEqual(80);
    expect(b.length).toBeLessThanOrEqual(80);
  });
});
