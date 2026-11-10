export interface ActiveInstallsRow {
  day: string;
  active_installs: number;
  sessions_started: number;
}

export interface ReportArgs {
  days: number;
  json: boolean;
}

export interface TrendSummary {
  last7: number | null;
  previous7: number | null;
  changePct: number | null;
}

export interface FormatReportOptions {
  days: number;
  now?: Date;
  newestEvent?: Date | null;
}

export const COVERAGE_CAVEATS: readonly string[];
export function parseArgs(argv: string[]): ReportArgs;
export function activeInstallsSql(days: number): string;
export function freshnessSql(): string;
export function rowsFromResponse(payload: unknown): ActiveInstallsRow[];
export function newestEventFromResponse(payload: unknown): Date | null;
export function formatAge(ms: number): string;
export function trendSummary(
  rows: ActiveInstallsRow[],
  days: number,
  now?: Date,
): TrendSummary;
export function formatReport(
  rows: ActiveInstallsRow[],
  options?: FormatReportOptions,
): string;
export function main(argv?: string[], env?: NodeJS.ProcessEnv): Promise<void>;
export function runCli(label: string): void;

export function querySql(options: { accountId: string; apiToken: string; sql: string; fetchImpl?: typeof fetch }): Promise<unknown>;
