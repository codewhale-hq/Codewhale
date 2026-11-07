import type { ChangelogDict } from "../types";

/**
 * Simplified-Chinese dictionary for `app/[locale]/changelog/page.tsx`.
 * Version numbers, tags, dates, and release-note text are content, not copy.
 */
export const changelog: ChangelogDict = {
  metaTitle: "更新日志 · Codewhale",
  metaDescription:
    "Codewhale 与 Computer Use 插件的发布说明、已发布版本和后续改动。",
  kicker: "发布记录",
  title: "改了什么，在哪个版本。",
  lead:
    "正式发布与即将推出的改动。",
  publishedLabel: "最新已发布版本",
  publishedValue: "{tag} · 发布于 {date}",
  candidateLabel: "源码候选版",
  candidateValue: "{version} · 未发布",
  candidateMatches: "{version} · 与已发布版本一致",
  releasesLink: "GitHub Releases ↗",
  unreleasedHeading: "未发布",
  unreleasedNote: "自上一个标签以来合并到主分支的改动。它们属于源码候选版，不属于任何已发布的包。",
  compareLink: "在 GitHub 上比较 ↗",
  releasePageLink: "发布页 ↗",
  moreEntries: "显示 {shown} / {total} 条",
  fullNotes: "完整说明见 CHANGELOG.md ↗",
  releaseNotesLink: "{version} 完整说明 ↗",
  emptyTitle: "没有解析出发布说明",
  emptyBody: "构建时没有找到可解析的 CHANGELOG.md。GitHub 的发布列表仍然是权威记录。",
  versionMetaTitle: "Codewhale {version} 发布说明",
  versionMetaDescription: "Codewhale {version} 的全部改动，按章节列出，取自仓库中的 CHANGELOG.md。",
  versionKicker: "发布说明",
  versionLead: "发布于 {date}。以下是该版本的全部条目，取自仓库中的 CHANGELOG.md，条目原文为英文。",
  versionLeadUndated: "以下是该版本的全部条目，取自仓库中的 CHANGELOG.md，条目原文为英文。",
  allReleasesLink: "全部版本",
};
