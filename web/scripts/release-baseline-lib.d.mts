export interface ReleaseBaselineJson {
  format: string;
  tag: string;
  version: string;
  features: { id: string; name: string; summary: string; status: string; since: string | null }[];
  providers: { id: string; label: string; env: string }[];
  models: { id: string; provider: string | null; contextWindow: number | null; maxOutput: number | null; reasoning: boolean; addedAt: string | null }[];
  toolCount: number | null;
}
export const BASELINE_PATH: string;
export function tagResolves(tag: string): boolean;
export function deriveReleaseBaseline(release: { tag: string; version: string } | null): ReleaseBaselineJson | null;
export function readCommittedBaseline(): ReleaseBaselineJson | null;
export function serializeBaseline(baseline: ReleaseBaselineJson): string;
export function refreshReleaseBaseline(release: { tag: string; version: string } | null): ReleaseBaselineJson | null;
