import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { buildFunnelReport, funnelSql, parseArgs } from "../scripts/report-funnel.mjs";
import { contributionCohorts, exportPublicHistory } from "../scripts/report-contribution-cohorts.mjs";
import { FUNNEL_COUNTER_FIELDS } from "../src/schema";
import worker from "../src/index";
import { harness, postJson } from "./support";

const NOW = new Date("2026-11-10T00:00:00Z");
const ID = "3f2a9c1e-0000-4000-8000-000000000001";
const SECOND = "3f2a9c1e-0000-4000-8000-000000000002";
function counts(values: Record<string, number> = {}) { return { ...Object.fromEntries(FUNNEL_COUNTER_FIELDS.map((field) => [field, 0])), ...values }; }
function native(event: string, at: string, id = ID) { return { install_id: id, event, surface: "tui", install_kind: "install", session_source: "interactive", received_at: at, schema_version: 4, turns_ok: 1, weight: 1 }; }
function receipt() {
  return { format: "codewhale-funnel-receipt-v1", source: "synthetic", coverage: { from: "2026-10-01T00:00:00Z", to: "2026-11-01T00:00:00Z", complete: true, accepted_only: true }, website_rows: [{ day: "2026-10-04", counters: counts({ install_intent: 3, install_confirmed: 2, quickstart_confirmed: 1 }), weight: 2 }], native_rows: [native("install_or_upgrade", "2026-10-02T00:00:00Z"), native("model_usage", "2026-10-03T00:00:00Z"), native("install_or_upgrade", "2026-10-30T00:00:00Z", SECOND)] };
}

describe("existing-pipeline owner funnel report", () => {
  it("separates weighted self-reports from native technical outcomes and seven-day eligibility", () => {
    const report = buildFunnelReport(receipt(), { now: NOW });
    expect(report.synthetic).toBe(true);
    expect(report.website.totals).toMatchObject({ install_intent: 6, install_confirmed: 4, quickstart_confirmed: 2 });
    expect(report.native.observed_initialized_ids).toBe(2);
    expect(report.native.incomplete_followup_ids).toBe(1);
    expect(report.native.seven_day_initialization_cohort).toMatchObject({ denominator: 1, numerator: 1, fraction: 1 });
    expect(JSON.stringify(report)).not.toContain(ID);
    expect(report.caveats.join(" ")).toContain("late cancellation");
  });

  it("handles repeated accepted native rows without turning them into new installs", () => {
    const input = receipt(); input.native_rows.push(input.native_rows[0], input.native_rows[1]);
    const report = buildFunnelReport(input, { now: NOW });
    expect(report.native.observed_initialized_ids).toBe(2);
    expect(report.native.seven_day_initialization_cohort.numerator).toBe(1);
  });

  it("does not let older model observations hide completion after a reset state", () => {
    const input = receipt(); input.native_rows.unshift(native("model_usage", "2026-10-01T12:00:00Z"));
    expect(buildFunnelReport(input, { now: NOW }).native.seven_day_initialization_cohort.numerator).toBe(1);
  });

  it("excludes CI-contaminated native identities and reports missing coverage as unavailable", () => {
    const input = receipt(); input.native_rows.push({ ...native("session_start", "2026-10-04T00:00:00Z"), session_source: "ci" });
    const report = buildFunnelReport(input, { now: NOW });
    expect(report.native.excluded_ci_ids).toBe(1);
    expect(report.native.observed_completed_ids).toBe(0);
    expect(report.native.seven_day_initialization_cohort.fraction).toBeNull();
    input.coverage.complete = false;
    expect(buildFunnelReport(input, { now: NOW }).native.seven_day_initialization_cohort.unavailable_reason).toContain("incomplete");
  });

  it("excludes fleet worker, agent and scripted starts like CI, and accepts v6 model usage", () => {
    for (const source of ["fleet_worker", "agent", "scripted"]) {
      const input = receipt();
      input.native_rows.push({ ...native("session_start", "2026-10-04T00:00:00Z"), session_source: source });
      input.native_rows.push({ ...native("model_usage", "2026-10-04T00:00:00Z"), schema_version: 6 });
      const report = buildFunnelReport(input, { now: NOW });
      expect(report.native.excluded_ci_ids, source).toBe(1);
      expect(report.native.observed_completed_ids, source).toBe(0);
    }
    const input = receipt(); input.native_rows.push({ ...native("model_usage", "2026-10-04T00:00:00Z"), schema_version: 5 });
    expect(() => buildFunnelReport(input, { now: NOW })).toThrow(/version/);
  });

  it("does not compute a cohort fraction from sampled native records", () => {
    const input = receipt(); input.native_rows[0].weight = 4;
    expect(buildFunnelReport(input, { now: NOW }).native.seven_day_initialization_cohort).toMatchObject({ fraction: null, sampled: true });
  });

  it("rejects unsupported sources, unaccepted buffers, unknown counters and truncated receipts", () => {
    const input = receipt(); input.coverage.accepted_only = false;
    expect(() => buildFunnelReport(input, { now: NOW })).toThrow(/accepted-only/);
    expect(() => buildFunnelReport({ ...receipt(), source: "local-buffer" }, { now: NOW })).toThrow(/source/);
    const unknown = receipt(); unknown.website_rows[0].counters = counts({ raw_query: 1 });
    expect(() => buildFunnelReport(unknown, { now: NOW })).toThrow(/counter field/);
    expect(() => buildFunnelReport({ ...receipt(), native_rows: Array(10001).fill(native("model_usage", "2026-10-02T00:00:00Z")) }, { now: NOW })).toThrow(/truncated/);
  });

  it("refuses future or oversized windows and avoids printing query plans as observed data", () => {
    expect(() => buildFunnelReport({ ...receipt(), coverage: { ...receipt().coverage, to: "2026-12-01T00:00:00Z" } }, { now: NOW })).toThrow(/future/);
    expect(() => buildFunnelReport({ ...receipt(), coverage: { ...receipt().coverage, from: "2025-01-01T00:00:00Z" } }, { now: NOW })).toThrow(/90/);
    const sql = funnelSql("2026-10-01T00:00:00Z", "2026-11-01T00:00:00Z");
    expect(sql.website).not.toContain("index1");
    expect(sql.website).toContain("blob19 = '5'");
    expect(sql.native).toContain("LIMIT 10001");
    expect(() => funnelSql("'; SELECT private", "2026-11-01T00:00:00Z")).toThrow(/from/);
    expect(parseArgs(["--sql"]).sql).toBe(true);
  });

  it("reports the accepted local pipeline fixture, while rejection creates no receipt rows", async () => {
    const batch = JSON.parse(readFileSync(new URL("./golden/browser-v5.json", import.meta.url), "utf8"));
    const { env, written } = harness();
    expect((await worker.fetch(postJson(batch), env)).status).toBe(204);
    const point = written.find((row) => row.blobs[0] === "funnel_usage")!;
    const input = receipt(); input.website_rows = [{ day: "2026-10-04", counters: JSON.parse(point.blobs[17]), weight: 1 }];
    expect(buildFunnelReport(input, { now: NOW }).website.totals.install_intent).toBe(1);
    const rejected = harness(); batch.events[1].counters.private_query = 1;
    expect((await worker.fetch(postJson(batch), rejected.env)).status).toBe(400);
    expect(rejected.written).toHaveLength(0);
  });
});

