/**
 * Batch -> Workers Analytics Engine data points.
 *
 * One data point per event. A batch is capped at 200 events
 * (`BATCH_MAX_EVENTS`), and Analytics Engine allows 250 data points per Worker
 * invocation, so a conforming batch never needs a second pass and never has to
 * drop an event to fit.
 *
 * The column layout is fixed and positional because Analytics Engine columns
 * are `blob1..blob20` / `double1..double20` — the names live only in the SQL
 * you write. Renumbering a column silently rewrites every historical query, so
 * the order below is append-only: to add a field, take the next free slot.
 *
 * The layout is chosen so the two questions the owner actually has are one
 * query each. Both queries are written out in `README.md`:
 *
 *   (a) installs and sessions   -> index1 (install_id) + blob1 (event)
 *   (b) error classes and panic sites -> double11..16 + blob16, one GROUP BY
 *
 * Every value below comes out of the validated batch body and nothing else.
 * This module cannot see anything about the connection — that is enforced by
 * `test/no-ip.test.ts`, which fails if the type it takes is ever widened. See
 * the red-line comment at the top of `index.ts`.
 */

import type { Batch, Event } from "./schema";
import { COUNTER_FIELDS, ERROR_FIELDS, MODEL_COUNTER_FIELDS, TURN_WALL_FIELDS } from "./schema";

/** The subset of `AnalyticsEngineDataset` this Worker uses. */
export interface DataPointSink {
  writeDataPoint(point: {
    indexes?: string[];
    blobs?: string[];
    doubles?: number[];
  }): void;
}

/** One row, in Analytics Engine's positional form. */
export interface DataPoint {
  indexes: string[];
  blobs: string[];
  doubles: number[];
}

/**
 * Blob column names, in `blob1..blobN` order. Exported so the README's SQL and
 * the tests read from one list rather than two.
 */
export const BLOB_COLUMNS = [
  "event", // blob1
  "surface", // blob2
  "os", // blob3
  "arch", // blob4
  "libc", // blob5
  "app_version", // blob6
  "git_sha", // blob7  '' when null (a local build)
  "tty", // blob8  'true' | 'false'
  "install_kind", // blob9
  "previous_version", // blob10
  "session_source", // blob11
  "duration_bucket", // blob12
  "exit_class", // blob13
  "cold_start_bucket", // blob14
  "providers", // blob15 comma-joined, already sorted and deduplicated
  "panic_site", // blob16
  "sent_at", // blob17 the batch timestamp; events carry none
  "aggregate_counters", // blob18 closed product/operations JSON count object
  "schema_version", // blob19
  "privacy_version", // blob20: v3 notice, v2 consent, empty on v1; blob19 disambiguates
] as const;

/**
 * Double column names, in `double1..double20` order: the ten counters, the six
 * error classes, then the four turn-wall buckets. Exactly 20 — Analytics
 * Engine's ceiling — which is why `tty` is a blob.
 */
export const DOUBLE_COLUMNS = [
  ...COUNTER_FIELDS,
  ...ERROR_FIELDS,
  ...TURN_WALL_FIELDS,
] as const;

const EMPTY_DOUBLES: number[] = DOUBLE_COLUMNS.map(() => 0);

/** Build the rows for one validated batch. */
export function toDataPoints(batch: Batch): DataPoint[] {
  return batch.events.map((event) => toDataPoint(batch, event));
}

function toDataPoint(batch: Batch, event: Event): DataPoint {
  const blobs = new Array<string>(BLOB_COLUMNS.length).fill("");
  blobs[0] = event.event;
  blobs[1] = batch.surface;
  blobs[2] = batch.os;
  blobs[3] = batch.arch;
  blobs[4] = batch.libc;
  blobs[5] = batch.app_version;
  blobs[6] = batch.git_sha ?? "";
  blobs[7] = batch.tty ? "true" : "false";
  blobs[16] = batch.sent_at;
  blobs[18] = String(batch.schema_version);
  const privacyVersion = batch.schema_version === 3 ? batch.notice_version : batch.consent_version;
  blobs[19] = privacyVersion === undefined ? "" : String(privacyVersion);

  let doubles = EMPTY_DOUBLES;

  switch (event.event) {
    case "install_or_upgrade":
      blobs[8] = event.kind;
      blobs[9] = event.previous_version ?? "";
      break;
    case "session_start":
      blobs[10] = event.source;
      break;
    case "session_end":
      blobs[11] = event.duration_bucket;
      blobs[12] = event.exit_class;
      blobs[13] = event.cold_start_bucket ?? "";
      blobs[14] = event.providers.join(",");
      doubles = [
        ...COUNTER_FIELDS.map((field) => event.counters[field]),
        ...ERROR_FIELDS.map((field) => event.errors[field]),
        ...TURN_WALL_FIELDS.map((field) => event.turn_wall[field]),
      ];
      break;
    case "operations_summary": {
      const { event: _event, ...counts } = event;
      blobs[17] = JSON.stringify(counts);
      break;
    }
    case "product_usage":
      blobs[17] = JSON.stringify(event.counters);
      break;
    case "panic":
      blobs[15] = event.site;
      break;
    case "model_usage":
      blobs[14] = event.provider;
      blobs[15] = event.model;
      doubles = [
        ...MODEL_COUNTER_FIELDS.map((field) => event.counters[field]),
        ...TURN_WALL_FIELDS.map((field) => event.turn_wall[field]),
        ...EMPTY_DOUBLES.slice(MODEL_COUNTER_FIELDS.length + TURN_WALL_FIELDS.length),
      ];
      break;
    case "tool_usage":
      blobs[15] = event.tool;
      doubles = [event.calls, event.errors, ...EMPTY_DOUBLES.slice(2)];
      break;
  }

  return {
    // The one index. `install_id` is a random v4 UUID that the client rotates
    // every 90 days, and it is the only identifier in the schema — which is
    // also why `docs/TELEMETRY.md` says no count derived from it is a user
    // count. It is the index because both questions group by it or count it.
    indexes: [batch.install_id],
    blobs,
    doubles,
  };
}

/** Write one batch. `writeDataPoint` is non-blocking and is never awaited. */
export function writeBatch(sink: DataPointSink, batch: Batch): number {
  const points = toDataPoints(batch);
  for (const point of points) {
    sink.writeDataPoint(point);
  }
  return points.length;
}
