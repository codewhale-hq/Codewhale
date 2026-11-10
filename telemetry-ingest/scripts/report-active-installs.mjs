#!/usr/bin/env node

/**
 * Owner report: observed active installs.
 *
 * The metric is **observed active installs** — distinct rotating anonymous
 * install ids that produced a `session_start` event on a UTC day. It is never
 * a count of people, accounts, or total installs, and the report says so next
 * to every number it prints. `report-dau.mjs` is the compatibility entry
 * point; this file is the canonical one.
 */

import { pathToFileURL } from "node:url";

const SQL_ENDPOINT = (accountId) =>
  `https://api.cloudflare.com/client/v4/accounts/${encodeURIComponent(accountId)}/analytics_engine/sql`;

const DAY_MS = 86_400_000;

/**
 * Printed with the numbers, always — a report copy-pasted into a chat carries
 * its own caveats. Wording is pinned by test/report-active-installs.test.ts.
 */
export const COVERAGE_CAVEATS = [
  "Observed active installs = distinct rotating anonymous install ids with a session_start that UTC day. Not people, not accounts, not total installs.",
  "Coverage is a lower bound: clients older than the telemetry feature, opted-out installs, and non-emitting environments (kill switches, fleet workers, offline shutdowns, dropped flushes) are invisible.",
  "Install ids rotate every 90 days and are deleted on opt-out, so week-over-week comparisons are not a retention metric.",
];

export function parseArgs(argv) {
  // 15 = 14 complete UTC days plus the partial current day, the smallest
  // window that fills both sides of the 7-day trend.
  let days = 15;
  let json = false;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--json") {
      json = true;
      continue;
    }
    if (arg === "--days") {
      const raw = argv[index + 1];
      index += 1;
      days = Number(raw);
      continue;
    }
    throw new Error(`unknown argument: ${arg}`);
  }
  if (!Number.isInteger(days) || days < 1 || days > 90) {
    throw new Error("--days must be an integer from 1 through 90");
  }
  return { days, json };
}

export function activeInstallsSql(days) {
  return `SELECT
  toDate(timestamp) AS day,
  count(DISTINCT index1) AS active_installs,
  sum(_sample_interval) AS sessions_started
FROM codewhale_telemetry
WHERE timestamp >= toStartOfDay(NOW()) - INTERVAL '${days - 1}' DAY
  AND blob1 = 'session_start'
GROUP BY day
ORDER BY day DESC
FORMAT JSON`;
}

/** Newest ingested event of any kind — how stale the dataset is. */
export function freshnessSql() {
  return `SELECT
  max(timestamp) AS newest_event
FROM codewhale_telemetry
FORMAT JSON`;
}

function dataRows(payload) {
  const rows = Array.isArray(payload) ? payload : payload?.data;
  if (!Array.isArray(rows)) {
    throw new Error("Cloudflare SQL response did not contain a data array");
  }
  return rows;
}

export function rowsFromResponse(payload) {
  return dataRows(payload).map((row) => ({
    day: String(row.day),
    active_installs: Number(row.active_installs),
    sessions_started: Number(row.sessions_started),
  }));
}

/** `null` when the dataset has no rows at all. */
export function newestEventFromResponse(payload) {
  const raw = dataRows(payload)[0]?.newest_event;
  if (raw === undefined || raw === null || String(raw).startsWith("0000")) {
    return null;
  }
  // The SQL API returns "YYYY-MM-DD HH:MM:SS" in UTC without a zone marker.
  const normalized = /[zZ]|[+-]\d{2}:\d{2}$/.test(String(raw))
    ? String(raw)
    : `${String(raw).replace(" ", "T")}Z`;
  const parsed = new Date(normalized);
  return Number.isNaN(parsed.getTime()) ? null : parsed;
}

