import type { DocsModesDict } from "../types";

export const docsModes: DocsModesDict = {
  metaTitle: "设置模式与审批 · Codewhale 文档",
  metaDescription:
    "选择 Plan、Work 或 Operate，并设置 Codewhale 何时询问审批。哪些操作可以不经询问，由你决定。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "设置模式与审批",
  lede:
    "Plan 让你先检查情况、商定方案，再开始修改。Work 和 Operate 可以在你选择的权限范围内修改文件和运行命令。哪些操作可以不经询问，由你决定。",
  sections: [
    {
      id: "versions",
      title: "确认你使用的版本",
      blocks: [
        { code: "codewhale --version", lang: "Shell" },
        {
          p: "本页描述页面顶部标明的已发布版本。仅存在于项目源码中的行为，列在页面末尾带标注的一节。",
        },
      ],
    },
    {
      id: "modes",
      title: "选择 Plan、Work 或 Operate",
      until: { feature: "modes", describesCurrentRow: true },
      blocks: [
        {
          rows: [
            [
              "Plan",
              "Codewhale 可以阅读工作区、进行策略允许的研究，但不能修改文件或运行 shell 命令。研究可能访问外部服务，会话状态也仍可保存。",
            ],
            [
              "Work",
              "Codewhale 在你设置的审批、沙箱和策略边界内阅读、修改文件和运行命令，也可以把独立任务交给其他智能体。",
            ],
            [
              "Operate",
              "工具和权限与 Work 相同，但更倾向于把多部分任务交给并行智能体，并在宣布完成前检查结果。它不会绕过 Work 所用的审批、沙箱或策略边界。",
            ],
          ],
        },
        {
          p: "输入框为空时，按 Tab 按 Plan → Work → Operate 切换模式。输入 `/mode` 打开模式选择器，或使用以下命令：",
        },
        { code: "/mode plan\n/mode work\n/mode operate", lang: "Codewhale" },
        {
          p: "Plan 成功回复后，Codewhale 会询问是否切换到带 Ask 或 Auto-Review 权限的 Work，或者继续规划。选择 Work 会把已批准的计划作为后续消息发送。",
        },
      ],
    },
    {
      id: "approvals",
      title: "设置 Codewhale 何时询问你",
      until: { feature: "permission-postures", describesCurrentRow: true },
      blocks: [
        {
          p: "按 Shift+Tab 在权限设置间切换：Ask → Auto-Review → Full Access。全新安装从 Ask 开始。权限设置与模式相互独立。",
        },
        {
          rows: [
            [
              "Ask",
              "默认设置。Codewhale 在运行命令前询问你。已保存的批准和策略规则仍然适用。",
            ],
            [
              "Auto-Review",
              "能够确证安全的调用直接执行，确定性的安全规则会阻止发布操作和破坏性的后台操作。其余被拦下的调用交给一次独立的模型审查；高风险调用和审查失败的调用会被拒绝。Codewhale 仍可能请你作出用户选择。",
            ],
            [
              "Full Access",
              "命令无需审批提示即可访问这台计算机上的任何内容。托管策略与仓库规则仍然适用。",
            ],
          ],
        },
        {
          note: "通过[查看改动](/docs/review)检查修改和可用的恢复点。",
        },
      ],
    },
    {
      id: "prompt",
      title: "回应审批提示",
      blocks: [
        {
          p: "先阅读卡片上的工具调用和选项，再作答。普通工具审批卡片在提供对应选项时支持以下按键：",
        },
        {
          rows: [
            ["y", "只允许这一次调用。"],
            ["a", "在提供会话批准选项时，选择卡片显示的该选项。"],
            ["p", "在提供精确仓库允许规则时，保存卡片显示的该规则。"],
            ["n", "拒绝调用，并让 Codewhale 得知调用被拒绝。"],
            ["Esc", "在当前回合自己的审批卡片上停止回合；在子智能体的卡片上隐藏卡片，审批仍保持待处理状态。"],
          ],
          codeTerms: true,
        },
        {
          p: "Workflow 计划卡片有不同的选项，包括编辑或取消计划。请按照该卡片上的标签操作，而非套用上面的普通工具快捷键。",
        },
        {
          p: "新审批卡片默认选中“拒绝”，所以除非你更改了设置，按 Enter 会拒绝调用。审批提示默认无限等待；可以在用户配置中设置超时，让未答复的提示在 300 秒后被拒绝：",
        },
        {
          code: `[approval]\ndefault_selection = "deny"\ntimeout_seconds = 300`,
          lang: "config.toml",
        },
        {
          p: "默认用户配置文件是 `~/.codewhale/config.toml`；如果使用自定义配置路径，请参阅[修改设置](/docs/configuration)。提示设置决定已要求审批的卡片如何响应，不会向工具授予权限。",
        },
      ],
    },
  ],
  sourceGroups: [
    {
      claim: { feature: "modes", describesCurrentRow: true },
      sections: [
        {
          id: "source-modes",
          title: "Plan 和 Work",
          blocks: [
            {
              p: "Plan 和 Work 是仅有的两种模式。`/mode operate` 仍可作为 Work 的别名使用，并会说明 Operate 已退役。Tab 只补全命令、文件提及和建议，既不切换模式，也不发送文本。",
            },
            { code: "/mode plan\n/mode work", lang: "Codewhale" },
            {
              p: "Plan 成功回复后，Codewhale 会询问是否切换到带 Ask、Accept Edits 或 Auto-Review 权限的 Work，或者继续规划。",
            },
          ],
        },
      ],
    },
    {
      claim: { feature: "permission-postures", describesCurrentRow: true },
      sections: [
        {
          id: "source-approvals",
          title: "一个权限拨盘",
          blocks: [
            {
              p: "按 Shift+Tab 在同一个权限拨盘上逐档切换：Ask → Accept Edits → Plan → Auto-Review → Full Access → Ask。Plan 档使用 Plan 模式，其余各档使用 Work。",
            },
            {
              rows: [
                [
                  "Ask",
                  "Codewhale 在每次修改文件或运行命令前询问你，包括工作区内的修改。已保存的批准和策略规则仍然适用。",
                ],
                [
                  "Accept Edits",
                  "工作区 Git 工作树内的普通文件修改可以不经提示执行。命令、网络和 MCP 操作仍会询问；修改敏感文件、工作区外的文件或不属于 Git 仓库的文件夹也会询问。ACP 会话将此设置按 Ask 处理。",
                ],
              ],
            },
            {
              note: "未保存权限设置时，新安装从 Accept Edits 开始。明确保存的选择始终优先；托管审批策略可能锁定设置。",
            },
            {
              p: "只要没有其他弹窗阻挡，回合运行期间也可使用权限拨盘。改动在智能体的下一步生效；已经批准的调用只有在新权限相同或更宽时才会运行。每次有效变化都会写入对话记录。",
            },
          ],
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/review",
      label: "查看改动",
      note: "检查会话改动，并恢复可用的文件快照。",
    },
    {
      href: "/docs/sandbox",
      label: "限制命令的访问范围",
      note: "了解各平台上操作系统实际执行的限制。",
    },
    {
      href: "/docs/subagents",
      label: "并行运行智能体",
      note: "委派独立任务，并查看每个智能体的进度。",
    },
  ],
  sourceNote: "依据已发布版本标签下的 docs/MODES.md、docs/KEYBINDINGS.md 和 docs/CONFIGURATION.md；源码一节依据 main。",
};
