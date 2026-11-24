import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { DataPoint } from "../src/datapoint";
import type { Env } from "../src/index";
import { INGEST_PATH } from "../src/route";

/** `docs/TELEMETRY.md` — the published schema, and the thing we must match. */
export const DOC = readFileSync(
  new URL("../../docs/TELEMETRY.md", import.meta.url),
  "utf8",
);

/**
 * `crates/telemetry/tests/golden/v1.json` — the client's own pinned v1 wire
 * form. Reading the real file rather than a hand-written fixture is the point:
 * the Rust suite already welds this file to the doc and to the serializer, so
 * an endpoint that accepts it byte for byte is welded to both by transitivity.
 */
export const GOLDEN_PATH = fileURLToPath(
  new URL("../../crates/telemetry/tests/golden/v1.json", import.meta.url),
);

export const GOLDEN_TEXT = readFileSync(GOLDEN_PATH, "utf8");

/** A fresh deep copy of the golden batch. */
export function goldenBatch(): Record<string, unknown> {
  return JSON.parse(GOLDEN_TEXT) as Record<string, unknown>;
}

export interface Harness {
  env: Env;
  written: DataPoint[];
  limited: string[];
  ingestLimited: string[];
}

/** An `Env` whose bindings record instead of calling Cloudflare. */
export function harness(
  options: { rateLimit?: boolean; ingestRateLimit?: boolean | number } = {},
): Harness {
  const written: DataPoint[] = [];
  const limited: string[] = [];
  const ingestLimited: string[] = [];
  const env: Env = {
    TELEMETRY: {
      writeDataPoint(point) {
        written.push({
          indexes: point.indexes ?? [],
          blobs: point.blobs ?? [],
          doubles: point.doubles ?? [],
        });
      },
    },
  };
  if (options.rateLimit !== undefined) {
    env.RATE_LIMITER = {
      async limit({ key }) {
        limited.push(key);
        return { success: options.rateLimit === true };
      },
    };
  }
  if (options.ingestRateLimit !== undefined) {
    const ingestRateLimit = options.ingestRateLimit;
    const counts = new Map<string, number>();
    env.INGEST_RATE_LIMITER = {
      async limit({ key }) {
        ingestLimited.push(key);
        if (typeof ingestRateLimit === "boolean") {
          return { success: ingestRateLimit };
        }
        const next = (counts.get(key) ?? 0) + 1;
        counts.set(key, next);
        return { success: next <= ingestRateLimit };
      },
    };
  }
  return { env, written, limited, ingestLimited };
}

const ORIGIN = "https://telemetry.invalid";

/** A POST shaped the way `crates/telemetry/src/client.rs` shapes it. */
export function post(
  body: BodyInit,
  init: { path?: string; contentType?: string | null } = {},
): Request {
  const headers = new Headers();
  if (init.contentType !== null) {
    headers.set("content-type", init.contentType ?? "application/json");
  }
  return new Request(`${ORIGIN}${init.path ?? INGEST_PATH}`, {
    method: "POST",
    headers,
    body,
  });
}

/** A POST of a JSON value. */
export function postJson(value: unknown): Request {
  return post(JSON.stringify(value));
}
