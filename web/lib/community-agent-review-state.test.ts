import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  agentChat: vi.fn(),
  getAgentEnv: vi.fn(),
  validateSession: vi.fn(),
  fetchRepoStats: vi.fn(),
}));

vi.mock("@/lib/community-agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./community-agent")>();
  return {
    ...actual,
    agentChat: mocks.agentChat,
    getAgentEnv: mocks.getAgentEnv,
    validateSession: mocks.validateSession,
  };
});

vi.mock("@/lib/github", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./github")>();
  return { ...actual, fetchRepoStats: mocks.fetchRepoStats };
});

import { POST as adminPost } from "../app/api/admin/post/route";
import {
  isPublishedDigest,
  listDrafts,
  MAX_LISTED_DRAFTS,
  reviewedBodyHash,
  saveDraft,
  type AgentDraft,
} from "./community-agent";
import { runDigest, runDupes, runTriage } from "./community-agent-tasks";
import { FakeDraftClaimLock } from "./draft-claim-lock.fake";

/** In-memory KV that pages like Cloudflare KV (max 1000 keys per list call). */
class FakeKv {
  readonly draftStores = { CURATED_KV: this, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock() };
  readonly values = new Map<string, string>();
  failPutsMatching: RegExp | null = null;
  /** Yield to the event loop inside every call, like a network round trip. */
  yieldEachOp = false;
  pageSize = 1000;
  failListAtCursor: string | null = null;
  /** Awaited before a put lands, to hold a request at an exact step. */
  beforePut: ((key: string, value: string) => Promise<void>) | null = null;

  private async tick() {
    if (this.yieldEachOp) await new Promise((resolve) => setTimeout(resolve, 0));
  }

  async get(key: string): Promise<string | null> {
    await this.tick();
    return this.values.get(key) ?? null;
  }

  async put(key: string, value: string): Promise<void> {
    await this.tick();
    if (this.beforePut) await this.beforePut(key, value);
    if (this.failPutsMatching?.test(key)) throw new Error("kv put failed");
    this.values.set(key, value);
  }

  async list(options?: { prefix?: string; limit?: number; cursor?: string }) {
    if (options?.cursor && options.cursor === this.failListAtCursor) throw new Error("kv list failed");
    const prefix = options?.prefix ?? "";
    const limit = Math.min(options?.limit ?? 1000, this.pageSize);
    const start = options?.cursor ? Number(options.cursor) : 0;
    const all = [...this.values.keys()].filter((k) => k.startsWith(prefix)).sort();
    const page = all.slice(start, start + limit);
    const next = start + page.length;
    const complete = next >= all.length;
    return {
      keys: page.map((name) => ({ name })),
      list_complete: complete,
      ...(complete ? {} : { cursor: String(next) }),
    };
  }

  async delete(key: string): Promise<void> {
    this.values.delete(key);
  }
}

function draft(overrides: Partial<AgentDraft> = {}): AgentDraft {
  return {
    id: "42",
    type: "triage",
    targetNumber: 42,
    bodyEn: "English body",
    bodyZh: "中文正文",
    generatedAt: "2026-01-01T00:00:00.000Z",
    posted: false,
    ...overrides,
  };
}

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
}

function inputUrl(input: string | URL | Request): string {
  if (typeof input === "string") return input;
  return input instanceof URL ? input.toString() : input.url;
}

function adminRequest(body: BodyInit, headers: Record<string, string> = {}): Request {
  return new Request("https://codewhale.net/api/admin/post", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      cookie: "mt_sid=test-session",
      origin: "https://codewhale.net",
      ...headers,
    },
    body,
    // Required by undici for a streamed request body.
    ...(body instanceof ReadableStream ? { duplex: "half" } : {}),
  } as RequestInit);
}

function postBody(value: Record<string, unknown>): string {
  return JSON.stringify(value);
}

/**
 * An admin action as the admin page sends it: bound to the stored draft text
 * in the selected language, as shown to the maintainer.
 */
async function act(kv: FakeKv, fields: Record<string, unknown>): Promise<Response> {
  let reviewedSha256 = fields.reviewedSha256;
  if (reviewedSha256 === undefined) {
    const raw = kv.values.get(String(fields.draftKey));
    const stored = raw ? (JSON.parse(raw) as AgentDraft) : null;
    reviewedSha256 = await reviewedBodyHash(
      stored ? (fields.lang === "zh" ? stored.bodyZh : stored.bodyEn) : ""
    );
  }
  return adminPost(adminRequest(postBody({ ...fields, reviewedSha256 })));
}

function stubAdminEnv(kv: FakeKv, lock?: FakeDraftClaimLock) {
  mocks.getAgentEnv.mockResolvedValue({
    CURATED_KV: kv,
    ...(lock ? { DRAFT_CLAIM_LOCK: lock } : {}),
    MAINTAINER_TOKEN: "configured",
    MAINTAINER_GITHUB_PAT: "ghp_test",
    GITHUB_REPO: "codewhale-hq/CodeWhale",
  });
}

/**
 * GitHub stub that records every comment/issue creation. Reads (the lookup
 * for an earlier unknown-outcome post) answer `existing` and are not posts.
 */
