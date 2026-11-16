import { startOfUtcDay, statChunks, utcDay } from "./queries";
import type { D1Like, SqlRunner, StatRow } from "./types";

const DAY_MS = 86_400_000;
const RETENTION_DAYS = 90;
const CHUNK = 50;
const KEPT_DIMS = new Set(["all", "funnel"]);

export async function rollupDaily(runner: SqlRunner, db: D1Like, now: Date = new Date()): Promise<{ from: string; to: string; rows: number }> {
  const today = startOfUtcDay(now);
  const last = await db.prepare("SELECT max(day) AS day FROM daily_totals").first<{ day: string | null }>();
  const floor = today.getTime() - RETENTION_DAYS * DAY_MS;
  const resumeFrom = last?.day ? Date.parse(`${last.day}T00:00:00Z`) - DAY_MS : floor;
  const from = new Date(Math.max(floor, Math.min(resumeFrom, today.getTime() - 2 * DAY_MS)));
  let written = 0;
  for await (const chunk of statChunks(runner, from, today)) {
    const rows = chunk.filter((row) => KEPT_DIMS.has(row.dim));
    if (rows.length === 0) continue;
    await writeRows(db, rows);
    written += rows.length;
  }
  return { from: utcDay(from), to: utcDay(today), rows: written };
}

async function writeRows(db: D1Like, rows: StatRow[]): Promise<void> {
  const statement = "INSERT OR REPLACE INTO daily_totals (day, metric, dim, key, value) VALUES (?, ?, ?, ?, ?)";
  for (let start = 0; start < rows.length; start += CHUNK) {
    await db.batch(
      rows.slice(start, start + CHUNK).map((row) => db.prepare(statement).bind(row.day, row.metric, row.dim, row.key, row.value)),
    );
  }
}