function publicSnapshot() {
  const user = (login: string) => ({ id: login.toLowerCase() === "alice" ? 1 : 2, login, type: "User" });
  return { format: "codewhale-public-contributions-v1", repository: "codewhale-hq/CodeWhale", source: "synthetic", coverage: { issues_complete: true, pulls_complete: true }, issues: [
    { number: 1, created_at: "2026-10-01T00:00:00Z", user: user("Alice") },
    { number: 2, created_at: "2026-10-04T00:00:00Z", user: user("alice") },
    { number: 3, created_at: "2026-10-05T00:00:00Z", user: user("Bob"), pull_request: {} },
    { number: 4, created_at: "2026-10-05T00:00:00Z", user: { login: "a-bot", type: "Bot" } },
  ], pulls: [
    { number: 3, merged_at: "2026-10-08T00:00:00Z", user: user("Bob") },
    { number: 5, merged_at: "2026-10-06T00:00:00Z", user: user("Bob") },
    { number: 6, merged_at: null, user: user("Alice") },
  ] };
}

describe("separate public repository contribution cohorts", () => {
  it("finds first issues and first merges separately, excluding bots and PRs from issues", () => {
    const result = contributionCohorts(publicSnapshot(), { from: "2026-10-01T00:00:00Z", to: "2026-11-01T00:00:00Z" });
    expect(result.totals).toEqual({ first_public_issue_authors: 1, first_merged_pr_authors: 1 });
    expect(result.days.find((row) => row.first_merged_pr_authors)?.day).toBe("2026-10-06");
    expect(result.input_counts).toEqual({ public_issues: 2, merged_prs: 2, excluded_bot_or_deleted_records: 1 });
    expect(JSON.stringify(result)).not.toMatch(/Alice|alice|Bob/);
  });

  it("does not count a returning contributor's later issue as their first in a reporting window", () => {
    const result = contributionCohorts(publicSnapshot(), { from: "2026-10-03T00:00:00Z", to: "2026-10-07T00:00:00Z" });
    expect(result.totals).toEqual({ first_public_issue_authors: 0, first_merged_pr_authors: 1 });
  });

  it("refuses incomplete history instead of trusting FIRST_TIME_CONTRIBUTOR display metadata", () => {
    const snapshot = publicSnapshot(); snapshot.coverage.pulls_complete = false;
    expect(() => contributionCohorts(snapshot)).toThrow(/incomplete/);
  });

  it("verifies public visibility before reading history and fails at the pagination cap", async () => {
    const privateCalls: string[] = [];
    await expect(exportPublicHistory({ get: async (url) => { privateCalls.push(url); return { private: true }; } })).rejects.toThrow(/public/);
    expect(privateCalls).toHaveLength(1);
    await expect(exportPublicHistory({ maxPages: 1, get: async (url) => url.includes("?") ? Array(100).fill({}) : { private: false } })).rejects.toThrow(/incomplete/);
  });

  it("completes short paginated histories without writing handles or contacting a telemetry sink", async () => {
    const snapshot = publicSnapshot(); const calls: string[] = [];
    const exported = await exportPublicHistory({ get: async (url) => { calls.push(url); return url.includes("/issues?") ? snapshot.issues : url.includes("/pulls?") ? snapshot.pulls : { private: false }; } });
    expect(calls).toHaveLength(3);
    expect(contributionCohorts(exported).synthetic).toBe(false);
  });
});
