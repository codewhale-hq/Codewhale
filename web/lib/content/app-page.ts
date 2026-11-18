/**
 * app-page.ts — the copy for /app, the page about the desktop app and the
 * hosted browser app.
 *
 * TRUTH CONTRACT: the desktop app is a development build with no public
 * download, and the hosted browser app is not open yet. The terminal and the
 * local browser client (codewhale web) are released and need no account.
 * Nothing here names prices, plans, hours, dates, an auth vendor or a
 * download, and the page never links to the hosted app itself.
 */

import type { DesktopShotId } from "@/components/desktop-shot";
import type { LocalizedText } from "./vocabulary";

export interface AppShot {
  file: DesktopShotId;
  alt: LocalizedText;
  caption: LocalizedText;
}

export interface AppRow {
  title: LocalizedText;
  body: LocalizedText;
}

export const APP_COPY = {
  metadata: {
    title: { en: "Desktop and browser apps · Codewhale", zh: "桌面与浏览器应用 · Codewhale" },
    description: {
      en: "The Codewhale desktop app is built on the same agent you can read, in development today. The terminal and the local browser client are released.",
      zh: "Codewhale 桌面应用基于同一个可读智能体构建，目前处于开发阶段。终端与本地浏览器客户端均已发布。",
    },
  },
  title: {
    en: "The desktop app is built on the same agent you can read.",
    zh: "桌面应用基于同一个可读智能体构建。",
  },
  lede: {
    en: "It runs the same engine as the terminal, so the app and the terminal share one agent. It is a development build today, and the browser app is not open yet.",
    zh: "它与终端运行同一个引擎，因此应用与终端共用同一个智能体。目前它是开发版本，浏览器应用尚未开放。",
  },
  surfaces: {
    desktop: { en: "Desktop", zh: "桌面端" },
    browser: { en: "Browser app", zh: "浏览器应用" },
  },
  actions: {
    install: { en: "Install the terminal", zh: "安装终端版" },
    guide: { en: "Use codewhale web today", zh: "现在就用 codewhale web" },
    signIn: { en: "How sign-in works", zh: "登录方式" },
  },
  guidePath: "/docs/web",

  shotsHeading: { en: "What the development build looks like", zh: "开发版本长什么样" },
  shotsScope: {
    en: "These screens come from the desktop development build running the real engine and a real model, qwen3:4b, on this computer through Ollama. Details will change before a release.",
    zh: "以下画面来自桌面开发版本，运行的是真实引擎和真实模型：通过 Ollama 在本机运行的 qwen3:4b。发布前细节仍会变化。",
  },
  shots: [
    {
      file: "review",
      alt: {
        en: "The desktop app with the session list on the left, a finished task in the middle that ran python3 -m pytest -q with 3 tests passed, and on the right the review panel showing the change to core.py with Mark this diff reviewed and Commit and history.",
        zh: "桌面应用：左侧是会话列表，中间是一项已完成的任务，它运行了 python3 -m pytest -q，3 个测试通过；右侧审阅面板显示对 core.py 的修改，以及“标记此差异已审阅”和“提交与历史”。",
      },
      caption: {
        en: "A finished task with its change open in the review panel. You can mark the diff reviewed and commit from the same panel.",
        zh: "一项已完成的任务，修改在审阅面板中打开。你可以在同一面板中标记差异已审阅并提交。",
      },
    },
    {
      file: "approval",
      alt: {
        en: "The desktop app asking Run a command? for python3 -m pytest -q, with the buttons Allow once, Allow for this session and Don't allow, below the applied edit to tally/core.py.",
        zh: "桌面应用针对 python3 -m pytest -q 询问“Run a command?”，提供 Allow once、Allow for this session 和 Don't allow 三个按钮，上方是已应用到 tally/core.py 的修改。",
      },
      caption: {
        en: "In Accept Edits the edit went through, and the test command waits for your answer with the exact command in view.",
        zh: "在 Accept Edits 模式下，修改已直接应用，测试命令则等待你的回答，并展示完整命令。",
      },
    },
    {
      file: "permissions",
      alt: {
        en: "The Permissions menu open above the composer, listing Ask, Accept Edits (selected), Plan, Auto-Review and Full Access, and under Allowed for this session a Revoke entry for python3 -m pytest -q.",
        zh: "输入框上方打开的 Permissions 菜单，列出 Ask、Accept Edits（已选中）、Plan、Auto-Review 和 Full Access，并在“本会话已允许”下列出可撤销的 python3 -m pytest -q。",
      },
      caption: {
        en: "The Permissions menu sets what the agent may do without asking, and lists what you allowed for this session so you can revoke it.",
        zh: "Permissions 菜单决定智能体无需询问即可做什么，并列出你在本会话中允许过的操作，方便你撤销。",
      },
    },
    {
      file: "sessions",
      alt: {
        en: "The desktop app with three sessions in the tally folder: two marked Running and one finished, and the open session thinking about a request to explain the project.",
        zh: "桌面应用中 tally 文件夹下的三个会话：两个标记为运行中，一个已完成；打开的会话正在思考一个解释项目的请求。",
      },
      caption: {
        en: "Two sessions work in the same folder at once, and the list shows which ones are running.",
        zh: "两个会话同时在同一个文件夹中工作，列表会显示哪些正在运行。",
      },
    },
  ] satisfies AppShot[],

  accountHeading: { en: "What works without an account, and what an account adds", zh: "无需账号可用的功能，以及账号能带来什么" },
  accountScope: {
    en: "The app does not ask you to sign in up front. Where a control needs an account, it says so beside that control. Sign-in is not available in every build yet.",
    zh: "应用不会一开始就要求你登录。需要账号的控件会在旁边说明。并非所有版本都已提供登录。",
  },
  withoutTitle: { en: "Works without an account", zh: "无需账号" },
  without: [
    {
      title: { en: "Your folders and conversations", zh: "你的文件夹与对话" },
      body: { en: "They stay on this computer, and signing out leaves them in place.", zh: "它们保存在这台电脑上，退出登录后仍然保留。" },
    },
    {
      title: { en: "Your own models", zh: "你自己的模型" },
      body: { en: "Use your own model keys or a local model.", zh: "使用你自己的模型密钥或本地模型。" },
    },
    {
      title: { en: "Plugins", zh: "插件" },
      body: { en: "Skills, connectors, hooks and the plugin bundles that carry them run on this computer with no sign-in.", zh: "技能、连接器、钩子以及携带它们的插件包都在这台电脑上运行，无需登录。" },
    },
    {
      title: { en: "The terminal and the local browser", zh: "终端与本地浏览器" },
      body: { en: "Both are released and need no account.", zh: "两者均已发布，无需账号。" },
    },
  ] satisfies AppRow[],
  withTitle: { en: "What an account adds", zh: "账号能带来什么" },
  with: [
    {
      title: { en: "Shared conversations", zh: "共享对话" },
      body: { en: "Share a conversation with other people.", zh: "与其他人共享对话。" },
    },
    {
      title: { en: "Projects in your account", zh: "账号中的项目" },
      body: { en: "A project lives in your account instead of on one computer.", zh: "项目保存在你的账号中，而不是某一台电脑上。" },
    },
    {
      title: { en: "Cloud computers", zh: "云端计算机" },
      body: { en: "Send work to a cloud computer. Cloud computers are invite-only.", zh: "把任务交给云端计算机。云端计算机仅限受邀使用。" },
    },
    {
      title: { en: "Agents that keep going", zh: "持续工作的智能体" },
      body: { en: "Let an agent look after a repository on GitHub while you are away.", zh: "让智能体在你离开时照看 GitHub 上的仓库。" },
    },
  ] satisfies AppRow[],
  signInNote: {
    en: "When sign-in is available and you have an account, the app opens your browser to finish signing in and then returns you to the app. Signing out leaves your folders, conversations and model keys in place.",
    zh: "当登录可用且你已有账号时，应用会打开浏览器完成登录，然后带你回到应用。退出登录后，你的文件夹、对话和模型密钥仍会保留。",
  },

  runsHeading: { en: "Where Codewhale runs today", zh: "Codewhale 目前在哪里运行" },
  runsSurface: { en: "Surface", zh: "平台" },
  runsState: { en: "State", zh: "状态" },
  runsDetail: { en: "What it means", zh: "说明" },

  closingTitle: { en: "Start with the terminal today.", zh: "今天就从终端开始。" },
  closingBody: {
    en: "The terminal and codewhale web run the same agent the desktop app is built on. The sign-in guide explains how accounts work.",
    zh: "终端和 codewhale web 运行的，正是桌面应用所基于的智能体。登录指南会说明账户如何使用。",
  },
} as const;
