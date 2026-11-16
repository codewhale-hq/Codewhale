import type { D1Like, D1Statement, SqlRunner } from "../src/types";

export const INSTALL_A = "3f2b1c4e-5a6d-4e7f-8a9b-0c1d2e3f4a5b";
export const INSTALL_B = "9a8b7c6d-5e4f-4a3b-9c2d-1e0f9a8b7c6d";

export function fakeRunner(respond: (sql: string) => unknown[], source: SqlRunner["source"] = "binding") {
  const seen: string[] = [];
  const runner: SqlRunner = {
    source,
    async run(sql) {
      seen.push(sql);
      return respond(sql);
    },
  };
  return { runner, seen };
}

export function answer(sql: string): unknown[] {
  if (sql.includes("blob1 = 'session_start'")) {
    const key = sql.includes("blob6 AS key") ? "0.10.3" : sql.includes("blob2 AS key") ? "tui" : sql.includes("blob3 AS key") ? "macos" : "all";
    return [
      { day: "2026-11-10", key, installs: "4", sessions: "10", index1: INSTALL_A, install_id: INSTALL_A },
      { day: "2026-11-11", key, installs: "5", sessions: "22" },
    ];
  }
  if (sql.includes("blob1 = 'session_end'")) return [{ day: "2026-11-10", surface: "tui", turns: "30", tool_calls: "90" }, { day: "2026-11-10", surface: "exec", turns: "5", tool_calls: "1" }];
  if (sql.includes("blob1 = 'tool_usage'")) return [{ day: "2026-11-10", tool: "bash", calls: "70", errors: "3" }];
  if (sql.includes("blob1 = 'model_usage' AND double1")) return [{ day: "2026-11-10", installs: "2" }];
  if (sql.includes("blob1 = 'model_usage'")) return [{ day: "2026-11-10", provider: "deepseek", model: "other", turns_ok: "8", turns_failed: "1", input_tokens: "1000", output_tokens: "200" }, { day: "2026-11-10", provider: "deepseek", model: "deepseek-v4", turns_ok: "2", turns_failed: "0", input_tokens: "50", output_tokens: "5" }];
  if (sql.includes("'install_or_upgrade'")) return [{ day: "2026-11-10", installs: "3" }];
  if (sql.includes("'funnel_usage'")) return [
    { day: "2026-11-10", counters: JSON.stringify({ home_view: 4, install_view: 2 }), weight: "2" },
    { day: "2026-11-10", counters: JSON.stringify({ home_view: 1, install_view: 0 }), weight: "1" },
  ];
  return [];
}

export class FakeD1 implements D1Like {
  rows = new Map<string, { day: string; metric: string; dim: string; key: string; value: number }>();
  prepare(sql: string): D1Statement {
    const db = this;
    let args: unknown[] = [];
    const stmt: D1Statement & { run(): void } = {
      bind(...values: unknown[]) {
        args = values;
        return stmt;
      },
      async all() {
        if (sql.includes("FROM daily_totals")) {
          const [from, ...metrics] = args as string[];
          return { results: [...db.rows.values()].filter((r) => r.dim === "all" && r.day >= from && metrics.includes(r.metric)) as never[] };
        }
        return { results: [] };
      },
      async first() {
        const days = [...db.rows.values()].map((r) => r.day).sort();
        return { day: days.length ? days[days.length - 1] : null } as never;
      },
      run() {
        const [day, metric, dim, key, value] = args as [string, string, string, string, number];
        db.rows.set(`${day}|${metric}|${dim}|${key}`, { day, metric, dim, key, value });
      },
    };
    return stmt;
  }
  async batch(statements: D1Statement[]) {
    for (const s of statements) (s as unknown as { run(): void }).run();
    return [];
  }
}
