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

import type { LocalizedText } from "./vocabulary";

export interface AppShot {
  file: "conversation" | "approval" | "model-picker";
  width: number;
  height: number;
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
    guide: { en: "Read the app guide", zh: "阅读应用指南" },
    signIn: { en: "How sign-in works", zh: "登录方式" },
  },
  guideUrl: "https://docs.codewhale.net",

  shotsHeading: { en: "What the development build looks like", zh: "开发版本长什么样" },
  shotsScope: {
    en: "These are captures of the desktop development build, taken against a test fixture, so some names and text in them are placeholders. Details will change before a release.",
    zh: "以下是桌面开发版本的截图，取自测试环境，其中部分名称和文字只是占位内容。发布前细节仍会变化。",
  },
  shots: [
    {
      file: "conversation",
      width: 1440,
      height: 725,
      alt: {
        en: "The desktop app showing a session named Codewhale core. A cloud agent named Fix prompt tests changed two files, ran an approved test command, and the side panel lists Changes, Ready to review, Environment, People and Activity.",
        zh: "桌面应用中名为 Codewhale core 的会话。名为 Fix prompt tests 的云端智能体修改了两个文件并运行了已批准的测试命令，侧边栏列出更改、待审阅、环境、成员和活动记录。",
      },
      caption: {
        en: "A session with a cloud agent ready to review. The side panel keeps changes, environment, people and activity next to the conversation. Development build capture.",
        zh: "云端智能体完成改动待审阅的会话。侧边栏把更改、环境、成员和活动记录放在对话旁边。开发版本截图。",
      },
    },
    {
      file: "approval",
      width: 1440,
      height: 725,
      alt: {
        en: "The desktop app asking Run a command? with the exact shell command shown and two buttons: Allow once and Don't allow.",
        zh: "桌面应用弹出“Run a command?”的询问，显示完整的 shell 命令，并提供两个按钮：Allow once 和 Don't allow。",
      },
      caption: {
        en: "A command waits for your answer before it runs, with the exact command in view. Development build capture.",
        zh: "命令在运行前等待你的回答，并展示完整命令。开发版本截图。",
      },
    },
    {
      file: "model-picker",
      width: 1440,
      height: 725,
      alt: {
        en: "The desktop app's new session screen with the model picker open, DeepSeek selected on the provider rail, four DeepSeek models listed, and a switch at the bottom for This session or New sessions.",
        zh: "桌面应用的新会话界面，模型选择器已打开，左侧供应商栏选中 DeepSeek 并列出四个 DeepSeek 模型，底部提供“当前会话”或“新会话”的切换开关。",
      },
      caption: {
        en: "Pick a provider and model before you start, and choose whether the setting applies to this session or new sessions. Development build capture.",
        zh: "开始前按供应商选择模型，并决定设置应用于当前会话还是新会话。开发版本截图。",
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
