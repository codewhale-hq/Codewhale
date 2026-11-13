import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import worker from "../src/index";
import { INGEST_PATH } from "../src/route";
import { BLOB_COLUMNS, DOUBLE_COLUMNS, MODEL_USAGE_DOUBLE_COLUMNS } from "../src/datapoint";
import { MAX_BODY_BYTES, SURFACES, validateBatch } from "../src/schema";
import { goldenBatch, harness, post, postJson } from "./support";

describe("method and route", () => {
  it("answers 405 to GET, with no body", async () => {
    const { env, written } = harness();
    const response = await worker.fetch(
      new Request(`https://telemetry.invalid${INGEST_PATH}`),
      env,
    );
    expect(response.status).toBe(405);
    expect(response.headers.get("allow")).toBe("POST");
    expect(await response.text()).toBe("");
    expect(written).toHaveLength(0);
  });

  it.each(["HEAD", "PUT", "PATCH", "DELETE", "OPTIONS"])(
    "answers 405 to %s",
    async (method) => {
      const { env } = harness();
      const response = await worker.fetch(
        new Request(`https://telemetry.invalid${INGEST_PATH}`, { method }),
        env,
      );
      expect(response.status).toBe(405);
    },
  );

  it("has no readable surface: GET on any other path is also 405", async () => {
    const { env } = harness();
    for (const path of ["/", "/v1", "/v1/telemetry/query", "/health"]) {
      const response = await worker.fetch(
        new Request(`https://telemetry.invalid${path}`),
        env,
      );
      expect(response.status).toBe(405);
      expect(await response.text()).toBe("");
    }
  });

  it("answers 404 to a POST at another path", async () => {
    const { env } = harness();
    const response = await worker.fetch(
      post(JSON.stringify(goldenBatch()), { path: "/v1/other" }),
      env,
    );
    expect(response.status).toBe(404);
  });

  it("answers 415 without a JSON content type", async () => {
    const { env } = harness();
    const response = await worker.fetch(
      post(JSON.stringify(goldenBatch()), { contentType: "text/plain" }),
      env,
    );
    expect(response.status).toBe(415);
  });
});

describe("a valid batch", () => {
  it("accepts the client's own golden v1 batch with 204 and no body", async () => {
    const { env, written } = harness();
    const response = await worker.fetch(postJson(goldenBatch()), env);

    expect(response.status).toBe(204);
    expect(await response.text()).toBe("");
    expect(response.headers.get("content-type")).toBeNull();
    expect(written).toHaveLength(4);
  });

  it("writes one data point per event, indexed by install_id only", async () => {
    const { env, written } = harness();
    await worker.fetch(postJson(goldenBatch()), env);

    for (const point of written) {
      expect(point.indexes).toEqual(["3f2a9c1e-0000-4000-8000-000000000001"]);
      expect(point.blobs).toHaveLength(BLOB_COLUMNS.length);
      expect(point.doubles).toHaveLength(DOUBLE_COLUMNS.length);
    }
    expect(written.map((point) => point.blobs[0])).toEqual([
      "install_or_upgrade",
      "session_start",
      "session_end",
      "panic",
    ]);
  });

  it("puts the counters, errors and turn_wall in the documented columns", async () => {
    const { env, written } = harness();
    await worker.fetch(postJson(goldenBatch()), env);

    const sessionEnd = written[2];
    const column = (name: string) =>
      sessionEnd.doubles[DOUBLE_COLUMNS.indexOf(name as never)];
    expect(column("turns")).toBe(14);
    expect(column("tool_calls")).toBe(61);
    expect(column("subagent_spawn")).toBe(2);
    expect(column("provider_http_5xx")).toBe(1);
    expect(column("lt_5s")).toBe(9);
    expect(column("gte_120s")).toBe(0);

    const blob = (name: string) =>
      sessionEnd.blobs[BLOB_COLUMNS.indexOf(name as never)];
    expect(blob("providers")).toBe("custom,deepseek");
    expect(blob("exit_class")).toBe("panic");
    expect(blob("cold_start_bucket")).toBe("250_1000");

    expect(written[3].blobs[BLOB_COLUMNS.indexOf("panic_site")]).toBe(
      "crates/tui/src/tui/ui.rs:8801:17",
    );
  });

  it("stores nothing that is not in the batch", async () => {
    const { env, written } = harness();
    await worker.fetch(postJson(goldenBatch()), env);

    const stored = written.flatMap((point) => [
      ...point.indexes,
      ...point.blobs,
    ]);
    const batchText = JSON.stringify(goldenBatch());
    for (const value of stored) {
      if (value === "" || value === "true" || value === "false") continue;
      // Every stored string is a substring of the batch the client sent, or a
      // comma-join of values from it. Nothing is derived from the request.
      for (const part of value.split(",")) {
        expect(batchText).toContain(part);
      }
    }
  });
});

