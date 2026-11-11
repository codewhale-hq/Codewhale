export interface ReleaseDocSource { path: string; sha256: string; content: string }
export interface ReleaseDocTopic { id: string; slug: string; sources: ReleaseDocSource[]; missing: string[] }
export interface ReleaseDocsJson {
  format: string;
  tag: string;
  version: string;
  topics: ReleaseDocTopic[];
  omitted: { id: string; slug: string; missing: string[] }[];
}
type Release = { tag: string; version: string };
export const RELEASE_DOCS_PATH: string;
export function tagResolves(tag: string): boolean;
export function deriveReleaseDocs(webDir: string, release: Release | null): ReleaseDocsJson | null;
export function serializeReleaseDocs(snapshot: ReleaseDocsJson): string;
export function readCommittedReleaseDocs(): ReleaseDocsJson | null;
export function refreshReleaseDocs(webDir: string, release: Release | null): ReleaseDocsJson | null;
export function checkReleaseDocs(snapshot: ReleaseDocsJson | null, release: Release | null, registry: { slug: string }[], derived: ReleaseDocsJson | null, index?: unknown): string[];
export function releaseDocsIndex(snapshot: ReleaseDocsJson): { tag: string; slugs: string[]; omitted: string[] };
export function readCommittedReleaseDocsIndex(): { tag: string; slugs: string[]; omitted: string[] } | null;
export function currentRelease(): Release | null;
