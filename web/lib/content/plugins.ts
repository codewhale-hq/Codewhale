import { compareVersions } from "../features";
import type { LocalizedText } from "./vocabulary";

export const PLUGINS_COPY = {
  metaTitle: { en: "Plugins · Codewhale", zh: "插件 · Codewhale" },
  metaDescription: {
    en: "A plugin adds skills, commands, agent profiles, hooks and MCP connectors to Codewhale. You install it, review what it asks for, trust it and enable it.",
    zh: "插件可为 Codewhale 添加技能、命令、代理配置、钩子和 MCP 连接器。由你安装、审阅其所请求的能力、信任并启用。",
  },
  title: { en: "Add to Codewhale with plugins", zh: "用插件扩展 Codewhale" },
  lead: {
    en: "A plugin bundle can carry skills, commands, agent profiles, hooks and MCP connectors. You install it, review what it asks for, trust it and enable it, and nothing in it runs before that. A skill, a connector or a hook can also be added on its own with /skill install, /mcp add or /hooks add.",
    zh: "一个插件包可以携带技能、命令、代理配置、钩子和 MCP 连接器。你安装它，审阅它所请求的能力，信任并启用它，在此之前其中的任何内容都不会运行。技能、连接器或钩子也可以单独添加，分别使用 /skill install、/mcp add 或 /hooks add。",
  },
  marketplaceCta: { en: "Browse the catalog", zh: "浏览目录" },
  docsCta: { en: "Authoring guide", zh: "编写指南" },

  cuLabel: { en: "First-party plugin · macOS beta", zh: "官方插件 · macOS 测试版" },
  cuTitle: { en: "Computer Use", zh: "Computer Use" },
  cuHomeTitle: { en: "Work in Mac apps while you keep working", zh: "智能体操作 Mac 应用，你继续工作" },
  cuFeatures: [
    {
      title: { en: "Works in the background", zh: "后台工作" },
      detail: {
        en: "On macOS the agent can type, click and scroll in a selected app without moving your pointer or taking focus. Some apps and dialogs need foreground interaction.",
        zh: "在 macOS 上，智能体可在选定应用中输入、点击和滚动，无需移动你的指针或抢占焦点。部分应用和对话框需要前台操作。",
      },
    },
    {
      title: { en: "Read app controls", zh: "读取应用控件" },
      detail: {
        en: "Accessible controls expose buttons, fields and their values, so a text-only model can operate supported apps. Screenshots remain available when the task needs them.",
        zh: "无障碍控件提供按钮、输入框及其当前值，让纯文本模型也能操作支持的应用。任务需要时，仍可使用截图。",
      },
    },
    {
      title: { en: "Screenshots and local text recognition", zh: "截图与本地文字识别" },
      detail: {
        en: "Screenshots and zoom feed compatible vision models. On macOS, local OCR reads window text on-device with no remote service or vision model.",
        zh: "截图与缩放供兼容的视觉模型使用。在 macOS 上，本地 OCR 直接在设备上读取窗口文字，无需远程服务或视觉模型。",
      },
    },
    {
      title: { en: "Explore remote computers · experimental", zh: "尝试远程计算机 · 实验功能" },
      detail: {
        en: "Experimental routes reach registered SSH hosts, disposable Docker desktops and HarmonyOS devices over hdc. These need separate setup; Mac background-control guarantees do not apply to their shared desktops.",
        zh: "实验性连接可访问已注册的 SSH 主机、一次性 Docker 桌面及通过 hdc 连接的 HarmonyOS 设备。这些连接需要单独设置，其共享桌面不具备 Mac 后台操作的保证。",
      },
    },
    {
      title: { en: "Manage system permissions", zh: "管理系统权限" },
      detail: {
        en: "You grant Accessibility and Screen Recording in macOS System Settings. Standard mode then works without per-app prompts; denied apps, Pause and Stop still apply, and strict approval is optional.",
        zh: "由你在 macOS 系统设置中授予辅助功能和屏幕录制权限。标准模式随后无需逐个应用询问；禁用的应用、Pause 和 Stop 仍然有效，也可选择严格审批模式。",
      },
    },
    {
      title: { en: "Stop when an action cannot be verified", zh: "无法确认操作时停止" },
      detail: {
        en: "Stale observations, unexpected foreground changes, and unavailable capabilities refuse with a receipt instead of guessing. Each task owns its session; stopping releases every held input.",
        zh: "过期的观察结果、意外的前台变化与不可用的能力都会携带回执拒绝执行，而非猜测。每个任务独占自己的会话；停止即释放所有按住的输入。",
      },
    },
    {
      title: { en: "Automate a separate browser profile", zh: "在独立浏览器配置中自动操作" },
      detail: {
        en: "Browse, fill forms and capture pages in a separate Chromium profile. Computer Use leaves your personal browser tabs and logins alone; signed-in Chrome uses the separate Chromewhale connector.",
        zh: "在独立的 Chromium 配置中浏览网页、填写表单并截图。Computer Use 不会操作你的个人浏览器标签页和登录信息；使用已登录的 Chrome 需另行连接 Chromewhale。",
      },
    },
    {
      title: { en: "Preview and record your work", zh: "预览并录制操作过程" },
      detail: {
        en: "Request a live app preview or record a local Mac demonstration with ScreenCaptureKit. Capture starts only when requested; Pause and Stop remain available from the helper.",
        zh: "按需打开应用实时预览，或用 ScreenCaptureKit 录制本地 Mac 演示。只有请求后才会开始采集，助手中的 Pause 和 Stop 控件始终可用。",
      },
    },
  ],

  kindsTitle: { en: "What a plugin can add", zh: "插件能添加什么" },
  originBefore: { en: "The idea comes from ", zh: "这个想法来自 " },
  originName: { en: "DeepSeek Harness", zh: "DeepSeek Harness" },
  originAfter: {
    en: ", whose README describes an everything-is-a-plugin architecture. Codewhale adopts it inside one Engine that keeps turns, sessions, approvals and receipts, so a plugin adds capabilities without adding a second runtime.",
    zh: "，其 README 将自身描述为“everything-is-a-plugin”架构。Codewhale 在同一个引擎内采用这一思路：回合、会话、审批和回执仍由引擎持有，插件只增加能力，不增加第二套运行时。",
  },
  dshStatus: {
    en: "Reviewed DeepSeek Harness plugins can run in the experimental extension host. /plugin import dsh previews a bundle, then installs it disabled and untrusted. Services Codewhale owns, such as turns, sessions, approvals and credentials, are refused by name. Update and rollback are not verified, and none of this is a release qualification.",
    zh: "经审阅的 DeepSeek Harness 插件可在实验性扩展宿主中运行。/plugin import dsh 先预览插件包，再以未启用、未信任状态安装。Codewhale 自己持有的服务，如回合、会话、审批和凭据，会按名称拒绝。更新与回滚尚未验证，以上均不代表通过发布验证。",
  },
  dshLink: { en: "DeepSeek Harness compatibility notes (current source, ahead of the release)", zh: "DeepSeek Harness 兼容性说明（当前源码，领先于已发布版本）" },

  connectorsTitle: { en: "Connectors: the apps you already use", zh: "连接器：你已在用的应用" },
  connectorsLead: {
    en: "An MCP server is how Codewhale reaches an app. Add one directly, or install a plugin that carries it.",
    zh: "Codewhale 通过 MCP 服务器连接应用。可以直接添加，也可以安装自带它的插件。",
  },
  authOauth: { en: "OAuth", zh: "OAuth" },
  authToken: { en: "Token", zh: "令牌" },
  authNone: { en: "No sign-in", zh: "无需登录" },
  connectorsMore: {
    en: "more in the catalog",
    zh: "个，见目录",
  },
  connectorsNote: {
    en: "Add a connector directly to use OAuth; a connector bundled in a plugin can only read a token from your environment. Either way, run one read-only tool after connecting to confirm it works.",
    zh: "直接添加的连接器可使用 OAuth；插件内置的连接器只能从环境变量读取令牌。无论哪种方式，连接后请先运行一个只读工具确认可用。",
  },
  connectorsGoogle: {
    en: "Gmail and Google Calendar have no connector here. The skills-google plugin adds workflows that use a Gmail or Calendar tool you have connected.",
    zh: "这里没有 Gmail 和 Google Calendar 连接器。skills-google 插件提供的工作流使用你已连接的 Gmail 或日历工具。",
  },
  connectorsLink: { en: "MCP setup, authentication and troubleshooting", zh: "MCP 设置、认证与故障排查" },

  comingTitle: { en: "Coming in 0.10.3, not in the current release", zh: "即将在 0.10.3 推出，当前发布版本尚未包含" },
  comingLead: {
    en: "These are planned for 0.10.3. They are not in the version you can install today, and the plan can change before the release. The rest of this page describes the released version.",
    zh: "以下内容计划在 0.10.3 推出，当前可安装的版本尚未包含，发布前计划也可能调整。本页其余内容描述的都是已发布版本。",
  },

  catalogTitle: { en: "Plugins from the catalog", zh: "目录中的插件" },
  catalogLead: {
    en: "Browsing the catalog runs nothing. The terminal ships an offline snapshot, so the entries and versions you see depend on your release.",
    zh: "浏览目录不会运行任何内容。终端内置离线快照，所见条目与版本取决于你的发布版本。",
  },
  catalogSeeAll: { en: "See all plugins in the catalog repository", zh: "在目录仓库中查看全部插件" },

  safeTitle: { en: "Installed disabled and untrusted", zh: "安装后未启用、未信任" },
  safeLead: {
    en: "Every plugin installs disabled and untrusted. You review what it asks for, trust it, then enable it.",
    zh: "每个插件安装后都处于未启用、未信任状态。你先审阅它所请求的能力，再信任，最后启用。",
  },
  safeSteps: [
    { title: { en: "Install", zh: "安装" }, command: "/plugin install", detail: { en: "Lands disabled and untrusted.", zh: "安装后未启用、未信任。" } },
    { title: { en: "Review", zh: "审阅" }, command: "/plugin show <name>", detail: { en: "Shows the manifest and the capabilities it declares.", zh: "显示清单及其声明的能力。" } },
    { title: { en: "Trust", zh: "信任" }, command: "/plugin trust <name>", detail: { en: "Prints the confirmation command to run.", zh: "打印需要运行的确认命令。" } },
    { title: { en: "Enable", zh: "启用" }, command: "/plugin enable <name>", detail: { en: "Activates the bundle.", zh: "激活插件包。" } },
  ],
  safeHashes: {
    en: "The confirmation command carries two full hashes, one for the plugin’s bytes and one for its declared capabilities, so an update with changed bytes needs review again. Catalog provenance alone grants no permission.",
    zh: "确认命令带有两个完整哈希，分别对应插件内容和其声明的能力，因此内容有变更的更新需要重新审阅。仅凭目录来源不会授予任何权限。",
  },
  sourcesLead: {
    en: "Install from any of these sources:",
    zh: "可从以下任一来源安装：",
  },

  referenceTitle: { en: "Command reference and formats", zh: "命令参考与格式" },
  referenceFormats: { en: "Plugin formats", zh: "插件格式" },
  referenceCommands: { en: "Commands", zh: "命令" },

  contributeTitle: { en: "Share a plugin", zh: "分享你的插件" },
  contributeLead: {
    en: "Package a workflow, document its dependencies and show what you verified. The authoring guide covers a local bundle; the catalog repository is where you propose an entry.",
    zh: "将工作流打包，说明依赖并提供验证结果。编写指南介绍本地插件包；要提交目录条目，请前往目录仓库。",
  },
  contributeLink: { en: "Propose a catalog entry", zh: "提交目录条目" },
} satisfies Record<
  string,
  LocalizedText | string | { title: LocalizedText; detail: LocalizedText }[] | { title: LocalizedText; command: string; detail: LocalizedText }[]
