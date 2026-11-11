/**
 * availability.ts — where Codewhale runs today, one row per surface.
 *
 * /product and /app both render these rows. A row lists the pages that show
 * it; wording changes here change every page at once.
 */

import type { LocalizedText } from "./vocabulary";

export type AvailabilityTone = "ready" | "attention" | "idle";

export interface ProductAvailabilityRow {
  id: "terminal" | "local-browser" | "vscode" | "hosted-web" | "desktop" | "phone" | "computer-use";
  surface: LocalizedText;
  status: LocalizedText;
  tone: AvailabilityTone;
  pages: ReadonlyArray<"product" | "app">;
  detail: LocalizedText;
  /** Locale-relative route with the full story, an absolute external URL, or null. */
  href: string | null;
  linkLabel: LocalizedText | null;
}

export const AVAILABILITY_ROWS: ProductAvailabilityRow[] = [
  {
    id: "terminal",
    surface: { en: "Terminal", zh: "终端" },
    status: { en: "Released", zh: "已发布" },
    tone: "ready",
    pages: ["product", "app"],
    detail: {
      en: "Install release binaries for Linux, macOS, or Windows; npm and Cargo also work, and Android on Termux is a preview. The interactive TUI and codewhale exec for scripts ship together.",
      zh: "安装适用于 Linux、macOS 或 Windows 的发布版二进制；也可以用 npm 和 Cargo 安装，Android 上的 Termux 为预览。交互式 TUI 与用于脚本的 codewhale exec 一同发布。",
    },
    href: "/install",
    linkLabel: { en: "Install guide", zh: "安装指南" },
  },
  {
    id: "local-browser",
    surface: { en: "Local browser", zh: "本地浏览器" },
    status: { en: "Released", zh: "已发布" },
    tone: "ready",
    pages: ["product", "app"],
    detail: {
      en: "Run codewhale web to open Codewhale in your browser. Read the conversation, send a task, and answer approvals on your machine. It comes with the terminal and needs no account.",
      zh: "运行 codewhale web，在浏览器中打开 Codewhale。在本机上查看对话、发送任务并回应审批。它随终端提供，无需账号。",
    },
    href: "/docs/web",
    linkLabel: { en: "Local browser guide", zh: "本地浏览器指南" },
  },
  {
    id: "vscode",
    surface: { en: "CodeWhale GUI (VS Code)", zh: "CodeWhale GUI（VS Code）" },
    status: { en: "Available", zh: "可用" },
    tone: "ready",
    pages: ["product"],
    detail: {
      en: "A separate community project: chat, threads, and file changes in a VS Code sidebar over the local Runtime. Install it from the VS Code Marketplace; the source is on GitHub.",
      zh: "独立的社区项目：在 VS Code 侧边栏中连接本地 Runtime，进行对话、管理线程并查看文件变更。可从 VS Code Marketplace 安装；源码见 GitHub。",
    },
    href: "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode",
    linkLabel: { en: "VS Code Marketplace", zh: "VS Code Marketplace" },
  },
  {
    id: "hosted-web",
    surface: { en: "Browser app", zh: "浏览器应用" },
    status: { en: "Not open yet", zh: "暂未开放" },
    tone: "idle",
    pages: ["product", "app"],
    detail: {
      en: "Being rebuilt to match the desktop app, and not open yet. For a browser view today, run codewhale web on your machine.",
      zh: "正在重建，以与桌面应用保持一致，暂未开放。如需现在在浏览器中使用，请在本机运行 codewhale web。",
    },
    href: null,
    linkLabel: null,
  },
  {
    id: "desktop",
    surface: { en: "Desktop", zh: "桌面端" },
    status: { en: "Development build", zh: "开发版本" },
    tone: "idle",
    pages: ["product", "app"],
    detail: {
      en: "A desktop app on the same engine. Review changed files and commit them, and edit an earlier message to continue from it in a new conversation. No public download yet.",
      zh: "基于同一引擎的桌面应用。审阅已更改的文件并提交，还能编辑较早的一条消息，并在新对话中从那里继续。暂无公开下载。",
    },
    href: null,
    linkLabel: null,
  },
  {
    id: "phone",
    surface: { en: "Phone", zh: "手机" },
    status: { en: "Not public", zh: "未公开" },
    tone: "idle",
    pages: ["app"],
    detail: {
      en: "The phone app is not available to the public yet.",
      zh: "手机应用尚未向公众开放。",
    },
    href: null,
    linkLabel: null,
  },
  {
    id: "computer-use",
    surface: { en: "Computer Use", zh: "Computer Use" },
    status: { en: "Mac beta", zh: "Mac 测试版" },
    tone: "attention",
    pages: ["product"],
    detail: { en: "Background app control on macOS 13.5 or later. Requires the separate helper and plugin.", zh: "在 macOS 13.5 或更高版本上后台操作应用。需要单独安装辅助应用与插件。" },
    href: "/computer-use",
    linkLabel: { en: "Set up Computer Use", zh: "设置 Computer Use" },
  },
];
