import type { Env, SqlRunner } from "./types";

export const ROW_LIMIT = 10_001;

const DATASET = "codewhale_telemetry";

export function tableName(source: SqlRunner["source"]): string {
  return source === "binding" ? `events.analyticsEngine."${DATASET}"` : DATASET;
}

export function createRunner(env: Env, fetchImpl: typeof fetch = fetch): SqlRunner | null {
  if (env.ANALYTICS_SQL) {
    const binding = env.ANALYTICS_SQL;
    return {
      source: "binding",
      async run(sql) {
        const result = await binding.query({ query: sql });
        return rowsOf(result);
      },
    };
  }
  const accountId = env.CF_ACCOUNT_ID?.trim();
  const apiToken = env.CF_API_TOKEN?.trim();
  if (accountId && apiToken) {
    return {
      source: "rest",
      async run(sql) {
        const response = await fetchImpl(
          `https://api.cloudflare.com/client/v4/accounts/${encodeURIComponent(accountId)}/analytics_engine/sql`,
          {
            method: "POST",
            headers: { Authorization: `Bearer ${apiToken}`, "content-type": "text/plain; charset=utf-8" },
            body: `${sql} FORMAT JSON`,
          },
        );
        if (!response.ok) throw new Error(`Cloudflare SQL request failed (${response.status})`);
        return rowsOf(await response.json());
      },
    };
  }
  return null;
}

function rowsOf(payload: unknown): unknown[] {
  const rows = Array.isArray(payload) ? payload : (payload as { data?: unknown } | null)?.data;
  if (!Array.isArray(rows)) throw new Error("SQL response did not contain a data array");
  if (rows.length >= ROW_LIMIT) throw new Error("SQL response reached the row limit; narrow the window");
  return rows;
}
