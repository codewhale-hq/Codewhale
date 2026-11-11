import type { ContributeDict } from "../types";

/** Chinese dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "参与贡献 · Codewhale",
  metaDescription: "所有反馈都在帮助 Codewhale，不论是否包含代码。看看 bug、想法、翻译、提供商或修复应该提交到哪里。",
  kicker: "一起构建 Codewhale",
  title: "一起构建 Codewhale",
  lede: "所有反馈都有帮助，不论是否包含代码。一个 bug、一个想法，或一个看起来不对劲的界面，都足以开始。",
  fileIssue: "提交 issue",
  browsePulls: "查看 pull requests",
  fullGuide: "贡献指南",
  pathsTitle: "选择适合你的第一步",
  workflowTitle: "提交便于他人审查的修改",
  reviewTitle: "说明修改内容和验证方式",
  reviewScope: "把问题、修改理由、测试证据和剩余风险交给审查者。范围越集中，反馈越清楚。",
  devTitle: "构建并验证代码修改",
  devScope: "修改 Rust 代码时，先构建相关代码并运行针对性的检查。按照贡献指南，补充你的修改所需的更完整检查。",
  mergedAccountsCount: {
    other: "过去 30 天内有 {n} 个 GitHub 账号的 pull request 被合并。",
  },
  mergedPeopleCount: {
    other: "过去 30 天内有 {n} 人的 pull request 被合并。",
  },
};
