import type { FeedDict } from "../types";

/** Chinese dictionary for `app/[locale]/feed/page.tsx`. */
export const feed: FeedDict = {
  metaTitle: "动态 · Codewhale",
  metaDescription: "来自 codewhale-hq/CodeWhale GitHub 仓库的议题、拉取请求和发布的实时动态。",
  title: "动态",
  titleAside: "Activity",
  titleAsideLang: "en",
  lede: "来自 {repo} 的议题与拉取请求镜像，每十分钟刷新一次。点击任意条目跳转至 GitHub。",
  pulls: "拉取请求",
  issues: "议题",
  shownCount: "{count} 条",
  openIssue: "提交议题",
  openPull: "提交拉取请求",
  startDiscussion: "向社区提问",
};
