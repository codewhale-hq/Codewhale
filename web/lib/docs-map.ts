/**
 * docs-map.ts — canonical documentation registry for codewhale.net.
 *
 * Maps every first-class documentation topic area to its repo source file(s)
 * and website route. This is the single source of truth for the docs hub
 * sidebar, breadcrumbs, and drift/parity checks.
 *
 * EXTENSION PATH FOR NEW LOCALES:
 *   Labels are keyed by locale. Add a new locale column and update the page
 *   components that consume this map. The topic IDs, slugs, and repo sources
 *   are locale-agnostic.
 */

export interface DocTopic {
  /** Stable identifier used in routes and anchors. */
  id: string;
  /** URL slug for the docs sub-route (e.g. "install"). */
  slug: string;
  /** Label per locale. */
  label: { en: string; zh: string };
  /** Short description per locale. */
  description: { en: string; zh: string };
  /** Repo source file(s) — the canonical markdown doc in the repo. */
  repoSource: string | string[];
  /** Whether this topic has a dedicated website page (vs. linking out). */
  hasPage: boolean;
  /** Locale-relative website path when the page lives outside `/docs/<slug>`. */
  sitePath?: string;
  /** Category for grouping in the sidebar. */
  category: "getting-started" | "core-concepts" | "reference" | "extending" | "operations";
}

/** Sidebar and breadcrumb labels for each docs-map category. */
export const DOC_CATEGORY_LABELS: Record<DocTopic["category"], { en: string; zh: string }> = {
  "getting-started": { en: "Get started", zh: "开始使用" },
  "core-concepts": { en: "Work with Codewhale", zh: "日常使用" },
  extending: { en: "Extend and automate", zh: "扩展与自动化" },
  operations: { en: "Get help", zh: "获取帮助" },
  reference: { en: "Reference", zh: "参考" },
};

