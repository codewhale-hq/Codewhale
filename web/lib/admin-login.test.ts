import { beforeEach, describe, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  getAgentEnv: vi.fn(), safeEqual: vi.fn(), createSession: vi.fn(), limit: vi.fn(),
}));
vi.mock("@/lib/community-agent", () => mocks);
import { POST } from "@/app/api/admin/login/route";

const STRONG_TOKEN = "local-fixture-token-0123456789-abcdefghij";
const LONE_SURROGATES = Array.from({ length: 16 }, (_, index) => String.fromCharCode(0xd800 + index)).join("").repeat(2);

function request(token = "wrong", ip = "127.0.0.1") {
  return new Request("https://codewhale.net/api/admin/login?locale=en", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded", "CF-Connecting-IP": ip },
    body: new URLSearchParams({ token }),
  });
}
beforeEach(() => {
  vi.resetAllMocks();
  mocks.getAgentEnv.mockResolvedValue({ MAINTAINER_TOKEN: STRONG_TOKEN, ADMIN_LOGIN_LIMITER: { limit: mocks.limit }, CURATED_KV: {} });
  mocks.limit.mockResolvedValue({ success: true });
  mocks.safeEqual.mockResolvedValue(false);
  mocks.createSession.mockResolvedValue("session-id");
});
describe("admin login attempt boundary", () => {
  it("bounds repeated guesses independently of token and forged IP", async () => {
    let attempts = 0;
    mocks.limit.mockImplementation(async () => ({ success: ++attempts <= 5 }));
    for (let index = 0; index < 7; index++) {
      const response = await POST(request(`guess-${index}`, `192.0.2.${index}`));
      expect(response.status).toBe(index < 5 ? 303 : 429);
      if (index >= 5) expect(response.headers.get("Retry-After")).toBe("60");
      expect(response.headers.get("Cache-Control")).toBe("no-store");
    }
    expect(mocks.safeEqual).toHaveBeenCalledTimes(5);
    expect(mocks.createSession).not.toHaveBeenCalled();
    expect(mocks.limit.mock.calls.map(([options]) => options.key)).toEqual(Array(7).fill("codewhale-web:admin-login"));
  });
  it("does not parse or compare credentials after refusal", async () => {
    mocks.limit.mockResolvedValue({ success: false });
    const req = request(STRONG_TOKEN);
    const read = vi.spyOn(req, "text");
    expect((await POST(req)).status).toBe(429);
    expect(read).not.toHaveBeenCalled();
    expect(mocks.safeEqual).not.toHaveBeenCalled();
    expect(mocks.createSession).not.toHaveBeenCalled();
  });
  it("fails closed if the binding is missing or unavailable", async () => {
    mocks.getAgentEnv.mockResolvedValueOnce({ MAINTAINER_TOKEN: STRONG_TOKEN });
    expect((await POST(request())).status).toBe(503);
    mocks.limit.mockRejectedValue(new Error("binding unavailable"));
    expect((await POST(request())).status).toBe(503);
    expect(mocks.safeEqual).not.toHaveBeenCalled();
    expect(mocks.createSession).not.toHaveBeenCalled();
  });
  it("refuses to run with a configured token below the minimum strength", async () => {
    const weakTokens = [
      "x", "configured", "a".repeat(31), "a".repeat(32), " ".repeat(40), "password".repeat(4),
      "Password1".repeat(4), "0123456".repeat(5), "\u{1F433}".repeat(16), "L".repeat(600), LONE_SURROGATES,
    ];
    for (const weak of weakTokens) {
      mocks.getAgentEnv.mockResolvedValueOnce({ MAINTAINER_TOKEN: weak, ADMIN_LOGIN_LIMITER: { limit: mocks.limit }, CURATED_KV: {} });
      mocks.safeEqual.mockResolvedValue(true);
      const response = await POST(request(weak));
      expect(response.status).toBe(503);
      expect(response.headers.get("Cache-Control")).toBe("no-store");
      expect(await response.text()).toContain("at least 32 characters");
    }
    expect(mocks.limit).not.toHaveBeenCalled();
    expect(mocks.safeEqual).not.toHaveBeenCalled();
    expect(mocks.createSession).not.toHaveBeenCalled();
  });
  it("accepts a token of exactly the minimum length", async () => {
    const exact = "abcdefghijklmnopqrstuvwxyz012345";
    expect(exact).toHaveLength(32);
    mocks.getAgentEnv.mockResolvedValueOnce({ MAINTAINER_TOKEN: exact, ADMIN_LOGIN_LIMITER: { limit: mocks.limit }, CURATED_KV: {} });
    mocks.safeEqual.mockResolvedValue(true);
    expect((await POST(request(exact))).status).toBe(303);
  });
  it("accepts a 32-character hex secret with fewer than 16 distinct characters", async () => {
    const hex = "3f9a3f9a1c7e1c7e3f9a1c7e5b2d5b2d";
    expect(new Set(hex).size).toBeLessThan(16);
    mocks.getAgentEnv.mockResolvedValueOnce({ MAINTAINER_TOKEN: hex, ADMIN_LOGIN_LIMITER: { limit: mocks.limit }, CURATED_KV: {} });
    mocks.safeEqual.mockResolvedValue(true);
    expect((await POST(request(hex))).status).toBe(303);
  });
  it("retains the authenticated session and redirect after an allowed attempt", async () => {
    mocks.safeEqual.mockResolvedValue(true);
    const response = await POST(request(STRONG_TOKEN));
    expect(response.status).toBe(303);
    expect(response.headers.get("Location")).toBe("https://codewhale.net/en/admin");
    const cookie = response.headers.get("Set-Cookie");
    expect(cookie).toContain("mt_sid=session-id");
    expect(cookie).toContain("HttpOnly");
    expect(cookie).toContain("Secure");
    expect(cookie?.toLowerCase()).toContain("samesite=strict");
    expect(mocks.createSession).toHaveBeenCalledOnce();
  });
});
