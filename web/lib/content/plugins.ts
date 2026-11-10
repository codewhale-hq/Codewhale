import type { LocalizedText } from "./vocabulary";

export const PLUGINS_COPY = {
  metaTitle: { en: "Plugins, skills and MCP servers · Codewhale", zh: "插件与市场 · Codewhale" },
  metaDescription: {
    en: "Connect local and remote MCP servers, add reusable skills, and install reviewed plugins for browser work, documentation, code review and desktop apps.",
    zh: "连接本地与远程 MCP 服务器、加载可复用的技能，并安装用于浏览器、文档、代码审阅和桌面应用的已审阅插件。",
  },
  kicker: { en: "Plugins and marketplace", zh: "插件与市场" },
  title: { en: "Connect your tools to Codewhale", zh: "将你的工具连接到 Codewhale" },
  lead: {
    en: "Connect an MCP server for tools, load a skill for instructions, or install a plugin that packages both. You choose which integrations run.",
    zh: "连接 MCP 服务器来添加工具，加载技能来复用指令，或安装将两者打包的插件。由你决定运行哪些集成。",
  },
  marketplaceCta: { en: "Browse the Codewhale catalog", zh: "浏览 Codewhale 目录" },
  docsCta: { en: "Plugin authoring guide", zh: "插件编写指南" },

  cuLabel: { en: "First-party plugin · macOS beta", zh: "官方插件 · macOS 测试版" },
  cuTitle: { en: "Computer Use", zh: "Computer Use" },
  cuHomeTitle: { en: "Work in Mac apps while you keep working", zh: "智能体操作 Mac 应用，你继续工作" },
  cuLead: {
    en: "Let an agent work in Mac apps while you keep using your computer. Connect the Computer Use helper and plugin to Codewhale or another compatible MCP host.",
    zh: "智能体在 Mac 应用中后台操作，你可以继续使用电脑。将 Computer Use 助手和插件连接到 Codewhale 或其他兼容 MCP 的宿主。",
  },
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
  cuToolsNote: {
    en: "The marketplace plugin is available for Mac. GitHub releases also offer an unsigned experimental Windows preview; Linux remains experimental source and Docker setup. Other platforms do not provide Mac-style background input.",
    zh: "市场插件适用于 Mac。GitHub 发布页另有未签名的 Windows 实验性预览包；Linux 仍需实验性源码或 Docker 设置。其他平台不提供 Mac 式后台输入。",
  },

  catalogLabel: { en: "First-party catalog", zh: "官方目录" },
  catalogTitle: { en: "Browse Codewhale plugins", zh: "浏览 Codewhale 插件" },
  catalogLead: {
    en: "These are examples from the public Codewhale catalog. The terminal ships an offline snapshot: available entries and pinned versions depend on your CLI release. Browsing the catalog runs nothing.",
    zh: "以下是公开 Codewhale 目录中的部分示例。终端内置离线快照，条目及固定版本取决于 CLI 版本；浏览目录不会运行任何内容。",
  },
  catalogBrowse: { en: "/plugin marketplace list", zh: "/plugin marketplace list" },
  catalogNote: {
    en: "Catalog source: github.com/codewhale-hq/codewhale-plugin-marketplace. A local catalog named codewhale takes precedence over the bundled snapshot.",
    zh: "目录来源：github.com/codewhale-hq/codewhale-plugin-marketplace。名为 codewhale 的本地目录优先于内置快照。",
  },

  sourcesLabel: { en: "Install sources", zh: "安装来源" },
  sourcesTitle: { en: "Install a bundle from its source", zh: "从来源安装插件包" },
  sourcesLead: {
    en: "Point /plugin install at a directory, a GitHub repository, a tarball, or an exact-version npm package. The fetched tree must hold exactly one bundle root: a plugin.json, a compatible kimi.plugin.json or .claude-plugin/plugin.json, or a legacy plugin.toml.",
    zh: "/plugin install 可以指向目录、GitHub 仓库、压缩包或指定版本的 npm 包。获取的目录树必须恰好包含一个 bundle 根：plugin.json、兼容的 kimi.plugin.json 或 .claude-plugin/plugin.json，或旧版 plugin.toml。",
  },
  sources: [
    {
      title: { en: "GitHub repository", zh: "GitHub 仓库" },
      detail: {
        en: "Fetches a repository archive. Pin a commit for repeatable installs.",
        zh: "获取仓库归档。固定提交可实现可重复安装。",
      },
      reference: "/plugin install github:owner/repo",
    },
    {
      title: { en: "Local directory", zh: "本地目录" },
      detail: {
        en: "Copied from disk. The fastest loop while authoring your own bundle.",
        zh: "从磁盘复制。编写自己的 bundle 时最快的迭代方式。",
      },
      reference: "/plugin install ./path/to/bundle",
    },
    {
      title: { en: "Tarball URL", zh: "压缩包 URL" },
      detail: {
        en: "Downloads the archive directly under the per-domain network policy. An unknown host is refused until you allow it.",
        zh: "在按域网络策略下直接下载归档。未知主机会被拒绝，直到你允许它。",
      },
      reference: "/plugin install https://example.com/x.tar.gz",
    },
    {
      title: { en: "Version-pinned npm package", zh: "固定版本的 npm 包" },
      detail: { en: "Fetches the exact tarball without lifecycle scripts or dependency installation. It must contain a supported native plugin manifest.", zh: "获取指定版本的压缩包，不运行生命周期脚本或安装依赖。包中须包含受支持的原生插件清单。" },
      reference: "/plugin install npm:@example/plugin@1.0.0",
    },
  ],

  trustLabel: { en: "Trust lifecycle", zh: "信任生命周期" },
  trustTitle: { en: "Review and enable a plugin", zh: "审阅并启用插件" },
  trustLead: {
    en: "Every bundle lands disabled and untrusted. Review the manifest and declared capabilities, then trust and enable it. An update with changed bytes requires review again.",
    zh: "每个 bundle 安装后均为未启用、未信任状态。审阅清单与声明的能力后，再信任并启用。内容有变更的更新需要重新审阅。",
  },
  trustSteps: [
    {
      title: { en: "Review", zh: "审阅" },
      detail: {
        en: "/plugin trust <name> shows the capability review and prints the confirmation command.",
        zh: "/plugin trust <name> 显示能力审阅内容，并打印确认命令。",
      },
    },
    {
      title: { en: "Trust", zh: "信任" },
      detail: {
        en: "After reviewing, copy and run the exact confirmation command, including both full hashes. Catalog provenance alone grants no permission.",
        zh: "审阅后，复制并运行打印出的完整确认命令，保留两个完整哈希。目录来源本身不授予权限。",
      },
    },
    {
      title: { en: "Enable", zh: "启用" },
      detail: {
        en: "/plugin enable <name> activates the bundle. Plugin-contributed MCP servers use namespaced identities and the plugin’s reviewed permissions.",
        zh: "/plugin enable <name> 激活 bundle。插件提供的 MCP 服务器使用带命名空间的身份，受已审阅的插件权限约束。",
      },
    },
  ],
  trustNote: {
    en: "Downloads are tarball-only with a size cap and no symlinks; traversal, ambiguous roots, and changed plugin identities are rejected. /plugin suggest ranks installed and catalog bundles against a task without installing anything.",
    zh: "下载仅限压缩包，有大小限制且不允许符号链接；路径穿越、模糊根目录与插件身份变更都会被拒绝。/plugin suggest 仅对已安装与目录中的 bundle 按任务排序，不会安装任何内容。",
  },

  commandsTitle: { en: "Command reference", zh: "命令参考" },
  commandsLead: {
    en: "Run the full lifecycle from the terminal:",
    zh: "在终端中完成整个生命周期：",
  },
} satisfies Record<
  string,
  | LocalizedText
  | { title: LocalizedText; detail: LocalizedText; reference: string }[]
  | { title: LocalizedText; detail: LocalizedText }[]
