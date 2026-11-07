import type { LocalizedText } from "./vocabulary";

export const RELEASE_UPDATES_COPY = {
  title: { en: "Latest releases", zh: "最新发布" },
  source: { en: "Published on GitHub. Updates automatically.", zh: "来自 GitHub 的正式发布，自动更新。" },
  terminal: { en: "Codewhale terminal", zh: "Codewhale 终端" },
  computerUse: { en: "Computer Use plugin", zh: "Computer Use 插件" },
  notes: { en: "Release notes", zh: "发布说明" },
  all: { en: "All release notes", zh: "全部发布说明" },
  rss: { en: "RSS", zh: "RSS" },
  cached: { en: "Last known release", zh: "上次确认的版本" },
  unavailable: { en: "Check published releases", zh: "查看已发布版本" },
  refreshFailed: { en: "Updates could not be checked. The release links still work.", zh: "暂时无法检查更新，仍可通过链接查看发布。" },
} satisfies Record<string, LocalizedText>;
