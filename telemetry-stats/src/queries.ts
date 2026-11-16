import { tableName } from "./sql";
import { ROW_LIMIT } from "./sql";
import type { SqlRunner, StatRow } from "./types";

export const FUNNEL_COUNTER_FIELDS = [
  "home_view", "install_view", "quickstart_view", "contribute_view",
  "install_intent", "install_confirmed", "star_exit", "issue_exit",
  "help_wanted_exit", "guide_exit", "docs_search", "docs_search_empty",
  "docs_search_result", "quickstart_confirmed",
] as const;

const DAY_MS = 86_400_000;
const CHUNK_DAYS = 7;
const BATCH = 4;
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;
const HEX32 = /[0-9a-f]{32}/i;
const RUNTIME_SURFACES = "'tui', 'exec', 'cli', 'app-server', 'mcp-server', 'serve'";
const SURFACE_VALUES = new Set(["tui", "exec", "cli", "app-server", "mcp-server", "serve", "website", "web-app", "desktop", "control-plane", "vscode-extension"]);
const OS_VALUES = new Set(["linux", "macos", "windows", "freebsd", "android", "other"]);
const KEY_PATTERNS: Record<KeyKind, RegExp> = {
  version: /^\d+\.\d+\.\d+(-[a-z]+(\.\d+)?)?$/,
  provider: /^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$/,
  model: /^[a-z0-9][a-z0-9._:/+-]{0,127}$/,
  tool: /^[a-z][a-z0-9_]{0,39}$/,
  surface: /^[a-z][a-z-]{0,19}$/,
  os: /^[a-z]{3,10}$/,
  all: /^all$/,
};

type KeyKind = "version" | "provider" | "model" | "tool" | "surface" | "os" | "all";

export const CAVEATS = [
  "Observed active installs are distinct rotating anonymous ids with a session_start that UTC day. They are not people, accounts or total installs.",
  "Sessions, turns, tool calls, tokens and funnel counters are weighted by sample interval, so they stay correct if Analytics Engine samples. Active installs and the native funnel are distinct counts and are not weighted.",
  "The native funnel counts distinct ids from the six runtime surfaces that initialized or completed a successful turn that UTC day; ids that started from CI are not excluded.",
  "Coverage is a lower bound: opted-out installs, older clients, offline shutdowns and dropped flushes are invisible. Funnel dates are received-batch dates.",
  "Raw rows age out of Analytics Engine after three months; the daily totals kept beyond that hold counts only and no install ids.",
];

export function utcDay(date: Date): string {
  return date.toISOString().slice(0, 10);
}

export function startOfUtcDay(date: Date): Date {
  return new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate()));
}

function sqlTime(date: Date): string {
  return date.toISOString().slice(0, 19).replace("T", " ");
}

function windowClause(source: Source, from: Date, to: Date): string {
  if (!(from < to)) throw new Error("from must precede to");
  if (source === "binding") return `timestamp >= '${from.toISOString().slice(0, 19)}Z' AND timestamp < '${to.toISOString().slice(0, 19)}Z'`;
  return `timestamp >= toDateTime('${sqlTime(from)}') AND timestamp < toDateTime('${sqlTime(to)}')`;
}

function weighted(source: Source, expr: string): string {
  return source === "binding" ? `SUM(${expr})` : `sum(_sample_interval * ${expr})`;
}

function weightedCount(source: Source): string {
  return source === "binding" ? "COUNT(*)" : "sum(_sample_interval)";
}

function safeDay(value: unknown): string {
  const day = String(value).slice(0, 10);
  if (!/^\d{4}-\d\d-\d\d$/.test(day)) throw new Error("invalid day in SQL response");
  return day;
}

function safeKey(value: unknown, kind: KeyKind): string {
  const key = String(value ?? "").trim();
  if (key === "") return "unknown";
  if (key.length > 80 || UUID.test(key) || HEX32.test(key) || !KEY_PATTERNS[kind].test(key)) return "other";
  if (kind === "surface" && !SURFACE_VALUES.has(key)) return "other";
  if (kind === "os" && !OS_VALUES.has(key)) return "other";
  return key;
}