export const DOC_TOPICS: DocTopic[] = [
  {
    id: "first-time",
    slug: "first-time",
    label: { en: "Use Codewhale for the first time", zh: "第一次使用 Codewhale" },
    description: {
      en: "Open a terminal, install, pick the folder Codewhale works in, connect a model and finish a first task.",
      zh: "打开终端、安装、选择 Codewhale 工作的文件夹、连接模型并完成第一个任务。",
    },
    repoSource: ["docs/GUIDE.md", "docs/INSTALL.md", "docs/MODES.md", "docs/SANDBOX.md", "docs/KEYBINDINGS.md"],
    hasPage: true,
    category: "getting-started",
  },
  {
    id: "install",
    slug: "install",
    label: { en: "Install Codewhale", zh: "安装 Codewhale" },
    description: {
      en: "The installer, npm, Homebrew tap, Cargo, and release binaries, with the output each step prints.",
      zh: "安装脚本、npm、Homebrew tap、Cargo 与发布版二进制文件，以及每一步应有的输出。",
    },
    repoSource: "docs/INSTALL.md",
    hasPage: true,
    sitePath: "install",
    category: "getting-started",
  },
  {
    id: "guide",
    slug: "guide",
    label: { en: "Start your first task", zh: "开始第一个任务" },
    description: {
      en: "Install, connect a model, and give Codewhale a task in your project.",
      zh: "安装、连接模型，然后在你的项目里交给 Codewhale 一项任务。",
    },
    repoSource: ["docs/GUIDE.md", "docs/INSTALL.md", "docs/KEYBINDINGS.md"],
    hasPage: true,
    category: "getting-started",
  },
  {
    id: "auth",
    slug: "auth",
    label: { en: "Connect a provider", zh: "连接模型提供商" },
    description: {
      en: "Save a provider key, check which key is used, or run a local model.",
      zh: "保存提供商密钥、查看当前使用的密钥，或运行本地模型。",
    },
    repoSource: ["docs/INSTALL.md", "docs/PROVIDERS.md", "docs/CODEWHALE_AGENT.md"],
    hasPage: true,
    category: "getting-started",
  },
  {
    id: "account",
    slug: "account",
    label: { en: "Sign in to Codewhale", zh: "登录 Codewhale" },
    description: {
      en: "Sign in to an existing account from the web, the desktop app, or the terminal, sign out, and find data export and deletion.",
      zh: "在网页、桌面应用或终端登录已有账户、退出登录，以及找到数据导出和删除的入口。",
    },
    repoSource: ["docs/CONFIGURATION.md", "docs/PROVIDERS.md"],
    hasPage: true,
    category: "getting-started",
  },
  {
    id: "providers",
    slug: "providers",
    label: { en: "Choose a model", zh: "选择模型" },
    description: {
      en: "Supported providers, switching models, and running a model yourself.",
      zh: "支持的提供商、切换模型，以及自己运行模型。",
    },
    repoSource: ["docs/PROVIDERS.md", "docs/MODEL_LAB.md"],
    hasPage: true,
    sitePath: "models",
    category: "getting-started",
  },
  {
    id: "configuration",
    slug: "configuration",
    label: { en: "Change settings", zh: "修改设置" },
    description: {
      en: "Find the config file, change settings from the session or shell, and see what a repository may override.",
      zh: "找到配置文件，在会话或 shell 中修改设置，并了解仓库可以覆盖哪些设置。",
    },
    repoSource: ["docs/CONFIGURATION.md", "docs/LEGACY_PATHS.md"],
    hasPage: true,
    category: "getting-started",
  },
  {
    id: "modes",
    slug: "modes",
    label: { en: "Set modes and approvals", zh: "设置模式与审批" },
    description: {
      en: "Choose Plan, Work or Operate, and choose when Codewhale asks for approval.",
      zh: "选择 Plan、Work 或 Operate，并选择 Codewhale 何时询问审批。",
    },
    repoSource: "docs/MODES.md",
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "review",
    slug: "review",
    label: { en: "Review what changed", zh: "查看改动" },
    description: {
      en: "See every edit, roll files back to an earlier turn, get a code review, and keep a review receipt.",
      zh: "查看每一处修改，把文件回滚到之前的回合，做代码审查，并保留审查收据。",
    },
    repoSource: ["docs/RECEIPTS.md", "docs/CONFIGURATION.md"],
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "work",
    slug: "work",
    label: { en: "Track progress", zh: "跟踪进度" },
    description: {
      en: "Follow the goal, To-do list, and sub-agents in the workbar, and hand work to a fresh session.",
      zh: "在工作栏中跟踪目标、To-do 列表和子 Agent，并把工作交接给新的会话。",
    },
    repoSource: ["docs/TOOL_SURFACE.md", "docs/TOOL_LIFECYCLE.md"],
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "pet",
    slug: "pet",
    label: { en: "Watch each session with pet mode", zh: "用 Pet 模式查看每个会话" },
    description: {
      en: "A whale view per terminal that shows working, waiting for you, done, or failed, with replies and agents one key away.",
      zh: "每个终端一头鲸鱼，显示工作中、在等你、已完成或失败，回复和智能体只需按一个键即可查看。",
    },
    repoSource: "docs/PET_MODE.md",
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "subagents",
    slug: "subagents",
    label: { en: "Run agents in parallel", zh: "并行运行 Agent" },
    description: {
      en: "Hand independent parts of a task to sub-agents, pick roles, and keep parallel edits in worktrees.",
      zh: "把任务中独立的部分交给子 Agent，选择角色，并用工作树隔离并行修改。",
    },
    repoSource: ["docs/SUBAGENTS.md", "docs/FLEET.md", "docs/MODES.md"],
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "fleet",
    slug: "fleet",
    label: { en: "Run a workflow", zh: "运行 Workflow" },
    description: {
      en: "Save roles in a Fleet, write a repeatable Workflow, run it as a Lane, and run batches of tasks.",
      zh: "在 Fleet 中保存角色，编写可重复的 Workflow，把它作为 Lane 运行，并批量执行任务。",
    },
    repoSource: ["docs/FLEET.md", "docs/FLEET_WORKFLOW_TUTORIAL.md", "docs/WORKFLOW_AUTHORING.md"],
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "sandbox",
    slug: "sandbox",
    label: { en: "Limit what commands can touch", zh: "限制命令的访问范围" },
    description: {
      en: "The OS sandbox on macOS, Linux, and Windows, how to turn it on, and how much a command may write.",
      zh: "macOS、Linux 和 Windows 上的操作系统沙箱、如何开启，以及命令能写到哪里。",
    },
    repoSource: ["docs/SANDBOX.md", "docs/AUTHORIZATION_ORDER.md"],
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "trust",
    slug: "trust",
    label: { en: "See what leaves your machine", zh: "了解哪些数据会离开本机" },
    description: {
      en: "What stays local, what a provider receives, what usage counting sends, and how to turn it off.",
      zh: "哪些留在本地、提供商会收到什么、用量统计发送什么，以及如何关闭。",
    },
    repoSource: ["docs/SANDBOX.md", "docs/AUTHORIZATION_ORDER.md", "docs/TELEMETRY.md", "docs/public-surface-facts.json", "docs/PREVIEW_REQUEST.md"],
    hasPage: true,
    category: "core-concepts",
  },
  {
    id: "mcp",
    slug: "mcp",
    label: { en: "Connect tools with MCP", zh: "用 MCP 连接工具" },
    description: {
      en: "Add a connector: a local (stdio) or remote (HTTP) MCP server. Sign in with OAuth, serve Codewhale over MCP, and try code mode.",
      zh: "添加连接器：本地（stdio）或远程（HTTP）MCP 服务器。用 OAuth 登录、把 Codewhale 作为 MCP 服务器运行，并试用代码模式。",
    },
    repoSource: "docs/MCP.md",
    hasPage: true,
    category: "extending",
  },
  {
    id: "hooks",
    slug: "hooks",
    label: { en: "Run commands on events", zh: "在事件发生时运行命令" },
    description: {
      en: "Add a hook that runs your own script at session start, before a tool call, at turn end, or when Codewhale waits for you.",
      zh: "添加钩子，在会话开始、工具调用之前、回合结束或 Codewhale 等你回应时运行你自己的脚本。",
    },
    repoSource: ["docs/HOOKS.md", "docs/CONFIGURATION.md"],
    hasPage: true,
    category: "extending",
  },
  {
    id: "skills",
    slug: "skills",
    label: { en: "Add skills", zh: "添加技能" },
    description: {
      en: "Install, find, trust, and load reusable instruction packages. A skill can come on its own or inside a plugin.",
      zh: "安装、查找、信任并加载可复用的指令包。技能可以单独添加，也可以包含在插件中。",
    },
    repoSource: ["docs/SKILLS.md", "docs/CLAUDE_PLUGIN_COMPAT.md"],
    hasPage: true,
    category: "extending",
  },
  {
    id: "claude-code",
    slug: "claude-code",
    label: { en: "Bring your settings from another coding agent", zh: "从其他编码智能体带来你的设置" },
    description: {
      en: "Keep compatible instructions and skills, and review settings and MCP imports without running other agents' plugin runtimes.",
      zh: "保留兼容的指令与技能，审阅设置与 MCP 导入；不会运行其他智能体的插件运行时。",
    },
    repoSource: ["docs/CLAUDE_PLUGIN_COMPAT.md", "docs/CONFIGURATION.md"],
    hasPage: true,
    category: "getting-started",
  },
  {
    id: "plugins",
    slug: "plugins",
    label: { en: "Add plugins", zh: "添加插件" },
    description: {
      en: "Choose plugin formats, browse catalog examples, and review, trust and enable plugin bundles. A bundle can carry skills, connectors and hooks.",
      zh: "选择插件格式，浏览目录示例，并审阅、信任和启用插件包。一个插件包可以携带技能、连接器和钩子。",
    },
    repoSource: ["docs/PLUGINS.md", "docs/PLUGIN_BUNDLES.md", "docs/PLUGIN_MARKETPLACE.md", "docs/EXTENSIONS.md"],
    hasPage: true,
    sitePath: "plugins",
    category: "extending",
  },
  {
    id: "runtime-api",
    slug: "runtime-api",
    label: { en: "Automate with the Runtime API", zh: "用 Runtime API 自动化" },
    description: {
      en: "Run one-shot jobs from scripts, or drive threads, events, and approvals over the local HTTP API.",
      zh: "在脚本中运行一次性任务，或通过本地 HTTP API 驱动线程、事件和审批。",
    },
    repoSource: "docs/RUNTIME_API.md",
    hasPage: true,
    category: "extending",
  },
  {
    id: "web",
    slug: "web",
    label: { en: "Open the browser client", zh: "打开浏览器客户端" },
    description: {
      en: "Work in a local browser tab, or continue a terminal session from the web app with /rc.",
      zh: "在本机浏览器标签页中工作，或用 /rc 在网页应用中接着使用终端会话。",
    },
    repoSource: "docs/WEB.md",
    hasPage: true,
    category: "extending",
  },
  // Fleet is the canonical customer noun; `/docs/pod` remains a
  // permanent compatibility redirect in app/[locale]/docs/pod/page.tsx.
  {
    id: "troubleshooting",
    slug: "troubleshooting",
    label: { en: "Fix a problem", zh: "排查问题" },
    description: {
      en: "Diagnose in one command, then fix install, key, network, stuck-turn, session, and MCP problems.",
      zh: "用一条命令诊断，再解决安装、密钥、网络、回合卡住、会话和 MCP 等问题。",
    },
    repoSource: ["docs/INSTALL.md", "docs/OPERATIONS_RUNBOOK.md", "docs/DOCKER.md"],
    hasPage: true,
    category: "operations",
  },
  {
    id: "contribution",
    slug: "contribution",
    label: { en: "Contribute", zh: "参与贡献" },
    description: {
      en: "The contributing guide, agent ethos, contributor credits, and release process.",
      zh: "贡献指南、Agent 准则、贡献者致谢和发布流程。",
    },
    repoSource: [
      "CONTRIBUTING.md",
      "docs/AGENT_ETHOS.md",
      "docs/CONTRIBUTORS.md",
      "docs/RELEASE_CHECKLIST.md",
    ],
    hasPage: false,
    category: "operations",
  },
  {
    id: "runtime-reference",
    slug: "runtime-reference",
    label: { en: "Runtime API reference", zh: "Runtime API 参考" },
    description: { en: "Read the HTTP routes, request fields and event contracts in the repository.", zh: "查阅仓库中的 HTTP 路由、请求字段和事件约定。" },
    repoSource: ["docs/RUNTIME_API.md", "docs/RUNTIME_ROUTES.md"],
    hasPage: true,
    category: "reference",
  },
  {
    id: "workflow-tutorial",
    slug: "workflow-tutorial",
    label: { en: "Fleet and Workflow tutorial", zh: "Fleet 与 Workflow 教程" },
    description: { en: "Follow the full task-specification tutorial, including validation, launch and recovery.", zh: "按照完整任务规范教程进行验证、启动和恢复。" },
    repoSource: "docs/FLEET_WORKFLOW_TUTORIAL.md",
    hasPage: true,
    category: "extending",
  },
  {
    id: "plugin-authoring",
    slug: "plugin-authoring",
    label: { en: "Write your first plugin", zh: "编写第一个插件" },
    description: { en: "Build, inspect and activate a two-file skill bundle with a visible greeting check.", zh: "创建、检查并启用包含两个文件的技能包，通过问候标记核对结果。" },
    repoSource: "docs/PLUGIN_AUTHORING.md",
    hasPage: true,
    category: "extending",
  },
  {
    id: "vocabulary",
    slug: "vocabulary",
    label: { en: "Product terms", zh: "产品名词" },
    description: {
      en: "Understand Fleet, Workflow, Lane and Runtime, plus the modes and permission postures.",
      zh: "了解 Fleet、Workflow、Lane、Runtime，以及模式与权限设置。",
    },
    repoSource: ["docs/FLEET.md", "docs/MODES.md", "docs/public-surface-facts.json"],
    hasPage: true,
    category: "reference",
  },
  {
    id: "constitution",
    slug: "constitution",
    label: { en: "Constitution", zh: "宪章" },
    description: {
      en: "The rules the agent follows: built-in rules, your personal constitution, and a repository's own.",
      zh: "Agent 遵循的规则：内置规则、你的个人宪章，以及仓库自己的宪章。",
    },
    repoSource: "docs/ARCHITECTURE.md",
    hasPage: true,
    category: "reference",
  },
  {
    id: "tools",
    slug: "tools",
    label: { en: "Built-in tools", zh: "内置工具" },
    description: {
      en: "The small set of tools every session starts with, and how Codewhale finds the rest.",
      zh: "每个会话默认携带的少量工具，以及 Codewhale 如何按需找到其余工具。",
    },
    repoSource: ["docs/TOOL_SURFACE.md", "docs/RUNTIME_SIMPLIFICATION_DESIGN.md", "docs/LSP_PHP_CUSTOM.md"],
    hasPage: true,
    category: "reference",
  },
];

