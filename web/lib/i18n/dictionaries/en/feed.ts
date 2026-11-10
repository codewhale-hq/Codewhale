import type { FeedDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/feed/page.tsx`. Copy moved
 * verbatim from the page's `isZh` ternaries. The feed items come from GitHub
 * and are rendered as fetched.
 */
export const feed: FeedDict = {
  metaTitle: "Activity · Codewhale",
  metaDescription:
    "Live feed of issues, pull requests, and releases mirrored from the codewhale-hq/CodeWhale GitHub repo.",
  title: "Activity",
  titleAside: "动态",
  titleAsideLang: "zh",
  lede: "Follow issues and pull requests from {repo}, mirrored here and refreshed every ten minutes. Select any item to open it on GitHub.",
  pulls: "Pull requests",
  issues: "Issues",
  shownCount: "{count} shown",
  openIssue: "Open an issue",
  openPull: "Open a pull request",
  startDiscussion: "Ask the community a question",
};
