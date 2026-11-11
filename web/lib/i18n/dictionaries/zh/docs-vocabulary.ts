import type { DocsVocabularyDict } from "../types";

/** Chinese dictionary for `app/[locale]/docs/vocabulary/page.tsx`. */
export const docsVocabulary: DocsVocabularyDict = {
  metaTitle: "产品名词 · Codewhale 文档",
  metaDescription:
    "确切的产品名词：Fleet、Workflow、Lane、Runtime、Advisor，Plan、Work、Operate 与权限设置，以及请求→实际思考强度、路由来源与测量原则。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "产品名词",
  lead: "这些定义介绍 Codewhale 的产品名词，先写已发布版本的控制方式，仅存在于源码的控制方式单独放在带标注的一节。执行名词与仓库中的公共事实矩阵一致。",
  executionHeading: "执行名词",
  controlHeading: "模式与权限",
  controlLead:
    "输入框为空时，Tab 按 Plan → Work → Operate 切换模式；Shift+Tab 按 Ask → Auto-Review → Full Access 切换权限设置。哪些操作可以不经询问，由你决定。",
  sourceControlHeading: "源码中的模式与权限",
  routeHeading: "路由身份：提供商 · 模型 · 请求→实际思考强度 · 来源",
  advisoryHeading: "咨询角色",
  measurementHeading: "测量原则",
  leaderboardNote: "基准排行榜：本站没有，也不会在没有路由身份与测量工具链的情况下出现。",
  sourceNote:
    "来源文档：docs/FLEET.md、已发布版本标签下的 docs/MODES.md、docs/public-surface-facts.json · 名词文案来自 web/lib/content/vocabulary.ts；更新时请同步修改 docs-map.ts。",
};
