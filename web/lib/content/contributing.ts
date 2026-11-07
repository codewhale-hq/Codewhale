import type { IconName } from "@/components/icon";
import type { LocalizedText } from "./vocabulary";
import { REPO_URL, REPO_ISSUES_URL } from "../i18n/links";

export interface ContributionPath {
  title: LocalizedText;
  body: LocalizedText;
  label: LocalizedText;
  href: string;
  icon: IconName;
}

export const CONTRIBUTION_PATHS: ContributionPath[] = [
  {
    title: { en: "Report a bug", zh: "报告问题" },
    body: {
      en: "Found something that gets in your way? Share your version, setup and the steps to reproduce it, with any logs that are safe to publish.",
      zh: "遇到影响使用的问题？请提供版本、运行环境和复现步骤，并附上可以公开的日志。",
    },
    label: { en: "Report an issue", zh: "提交 issue" },
    href: `${REPO_ISSUES_URL}/new/choose`,
    icon: "alert",
  },
  {
    title: { en: "Improve docs or translations", zh: "改进文档或翻译" },
    body: {
      en: "Fix an unclear instruction, add a useful example or improve a translation. You do not need Rust experience to help someone get started.",
      zh: "修正不清楚的说明、补充实用示例，或改进翻译。无需 Rust 经验，也能帮助别人开始使用。",
    },
    label: { en: "Read the localization guide", zh: "阅读本地化指南" },
    href: `${REPO_URL}/blob/main/docs/LOCALIZATION.md`,
    icon: "book",
  },
  {
    title: { en: "Fix code or build a feature", zh: "修复代码或开发功能" },
    body: {
      en: "Start with a problem you care about. Check existing issues, agree on the scope of larger changes, and send a focused patch with the checks you ran.",
      zh: "从你关心的问题开始。先查看已有 issue，大改动先确认范围，再提交专注于一个问题的补丁并说明已运行的检查。",
    },
    label: { en: "Find an issue to work on", zh: "寻找想解决的 issue" },
    href: REPO_ISSUES_URL,
    icon: "git-pull-request",
  },
  {
    title: { en: "Test on your setup", zh: "在你的环境中验证" },
    body: {
      en: "Try an open pull request or reproduce a reported bug with your platform and model. Share what worked, what failed and the exact version you checked.",
      zh: "用你的平台和模型验证开放的 pull request，或复现已报告的问题。说明成功和失败的部分，以及所验证的准确版本。",
    },
    label: { en: "Explore open pull requests", zh: "查看开放的 pull requests" },
    href: `${REPO_URL}/pulls`,
    icon: "monitor",
  },
];

export const CONTRIBUTION_STEPS = [
  {
    id: "problem",
    title: { en: "Choose one clear problem", zh: "选择一个明确的问题" },
    body: {
      en: "Browse existing issues for something you can reproduce or improve. If the problem is not tracked, open an issue with a reproduction before starting a large change.",
      zh: "浏览已有 issue，寻找你能复现或改进的问题。如果尚未记录，请在开始大改动前提交带复现步骤的 issue。",
    },
    cta: { label: { en: "Browse open issues", zh: "查看开放的 issues" }, href: REPO_ISSUES_URL },
  },
  {
    id: "branch",
    title: { en: "Fork and create a branch", zh: "Fork 并创建分支" },
    body: {
      en: "Clone your fork and use a short branch name such as fix/provider-timeout or docs/fleet-example. Keep unrelated changes in separate pull requests.",
      zh: "克隆你的 fork，并使用简短的分支名，例如 fix/provider-timeout 或 docs/fleet-example。无关修改请拆成不同的 pull request。",
    },
    cta: { label: { en: "Open the repository", zh: "打开仓库" }, href: REPO_URL },
  },
  {
    id: "verify",
    title: { en: "Check the behavior you changed", zh: "验证你修改的行为" },
    body: {
      en: "Run the smallest relevant check, then follow the contributor guide for the area you changed. For docs, verify the commands and examples against the current product.",
      zh: "先运行最小相关检查，再按照贡献指南验证所修改的部分。修正文档时，请对照当前产品核对命令和示例。",
    },
    cta: { label: { en: "Read the contributor guide", zh: "阅读贡献指南" }, href: `${REPO_URL}/blob/main/CONTRIBUTING.md` },
  },
  {
    id: "pull-request",
    title: { en: "Explain the result in the PR", zh: "在 PR 中说明结果" },
    body: {
      en: "Describe the problem, the reason for the change, the checks you ran, and any remaining risk. Link the issue when one exists.",
      zh: "说明问题、修改原因、已运行的检查和剩余风险；如果已有 issue，请在 PR 中关联。",
    },
    cta: { label: { en: "View open pull requests", zh: "查看开放的 pull requests" }, href: `${REPO_URL}/pulls` },
  },
];

export const CONTRIBUTION_REVIEW_NOTES: LocalizedText[] = [
  {
    en: "Keep one problem per pull request so the change is easy to test, review, and credit.",
    zh: "一个 PR 只解决一个问题，便于测试、审查和保留贡献者署名。",
  },
  {
    en: "Add tests for new behavior. For documentation fixes, verify commands, configuration names, and the current version against the repository.",
    zh: "新增行为应有测试；修正文档时，请核对实际命令、配置名和当前版本。",
  },
  {
    en: "Open an issue before changing authentication, credentials, sandbox policy, release plumbing, branding, or global prompts.",
    zh: "修改认证、凭据、沙箱、发布流程、品牌或全局提示词前，请先提交 issue 并确认范围。",
  },
  {
    en: "If a maintainer needs to adapt or harvest a patch, the original contributor remains credited in the commit, changelog, and contributor list.",
    zh: "如果维护者需要整理或摘取补丁，原作者仍会在提交、CHANGELOG 和贡献者名单中获得署名。",
  },
];
