/**
 * Private, unverified merch launch opt-ins, separate from orders and payment.
 * Real KV and the native rate limiter are mandatory: no in-memory success path.
 * A new submission replaces that email's choices and expires after 180 days.
 * No email verification, campaign delivery, public counts, or inventory promise.
 * KV is eventually consistent; the maintainer list may lag a successful write.
 */
import { readBoundedBody, BodyReadError } from "../bounded-body";
import { getAgentEnv, validateSession, type CommunityAgentEnv } from "../community-agent";
import { isValidLocale } from "../i18n/config";
import { getEnv, type KVNamespace } from "../kv";
import { maintainerTokenUsable } from "@/lib/maintainer-token";
import { MERCH_INTEREST_ITEMS, MERCH_INTEREST_PRICE_CHOICES, type MerchInterestPriceChoice, type MerchInterestCurrency } from "./interest-options";

const PREFIX = "private:merch-interest:v1:";
const RETENTION_SECONDS = 180 * 24 * 60 * 60;
const MAX_BODY_BYTES = 8192;
type PriceChoice = MerchInterestPriceChoice;
type Currency = MerchInterestCurrency;
type Pick = { id: string; priceChoice: PriceChoice; surveyAmount: number | null };
export type MerchInterest = {
  schemaVersion: 1;
  email: string;
  country: string;
  currency: Currency;
  locale: string;
  consent: true;
  emailVerified: false;
  submittedAt: string;
  expiresAt: string;
  picks: Pick[];
};
export interface MerchInterestEnv {
  CURATED_KV?: KVNamespace;
  MERCH_INTEREST_LIMITER?: { limit(options: { key: string }): Promise<{ success: boolean }> };
  MERCH_INTEREST_SITE_ORIGIN?: string;
}
class InterestError extends Error {
  constructor(readonly status: number, readonly code: string, message: string) { super(message); }
}
function json(data: unknown, status = 200) {
  return Response.json(data, { status, headers: { "Cache-Control": "no-store", ...(status === 429 ? { "Retry-After": "60" } : {}) } });
}
function invalid(): never { throw new InterestError(422, "invalid_request", "Check the interest form and try again."); }
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) invalid();
  return value as Record<string, unknown>;
}
async function body(request: Request) {
  const mediaType = request.headers.get("content-type")?.split(";", 1)[0].trim().toLowerCase();
  if (mediaType !== "application/json") throw new InterestError(415, "invalid_request", "Use the interest form on the website.");
  const bytes = await readBoundedBody(request, MAX_BODY_BYTES);
  try { return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)) as unknown; }
  catch { invalid(); }
}
function loopback(url: URL) { return ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname); }
export function interestOriginAllowed(request: Request, env: MerchInterestEnv, development = process.env.NODE_ENV === "development") {
  const origin = request.headers.get("origin");
  if (!origin) return false;
  try {
    const requestUrl = new URL(request.url), incoming = new URL(origin);
    if (incoming.origin !== origin) return false;
    if (development && loopback(requestUrl) && origin === requestUrl.origin) return true;
    if (env.MERCH_INTEREST_SITE_ORIGIN) {
      const configured = new URL(env.MERCH_INTEREST_SITE_ORIGIN);
      if (configured.origin !== env.MERCH_INTEREST_SITE_ORIGIN || origin !== configured.origin) return false;
      // OpenNext's local production preview also needs its real bindings. An
      // explicit loopback origin remains confined to that same local request.
      if (loopback(configured)) return ["http:", "https:"].includes(configured.protocol) && requestUrl.origin === configured.origin;
      return configured.protocol === "https:";
    }
    return origin === "https://codewhale.net" || origin === "https://www.codewhale.net";
  } catch { return false; }
}
function requireOrigin(request: Request, env: MerchInterestEnv) {
  if (!interestOriginAllowed(request, env)) throw new InterestError(403, "request_blocked", "Use the interest form on the website.");
}
function available(env: MerchInterestEnv) { return Boolean(env.CURATED_KV && env.MERCH_INTEREST_LIMITER); }
function requireStorage(env: MerchInterestEnv): KVNamespace {
  if (!available(env)) throw new InterestError(503, "interest_unavailable", "The interest list is temporarily unavailable. Please try again later.");
  return env.CURATED_KV!;
}
async function digest(value: string) {
  const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(bytes)].map(value => value.toString(16).padStart(2, "0")).join("");
}
async function limited(env: MerchInterestEnv, key: string) {
  const result = await env.MERCH_INTEREST_LIMITER!.limit({ key });
  if (!result.success) throw new InterestError(429, "try_later", "Please wait a minute and try again.");
}
function validated(value: unknown, now: number): MerchInterest {
  const input = object(value);
  if (input.website !== undefined && input.website !== "") invalid();
  if (typeof input.email !== "string" || input.email.length > 254) invalid();
  const email = input.email.trim().toLowerCase();
  if (!/^[^\s@<>\x00-\x1f\x7f]+@[^\s@<>\x00-\x1f\x7f]+\.[^\s@<>\x00-\x1f\x7f]+$/.test(email)) invalid();
  if (typeof input.country !== "string" || !input.country.trim() || input.country.length > 80 || /[\x00-\x1f\x7f]/.test(input.country)) invalid();
  if (input.currency !== "cny" && input.currency !== "usd") invalid();
  if (typeof input.locale !== "string" || input.locale.length > 16 || !isValidLocale(input.locale) || input.consent !== true) invalid();
  if (!Array.isArray(input.picks) || !input.picks.length || input.picks.length > Math.min(12, MERCH_INTEREST_ITEMS.length)) invalid();
  const seen = new Set<string>(), currency = input.currency;
  const picks = input.picks.map(value => {
    const pick = object(value);
    if (typeof pick.id !== "string" || seen.has(pick.id) || Object.keys(pick).some(key => !["id", "priceChoice"].includes(key))) invalid();
    const item = MERCH_INTEREST_ITEMS.find(item => item.id === pick.id);
    const priceChoice = pick.priceChoice ?? "unsure";
    if (!item || !MERCH_INTEREST_PRICE_CHOICES.includes(priceChoice as PriceChoice)) invalid();
    seen.add(pick.id);
    const index = ["low", "mid", "high"].indexOf(priceChoice as string);
    const surveyAmount = index === -1 ? null : item.prices[currency][index];
    if (surveyAmount !== null && (!Number.isFinite(surveyAmount) || surveyAmount <= 0)) throw new InterestError(503, "interest_unavailable", "The interest list is temporarily unavailable.");
    return { id: pick.id, priceChoice: priceChoice as PriceChoice, surveyAmount };
  });
  return { schemaVersion: 1, email, country: input.country.trim(), currency, locale: input.locale, consent: true, emailVerified: false, submittedAt: new Date(now).toISOString(), expiresAt: new Date(now + RETENTION_SECONDS * 1000).toISOString(), picks };
}
function errorResponse(error: unknown) {
  if (error instanceof InterestError) return json({ error: error.message, code: error.code }, error.status);
  if (error instanceof BodyReadError) return json({ error: "The interest form is too large or invalid.", code: "invalid_request" }, error.status);
  // Never echo provider errors, keys, addresses, or request payloads.
  return json({ error: "The interest list is temporarily unavailable. Please try again later.", code: "interest_unavailable" }, 503);
}
export async function handleMerchInterest(request: Request, env?: MerchInterestEnv): Promise<Response> {
  try {
    const current = env ?? await getEnv();
    if (request.method === "GET") return json({ available: available(current) });
    if (request.method !== "POST") return json({ error: "Method not allowed." }, 405);
    requireOrigin(request, current);
    const kv = requireStorage(current);
    // Cloudflare supplies this trusted header at the edge. Missing headers share
    // one bucket; X-Forwarded-For and user-provided identifiers are not accepted.
    const ip = request.headers.get("cf-connecting-ip")?.slice(0, 64) ?? "unattributed";
    await limited(current, "merch-interest:ip:" + await digest(ip));
    const record = validated(await body(request), Date.now());
    const emailHash = await digest(record.email);
    await limited(current, "merch-interest:email:" + emailHash);
    await kv.put(PREFIX + emailHash, JSON.stringify(record), { expirationTtl: RETENTION_SECONDS });
    return json({ ok: true });
  } catch (error) { return errorResponse(error); }
}
async function requireAdmin(request: Request, auth: CommunityAgentEnv) {
  const sid = request.headers.get("cookie")?.split(";").map(cookie => cookie.trim()).find(cookie => cookie.startsWith("mt_sid="))?.slice(7);
  if (!maintainerTokenUsable(auth.MAINTAINER_TOKEN) || !await validateSession(auth.CURATED_KV, sid)) throw new InterestError(401, "unauthorized", "Maintainer sign-in is required.");
}
export async function handleAdminMerchInterest(request: Request, env?: MerchInterestEnv, authEnv?: CommunityAgentEnv): Promise<Response> {
  try {
    const current = env ?? await getEnv();
    await requireAdmin(request, authEnv ?? await getAgentEnv());
    if (!current.CURATED_KV) throw new InterestError(503, "interest_unavailable", "Interest storage is unavailable.");
    const kv = current.CURATED_KV;
    if (request.method === "GET") {
      const url = new URL(request.url), cursor = url.searchParams.get("cursor") ?? undefined;
      const rawLimit = url.searchParams.get("limit") ?? "50";
      if ((cursor && (cursor.length > 2048 || /[\x00-\x1f\x7f]/.test(cursor))) || !/^\d{1,2}$/.test(rawLimit)) invalid();
      const limit = Number(rawLimit);
      if (limit < 1 || limit > 50) invalid();
      const listed = await kv.list({ prefix: PREFIX, limit, ...(cursor ? { cursor } : {}) });
      const entries = await Promise.all(listed.keys.slice(0, limit).map(async ({ name }) => {
        if (!name.startsWith(PREFIX) || !/^[a-f0-9]{64}$/.test(name.slice(PREFIX.length))) return null;
        const raw = await kv.get(name);
        if (!raw || raw.length > MAX_BODY_BYTES) return null;
        try {
          const record = JSON.parse(raw) as MerchInterest;
          if (record.schemaVersion !== 1 || Date.parse(record.expiresAt) <= Date.now() || !Number.isFinite(Date.parse(record.expiresAt))) return null;
          return { id: name.slice(PREFIX.length), ...record };
        } catch { return null; }
      }));
      return json({ entries: entries.filter(entry => entry !== null), cursor: listed.list_complete === false ? listed.cursor ?? null : null });
    }
    if (request.method === "DELETE") {
      requireOrigin(request, current);
      const input = object(await body(request));
      if (typeof input.id !== "string" || !/^[a-f0-9]{64}$/.test(input.id)) invalid();
      await kv.delete(PREFIX + input.id);
      return json({ ok: true });
    }
    return json({ error: "Method not allowed." }, 405);
  } catch (error) { return errorResponse(error); }
}
