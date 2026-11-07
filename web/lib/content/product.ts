/**
 * product.ts — the copy for /product, the "what is this and what do I get"
 * page behind the primary nav's first link.
 *
 * TRUTH CONTRACT: availability is stated per surface as it is today. The
 * terminal, browser, hosted web, and desktop rows mirror the homepage's
 * availability chapter and docs/public-surface-facts.json; the GUI row states
 * the community graphical frontend — a separate project from this repository's
 * own VS Code extension — exactly as README.md links it.
 * Nothing here claims cloud execution; the hosted web app is not open;
 * desktop is a development build.
 * Counts (providers, tools, sandbox backends) come from the facts layer at
 * render time, never typed here.
 */

import type { LocalizedText } from "./vocabulary";

export interface ProductRow {
  title: LocalizedText;
  body: LocalizedText;
}

export interface ProductAvailabilityRow {
  surface: LocalizedText;
  status: LocalizedText;
  detail: LocalizedText;
  /** Locale-relative route with the full story, an absolute external URL, or null. */
  href: string | null;
  linkLabel: LocalizedText | null;
}

export const PRODUCT_COPY = {
  metadata: {
    title: { en: "Build and automate · Codewhale", zh: "构建与自动化 · Codewhale" },
    description: {
      en: "Build apps, automate workflows and work across connected tools with an open-source agent. Bring your existing model APIs or local and self-hosted inference.",
      zh: "用开源智能体构建应用、自动处理工作，并连接你常用的工具。使用你已有的模型 API、本地模型或自托管推理服务。",
    },
  },
  title: {
    en: "Build apps and automate workflows",
    zh: "构建应用，自动化工作流",
  },
  lede: {
    en: "Codewhale brings files, commands, model teams and connected tools into one open-source agent. Start in your terminal or local browser, and add the capabilities your task needs.",
    zh: "Codewhale 把文件、命令、模型团队与已连接的工具集中在一个开源智能体中。从终端或本地浏览器开始，按任务需要添加能力。",
  },

  gainHeading: { en: "What you can build and automate", zh: "可以构建与自动化的工作" },
  gain: [
    {
      title: { en: "Build apps and useful tools", zh: "构建应用和实用工具" },
      body: {
        en: "Create a new application, extend a project, turn data into a report, or write a script that saves you time. The agent can create files, run commands and test the result.",
        zh: "做一个新应用、扩展现有项目、将数据整理成报告，或写一个节省时间的脚本。智能体可以创建文件、运行命令并测试成果。",
      },
    },
    {
      title: { en: "Work across your apps", zh: "让应用一起参与工作" },
      body: {
        en: "Add tools through plugins and MCP servers, or use APIs from your own scripts. Each service needs its own setup and authentication.",
        zh: "通过插件和 MCP 服务添加工具，或在自己的脚本中使用 API。每项服务都需要单独配置并完成身份验证。",
      },
    },
    {
      title: { en: "Make a workflow you can reuse", zh: "把工作变成可复用的流程" },
      body: {
        en: "Run tasks from scripts or CI with codewhale exec. For larger jobs, save a Fleet of agents with different roles and models and bring the team back when you need it.",
        zh: "用 codewhale exec 从脚本或 CI 中运行任务。面对更大的工作，保存由不同角色、不同模型组成的 Fleet 智能体团队，需要时再一起上场。",
      },
    },
  ] satisfies ProductRow[],

  workHeading: { en: "Give the job to a team you choose", zh: "把任务交给你选择的团队" },
  workLede: {
    en: "Start with a task, then bring in specialists or save the process for next time. Plans, models and permissions stay visible as the work grows.",
    zh: "从一个任务开始，按需加入专业智能体，或保存流程供下次使用。工作扩大时，计划、模型与权限依然清楚可见。",
  },
  workPatterns: [
    {
      title: { en: "Keep a bigger job moving with Operate", zh: "用 Operate 推进较大的任务" },
      body: {
        en: "Give Codewhale an objective. Operate emphasizes planning, delegation and verification while using the same tools and permissions as Work.",
        zh: "把目标交给 Codewhale。Operate 注重计划、委派与验证，使用与 Work 相同的工具，遵循相同的权限。",
      },
      href: "/docs/modes",
      command: "/mode operate",
    },
    {
      title: { en: "Put different models on the same team", zh: "让不同模型在同一个团队协作" },
      body: {
        en: "Choose one model to investigate, another to build and a third to review. Save the roles and model choices as a Fleet you can use again.",
        zh: "选择一个模型调查、另一个实现，再由第三个审阅。把角色与模型选择保存为 Fleet，以后继续使用。",
      },
      href: "/docs/fleet",
      command: "/fleet setup",
    },
    {
      title: { en: "Turn a process into a reusable Workflow", zh: "把流程变成可复用的 Workflow" },
      body: {
        en: "Connect named steps, parallel branches and review gates. For example, have code, test and docs reviewers work in parallel, then combine their findings.",
        zh: "连接具名步骤、并行分支与审阅关卡。例如，让代码、测试与文档审阅者并行工作，再汇总他们的发现。",
      },
      href: "https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/AUTOMATIC_WORKFLOWS.md",
      command: "codewhale workflow run",
    },
    {
      title: { en: "Teach Codewhale how you work", zh: "让 Codewhale 学习你的工作方式" },
      body: {
        en: "Write a Markdown skill for your review checklist or report format. Bundle skills and connected tools into a plugin, and share it with other people.",
        zh: "用 Markdown 技能记录审阅清单或报告格式。把技能和已连接的工具打包为插件，与他人分享。",
      },
      href: "/docs/skills",
      command: "SKILL.md",
    },
    {
      title: { en: "See which model did what", zh: "查看哪个模型做了什么" },
      body: {
        en: "Inspect the next primary request with /preview-request. Review recorded actions and approvals with /receipts, and use /undo for recorded file changes.",
        zh: "用 /preview-request 检查下一次主要请求，用 /receipts 查看已记录的操作与审批，并用 /undo 撤回已记录的文件改动。",
      },
      href: "/docs/review",
      command: "/receipts",
    },
  ],

  availabilityHeading: { en: "Available now and in development", zh: "已可使用和正在开发的版本" },
  availabilityLede: {
    en: "The terminal, local browser client, and community CodeWhale GUI are available now and need no Codewhale account. The desktop and hosted web apps are in development and share the same session model; your model connection stays your choice.",
    zh: "终端、本地浏览器客户端与社区维护的 CodeWhale GUI 现已可用，无需 Codewhale 账户。桌面和托管网页应用正在开发，共用同一会话模型；模型连接仍由你选择。",
  },
  availability: [
    {
      surface: { en: "Terminal", zh: "终端" },
      status: { en: "Released", zh: "已发布" },
      detail: {
        en: "Install release binaries for Linux, macOS, or Windows; npm and Cargo also work, and Android on Termux is a preview. The interactive TUI and codewhale exec for scripts ship together.",
        zh: "安装适用于 Linux、macOS 或 Windows 的发布版二进制；也可以用 npm 和 Cargo 安装，Android 上的 Termux 为预览。交互式 TUI 与用于脚本的 codewhale exec 一同发布。",
      },
      href: "/install",
      linkLabel: { en: "Install guide", zh: "安装指南" },
    },
    {
      surface: { en: "Local browser", zh: "本地浏览器" },
      status: { en: "Included with the terminal", zh: "随终端提供" },
      detail: {
        en: "Run codewhale web to open Codewhale in your browser. Read the conversation, send a task, and answer approvals on your machine.",
        zh: "运行 codewhale web，在浏览器中打开 Codewhale。在本机上查看对话、发送任务并回应审批。",
      },
      href: "/docs/web",
      linkLabel: { en: "Local browser guide", zh: "本地浏览器指南" },
    },
    {
      surface: { en: "CodeWhale GUI (VS Code)", zh: "CodeWhale GUI（VS Code）" },
      status: { en: "Available", zh: "可用" },
      detail: {
        en: "A separate community project: chat, threads, and file changes in a VS Code sidebar over the local Runtime. Install it from the VS Code Marketplace; the source is on GitHub.",
        zh: "独立的社区项目：在 VS Code 侧边栏中连接本地 Runtime，进行对话、管理线程并查看文件变更。可从 VS Code Marketplace 安装；源码见 GitHub。",
      },
      href: "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode",
      linkLabel: { en: "VS Code Marketplace", zh: "VS Code Marketplace" },
    },
    {
      surface: { en: "Hosted web app", zh: "托管网页应用" },
      status: { en: "In development", zh: "开发中" },
      detail: {
        en: "Being rebuilt to match the desktop app, and not open yet. For a browser view today, run codewhale web on your machine.",
        zh: "正在重建，以与桌面应用保持一致，暂未开放。如需现在在浏览器中使用，请在本机运行 codewhale web。",
      },
      href: null,
      linkLabel: null,
    },
    {
      surface: { en: "Desktop", zh: "桌面端" },
      status: { en: "Development build", zh: "开发版本" },
      detail: {
        en: "A native app on the same engine. Review and commit changes, follow work across projects, and edit a message to resend it as a new branch. No public download yet.",
        zh: "基于同一引擎的原生应用。审阅并提交改动、跨项目跟进工作，还能编辑一条消息并作为新分支重新发送。暂无公开下载。",
      },
      href: null,
      linkLabel: null,
    },
  ] satisfies ProductAvailabilityRow[],

  controlHeading: { en: "Set permissions and approvals", zh: "设置权限与审批方式" },
  controlLede: {
    en: "Use Plan to explore, Work to carry out a task, and Operate to coordinate larger jobs. Approval settings decide which actions wait for you.",
    zh: "用 Plan 探索方案、Work 执行任务、Operate 协调较大的工作。审批设置决定哪些操作需要等你确认。",
  },
  modes: [
    { title: { en: "Plan", zh: "Plan" }, body: { en: "Blocks file mutation and shell execution. Permitted research may contact external services; session state can still be saved.", zh: "禁止文件修改与 shell 执行。获准的研究可访问外部服务；会话状态仍可保存。" } },
    { title: { en: "Work", zh: "Work" }, body: { en: "Edits files and runs commands within the permission you set.", zh: "在你设定的权限内修改文件、运行命令。" } },
    { title: { en: "Operate", zh: "Operate" }, body: { en: "Coordinates larger jobs, delegating to other agents when useful. Uses the same tools and permission boundaries as Work.", zh: "协调较大的任务，按需委派其他智能体。使用与 Work 相同的工具，遵循相同的权限边界。" } },
  ] satisfies ProductRow[],
  permissions: [
    { title: { en: "Ask", zh: "Ask" }, body: { en: "Prompts according to the active approval rules; saved permissions and hard policy boundaries still apply.", zh: "按当前审批规则询问；已保存的权限与强制策略边界仍然生效。" } },
    { title: { en: "Auto-Review", zh: "Auto-Review" }, body: { en: "Automatically reviews eligible actions and reports any action it cannot approve.", zh: "自动审核符合条件的操作，并报告无法批准的操作。" } },
    { title: { en: "Full Access", zh: "Full Access" }, body: { en: "Reduces approval prompts. It does not bypass hard policy boundaries or grant access outside the allowed scope.", zh: "减少审批提示，但不会绕过强制策略边界，也不会授予允许范围之外的访问权限。" } },
  ] satisfies ProductRow[],

  surfacesHeading: { en: "Work across files and tools", zh: "在文件与工具之间完成工作" },
  surfacesLede: {
    en: "Work with your project, connected services, browser tools and agent teams from one session. Choose the model, add the connections you need, and return to the work without starting over.",
    zh: "在同一个会话中使用项目文件、已连接的服务、浏览器工具和智能体团队。选择模型，添加所需的连接，之后随时接着做，无需从头开始。",
  },
  surfacesLink: { en: "See all integrations", zh: "查看全部集成" },

  actions: {
    install: { en: "Install Codewhale", zh: "安装 Codewhale" },
    models: { en: "See every provider", zh: "查看所有提供商" },
    docs: { en: "Read the docs", zh: "阅读文档" },
  },
} as const;