describe("the closed field set", () => {
  it("rejects an unknown key on the envelope", async () => {
    const { env, written } = harness();
    const batch = goldenBatch();
    batch.cwd = "/Users/someone/work/secret-repo";
    const response = await worker.fetch(postJson(batch), env);

    expect(response.status).toBe(400);
    expect(await response.text()).toBe("");
    expect(written).toHaveLength(0);
  });

  it("rejects an unknown key nested inside an event", async () => {
    const { env, written } = harness();
    const batch = goldenBatch();
    const events = batch.events as Record<string, unknown>[];
    events[2].prompt = "please refactor crates/tui/src/main.rs";
    const response = await worker.fetch(postJson(batch), env);

    expect(response.status).toBe(400);
    expect(written).toHaveLength(0);
  });

  it("rejects an unknown key nested inside counters", async () => {
    const { env } = harness();
    const batch = goldenBatch();
    const events = batch.events as Record<string, Record<string, unknown>>[];
    events[2].counters.git_branch_switches = 1;
    expect((await worker.fetch(postJson(batch), env)).status).toBe(400);
  });

  it("rejects an unknown event discriminant", async () => {
    const { env } = harness();
    const batch = goldenBatch();
    (batch.events as unknown[]).push({ event: "keystroke", site: "<dep>" });
    expect((await worker.fetch(postJson(batch), env)).status).toBe(400);
  });

  it("rejects a batch with any envelope key removed", async () => {
    const original = goldenBatch();
    for (const key of Object.keys(original)) {
      const { env } = harness();
      const batch = goldenBatch();
      delete batch[key];
      const response = await worker.fetch(postJson(batch), env);
      expect(response.status, `deleting ${key} must reject`).toBe(400);
    }
  });

  it("rejects a batch with any event key removed", async () => {
    const events = goldenBatch().events as Record<string, unknown>[];
    for (let index = 0; index < events.length; index += 1) {
      for (const key of Object.keys(events[index])) {
        const { env } = harness();
        const batch = goldenBatch();
        delete (batch.events as Record<string, unknown>[])[index][key];
        const response = await worker.fetch(postJson(batch), env);
        expect(response.status, `deleting events[${index}].${key}`).toBe(400);
      }
    }
  });
});

