import { BodyReadError, readBoundedBody } from "@/lib/bounded-body";
import { NextResponse } from "next/server";
import { MAX_ENVELOPE_BYTES, privacySignalHeader, validateEnvelope } from "@/lib/telemetry/product-usage";

/**
 * POST /api/product-telemetry — the website's same-origin forwarder.
 *
 * Inert without configuration: unless `CODEWHALE_TELEMETRY_INGEST_URL` is set
 * to the exact canonical first-party ingest, the route accepts nothing and
 * forwards nothing. With it set, a batch is forwarded only when it passes the
 * closed-set validator for the `website` surface and fits in 4 KiB. The
 * forward sets only a content-type header and the validated body; it does not
 * copy client headers. Hosting infrastructure may add transport headers, so
 * this is not a claim of network anonymity. Forwarding has a 1.5-second timeout
 * with no retry. The response is
 * `{ accepted, reason? }`, never the ingest's own body.
 *
 * The PostHog token, if the ingest has one, lives there. This route never
 * holds it.
 */

// No `runtime = "edge"`: @opennextjs/cloudflare does not support the edge
// runtime. Its `migrate` command says to remove the declaration, and its
// server bundle replaces Next's edge runtime with an empty shim. How the
// deployed route behaved while it was declared has not been checked. The
// default runtime already has fetch, Request, and TextDecoder.

export const CANONICAL_INGEST_URL = "https://telemetry.codewhale.net/v1/telemetry";
const FORWARD_TIMEOUT_MS = 1500;

export function ingestUrl(env: Record<string, string | undefined> = process.env): string | null {
  const configured = env.CODEWHALE_TELEMETRY_INGEST_URL?.trim();
  return configured === CANONICAL_INGEST_URL ? configured : null;
}

type Forward = (url: string, body: string, signal: AbortSignal) => Promise<{ ok: boolean }>;

const defaultForward: Forward = (url, body, signal) =>
  fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
    signal,
    redirect: "error",
  });

export async function handleProductTelemetry(
  request: Request,
  deps: { ingestUrl?: string | null; forward?: Forward } = {},
): Promise<Response> {
  const target = deps.ingestUrl === undefined ? ingestUrl() : deps.ingestUrl;
  const reply = (status: number, accepted: boolean, reason?: string) =>
    NextResponse.json(reason ? { accepted, reason } : { accepted }, {
      status,
      headers: { "cache-control": "no-store" },
    });

  if (privacySignalHeader(request.headers) || !target) return reply(200, false, "disabled");

  let text: string;
  try {
    const bytes = await readBoundedBody(request, MAX_ENVELOPE_BYTES);
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch (cause) {
    if (cause instanceof BodyReadError) {
      return reply(cause.status, false, cause.status === 413 ? "too_large" : "invalid_body");
    }
    return reply(422, false, "invalid_json");
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return reply(422, false, "invalid_json");
  }
  const validated = validateEnvelope(parsed, { surfaces: ["website"] });
  if (!validated.ok) return reply(422, false, "schema");

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), FORWARD_TIMEOUT_MS);
  try {
    // Re-serialise the validated envelope so only known fields travel.
    const response = await (deps.forward ?? defaultForward)(
      target,
      JSON.stringify(validated.envelope),
      controller.signal,
    );
    return reply(200, response.ok, response.ok ? undefined : "unavailable");
  } catch {
    return reply(200, false, "unavailable");
  } finally {
    clearTimeout(timer);
  }
}

export async function POST(request: Request): Promise<Response> {
  return handleProductTelemetry(request);
}
