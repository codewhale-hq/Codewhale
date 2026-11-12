import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const securityMocks = vi.hoisted(() => ({
  agentChat: vi.fn(),
  getAgentEnv: vi.fn(),
  validateSession: vi.fn(),
}));

vi.mock("@/lib/community-agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./community-agent")>();
  return {
    ...actual,
    agentChat: securityMocks.agentChat,
    getAgentEnv: securityMocks.getAgentEnv,
    validateSession: securityMocks.validateSession,
  };
});

import { POST as adminPost } from "../app/api/admin/post/route";
import { reviewedBodyHash } from "./community-agent";

class FakeKv {
  readonly values = new Map<string, string>();
  readonly reads: string[] = [];
  readonly deleted: string[] = [];

  async get(key: string): Promise<string | null> {
    this.reads.push(key);
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

function inputUrl(input: string | URL | Request): string {
  if (typeof input === "string") return input;
  return input instanceof URL ? input.toString() : input.url;
}

beforeEach(() => {
  securityMocks.agentChat.mockReset();
  securityMocks.getAgentEnv.mockReset();
  securityMocks.validateSession.mockReset();
  securityMocks.validateSession.mockResolvedValue(true);
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

describe("public security boundaries", () => {
  it("rejects a non-draft admin key before any draft KV read or delete", async () => {
    const kv = new FakeKv();
    kv.values.set("agent:state:latest", JSON.stringify({ state: "running" }));
    securityMocks.getAgentEnv.mockResolvedValue({
      CURATED_KV: kv,
      MAINTAINER_TOKEN: "local-fixture-token-0123456789-abcdefghij",
    });

    const response = await adminPost(new Request("https://codewhale.net/api/admin/post", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        cookie: "mt_sid=test-session",
        origin: "https://codewhale.net",
      },
      body: JSON.stringify({ action: "discard", draftKey: "agent:state:latest" }),
    }));

    await expect(response.json()).resolves.toEqual({ error: "invalid draftKey namespace" });
    expect(response.status).toBe(400);
    expect(securityMocks.validateSession).toHaveBeenCalledOnce();
    expect(kv.reads).toEqual([]);
    expect(kv.deleted).toEqual([]);
    expect(kv.values.has("agent:state:latest")).toBe(true);
  });

  it("posts bodyZh when lang=zh and no editedBody is supplied", async () => {
    const draft = {
      id: "42",
      type: "linkcheck",
      targetNumber: 42,
      bodyEn: "English body",
      bodyZh: "中文正文",
      generatedAt: "2026-01-01T00:00:00.000Z",
      posted: false,
    };
    const kv = new FakeKv();
    kv.values.set("draft:linkcheck:42", JSON.stringify(draft));

    const capturedBodies: string[] = [];
    const mockFetch = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      const url = inputUrl(input);
      if (url.includes("/issues/42/comments")) {
        const reqBody = JSON.parse((init?.body as string) ?? "{}");
        capturedBodies.push(reqBody.body);
        return new Response(JSON.stringify({ id: 1 }), { status: 201, headers: { "content-type": "application/json" } });
      }
      throw new Error(`unexpected URL: ${url}`);
    });
    vi.stubGlobal("fetch", mockFetch);

    securityMocks.getAgentEnv.mockResolvedValue({
      CURATED_KV: kv,
      MAINTAINER_TOKEN: "local-fixture-token-0123456789-abcdefghij",
      MAINTAINER_GITHUB_PAT: "ghp_test",
      GITHUB_REPO: "codewhale-hq/CodeWhale",
    });

    const response = await adminPost(new Request("https://codewhale.net/api/admin/post", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        cookie: "mt_sid=test-session",
        origin: "https://codewhale.net",
      },
      body: JSON.stringify({
        action: "post",
        draftKey: "draft:linkcheck:42",
        lang: "zh",
        reviewedSha256: await reviewedBodyHash("中文正文"),
      }),
    }));

    await expect(response.json()).resolves.toMatchObject({ ok: true });
    expect(capturedBodies).toHaveLength(1);
    expect(capturedBodies[0]).toBe("中文正文");
  });
});
