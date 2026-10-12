import type { DocsPetDict } from "../types";

/** 「用 Pet 模式查看每个会话」页的简体中文词典；与 `en/docs-pet.ts` 逐段对应。 */
export const docsPet: DocsPetDict = {
  metaTitle: "用 Pet 模式查看每个会话 · Codewhale 文档",
  metaDescription:
    "Pet 模式把每个终端变成一眼可读的状态视图：一头鲸鱼显示工作中、在等你、已完成或失败，回复和智能体只需按一个键即可查看。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "用 Pet 模式查看每个会话",
  lede:
    "Pet 模式把你正在使用的终端变成一眼可读的状态视图。一头鲸鱼占据主视图，显示会话是在工作、在等你、已完成还是失败，消息框始终留在底部。在多个终端里各开一个，每个都显示自己的会话。",
  sections: [
    {
      id: "turn-on",
      title: "开启",
      blocks: [
        {
          code: "/pet on        # pet mode becomes the main view\n/pet off       # back to the standard view\n/pet status    # on or off, and the companion's identity",
          lang: "Codewhale",
        },
        {
          p: "你的选择会保存为 `pet_mode` 设置，新会话沿用上次的状态。Pet 模式只呈现你已在运行的会话：不会发起回合、不会调用模型，也不会改动草稿、模式或审批。",
        },
      ],
    },
    {
      id: "states",
      title: "看懂鲸鱼",
      blocks: [
        {
          p: "第一行和鲸鱼下方的标记同时给出标记与文字，因此不依赖颜色或动效。",
        },
        {
          rows: [
            ["○ idle", "没有任务在运行。"],
            ["● working", "回合正在运行，包括工具调用与验证。"],
            ["需要你", "会话在等你审批或回答。"],
            ["✓ Done", "上一个回合已完成。"],
            ["✕ failed", "上一个回合以失败结束。"],
          ],
          codeTerms: false,
        },
        {
          p: "状态栏保留原有的实时读数，例如智能体数量和任务面板。",
        },
      ],
    },
    {
      id: "inspect",
      title: "需要时再打开回复与智能体",
      blocks: [
        {
          p: "主视图保持安静。按 `F5` 或运行 `/pet inspect`，即可在其上打开当前回复和智能体列表。按 `Esc` 回到消息框；草稿和正在运行的回合都不受影响。",
        },
        {
          rows: [
            ["F5", "打开回复与智能体。"],
            ["Esc", "回到消息框。"],
            ["Tab", "在 Reply 与 Agents 面板之间切换。"],
            ["Enter", "在 Agents 面板中打开所选智能体的对话记录。"],
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
          p: "Pet 模式按终端独立。在每个终端里运行 `/pet on`，每头鲸鱼都跟随自己的会话，所以一个可以显示 Done，另一个仍在工作。不同会话不会混在一起。",
        },
        {
          p: "同一台机器上的所有终端共用一个本地伴生进程，它保存 Pet 的标识、声音和外观。它一次只跟随一个会话，称为来源（source）：第一个接入的已保存终端，或你运行 `/pet source` 的那个终端。其他终端仍显示各自的状态；只有鲸鱼内更细的活动细节跟随来源。",
        },
      ],
    },
    {
      id: "commands",
      title: "命令",
      blocks: [
        {
          rows: [
            ["/pet on、/pet off、/pet", "为当前终端开启、关闭或切换 Pet 模式。"],
            ["/pet inspect", "打开回复与智能体。"],
            ["/pet status", "显示模式、伴生进程标识和来源会话。"],
            ["/pet source", "让当前终端的会话成为来源。"],
            ["/pet appearance", "在浏览器中打开外观工作室，仅限本机。"],
            ["/pet window", "若已安装 macOS 的 Codewhale Pet 应用，则打开原生窗口。"],
            ["/pet sound on、/pet sound off", "开关伴生进程的声音。声音默认关闭，且需要 ffplay。"],
            ["/pet avatar", "列出头像，或选择其一。"],
            ["/pet export", "把回放保存到当前已保存会话的产物目录。"],
          ],
          codeTerms: false,
        },
      ],
    },
    {
      id: "privacy",
      title: "它能看到什么",
      blocks: [
        {
          p: "伴生进程只负责呈现。它没有智能体、提示词、提供商或执行权限，收到的是事件类型和结果，而不是提示词、回复、工具参数、结果或路径。它只监听 `127.0.0.1`，并受私有令牌保护，不会有任何内容离开你的机器。",
        },
      ],
    },
    {
      id: "limits",
      title: "已知限制",
      blocks: [
        {
          note: "目前还没有单独的“回复已就绪”提示。回合结束后鲸鱼显示 Done；按 `F5` 即可阅读回复。",
        },
        {
          note: "智能体运行期间，检查视图中的智能体列表在回合结束时刷新。状态栏的智能体数量和任务面板是实时的。",
        },
        {
          note: "`/pet window` 需要 macOS 的 Codewhale Pet 应用，声音需要 ffplay。",
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
