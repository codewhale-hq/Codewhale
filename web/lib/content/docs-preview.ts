import type { LocalizedText } from "./vocabulary";

export type DocsPreviewSlug = "install" | "quickstart" | "modes" | "runtime-routes";

export const DOCS_PREVIEW_NAV: readonly { group: LocalizedText; pages: readonly { slug: DocsPreviewSlug; label: LocalizedText; description: LocalizedText }[] }[] = [
  {
    group: { en: "Get started", zh: "开始使用" },
    pages: [
      { slug: "install", label: { en: "Install", zh: "安装" }, description: { en: "Every install channel, with the output each step prints and how to verify, update and remove.", zh: "所有安装渠道、每一步应有的输出，以及验证、更新和卸载的方法。" } },
      { slug: "quickstart", label: { en: "Quickstart", zh: "快速上手" }, description: { en: "Your first hour: the main workflow, the safety controls and where to go next.", zh: "第一个小时：主要工作流、安全控制，以及接下来读什么。" } },
    ],
  },
  {
    group: { en: "Concepts", zh: "概念" },
    pages: [
      { slug: "modes", label: { en: "Modes and approvals", zh: "模式与审批" }, description: { en: "Plan and Work, the permission dial, and when Codewhale asks before it acts.", zh: "Plan 与 Work、权限档位，以及 Codewhale 何时在执行前询问。" } },
    ],
  },
  {
    group: { en: "Reference", zh: "参考" },
    pages: [
      { slug: "runtime-routes", label: { en: "Runtime API routes", zh: "Runtime API 路由" }, description: { en: "Every HTTP path and method the local Runtime API registers, generated from the route table in source.", zh: "本地 Runtime API 注册的所有 HTTP 路径和方法，由源码中的路由表生成。" } },
    ],
  },
];

export const DOCS_PREVIEW_COPY = {
  title: { en: "Documentation preview", zh: "文档预览" },
  lead: { en: "A prototype of the next documentation shell. The pages render the repository's Markdown and source data at build time; the published documentation is unchanged.", zh: "下一版文档外壳的原型。页面在构建时渲染仓库中的 Markdown 和源码数据；已发布的文档没有变化。" },
  banner: { en: "Preview: this prototype describes the main branch, not a published release.", zh: "预览：此原型描述的是 main 分支，而不是已发布版本。" },
  search: { en: "Search", zh: "搜索" },
  searchEmpty: { en: "Type to search every heading and paragraph.", zh: "输入即可搜索所有标题和段落。" },
  searchNone: { en: "Nothing matches “{query}”", zh: "没有与“{query}”匹配的内容" },
  searchFailed: { en: "The search index could not load.", zh: "搜索索引加载失败。" },
  close: { en: "Close", zh: "关闭" },
  copyPage: { en: "Copy page", zh: "复制页面" },
  viewMarkdown: { en: "View as Markdown", zh: "查看 Markdown" },
  source: { en: "Source", zh: "源文件" },
  previous: { en: "Previous", zh: "上一页" },
  next: { en: "Next", zh: "下一页" },
  english: { en: "This page has no translation for your language yet, so it is shown in English.", zh: "此页面尚无你所选语言的译文，因此以英文显示。" },
  routesLead: { en: "{count} literal paths in {groups} groups. A registered path is not a grant of permission or a promise that a feature is enabled in your installed version; the Runtime API contract explains fields, events and authorization.", zh: "共 {count} 个字面路径，分为 {groups} 组。已注册的路径并不代表授予权限，也不保证你安装的版本启用了该功能；Runtime API 约定说明了字段、事件和授权。" },
  method: { en: "Method", zh: "方法" },
  path: { en: "Path", zh: "路径" },
} satisfies Record<string, LocalizedText>;