describe("value rules from the published schema", () => {
  const reject = async (mutate: (batch: Record<string, unknown>) => void) => {
    const { env, written } = harness();
    const batch = goldenBatch();
    mutate(batch);
    const response = await worker.fetch(postJson(batch), env);
    expect(response.status).toBe(400);
    expect(written).toHaveLength(0);
  };

  it("rejects a wrong schema_version", () =>
    reject((batch) => {
      batch.schema_version = 2;
    }));

  it("rejects a non-UTC or sub-second sent_at", () =>
    reject((batch) => {
      batch.sent_at = "2026-08-03T18:04:11.234Z";
    }));

  it("rejects an install_id that is not a v4 uuid", () =>
    reject((batch) => {
      batch.install_id = "hostname-derived-id";
    }));

  it("rejects an app_version that is not a release version", () =>
    reject((batch) => {
      batch.app_version = "0.9.4 (/Users/someone/src/codewhale)";
    }));

  it("rejects a git_sha that is not 12 hex chars", () =>
    reject((batch) => {
      batch.git_sha = "refs/heads/feature-acme-migration";
    }));

  it("accepts a null git_sha — every locally built binary sends one", async () => {
    const { env } = harness();
    const batch = goldenBatch();
    batch.git_sha = null;
    expect((await worker.fetch(postJson(batch), env)).status).toBe(204);
  });

  it("accepts a null cold_start_bucket — non-TUI surfaces send one", async () => {
    const { env } = harness();
    const batch = goldenBatch();
    (batch.events as Record<string, unknown>[])[2].cold_start_bucket = null;
    expect((await worker.fetch(postJson(batch), env)).status).toBe(204);
  });

  it("rejects a surface outside the closed enum", () =>
    reject((batch) => {
      batch.surface = "desktop";
    }));

  it("rejects a provider list that is not sorted and deduplicated", () =>
    reject((batch) => {
      (batch.events as Record<string, unknown>[])[2].providers = [
        "deepseek",
        "custom",
      ];
    }));

  it("rejects a provider entry shaped like a config table key", () =>
    reject((batch) => {
      (batch.events as Record<string, unknown>[])[2].providers = [
        "acme_internal_gateway",
      ];
    }));

  it("rejects a panic site outside the crates/ allowlist", () =>
    reject((batch) => {
      (batch.events as Record<string, unknown>[])[3].site =
        "/Users/builder/.cargo/registry/src/index.crates.io/ratatui-0.29.0/src/lib.rs:1:1";
    }));

  it("accepts the reduced <dep> panic site", async () => {
    const { env } = harness();
    const batch = goldenBatch();
    (batch.events as Record<string, unknown>[])[3].site = "<dep>";
    expect((await worker.fetch(postJson(batch), env)).status).toBe(204);
  });

  it("rejects a counter that is not a non-negative integer", () =>
    reject((batch) => {
      const events = batch.events as Record<string, Record<string, unknown>>[];
      events[2].counters.turns = -1;
    }));

  it("rejects more events than the client can put in one batch", () =>
    reject((batch) => {
      batch.events = Array.from({ length: 201 }, () => ({
        event: "session_start",
        source: "interactive",
      }));
    }));

  it("rejects a body that is not JSON", async () => {
    const { env } = harness();
    const response = await worker.fetch(post("not json at all"), env);
    expect(response.status).toBe(400);
  });

  it("rejects a JSON array at the top level", async () => {
    const { env } = harness();
    expect((await worker.fetch(postJson([goldenBatch()]), env)).status).toBe(
      400,
    );
  });
});

