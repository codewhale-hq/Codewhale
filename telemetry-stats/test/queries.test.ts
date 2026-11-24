import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { FUNNEL_COUNTER_FIELDS as INGEST_FUNNEL } from "../../telemetry-ingest/src/schema";
import { allPlanSql, collectStats, FUNNEL_COUNTER_FIELDS } from "../src/queries";
import { answer, fakeRunner, INSTALL_A } from "./support";

const FROM = new Date("2026-11-10T00:00:00Z");
const TO = new Date("2026-11-12T00:00:00Z");

describe("queries", () => {
  it("keeps the funnel counter list identical to the ingest schema", () => {
    expect([...FUNNEL_COUNTER_FIELDS]).toEqual([...INGEST_FUNNEL]);
  });

  it("only ever uses index1 inside count(DISTINCT index1)", () => {
    for (const source of ["binding", "rest"] as const) {
      for (const sql of allPlanSql(source, FROM, TO)) {
        expect(sql.replaceAll("count(DISTINCT index1)", "")).not.toMatch(/index1|install_id/);
      }
    }
  });

  it("uses the binding table without FORMAT or accountTag, and bare table for REST", () => {
    const [binding] = allPlanSql("binding", FROM, TO);
    expect(binding).toContain('events.analyticsEngine."codewhale_telemetry"');
    expect(binding).not.toMatch(/FORMAT|accountTag/);
    expect(allPlanSql("rest", FROM, TO)[0]).toContain("FROM codewhale_telemetry WHERE");
  });

  it("weights per dialect: automatic on the binding, by _sample_interval on the REST API", () => {
    for (const sql of allPlanSql("binding", FROM, TO)) {
      expect(sql).not.toMatch(/_sample_interval|sampleInterval|toString|toDate\(/);
      expect(sql).toContain("toStartOfDay(timestamp)");
      expect(sql).toContain("timestamp >= '2026-11-10T00:00:00Z'");
    }
    for (const sql of allPlanSql("rest", FROM, TO)) {
      if (!sql.includes("count(DISTINCT index1)")) expect(sql).toContain("_sample_interval");
      expect(sql).toContain("toDateTime('2026-11-10 00:00:00')");
      expect(sql).not.toMatch(/toString|COUNT\(\*\)/);
    }
    const all = allPlanSql("binding", FROM, TO);
    expect(all.some((sql) => sql.includes("COUNT(*)"))).toBe(true);
    expect(all.some((sql) => sql.includes("SUM(double1)"))).toBe(true);
  });

  it("limits the native funnel to the six runtime surfaces", () => {
    const native = allPlanSql("binding", FROM, TO).filter((sql) => sql.includes("'install_or_upgrade'") || sql.includes("double1 > 0"));
    expect(native).toHaveLength(2);
    for (const sql of native) expect(sql).toContain("blob2 IN ('tui', 'exec', 'cli', 'app-server', 'mcp-server', 'serve')");
  });

  it("walks a long window in weekly chunks", async () => {
    const { runner, seen } = fakeRunner(() => []);
    await collectStats(runner, new Date("2026-08-14T00:00:00Z"), new Date("2026-11-12T00:00:00Z"));
    expect(seen.length).toBe(13 * 10);
    expect(seen.some((sql) => sql.includes("'2026-08-21T00:00:00Z'"))).toBe(true);
  });

  it("rejects an empty or inverted window", () => {
    expect(() => allPlanSql("binding", TO, FROM)).toThrow();
  });

  it("turns a fake Analytics Engine response into aggregate rows", async () => {
    const { runner } = fakeRunner(answer);
    const rows = await collectStats(runner, FROM, TO);
    const pick = (metric: string, dim: string, key: string, day = "2026-11-10") => rows.find((r) => r.metric === metric && r.dim === dim && r.key === key && r.day === day)?.value;
    expect(pick("active_installs", "all", "all")).toBe(4);
    expect(pick("sessions", "version", "0.10.3")).toBe(10);
    expect(pick("active_installs", "os", "macos", "2026-11-11")).toBe(5);
    expect(pick("turns", "all", "all")).toBe(35);
    expect(pick("tool_calls", "all", "all")).toBe(91);
    expect(pick("tool_calls", "tool", "bash")).toBe(70);
    expect(pick("model_turns_ok", "provider", "deepseek")).toBe(10);
    expect(pick("model_turns_ok", "model", "deepseek/other")).toBe(8);
    expect(pick("model_input_tokens", "model", "deepseek/deepseek-v4")).toBe(50);
    expect(pick("native_installs_initialized", "funnel", "all")).toBe(3);
    expect(pick("native_installs_with_successful_turn", "funnel", "all")).toBe(2);
    expect(pick("website_funnel", "funnel", "home_view")).toBe(4 * 2 + 1);
    expect(pick("website_funnel", "funnel", "install_view")).toBe(4);
  });

  it("sums rows whose keys collapse to other instead of overwriting them", async () => {
    const { runner } = fakeRunner((sql) =>
      sql.includes("blob6 AS key") ? [{ day: "2026-11-10", key: "0.10.3 (weird)", installs: "4", sessions: "1" }, { day: "2026-11-10", key: "0.10.3 [dev]", installs: "6", sessions: "2" }] : [],
    );
    const rows = await collectStats(runner, FROM, TO);
    expect(rows.filter((r) => r.dim === "version" && r.metric === "active_installs")).toEqual([{ day: "2026-11-10", metric: "active_installs", dim: "version", key: "other", value: 10 }]);
  });

  it("rejects negative or non-numeric counts", async () => {
    const { runner } = fakeRunner(() => [{ day: "2026-11-10", key: "all", installs: "-1", sessions: "1" }]);
    await expect(collectStats(runner, FROM, TO)).rejects.toThrow();
  });

  it("preserves every lowercased bundled catalog model id without folding to other", async () => {
    const catalog = JSON.parse(readFileSync(new URL("../../crates/config/assets/models_dev.bundled.json", import.meta.url), "utf8")) as {
      models?: Record<string, unknown>;
      providers?: Record<string, { models?: Record<string, unknown> }>;
      _reviewed?: { intrinsic?: Record<string, unknown> };
    };
    const ids = new Set<string>();
    for (const id of Object.keys(catalog.models ?? {})) ids.add(id.toLowerCase());
    for (const provider of Object.values(catalog.providers ?? {})) {
      for (const id of Object.keys(provider.models ?? {})) ids.add(id.toLowerCase());
    }
    for (const id of Object.keys(catalog._reviewed?.intrinsic ?? {})) ids.add(id.toLowerCase());
    expect(ids.size).toBeGreaterThan(0);

    const { runner } = fakeRunner((sql) =>
      sql.includes("blob1 = 'model_usage'") && !sql.includes("double1 > 0")
        ? [...ids].map((model) => ({
            day: "2026-11-10",
            provider: "openrouter",
            model,
            turns_ok: "1",
            turns_failed: "0",
            input_tokens: "10",
            output_tokens: "5",
          }))
        : [],
    );
    const rows = await collectStats(runner, FROM, TO);
    const modelKeys = rows.filter((r) => r.dim === "model" && r.metric === "model_turns_ok").map((r) => r.key);
    expect(modelKeys).not.toContain("openrouter/other");
    expect(modelKeys.sort()).toEqual([...ids].map((id) => `openrouter/${id}`).sort());
  });

  it("skips funnel rows whose JSON counters parse to null or a non-object", async () => {
    const { runner } = fakeRunner((sql) =>
      sql.includes("'funnel_usage'")
        ? [
            { day: "2026-11-10", counters: "null", weight: "1" },
            { day: "2026-11-10", counters: "123", weight: "1" },
            { day: "2026-11-10", counters: "[]", weight: "1" },
            { day: "2026-11-10", counters: JSON.stringify({ home_view: 3 }), weight: "2" },
          ]
        : [],
    );
    const rows = await collectStats(runner, FROM, TO);
    expect(rows.filter((r) => r.metric === "website_funnel")).toEqual([
      { day: "2026-11-10", metric: "website_funnel", dim: "funnel", key: "home_view", value: 6 },
    ]);
  });
});

describe("no install id leaves the stats layer", () => {
  const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

  it("no field is an install id and no value looks like one, even when the source row carries one", async () => {
    const { runner } = fakeRunner((sql) => {
      const rows = answer(sql) as Record<string, unknown>[];
      return rows.map((row) => ({ ...row, index1: INSTALL_A, install_id: INSTALL_A, key: row.key ?? INSTALL_A }));
    });
    const rows = await collectStats(runner, FROM, TO);
    const text = JSON.stringify(rows);
    expect(text).not.toMatch(UUID);
    expect(text).not.toMatch(/install_id|index1/);
    for (const row of rows) expect(Object.keys(row).sort()).toEqual(["day", "dim", "key", "metric", "value"]);
  });

  it("maps an id-shaped dimension value to other", async () => {
    const { runner } = fakeRunner((sql) => (sql.includes("'tool_usage'") ? [{ day: "2026-11-10", tool: INSTALL_A, calls: "1", errors: "0" }] : []));
    const rows = await collectStats(runner, FROM, TO);
    expect(rows.filter((r) => r.dim === "tool").map((r) => r.key)).toEqual(["other", "other"]);
  });

  it("maps unhyphenated ids, emails, paths and host:port to other", async () => {
    for (const bad of ["3f2b1c4e5a6d4e7f8a9b0c1d2e3f4a5b", "alice@example.com", "/users/alice/x", "10.0.0.1:8080"]) {
      const { runner } = fakeRunner((sql) => (sql.includes("blob6 AS key") ? [{ day: "2026-11-10", key: bad, installs: "1", sessions: "1" }] : []));
      const rows = await collectStats(runner, FROM, TO);
      expect(rows.filter((r) => r.dim === "version").map((r) => r.key)).toEqual(["other", "other"]);
    }
  });
});