function stubGitHub(status = 201, existing: unknown[] = []) {
  const posts: string[] = [];
  const fetchMock = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
    const url = inputUrl(input);
    if ((init?.method ?? "GET") === "GET") return jsonResponse(existing);
    posts.push(url);
    if (url.endsWith("/issues")) {
      return jsonResponse({ number: 900, html_url: "https://github.com/codewhale-hq/CodeWhale/issues/900" }, status);
    }
    return jsonResponse({ id: 1 }, status);
  });
  vi.stubGlobal("fetch", fetchMock);
  return posts;
}

const DIGEST_MODEL_OUTPUT = {
  titleEn: "Weekly Digest",
  titleZh: "每周摘要",
  summaryEn: "A quiet week.",
  summaryZh: "平静的一周。",
  sections: [{ heading: "Shipped", items: ["PR #1: fix"] }],
};

function stubDigestSources() {
  mocks.fetchRepoStats.mockResolvedValue({ stars: 1, forks: 1 });
  vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request) => {
    const url = inputUrl(input);
    if (url.includes("/issues?") || url.includes("/pulls?")) return jsonResponse([]);
    throw new Error(`unexpected URL: ${url}`);
  }));
}

function onlyKey(kv: FakeKv, prefix: string): string {
  const keys = [...kv.values.keys()].filter((k) => k.startsWith(prefix));
  expect(keys).toHaveLength(1);
  return keys[0];
}

