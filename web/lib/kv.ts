/**
 * Cloudflare KV access via the OpenNext binding helper.
 * Falls back to in-memory cache for `next dev` outside of `wrangler dev`.
 */
import type { DraftClaimLockNamespace } from "./draft-claim-lock";
import type { CuratedDispatch } from "./types";
import type { MerchEnv } from "./merch/types";

const MEM = new Map<string, string>();

export interface KVNamespace {
  get(key: string): Promise<string | null>;
  put(key: string, value: string, opts?: { expirationTtl?: number }): Promise<void>;
  list(opts?: { prefix?: string; limit?: number; cursor?: string }): Promise<{ keys: { name: string }[]; cursor?: string; list_complete?: boolean }>;
  delete(key: string): Promise<void>;
}

/** Native KV streaming reads keep signed facts bounded before allocation. */
export interface KVStreamNamespace {
  get(key: string, type: "stream"): Promise<ReadableStream<Uint8Array> | null>;
  put: KVNamespace["put"];
}

interface CloudflareEnv extends MerchEnv {
  DRAFT_CLAIM_LOCK?: DraftClaimLockNamespace;
  CURATED_KV?: KVNamespace & KVStreamNamespace;
  MERCH_INTEREST_LIMITER?: { limit(options: { key: string }): Promise<{ success: boolean }> };
  MERCH_INTEREST_SITE_ORIGIN?: string;
  DEEPSEEK_API_KEY?: string;
  DEEPSEEK_BASE_URL?: string;
  DEEPSEEK_MODEL?: string;
  GITHUB_TOKEN?: string;
  CRON_SECRET?: string;
  GITHUB_REPO?: string;
  /** Cloud facts (facts/v1): Supabase Data API URL + publishable (anon) key. Never a service key. */
  SUPABASE_URL?: string;
  SUPABASE_PUBLISHABLE_KEY?: string;
}

function envFromProcess(): CloudflareEnv {
  return {
    MERCH_CONFIG_JSON: process.env.MERCH_CONFIG_JSON,
    MERCH_INTEREST_SITE_ORIGIN: process.env.MERCH_INTEREST_SITE_ORIGIN,
    MERCH_STRIPE_KEY: process.env.MERCH_STRIPE_KEY,
    MERCH_STRIPE_WEBHOOK_SECRET: process.env.MERCH_STRIPE_WEBHOOK_SECRET,
    MERCH_PAYMENT_CONFIGURATION: process.env.MERCH_PAYMENT_CONFIGURATION,
    YOYCOL_ACCESS_KEY: process.env.YOYCOL_ACCESS_KEY,
    YOYCOL_SECRET_KEY: process.env.YOYCOL_SECRET_KEY,
    SUPABASE_URL: process.env.SUPABASE_URL,
    SUPABASE_PUBLISHABLE_KEY: process.env.SUPABASE_PUBLISHABLE_KEY,
    DEEPSEEK_API_KEY: process.env.DEEPSEEK_API_KEY,
    DEEPSEEK_BASE_URL: process.env.DEEPSEEK_BASE_URL,
    DEEPSEEK_MODEL: process.env.DEEPSEEK_MODEL,
    GITHUB_TOKEN: process.env.GITHUB_TOKEN,
    CRON_SECRET: process.env.CRON_SECRET,
    GITHUB_REPO: process.env.GITHUB_REPO,
  };
}

export async function getEnv(): Promise<CloudflareEnv> {
  if (process.env.NEXT_PHASE === "phase-production-build") {
    return envFromProcess();
  }

  try {
    const mod = await import("@opennextjs/cloudflare");
    const ctx = await mod.getCloudflareContext({ async: true });
    return ctx.env as CloudflareEnv;
  } catch {
    return envFromProcess();
  }
}

export async function getDispatch(): Promise<CuratedDispatch | null> {
  const env = await getEnv();
  const raw = env.CURATED_KV ? await env.CURATED_KV.get("dispatch:latest") : MEM.get("dispatch:latest") ?? null;
  if (!raw) return null;
  try {
    return JSON.parse(raw) as CuratedDispatch;
  } catch {
    return null;
  }
}

export async function putDispatch(d: CuratedDispatch): Promise<void> {
  const env = await getEnv();
  await putDispatchWithKv(env.CURATED_KV, d);
}

export async function putDispatchWithKv(kv: KVNamespace | undefined, d: CuratedDispatch): Promise<void> {
  const value = JSON.stringify(d);
  if (kv) {
    await kv.put("dispatch:latest", value, { expirationTtl: 60 * 60 * 24 * 7 });
  } else {
    MEM.set("dispatch:latest", value);
  }
}
