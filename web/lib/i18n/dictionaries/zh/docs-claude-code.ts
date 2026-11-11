import type { DocsPageDict } from "../types";

export const docsClaudeCode: DocsPageDict = {
  metaTitle: "从其他编码智能体带来你的设置 · Codewhale 文档",
  metaDescription: "保留你已有的项目说明与技能文件夹，并在应用前审阅设置和 MCP 导入方案。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "从其他编码智能体带来你的设置",
  lede: "保留兼容的项目说明与技能文件夹，同时选择你自己的模型连接。应用设置和插件导入前，请先审阅。",
  sections: [
    {
      "id": "instructions",
      "title": "保留项目说明",
      "blocks": [
        {
          "p": "AGENTS.md 是项目说明的规范文件。如果项目只有 `CLAUDE.md` 或 `.claude/instructions.md`，Codewhale 会把它们作为兼容回退读取；两者都不会覆盖 Codewhale 安全控制。"
        },
        {
          "p": "普通 `.claude/skills/<name>/SKILL.md` 文件夹通过常规[技能注册表](/docs/skills)发现，并受工作区信任与调用控制约束。"
        }
      ]
    },
    {
      "id": "settings",
      "title": "审阅设置导入",
      "blocks": [
        {
          "code": "/import-claude",
          "lang": "Codewhale"
        },
        {
          "p": "该命令读取已有 `.claude` 文件夹中的设置，生成可审阅报告与尚未应用的配置包。MCP 服务器使用现有的导入同意流程；权限和钩子需要单独审阅。"
        },
        {
          "note": "疑似密钥和未知环境变量的值不会被导入或回显。运行方案不会启用服务器，也不会应用权限。"
        }
      ]
    },
    {
      "id": "plugins",
      "title": "安装支持的插件子集",
      "blocks": [
        {
          "code": "/plugin install <spec>",
          "lang": "Codewhale"
        },
        {
          "p": "声明式 `.claude-plugin/plugin.json` 子集支持技能、命令、智能体配置与 MCP 声明。Codewhale 的审阅哈希、显式信任与启用流程仍然适用。"
        },
        {
          "note": "Codewhale 不会运行其他智能体的插件运行时。钩子、插件 LSP 声明、构建步骤与 TypeScript 智能体不在该导入器的支持范围内。"
        },
        {
          "p": "[阅读兼容性限制](https://github.com/codewhale-hq/CodeWhale/blob/v0.10.2/docs/CLAUDE_PLUGIN_COMPAT.md)。在[模型设置](/models)中选择凭据与端点。"
        }
      ]
    }
  ],
  next: [
    {
      "href": "/docs/skills",
      "label": "管理技能",
      "note": "导入前检查兼容目录。"
    },
    {
      "href": "/docs/auth",
      "label": "连接提供商",
      "note": "使用你选择的托管模型或本地模型。"
    }
  ],
  sourceNote: "来源：docs/CONFIGURATION.md、docs/CLAUDE_PLUGIN_COMPAT.md 与 docs/features.toml 中的 claude-settings-import 条目（均取自已发布版本标签）。"
};
