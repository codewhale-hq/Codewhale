import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ getEnv: vi.fn(), safeEqual: vi.fn(), limit: vi.fn() }));
vi.mock("@/lib/kv", () => ({ getEnv: mocks.getEnv }));
vi.mock("@/lib/community-agent", () => ({ safeEqual: mocks.safeEqual }));
vi.mock("@/lib/facts-drift", () => ({ runFactsDrift: vi.fn() }));
vi.mock("@/lib/content-watch", () => ({ runLinkCheck: vi.fn(), runSemanticDrift: vi.fn() }));
import { GET } from "@/app/api/cron/route";

function request(secret = "wrong", ip = "127.0.0.1") {
  return new Request("https://codewhale.net/api/cron?task=nope", {
    headers: { "x-cron-secret": secret, "CF-Connecting-IP": ip },
  });
}

beforeEach(() => {
  vi.resetAllMocks();
  mocks.getEnv.mockResolvedValue({ CRON_SECRET: "configured-cron-secret", ADMIN_LOGIN_LIMITER: { limit: mocks.limit } });
  mocks.limit.mockResolvedValue({ success: true });
  mocks.safeEqual.mockResolvedValue(false);
});

describe("manual cron trigger attempt boundary", () => {
  it("bounds repeated guesses independently of the secret and forged IP", async () => {
    let attempts = 0;
    mocks.limit.mockImplementation(async () => ({ success: ++attempts <= 5 }));
    for (let index = 0; index < 7; index++) {
      const response = await GET(request(`guess-${index}`, `192.0.2.${index}`));
      expect(response.status).toBe(index < 5 ? 401 : 429);
      if (index >= 5) expect(response.headers.get("Retry-After")).toBe("60");
      expect(response.headers.get("Cache-Control")).toBe("no-store");
    }
    expect(mocks.safeEqual).toHaveBeenCalledTimes(5);
    expect(mocks.limit.mock.calls.map(([options]) => options.key)).toEqual(Array(7).fill("codewhale-web:cron-secret"));
  });
  it("does not compare the secret after refusal", async () => {
    mocks.limit.mockResolvedValue({ success: false });
    expect((await GET(request("configured-cron-secret"))).status).toBe(429);
    expect(mocks.safeEqual).not.toHaveBeenCalled();
  });
  it("fails closed if the binding is missing or unavailable", async () => {
    mocks.getEnv.mockResolvedValueOnce({ CRON_SECRET: "configured-cron-secret" });
    expect((await GET(request())).status).toBe(503);
    mocks.limit.mockRejectedValue(new Error("binding unavailable"));
    expect((await GET(request())).status).toBe(503);
    expect(mocks.safeEqual).not.toHaveBeenCalled();
  });
  it("stays disabled without a configured secret and never consults the limiter", async () => {
    mocks.getEnv.mockResolvedValue({ ADMIN_LOGIN_LIMITER: { limit: mocks.limit } });
    expect((await GET(request())).status).toBe(503);
    expect(mocks.limit).not.toHaveBeenCalled();
  });
  it("still reaches task validation after an allowed, correct attempt", async () => {
    mocks.safeEqual.mockResolvedValue(true);
    expect((await GET(request("configured-cron-secret"))).status).toBe(400);
  });
});