describe("size caps", () => {
  it("rejects an oversized body declared by content-length", async () => {
    const { env, written } = harness();
    const batch = goldenBatch();
    batch.events = [
      {
        event: "panic",
        site: `crates/tui/src/${"a".repeat(MAX_BODY_BYTES)}.rs:1:1`,
      },
    ];
    const body = JSON.stringify(batch);
    expect(body.length).toBeGreaterThan(MAX_BODY_BYTES);

    const response = await worker.fetch(post(body), env);
    expect(response.status).toBe(413);
    expect(await response.text()).toBe("");
    expect(written).toHaveLength(0);
  });

  it("rejects an oversized body that declares no content-length", async () => {
    const { env, written } = harness();
    const chunk = new TextEncoder().encode("x".repeat(8 * 1024));
    let remaining = MAX_BODY_BYTES + 8 * 1024;
    const stream = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (remaining <= 0) {
          controller.close();
          return;
        }
        remaining -= chunk.byteLength;
        controller.enqueue(chunk);
      },
    });

    const request = new Request(`https://telemetry.invalid${INGEST_PATH}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: stream,
      // Node requires this for a streaming request body; Workers does not.
      duplex: "half",
    } as RequestInit);

    expect(request.headers.get("content-length")).toBeNull();
    const response = await worker.fetch(request, env);
    expect(response.status).toBe(413);
    expect(written).toHaveLength(0);
  });

  it("accepts a batch at the documented ceiling of 200 events", async () => {
    const { env, written } = harness();
    const batch = goldenBatch();
    batch.events = Array.from({ length: 200 }, () => ({
      event: "session_start",
      source: "interactive",
    }));
    const response = await worker.fetch(postJson(batch), env);
    expect(response.status).toBe(204);
    expect(written).toHaveLength(200);
  });
});

describe("rate limiting", () => {
  it("keys the limiter on install_id and nothing else", async () => {
    const { env, limited } = harness({ rateLimit: true });
    const response = await worker.fetch(postJson(goldenBatch()), env);
    expect(response.status).toBe(204);
    expect(limited).toEqual(["3f2a9c1e-0000-4000-8000-000000000001"]);
  });

  it("answers 429 with no body when the limiter refuses", async () => {
    const { env, written } = harness({ rateLimit: false });
    const response = await worker.fetch(postJson(goldenBatch()), env);
    expect(response.status).toBe(429);
    expect(await response.text()).toBe("");
    expect(written).toHaveLength(0);
  });
});

/**
 * The surface whitelist is the field that tells one client of this schema from
 * another, so a value added to it has to be proved *reachable* — not merely
 * present in a constant that `schema-doc.test.ts` compares against the doc.
 */
describe("the v3 surface whitelist", () => {
  /**
   * `crates/telemetry/tests/golden/v2.json` is a real client batch, and v3
   * changed no field: its v3 form is that envelope with the explicit-consent
   * field swapped for the policy field. Kept local rather than added to
   * `support.ts`, because v1 is the only golden the endpoint is contractually
   * welded to.
   */
  const v3Batch = (surface: string): Record<string, unknown> => {
    const batch = JSON.parse(
      readFileSync(
        new URL("../../crates/telemetry/tests/golden/v2.json", import.meta.url),
        "utf8",
      ),
    ) as Record<string, unknown>;
    delete batch.consent_version;
    batch.schema_version = 3;
    batch.notice_version = 5;
    batch.surface = surface;
    // `operations_summary` is accepted only on `control-plane`; the other
    // values in the whitelist must not be made to fail on an unrelated rule.
    batch.events = (batch.events as Array<Record<string, unknown>>).filter(
      (event) => event.event !== "operations_summary" || surface === "control-plane",
    );
    return batch;
  };

  it("accepts every value in the whitelist and stores it in blob2", async () => {
    for (const surface of SURFACES) {
      const { env, written } = harness();
      const response = await worker.fetch(postJson(v3Batch(surface)), env);
      expect(response.status, surface).toBe(204);
      expect(written[0]?.blobs[BLOB_COLUMNS.indexOf("surface")]).toBe(surface);
    }
  });

  it("rejects a value that is not in the whitelist", async () => {
    const { env, written } = harness();
    const response = await worker.fetch(postJson(v3Batch("vscode")), env);
    expect(response.status).toBe(400);
    expect(written).toHaveLength(0);
  });

  it("the v1 contract still refuses the extension surface", async () => {
    const { env, written } = harness();
    const batch = goldenBatch();
    batch.surface = "vscode-extension";
    expect((await worker.fetch(postJson(batch), env)).status).toBe(400);
    expect(written).toHaveLength(0);
  });
});

describe("failing closed and quiet", () => {
  it("answers 500 with no body when a binding throws", async () => {
    const env = {
      TELEMETRY: {
        writeDataPoint() {
          throw new Error("dataset unavailable");
        },
      },
    };
    const response = await worker.fetch(postJson(goldenBatch()), env);
    expect(response.status).toBe(500);
    expect(await response.text()).toBe("");
  });

  it("never returns a body on any path", async () => {
    const { env } = harness();
    const requests = [
      new Request(`https://telemetry.invalid${INGEST_PATH}`),
      post("{", {}),
      post(JSON.stringify(goldenBatch()), { path: "/nope" }),
      postJson(goldenBatch()),
    ];
    for (const request of requests) {
      const response = await worker.fetch(request, env);
      expect(await response.text()).toBe("");
    }
  });
});