beforeEach(() => {
  mocks.agentChat.mockReset();
  mocks.getAgentEnv.mockReset();
  mocks.fetchRepoStats.mockReset();
  mocks.validateSession.mockReset();
  mocks.validateSession.mockResolvedValue(true);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("weekly digest publication requires maintainer approval", () => {
  it("stages the cron digest unapproved and publishes it only when the maintainer posts it", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    // Model output cannot self-approve.
    mocks.agentChat.mockResolvedValue({
      content: JSON.stringify({ ...DIGEST_MODEL_OUTPUT, approved: true }),
      usage: { input: 1, output: 1 },
    });

    await expect(runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" })).resolves.toMatchObject({ ok: true });
    const recordKey = onlyKey(kv, "digest:weekly-");
    const staged: unknown = JSON.parse(kv.values.get(recordKey)!);
    expect(staged).toMatchObject({ approved: false });
    expect(isPublishedDigest(staged)).toBe(false);

    stubAdminEnv(kv);
    const posts = stubGitHub();
    const draftKey = onlyKey(kv, "draft:digest:");
    const res = await act(kv, { action: "post", draftKey, lang: "en" });
    await expect(res.json()).resolves.toMatchObject({ ok: true, published: true, number: 900 });
    expect(posts).toHaveLength(1);
    const published = JSON.parse(kv.values.get(recordKey)!);
    expect(isPublishedDigest(published)).toBe(true);
    // Only the reviewed language is published.
    expect(published).toMatchObject({ approvedLang: "en" });
  });

  it("records the Chinese review as the published language when posted from the zh admin", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });

    stubAdminEnv(kv);
    stubGitHub();
    const draftKey = onlyKey(kv, "draft:digest:");
    const res = await act(kv, { action: "post", draftKey, lang: "zh" });
    await expect(res.json()).resolves.toMatchObject({ ok: true, published: true });
    expect(JSON.parse(kv.values.get(onlyKey(kv, "digest:weekly-"))!)).toMatchObject({ approvedLang: "zh" });
  });

  it("refuses to discard a posted digest, which would silently unpublish it", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });

    stubAdminEnv(kv);
    stubGitHub();
    const draftKey = onlyKey(kv, "draft:digest:");
    expect((await act(kv, { action: "post", draftKey, lang: "en" })).status).toBe(200);

    const discard = await act(kv, { action: "discard", draftKey });
    expect(discard.status).toBe(409);
    expect(isPublishedDigest(JSON.parse(kv.values.get(onlyKey(kv, "digest:weekly-"))!))).toBe(true);
  });

  it("does not publish an edited digest's unedited model text", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });

    stubAdminEnv(kv);
    stubGitHub();
    const draftKey = onlyKey(kv, "draft:digest:");
    const res = await act(kv, { action: "post", draftKey, lang: "en", editedBody: "# Edited" });
    // The maintainer is told why the digest is not on /digest.
    await expect(res.json()).resolves.toMatchObject({
      ok: true,
      published: false,
      warning: expect.stringContaining("/digest"),
    });
    expect(isPublishedDigest(JSON.parse(kv.values.get(onlyKey(kv, "digest:weekly-"))!))).toBe(false);
  });

  it("discarding a digest removes the staged record and the cron does not regenerate it", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });

    stubAdminEnv(kv);
    const draftKey = onlyKey(kv, "draft:digest:");
    const res = await act(kv, { action: "discard", draftKey });
    await expect(res.json()).resolves.toMatchObject({ ok: true, action: "discarded" });
    expect([...kv.values.keys()].filter((k) => k.startsWith("digest:weekly-"))).toEqual([]);

    mocks.agentChat.mockClear();
    await expect(runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" })).resolves.toMatchObject({ skipped: true });
    expect(mocks.agentChat).not.toHaveBeenCalled();
    expect(kv.values.has(draftKey)).toBe(false);
  });

  it("surfaces a GitHub failure on a digest post as a 502 and does not publish", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });

    stubAdminEnv(kv);
    stubGitHub(422);
    const draftKey = onlyKey(kv, "draft:digest:");
    const res = await act(kv, { action: "post", draftKey, lang: "en" });
    expect(res.status).toBe(502);
    const payload = await res.json();
    expect(payload.ok).toBeUndefined();
    expect(payload.error).toMatch(/^GitHub 422/);
    expect(isPublishedDigest(JSON.parse(kv.values.get(onlyKey(kv, "digest:weekly-"))!))).toBe(false);
  });

  it("finds a digest issue an unknown-outcome attempt created and publishes without a second issue", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });
    stubAdminEnv(kv);
    const draftKey = onlyKey(kv, "draft:digest:");
    stubGitHub(504);
    expect((await act(kv, { action: "post", draftKey, lang: "en" })).status).toBe(502);
    // The claim and "posting" marker lapse (15 minutes in production).
    for (const key of [...kv.values.keys()]) if (/^draft-(claim|resolved):digest:/.test(key)) kv.values.delete(key);

    const body = JSON.parse(kv.values.get(draftKey)!).bodyEn as string;
    const title = body.split("\n")[0].replace(/^#+\s*/, "").trim();
    const posts = stubGitHub(201, [{ number: 901, html_url: "https://github.com/codewhale-hq/CodeWhale/issues/901", title, body }]);
    const retry = await act(kv, { action: "post", draftKey, lang: "en" });
    await expect(retry.json()).resolves.toMatchObject({ ok: true, number: 901, published: true });
    expect(posts).toEqual([]);
  });

  it("never publishes an older record when a later run saved its draft but not its record", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });
    const recordKey = onlyKey(kv, "digest:weekly-");
    const recordA = kv.values.get(recordKey)!;

    // Run B saves its draft, then its record write fails: draft B + record A.
    mocks.agentChat.mockResolvedValue({
      content: JSON.stringify({ ...DIGEST_MODEL_OUTPUT, titleEn: "Digest B", summaryEn: "Run B." }),
      usage: { input: 1, output: 1 },
    });
    kv.failPutsMatching = /^digest:weekly-/;
    await expect(runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" })).resolves.toMatchObject({ ok: false });
    kv.failPutsMatching = null;
    const draftKey = onlyKey(kv, "draft:digest:");
    expect(JSON.parse(kv.values.get(draftKey)!).bodyEn).toContain("Digest B");
    expect(kv.values.get(recordKey)).toBe(recordA);

    stubAdminEnv(kv);
    const bodies: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (_input: string | URL | Request, init?: RequestInit) => {
      bodies.push(String(init?.body));
      return jsonResponse({ number: 900, html_url: "https://github.com/codewhale-hq/CodeWhale/issues/900" }, 201);
    }));
    const res = await act(kv, { action: "post", draftKey, lang: "en" });
    await expect(res.json()).resolves.toMatchObject({
      ok: true,
      published: false,
      warning: expect.stringContaining("does not match the reviewed text"),
    });
    // GitHub got the reviewed B; /digest shows nothing rather than A.
    expect(bodies).toHaveLength(1);
    expect(JSON.parse(bodies[0]).title).toBe("Digest B");
    expect(isPublishedDigest(JSON.parse(kv.values.get(recordKey)!))).toBe(false);
  });

  it("publishes only the fields of a record that matches the reviewed text", async () => {
    const kv = new FakeKv();
    stubDigestSources();
    mocks.agentChat.mockResolvedValue({ content: JSON.stringify(DIGEST_MODEL_OUTPUT), usage: { input: 1, output: 1 } });
    await runDigest({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });
    const recordKey = onlyKey(kv, "digest:weekly-");
    kv.values.set(recordKey, JSON.stringify({ ...JSON.parse(kv.values.get(recordKey)!), extra: "<script>" }));

    stubAdminEnv(kv);
    stubGitHub();
    const draftKey = onlyKey(kv, "draft:digest:");
    await expect((await act(kv, { action: "post", draftKey, lang: "zh" })).json()).resolves.toMatchObject({
      published: true,
    });
    const published = JSON.parse(kv.values.get(recordKey)!);
    expect(isPublishedDigest(published)).toBe(true);
    expect(published).not.toHaveProperty("extra");
    expect(published).toMatchObject({ approvedLang: "zh", titleZh: DIGEST_MODEL_OUTPUT.titleZh });
  });

  it("hides legacy records that were never approved", () => {
    const legacy = { ...DIGEST_MODEL_OUTPUT, weekId: "2026-W01", generatedAt: "2026-01-05T00:00:00.000Z" };
    expect(isPublishedDigest(legacy)).toBe(false);
    expect(isPublishedDigest({ ...legacy, approved: true })).toBe(false);
    expect(isPublishedDigest({ ...legacy, approved: true, approvedLang: "en" })).toBe(true);
    expect(
      isPublishedDigest({ ...legacy, approved: true, approvedLang: "en", sections: [{ heading: "x", items: [1] }] })
    ).toBe(false);
  });
});

