import type { LocalizedText } from "./vocabulary";
import { pickText } from "@/lib/i18n/dictionaries";

/** Labels describe the captured application views; terminal contents stay verbatim. */
export const NATIVE_TERMINAL_VIEWS = {
  home: {
    label: { en: "Home", zh: "首页" },
    description: {
      en: "The conversation, composer and model status in one view.",
      zh: "在同一界面查看对话、输入框和模型状态。",
    },
  },
  composer: {
    label: { en: "Composer", zh: "输入框" },
    description: {
      en: "A follow-up message in the composer, ready to send.",
      zh: "输入框中准备发送的后续消息。",
    },
  },
  workbar: {
    label: { en: "Workbar", zh: "工作栏" },
    description: {
      en: "Task progress and session details in the workbar.",
      zh: "在工作栏中查看任务进度与会话详情。",
    },
  },
  "workbar-fleet": {
    label: { en: "Fleet", zh: "智能体团队" },
    description: {
      en: "A delegated reviewer and its completed task in the Fleet workbar.",
      zh: "团队工作栏中受委派的审阅智能体及其已完成任务。",
    },
  },
  "provider-picker": {
    label: { en: "Providers", zh: "提供商" },
    description: {
      en: "The provider list and connection details, captured with a local demo model.",
      zh: "使用本地演示模型实录的提供商列表和连接详情。",
    },
  },
  help: {
    label: { en: "Help", zh: "帮助" },
    description: {
      en: "Commands and keyboard shortcuts.",
      zh: "命令与键盘快捷键。",
    },
  },
} satisfies Record<string, { label: LocalizedText; description: LocalizedText }>;

export const NATIVE_TERMINAL_COPY = {
  title: { en: "Explore the terminal", zh: "探索终端界面" },
  description: {
    en: "Captured views from a local Codewhale demo.",
    zh: "Codewhale 本地演示的实录画面。",
  },
  viewsLabel: { en: "Terminal views", zh: "终端视图" },
  showFull: { en: "Show full terminal", zh: "查看完整终端" },
  showDetail: { en: "Focus on this panel", zh: "聚焦此面板" },
  detailLabel: { en: "Panel detail from the captured terminal", zh: "终端实录中的面板详情" },
  scrollHint: { en: "Scroll sideways to see the full terminal.", zh: "左右滚动，查看完整终端。" },
} satisfies Record<string, LocalizedText>;

export function getNativeTerminalCopy(locale: string) {
  return {
    title: pickText(NATIVE_TERMINAL_COPY.title, locale),
    description: pickText(NATIVE_TERMINAL_COPY.description, locale),
    viewsLabel: pickText(NATIVE_TERMINAL_COPY.viewsLabel, locale),
    showFull: pickText(NATIVE_TERMINAL_COPY.showFull, locale),
    showDetail: pickText(NATIVE_TERMINAL_COPY.showDetail, locale),
    detailLabel: pickText(NATIVE_TERMINAL_COPY.detailLabel, locale),
    scrollHint: pickText(NATIVE_TERMINAL_COPY.scrollHint, locale),
    views: Object.fromEntries(Object.entries(NATIVE_TERMINAL_VIEWS).map(([id, view]) => [id, {
      label: pickText(view.label, locale),
      description: pickText(view.description, locale),
    }])),
  };
}
