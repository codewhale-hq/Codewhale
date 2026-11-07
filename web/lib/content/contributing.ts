import type { IconName } from "@/components/icon";
import type { LocalizedText } from "./vocabulary";
import { REPO_URL, REPO_ISSUES_URL } from "../i18n/links";

export interface ContributionPath {
  title: LocalizedText;
  body: LocalizedText;
  href: string;
  icon: IconName;
}

export const CONTRIBUTION_PATHS: ContributionPath[] = [
  {
    title: { en: "Report a bug", zh: "报告问题" },
    body: {
      en: "Include your version, setup and steps to reproduce the problem. Remove secrets from logs.",
      zh: "提供版本、运行环境和复现步骤。移除日志中的密钥。",
    },
    href: `${REPO_ISSUES_URL}/new/choose`,
    icon: "alert",
  },
  {
    title: { en: "Improve docs or translations", zh: "改进文档或翻译" },
    body: {
      en: "Fix an instruction, add an example or improve a translation.",
      zh: "修正说明、补充示例或改进翻译。",
    },
    href: `${REPO_URL}/blob/main/docs/LOCALIZATION.md`,
    icon: "book",
  },
  {
    title: { en: "Fix code or build a feature", zh: "修复代码或开发功能" },
    body: {
      en: "Pick an open issue and send a focused patch.",
      zh: "选择一个开放的 issue，提交范围明确的补丁。",
    },
    href: REPO_ISSUES_URL,
    icon: "git-pull-request",
  },
  {
    title: { en: "Test on your setup", zh: "在你的环境中验证" },
    body: {
      en: "Reproduce a bug or test a pull request on your platform and model.",
      zh: "用你的平台和模型复现问题，或验证 pull request。",
    },
    href: `${REPO_URL}/pulls`,
    icon: "monitor",
  },
];