describe("resolved drafts are not resurrected by the cron", () => {
  function stubIssues(updatedAt: string) {
    vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request) => {
      const url = inputUrl(input);
      if (!url.includes("/issues?")) throw new Error(`unexpected URL: ${url}`);
      return jsonResponse([{
        number: 42,
        title: "Issue",
        body: "body",
        updated_at: updatedAt,
        html_url: "https://github.com/codewhale-hq/CodeWhale/issues/42",
        labels: [],
      }]);
    }));
  }

  beforeEach(() => {
    mocks.agentChat.mockResolvedValue({
      content: JSON.stringify({ bodyEn: "review", bodyZh: "审阅" }),
      usage: { input: 1, output: 1 },
    });
  });

  it("does not redraft a discarded triage item", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    const res = await act(kv, { action: "discard", draftKey: "draft:triage:42" });
    expect(res.status).toBe(200);

    stubIssues("2020-01-01T00:00:00.000Z");
    await expect(runTriage({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" })).resolves.toMatchObject({ processed: 0, skipped: 1 });
    expect(mocks.agentChat).not.toHaveBeenCalled();
    expect(kv.values.has("draft:triage:42")).toBe(false);
  });

  it("does not turn a posted triage draft back into a pending one after the issue updates", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    stubGitHub();
    const res = await act(kv, { action: "post", draftKey: "draft:triage:42", lang: "en" });
    expect(res.status).toBe(200);

    // Our own comment bumps updated_at past the draft's generatedAt.
    stubIssues(new Date().toISOString());
    await runTriage({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" });
    expect(mocks.agentChat).not.toHaveBeenCalled();
    expect(JSON.parse(kv.values.get("draft:triage:42")!)).toMatchObject({ posted: true });
  });

  it("drafts again when the item has new activity well after the maintainer's decision", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    stubGitHub();
    expect((await act(kv, { action: "post", draftKey: "draft:triage:42" })).status).toBe(200);

    // New commits or a reply a day later.
    stubIssues(new Date(Date.now() + 24 * 60 * 60 * 1000).toISOString());
    await expect(runTriage({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" })).resolves.toMatchObject({ processed: 1 });
    expect(JSON.parse(kv.values.get("draft:triage:42")!)).toMatchObject({ posted: false, bodyEn: "review" });

    // The fresh draft can be posted.
    const posts = stubGitHub();
    expect((await act(kv, { action: "post", draftKey: "draft:triage:42" })).status).toBe(200);
    expect(posts).toHaveLength(1);
  });

  it("does not let the dupes run overwrite a posted dupes draft", async () => {
    const kv = new FakeKv();
    kv.values.set("draft:dupes:7", JSON.stringify(draft({ id: "7", type: "dupes", targetNumber: 7, posted: true })));
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(
      [1, 2, 7].map((n) => ({ number: n, title: `t${n}`, updated_at: "2020-01-01T00:00:00.000Z", html_url: `u${n}` }))
    )));
    mocks.agentChat.mockResolvedValue({
      content: JSON.stringify({ suggestions: [{ targetNumber: 1, duplicateNumber: 7, reason: "r", bodyEn: "dup", bodyZh: "重复" }] }),
      usage: { input: 1, output: 1 },
    });

    await expect(runDupes({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: new FakeDraftClaimLock(), DEEPSEEK_API_KEY: "k" })).resolves.toMatchObject({ ok: true, processed: 0 });
    expect(JSON.parse(kv.values.get("draft:dupes:7")!)).toMatchObject({ posted: true });
  });
});

describe("admin post action is idempotent and bounded", () => {
  it("refuses to post a draft that is already posted", async () => {
    const kv = new FakeKv();
    kv.values.set("draft:triage:42", JSON.stringify(draft({ posted: true })));
    stubAdminEnv(kv);
    const posts = stubGitHub();

    const res = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(res.status).toBe(409);
    expect(posts).toEqual([]);
  });

  it("returns ok when saving state fails after GitHub accepted the comment, and a retry does not post twice", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    const posts = stubGitHub();
    kv.failPutsMatching = /^draft:triage:42$/;

    const first = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(first.status).toBe(200);
    await expect(first.json()).resolves.toMatchObject({ ok: true, action: "posted", warning: expect.any(String) });

    const retry = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(retry.status).toBe(409);
    expect(posts).toHaveLength(1);
  });

  it("releases the claim when GitHub rejects the post so the maintainer can retry", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    stubGitHub(422);

    const failed = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(failed.status).toBe(502);

    const posts = stubGitHub();
    const retry = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(retry.status).toBe(200);
    expect(posts).toHaveLength(1);
  });

  it.each([500, 502, 504, 429])(
    "keeps the claim when GitHub answers %i, since the post may have been created",
    async (status) => {
      const kv = new FakeKv();
      await saveDraft(kv.draftStores, draft());
      stubAdminEnv(kv);
      const posts = stubGitHub(status);

      const failed = await act(kv, { action: "post", draftKey: "draft:triage:42" });
      expect(failed.status).toBe(502);
      await expect(failed.json()).resolves.toMatchObject({ error: expect.stringContaining("check GitHub before retrying") });

      stubGitHub();
      const retry = await act(kv, { action: "post", draftKey: "draft:triage:42" });
      expect(retry.status).toBe(409);
      expect(posts).toHaveLength(1);
    }
  );

  it("never posts twice when two requests for the same draft race", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    kv.yieldEachOp = true;
    stubAdminEnv(kv);
    const posts = stubGitHub();

    const results = await Promise.all([
      act(kv, { action: "post", draftKey: "draft:triage:42" }),
      act(kv, { action: "post", draftKey: "draft:triage:42" }),
    ]);
    const statuses = results.map((r) => r.status);
    // This fake KV makes every write visible at once, so the claim is exact
    // here; on Workers KV it is best effort (see claimDraft).
    expect(statuses.filter((s) => s === 200)).toHaveLength(posts.length);
    expect(posts.length).toBeLessThanOrEqual(1);
    expect(statuses.every((s) => s === 200 || s === 409)).toBe(true);

    // Refused claims are released, so the maintainer's retry posts exactly once.
    if (posts.length === 0) {
      expect((await act(kv, { action: "post", draftKey: "draft:triage:42" })).status).toBe(200);
    }
    expect(posts).toHaveLength(1);
    expect((await act(kv, { action: "post", draftKey: "draft:triage:42" })).status).toBe(409);
    expect(posts).toHaveLength(1);
  });

  /**
   * Hold two admin actions at the reviewer's interleaving: both pass the
   * "no decision yet" check, the first takes its claim and reaches GitHub,
   * and only then does the second's claim write land.
   */
  function raceAfterAbsenceChecks(kv: FakeKv) {
    let prechecks = 0;
    let bothChecked!: () => void;
    const bothCheckedP = new Promise<void>((resolve) => (bothChecked = resolve));
    let firstAtGitHub!: () => void;
    const firstAtGitHubP = new Promise<void>((resolve) => (firstAtGitHub = resolve));
    const realGet = kv.get.bind(kv);
    kv.get = async (key: string) => {
      const value = await realGet(key);
      if (key.startsWith("draft-resolved:") && ++prechecks === 2) bothChecked();
      return value;
    };
    let claimPuts = 0;
    kv.beforePut = async (key, value) => {
      const isClaim = key.startsWith("draft-claim:") || (key.startsWith("draft-resolved:") && value.includes('"claim"'));
      if (!isClaim) return;
      // A discard only ever overlaps a post already at GitHub. Gate it on
      // that directly so the test does not depend on which request the
      // scheduler lets claim first (a discard claiming first would park the
      // post forever waiting for a GitHub call that never happens).
      if (value.startsWith("discard:") || value.includes('"discard')) {
        await firstAtGitHubP;
        return;
      }
      claimPuts += 1;
      await (claimPuts === 1 ? bothCheckedP : firstAtGitHubP);
    };
    const posts: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request) => {
      posts.push(inputUrl(input));
      firstAtGitHub();
      // Let the second request finish its claim while this post is in flight.
      await new Promise((resolve) => setTimeout(resolve, 20));
      return jsonResponse({ id: 1 }, 201);
    }));
    return posts;
  }

  it("refuses a second post whose claim lands while the first is at GitHub", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    kv.yieldEachOp = true;
    stubAdminEnv(kv);
    const posts = raceAfterAbsenceChecks(kv);

    const results = await Promise.all([
      act(kv, { action: "post", draftKey: "draft:triage:42" }),
      act(kv, { action: "post", draftKey: "draft:triage:42" }),
    ]);

    expect(results.map((r) => r.status).sort()).toEqual([200, 409]);
    expect(posts).toHaveLength(1);
  });

  it("refuses a discard that overlaps an in-flight post of the same draft", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    kv.yieldEachOp = true;
    stubAdminEnv(kv);
    const posts = raceAfterAbsenceChecks(kv);

    const [post, discard] = await Promise.all([
      act(kv, { action: "post", draftKey: "draft:triage:42" }),
      act(kv, { action: "discard", draftKey: "draft:triage:42" }),
    ]);

    expect(post.status).toBe(200);
    expect(discard.status).toBe(409);
    expect(posts).toHaveLength(1);
    expect(JSON.parse(kv.values.get("draft-resolved:triage:42")!)).toMatchObject({ state: "posted" });
    expect(JSON.parse(kv.values.get("draft:triage:42")!)).toMatchObject({ posted: true });
  });

  it("claims without KV list, whose results can lag writes by up to a minute", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    const posts = stubGitHub();
    // A stale listing that never shows claim keys must not affect the claim.
    const realList = kv.list.bind(kv);
    kv.list = async (options) => {
      if (options?.prefix?.startsWith("draft-claim:")) throw new Error("claim must not list");
      return realList(options);
    };

    const res = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(res.status).toBe(200);
    expect(posts).toHaveLength(1);
    expect([...kv.values.keys()].filter((k) => k.startsWith("draft-claim:"))).toEqual([]);
  });

  it("answers a claim it cannot read back as retryable, not as a lost race, and the retry posts", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    const posts = stubGitHub();
    // The first read-back of the request's own claim misses the write.
    const realGet = kv.get.bind(kv);
    let claimReads = 0;
    kv.get = async (key: string) => {
      if (key.startsWith("draft-claim:") && ++claimReads === 2) return null;
      return realGet(key);
    };

    const first = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(first.status).toBe(503);
    expect(posts).toEqual([]);
    expect([...kv.values.keys()].filter((k) => k.startsWith("draft-claim:") || k.startsWith("draft-resolved:"))).toEqual([]);

    const retry = await act(kv, { action: "post", draftKey: "draft:triage:42" });
    expect(retry.status).toBe(200);
    expect(posts).toHaveLength(1);
  });

  it("refuses a post while another request's claim is visible, and leaves that claim alone", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    const posts = stubGitHub();
    kv.values.set("draft-claim:triage:42", "other-request");

    for (const action of ["post", "discard"]) {
      const res = await act(kv, { action, draftKey: "draft:triage:42" });
      expect(res.status).toBe(409);
    }
    expect(posts).toEqual([]);
    expect(kv.values.get("draft-claim:triage:42")).toBe("other-request");
  });

  it("treats a repeated discard as a no-op, including one that sees the marker only at claim time", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    stubGitHub();

    const first = await act(kv, { action: "discard", draftKey: "draft:triage:42" });
    expect(first.status).toBe(200);
    // A double click whose request still reads the draft (its delete not yet
    // visible), first past the marker, then with the marker missed by the
    // route's own check and seen only by the claim.
    const staleDraft = JSON.stringify(draft());
    kv.values.set("draft:triage:42", staleDraft);
    const again = await act(kv, { action: "discard", draftKey: "draft:triage:42" });
    expect(again.status).toBe(200);
    await expect(again.json()).resolves.toEqual({ ok: true, action: "discarded" });

    kv.values.set("draft:triage:42", staleDraft);
    const realGet = kv.get.bind(kv);
    let markerReads = 0;
    kv.get = async (key: string) => {
      if (key.startsWith("draft-resolved:") && ++markerReads === 1) return null;
      return realGet(key);
    };
    const racing = await act(kv, { action: "discard", draftKey: "draft:triage:42" });
    expect(racing.status).toBe(200);
    expect(JSON.parse(kv.values.get("draft-resolved:triage:42")!)).toMatchObject({ state: "discarded" });
    expect([...kv.values.keys()].filter((k) => k.startsWith("draft-claim:"))).toEqual([]);
  });

  it("refuses an action on a draft regenerated after the page loaded, before calling GitHub", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft({ bodyEn: "reviewed A" }));
    stubAdminEnv(kv);
    const shownA = await reviewedBodyHash("reviewed A");
    // The cron regenerates the pending draft while the admin page shows A.
    await saveDraft(kv.draftStores, draft({ bodyEn: "unseen B", generatedAt: "2026-01-02T00:00:00.000Z" }));
    const posts = stubGitHub();

    for (const action of ["post", "discard"]) {
      const res = await act(kv, { action, draftKey: "draft:triage:42", lang: "en", reviewedSha256: shownA });
      expect(res.status).toBe(409);
      await expect(res.json()).resolves.toMatchObject({ error: expect.stringContaining("changed") });
    }
    expect(posts).toEqual([]);
    expect(JSON.parse(kv.values.get("draft:triage:42")!)).toMatchObject({ bodyEn: "unseen B", posted: false });
    expect([...kv.values.keys()].filter((k) => k.startsWith("draft-resolved:") || k.startsWith("draft-claim:"))).toEqual([]);
  });

  it("requires the reviewed-text hash", async () => {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    stubAdminEnv(kv);
    const posts = stubGitHub();

    const res = await adminPost(adminRequest(postBody({ action: "post", draftKey: "draft:triage:42" })));
    expect(res.status).toBe(400);
    expect(posts).toEqual([]);
  });

  it("answers malformed JSON with a JSON 400", async () => {
    const kv = new FakeKv();
    stubAdminEnv(kv);

    const res = await adminPost(adminRequest("{not json"));
    expect(res.status).toBe(400);
    await expect(res.json()).resolves.toEqual({ error: "invalid JSON body" });
  });

  it("counts streamed body bytes instead of trusting a missing Content-Length", async () => {
    const kv = new FakeKv();
    stubAdminEnv(kv);
    const chunk = new TextEncoder().encode("x".repeat(40_000));
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(chunk);
        controller.enqueue(chunk);
        controller.close();
      },
    });

    const res = await adminPost(adminRequest(stream));
    expect(res.status).toBe(413);
  });
});

