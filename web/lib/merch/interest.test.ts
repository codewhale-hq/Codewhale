import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSession, type CommunityAgentEnv } from "../community-agent";
import { handleAdminMerchInterest, handleMerchInterest, interestOriginAllowed, type MerchInterestEnv, type MerchInterest } from "./interest";
import { MERCH_INTEREST_ITEMS } from "./interest-options";

const PREFIX = "private:merch-interest:v1:";
let records: Map<string, string>, env: MerchInterestEnv;
const put = vi.fn(), get = vi.fn(), list = vi.fn(), remove = vi.fn(), limit = vi.fn();
const input = () => ({ email: "fixture@example.test", country: "China", currency: "cny", locale: "zh", consent: true, website: "", picks: [{ id: "signature-tee", priceChoice: "mid" }, { id: "mousepad", priceChoice: "low" }] });
function request(value: unknown = input(), headers: Record<string, string> = {}) {
  return new Request("https://codewhale.net/api/merch/interest", { method: "POST", headers: { "Content-Type": "application/json", Origin: "https://codewhale.net", "CF-Connecting-IP": "192.0.2.10", ...headers }, body: JSON.stringify(value) });
}
function interestRecords() { return [...records.entries()].filter(([key]) => key.startsWith(PREFIX)); }
beforeEach(() => {
  vi.clearAllMocks();
  records = new Map();
  get.mockImplementation(async (key: string) => records.get(key) ?? null);
  put.mockImplementation(async (key: string, value: string) => { records.set(key, value); });
  remove.mockImplementation(async (key: string) => { records.delete(key); });
  list.mockImplementation(async ({ prefix, limit: pageSize, cursor = "0" }: { prefix: string; limit: number; cursor?: string }) => {
    const keys = [...records.keys()].filter(key => key.startsWith(prefix)).sort();
    const page = keys.slice(Number(cursor), Number(cursor) + pageSize);
    const complete = Number(cursor) + pageSize >= keys.length;
    return { keys: page.map(name => ({ name })), list_complete: complete, cursor: complete ? "" : String(Number(cursor) + pageSize) };
  });
  limit.mockResolvedValue({ success: true });
  env = { CURATED_KV: { get, put, list, delete: remove }, MERCH_INTEREST_LIMITER: { limit } };
  vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("No external calls are allowed in interest tests"); }));
});
afterEach(() => { vi.unstubAllGlobals(); });

