/**
 * getting-started.ts — the canonical new-user path for codewhale.net.
 *
 * Four steps, in order: install → provider connection → first task → verify the answer
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
  id: "install" | "first-session" | "connect-provider" | "verify-answer" | "fleet-workflow";
  title: LocalizedText;
  body: LocalizedText;
  /** Locale-agnostic shell commands shown for the step (may be empty). */
  commands: string[];
  prompt?: LocalizedText;
  optional?: boolean;
  /** Deeper-reading link; href is locale-relative. */
  link: { href: string; label: LocalizedText };
}

export const GETTING_STARTED_STEPS: GuideStep[] = [
  {
    id: "install",
    title: { en: "Install Codewhale", zh: "安装 Codewhale" },
    body: {
      en: "Install the latest release on macOS or Linux, follow any PATH instruction, and check that the version command prints a version. The guide covers Windows and other install options.",
      zh: "在 macOS 或 Linux 上安装最新发布版本，按提示完成 PATH 设置，再确认版本命令能显示版本号。Windows 和其他安装方式见指南。",
    },
    commands: ["curl -fsSL https://codewhale.net/install.sh | sh", "codewhale --version"],
    link: {
      href: "/install",
      label: { en: "Full install guide", zh: "完整安装指南" },
    },
  },
  {
    id: "connect-provider",
    title: { en: "Connect your model", zh: "连接你的模型" },
    body: {
      en: "Connect a model API with your provider key. The command below saves a DeepSeek key without echoing it. To run a model yourself, see the provider guide.",
      zh: "使用你的提供商密钥连接模型 API。下方命令会保存 DeepSeek 密钥，输入时不会回显。若要自己运行模型，请参阅提供商指南。",
    },
    commands: ["codewhale auth set --provider deepseek"],
    link: {
      href: "/docs/auth",
      label: { en: "Connect a provider", zh: "连接模型提供商" },
    },
  },
  {
    id: "first-session",
    title: { en: "Ask about one project file", zh: "询问一个项目文件" },
    body: {
      en: "Open an existing project folder in your terminal, start Codewhale, choose Plan, and paste the prompt below into the message box. The model receives the file content it needs to answer.",
      zh: "在终端中进入一个已有的项目文件夹，启动 Codewhale，选择 Plan，再把下方提示粘贴到消息框中。模型会收到回答所需的文件内容。",
    },
    commands: ["cd /path/to/your/project", "codewhale"],
    prompt: {
      en: "Read one source file in this project and explain what it does. Cite its file path and the lines that support your explanation. Do not edit files or run commands.",
      zh: "阅读这个项目中的一个源文件，说明它的作用，并引用文件路径和支持说明的行号。不要修改文件或运行命令。",
    },
    link: {
      href: "/docs/modes",
      label: { en: "Set modes and approvals", zh: "设置模式与审批" },
    },
  },
  {
    id: "verify-answer",
    title: { en: "Check the answer against the file", zh: "对照文件检查回答" },
    body: {
      en: "Open the cited file yourself and confirm that the cited lines support the explanation to complete this task. Ask about any mismatch, or use the troubleshooting guide if setup fails or no reply arrives.",
      zh: "自行打开引用的文件，确认相关行支持说明后，这个任务就完成了。你可以追问不一致之处；如果设置失败或没有回复，请查看排查指南。",
    },
    commands: [],
    link: { href: "/docs/troubleshooting", label: { en: "Fix a setup or reply problem", zh: "解决设置或回复问题" } },
  },
  {
    id: "fleet-workflow",
    optional: true,
    title: { en: "Use multiple agents when you need them", zh: "需要时再使用多个智能体" },
    body: {
      en: "After your first task, you can optionally save a reusable role and model with /fleet setup. Ask Codewhale to split a larger task across agents, then use /fleet workers to watch the agents in your current session.",
      zh: "完成第一个任务后，你可以按需用 /fleet setup 保存可复用的角色和模型，再请 Codewhale 把较大任务分给多个智能体。用 /fleet workers 查看当前会话的智能体。",
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
      en: "Compare the published release and source-candidate controls, then choose when Codewhale asks for approval.",
      zh: "比较已发布版本与源码候选版本的控制方式，再选择 Codewhale 何时询问审批。",
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
