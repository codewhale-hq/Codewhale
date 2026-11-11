import type { DocsShellDict } from "../types";

/**
 * Simplified-Chinese dictionary for the docs shell: portal hero, hub
 * metadata, task/topic search, sidebar and breadcrumb chrome, the
 * release-truth band, the contextual help band on every docs page, the
 * shared page-body chrome, and the session recording panel.
 */
export const docsShell: DocsShellDict = {
  metaTitle: "文档 · Codewhale",
  metaDescription:
    "安装 Codewhale，连接模型提供商，然后把事情做完：模式与审批、查看改动、工作流、子智能体、MCP 工具、钩子、Runtime API 以及问题排查。",
  heroTitle: "安装与使用 Codewhale",
  heroLead: "查找安装步骤、模型连接、工具配置和问题排查指南。每份指南都包含可以运行的命令和示例。",
  installCta: "安装 Codewhale",

  releaseLabel: "版本",
  releasePublished: "最新发布 {tag} · {date}",
  releaseDescribes: "本站文档描述的是已发布的 {tag}。",
  sourceLead: "本节描述仅存在于项目源码、尚未包含在已发布版本中的行为。可用 `codewhale --version` 查看你使用的版本。",
  releaseChangelog: "更新日志 →",

  searchLabel: "搜索文档",
  searchPlaceholder: "搜索指南与参考",
  searchLoading: "正在加载文档索引。搜索词只保留在此浏览器中。",
  searchUnavailable: "完整索引未能加载。请重试，以搜索正文和参考命令。",
  searchRetry: "重新加载索引",
  searchClear: "清除",
  searchMatches: "{matched} / {total} 条匹配 “{query}”",
  searchNoMatches: "没有条目匹配 “{query}”",
  tasksHeading: "按任务",
  tasksLead: "从你想完成的事情开始。",
  topicsHeading: "按主题",
  webGuideTag: "网页指南",
  sourceDocTag: "源文档",
  sourceDetails: "详情",
  emptyTitle: "没有匹配的条目",
  emptyBody: "换一个关键词试试——中英文都可以搜索——或浏览 GitHub 上的完整文档目录。",
  emptyCta: "GitHub 文档目录 ↗",
  indexNote: "“网页指南”条目在 codewhale.net 上打开；“源文档”条目会打开 GitHub 仓库中的完整参考资料。",

  sidebarHeading: "文档目录",
  sidebarAria: "文档目录",
  breadcrumbAria: "面包屑导航",
  breadcrumbHome: "首页",
  breadcrumbDocs: "文档",

  helpTitle: "获取帮助",
  helpLead: "在 Discord 提问，或在 GitHub 报告文档问题。",
  helpSource: "来源：{name}",
  helpTroubleshooting: "排查问题",
  helpFaq: "常见问题",
  helpDiscord: "到 Discord 提问 ↗",
  helpIssue: "报告文档问题 ↗",

  nextHeading: "下一步",
  noteLabel: "注意：",
  onThisPage: "本页内容",

  mediaPendingNote: "还没有录像。录好之后会放在这里，附字幕、文字稿和可选的 GIF 下载。",
  mediaPlanLink: "录制计划与验收清单 ↗",
  mediaGifFallback: "GIF 下载回退（无视频环境）",
  mediaTranscript: "文字稿 ↗",
};