function safeValue(value: unknown): number {
  const number = Number(value);
  if (!Number.isFinite(number) || number < 0) throw new Error("invalid count in SQL response");
  return number;
}

const DAY = "toStartOfDay(timestamp)";


type Row = Record<string, unknown>;
type Source = SqlRunner["source"];

function sumInto(rows: StatRow[]): StatRow[] {
  const merged = new Map<string, StatRow>();
  for (const row of rows) {
    const id = `${row.day}\u0000${row.metric}\u0000${row.dim}\u0000${row.key}`;
    const existing = merged.get(id);
    if (existing) existing.value += row.value;
    else merged.set(id, { ...row });
  }
  return [...merged.values()];
}

interface Plan {
  sql: string;
  parse(rows: Row[]): StatRow[];
}

function installDimension(source: Source, table: string, w: string, dim: string, column: string | null, kind: KeyKind): Plan {
  const keyExpr = column ?? "'all'";
  return {
    sql: `SELECT ${DAY} AS day, ${keyExpr} AS key, count(DISTINCT index1) AS installs, ${weightedCount(source)} AS sessions FROM ${table} WHERE ${w} AND blob1 = 'session_start' GROUP BY day, key LIMIT ${ROW_LIMIT}`,
    parse: (rows) =>
      rows.flatMap((row) => [
        { day: safeDay(row.day), metric: "active_installs", dim, key: safeKey(row.key, kind), value: safeValue(row.installs) },
        { day: safeDay(row.day), metric: "sessions", dim, key: safeKey(row.key, kind), value: safeValue(row.sessions) },
      ]),
  };
}

export function buildPlans(source: Source, from: Date, to: Date): Plan[] {
  const table = tableName(source);
  const w = windowClause(source, from, to);
  return [
    installDimension(source, table, w, "all", null, "all"),
    installDimension(source, table, w, "version", "blob6", "version"),
    installDimension(source, table, w, "surface", "blob2", "surface"),
    installDimension(source, table, w, "os", "blob3", "os"),
    {
      sql: `SELECT ${DAY} AS day, blob2 AS surface, ${weighted(source, "double1")} AS turns, ${weighted(source, "double2")} AS tool_calls FROM ${table} WHERE ${w} AND blob1 = 'session_end' GROUP BY day, surface LIMIT ${ROW_LIMIT}`,
      parse: (rows) =>
        rows.flatMap((row) =>
          ["all", "surface"].flatMap((dim) =>
            [["turns", row.turns], ["tool_calls", row.tool_calls]].map(([metric, value]) => ({
              day: safeDay(row.day),
              metric: String(metric),
              dim,
              key: dim === "all" ? "all" : safeKey(row.surface, "surface"),
              value: safeValue(value),
            })),
          ),
        ),
    },
    {
      sql: `SELECT ${DAY} AS day, blob16 AS tool, ${weighted(source, "double1")} AS calls, ${weighted(source, "double2")} AS errors FROM ${table} WHERE ${w} AND blob1 = 'tool_usage' GROUP BY day, tool LIMIT ${ROW_LIMIT}`,
      parse: (rows) =>
        rows.flatMap((row) => [
          { day: safeDay(row.day), metric: "tool_calls", dim: "tool", key: safeKey(row.tool, "tool"), value: safeValue(row.calls) },
          { day: safeDay(row.day), metric: "tool_errors", dim: "tool", key: safeKey(row.tool, "tool"), value: safeValue(row.errors) },
        ]),
    },
    {
      sql: `SELECT ${DAY} AS day, blob15 AS provider, blob16 AS model, ${weighted(source, "double1")} AS turns_ok, ${weighted(source, "double2")} AS turns_failed, ${weighted(source, "double4")} AS input_tokens, ${weighted(source, "double5")} AS output_tokens FROM ${table} WHERE ${w} AND blob1 = 'model_usage' GROUP BY day, provider, model LIMIT ${ROW_LIMIT}`,
      parse: (rows) =>
        rows.flatMap((row) =>
          (
            [
              ["model_turns_ok", row.turns_ok],
              ["model_turns_failed", row.turns_failed],
              ["model_input_tokens", row.input_tokens],
              ["model_output_tokens", row.output_tokens],
            ] as const
          ).flatMap(([metric, value]) => [
            { day: safeDay(row.day), metric, dim: "provider", key: safeKey(row.provider, "provider"), value: safeValue(value) },
            { day: safeDay(row.day), metric, dim: "model", key: `${safeKey(row.provider, "provider")}/${safeKey(row.model, "model")}`, value: safeValue(value) },
          ]),
        ),
    },
    {
      sql: `SELECT ${DAY} AS day, count(DISTINCT index1) AS installs FROM ${table} WHERE ${w} AND blob1 = 'install_or_upgrade' AND blob9 = 'install' AND blob2 IN (${RUNTIME_SURFACES}) GROUP BY day LIMIT ${ROW_LIMIT}`,
      parse: (rows) =>
        rows.map((row) => ({ day: safeDay(row.day), metric: "native_installs_initialized", dim: "funnel", key: "all", value: safeValue(row.installs) })),
    },
    {
      sql: `SELECT ${DAY} AS day, count(DISTINCT index1) AS installs FROM ${table} WHERE ${w} AND blob1 = 'model_usage' AND double1 > 0 AND blob2 IN (${RUNTIME_SURFACES}) GROUP BY day LIMIT ${ROW_LIMIT}`,
      parse: (rows) =>
        rows.map((row) => ({ day: safeDay(row.day), metric: "native_installs_with_successful_turn", dim: "funnel", key: "all", value: safeValue(row.installs) })),
    },
    funnelPlan(source, from, to),
  ];
}

