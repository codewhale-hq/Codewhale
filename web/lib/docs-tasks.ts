/**
 * docs-tasks.ts — the task-based index of codewhale.net documentation.
 *
 * `docs-map.ts` answers "what topics exist"; this registry answers "I am
 * trying to do X — where do I go". Every task points at a first-party route
 * (locale-relative, so `/${locale}${href}` always exists) and names the
 * topic it belongs to, so the hub can search tasks and topics together and
 * `docs-tasks.test.ts` can prove every target resolves.
 *
 * TRUTH CONTRACT: a task may only describe behaviour the target page (and
 * its repository source document) actually documents. Keep the verbs
 * concrete and keep the list short enough to read in one screen.
 *
 * Labels are `{ en, zh }` pairs like docs-map.ts; other locales fall back to
 * English through `pickText`.
 */
import type { LocalizedText } from "./content/vocabulary";
import { DOC_TOPICS, type DocTopic } from "./docs-map";

export interface DocTask {
  id: string;
  label: LocalizedText;
  description: LocalizedText;
  /** Locale-relative route, e.g. "/install" or "/docs/modes". */
  href: string;
  /** Owning docs-map topic id, for grouping and source attribution. */
  topicId: DocTopic["id"];
  /** Extra search words, both languages, lowercase not required. */
  keywords: LocalizedText;
}

