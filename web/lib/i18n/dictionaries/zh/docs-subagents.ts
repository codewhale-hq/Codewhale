import type { DocsSubagentsDict } from "../types";

/** 「并行运行智能体」页的简体中文词典；与 `en/docs-subagents.ts` 逐段对应。 */
export const docsSubagents: DocsSubagentsDict = {
  metaTitle: "并行运行智能体 · Codewhale 文档",
  metaDescription:
    "让 Codewhale 把任务中彼此独立的部分交给子智能体，为它们选择角色，把它们的修改放在各自的工作树里，并随时查看进度。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "并行运行智能体",
  lede:
    "Codewhale 可以把一块聚焦的工作交给子智能体，自己继续往下做。当一个任务能拆成彼此独立的几部分——梳理代码库、审查改动、跑测试——时，用这种方式让它们同时进行。",
  sections: [
    {
      id: "ask",
      title: "直接提出来",
      blocks: [
        {
          p: "在 Work 中，用平常的话请 Codewhale 把任务中独立的部分交给子智能体。请说明你想怎么拆分：",
        },
        {
          code: `Use three explore agents in parallel: one maps the auth code,
one maps the billing code, one lists the tests that cover both.
Then summarize what would break if we changed the session format.`,
          lang: "提示词",
        },
        {
          p: "每个子智能体都在后台启动，完成后回报结果。你的输入框始终可用，主回合会接着处理这些结果。",
        },
      ],
    },
    {
      id: "roles",
      title: "选择角色",
      blocks: [
        {
          p: "角色决定了子智能体如何做事。你可以在请求里点名，也可以让 Codewhale 自己选。子智能体拿到的权限永远不会超过你当前会话的权限。",
        },
        {
          rows: [
            ["general", "按任务说明做事，可以修改文件、运行命令。默认角色。"],
            ["explore", "只读。快速梳理相关代码——比如“找出这个函数的所有调用方”。"],
            ["planner", "设计方案，不做任何改动。"],
            ["reviewer", "阅读并评估一处改动，为每个发现标注严重程度。"],
            ["implement", "用最小的修改落实一项具体改动。"],
            ["test", "运行测试和检查，报告通过或失败。不修改代码。"],
            ["advisor", "针对需要判断的问题，给出简短而审慎的第二意见。不运行命令。"],
            ["custom", "只能使用你列出的工具，适合权限需要严格收紧的任务。"],
          ],
          codeTerms: true,
        },
        {
          p: "想让某个角色固定使用某个模型，用 `/fleet setup` 保存——见[运行工作流](/docs/fleet)。",
        },
      ],
    },
    {
      id: "worktrees",
      title: "让并行修改互不干扰",
      blocks: [
        {
          p: "如果两个子智能体会修改同一个仓库，请让它们各自在自己的工作树里工作。Codewhale 会在你的仓库旁边的 `.codewhale-worktrees/` 下，为该智能体创建新的 git 工作树和分支；在你合并之前，你的工作副本保持干净。",
        },
        {
          p: "工作树只是隔离，不代表授权：写入者仍需可写角色和允许修改的路径。如果新写入声明与同一工作副本中的活动声明重叠，宿主会拒绝新申请，不会回退或暂停已在工作的写入者。",
        },
      ],
    },
    {
      id: "watch",
      title: "查看进度并回应它们",
      blocks: [
        { code: "/subagents", lang: "Codewhale" },
        {
          p: "`/subagents`（与 `/fleet workers` 是同一个视图）会列出挂在本次会话上的智能体，以及每个智能体正在做什么。选中一个即可阅读它的对话记录。",
        },
        {
          p: "子智能体遵循你的[审批设置](/docs/modes)，并可能受到更严格的限制。在 Ask 下，需要审批的调用会出现在你的会话中，智能体等待回应。Auto-Review 按策略处理工具审批，并在适用时进行独立审查；被策略拦截或审查失败的调用会被拒绝。可在子智能体对话记录中查看决定和拒绝原因。操作系统及已连接服务的权限仍可能需要你处理。",
        },
      ],
    },
    {
      id: "limits",
      title: "了解上限",
      blocks: [
        {
          rows: [
            ["同时运行", "默认 64 个。可在 `~/.codewhale/config.toml` 中设置 `max_subagents`（最多 128）。"],
            ["排队加运行", "默认最多接纳 1,024 个，可用 `[subagents] max_admitted` 配置。已接纳的智能体等待执行位置；超过接纳上限的新启动请求会被拒绝。"],
            ["嵌套", "默认没有产品层级深度上限。可用 `[subagents] max_depth` 设置你选择的上限；子智能体请求不能放宽继承的限制。"],
          ],
        },
        {
          p: "这些是容量控制，不是目标，请明确每个智能体的范围。持久的 [Fleet 运行](/docs/fleet)会在管理进程退出后保留记录，但恢复需要明确操作：核对过期租约、检查结果，再决定是否重启某个智能体。机器休眠时不会继续执行。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/fleet",
      label: "运行工作流",
      note: "把反复使用的计划写成工作流，分阶段执行，每次运行都有记录。",
    },
    {
      href: "/docs/work",
      label: "跟踪进度",
      note: "To-do 列表如何显示已完成、进行中和剩余的工作。",
    },
    {
      href: "/docs/review",
      label: "查看改动",
      note: "在保留之前，先检查这些智能体改了什么。",
    },
  ],
  sourceNote: "来源文档：docs/SUBAGENTS.md、docs/FLEET.md、docs/MODES.md · 修改时同步更新 docs-map.ts。",
};