describe("durable merch interest registration", () => {
  it("writes consent, unverified status and authoritative currency choices before generic success", async () => {
    const response = await handleMerchInterest(request({ ...input(), email: " Fixture@Example.Test ", country: " China " }), env);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ ok: true });
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(put).toHaveBeenCalledTimes(1);
    const [key, serialized, ttl] = put.mock.calls[0];
    expect(key).toMatch(/^private:merch-interest:v1:[a-f0-9]{64}$/);
    expect(key).not.toContain("fixture");
    expect(ttl).toEqual({ expirationTtl: 15552000 });
    const saved = JSON.parse(serialized) as MerchInterest;
    expect(saved).toMatchObject({ schemaVersion: 1, email: "fixture@example.test", country: "China", currency: "cny", locale: "zh", consent: true, emailVerified: false, picks: [{ id: "signature-tee", priceChoice: "mid", surveyAmount: 69 }, { id: "mousepad", priceChoice: "low", surveyAmount: 29 }] });
    expect(Date.parse(saved.expiresAt) - Date.parse(saved.submittedAt)).toBe(15552000000);
    expect(limit.mock.calls.map(([value]) => value.key)).toEqual([expect.stringMatching(/^merch-interest:ip:[a-f0-9]{64}$/), expect.stringMatching(/^merch-interest:email:[a-f0-9]{64}$/)]);
    expect(fetch).not.toHaveBeenCalled();
  });

  it("replaces repeated email choices without exposing whether it existed", async () => {
    const first = await handleMerchInterest(request(), env);
    const second = await handleMerchInterest(request({ ...input(), currency: "usd", picks: [{ id: "plush", priceChoice: "unsure" }] }), env);
    expect(await first.json()).toEqual(await second.json());
    expect(interestRecords()).toHaveLength(1);
    expect(JSON.parse(interestRecords()[0][1])).toMatchObject({ currency: "usd", picks: [{ id: "plush", priceChoice: "unsure", surveyAmount: null }] });
  });

  it("accepts all registered items and stores USD amounts from shared survey options", async () => {
    const picks = MERCH_INTEREST_ITEMS.map(item => ({ id: item.id, priceChoice: "high" }));
    const response = await handleMerchInterest(request({ ...input(), currency: "usd", locale: "pt-BR", picks }), env);
    expect(response.status).toBe(200);
    const saved = JSON.parse(interestRecords()[0][1]) as MerchInterest;
    expect(saved.picks.map(pick => pick.surveyAmount)).toEqual(MERCH_INTEREST_ITEMS.map(item => item.prices.usd[2]));
  });

  it.each(["storage", "limiter"])("fails closed when %s is absent, including status", async (missing) => {
    if (missing === "storage") delete env.CURATED_KV;
    else delete env.MERCH_INTEREST_LIMITER;
    const status = await handleMerchInterest(new Request("https://codewhale.net/api/merch/interest"), env);
    expect(await status.json()).toEqual({ available: false });
    const denied = request();
    expect((await handleMerchInterest(denied, env)).status).toBe(503);
    expect(denied.bodyUsed).toBe(false);
    expect(put).not.toHaveBeenCalled();
  });

  it.each(["storage", "limiter"])("never returns success when %s throws, and does not echo errors", async (failed) => {
    (failed === "storage" ? put : limit).mockRejectedValue(new Error("fixture@example.test secret provider error"));
    const response = await handleMerchInterest(request(), env);
    expect(response.status).toBe(503);
    expect(await response.text()).not.toMatch(/fixture@example|secret provider/);
    expect(interestRecords()).toHaveLength(0);
  });

  it("blocks a denied native rate limit before reading the body", async () => {
    limit.mockResolvedValue({ success: false });
    const denied = request();
    const response = await handleMerchInterest(denied, env);
    expect(response.status).toBe(429);
    expect(response.headers.get("Retry-After")).toBe("60");
    expect(denied.bodyUsed).toBe(false);
    expect(put).not.toHaveBeenCalled();
  });

  it.each(["", "https://evil.example", "https://codewhale.net.evil.example", "https://codewhale.net/", "http://127.0.0.1:3100"])("blocks origin %s", async (origin) => {
    const response = await handleMerchInterest(request(input(), { Origin: origin }), env);
    expect(response.status).toBe(403);
    expect(limit).not.toHaveBeenCalled();
    expect(put).not.toHaveBeenCalled();
  });

  it("permits the exact configured HTTPS origin and development loopback only", () => {
    const custom = request(input(), { Origin: "https://merch.example.test" });
    expect(interestOriginAllowed(custom, { MERCH_INTEREST_SITE_ORIGIN: "https://merch.example.test" })).toBe(true);
    expect(interestOriginAllowed(request(), { MERCH_INTEREST_SITE_ORIGIN: "https://merch.example.test" })).toBe(false);
    expect(interestOriginAllowed(request(), { MERCH_INTEREST_SITE_ORIGIN: "https://codewhale.net/path" })).toBe(false);
    const local = new Request("http://127.0.0.1:3100/api/merch/interest", { headers: { Origin: "http://127.0.0.1:3100" } });
    expect(interestOriginAllowed(local, {}, true)).toBe(true);
    expect(interestOriginAllowed(local, {}, false)).toBe(false);
    expect(interestOriginAllowed(new Request(local.url, { headers: { Origin: "http://localhost:3100" } }), {}, true)).toBe(false);
  });

  it.each(["http://127.0.0.1:3100", "http://localhost:3100", "http://[::1]:3100", "https://localhost:3100"])("permits explicit same-origin native preview at %s with production semantics", async (siteOrigin) => {
    const localEnv = { ...env, MERCH_INTEREST_SITE_ORIGIN: siteOrigin };
    const local = new Request(siteOrigin + "/api/merch/interest", { method: "POST", headers: { Origin: siteOrigin, "Content-Type": "application/json" }, body: JSON.stringify(input()) });
    expect(interestOriginAllowed(local, localEnv, false)).toBe(true);
    const response = await handleMerchInterest(local, localEnv);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ ok: true });
    expect(interestRecords()).toHaveLength(1);
    const production = request(input(), { Origin: siteOrigin });
    expect(interestOriginAllowed(production, localEnv, false)).toBe(false);
    expect((await handleMerchInterest(production, localEnv)).status).toBe(403);
    expect(put).toHaveBeenCalledTimes(1);
  });

  it("blocks mismatched loopback ports and unencrypted public configured origins", () => {
    const local = new Request("http://127.0.0.1:3101/api/merch/interest", { headers: { Origin: "http://127.0.0.1:3100" } });
    expect(interestOriginAllowed(local, { MERCH_INTEREST_SITE_ORIGIN: "http://127.0.0.1:3100" }, false)).toBe(false);
    const publicHttp = new Request("http://codewhale.net/api/merch/interest", { headers: { Origin: "http://codewhale.net" } });
    expect(interestOriginAllowed(publicHttp, { MERCH_INTEREST_SITE_ORIGIN: "http://codewhale.net" }, false)).toBe(false);
  });

  it.each([
    { email: "invalid" }, { email: "a".repeat(250) + "@example.test" },
    { consent: false }, { consent: "true" }, { country: "" }, { country: "x".repeat(81) },
    { country: "China\nUS" }, { currency: "eur" }, { locale: "unknown" }, { website: "bot.example" },
    { picks: [] }, { picks: [{ id: "unknown" }] },
    { picks: [{ id: "mousepad" }, { id: "mousepad" }] },
    { picks: [{ id: "mousepad", priceChoice: "cheap" }] },
    { picks: [{ id: "mousepad", priceChoice: "low", surveyAmount: 0.01 }] },
  ])("rejects invalid payload %# without a write", async (patch) => {
    expect((await handleMerchInterest(request({ ...input(), ...patch }), env)).status).toBe(422);
    expect(put).not.toHaveBeenCalled();
  });

  it("bounds actual bytes as well as Content-Length and checks the media type exactly", async () => {
    const actual = new Request("https://codewhale.net/api/merch/interest", { method: "POST", headers: { Origin: "https://codewhale.net", "Content-Type": "application/json" }, body: JSON.stringify({ padding: "x".repeat(8192) }) });
    expect((await handleMerchInterest(actual, env)).status).toBe(413);
    expect((await handleMerchInterest(request(input(), { "Content-Length": "8193" }), env)).status).toBe(413);
    expect((await handleMerchInterest(request(input(), { "Content-Type": "application/jsonp" }), env)).status).toBe(415);
    expect(put).not.toHaveBeenCalled();
  });
});