export const DOC_TASKS: DocTask[] = [
  {
    id: "install",
    label: { en: "Install Codewhale", zh: "安装 Codewhale" },
    description: {
      en: "One command on macOS or Linux; npm, Cargo, release binaries, and the Homebrew tap on Linux also work.",
      zh: "在 macOS 或 Linux 上只需一条命令；也可以用 npm、Cargo、发布版二进制文件，或在 Linux 上用 Homebrew tap 安装。",
    },
    href: "/install",
    topicId: "install",
    keywords: { en: "download setup brew npm cargo docker termux binary windows", zh: "下载 安装 二进制 镜像" },
  },
  {
    id: "first-task",
    label: { en: "Start your first task", zh: "开始第一个任务" },
    description: {
      en: "Open Codewhale in a project, look around in Plan, then let it edit in Work.",
      zh: "在项目中打开 Codewhale，先在 Plan 里了解项目，再让它在 Work 里动手修改。",
    },
    href: "/docs/guide",
    topicId: "guide",
    keywords: { en: "getting started quickstart tutorial first run", zh: "入门 快速开始 教程 首次运行" },
  },
  {
    id: "connect-provider",
    label: { en: "Connect a provider", zh: "连接模型提供商" },
    description: {
      en: "Save a key for DeepSeek, OpenAI, Anthropic, OpenRouter, or another provider — or run a local model with no key.",
      zh: "为 DeepSeek、OpenAI、Anthropic、OpenRouter 等提供商保存密钥，或者不用密钥运行本地模型。",
    },
    href: "/docs/auth",
    topicId: "auth",
    keywords: { en: "api key byok auth set login account ollama vllm sglang local", zh: "密钥 登录 账户 本地模型" },
  },
  {
    id: "choose-model",
    label: { en: "Choose or switch models", zh: "选择或切换模型" },
    description: {
      en: "Supported providers, switching mid-session, and local runners.",
      zh: "支持的提供商、在会话中切换模型，以及本地运行器。",
    },
    href: "/models",
    topicId: "providers",
    keywords: { en: "model switch provider openai-compatible", zh: "模型 切换 提供商" },
  },
  {
    id: "set-approvals",
    label: { en: "Decide what runs without asking", zh: "决定哪些操作无需询问" },
    description: {
      en: "Plan, Work, or Operate; Ask, Auto-Review, or Full Access; and how to answer a prompt.",
      zh: "Plan、Work 或 Operate；Ask、Auto-Review 或 Full Access；以及如何回应审批提示。",
    },
    href: "/docs/modes",
    topicId: "modes",
    keywords: { en: "mode tab shift+tab ask auto-review full access permission", zh: "模式 审批 权限" },
  },
  {
    id: "review-changes",
    label: { en: "Review what changed", zh: "查看改动" },
    description: {
      en: "See every edit, roll files back to an earlier turn, and get a code review before you push.",
      zh: "查看每一处修改，把文件回滚到之前的回合，并在推送前做代码审查。",
    },
    href: "/docs/review",
    topicId: "review",
    keywords: { en: "diff undo restore rollback snapshot code review", zh: "差异 撤销 回滚 快照 审查" },
  },
  {
    id: "receipts",
    label: { en: "Keep a receipt of a review", zh: "为审查保留收据" },
    description: {
      en: "Write a review receipt and check it before you push.",
      zh: "生成审查收据，并在推送前核对它。",
    },
    href: "/docs/review",
    topicId: "review",
    keywords: { en: "receipt write-receipt check-receipt pre-push export", zh: "收据 推送 导出" },
  },
  {
    id: "watch-sessions",
    label: { en: "Watch each session at a glance", zh: "一眼查看每个会话" },
    description: {
      en: "Turn on pet mode in each terminal to see which session is working, waiting for you, done, or failed.",
      zh: "在每个终端开启 Pet 模式，看清哪个会话在工作、在等你、已完成或失败。",
    },
    href: "/docs/pet",
    topicId: "pet",
    keywords: { en: "pet whale status glance terminals sessions inspect f5", zh: "鲸鱼 状态 终端 会话 查看" },
  },
  {
    id: "run-workflow",
    label: { en: "Run a workflow", zh: "运行 Workflow" },
    description: {
      en: "Write a repeatable Workflow, run it as a Lane, and watch or stop it from any terminal.",
      zh: "编写可重复的 Workflow，把它作为 Lane 运行，并在任何终端查看或停止它。",
    },
    href: "/docs/fleet",
    topicId: "fleet",
    keywords: { en: "fleet workflow lane operate durable tasks.json", zh: "编排 持久 工作流" },
  },
  {
    id: "parallel-agents",
    label: { en: "Run agents in parallel", zh: "并行运行 Agent" },
    description: {
      en: "Roles, separate worktrees for parallel edits, and how to watch sub-agents.",
      zh: "角色、为并行修改准备的独立工作树，以及如何查看子 Agent。",
    },
    href: "/docs/subagents",
    topicId: "subagents",
    keywords: { en: "agent explore reviewer implement worktree concurrency subagents", zh: "子代理 并行 角色 工作树" },
  },
  {
    id: "mcp-server",
    label: { en: "Connect tools with MCP", zh: "用 MCP 连接工具" },
    description: {
      en: "Add a local or remote MCP server, sign in with OAuth, or serve Codewhale over MCP.",
      zh: "添加本地或远程 MCP 服务器、用 OAuth 登录，或把 Codewhale 作为 MCP 服务器提供。",
    },
    href: "/docs/mcp",
    topicId: "mcp",
    keywords: { en: "model context protocol tools stdio http oauth", zh: "工具 协议 服务器" },
  },
  {
    id: "code-mode",
    label: { en: "Try code mode", zh: "试用代码模式" },
    description: {
      en: "Let the model compose several tool calls — MCP and plugin tools included — in one short program, under the same approvals as direct calls. Experimental, on by default.",
      zh: "让模型在一段简短的程序中组合多个工具调用（包括 MCP 和插件工具），审批流程与直接调用相同。实验性功能，默认开启。",
    },
    href: "/docs/mcp",
    topicId: "mcp",
    keywords: { en: "code_mode execute_tools javascript experimental", zh: "代码模式 实验" },
  },
  {
    id: "hooks",
    label: { en: "Run commands on events", zh: "在事件发生时运行命令" },
    description: {
      en: "Block a risky command, add context, or get notified when Codewhale waits for you.",
      zh: "拦下危险命令、补充上下文，或在 Codewhale 等你回应时收到提醒。",
    },
    href: "/docs/hooks",
    topicId: "hooks",
    keywords: { en: "hook lifecycle tool_call_before session_start event", zh: "钩子 生命周期 事件" },
  },
  {
    id: "sandbox",
    label: { en: "Limit what commands can touch", zh: "限制命令的访问范围" },
    description: {
      en: "See which OS sandbox your platform has and turn on bubblewrap on Linux.",
      zh: "了解你的平台有哪种操作系统沙箱，并在 Linux 上开启 bubblewrap。",
    },
    href: "/docs/sandbox",
    topicId: "sandbox",
    keywords: { en: "sandbox seatbelt bwrap bubblewrap workspace-write", zh: "沙箱 隔离" },
  },
  {
    id: "automate",
    label: { en: "Automate with the Runtime API", zh: "用 Runtime API 自动化" },
    description: {
      en: "Run one-shot jobs in CI, or drive threads and approvals over the local HTTP API.",
      zh: "在 CI 中运行一次性任务，或通过本地 HTTP API 驱动线程和审批。",
    },
    href: "/docs/runtime-api",
    topicId: "runtime-api",
    keywords: { en: "http api exec ci acp integration sse", zh: "接口 集成 脚本" },
  },
  {
    id: "browser-client",
    label: { en: "Open the browser client", zh: "打开浏览器客户端" },
    description: {
      en: "Work in a local browser tab, or continue a session from the web app with /rc.",
      zh: "在本机浏览器标签页中工作，或用 /rc 在网页应用中继续会话。",
    },
    href: "/docs/web",
    topicId: "web",
    keywords: { en: "web ui localhost loopback remote control rc", zh: "网页 客户端 本机 远程" },
  },
  {
    id: "cloud-computer",
    label: { en: "Send a task to the cloud", zh: "把任务发送到云端" },
    description: {
      en: "Preview: quote and run a cloud agent through your account.",
      zh: "预览版：通过你的账户为云端 Agent 报价并运行。",
    },
    href: "/docs/computers",
    topicId: "computers",
    keywords: { en: "dispatch cloud agent quote account", zh: "云端 派发 远程" },
  },
  {
    id: "troubleshoot",
    label: { en: "Fix a problem", zh: "排查问题" },
    description: {
      en: "Diagnose in one command, then fix install, key, network, and stuck-turn problems.",
      zh: "用一条命令诊断，再解决安装、密钥、网络和回合卡住等问题。",
    },
    href: "/docs/troubleshooting",
    topicId: "troubleshooting",
    keywords: { en: "error crash doctor diagnose recover resume docker", zh: "错误 崩溃 诊断 恢复" },
  },
  {
    id: "trust",
    label: { en: "See what leaves your machine", zh: "了解哪些数据会离开本机" },
    description: {
      en: "What a provider receives, what usage counting sends, and how to turn it off.",
      zh: "提供商会收到什么、用量统计发送什么，以及如何关闭。",
    },
    href: "/docs/trust",
    topicId: "trust",
    keywords: { en: "privacy security telemetry data vulnerability report", zh: "隐私 安全 遥测 数据 漏洞" },
  },
];

/** The owning topic for a task, or undefined if the registry drifted. */
export function taskTopic(task: DocTask): DocTopic | undefined {
  return DOC_TOPICS.find((t) => t.id === task.topicId);
}

/** Lowercase haystack across both languages, the route, and the topic. */
export function docTaskHaystack(task: DocTask): string {
  return [
    task.id,
    task.label.en,
    task.label.zh,
    task.description.en,
    task.description.zh,
    task.keywords.en,
    task.keywords.zh,
    task.href,
    task.topicId,
  ]
    .join(" ")
    .toLowerCase();
}
