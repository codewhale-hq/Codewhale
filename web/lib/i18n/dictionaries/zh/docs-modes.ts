import type { DocsModesDict } from "../types";

export const docsModes: DocsModesDict = {
  metaTitle: "设置模式与审批 · Codewhale 文档",
  metaDescription:
    "选择 Plan 或 Work，并设置 Codewhale 何时询问审批。比较已发布的 v0.10.2 与 0.10.3 源码候选版本的控制方式。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "设置模式与审批",
  lede:
    "Plan 让你先检查情况、商定方案，再开始修改。Work 可以在你选择的权限范围内修改文件和运行命令。",
  sections: [
    {
      id: "versions",
      title: "确认你使用的版本",
      blocks: [
        { code: "codewhale --version", lang: "Shell" },
        {
          p: "最新已发布版本是 v0.10.2。在该版本中，输入框为空时按 Tab，按 Plan → Work → Operate 切换模式；按 Shift+Tab，按 Ask → Auto-Review → Full Access 切换权限。Operate 协调较大的任务，权限与 Work 相同；该版本的详细说明见 [v0.10.2 模式指南](https://github.com/codewhale-hq/CodeWhale/blob/v0.10.2/docs/MODES.md)。",
        },
        {
          note: "以下各节介绍尚未发布的 0.10.3 源码候选版本。安装最新已发布版本不会获得这些新的控制方式。",
        },
      ],
    },
    {
      id: "modes",
      title: "在源码候选版本中选择 Plan 或 Work",
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
          ],
        },
        {
          p: "输入 `/mode` 打开模式选择器，或使用以下命令。在源码候选版本中，`/mode operate` 仍是 Work 的兼容别名，并会说明 Operate 模式已退役。",
        },
        { code: "/mode plan\n/mode work", lang: "Codewhale" },
        {
          p: "Plan 成功回复后，Codewhale 会询问是否切换到带 Ask、Accept Edits 或 Auto-Review 权限的 Work，或者继续规划。选择 Work 会把已批准的计划作为后续消息发送。",
        },
      ],
    },
    {
      id: "approvals",
      title: "设置源码候选版本何时询问你",
      blocks: [
        {
          p: "按 Shift+Tab 在同一个权限拨盘上逐档切换：Ask → Accept Edits → Plan → Auto-Review → Full Access → Ask。Plan 档使用 Plan 模式，其余各档使用 Work。Tab 只补全命令、文件提及和建议，既不切换模式，也不发送文本。",
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
          note: "未保存权限设置时，新安装的源码候选版本从 Accept Edits 开始。明确保存的选择始终优先；托管审批策略可能锁定设置。通过[查看改动](/docs/review)检查修改和可用的恢复点。",
        },
        {
          p: "只要没有其他弹窗阻挡，回合运行期间也可使用权限拨盘。改动在智能体的下一步生效；已经批准的调用只有在新权限相同或更宽时才会运行。每次有效变化都会写入对话记录。",
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
  sourceNote: "源码候选版本的控制方式依据 docs/MODES.md、docs/KEYBINDINGS.md 和 docs/CONFIGURATION.md。已发布版本的链接指向 v0.10.2 标签。",
};
