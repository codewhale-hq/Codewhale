#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { FUNNEL_COUNTER_FIELDS } from "../src/schema.ts";
import { querySql } from "./report-active-installs.mjs";

const DAY_MS = 86_400_000;
const ROW_LIMIT = 10_001;
const NATIVE_SURFACES = ["tui", "exec", "cli", "app-server", "mcp-server", "serve"];
export const CAVEATS = [
  "Website counters are aggregate interactions, not unique visitors or user conversion. Self-confirmations do not verify installation or a useful task.",
  "Browser and native identifiers are independent; this report makes no website-to-CLI, GitHub or cross-device attribution join.",
  "Runtime install means first initialized enabled telemetry state, not all installations or new people. IDs rotate every 90 days and are removed on opt-out.",
  "Native success is observed model turns_ok, not a first-ever primary or useful task. Missing usage, same-process child activity and late cancellation can affect it.",
  "Dates are received batch dates rather than exact event times. Offline, opted-out and dropped deliveries are absent; ingest has no replay deduplication guarantee.",
  "Native IDs with an observed CI start are excluded in full because the aggregate schema cannot assign model counts to individual sessions.",
];

function instant(value, field) {
  if (typeof value !== "string" || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{3})?Z$/.test(value) || !Number.isFinite(Date.parse(value))) throw new Error(`invalid ${field}`);
  return new Date(value);
}
function count(value, field) {
  if (typeof value !== "number" && (typeof value !== "string" || !/^\d+(?:\.0+)?$/.test(value))) throw new Error(`invalid ${field}`);
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 0) throw new Error(`invalid ${field}`);
  return number;
}
function within(date, from, to) { return date >= from && date < to; }
function sqlDate(date) { return date.toISOString().slice(0, 19).replace("T", " "); }
function rows(payload) {
  if (!payload || !Array.isArray(payload.data) || payload.data.length >= ROW_LIMIT || payload.rows_before_limit_at_least >= ROW_LIMIT) throw new Error("incomplete or malformed query receipt");
  return payload.data;
}
function fresh(value) { return value.replace(" ", "T").replace(/(?:Z)?$/, "Z"); }

export function funnelSql(fromValue, toValue) {
  const from = instant(fromValue, "from"); const to = instant(toValue, "to");
  if (from >= to) throw new Error("from must precede to");
  const where = `timestamp >= toDateTime('${sqlDate(from)}') AND timestamp < toDateTime('${sqlDate(to)}')`;
  return {
    website: `SELECT toString(toDate(timestamp)) AS day, blob18 AS counters, SUM(_sample_interval) AS weight FROM codewhale_telemetry WHERE ${where} AND blob1 = 'funnel_usage' AND blob2 = 'website' AND blob19 = '5' GROUP BY day, counters LIMIT ${ROW_LIMIT} FORMAT JSON`,
    native: `SELECT index1 AS install_id, blob1 AS event, blob2 AS surface, blob9 AS install_kind, blob11 AS session_source, toString(timestamp) AS received_at, blob19 AS schema_version, double1 AS turns_ok, _sample_interval AS weight FROM codewhale_telemetry WHERE ${where} AND blob1 IN ('install_or_upgrade','session_start','model_usage') AND blob2 IN ('tui','exec','cli','app-server','mcp-server','serve') LIMIT ${ROW_LIMIT} FORMAT JSON`,
  };
}

