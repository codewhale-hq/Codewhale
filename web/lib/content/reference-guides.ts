import type { LocalizedText } from "./vocabulary";

export const REFERENCE_GUIDES = [
  {
    slug: "runtime-reference",
    repoSource: "docs/RUNTIME_API.md",
    title: { en: "Runtime API reference", zh: "Runtime API 参考" },
    lead: { en: "Read the canonical HTTP routes, request fields, event envelopes and compatibility contracts. For a first request, start with the Runtime API tutorial.", zh: "查阅规范的 HTTP 路由、请求字段、事件封装和兼容性约定。首次发送请求时，请先阅读 Runtime API 教程。" },
    tutorial: "/docs/runtime-api",
    tutorialLabel: { en: "Run the Runtime API tutorial", zh: "运行 Runtime API 教程" },
  },
  {
    slug: "workflow-tutorial",
    repoSource: "docs/FLEET_WORKFLOW_TUTORIAL.md",
    title: { en: "Fleet and Workflow tutorial", zh: "Fleet 与 Workflow 教程" },
    lead: { en: "Create a task specification, validate it, and follow a durable Fleet run. The full source tutorial includes the task fields, Workflow example and recovery limits.", zh: "创建任务规范，验证它，并跟踪持久 Fleet 运行。完整源码教程介绍任务字段、Workflow 示例和恢复限制。" },
    tutorial: "/docs/fleet",
    tutorialLabel: { en: "Start with the Workflow guide", zh: "先阅读 Workflow 指南" },
  },
  {
    slug: "plugin-authoring",
    repoSource: "docs/PLUGIN_AUTHORING.md",
    title: { en: "Write your first plugin", zh: "编写第一个插件" },
    lead: { en: "Build a two-file skill bundle, review its source, then trust and enable the exact reviewed content. Local installation and activation do not send a model request; the final greeting uses your connected provider.", zh: "创建包含两个文件的技能包，审阅源码后，再信任并启用已审阅的内容。本地安装和启用不发送模型请求；最后的问候会使用已连接的提供商。" },
    tutorial: "/plugins",
    tutorialLabel: { en: "Review plugin formats and permissions", zh: "查看插件格式与权限" },
  },
] satisfies { slug: string; repoSource: string; title: LocalizedText; lead: LocalizedText; tutorial: string; tutorialLabel: LocalizedText }[];

export const REFERENCE_GUIDE_COPY = {
  scope: { en: "The document below is generated from the repository's main branch, so it can describe behavior that the published release ({tag}) does not have. Check your installed version and read the tagged document when you need the published behavior.", zh: "下方文档由仓库 main 分支生成，因此可能描述已发布版本（{tag}）尚不具备的行为。请核对已安装版本；需要已发布版本的行为时，请阅读对应标签下的文档。" },
  published: { en: "Read this document as published in {tag}", zh: "阅读 {tag} 中发布的这份文档" },
  language: { en: "The complete reference below is rendered from the current English source. Its body has not been translated or reviewed in other languages.", zh: "下方完整参考内容由当前英文源码文档生成，正文尚未翻译为其他语言，也未完成相应语言审校。" },
  source: { en: "Read the source document", zh: "阅读源码文档" },
  revision: { en: "Inspect the source revision", zh: "查看来源版本" },
  code: { en: "Code example", zh: "代码示例" },
} satisfies Record<string, LocalizedText>;
