import { beforeEach, describe, expect, it } from "vitest";
import { resetAccessCache, teamName } from "../src/access";
import { handleFetch } from "../src/index";
import { createRunner } from "../src/sql";
import { answer, FakeD1, INSTALL_A } from "./support";

const TEAM = "codewhale";
const AUD = "aud-123";
const NOW = new Date("2026-11-12T10:00:00Z");

const b64 = (input: ArrayBuffer | string) => {
  const bytes = typeof input === "string" ? new TextEncoder().encode(input) : new Uint8Array(input);
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

async function signer() {
  const pair = (await crypto.subtle.generateKey({ name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const jwk = (await crypto.subtle.exportKey("jwk", pair.publicKey)) as JsonWebKey;
  const token = async (claims: Record<string, unknown>) => {
    const head = b64(JSON.stringify({ alg: "RS256", kid: "k1" }));
    const body = b64(JSON.stringify(claims));
    const sig = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", pair.privateKey, new TextEncoder().encode(`${head}.${body}`));
    return `${head}.${body}.${b64(sig)}`;
  };
  return { jwk: { ...jwk, kid: "k1" }, token };
}

const good = (extra: Record<string, unknown> = {}) => ({ iss: `https://${TEAM}.cloudflareaccess.com`, aud: [AUD], exp: NOW.getTime() / 1000 + 600, ...extra });

function env(overrides = {}) {
  const queries: string[] = [];
  return {
    queries,
    env: {
      ACCESS_TEAM_DOMAIN: TEAM,
      ACCESS_AUD: AUD,
      ANALYTICS_SQL: {
        async query({ query }: { query: string }) {
          queries.push(query);
          return { data: (answer(query) as Record<string, unknown>[]).map((row) => ({ ...row, index1: INSTALL_A })) };
        },
      },
      DB: new FakeD1(),
      ...overrides,
    },
  };
}

describe("worker", () => {
  beforeEach(() => resetAccessCache());

  it("fails closed when Access is not configured, and rejects missing or bad tokens", async () => {
    const request = new Request("https://stats.test/api/stats");
    expect((await handleFetch(request, { ...env().env, ACCESS_AUD: "" }, NOW)).status).toBe(503);
    expect((await handleFetch(request, env().env, NOW)).status).toBe(403);
    expect((await handleFetch(new Request("https://stats.test/api/stats", { headers: { "cf-access-jwt-assertion": "a.b.c" } }), env().env, NOW)).status).toBe(403);
    expect((await handleFetch(new Request("https://stats.test/api/stats", { method: "POST" }), env().env, NOW)).status).toBe(405);
  });

  it("serves aggregate stats with a valid Access token and never an install id", async () => {
    const { jwk, token } = await signer();
    const realFetch = globalThis.fetch;
    globalThis.fetch = (async () => new Response(JSON.stringify({ keys: [jwk] }))) as typeof fetch;
    try {
      const { env: e, queries } = env();
      const headers = { "cf-access-jwt-assertion": await token(good()) };
      const response = await handleFetch(new Request("https://stats.test/api/stats?days=7", { headers }), e, NOW);
      expect(response.status).toBe(200);
      const text = await response.text();
      expect(text).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i);
      expect(text).not.toMatch(/install_id|index1/);
      const body = JSON.parse(text);
      expect(body.source).toBe("binding");
      expect(body.from).toBe("2026-11-06");
      expect(body.to).toBe("2026-11-12");
      expect(body.rows.length).toBeGreaterThan(10);
      const before = queries.length;
      await handleFetch(new Request("https://stats.test/api/stats?days=7", { headers }), e, NOW);
      expect(queries.length).toBe(before);

      const wrongAud = { "cf-access-jwt-assertion": await token(good({ aud: ["other"] })) };
      expect((await handleFetch(new Request("https://stats.test/api/stats", { headers: wrongAud }), e, NOW)).status).toBe(403);
      const expired = { "cf-access-jwt-assertion": await token(good({ exp: 1 })) };
      expect((await handleFetch(new Request("https://stats.test/api/stats", { headers: expired }), e, NOW)).status).toBe(403);
    } finally {
      globalThis.fetch = realFetch;
    }
  });

  it("reads the kept totals from the rollup store for the trend", async () => {
    const { jwk, token } = await signer();
    const realFetch = globalThis.fetch;
    globalThis.fetch = (async () => new Response(JSON.stringify({ keys: [jwk] }))) as typeof fetch;
    try {
      const { env: e } = env();
      const db = e.DB as FakeD1;
      db.rows.set("a", { day: "2026-01-02", metric: "active_installs", dim: "all", key: "all", value: 7 });
      db.rows.set("b", { day: "2026-01-02", metric: "active_installs", dim: "version", key: "0.9", value: 3 });
      const headers = { "cf-access-jwt-assertion": await token(good()) };
      const response = await handleFetch(new Request("https://stats.test/api/trend?days=400", { headers }), e, NOW);
      expect((await response.json()).rows).toEqual([{ day: "2026-01-02", metric: "active_installs", dim: "all", key: "all", value: 7 }]);
    } finally {
      globalThis.fetch = realFetch;
    }
  });
});

describe("teamName", () => {
  it("accepts the bare team name or the full cloudflareaccess.com host, and refuses anything else", () => {
    expect(teamName({ ACCESS_TEAM_DOMAIN: "codewhale" })).toBe("codewhale");
    expect(teamName({ ACCESS_TEAM_DOMAIN: "https://CodeWhale.cloudflareaccess.com/" })).toBe("codewhale");
    expect(teamName({ ACCESS_TEAM_DOMAIN: "evil.example.com" })).toBe("");
  });
});

describe("createRunner", () => {
  it("prefers the binding, falls back to the REST API with a token, else none", async () => {
    expect(createRunner({ ANALYTICS_SQL: { query: async () => ({ data: [] }) } })?.source).toBe("binding");
    expect(createRunner({})).toBeNull();
    const calls: { url: string; body: string; auth: string }[] = [];
    const runner = createRunner({ CF_ACCOUNT_ID: "acct", CF_API_TOKEN: "tok" }, (async (url: string, init: RequestInit) => {
      calls.push({ url, body: String(init.body), auth: (init.headers as Record<string, string>).Authorization });
      return new Response(JSON.stringify({ data: [{ a: 1 }] }));
    }) as unknown as typeof fetch);
    expect(runner?.source).toBe("rest");
    expect(await runner?.run("SELECT 1")).toEqual([{ a: 1 }]);
    expect(calls[0]).toEqual({ url: "https://api.cloudflare.com/client/v4/accounts/acct/analytics_engine/sql", body: "SELECT 1 FORMAT JSON", auth: "Bearer tok" });
  });
});
