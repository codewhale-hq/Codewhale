export interface SnapshotPull {
  number: number;
  title: string;
  url: string;
  login: string;
  avatar: string;
  mergedAt: string;
  firstTime: boolean;
}
export interface SnapshotPerson {
  login: string;
  avatar: string;
}
export interface SnapshotMilestone {
  title: string;
  url: string;
  open: number;
  closed: number;
}
export interface SnapshotReleaseCredit {
  tag: string;
  url: string;
  publishedAt: string;
  previousPublishedAt: string;
  authors: SnapshotPerson[];
}
export const ACTIVITY_WINDOW_DAYS: number;
export const RELEASES_CREDITED: number;
export const SEARCH_RESULT_CAP: number;
export const READ_DEADLINE_MS: number;
export const READ_MAX_BYTES: number;
export function readCappedJson(
  url: string,
  options?: { headers?: Record<string, string>; fetchImpl?: typeof fetch; deadlineMs?: number; maxBytes?: number },
): Promise<unknown>;
export function windowStart(readAtIso: string, days?: number): string;
export function mergedPullFromSearchItem(item: unknown): SnapshotPull | null;
export function authorsBetween(pulls: SnapshotPull[], afterIso: string | null, untilIso: string): SnapshotPerson[];
export function versionedMilestones(rows: unknown): SnapshotMilestone[];
type ReleaseRow = { tag_name: string; html_url: string; published_at: string; draft?: boolean; prerelease?: boolean };
export function creditedPairs(releases: ReleaseRow[]): Array<{ release: ReleaseRow; previous: ReleaseRow }>;
export function releaseCredits(
  pairs: Array<{ release: ReleaseRow; previous: ReleaseRow }>,
  pullsFor: (release: ReleaseRow, previous: ReleaseRow) => SnapshotPull[],
): SnapshotReleaseCredit[];
