import { describe, expect, it } from "vitest";
import { rollupDaily } from "../src/rollup";
import { answer, FakeD1, fakeRunner } from "./support";

describe("rollupDaily", () => {
  it("backfills the retention window on an empty store and stores totals only", async () => {
    const { runner, seen } = fakeRunner(answer);
    const db = new FakeD1();
    const result = await rollupDaily(runner, db, new Date("2026-11-12T02:17:00Z"));
    expect(result).toMatchObject({ from: "2026-08-14", to: "2026-11-12" });
    expect(db.rows.size).toBeGreaterThan(8);
    expect(JSON.stringify([...db.rows.values()])).not.toMatch(/install_id|index1|[0-9a-f]{8}-[0-9a-f]{4}-/);
    expect(seen.some((sql) => sql.includes("2026-08-14T00:00:00Z"))).toBe(true);
    expect(db.rows.get("2026-11-10|active_installs|all|all")?.value).toBe(4);
  });

  it("is idempotent and only re-reads the last two days once data exists", async () => {
    const db = new FakeD1();
    await rollupDaily(fakeRunner(answer).runner, db, new Date("2026-11-12T02:17:00Z"));
    const size = db.rows.size;
    const second = fakeRunner(answer);
    const result = await rollupDaily(second.runner, db, new Date("2026-11-12T02:17:00Z"));
    expect(db.rows.size).toBe(size);
    expect(result.from).toBe("2026-11-10");
    expect(second.seen.every((sql) => sql.includes("2026-11-10T00:00:00Z"))).toBe(true);
  });

  it("stores only the all and funnel dimensions, never a per-version or per-model row", async () => {
    const db = new FakeD1();
    await rollupDaily(fakeRunner((sql) => (sql.includes("blob6 AS key") ? [{ day: "2026-11-10", key: "0.10.3-acme.internal.alice", installs: "1", sessions: "7" }] : answer(sql))).runner, db, new Date("2026-11-12T02:17:00Z"));
    const dims = new Set([...db.rows.values()].map((r) => r.dim));
    expect([...dims].sort()).toEqual(["all", "funnel"]);
    expect(JSON.stringify([...db.rows.values()])).not.toContain("acme");
  });

  it("backfills in chunks and writes each one, so a failing later chunk keeps the earlier days", async () => {
    const db = new FakeD1();
    let calls = 0;
    const runner = fakeRunner((sql) => {
      if (++calls > 10 * 2) throw new Error("boom");
      return answer(sql);
    }).runner;
    await expect(rollupDaily(runner, db, new Date("2026-11-12T02:17:00Z"))).rejects.toThrow("boom");
    expect(db.rows.size).toBeGreaterThan(0);
  });

  it("writes nothing when the source returns no rows", async () => {
    const db = new FakeD1();
    const result = await rollupDaily(fakeRunner(() => []).runner, db, new Date("2026-11-12T02:17:00Z"));
    expect(result.rows).toBe(0);
    expect(db.rows.size).toBe(0);
  });
});
