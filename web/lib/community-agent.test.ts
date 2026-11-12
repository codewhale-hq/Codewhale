import { describe, it, expect, vi, beforeEach } from "vitest";
import { createSession, logUsage, validateSession } from "./community-agent";

describe("validateSession", () => {
  let mockKV: {
    get: ReturnType<typeof vi.fn>;
    put: ReturnType<typeof vi.fn>;
    list: ReturnType<typeof vi.fn>;
    delete: ReturnType<typeof vi.fn>;
  };

  beforeEach(() => {
    mockKV = {
      get: vi.fn(),
      put: vi.fn(),
      list: vi.fn(),
      delete: vi.fn(),
    };
  });

  const VALID_SID = "a".repeat(40);
  const SESSION_PREFIX = "session:admin:";
  const TOKEN = "local-fixture-token-0123456789-abcdefghij";
  const ROTATED = "rotated-fixture-token-9876543210-zyxwvutsrq";

  it("should return false if kv is undefined", async () => {
    const result = await validateSession(undefined, VALID_SID, TOKEN);
    expect(result).toBe(false);
  });

  it("should return false if sid is undefined or null", async () => {
    const kv = mockKV as unknown as Parameters<typeof validateSession>[0];
    expect(await validateSession(kv, undefined, TOKEN)).toBe(false);
    expect(await validateSession(kv, null, TOKEN)).toBe(false);
  });

  it("should return false if sid is empty", async () => {
    const kv = mockKV as unknown as Parameters<typeof validateSession>[0];
    expect(await validateSession(kv, "", TOKEN)).toBe(false);
  });

  it("should return false if sid length is less than 40", async () => {
    const kv = mockKV as unknown as Parameters<typeof validateSession>[0];
    expect(await validateSession(kv, "a".repeat(39), TOKEN)).toBe(false);
  });

  it("should return false if sid length is greater than 64", async () => {
    const kv = mockKV as unknown as Parameters<typeof validateSession>[0];
    expect(await validateSession(kv, "a".repeat(65), TOKEN)).toBe(false);
  });

  it("should return false if sid contains invalid characters", async () => {
    const kv = mockKV as unknown as Parameters<typeof validateSession>[0];
    // Length 40 but contains invalid char '!'
    expect(await validateSession(kv, "a".repeat(39) + "!", TOKEN)).toBe(false);
  });

  it("should return false if session is not found in KV", async () => {
    const kv = mockKV as unknown as Parameters<typeof validateSession>[0];
    mockKV.get.mockResolvedValue(null);

    const result = await validateSession(kv, VALID_SID, TOKEN);
    expect(result).toBe(false);
    expect(mockKV.get).toHaveBeenCalledWith(SESSION_PREFIX + VALID_SID);
  });

  it("should return true for a session minted under the configured token", async () => {
    const store = new Map<string, string>();
    const kv = {
      get: async (key: string) => store.get(key) ?? null,
      put: async (key: string, value: string) => { store.set(key, value); },
    } as unknown as Parameters<typeof validateSession>[0];
    const sid = await createSession(kv, TOKEN);
    expect(sid).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(await validateSession(kv, sid, TOKEN)).toBe(true);
  });

  it("should refuse the same session once the configured token changes", async () => {
    const store = new Map<string, string>();
    const kv = {
      get: async (key: string) => store.get(key) ?? null,
      put: async (key: string, value: string) => { store.set(key, value); },
    } as unknown as Parameters<typeof validateSession>[0];
    const sid = await createSession(kv, TOKEN);
    expect(await validateSession(kv, sid, ROTATED)).toBe(false);
    expect(await validateSession(kv, sid, "short")).toBe(false);
    expect(await validateSession(kv, sid, undefined)).toBe(false);
    expect(await validateSession(kv, sid, TOKEN)).toBe(true);
  });

  it("should refuse a stored record with no token fingerprint or unparseable content", async () => {
    const kv = mockKV as unknown as Parameters<typeof validateSession>[0];
    for (const raw of [JSON.stringify({ createdAt: Date.now() }), "{}", "not json", JSON.stringify({ tokenFingerprint: 7 })]) {
      mockKV.get.mockResolvedValue(raw);
      expect(await validateSession(kv, VALID_SID, TOKEN)).toBe(false);
    }
  });

  it("should not mint a session under a weak token", async () => {
    const kv = mockKV as unknown as Parameters<typeof createSession>[0];
    for (const weak of [undefined, "", "short", "a".repeat(32)]) {
      expect(await createSession(kv, weak)).toBeNull();
    }
    expect(mockKV.put).not.toHaveBeenCalled();
  });
});

describe("logUsage", () => {
  it("keeps every call when cron tasks log at the same time", async () => {
    // A KV whose reads resolve before either write lands, as overlapping
    // Worker invocations see it.
    const store = new Map<string, string>();
    const kv = {
      get: async (key: string) => { await new Promise((resolve) => setTimeout(resolve, 5)); return store.get(key) ?? null; },
      put: async (key: string, value: string) => { await new Promise((resolve) => setTimeout(resolve, 5)); store.set(key, value); },
    } as unknown as Parameters<typeof logUsage>[0];
    await Promise.all([logUsage(kv, 100, 10), logUsage(kv, 200, 20), logUsage(kv, 300, 30)]);
    const records = [...store.values()].map((value) => JSON.parse(value) as { calls: number; inputTokens: number });
    expect(records.reduce((sum, record) => sum + record.calls, 0)).toBe(3);
    expect(records.reduce((sum, record) => sum + record.inputTokens, 0)).toBe(600);
    expect([...store.keys()].every((key) => key.startsWith(`usage:${new Date().toISOString().slice(0, 10)}`))).toBe(true);
  });
});
