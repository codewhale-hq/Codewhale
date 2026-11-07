import type { LocalizedText } from "./vocabulary";

export const PLUGINS_COPY = {
  metaTitle: { en: "Plugins and marketplace · Codewhale", zh: "插件与市场 · Codewhale" },
  metaDescription: {
    en: "Let the agent operate the desktop apps you choose with Computer Use, and install reviewed plugins from the Codewhale catalog, a GitHub repository, or a tarball.",
    zh: "用 Computer Use 让代理操作你选定的桌面应用，并从 Codewhale 目录、GitHub 仓库或压缩包安装经过审阅的插件。",
  },
  kicker: { en: "Plugins and marketplace", zh: "插件与市场" },
  title: { en: "Add tools and skills with plugins", zh: "通过插件添加工具和技能" },
  lead: {
    en: "Add capabilities from the first-party Codewhale catalog, a GitHub repository, or a tarball. Nothing runs until you read the manifest and trust it.",
    zh: "从官方 Codewhale 目录、GitHub 仓库或压缩包添加能力。在你阅读清单并信任它之前，不会运行任何内容。",
  },
  marketplaceCta: { en: "Browse the Codewhale catalog", zh: "浏览 Codewhale 目录" },
  docsCta: { en: "Plugin authoring guide", zh: "插件编写指南" },

  cuLabel: { en: "First-party plugin · macOS beta", zh: "官方插件 · macOS 测试版" },
  cuTitle: { en: "Let an agent work in your apps", zh: "让智能体在应用中替你操作" },
  cuLead: {
    en: "Let an agent work in Mac apps while you keep using your computer. Connect the Computer Use helper and plugin to Codewhale or another compatible MCP host.",
    zh: "智能体在 Mac 应用中后台操作，你可以继续使用电脑。将 Computer Use 助手和插件连接到 Codewhale 或其他兼容 MCP 的宿主。",
  },
  cuInstall: { en: "/plugin marketplace install codewhale computer-use", zh: "/plugin marketplace install codewhale computer-use" },
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
    en: "An offline snapshot of the Codewhale catalog ships with the terminal, so browsing never fetches or runs anything. Install from it with one command.",
    zh: "终端内置 Codewhale 目录的离线快照，浏览时不会拉取或运行任何内容。一条命令即可安装。",
  },
  catalogBrowse: { en: "/plugin marketplace list", zh: "/plugin marketplace list" },
  catalogEntries: [
    {
      title: { en: "WhaleWiki", zh: "WhaleWiki" },
      detail: {
        en: "Repository and concept documentation the agent can consult while it works.",
        zh: "供代理在工作时查阅的仓库与概念文档。",
      },
      reference: "/plugin marketplace install codewhale whalewiki",
    },
    {
      title: { en: "Cloudflare Docs", zh: "Cloudflare 文档" },
      detail: {
        en: "Cloudflare documentation lookup inside a session.",
        zh: "在会话内查询 Cloudflare 文档。",
      },
      reference: "/plugin marketplace install codewhale cloudflare-docs",
    },
    {
      title: { en: "Skill bundle", zh: "技能包" },
      detail: {
        en: "The bundled Codewhale skills the terminal ships with, kept in sync with the catalog.",
        zh: "终端内置的 Codewhale 技能集，与目录保持同步。",
      },
      reference: "#path=skills",
    },
  ],
  catalogNote: {
    en: "Catalog source: github.com/codewhale-hq/codewhale-plugin-marketplace. A local catalog named codewhale takes precedence over the bundled snapshot.",
    zh: "目录来源：github.com/codewhale-hq/codewhale-plugin-marketplace。名为 codewhale 的本地目录优先于内置快照。",
  },

  sourcesLabel: { en: "Install sources", zh: "安装来源" },
  sourcesTitle: { en: "Install from a repository, folder, or tarball.", zh: "从仓库、文件夹或压缩包安装。" },
  sourcesLead: {
    en: "Point /plugin install at a directory, a repository, or a tarball. The fetched tree must hold exactly one bundle root: a plugin.json, a compatible kimi.plugin.json or .claude-plugin/plugin.json, or a legacy plugin.toml.",
    zh: "/plugin install 可以指向目录、仓库或压缩包。获取的目录树必须恰好包含一个 bundle 根：plugin.json、兼容的 kimi.plugin.json 或 .claude-plugin/plugin.json，或旧版 plugin.toml。",
  },
  sources: [
    {
      title: { en: "GitHub repository", zh: "GitHub 仓库" },
      detail: {
        en: "Fetches an archive of the default branch, the same mechanism the marketplace catalog uses.",
        zh: "获取默认分支的归档，市场目录也使用这一机制。",
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
        en: "/plugin show <name> prints the manifest, declared capabilities, and install receipt, including the source and bundle selector.",
        zh: "/plugin show <name> 打印清单、声明的能力与安装回执，包括来源与 bundle 选择器。",
      },
    },
    {
      title: { en: "Trust", zh: "信任" },
      detail: {
        en: "/plugin trust <name> marks your decision. Official catalog provenance grants no execution or network permission on its own.",
        zh: "/plugin trust <name> 记录你的决定。官方目录来源本身不授予任何执行或网络权限。",
      },
    },
    {
      title: { en: "Enable", zh: "启用" },
      detail: {
        en: "/plugin enable <name> activates the bundle. Plugin-contributed MCP servers run under namespaced <plugin>-<server> identities with the stricter boundary.",
        zh: "/plugin enable <name> 激活 bundle。插件提供的 MCP 服务以 <plugin>-<server> 命名空间身份运行，适用更严格的边界。",
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
  "/plugin enable <name>",
  "/plugin update <name>",
  "/plugin suggest <task>",
] as const;
