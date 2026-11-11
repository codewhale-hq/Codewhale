import type { DocsPageDict } from "../types";

export const docsSkills: DocsPageDict = {
  metaTitle: "管理技能 · Codewhale 文档",
  metaDescription: "查找、安装并信任 SKILL.md 指令包，控制用户与模型调用，并导入其他编码智能体的兼容技能文件夹。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "管理可复用技能",
  lede: "技能是 SKILL.md 指令包。发现兼容文件夹后，先审阅来源与信任设置，再使用技能。",
  sections: [
    {
      "id": "manager",
      "title": "打开技能管理器",
      "blocks": [
        {
          "code": "/skills\n/skills manage\n/skills inspect\n/skill <name>",
          "lang": "Codewhale"
        },
        {
          "p": "管理器打开时不访问网络，并显示 Codewhale 自有目录。启用兼容目录扫描即可检查外部文件夹；导入目标始终是 Codewhale 自有目录。"
        },
        { note: "`/skill <name>` 为下一回合启用指令；启用本身不等于模型已经回复，也不能证明模型遵循了指令。插件教程提供一个问候标记，可在你明确发送消息后核对。" }
      ]
    },
    {
      "id": "install",
      "title": "安装到自有目录",
      "blocks": [
        {
          "p": "按照[双文件插件教程](/docs/plugin-authoring)编写并分享技能。"
        },
        {
          "code": "/skill install --project <spec>\n/skill install --global <spec>\n/skill trust --project <name>",
          "lang": "Codewhale"
        },
        {
          "p": "自有目录包括项目中的 .codewhale/skills 与全局 ~/.codewhale/skills。兼容的 .agents/skills 和 .claude/skills 目录只作为只读导入来源。"
        },
        {
          "note": "只有信任工作区后才会加载项目技能。配置的外部 skills_dir 始终只读；选择文件夹不会授予修改权限。"
        }
      ]
    },
    {
      "id": "invocation",
      "title": "选择谁可以加载技能",
      "blocks": [
        {
          "p": "Frontmatter 中的 disable-model-invocation: true 会将技能移出模型发现列表。user-invocable: false 会从用户菜单中隐藏技能并拒绝显式激活；两者同时设置会禁用两条路径。"
        },
        {
          "p": "技能正文在加载时才进入上下文。精简目录只包含有界的名称、别名和调用元数据。"
        }
      ]
    }
  ],
  next: [
    {
      "href": "/docs/claude-code",
      "label": "从其他编码智能体带来你的设置",
      "note": "带入兼容技能并审阅设置导入方案。"
    },
    {
      "href": "/plugins",
      "label": "添加插件",
      "note": "通过审阅与信任流程安装支持的插件包。"
    }
  ],
  sourceNote: "来源：docs/SKILLS.md 与 docs/CLAUDE_PLUGIN_COMPAT.md。"
};