export function formatAge(ms) {
  const minutes = Math.max(0, Math.floor(ms / 60_000));
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  const mins = minutes % 60;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${mins}m`;
  return `${mins}m`;
}

/**
 * Week-over-week trend over complete UTC days only — the partial current day
 * never enters either window. Sums of daily observed active installs, not
 * weekly uniques: the same id active on two days counts twice, deliberately,
 * because a distinct-over-the-week count would look like retention and the id
 * rotation makes retention unmeasurable here.
 */
export function trendSummary(rows, days, now = new Date()) {
  const completeDays = days - 1; // the query window includes the partial today
  const byDay = new Map(rows.map((row) => [row.day, row.active_installs]));
  const todayStart = Date.UTC(
    now.getUTCFullYear(),
    now.getUTCMonth(),
    now.getUTCDate(),
  );
  const sumWindow = (startOffset) => {
    let total = 0;
    for (let i = 0; i < 7; i += 1) {
      const day = new Date(todayStart - (startOffset + i) * DAY_MS)
        .toISOString()
        .slice(0, 10);
      total += byDay.get(day) ?? 0;
    }
    return total;
  };
  const last7 = completeDays >= 7 ? sumWindow(1) : null;
  const previous7 = completeDays >= 14 ? sumWindow(8) : null;
  let changePct = null;
  if (last7 !== null && previous7 !== null && previous7 > 0) {
    changePct = Math.round(((last7 - previous7) / previous7) * 1000) / 10;
  }
  return { last7, previous7, changePct };
}

export function formatReport(rows, { days, now = new Date(), newestEvent = null } = {}) {
  const today = now.toISOString().slice(0, 10);
  const lines = [
    "Codewhale observed active installs (UTC)",
    "day         active installs   sessions started",
  ];
  for (const row of rows) {
    const day = row.day === today ? `${row.day}*` : row.day;
    lines.push(
      `${day.padEnd(12)} ${String(row.active_installs).padStart(15)} ${String(row.sessions_started).padStart(18)}`,
    );
  }
  lines.push("", "* current UTC day is partial");

  const trend = trendSummary(rows, days, now);
  lines.push("", "7-day trend (complete UTC days; sums of daily observed active installs):");
  if (trend.last7 === null) {
    lines.push("  window too small for a 7-day trend — use --days 15 or more");
  } else {
    lines.push(`  last 7 days:      ${trend.last7}`);
    if (trend.previous7 === null) {
      lines.push("  previous 7 days:  not covered — use --days 15 or more");
    } else {
      lines.push(`  previous 7 days:  ${trend.previous7}`);
      const change =
        trend.changePct === null
          ? "n/a (previous window is zero)"
          : `${trend.changePct >= 0 ? "+" : ""}${trend.changePct}%`;
      lines.push(`  change:           ${change} (id rotation: not a retention metric)`);
    }
  }

  if (newestEvent === null) {
    lines.push("", "Freshness: no events ingested in the retention window");
  } else {
    const age = formatAge(now.getTime() - newestEvent.getTime());
    lines.push(
      "",
      `Freshness: newest ingested event ${newestEvent.toISOString()} (${age} ago)`,
    );
  }

  lines.push("", "Caveats:");
  for (const caveat of COVERAGE_CAVEATS) {
    lines.push(`  - ${caveat}`);
  }
  return lines.join("\n");
}

export async function querySql({ accountId, apiToken, sql, fetchImpl = fetch }) {
  const response = await fetchImpl(SQL_ENDPOINT(accountId), {
    method: "POST",
    headers: {
      Authorization: `Bearer ${apiToken}`,
      "content-type": "text/plain; charset=utf-8",
    },
    body: sql,
  });
  if (!response.ok) {
    throw new Error(`Cloudflare SQL request failed (${response.status})`);
  }
  return response.json();
}

export async function main(argv = process.argv.slice(2), env = process.env) {
  const { days, json } = parseArgs(argv);
  const accountId = env.CF_ACCOUNT_ID?.trim();
  const apiToken = env.CF_API_TOKEN?.trim();
  if (!accountId || !apiToken) {
    throw new Error("CF_ACCOUNT_ID and CF_API_TOKEN are required");
  }
  const rows = rowsFromResponse(
    await querySql({ accountId, apiToken, sql: activeInstallsSql(days) }),
  );
  const newestEvent = newestEventFromResponse(
    await querySql({ accountId, apiToken, sql: freshnessSql() }),
  );
  const now = new Date();
  if (json) {
    process.stdout.write(
      `${JSON.stringify(
        {
          metric: "observed_active_installs",
          timezone: "UTC",
          days,
          rows,
          trend: trendSummary(rows, days, now),
          freshness: {
            newest_event: newestEvent === null ? null : newestEvent.toISOString(),
            age_minutes:
              newestEvent === null
                ? null
                : Math.max(0, Math.floor((now.getTime() - newestEvent.getTime()) / 60_000)),
          },
          caveats: COVERAGE_CAVEATS,
        },
        null,
        2,
      )}\n`,
    );
  } else {
    process.stdout.write(`${formatReport(rows, { days, now, newestEvent })}\n`);
  }
}

export function runCli(label) {
  main().catch((error) => {
    process.stderr.write(`${label}: ${error.message}\n`);
    process.exitCode = 1;
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runCli("report:active-installs");
}