describe("private merch interest list and deletion", () => {
  async function signedIn(): Promise<{ auth: CommunityAgentEnv; headers: Record<string, string> }> {
    const auth = { CURATED_KV: env.CURATED_KV, MAINTAINER_TOKEN: "local-maintainer-fixture" };
    const sid = await createSession(auth.CURATED_KV);
    return { auth, headers: { Cookie: "mt_sid=" + sid } };
  }
  it("requires a real stored maintainer session for reading or deleting contacts", async () => {
    const response = await handleAdminMerchInterest(new Request("https://codewhale.net/api/admin/merch/interest"), env, { MAINTAINER_TOKEN: "fixture", CURATED_KV: env.CURATED_KV });
    expect(response.status).toBe(401);
    expect(list).not.toHaveBeenCalled();
    expect(remove).not.toHaveBeenCalled();
  });
  it("paginates only the private prefix and deletes an opaque id with same-origin CSRF", async () => {
    await handleMerchInterest(request(), env);
    await handleMerchInterest(request({ ...input(), email: "second@example.test" }), env);
    const { auth, headers } = await signedIn();
    const response = await handleAdminMerchInterest(new Request("https://codewhale.net/api/admin/merch/interest?limit=1", { headers }), env, auth);
    expect(response.status).toBe(200);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    const first = await response.json();
    expect(first.entries).toHaveLength(1);
    expect(first.cursor).toBe("1");
    expect(first.entries[0].id).toMatch(/^[a-f0-9]{64}$/);
    const second = await handleAdminMerchInterest(new Request("https://codewhale.net/api/admin/merch/interest?limit=1&cursor=1", { headers }), env, auth);
    expect((await second.json()).cursor).toBeNull();
    const deleting = (origin: string, id: string) => new Request("https://codewhale.net/api/admin/merch/interest", { method: "DELETE", headers: { ...headers, Origin: origin, "Content-Type": "application/json" }, body: JSON.stringify({ id }) });
    expect((await handleAdminMerchInterest(deleting("https://evil.example", first.entries[0].id), env, auth)).status).toBe(403);
    expect((await handleAdminMerchInterest(deleting("https://codewhale.net", "agent:state:latest"), env, auth)).status).toBe(422);
    const removed = await handleAdminMerchInterest(deleting("https://codewhale.net", first.entries[0].id), env, auth);
    expect(await removed.json()).toEqual({ ok: true });
    expect(interestRecords()).toHaveLength(1);
    expect(list).toHaveBeenCalledWith({ prefix: PREFIX, limit: 1, cursor: "1" });
  });
  it("bounds admin pagination and omits corrupt or expired records", async () => {
    await handleMerchInterest(request(), env);
    const [key, serialized] = interestRecords()[0];
    records.set(key, JSON.stringify({ ...JSON.parse(serialized), expiresAt: "2000-01-01T00:00:00.000Z" }));
    records.set(PREFIX + "a".repeat(64), "invalid fixture data");
    const { auth, headers } = await signedIn();
    expect((await handleAdminMerchInterest(new Request("https://codewhale.net/api/admin/merch/interest?limit=51", { headers }), env, auth)).status).toBe(422);
    expect((await handleAdminMerchInterest(new Request("https://codewhale.net/api/admin/merch/interest?cursor=" + "x".repeat(2049), { headers }), env, auth)).status).toBe(422);
    const response = await handleAdminMerchInterest(new Request("https://codewhale.net/api/admin/merch/interest", { headers }), env, auth);
    expect((await response.json()).entries).toEqual([]);
  });
});
