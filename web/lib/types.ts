export type FeedKind = "issue" | "pull" | "release" | "discussion";

export interface FeedItem {
  kind: FeedKind;
  /** Issue / pull number. Releases have none and carry `0` — read `tag`. */
  number: number;
  title: string;
  url: string;
  state: "open" | "closed" | "merged" | "draft" | "published";
  author: string;
  authorAvatar: string;
  createdAt: string; // ISO
  updatedAt: string; // ISO
  comments: number;
  labels: { name: string; color: string }[];
  body?: string;

  /** Release tag (`kind: "release"` only), e.g. "v0.9.4". */
  tag?: string;

  /**
   * When the item entered the state it is in — `merged_at`, `closed_at`,
   * `published_at`, or `created_at`. `updatedAt` still orders the feed, but a
   * surface that prints a verb ("merged", "opened") must date that verb, not
   * the last comment on the thread: an issue opened in March and commented on
   * today is not "opened, just now".
   */
  eventAt?: string;

  /**
   * GitHub's own `author_association === "FIRST_TIME_CONTRIBUTOR"` verdict,
   * copied verbatim from the list payload — never inferred by us, and never
   * derived from the size of the window we happened to fetch. GitHub
   * recomputes the association as the author gains commits, so this marks a
   * newcomer's contribution while it is still new, and stops on its own.
   */
  firstTimeContributor?: boolean;
}

export interface RepoStats {
  stars: number | null;
  forks: number | null;
  openIssues: number | null;
  openPulls: number | null;
  contributors: number | null;
  latestRelease?: { tag: string; publishedAt: string; url: string };
  fetchedAt: string | null;
  source: "live" | "snapshot" | "unknown";
}