describe("admin claims with the DRAFT_CLAIM_LOCK Durable Object bound", () => {
  const KEY = "draft:triage:42";

  async function lockedAdmin(status = 201) {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    kv.yieldEachOp = true;
    const lock = new FakeDraftClaimLock();
    stubAdminEnv(kv, lock);
    const claimKeysWritten: string[] = [];
    kv.beforePut = async (key) => {
      if (key.startsWith("draft-claim:")) claimKeysWritten.push(key);
    };
    const posts = stubGitHub(status);
    return { kv, lock, posts, claimKeysWritten };
  }

  /** What a location whose KV has not yet seen the first request's writes reads. */
  function forgetDecision(kv: FakeKv) {
    kv.values.set(KEY, JSON.stringify(draft()));
    kv.values.delete("draft-resolved:triage:42");
  }

  it("lets exactly one of several concurrent posts through, without KV claim keys", async () => {
    const { kv, lock, posts, claimKeysWritten } = await lockedAdmin();

    const results = await Promise.all(
      Array.from({ length: 4 }, () => act(kv, { action: "post", draftKey: KEY }))
    );

    expect(results.map((r) => r.status).sort()).toEqual([200, 409, 409, 409]);
    expect(posts).toHaveLength(1);
    expect(claimKeysWritten).toEqual([]);
    expect(lock.calls.filter((c) => c.op === "claim")).toHaveLength(4);
  });

  it("refuses a retry that reads stale KV after a recorded post, until the hold ends", async () => {
    const { kv, lock, posts } = await lockedAdmin();
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(200);

    forgetDecision(kv);
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(409);
    expect(posts).toHaveLength(1);

    // Past the hold the KV marker has propagated; a location that really
    // lacks it (a reopened draft) may act again.
    lock.now += 2 * 60 * 1000;
    forgetDecision(kv);
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(200);
    expect(posts).toHaveLength(2);
  });

  it("keeps the lease after an unknown GitHub outcome, and lets it expire", async () => {
    const { kv, lock, posts } = await lockedAdmin(502);
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(502);
    stubGitHub();

    // Even without the KV "posting" marker, the lease refuses a retry...
    kv.values.delete("draft-resolved:triage:42");
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(409);
    expect(posts).toHaveLength(1);

    // ...until it expires, so the draft is not wedged for good.
    lock.now += 15 * 60 * 1000;
    const retryPosts = stubGitHub();
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(200);
    expect(retryPosts).toHaveLength(1);
  });

  it("looks for the earlier attempt's comment after an unknown outcome instead of posting it twice", async () => {
    const { kv, lock } = await lockedAdmin(502);
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(502);
    lock.now += 15 * 60 * 1000;
    kv.values.delete("draft-resolved:triage:42");

    // GitHub did create it: the late retry records it and posts nothing.
    const posts = stubGitHub(201, [{ id: 7, body: "English body" }]);
    const retry = await act(kv, { action: "post", draftKey: KEY });
    expect(retry.status).toBe(200);
    await expect(retry.json()).resolves.toMatchObject({ ok: true, action: "posted", warning: expect.stringContaining("not posted again") });
    expect(posts).toEqual([]);
    expect(JSON.parse(kv.values.get("draft-resolved:triage:42")!).state).toBe("posted");
    expect(kv.values.has("draft-post-unknown:triage:42")).toBe(false);
  });

  it("posts nothing when the lookup for an earlier attempt fails, and frees the draft", async () => {
    const { kv, lock } = await lockedAdmin(502);
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(502);
    lock.now += 15 * 60 * 1000;
    kv.values.delete("draft-resolved:triage:42");

    const posts: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      if ((init?.method ?? "GET") === "GET") return jsonResponse({ message: "down" }, 503);
      posts.push(inputUrl(input));
      return jsonResponse({ id: 1 }, 201);
    }));
    const blocked = await act(kv, { action: "post", draftKey: KEY });
    expect(blocked.status).toBe(502);
    await expect(blocked.json()).resolves.toMatchObject({ error: expect.stringContaining("nothing was posted") });
    expect(posts).toEqual([]);

    // Once GitHub answers and has no such comment, the retry posts once.
    const retryPosts = stubGitHub();
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(200);
    expect(retryPosts).toHaveLength(1);
  });

  it("frees the draft at once when GitHub definitely rejected the post", async () => {
    const { kv } = await lockedAdmin(422);
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(502);
    const posts = stubGitHub();
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(200);
    expect(posts).toHaveLength(1);
  });

  it("treats a double-clicked discard as one discard, concurrent or repeated", async () => {
    const { kv, posts } = await lockedAdmin();

    const both = await Promise.all([
      act(kv, { action: "discard", draftKey: KEY }),
      act(kv, { action: "discard", draftKey: KEY }),
    ]);
    expect(both.map((r) => r.status)).toEqual([200, 200]);
    for (const res of both) await expect(res.json()).resolves.toEqual({ ok: true, action: "discarded" });
    expect(JSON.parse(kv.values.get("draft-resolved:triage:42")!)).toMatchObject({ state: "discarded" });

    // A third click served from stale KV hits the post-discard hold.
    forgetDecision(kv);
    expect((await act(kv, { action: "discard", draftKey: KEY })).status).toBe(200);
    expect(posts).toEqual([]);
  });

  it("refuses a discard while a post holds the draft, and a post after a discard", async () => {
    const { kv, posts } = await lockedAdmin();
    // Send the discard only once the post holds the draft (it has reached
    // GitHub), so the outcome does not depend on which request the
    // scheduler delivers to the lock first.
    const postP = act(kv, { action: "post", draftKey: KEY });
    await vi.waitFor(() => expect(posts).toHaveLength(1));
    const discard = await act(kv, { action: "discard", draftKey: KEY });
    const post = await postP;
    expect([post.status, discard.status]).toEqual([200, 409]);
    expect(posts).toHaveLength(1);

    const other = await lockedAdmin();
    expect((await act(other.kv, { action: "discard", draftKey: KEY })).status).toBe(200);
    forgetDecision(other.kv);
    expect((await act(other.kv, { action: "post", draftKey: KEY })).status).toBe(409);
    expect(other.posts).toEqual([]);
  });

  it("fails closed when the lock cannot be reached", async () => {
    const { kv, lock, posts } = await lockedAdmin();
    lock.get = () => ({ act: async () => { throw new Error("DO unavailable"); } }) as never;
    const res = await act(kv, { action: "post", draftKey: KEY });
    expect(res.status).toBe(500);
    expect(posts).toEqual([]);
    expect(kv.values.get("draft-resolved:triage:42")).toBeUndefined();
  });
});

