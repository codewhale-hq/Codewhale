import type { DocsVocabularyDict } from "../types";

/** Chinese dictionary for `app/[locale]/docs/vocabulary/page.tsx`. */
export const docsVocabulary: DocsVocabularyDict = {
  metaTitle: "产品名词 · Codewhale 文档",
  metaDescription:
    "确切的产品名词：Fleet、Workflow、Lane、Runtime、Advisor，0.10.3 源码候选版本的 Plan / Work 与权限设置，以及请求→实际思考强度、路由来源与测量原则。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "产品名词",
  lead: "这些定义介绍 Codewhale 的产品名词，控制名称的版本范围在下方说明。执行名词与仓库中的公共事实矩阵一致。",
  executionHeading: "执行名词",
  controlHeading: "0.10.3 源码候选版本的模式与权限",
  controlLead:
    "这些控制方式适用于尚未发布的 0.10.3 源码候选版本；已发布的 v0.10.2 使用不同的控制方式。Shift+Tab 按 Ask → Accept Edits → Plan → Auto-Review → Full Access 逐档切换，Tab 用于补全文本。",
  routeHeading: "路由身份：提供商 · 模型 · 请求→实际思考强度 · 来源",
  advisoryHeading: "咨询角色",
  measurementHeading: "测量原则",
  leaderboardNote: "基准排行榜：本站没有，也不会在没有路由身份与测量工具链的情况下出现。",
  sourceNote:
    "来源文档：docs/FLEET.md、docs/MODES.md、docs/public-surface-facts.json · 名词文案来自 web/lib/content/vocabulary.ts；更新时请同步修改 docs-map.ts。",
};
