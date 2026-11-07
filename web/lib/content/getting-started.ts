/**
 * getting-started.ts — the canonical new-user path for codewhale.net.
 *
 * Four steps, in order: install → provider connection → first task
 * → optional fleet setup. Both the homepage band and the /docs/guide page
 * render from this module, so the path reads identically everywhere.
 *
 * TRUTH CONTRACT:
 *   - Step copy must match documented behavior in docs/GUIDE.md, docs/MODES.md,
 *     docs/PROVIDERS.md, and docs/FLEET.md. The runtime launches without any
 *     API key (recommended working-agreement setup); model replies require a provider —
 *     hosted key or a keyless loopback route. Do not imply otherwise.
 *   - `href` values are locale-relative (no locale prefix); consumers render
 *     `/${locale}${href}` and the tests assert every target route exists.
 *
 * EXTENSION PATH FOR NEW LOCALES: add the locale key to each `{ en, zh }`
 * pair; commands stay locale-agnostic shell.
 */

import type { LocalizedText } from "./vocabulary";

export interface GuideStep {
  id: "install" | "first-session" | "connect-provider" | "fleet-workflow";
  title: LocalizedText;
  body: LocalizedText;
  /** Locale-agnostic shell commands shown for the step (may be empty). */
  commands: string[];
  /** Deeper-reading link; href is locale-relative. */
  link: { href: string; label: LocalizedText };
}

export const GETTING_STARTED_STEPS: GuideStep[] = [
  {
    id: "install",
    title: { en: "Install Codewhale", zh: "安装 Codewhale" },
    body: {
      en: "Install the latest release on macOS or Linux. Follow any PATH instruction the installer prints, then open a folder where you want to work. The guide covers Windows and other install options.",
      zh: "在 macOS 或 Linux 上安装最新发布版本。按安装程序的提示完成 PATH 设置，然后打开你想工作的文件夹。Windows 和其他安装方式见指南。",
    },
    commands: ["curl -fsSL https://codewhale.net/install.sh | sh"],
    link: {
      href: "/install",
      label: { en: "Full install guide", zh: "完整安装指南" },
    },
  },
  {
    id: "connect-provider",
    title: { en: "Connect your model", zh: "连接你的模型" },
    body: {
      en: "Use a model API you already have, or connect local or self-hosted inference. The command below saves a DeepSeek key; local Ollama models do not need an API key.",
      zh: "使用你已有的模型 API，或连接本地、自托管推理服务。下方命令用于保存 DeepSeek 密钥；本地 Ollama 模型无需 API 密钥。",
    },
    commands: ["codewhale auth set --provider deepseek"],
    link: {
      href: "/docs/auth",
      label: { en: "Connect a provider", zh: "连接模型提供商" },
    },
  },
  {
    id: "first-session",
    title: { en: "Describe your first task", zh: "描述第一个任务" },
    body: {
      en: "Start Codewhale and describe the result: a small app, a script that saves you time, or a report from your data. Choose its permissions, review what it does and steer it as you go.",
      zh: "启动 Codewhale，说出你想要的结果：一个小应用、节省时间的脚本，或一份根据数据生成的报告。设定权限、查看它的工作，并随时调整方向。",
    },
    commands: ["codewhale"],
    link: {
      href: "/docs/modes",
      label: { en: "Set modes and approvals", zh: "设置模式与审批" },
    },
  },
  {
    id: "fleet-workflow",
    title: { en: "Use multiple agents", zh: "使用多个智能体" },
    body: {
      en: "Save a reusable role and model with /fleet setup. Ask Codewhale to split a larger task across agents, then use /fleet workers to watch the agents in your current session.",
      zh: "用 /fleet setup 保存可复用的角色和模型，再请 Codewhale 把较大任务分给多个智能体。用 /fleet workers 查看当前会话的智能体。",
    },
    commands: ["/fleet setup", "/fleet workers"],
    link: {
      href: "/docs/fleet",
      label: { en: "Set up a Fleet", zh: "配置 Fleet" },
    },
  },
];

/**
 * Where to go after the path — discovery links rendered at the end of the
 * /docs/guide page. Hooks are first-class here on purpose: they are the
 * supported extension point a new user should find without digging.
 */
export const GUIDE_NEXT_LINKS: { href: string; label: LocalizedText; note: LocalizedText }[] = [
  {
    href: "/docs/review",
    label: { en: "Review what changed", zh: "查看改动" },
    note: {
      en: "Review recorded edits, restore available snapshots and get a code review before pushing.",
      zh: "查看已记录的修改、恢复可用快照，并在推送前做代码审查。",
    },
  },
  {
    href: "/docs/modes",
    label: { en: "Set modes and approvals", zh: "设置模式与审批" },
    note: {
      en: "Plan, Work, or Operate for the kind of work; Ask, Auto-Review, or Full Access for when it stops to ask you.",
      zh: "用 Plan、Work、Operate 选择工作类型，用 Ask、Auto-Review、Full Access 决定它什么时候停下来问你。",
    },
  },
  {
    href: "/docs/mcp",
    label: { en: "Connect your tools", zh: "连接你常用的工具" },
    note: {
      en: "Connect an email tool through MCP, then ask Codewhale to turn the messages it can access into an action list.",
      zh: "通过 MCP 连接邮件工具，再让 Codewhale 把可访问的邮件整理成行动清单。",
    },
  },
  {
    href: "/docs/hooks",
    label: { en: "Run commands on events", zh: "在事件发生时运行命令" },
    note: {
      en: "Run your own scripts when a session starts, before a tool call, or when a turn ends. Use them to add context, enforce a rule, or send a notification.",
      zh: "在会话开始、工具调用之前或回合结束时运行你自己的脚本。可以用它们补充上下文、执行规则或发送通知。",
    },
  },
];
