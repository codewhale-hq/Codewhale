import type { DocsPetDict } from "../types";

/** 「用鲸鱼视图查看每个会话」页的简体中文词典；与 `en/docs-pet.ts` 逐段对应。 */
export const docsPet: DocsPetDict = {
  metaTitle: "用鲸鱼视图查看每个会话 · Codewhale 文档",
  metaDescription:
    "/whale 把每个终端变成一眼可读的状态视图：一头由点组成的鲸鱼显示工作中、在等你、已完成或失败，回复和智能体只需按一个键即可查看。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "用鲸鱼视图查看每个会话",
  lede:
    "`/whale` 把你正在使用的终端变成一眼可读的状态视图。一头由点组成的鲸鱼占据主视图，显示会话是在工作、在等你、已完成还是失败，消息框始终留在底部。每个终端根据自己的会话绘制自己的鲸鱼，此外不运行任何别的东西。",
  sections: [
    {
      id: "turn-on",
      title: "开启",
      blocks: [
        { note: "本页描述的是 Codewhale 0.10.5。命令现在是 `/whale`；`/pet` 仍然可以运行它，但不再列出。更早的版本会运行一个共享的伴随进程，提供声音、回放导出、外观工作室和其他视图。这些已在 0.10.5 中移除，见下文“0.10.5 移除了什么”。" },
        {
          code: "/whale         # turn the whale view on or off\n/whale on      # on, without toggling\n/whale off     # back to the standard view",
          lang: "Codewhale",
        },
        {
          p: "视图的开关会保存为 `pet_mode` 设置，新会话沿用上次的状态。鲸鱼视图只呈现你已在运行的会话：不会发起回合、不会调用模型，也不会改动草稿、模式或审批。",
        },
      ],
    },
    {
      id: "states",
      title: "看懂鲸鱼",
      blocks: [
        {
          p: "第一行和鲸鱼下方的标记用一个符号加一个词给出状态，所以不靠颜色或动效也能读懂。",
        },
        {
          rows: [
            ["○ idle", "没有任务在运行。"],
            ["● working", "一个回合正在运行，包括工具和验证。"],
            ["needs you", "会话在等待审批或回答。"],
            ["✓ Done", "上一个回合已完成。"],
            ["✕ failed", "上一个回合以失败结束。"],
          ],
          codeTerms: false,
        },
        {
          p: "回合运行时，鲸鱼的点会变成正在进行的工作本身，例如命令是扳手、编辑是铅笔、阅读是书本，然后回到鲸鱼。状态栏保留实时读数，例如智能体数量。",
        },
      ],
    },
    {
      id: "inspect",
      title: "需要时打开回复和智能体",
      blocks: [
        {
          p: "主视图保持安静。按 `F5` 在其上打开当前回复和智能体列表。按 `Esc` 回到消息框；草稿和正在运行的回合都不受影响。",
        },
        {
          rows: [
            ["F5", "打开回复和智能体。"],
            ["Esc", "回到消息框。"],
            ["Tab", "在“回复”和“智能体”两个窗格之间切换。"],
            ["Enter", "在“智能体”窗格中打开所选智能体的对话记录。"],
            ["c", "复制最近一条已完成的回复，与 `/copy` 相同。"],
          ],
          codeTerms: true,
        },
      ],
    },
    {
      id: "terminals",
      title: "在多个终端中使用",
      blocks: [
        {
          p: "鲸鱼视图按终端生效。在每个终端运行 `/whale`，每头鲸鱼都跟随自己的会话，所以一个可以显示 Done，另一个仍在工作。会话之间绝不混用，终端之间也不通信：没有后台进程、没有端口，也没有共享状态。",
        },
      ],
    },
    {
      id: "commands",
      title: "命令",
      blocks: [
        {
          rows: [
            ["/whale, /whale on, /whale off", "为当前终端切换、开启或关闭鲸鱼视图。"],
            ["/whale full", "与 `/whale` 相同；保留这个词是为了让命令与桌面应用中的写法一致。"],
            ["/whale still", "让鲸鱼保持静止，或让它重新动起来。"],
            ["/whale name <text>", "给鲸鱼起名，最多 32 个字符。名字会替换视图第一行中的 Codewhale。"],
            ["/whale color <#rrggbb>", "给鲸鱼上色。`/whale color default` 恢复它自己的颜色。"],
          ],
          codeTerms: false,
        },
        {
          p: "名字、颜色和保持静止只在当前终端关闭前有效，不会保存。",
        },
      ],
    },
    {
      id: "privacy",
      title: "它能看到什么",
      blocks: [
        {
          p: "鲸鱼在终端进程内绘制。它只从会话读取一小组事件类型、标识和结果，例如“开始了一次读取”或“回合已完成”。提示文本、推理、工具名称、参数、结果和路径都不会被读取，也不会有任何内容写入磁盘或发送到任何地方。",
        },
      ],
    },
    {
      id: "removed",
      title: "0.10.5 移除了什么",
      blocks: [
        {
          p: "共享的伴随进程（`codewhale pet serve`）已移除，只有它才能提供的功能也一并移除：",
        },
        {
          list: [
            "声音（`/pet sound`、`F6`）。",
            "回放录制和 `/pet export`。",
            "浏览器外观工作室（`/pet appearance`、`F8`）和 `/pet style`。",
            "来源选择（`/pet source`）和 `/pet status`。",
            "其他视图、视图切换、`/pet avatar`、`/pet action`、`/pet view`，以及终端中的头像包。",
            "`/pet inspect`；请使用 `F5`。",
          ],
        },
        {
          p: "旧版本留在 `~/.codewhale/pet-shared` 中的文件不会被读取，也不会被删除；你可以自行删除该文件夹。",
        },
      ],
    },
    {
      id: "limits",
      title: "已知限制",
      blocks: [
        {
          note: "目前还没有单独的“回复已就绪”提示。回合结束后鲸鱼会显示 Done；按 `F5` 阅读回复。",
        },
        {
          note: "智能体运行期间，检查器中的智能体列表在回合结束时才刷新。状态栏中的智能体数量是实时的。",
        },
        {
          note: "显示鲸鱼视图时工作栏会隐藏。用 `/whale` 关闭视图即可使用它。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/work",
      label: "跟踪进度",
      note: "在工作栏中跟踪目标、待办列表和子智能体。",
    },
    {
      href: "/docs/subagents",
      label: "并行运行 Agent",
      note: "把任务中独立的部分交给子 Agent。",
    },
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "决定会话何时等你确认。",
    },
  ],
  sourceNote:
    "来源文档：docs/PET_MODE.md · 修改时同步更新 docs-map.ts。",
};