>;

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

export const INTEGRATIONS_COPY = {
  chooseTitle: { en: "Plugin, skill, or MCP server?", zh: "插件、技能，还是 MCP 服务器？" },
  chooseLead: { en: "Start with what you want to add.", zh: "根据你想添加的能力选择。" },
  mcpTitle: { en: "Connect an MCP server directly", zh: "直接连接 MCP 服务器" },
  mcpLead: { en: "Use local tools over stdio or a remote service over Streamable HTTP. Direct remote connections can use OAuth or environment-backed tokens. A plugin is optional.", zh: "通过 stdio 使用本地工具，或通过 Streamable HTTP 连接远程服务。直接连接的远程服务器可使用 OAuth 或环境变量令牌，无需安装插件。" },
  mcpLink: { en: "MCP setup, authentication and troubleshooting", zh: "MCP 设置、认证与故障排查" },
  compatibilityTitle: { en: "Which plugin formats work?", zh: "支持哪些插件格式？" },
  compatibilityLead: { en: "Compatibility depends on the capabilities inside a bundle. A recognized manifest does not mean every feature from another host will run.", zh: "兼容性取决于插件包含的能力。能识别清单，并不代表其他宿主的每项功能都能运行。" },
  contributeTitle: { en: "Share an integration", zh: "分享你的集成" },
  contributeLead: { en: "Package a useful workflow, document its dependencies, and show what you verified. The authoring guide covers a local bundle; the catalog repository is where to propose an entry or improve an existing one.", zh: "将实用工作流打包，说明依赖并提供验证结果。编写指南介绍本地插件包；新增目录条目或改进现有条目，请前往目录仓库。" },
  catalogLink: { en: "Contribute to the catalog", zh: "参与目录贡献" },
  cuSetupNote: { en: "The published CLI v0.10.2 includes an older Computer Use bundle. Installing the catalog entry does not upgrade that built-in copy. Follow the version-specific setup before connecting the current Mac helper.", zh: "已发布的 CLI v0.10.2 内置较旧的 Computer Use 插件。安装目录条目不会升级内置副本。连接当前 Mac 助手前，请按对应版本的指南设置。" },
} satisfies Record<string, LocalizedText>;

