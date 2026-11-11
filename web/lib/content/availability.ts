/**
 * availability.ts — where Codewhale runs today, one row per surface.
 *
 * /product and /app both render these rows. A row lists the pages that show
 * it; wording changes here change every page at once.
 */

import { DESKTOP_RELEASE, desktopDownload, type DesktopDownload, type DesktopReleaseFact } from "../desktop-release";
import { stateLabel, STATE_TONE, surfaceCapabilities, surfaceState, type SurfaceId, type SurfaceState, type SurfaceTone } from "../surface-states";
import { pickText } from "../i18n/dictionaries";
import type { LocalizedText } from "./vocabulary";

export type AvailabilityTone = SurfaceTone;

export interface ProductAvailabilityRow {
  id: Extract<SurfaceId, "terminal" | "local-browser" | "vscode" | "hosted-web" | "desktop" | "phone" | "computer-use">;
  surface: LocalizedText;
  pages: ReadonlyArray<"product" | "app">;
  detail: LocalizedText;
  detailReleased?: LocalizedText;
  /** Locale-relative route with the full story, an absolute external URL, or null. */
  href: string | null;
  linkLabel: LocalizedText | null;
}

export const AVAILABILITY_ROWS: ProductAvailabilityRow[] = [
  {
    id: "terminal",
    surface: { en: "Terminal", zh: "终端" },
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
    pages: ["product", "app"],
    detail: {
      en: "A desktop app on the same engine. Review changed files and commit them, and edit an earlier message to continue from it in a new conversation. No public download yet.",
      zh: "基于同一引擎的桌面应用。审阅已更改的文件并提交，还能编辑较早的一条消息，并在新对话中从那里继续。暂无公开下载。",
    },
    detailReleased: {
      en: "A desktop app on the same engine. Review changed files and commit them, and edit an earlier message to continue from it in a new conversation.",
      zh: "基于同一引擎的桌面应用。审阅已更改的文件并提交，还能编辑较早的一条消息，并在新对话中从那里继续。",
    },
    href: null,
    linkLabel: null,
  },
  {
    id: "phone",
    surface: { en: "Phone", zh: "手机" },
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
    surface: { en: "Computer Use (Mac)", zh: "Computer Use（Mac）" },
    pages: ["product"],
    detail: { en: "Background app control on macOS 13.5 or later. Requires the separate helper and plugin.", zh: "在 macOS 13.5 或更高版本上后台操作应用。需要单独安装辅助应用与插件。" },
    href: "/computer-use",
    linkLabel: { en: "Set up Computer Use", zh: "设置 Computer Use" },
  },
];

export interface AvailabilityView {
  state: SurfaceState;
  status: string;
  tone: SurfaceTone;
  detail: string;
  link: boolean;
  download: DesktopDownload | null;
}

export function describeRow(
  row: ProductAvailabilityRow,
  locale: string,
  desktopRelease: DesktopReleaseFact | null = DESKTOP_RELEASE,
): AvailabilityView {
  const context = { desktopRelease };
  const state = surfaceState(row.id, context);
  const capabilities = surfaceCapabilities(row.id, context);
  return {
    state,
    status: stateLabel(state, locale),
    tone: STATE_TONE[state],
    detail: pickText(capabilities.download && row.detailReleased ? row.detailReleased : row.detail, locale),
    link: capabilities.link && row.href !== null && row.linkLabel !== null,
    download: capabilities.download ? desktopDownload(desktopRelease) : null,
  };
}