>;

export const PLUGIN_KINDS = [
  { title: { en: "Skills", zh: "技能" }, detail: { en: "Markdown instructions for a task, loaded when they apply.", zh: "针对某项任务的 Markdown 指令，适用时加载。" } },
  { title: { en: "Commands", zh: "命令" }, detail: { en: "Slash commands that start a workflow.", zh: "启动某个工作流的斜杠命令。" } },
  { title: { en: "Agent profiles", zh: "代理配置" }, detail: { en: "Named roles for sub-agents, such as a read-only reviewer.", zh: "子代理的命名角色，例如只读审阅者。" } },
  { title: { en: "Hooks", zh: "钩子" }, detail: { en: "Commands that run on session events.", zh: "在会话事件上运行的命令。" } },
  { title: { en: "MCP connectors", zh: "MCP 连接器" }, detail: { en: "Servers that give the agent tools in other apps.", zh: "为智能体提供其他应用中工具的服务器。" } },
];

export const PLUGINS_COMING_VERSION = "0.10.3";

export function pluginsComingApplies(release: { version: string } | null): boolean {
  if (!release) return true;
  const order = compareVersions(release.version, PLUGINS_COMING_VERSION);
  return order === null || order < 0;
}

export const PLUGINS_COMING = [
  {
    title: { en: "One Plugins place", zh: "统一的“插件”页面" },
    detail: {
      en: "Everything you add is listed in one Plugins place. Each row says what it adds (Skill, Connector, Hook or Command), where it came from and whether it is on. /skills, /mcp and /hooks keep working and open that place on their own filter.",
      zh: "你添加的所有内容都会列在同一个“插件”页面。每一行标明它添加了什么（技能、连接器、钩子或命令）、来源以及是否已启用。/skills、/mcp 和 /hooks 仍然可用，并会按各自的筛选条件打开该页面。",
    },
  },
  {
    title: { en: "Standard mode and Creator mode", zh: "Standard 模式与 Creator 模式" },
    detail: {
      en: "You choose a mode for each conversation. In Creator mode the agent can write a plugin for you. A Standard conversation can install or remove a plugin, and the permission mode decides whether it asks you first.",
      zh: "你为每个对话选择一种模式。在 Creator 模式下，智能体可以为你编写插件。Standard 模式的对话可以安装或移除插件，是否先询问你由权限模式决定。",
    },
  },
  {
    title: { en: "Your permission mode decides what turns on", zh: "启用什么由权限模式决定" },
    detail: {
      en: "Turning on a plugin the agent wrote is treated like any other action: Ask asks you, Auto-Review's reviewer model judges it, Plan refuses it, and in Full Access the agent may turn it on. Codewhale records that the agent did it and shows it wherever plugins are listed. Until you review it once, that plugin works only in its own project.",
      zh: "启用智能体编写的插件与其他操作一样处理：Ask 会询问你，Auto-Review 由审阅模型判断，Plan 会拒绝，在 Full Access 下智能体可以直接启用。Codewhale 会记录这是智能体所为，并在列出插件的各处显示。在你审阅一次之前，该插件只在它所属的项目中生效。",
    },
  },
  {
    title: { en: "Permissions", zh: "Permissions（权限）" },
    detail: {
      en: "The control that holds Plan and Work and the permission dial is renamed Permissions, with Plan still on it. The new Mode menu holds Standard and Creator.",
      zh: "包含 Plan、Work 与权限档位的那个控件改名为 Permissions，Plan 仍在其中。新的 Mode 菜单包含 Standard 与 Creator。",
    },
  },
] satisfies { title: LocalizedText; detail: LocalizedText }[];

