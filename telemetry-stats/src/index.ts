import { accessConfigured, verifyAccess } from "./access";
import { CAVEATS, collectStats, startOfUtcDay, utcDay } from "./queries";
import { rollupDaily } from "./rollup";
import { createRunner } from "./sql";
import type { Env } from "./types";

const DAY_MS = 86_400_000;
const CACHE_SECONDS = 300;
const TREND_METRICS = ["active_installs", "sessions", "turns", "tool_calls"];

const cache = new Map<string, { at: number; body: string }>();

function reply(body: string, status = 200, cacheable = false): Response {
  return new Response(body, {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": cacheable ? `private, max-age=${CACHE_SECONDS}` : "no-store",
      "x-content-type-options": "nosniff",
    },
  });
}

function daysParam(url: URL, fallback: number, max: number): number {
  const raw = url.searchParams.get("days");
  if (raw === null) return fallback;
  const days = Number(raw);
  if (!Number.isInteger(days) || days < 1 || days > max) return fallback;
  return days;
}

export async function handleFetch(request: Request, env: Env, now: Date = new Date()): Promise<Response> {
  if (request.method !== "GET" && request.method !== "HEAD") return reply("{}", 405);
  if (!accessConfigured(env)) return reply(JSON.stringify({ error: "access_not_configured" }), 503);
  if (!(await verifyAccess(request, env, fetch, now.getTime()))) return reply(JSON.stringify({ error: "forbidden" }), 403);

  const url = new URL(request.url);
  if (url.pathname === "/api/stats") {
    const days = daysParam(url, 14, 31);
    const cached = cache.get(`stats:${days}`);
    if (cached && now.getTime() - cached.at < CACHE_SECONDS * 1000) return reply(cached.body, 200, true);
    const runner = createRunner(env);
    if (!runner) return reply(JSON.stringify({ error: "no_analytics_source" }), 503);
    const to = new Date(startOfUtcDay(now).getTime() + DAY_MS);
    const from = new Date(to.getTime() - days * DAY_MS);
    try {
      const rows = await collectStats(runner, from, to);
      const body = JSON.stringify({
        source: runner.source,
        generated_at: now.toISOString(),
        from: utcDay(from),
        to: utcDay(new Date(to.getTime() - DAY_MS)),
        rows,
        caveats: CAVEATS,
      });
      cache.set(`stats:${days}`, { at: now.getTime(), body });
      return reply(body, 200, true);
    } catch {
      return reply(JSON.stringify({ error: "query_failed" }), 502);
    }
  }
  if (url.pathname === "/api/trend") {
    if (!env.DB) return reply(JSON.stringify({ error: "no_rollup_store" }), 503);
    const days = daysParam(url, 365, 1100);
    const from = utcDay(new Date(startOfUtcDay(now).getTime() - days * DAY_MS));
    const placeholders = TREND_METRICS.map(() => "?").join(",");
    const result = await env.DB.prepare(
      `SELECT day, metric, dim, key, value FROM daily_totals WHERE dim = 'all' AND day >= ? AND metric IN (${placeholders}) ORDER BY day`,
    )
      .bind(from, ...TREND_METRICS)
      .all();
    return reply(JSON.stringify({ from, rows: result.results ?? [] }), 200, true);
  }
  if (!env.ASSETS) return reply(JSON.stringify({ error: "not_found" }), 404);
  return env.ASSETS.fetch(request);
}

export default {
  fetch: (request: Request, env: Env) => handleFetch(request, env),
  async scheduled(_controller: unknown, env: Env): Promise<void> {
    const runner = createRunner(env);
    if (!runner || !env.DB) return;
    await rollupDaily(runner, env.DB);
  },
};