export function funnelPlan(source: Source, from: Date, to: Date): Plan {
  const table = tableName(source);
  const w = windowClause(source, from, to);
  return {
    sql: `SELECT ${DAY} AS day, blob18 AS counters, ${weightedCount(source)} AS weight FROM ${table} WHERE ${w} AND blob1 = 'funnel_usage' AND blob2 = 'website' AND blob19 = '5' GROUP BY day, counters LIMIT ${ROW_LIMIT}`,
    parse(rows) {
      const out: StatRow[] = [];
      for (const row of rows) {
        let parsed: unknown;
        try {
          parsed = JSON.parse(String(row.counters));
        } catch {
          continue;
        }
        if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) continue;
        const counters = parsed as Record<string, unknown>;
        const weight = safeValue(row.weight);
        for (const field of FUNNEL_COUNTER_FIELDS) {
          const raw = counters[field];
          if (raw === null || raw === undefined) continue;
          const value = Number(raw);
          if (!Number.isFinite(value) || value < 0) continue;
          out.push({ day: safeDay(row.day), metric: "website_funnel", dim: "funnel", key: field, value: value * weight });
        }
      }
      return out;
    },
  };
}

function order(rows: StatRow[]): StatRow[] {
  return rows.sort((a, b) => a.day.localeCompare(b.day) || a.metric.localeCompare(b.metric) || a.dim.localeCompare(b.dim) || a.key.localeCompare(b.key));
}

export async function* statChunks(runner: SqlRunner, from: Date, to: Date): AsyncGenerator<StatRow[]> {
  for (let start = from.getTime(); start < to.getTime(); start += CHUNK_DAYS * DAY_MS) {
    const end = new Date(Math.min(start + CHUNK_DAYS * DAY_MS, to.getTime()));
    const plans = buildPlans(runner.source, new Date(start), end);
    const rows: StatRow[] = [];
    for (let i = 0; i < plans.length; i += BATCH) {
      const parsed = await Promise.all(plans.slice(i, i + BATCH).map(async (plan) => plan.parse((await runner.run(plan.sql)) as Row[])));
      rows.push(...parsed.flat());
    }
    yield order(sumInto(rows));
  }
}

export async function collectStats(runner: SqlRunner, from: Date, to: Date): Promise<StatRow[]> {
  const rows: StatRow[] = [];
  for await (const chunk of statChunks(runner, from, to)) rows.push(...chunk);
  return rows;
}

export function allPlanSql(source: Source, from: Date, to: Date): string[] {
  return buildPlans(source, from, to).map((plan) => plan.sql);
}