/** Convenience lookup. */
export function getTopic(id: string): DocTopic | undefined {
  return DOC_TOPICS.find((t) => t.id === id);
}

/** Group topics by category for sidebar rendering. */
export function getTopicsByCategory(): Map<DocTopic["category"], DocTopic[]> {
  const map = new Map<DocTopic["category"], DocTopic[]>();
  for (const t of DOC_TOPICS) {
    const group = map.get(t.category) ?? [];
    group.push(t);
    map.set(t.category, group);
  }
  return map;
}

/** Resolve a topic to its on-site route or canonical repository document. */
export function docTopicHref(topic: DocTopic, locale: string): string {
  if (topic.sitePath) return `/${locale}/${topic.sitePath}`;
  if (topic.hasPage) return `/${locale}/docs/${topic.slug}`;
  const source = Array.isArray(topic.repoSource) ? topic.repoSource[0] : topic.repoSource;
  return `${REPO_DOCS_BASE}/${source}`;
}

/** Whether following a topic leaves codewhale.net for the source document. */
export function docTopicIsExternal(topic: DocTopic): boolean {
  return !topic.hasPage;
}

/** Repo source base URL for generating direct links. */
export const REPO_DOCS_BASE = "https://github.com/codewhale-hq/CodeWhale/blob/main";