export const FEATURED_CONNECTORS = [
  "github", "linear", "notion", "stripe", "supabase", "sentry", "figma", "atlassian", "canva", "posthog", "hugging-face", "context7",
] as const;

export const CATALOG_FEATURED: { name: string; title: string; status?: LocalizedText; detail: LocalizedText }[] = [
  { name: "computer-use", title: "Computer Use", status: { en: "macOS beta", zh: "macOS 测试版" }, detail: { en: "Let an agent work in Mac apps while you keep using your computer. Setup and limits are on the Computer Use page.", zh: "让智能体在 Mac 应用中工作，你继续使用电脑。设置与限制见 Computer Use 页面。" } },
  { name: "chromewhale", title: "Codewhale for Chrome", detail: { en: "Work in signed-in Chrome, one granted site at a time. Needs Node and the separate browser extension.", zh: "在已登录的 Chrome 中逐站点授权后操作。需要 Node 和独立的浏览器扩展。" } },
  { name: "skills-google", title: "Google skills", detail: { en: "Gmail and Calendar workflows through a tool you connect or your own OAuth client. Connects no account by itself.", zh: "通过你连接的工具或你自己的 OAuth 客户端运行 Gmail 与日历工作流。本身不连接任何账号。" } },
  { name: "github-workflow", title: "GitHub Workflow", detail: { en: "Review pull requests and triage issues through GitHub’s remote MCP. Needs your GitHub token; nothing posts without your approval.", zh: "通过 GitHub 远程 MCP 审阅拉取请求并分流问题。需要你的 GitHub 令牌；未经你批准不会发布任何内容。" } },
  { name: "linear-workflow", title: "Linear Workflow", detail: { en: "Triage issues, claim work and post status updates through Linear’s remote MCP. Needs your Linear API key.", zh: "通过 Linear 远程 MCP 分流问题、认领工作并发布状态更新。需要你的 Linear API 密钥。" } },
  { name: "cloudflare", title: "Cloudflare", detail: { en: "Build and test Workers, and deploy with cf only after you approve.", zh: "构建并测试 Workers，仅在你批准后用 cf 部署。" } },
  { name: "vercel-workflow", title: "Vercel Workflow", detail: { en: "Preview and production deploys only after you approve, plus environment variables and logs.", zh: "仅在你批准后进行预览与生产部署，并管理环境变量和日志。" } },
  { name: "office", title: "Office", detail: { en: "Make PowerPoint, Excel and Word files, and analyze CSV data, with Node alone.", zh: "仅用 Node 生成 PowerPoint、Excel 和 Word 文件，并分析 CSV 数据。" } },
  { name: "review-toolkit", title: "Review Toolkit", detail: { en: "Six read-only specialist reviewers and a /review-pr command that fans them out over your diff.", zh: "六位只读专项审阅者，以及对你的差异并行调用它们的 /review-pr 命令。" } },
  { name: "whalewiki", title: "WhaleWiki", detail: { en: "Understand a repository, find where to change it and see which docs depend on a file. Needs Node.", zh: "理解仓库、找到修改位置，并查看哪些文档依赖某个文件。需要 Node。" } },
];

