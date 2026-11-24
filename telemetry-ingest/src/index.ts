/**
 * Codewhale first-party telemetry ingest.
 *
 * ============================ THE RED LINE ============================
 *
 * THIS WORKER NEVER READS, LOGS, STORES, OR FORWARDS THE CLIENT IP.
 *
 * `docs/TELEMETRY.md` publishes "Batches are IP-stripped at ingest. No IP is
 * stored, logged, or joined to `install_id`." This file is the whole of what
 * makes that sentence true. There is no other component. If you add an IP read
 * here, the published document becomes a false statement about a shipped
 * product, and every user who opted in did so on the strength of it.
 *
 * Concretely, and permanently:
 *
 *   - Do not read the connecting-address header, the proxy-chain header, or any
 *     other header that carries a network address. The names are deliberately
 *     spelled nowhere in this directory: `test/no-ip.test.ts` greps the source
 *     for them and fails the build, so an edit that adds one cannot land quietly
 *     and cannot be justified as "just for debugging".
 *   - Do not read the `cf` property of the request. Country, colo, city, region,
 *     ASN, and coordinates all live there; none of them are in the schema, and
 *     the same test greps for that access too.
 *   - Read exactly two headers, ever: `content-type` and `content-length`. The
 *     same test extracts every header name this source asks for and fails if the
 *     set grows.
 *   - Do not log the request. Structured logs in Workers are queryable, and a
 *     log line is storage. Nothing in this file logs a payload or a header.
 *   - Analytics Engine rows are built in `datapoint.ts` from the *validated
 *     batch body only*. The request object is not in scope there.
 *
 * Debugging without an IP is a solved problem: the schema carries `os`, `arch`,
 * `libc`, `surface`, `app_version`, and `git_sha`, which is what a crash triage
 * actually needs. If you find yourself wanting the IP, you want a different
 * feature, and it needs the owner's sign-off and a doc change first.
 *
 * ======================================================================
 *
 * Shape of the service: write-only. One POST route, no GET that returns data,
 * no response body on any path, ever. The client
 * (`crates/telemetry/src/client.rs`) reads only the status class and drops the
 * batch on anything that is not 2xx — no retry, no backoff, no re-queue — so a
 * rejection here is invisible to the user by construction, and a 5xx can never
 * become a client-visible error. That is what lets this endpoint fail closed:
 * when in doubt, refuse the batch.
 */

import { writeBatch, type DataPointSink } from "./datapoint";
import { INGEST_PATH } from "./route";
import { MAX_BODY_BYTES, validateBatch } from "./schema";

/**
 * Rate-limit binding shape (`ratelimits` in `wrangler.jsonc`).
 *
 * Note that this module exports exactly one value — the default handler. The
 * Workers runtime maps every *named* export of the entrypoint to an entrypoint
 * of its own and refuses to start when one is not callable. Interfaces are
 * erased at build time, so these cost nothing; a constant would not.
 */
export interface RateLimiter {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

export interface Env {
  /** `analytics_engine_datasets` binding. */
  TELEMETRY: DataPointSink;
  /**
   * Optional per-location ingest rate limiter keyed on the server-controlled
   * route path (`INGEST_PATH`) before body reading, parsing, or downstream
   * delivery. Caps aggregate throughput even when a caller rotates `install_id`
   * on every request, without reading any client IP or network header.
   */
  INGEST_RATE_LIMITER?: RateLimiter;
  /**
   * Optional per-install rate limiter.
   *
   * Keyed on `install_id` — the identifier the batch already carries — and
   * never on a network address. Known limitation: `install_id` is caller-
   * supplied and can be rotated between POSTs; `INGEST_RATE_LIMITER` above and
   * Cloudflare edge rules provide the rotation-resistant cap so this Worker
   * never handles IPs.
   */
  RATE_LIMITER?: RateLimiter;
}

/** Every response is a bare status. No body, no echo of the payload, ever. */
function status(code: number, headers?: HeadersInit): Response {
  return new Response(null, { status: code, headers });
}

/**
 * Read at most `limit` bytes, aborting the stream the moment it goes over.
 *
 * `content-length` is checked first as a cheap reject, but it is client-supplied
 * and may be absent or wrong, so the real bound is enforced while reading.
 * Returns `null` when the body is missing or too large.
 */
async function readBounded(
  body: ReadableStream<Uint8Array> | null,
  limit: number,
): Promise<Uint8Array | null> {
  if (body === null) return null;
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (value === undefined) continue;
      total += value.byteLength;
      if (total > limit) {
        await reader.cancel();
        return null;
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    joined.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return joined;
}

async function ingest(request: Request, env: Env): Promise<Response> {
  // Method before path, so a probe of any path with any verb other than POST
  // gets the same answer and learns nothing about what exists here.
  if (request.method !== "POST") {
    return status(405, { allow: "POST" });
  }
  if (new URL(request.url).pathname !== INGEST_PATH) {
    return status(404);
  }

  // Header read #1 of 2. `client.rs` sends exactly `application/json`.
  const contentType = request.headers.get("content-type") ?? "";
  if (!contentType.toLowerCase().startsWith("application/json")) {
    return status(415);
  }

  // Header read #2 of 2, and the last. See the red line above.
  const declared = request.headers.get("content-length");
  if (declared !== null) {
    const length = Number(declared);
    if (!Number.isFinite(length) || length > MAX_BODY_BYTES) {
      return status(413);
    }
  }

  if (env.INGEST_RATE_LIMITER !== undefined) {
    const { success } = await env.INGEST_RATE_LIMITER.limit({
      key: INGEST_PATH,
    });
    if (!success) return status(429);
  }

  const raw = await readBounded(request.body, MAX_BODY_BYTES);
  if (raw === null) return status(413);

  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(raw);
  } catch {
    return status(400);
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return status(400);
  }

  // The closed-field-set check. An unexpected key anywhere rejects the whole
  // batch: a future client bug that starts attaching a path or a prompt must be
  // refused by the server rather than quietly stored. The reason string stays
  // here — the response carries no body, because a parse error echoed back is a
  // way to learn what this endpoint keeps.
  const result = validateBatch(parsed);
  if (!result.ok) return status(400);

  // Rate limiting is keyed on the install id the batch already carries. It runs
  // after validation because that is the only way to have a non-network key.
  if (env.RATE_LIMITER !== undefined) {
    const { success } = await env.RATE_LIMITER.limit({
      key: result.batch.install_id,
    });
    if (!success) return status(429);
  }

  // `writeDataPoint` is non-blocking and returns void; it is never awaited.
  writeBatch(env.TELEMETRY, result.batch);

  return status(204);
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      return await ingest(request, env);
    } catch {
      // Fail closed and quiet. Nothing is written, nothing is logged, and the
      // response has no body. The client treats any non-2xx as "dropped" and
      // surfaces nothing to the user, so a 5xx here costs one batch and never
      // becomes a user-visible error.
      return status(500);
    }
  },
};