export const INTEGRATION_CHOICES = [
  { title: { en: "Skill", zh: "技能" }, detail: { en: "Reusable Markdown instructions for a task. Uses the tools already available; it does not create a new service connection.", zh: "可复用的 Markdown 任务指令。使用已有工具，不会自行创建服务连接。" }, href: "/docs/skills" },
  { title: { en: "MCP server", zh: "MCP 服务器" }, detail: { en: "An external program or service that exposes tools, resources or prompts. Configure its command or URL and authentication.", zh: "提供工具、资源或提示词的外部程序或服务。配置命令或 URL 以及认证方式。" }, href: "/docs/mcp" },
  { title: { en: "Plugin", zh: "插件" }, detail: { en: "A reviewed bundle of skills, commands, agent profiles, hooks, MCP servers or provider declarations. Install, review, trust, then enable.", zh: "将技能、命令、代理配置、钩子、MCP 服务器或模型提供商声明打包。安装、审阅、信任后再启用。" }, href: "#trust-title" },
];

export const PLUGIN_EXAMPLES = [
  { name: "cloudflare-docs", title: "Cloudflare Docs", detail: { en: "Look up Cloudflare documentation with a remote MCP server and skill. No credentials required.", zh: "通过远程 MCP 服务器与技能查阅 Cloudflare 文档，无需凭据。" } },
  { name: "chromewhale", title: "Codewhale for Chrome", detail: { en: "Work in signed-in Chrome through local MCP. Requires Node and the separate browser extension, with per-site grants.", zh: "通过本地 MCP 操作已登录的 Chrome。需要 Node 和独立浏览器扩展，并按站点授权。" } },
  { name: "github-workflow", title: "GitHub Workflow", detail: { en: "Review issues and pull requests with skills, commands and GitHub’s remote MCP. Requires your GitHub token; review its scopes.", zh: "使用技能、命令及 GitHub 远程 MCP 审阅问题与拉取请求。需要 GitHub 令牌，请审阅其权限范围。" } },
  { name: "linear-workflow", title: "Linear Workflow", detail: { en: "Turn issue context into a plan with skills, commands and remote MCP. Requires a Linear API key.", zh: "通过技能、命令及远程 MCP 根据问题上下文制定计划。需要 Linear API 密钥。" } },
  { name: "review-toolkit", title: "Review Toolkit", detail: { en: "Use focused review agent profiles, a command and a skill. Adds no MCP server or service login.", zh: "使用专门的审阅代理配置、命令和技能。不添加 MCP 服务器，也无需服务登录。" } },
  { name: "whalewiki", title: "WhaleWiki", detail: { en: "Research a repository with a skill, command, agent profile and local MCP. Requires Node.", zh: "通过技能、命令、代理配置及本地 MCP 研究仓库。需要 Node。" } },
];

export const PLUGIN_COMPATIBILITY = [
  { title: { en: "Codewhale / Agent Plugins v1", zh: "Codewhale / Agent Plugins v1" }, detail: { en: "Declarative skills, commands, agent profiles, hooks, MCP and provider declarations. Native TypeScript extensions are experimental and require extension_host; declaring LSP or filesystem roots does not activate them.", zh: "支持声明式技能、命令、代理配置、钩子、MCP 及模型提供商声明。原生 TypeScript 扩展为实验功能，需启用 extension_host；声明 LSP 或文件系统根目录不会启用对应能力。" }, href: "https://github.com/codewhale-hq/Codewhale/blob/main/docs/PLUGIN_AUTHORING.md" },
  { title: { en: "Claude Code and Kimi bundles", zh: "Claude Code 与 Kimi 插件包" }, detail: { en: "Compatible skills, commands, agent profiles and MCP declarations can be imported. Claude hooks, LSP, custom MCP paths and plugin-root expansion are rejected. Review the imported capabilities.", zh: "可导入兼容的技能、命令、代理配置及 MCP 声明。Claude 钩子、LSP、自定义 MCP 路径及插件根路径展开会被拒绝。请审阅导入的能力。" }, href: "/docs/claude-code" },
  { title: { en: "DeepSeek Harness (DSH)", zh: "DeepSeek Harness（DSH）" }, detail: { en: "Import work is in progress. Full published-plugin workflows and interactive views are not release-qualified; do not assume a DSH package runs unchanged.", zh: "导入兼容工作进行中。已发布插件的完整工作流及交互视图尚未通过发布验证，请勿假定 DSH 包可直接运行。" }, href: "https://github.com/codewhale-hq/Codewhale/blob/main/docs/DSH_PLUGIN_COMPAT.md" },
  { title: { en: "MCP authentication and UI", zh: "MCP 认证与界面" }, detail: { en: "Plugin-bundled MCP does not support OAuth: configure that server directly instead. MCP Apps UI and every optional protocol feature are not generally supported.", zh: "插件内的 MCP 不支持 OAuth，请改为直接配置服务器。MCP Apps 界面及所有可选协议功能并非普遍受支持。" }, href: "/docs/mcp#compatibility" },
];