describe("admin draft queue", () => {
  function fill(kv: FakeKv, count: number) {
    for (let i = 1; i <= count; i++) {
      kv.values.set(`draft:triage:${i}`, JSON.stringify(draft({ id: String(i), targetNumber: i })));
    }
  }

  it("follows the KV list cursor past the first page", async () => {
    const kv = new FakeKv();
    kv.pageSize = 100;
    fill(kv, 250);

    await expect(listDrafts(kv)).resolves.toHaveLength(250);
  });

  it("reads at most MAX_LISTED_DRAFTS drafts in one request", async () => {
    const kv = new FakeKv();
    fill(kv, MAX_LISTED_DRAFTS + 1_000);
    const get = vi.spyOn(kv, "get");

    await expect(listDrafts(kv)).resolves.toHaveLength(MAX_LISTED_DRAFTS);
    expect(get).toHaveBeenCalledTimes(MAX_LISTED_DRAFTS);
  });

  it("keeps the drafts already read when a later list page fails", async () => {
    const kv = new FakeKv();
    kv.pageSize = 100;
    kv.failListAtCursor = "200";
    fill(kv, 250);
    vi.spyOn(console, "error").mockImplementation(() => {});

    await expect(listDrafts(kv)).resolves.toHaveLength(200);
  });
});

