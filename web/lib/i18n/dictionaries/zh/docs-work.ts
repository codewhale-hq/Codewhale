import type { DocsWorkDict } from "../types";

/** 「跟踪进度」页的简体中文词典；与 `en/docs-work.ts` 逐段对应。 */
export const docsWork: DocsWorkDict = {
  metaTitle: "跟踪进度 · Codewhale 文档",
  metaDescription:
    "在任务面板中跟踪多步骤任务：目标、待办 列表和智能体。设置一个跨回合持续的目标，并在不丢失进度的情况下把工作交给新的会话。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "跟踪进度",
  lede:
    "当一个任务需要不止一步时，Codewhale 会维护一份 待办 列表，并把它显示在任务面板里，旁边是目标和各个智能体。对话记录一路往下滚动的同时，你始终能看到哪些已完成、哪些正在进行、还剩哪些。",
  sections: [
    {
      id: "workbar",
      title: "看懂任务面板",
      blocks: [
        {
          p: "任务面板默认位于输入框下方，显示当前目标、待办 列表，以及为本次会话工作的智能体。已完成的条目会以“已完成”的状态保留，而不是消失。选中某一行，或在该行上按 Enter，即可打开详情。",
        },
        {
          rows: [
            ["pending", "尚未开始。"],
            ["in progress", "Codewhale 正在做的事——一次只有一项。"],
            ["completed", "已完成。"],
            ["cancelled", "已放弃，但仍保留在列表中，让你知道它被放弃了。"],
          ],
        },
        { p: "移动任务面板，或更改它显示的内容：" },
        { code: "/workbar left\n/workbar bottom --save\n/workbar off", lang: "Codewhale" },
        {
          p: "可选位置有 `bottom`、`top`、`left`、`right` 和 `off`。加上 `--save` 会在以后的会话中沿用你的选择。",
        },
      ],
    },
    {
      id: "goal",
      title: "设定跨回合持续的目标",
      blocks: [
        {
          p: "目标会让一个任务始终保持在视线之内，直到它完成。你可以自己设定；当你提出一个明确的完成条件时（比如“直到测试全部通过”），Codewhale 也可能自行设定，并用一行提示告诉你。控制权始终在你手里：",
        },
        {
          code: `/goal make the CLI tests pass on Windows budget: 200000
/goal            # show progress
/goal pause
/goal resume
/goal done
/goal clear`,
          lang: "Codewhale",
        },
        {
          p: "可选的 `budget:` 是这个目标的 token 上限。设定目标不会改变你的模式、审批设置或模型。",
        },
      ],
    },
    {
      id: "relay",
      title: "在新会话中继续",
      blocks: [
        {
          p: "会话变长之后，`/relay` 会为新线程写一份交接说明，其中原样附上当前的 待办 列表，让下一个会话从你真正所处的位置接着做，而不是从一段概括开始。可以加上一个重点来引导交接内容：",
        },
        { code: "/relay finish the Windows test fixes", lang: "Codewhale" },
        {
          p: "带着父会话上下文启动的智能体也会收到同一份列表，因此它们同样知道哪些已经做完。",
        },
      ],
    },
    {
      id: "one-list",
      title: "弄清哪份列表才算数",
      blocks: [
        {
          p: "进度列表只有一份：待办。Codewhale 写出来的计划——做法、风险和验证方式——说明的是它打算怎么做，并不跟踪进度，任务面板也不会把它显示成第二份列表。",
        },
        {
          p: "任务面板里的列表是给你看的。Codewhale 是通过对话中的普通结果看到自己的更新，这份列表不会在每一步都重新发送给模型。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/subagents",
      label: "并行运行智能体",
      note: "把彼此独立的 待办 条目交给智能体。",
    },
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "选择模式，并决定 Codewhale 何时询问审批。",
    },
    {
      href: "/docs/review",
      label: "查看改动",
      note: "检查每个已完成条目背后的实际改动。",
    },
  ],
  sourceNote:
    "来源文档：docs/GUIDE.md、docs/MODES.md、docs/TOOL_SURFACE.md、docs/TOOL_LIFECYCLE.md · 修改时同步更新 docs-map.ts。",
};
