import type { DocsGuideDict } from "../types";

/** 「开始第一个任务」页的简体中文词典；四个步骤见 `web/lib/content/getting-started.ts`。 */
export const docsGuide: DocsGuideDict = {
  metaTitle: "开始第一个任务 · Codewhale 文档",
  metaDescription:
    "安装 Codewhale，连接模型，然后在你的项目里交给它一项任务。之后如果需要多个模型和角色，再配置 Fleet。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  overviewTitle: "开始第一个任务",
  overviewLead:
    "你需要一个终端、一个已有的项目文件夹，以及提供商密钥或正在运行的本地模型；请把 /path/to/your/project 替换为你的文件夹路径。安装 Codewhale、连接模型，再核对它对真实文件的说明，完成后可以按需配置 Fleet。",
  sessionTitle: "看一次真实会话",
  sessionLead: "查看一项任务从首次请求到完成的全过程。",
  nextTitle: "下一步",
  sourceNote:
    "来源文档：docs/GUIDE.md、docs/INSTALL.md、docs/KEYBINDINGS.md · 修改时同步更新 docs-map.ts。",
};
