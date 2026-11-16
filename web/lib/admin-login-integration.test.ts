import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ getAgentEnv: vi.fn(), limit: vi.fn() }));
vi.mock("@/lib/community-agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/community-agent")>();
  return {
    ...actual,
    getAgentEnv: mocks.getAgentEnv,
    safeEqual: vi.fn(actual.safeEqual),
    createSession: vi.fn(actual.createSession),
  };
});

import { POST } from "@/app/api/admin/login/route";
import { createSession, safeEqual, validateSession } from "@/lib/community-agent";

const FIXTURE_TOKEN = "local-admin-integration-fixture-0123456789";
const sessions = new Map<string, string>();
const kv = {
  put: vi.fn(async (key: string, value: string) => { sessions.set(key, value); }),
  get: vi.fn(async (key: string) => sessions.get(key) ?? null),
};
const sessionKv = kv as unknown as NonNullable<Parameters<typeof createSession>[0]>;

function request(token = FIXTURE_TOKEN, ip = "192.0.2.1") {
  return new Request("https://admin.example.test/api/admin/login?locale=en", {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      "CF-Connecting-IP": ip,
    },
    body: new URLSearchParams({ token }),
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  sessions.clear();
  const attempts = new Map<string, number>();
  mocks.limit.mockImplementation(async ({ key }: { key: string }) => {
    const count = (attempts.get(key) ?? 0) + 1;
    attempts.set(key, count);
    return { success: count <= 5 };
  });
  mocks.getAgentEnv.mockResolvedValue({
    MAINTAINER_TOKEN: FIXTURE_TOKEN,
    ADMIN_LOGIN_LIMITER: { limit: mocks.limit },
    CURATED_KV: sessionKv,
  });
});

describe("admin login with real credential and session helpers", () => {
  it("issues a cookie for a stored session that the real validator accepts", async () => {
    const response = await POST(request());
    expect(response.status).toBe(303);
    expect(response.headers.get("Location")).toBe("https://admin.example.test/en/admin");
    const sid = response.cookies.get("mt_sid")?.value;
    expect(sid).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(sessions.size).toBe(1);
    expect(await validateSession(sessionKv, sid, FIXTURE_TOKEN)).toBe(true);
    expect(safeEqual).toHaveBeenCalledExactlyOnceWith(FIXTURE_TOKEN, FIXTURE_TOKEN);
    expect(createSession).toHaveBeenCalledExactlyOnceWith(sessionKv, FIXTURE_TOKEN);
    const cookie = response.headers.get("Set-Cookie")?.toLowerCase();
    expect(cookie).toContain("httponly");
    expect(cookie).toContain("secure");
    expect(cookie).toContain("samesite=strict");
    expect(cookie).toContain("max-age=86400");
    expect(kv.put.mock.calls[0]).toEqual([
      expect.any(String), expect.any(String), { expirationTtl: 86400 },
    ]);
    sessions.clear();
    expect(await validateSession(sessionKv, sid, FIXTURE_TOKEN)).toBe(false);
  });

  it("stops honouring a session after the maintainer token is rotated", async () => {
    const sid = (await POST(request())).cookies.get("mt_sid")?.value;
    expect(await validateSession(sessionKv, sid, FIXTURE_TOKEN)).toBe(true);
    expect(await validateSession(sessionKv, sid, "rotated-integration-fixture-9876543210")).toBe(false);
  });

  it("rejects a wrong credential without creating a session", async () => {
    const response = await POST(request("wrong-local-fixture"));
    expect(response.status).toBe(303);
    expect(response.headers.get("Location")).toBe("https://admin.example.test/en/admin?err=1");
    expect(response.headers.get("Set-Cookie")).toBeNull();
    expect(safeEqual).toHaveBeenCalledExactlyOnceWith("wrong-local-fixture", FIXTURE_TOKEN);
    expect(createSession).not.toHaveBeenCalled();
    expect(kv.put).not.toHaveBeenCalled();
    expect(sessions.size).toBe(0);
  });

  it("refuses a correct credential after five attempts without reading or comparing it", async () => {
    for (let index = 0; index < 5; index++) {
      expect((await POST(request(`wrong-${index}`, `192.0.2.${index}`))).status).toBe(303);
    }
    const denied = request(FIXTURE_TOKEN, "198.51.100.1");
    const response = await POST(denied);
    expect(response.status).toBe(429);
    expect(response.headers.get("Retry-After")).toBe("60");
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(denied.bodyUsed).toBe(false);
    expect(safeEqual).toHaveBeenCalledTimes(5);
    expect(createSession).not.toHaveBeenCalled();
    expect(kv.put).not.toHaveBeenCalled();
    expect(sessions.size).toBe(0);
    expect(new Set(mocks.limit.mock.calls.map(([options]) => options.key)).size).toBe(1);
  });

  it.each(["missing", "failed"])("fails closed before comparison when the limiter is %s", async (mode) => {
    if (mode === "missing") {
      mocks.getAgentEnv.mockResolvedValue({ MAINTAINER_TOKEN: FIXTURE_TOKEN, CURATED_KV: sessionKv });
    } else {
      mocks.limit.mockRejectedValue(new Error("fixture limiter unavailable"));
    }
    const denied = request();
    const response = await POST(denied);
    expect(response.status).toBe(503);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(response.headers.get("Set-Cookie")).toBeNull();
    expect(denied.bodyUsed).toBe(false);
    expect(safeEqual).not.toHaveBeenCalled();
    expect(createSession).not.toHaveBeenCalled();
    expect(kv.put).not.toHaveBeenCalled();
    expect(sessions.size).toBe(0);
  });
});
