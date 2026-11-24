import type { Env } from "./types";

interface Jwk {
  kid: string;
  kty: string;
  n: string;
  e: string;
}

let certCache: { team: string; at: number; keys: Jwk[] } | null = null;

export function resetAccessCache(): void {
  certCache = null;
}

function b64url(input: string): Uint8Array<ArrayBuffer> {
  const padded = input.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(input.length / 4) * 4, "=");
  const raw = atob(padded);
  return Uint8Array.from(raw, (char) => char.charCodeAt(0));
}

function json(input: string): Record<string, unknown> {
  return JSON.parse(new TextDecoder().decode(b64url(input)));
}

async function certs(team: string, fetchImpl: typeof fetch, now: number): Promise<Jwk[]> {
  if (certCache && certCache.team === team && now - certCache.at < 3_600_000) return certCache.keys;
  const response = await fetchImpl(`https://${team}.cloudflareaccess.com/cdn-cgi/access/certs`);
  if (!response.ok) throw new Error("Access certs unavailable");
  const body = (await response.json()) as { keys?: Jwk[] };
  const keys = Array.isArray(body.keys) ? body.keys : [];
  certCache = { team, at: now, keys };
  return keys;
}

export function teamName(env: Env): string {
  const withoutScheme = (env.ACCESS_TEAM_DOMAIN ?? "")
    .trim()
    .toLowerCase()
    .replace(/^https?:\/\//, "");
  const slash = withoutScheme.indexOf("/");
  const host = slash >= 0 ? withoutScheme.slice(0, slash) : withoutScheme;
  const name = host.replace(/\.cloudflareaccess\.com$/, "");
  return /^[a-z0-9-]+$/.test(name) ? name : "";
}

export function accessConfigured(env: Env): boolean {
  return Boolean(teamName(env) && env.ACCESS_AUD?.trim());
}

export async function verifyAccess(
  request: Request,
  env: Env,
  fetchImpl: typeof fetch = fetch,
  now: number = Date.now(),
): Promise<boolean> {
  const team = teamName(env);
  const aud = env.ACCESS_AUD?.trim();
  const token = request.headers.get("cf-access-jwt-assertion");
  if (!team || !aud || !token) return false;
  const parts = token.split(".");
  if (parts.length !== 3) return false;
  try {
    const header = json(parts[0]);
    const payload = json(parts[1]);
    if (header.alg !== "RS256" || typeof header.kid !== "string") return false;
    const jwk = (await certs(team, fetchImpl, now)).find((key) => key.kid === header.kid);
    if (!jwk) return false;
    const key = await crypto.subtle.importKey(
      "jwk",
      { kty: jwk.kty, n: jwk.n, e: jwk.e, alg: "RS256", ext: true },
      { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
      false,
      ["verify"],
    );
    const ok = await crypto.subtle.verify(
      "RSASSA-PKCS1-v1_5",
      key,
      b64url(parts[2]),
      new TextEncoder().encode(`${parts[0]}.${parts[1]}`),
    );
    if (!ok) return false;
    const audience = Array.isArray(payload.aud) ? payload.aud : [payload.aud];
    const seconds = now / 1000;
    return (
      payload.iss === `https://${team}.cloudflareaccess.com` &&
      audience.includes(aud) &&
      typeof payload.exp === "number" &&
      payload.exp > seconds &&
      (payload.nbf === undefined || (typeof payload.nbf === "number" && payload.nbf <= seconds + 60))
    );
  } catch {
    return false;
  }
}
