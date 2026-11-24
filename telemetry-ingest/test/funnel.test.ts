import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import worker from "../src/index";
import {
  ENVELOPE_FIELDS, FUNNEL_COUNTER_FIELDS, FUNNEL_NOTICE_VERSION, FUNNEL_SCHEMA_VERSION,
  PRODUCT_COUNTER_FIELDS, WEBSITE_FUNNEL_SCHEMA, validateBatch,
} from "../src/schema";
import { BLOB_COLUMNS, DOUBLE_COLUMNS } from "../src/datapoint";
import { harness, postJson } from "./support";

const fixture = () => JSON.parse(readFileSync(new URL("./golden/browser-v5.json", import.meta.url), "utf8"));
const counters = (batch: ReturnType<typeof fixture>) => batch.events.find((event: { event: string }) => event.event === "funnel_usage").counters;

afterEach(() => vi.unstubAllGlobals());

describe("additive website funnel contract", () => {
  it("accepts the local v5 fixture through the existing pipeline with unchanged column positions", async () => {
    const batch = fixture();
    expect(Object.keys(batch)).toEqual([...ENVELOPE_FIELDS]);
    expect(validateBatch(batch).ok).toBe(true);
    expect(Object.keys(counters(batch))).toEqual([...FUNNEL_COUNTER_FIELDS]);
    expect(Object.keys(batch.events[0].counters)).toEqual([...PRODUCT_COUNTER_FIELDS]);
    const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
    const { env, written } = harness();
    const response = await worker.fetch(postJson(batch), {
      ...env, POSTHOG_HOST: "https://us.i.posthog.com", POSTHOG_PROJECT_TOKEN: "phc_local_test_fixture", POSTHOG_IP_SAFE_EGRESS_VERIFIED: "true",
    });
    expect(response.status).toBe(204);
    expect(await response.text()).toBe("");
    expect(written).toHaveLength(2);
    const row = written[1];
    expect(row.indexes).toEqual([batch.install_id]);
    expect(row.blobs).toHaveLength(20);
    expect(row.doubles).toHaveLength(20);
    expect(row.blobs[BLOB_COLUMNS.indexOf("event")]).toBe("funnel_usage");
    expect(JSON.parse(row.blobs[BLOB_COLUMNS.indexOf("aggregate_counters")])).toEqual(counters(batch));
    expect(row.blobs.slice(18)).toEqual(["5", "7"]);
    expect(DOUBLE_COLUMNS).toHaveLength(20);
    expect(fetch).not.toHaveBeenCalled();
  });

  it("exports the exact browser contract without changing the v3 application artifact", () => {
    const schema = JSON.parse(readFileSync(new URL("../schema/website-funnel-v5.schema.json", import.meta.url), "utf8"));
    expect(schema).toEqual(WEBSITE_FUNNEL_SCHEMA);
    const doc = readFileSync(new URL("../../docs/TELEMETRY.md", import.meta.url), "utf8");
    const extension = doc.split("## Website funnel aggregates in the source candidate")[1];
    const documented = JSON.parse(extension.match(/```json\n([\s\S]*?)```/)![1]);
    expect(documented.event).toBe("funnel_usage");
    expect(Object.keys(documented.counters)).toEqual([...FUNNEL_COUNTER_FIELDS]);
    expect(schema.properties.schema_version.const).toBe(FUNNEL_SCHEMA_VERSION);
    expect(schema.properties.notice_version.const).toBe(FUNNEL_NOTICE_VERSION);
    expect(schema.properties.events.items.oneOf[1].properties.counters.required).toEqual([...FUNNEL_COUNTER_FIELDS]);
    const legacy = JSON.parse(readFileSync(new URL("../schema/cwc-product-v3.schema.json", import.meta.url), "utf8"));
    expect(legacy.properties.schema_version.const).toBe(3);
    expect(legacy.properties.events.items.properties.counters.required).toEqual([...PRODUCT_COUNTER_FIELDS]);
  });

  it.each(FUNNEL_COUNTER_FIELDS)("requires bounded aggregate %s", (field) => {
    for (const invalid of [undefined, -1, 0.5, 4294967296, NaN, Infinity, "1", true]) {
      const batch = fixture(); counters(batch)[field] = invalid;
      expect(validateBatch(batch).ok, `${field}: ${String(invalid)}`).toBe(false);
    }
    const batch = fixture(); counters(batch)[field] = 4294967295;
    expect(validateBatch(batch).ok).toBe(true);
  });

  it.each([1, 2, 3, 4])("refuses funnel events in historical schema %i", (version) => {
    const batch = fixture(); batch.schema_version = version;
    if (version === 1) delete batch.notice_version;
    if (version === 2) { delete batch.notice_version; batch.consent_version = 4; }
    if (version === 3) batch.notice_version = 5;
    if (version === 4) batch.notice_version = 6;
    expect(validateBatch(batch).ok).toBe(false);
  });

  it.each(["tui", "exec", "cli", "web-app", "desktop", "control-plane"])("refuses v5 on %s", (surface) => {
    const batch = fixture(); batch.surface = surface;
    expect(validateBatch(batch).ok).toBe(false);
  });

  it("rejects content, fingerprinting, empty events and incorrect notice before any write", async () => {
    const mutations = [
      (batch: ReturnType<typeof fixture>) => { batch.url = "/private"; },
      (batch: ReturnType<typeof fixture>) => { counters(batch).search_query = "private"; },
      (batch: ReturnType<typeof fixture>) => { batch.git_sha = "abcdef012345"; },
      (batch: ReturnType<typeof fixture>) => { batch.os = "macos"; },
      (batch: ReturnType<typeof fixture>) => { batch.tty = true; },
      (batch: ReturnType<typeof fixture>) => { batch.notice_version = 6; },
      (batch: ReturnType<typeof fixture>) => { batch.events = []; },
      (batch: ReturnType<typeof fixture>) => { batch.events = [{ event: "session_start", source: "interactive" }]; },
    ];
    for (const mutate of mutations) {
      const batch = fixture(); mutate(batch);
      const { env, written } = harness();
      expect((await worker.fetch(postJson(batch), env)).status).toBe(400);
      expect(written).toHaveLength(0);
    }
  });

  it("keeps the historical goldens and v6 accepted, with runtime v4 and v6 first-party only", async () => {
    const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
    for (const version of [1, 2, 3, 4, 6]) {
      const batch = JSON.parse(readFileSync(new URL(`../../crates/telemetry/tests/golden/v${version}.json`, import.meta.url), "utf8"));
      expect(validateBatch(batch).ok, `v${version}`).toBe(true);
      if (version !== 4 && version !== 6) continue;
      const { env, written } = harness();
      expect((await worker.fetch(postJson(batch), { ...env, POSTHOG_HOST: "https://us.i.posthog.com", POSTHOG_PROJECT_TOKEN: "phc_local_test_fixture", POSTHOG_IP_SAFE_EGRESS_VERIFIED: "true" })).status).toBe(204);
      expect(written).toHaveLength(batch.events.length);
      expect(fetch).not.toHaveBeenCalled();
    }
  });
});
