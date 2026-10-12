import type { DocsComputersDict } from "../types";

/** 「把任务发送到云端」页的简体中文词典；与 `en/docs-computers.ts` 逐段对应。 */
export const docsComputers: DocsComputersDict = {
  metaTitle: "把任务发送到云端 · Codewhale 文档",
  metaDescription:
    "把编码任务交给 Codewhale 云端智能体，通过你的账户运行——先报价，只有你同意后才会开始。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "把任务发送到云端",
  lede:
    "云端智能体会把任务从你的机器上接走：它在云端算力上工作，你则可以继续在本地工作。在你看过报价并同意之前，不会启动任何东西。",
  sections: [
    {
      id: "status",
      title: "了解目前的完成度",
      blocks: [
        {
          note: "云端智能体目前是预览版。任务通过你已登录的 Codewhale 账户运行，因此下文的报价和计算位置确认适用于每个任务。",
        },
      ],
    },
    {
      id: "before",
      title: "开始之前",
      blocks: [
        {
          list: [
            "用 `codewhale login` 登录你的 Codewhale 账户。派发任务始终通过已登录的账户运行。",
          ],
        },
      ],
    },
    {
      id: "send",
      title: "先报价、再确认、后运行",
      blocks: [
        {
          code: `codewhale dispatch "fix the flaky login test and open a PR"`,
          lang: "终端",
        },
        {
          p: "Codewhale 会显示你账户的任务时长报价，并请你确认仓库代码和你附加的文件将在欧盟计算资源上运行。在你同意之前不会启动任何任务。脚本中可传入 `--yes --confirm-eu-compute` 一次性回答这两个问题。",
        },
        {
          p: "用 `--seconds` 设置任务时长，用 `--agent` 选择智能体，用 `--context-file` 或 `--file` 附加上下文。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "连接模型提供商",
      note: "登录 Codewhale 账户并管理密钥。",
    },
    {
      href: "/docs/review",
      label: "查看改动",
      note: "合并之前，先审查智能体提交的拉取请求。",
    },
    {
      href: "/docs/fleet",
      label: "运行工作流",
      note: "改为在你自己的机器上运行更长的多步骤工作。",
    },
  ],
  sourceNote: "来源文档：docs/GUIDE.md、docs/CODEWHALE_AGENT.md · 修改时同步更新 docs-map.ts。",
};