export const PLUGIN_SOURCES = `/plugin install github:owner/repo
/plugin install ./path/to/bundle
/plugin install https://example.com/x.tar.gz
/plugin install npm:@example/plugin@1.0.0`;

export const PLUGIN_COMMANDS = [
  "/plugin marketplace list",
  "/plugin marketplace show codewhale",
  "/plugin marketplace install codewhale <name>",
  "/plugin install github:owner/repo",
  "/plugin show <name>",
  "/plugin trust <name>",
  "/plugin trust <name> <full-content-sha256>.<full-capability-sha256>",
  "/plugin enable <name>",
  "/plugin update <name>",
  "/plugin suggest <task>",
] as const;

export const PLUGIN_COMPATIBILITY = [
  { title: { en: "Codewhale / Agent Plugins v1", zh: "Codewhale / Agent Plugins v1" }, detail: { en: "Declarative skills, commands, agent profiles, hooks, MCP and provider declarations. Native TypeScript extensions are experimental and require extension_host.", zh: "声明式技能、命令、代理配置、钩子、MCP 及模型提供商声明。原生 TypeScript 扩展为实验功能，需启用 extension_host。" }, href: "https://github.com/codewhale-hq/Codewhale/blob/main/docs/PLUGIN_AUTHORING.md" },
  { title: { en: "Plugin bundles from other agents", zh: "来自其他智能体的插件包" }, detail: { en: "Compatible skills, commands, agent profiles and MCP declarations in .claude-plugin bundles can be imported. Hooks, LSP declarations and custom MCP paths are rejected.", zh: "可导入 .claude-plugin 插件包中兼容的技能、命令、代理配置及 MCP 声明。钩子、LSP 声明和自定义 MCP 路径会被拒绝。" }, href: "/docs/claude-code" },
  { title: { en: "DeepSeek Harness (DSH)", zh: "DeepSeek Harness（DSH）" }, detail: { en: "Experimental. Import previews a bundle and installs it disabled and untrusted; services Codewhale owns are refused by name.", zh: "实验功能。导入会先预览插件包，并以未启用、未信任状态安装；Codewhale 自己持有的服务会按名称拒绝。" }, href: "https://github.com/codewhale-hq/Codewhale/blob/main/docs/DSH_PLUGIN_COMPAT.md" },
];
