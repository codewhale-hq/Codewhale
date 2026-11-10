export interface ContributionReport {
  report: string;
  synthetic: boolean;
  totals: { first_public_issue_authors: number; first_merged_pr_authors: number };
  input_counts: { public_issues: number; merged_prs: number; excluded_bot_or_deleted_records: number };
  days: Array<{ day: string; first_public_issue_authors: number; first_merged_pr_authors: number }>;
  caveats: string[];
}
export function contributionCohorts(snapshot: unknown, options?: { from?: string; to?: string }): ContributionReport;
export function exportPublicHistory(options?: { get?: (endpoint: string) => Promise<unknown>; maxPages?: number }): Promise<unknown>;
export function main(argv?: string[]): Promise<ContributionReport>;