describe("schema 6", () => {
  const golden = (version: number): Record<string, any> =>
    JSON.parse(
      readFileSync(
        new URL(`../../crates/telemetry/tests/golden/v${version}.json`, import.meta.url),
        "utf8",
      ),
    );
  const event = (batch: Record<string, any>, name: string) =>
    batch.events.find((item: Record<string, any>) => item.event === name);

  it("accepts every historical golden and the v6 golden", () => {
    for (const version of [1, 2, 3, 4, 6]) {
      expect(validateBatch(golden(version)).ok, `v${version}`).toBe(true);
    }
  });

  it("answers 204 to the v4 and v6 goldens", async () => {
    for (const version of [4, 6]) {
      const { env, written } = harness();
      expect((await worker.fetch(postJson(golden(version)), env)).status).toBe(204);
      expect(written).toHaveLength(golden(version).events.length);
    }
  });

  it("requires notice 8 on v6 and refuses the v4 notice", () => {
    const batch = golden(6);
    batch.notice_version = 6;
    expect(validateBatch(batch).ok).toBe(false);
    expect(validateBatch({ ...golden(4), notice_version: 8 }).ok).toBe(false);
  });

  it("refuses a v4 batch carrying any v6 key or value", () => {
    const mutations: Array<(batch: Record<string, any>) => void> = [
      (batch) => { event(batch, "model_usage").context = event(golden(6), "model_usage").context; },
      (batch) => { event(batch, "model_usage").counters.requests = 1; },
      (batch) => { event(batch, "model_usage").counters.reasoning_tokens = 1; },
      (batch) => { event(batch, "model_usage").counters.generation_secs = 1; },
      (batch) => { event(batch, "session_start").install_age = "older"; },
      (batch) => { event(batch, "session_start").source = "fleet_worker"; },
      (batch) => { event(batch, "session_start").source = "agent"; },
      (batch) => { event(batch, "session_start").source = "scripted"; },
      (batch) => { event(batch, "session_end").exit_class = "unclean"; },
    ];
    for (const [index, mutate] of mutations.entries()) {
      const batch = golden(4);
      mutate(batch);
      expect(validateBatch(batch).ok, `mutation ${index}`).toBe(false);
    }
  });

  it("refuses a v6 batch missing or adding a key", () => {
    const mutations: Array<(batch: Record<string, any>) => void> = [
      (batch) => { delete event(batch, "model_usage").context; },
      (batch) => { delete event(batch, "model_usage").context["32k_128k"]; },
      (batch) => { event(batch, "model_usage").context.gte_512k = 0; },
      (batch) => { delete event(batch, "model_usage").counters.requests; },
      (batch) => { delete event(batch, "model_usage").counters.reasoning_tokens; },
      (batch) => { delete event(batch, "model_usage").counters.generation_secs; },
      (batch) => { event(batch, "model_usage").counters.generation_ms = 1; },
      (batch) => { event(batch, "model_usage").model_family = "x"; },
      (batch) => { delete event(batch, "session_start").install_age; },
      (batch) => { event(batch, "session_start").extra = "x"; },
      (batch) => { event(batch, "session_end").counters.requests = 1; },
    ];
    for (const [index, mutate] of mutations.entries()) {
      const batch = golden(6);
      mutate(batch);
      expect(validateBatch(batch).ok, `mutation ${index}`).toBe(false);
    }
  });

  it("refuses open values where v6 has a closed set or a count", () => {
    const mutations: Array<(batch: Record<string, any>) => void> = [
      (batch) => { event(batch, "session_start").install_age = "2026-11-01"; },
      (batch) => { event(batch, "session_start").install_age = "OLDER"; },
      (batch) => { event(batch, "session_start").source = "github-actions"; },
      (batch) => { event(batch, "session_end").exit_class = "killed"; },
      (batch) => { event(batch, "model_usage").counters.requests = 4294967296; },
      (batch) => { event(batch, "model_usage").counters.requests = 1.5; },
      (batch) => { event(batch, "model_usage").counters.generation_secs = -1; },
      (batch) => { event(batch, "model_usage").counters.reasoning_tokens = "7"; },
      (batch) => { event(batch, "model_usage").context.lt_32k = 4294967296; },
      (batch) => { event(batch, "model_usage").context.lt_32k = "1"; },
      (batch) => { event(batch, "model_usage").model = "/Users/someone/model"; },
    ];
    for (const [index, mutate] of mutations.entries()) {
      const batch = golden(6);
      mutate(batch);
      expect(validateBatch(batch).ok, `mutation ${index}`).toBe(false);
    }
  });

  it("accepts a null install_age and every v6 source and exit class", () => {
    for (const source of ["interactive", "resume", "fork", "api", "unknown", "ci", "fleet_worker", "agent", "scripted"]) {
      for (const age of ["new_today", "under_week", "older", null]) {
        const batch = golden(6);
        event(batch, "session_start").source = source;
        event(batch, "session_start").install_age = age;
        expect(validateBatch(batch).ok, `${source}/${age}`).toBe(true);
      }
    }
    for (const exit of ["clean", "signal", "panic", "error", "unclean"]) {
      const batch = golden(6);
      event(batch, "session_end").exit_class = exit;
      expect(validateBatch(batch).ok, exit).toBe(true);
    }
  });

  it("writes v6 model_usage after the unchanged eleven doubles, and leaves v4 rows zero there", async () => {
    expect(MODEL_USAGE_DOUBLE_COLUMNS).toHaveLength(18);
    const v6 = harness();
    await worker.fetch(postJson(golden(6)), v6.env);
    const row = v6.written.find((point) => point.blobs[0] === "model_usage")!;
    expect(row.doubles).toEqual([
      12, 1, 0, 418230, 9120, 380001, 0,
      3, 8, 2, 0,
      27, 2310, 412,
      9, 11, 5, 2,
      0, 0,
    ]);
    expect(row.blobs[BLOB_COLUMNS.indexOf("schema_version")]).toBe("6");

    const v4 = harness();
    await worker.fetch(postJson(golden(4)), v4.env);
    const old = v4.written.find((point) => point.blobs[0] === "model_usage")!;
    expect(old.doubles).toEqual([
      12, 1, 0, 418230, 9120, 380001, 0,
      3, 8, 2, 0,
      0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
  });

  it("writes install_age to blob12 of session_start rows only", async () => {
    const { env, written } = harness();
    await worker.fetch(postJson(golden(6)), env);
    const start = written.find((point) => point.blobs[0] === "session_start")!;
    expect(start.blobs[BLOB_COLUMNS.indexOf("session_source")]).toBe("fleet_worker");
    expect(start.blobs[BLOB_COLUMNS.indexOf("duration_bucket")]).toBe("under_week");
    const end = written.find((point) => point.blobs[0] === "session_end")!;
    expect(end.blobs[BLOB_COLUMNS.indexOf("exit_class")]).toBe("unclean");
    expect(end.blobs[BLOB_COLUMNS.indexOf("duration_bucket")]).toBe("1m_10m");
  });
});