describe("post retry durability and generation admission", () => {
  const KEY = "draft:triage:42";
  async function prepared() {
    const kv = new FakeKv();
    await saveDraft(kv.draftStores, draft());
    const lock = new FakeDraftClaimLock();
    stubAdminEnv(kv, lock);
    return { kv, lock };
  }
  it("posts nothing when an earlier receipt cannot be read", async () => {
    const { kv } = await prepared();
    const originalGet = kv.get.bind(kv);
    kv.get = async (key) => { if (key.startsWith("draft-post-unknown:")) throw new Error("unavailable"); return originalGet(key); };
    const posts = stubGitHub();
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(503);
    expect(posts).toEqual([]);
  });
  it("writes the attempt before dispatch and refuses an unavailable receipt write", async () => {
    const { kv } = await prepared();
    kv.failPutsMatching = /^draft-post-unknown:/;
    const posts = stubGitHub();
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(502);
    expect(posts).toEqual([]);
  });
  it("finds an earlier post past page one even when KV lost the receipt", async () => {
    const { kv, lock } = await prepared();
    stubGitHub(504);
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(502);
    lock.now += 15 * 60 * 1000;
    kv.values.delete("draft-resolved:triage:42");
    kv.values.delete("draft-post-unknown:triage:42");
    const posts: string[] = [], pages: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      const url = inputUrl(input);
      if (init?.method === "POST") { posts.push(url); return jsonResponse({ id: 2 }, 201); }
      const page = new URL(url).searchParams.get("page"); pages.push(page!);
      return jsonResponse(page === "1" ? Array.from({ length: 100 }, () => ({ body: "another body" })) : [{ id: 7, body: "English body" }]);
    }));
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(200);
    expect(pages).toEqual(["1", "2"]);
    expect(posts).toEqual([]);
  });
  it("a bounded incomplete reconciliation never licenses another post", async () => {
    const { kv, lock } = await prepared();
    stubGitHub(504);
    await act(kv, { action: "post", draftKey: KEY });
    lock.now += 15 * 60 * 1000;
    kv.values.delete("draft-resolved:triage:42");
    const posts: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      if (init?.method === "POST") posts.push(inputUrl(input));
      return jsonResponse(Array.from({ length: 100 }, () => ({ body: "another body" })));
    }));
    expect((await act(kv, { action: "post", draftKey: KEY })).status).toBe(502);
    expect(posts).toEqual([]);
  });
  it("refuses changed retry text while the earlier attempt is unresolved", async () => {
    const { kv, lock } = await prepared();
    stubGitHub(504);
    await act(kv, { action: "post", draftKey: KEY });
    lock.now += 15 * 60 * 1000;
    kv.values.delete("draft-resolved:triage:42");
    const posts = stubGitHub();
    expect((await act(kv, { action: "post", draftKey: KEY, editedBody: "different body" })).status).toBe(409);
    expect(posts).toEqual([]);
  });
  it("admits one overlapping generation batch and holds completed KV propagation", async () => {
    const kv = new FakeKv(), lock = new FakeDraftClaimLock();
    const env = { CURATED_KV: kv, DRAFT_CLAIM_LOCK: lock, DEEPSEEK_API_KEY: "k" };
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse([{ number: 42, title: "issue", body: "body", updated_at: "2020-01-01T00:00:00.000Z", html_url: "https://github.com/codewhale-hq/CodeWhale/issues/42", labels: [] }])));
    let finish!: (value: unknown) => void;
    mocks.agentChat.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; })).mockResolvedValue({ content: JSON.stringify({ bodyEn: "other", bodyZh: "另一个" }), usage: { input: 1, output: 1 } });
    const first = runTriage(env);
    await vi.waitFor(() => expect(mocks.agentChat).toHaveBeenCalledOnce());
    const second = await runTriage(env);
    expect(second).toMatchObject({ processed: 0, skipped: 1 });
    finish({ content: JSON.stringify({ bodyEn: "generated", bodyZh: "生成" }), usage: { input: 1, output: 1 } });
    expect(await first).toMatchObject({ processed: 1 });
    kv.values.delete(KEY); // another location's stale KV must not spend again.
    expect(await runTriage(env)).toMatchObject({ processed: 0, skipped: 1 });
    expect(mocks.agentChat).toHaveBeenCalledOnce();
  });
  it("missing or failed durable generation admission starts no provider call", async () => {
    const kv = new FakeKv();
    const reads = vi.fn(); vi.stubGlobal("fetch", reads);
    expect(await runTriage({ CURATED_KV: kv, DEEPSEEK_API_KEY: "k" })).toMatchObject({ skipped: true });
    const broken = { idFromName: () => ({ toString: () => "x" }), get: () => ({ act: async () => { throw new Error("down"); } }) };
    expect(await runTriage({ CURATED_KV: kv, DRAFT_CLAIM_LOCK: broken, DEEPSEEK_API_KEY: "k" })).toMatchObject({ skipped: true });
    expect(reads).not.toHaveBeenCalled(); expect(mocks.agentChat).not.toHaveBeenCalled();
  });
});