export function buildFunnelReport(input, { now = new Date() } = {}) {
  if (!input || input.format !== "codewhale-funnel-receipt-v1" || !["analytics-engine", "offline-receipt", "synthetic"].includes(input.source)) throw new Error("unsupported receipt format or source");
  const coverage = input.coverage;
  if (!coverage || coverage.accepted_only !== true) throw new Error("receipt must establish accepted-only source coverage");
  const from = instant(coverage.from, "coverage.from"); const to = instant(coverage.to, "coverage.to");
  if (from >= to || to > now || to - from > 90 * DAY_MS) throw new Error("invalid or future coverage window; maximum 90 days");
  if (!Array.isArray(input.website_rows) || !Array.isArray(input.native_rows) || input.website_rows.length >= ROW_LIMIT || input.native_rows.length >= ROW_LIMIT) throw new Error("missing or truncated receipt rows");
  const totals = Object.fromEntries(FUNNEL_COUNTER_FIELDS.map((field) => [field, 0]));
  const days = new Map(); let sampled = false; let newest = null;
  for (const row of input.website_rows) {
    const date = instant(`${row.day}T00:00:00Z`, "website day");
    if (!within(date, new Date(from.toISOString().slice(0, 10) + "T00:00:00Z"), to)) throw new Error("website row outside coverage");
    const weight = count(row.weight, "weight"); if (weight < 1) throw new Error("weight must be positive");
    const counters = typeof row.counters === "string" ? JSON.parse(row.counters) : row.counters;
    if (!counters || typeof counters !== "object" || Object.keys(counters).length !== FUNNEL_COUNTER_FIELDS.length || Object.keys(counters).some((field) => !FUNNEL_COUNTER_FIELDS.includes(field))) throw new Error("unknown website counter field");
    const daily = days.get(row.day) ?? Object.fromEntries(FUNNEL_COUNTER_FIELDS.map((field) => [field, 0]));
    for (const field of FUNNEL_COUNTER_FIELDS) {
      const value = count(counters[field], field); if (value > 4294967295) throw new Error("website counter out of range");
      const weighted = value * weight;
      if (!Number.isSafeInteger(weighted) || !Number.isSafeInteger(totals[field] + weighted)) throw new Error("report counter overflow");
      daily[field] += weighted; totals[field] += weighted;
    }
    days.set(row.day, daily);
    if (newest === null || date > newest) newest = date;
  }
  const installs = new Map(); const successes = new Map(); const started = new Set(); const ci = new Set();
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  for (const row of input.native_rows) {
    if (!uuid.test(row.install_id) || !NATIVE_SURFACES.includes(row.surface) || ![1, 2, 3, 4].includes(count(row.schema_version, "schema_version"))) throw new Error("invalid native identity, surface or version");
    const at = instant(fresh(row.received_at), "received_at");
    if (!within(at, from, to)) throw new Error("native row outside coverage");
    if (newest === null || at > newest) newest = at;
    const weight = count(row.weight, "weight"); if (weight < 1) throw new Error("weight must be positive");
    if (weight !== 1) sampled = true;
    if (row.event === "install_or_upgrade") {
      if (!["install", "upgrade", "downgrade"].includes(row.install_kind)) throw new Error("invalid install kind");
      if (row.install_kind === "install" && (!installs.has(row.install_id) || at < installs.get(row.install_id))) installs.set(row.install_id, at);
    } else if (row.event === "session_start") {
      if (!["interactive", "resume", "fork", "api", "unknown", "ci"].includes(row.session_source)) throw new Error("invalid session source");
      if (row.session_source === "ci") ci.add(row.install_id);
      started.add(row.install_id);
    } else if (row.event === "model_usage") {
      if (count(row.schema_version, "schema_version") !== 4) throw new Error("model usage requires v4");
      if (count(row.turns_ok, "turns_ok") > 0) {
        const observations = successes.get(row.install_id) ?? []; observations.push(at); successes.set(row.install_id, observations);
      }
    } else throw new Error("unexpected native event");
  }
  const eligible = [...installs].filter(([id, at]) => !ci.has(id) && at.getTime() + 7 * DAY_MS <= to.getTime());
  const completed = eligible.filter(([id, at]) => successes.has(id) && successes.get(id).some((observed) => observed >= at && observed.getTime() < at.getTime() + 7 * DAY_MS));
  const unavailable = coverage.complete !== true ? "coverage is incomplete" : sampled ? "native query was sampled; distinct cohort coverage is incomplete" : eligible.length === 0 ? "no fully observed seven-day native initialization cohort" : null;
  return {
    report: "codewhale_observed_funnel", generated_at: now.toISOString(), source: input.source, synthetic: input.source === "synthetic", coverage,
    freshness: { newest_observed_received_at: newest?.toISOString() ?? null, age_hours: newest === null ? null : Math.max(0, (now - newest) / 3_600_000), website_time_resolution: "UTC day only" },
    website: { measure: "weighted interaction counts; no unique visitors or cross-surface conversion", totals, days: [...days].sort(([a], [b]) => a.localeCompare(b)).map(([day, counters]) => ({ day, counters })) },
    native: {
      measure: "observed native initialization and technical model completion, not people or useful tasks",
      observed_started_ids: [...started].filter((id) => !ci.has(id)).length,
      observed_initialized_ids: [...installs.keys()].filter((id) => !ci.has(id)).length,
      observed_completed_ids: [...successes.keys()].filter((id) => !ci.has(id)).length,
      excluded_ci_ids: ci.size, incomplete_followup_ids: [...installs].filter(([id, at]) => !ci.has(id) && at.getTime() + 7 * DAY_MS > to.getTime()).length,
      seven_day_initialization_cohort: { denominator: eligible.length, numerator: completed.length, fraction: unavailable === null ? completed.length / eligible.length : null, unavailable_reason: unavailable, sampled },
    },
    caveats: CAVEATS,
  };
}

export function parseArgs(argv) {
  const result = { input: null, from: null, to: null, sql: false };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (arg === "--sql") result.sql = true;
    else if (["--input", "--from", "--to"].includes(arg)) {
      const value = argv[++index]; if (!value || value.startsWith("--")) throw new Error(`missing ${arg}`);
      result[arg.slice(2)] = value;
    } else throw new Error("unknown report option");
  }
  return result;
}

export async function main(argv = process.argv.slice(2), env = process.env) {
  const args = parseArgs(argv);
  if (args.input) {
    if (args.from || args.to || args.sql) throw new Error("offline input already defines its coverage");
    return buildFunnelReport(JSON.parse(readFileSync(args.input, "utf8")));
  }
  const to = args.to ?? new Date().toISOString().slice(0, 10) + "T00:00:00Z";
  const from = args.from ?? new Date(Date.parse(to) - 30 * DAY_MS).toISOString();
  const sql = funnelSql(from, to);
  if (args.sql) return { source: "query-plan-only", sql, caveats: CAVEATS };
  const accountId = env.CF_ACCOUNT_ID?.trim(); const apiToken = env.CF_API_TOKEN?.trim();
  if (!accountId || !apiToken) throw new Error("CF_ACCOUNT_ID and CF_API_TOKEN are required; use --input for offline receipts or --sql for query plans");
  const [websitePayload, nativePayload] = await Promise.all([
    querySql({ accountId, apiToken, sql: sql.website }),
    querySql({ accountId, apiToken, sql: sql.native }),
  ]);
  const website = rows(websitePayload); const native = rows(nativePayload);
  return buildFunnelReport({ format: "codewhale-funnel-receipt-v1", source: "analytics-engine", coverage: { from, to, complete: true, accepted_only: true }, website_rows: website, native_rows: native });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then((report) => process.stdout.write(`${JSON.stringify(report, null, 2)}\n`)).catch((error) => { process.stderr.write(`report:funnel: ${error instanceof SyntaxError ? "invalid JSON receipt or query response" : error.message}\n`); process.exitCode = 1; });
}
