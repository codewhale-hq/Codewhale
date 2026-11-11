export interface FunnelReport {
  report: string;
  source: string;
  synthetic: boolean;
  freshness: { newest_observed_received_at: string | null; age_hours: number | null };
  website: { totals: Record<string, number>; days: Array<{ day: string; counters: Record<string, number> }> };
  native: {
    observed_started_ids: number;
    observed_initialized_ids: number;
    observed_completed_ids: number;
    excluded_ci_ids: number;
    incomplete_followup_ids: number;
    seven_day_initialization_cohort: { denominator: number; numerator: number; fraction: number | null; unavailable_reason: string | null; sampled: boolean };
  };
  caveats: string[];
}
export const CAVEATS: readonly string[];
export function funnelSql(from: string, to: string): { website: string; native: string };
export function buildFunnelReport(input: unknown, options?: { now?: Date }): FunnelReport;
export function parseArgs(argv: string[]): { input: string | null; from: string | null; to: string | null; sql: boolean };
export function main(argv?: string[], env?: NodeJS.ProcessEnv): Promise<unknown>;
