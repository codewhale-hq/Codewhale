import type { DocsModesDict } from "../types";

/** 「设置模式与审批」页的简体中文词典；与 `en/docs-modes.ts` 逐段对应。 */
export const docsModes: DocsModesDict = {
  metaTitle: "设置模式与审批 · Codewhale 文档",
  metaDescription:
    "用 Plan、Work、Operate 选择工作类型，用 Ask、Auto-Review、Full Access 决定 Codewhale 多久停下来问你一次。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "设置模式与审批",
  lede:
    "有两个相互独立的开关决定 Codewhale 能自己做什么。模式决定工作类型——只看、动手改，还是统筹协调；审批设置决定它什么时候停下来问你。两者都可以在回合之间随时用键盘切换。",
  sections: [
    {
      id: "modes",
      title: "选择模式",
      blocks: [
        {
          rows: [
            [
              "Plan",
              "只看、只规划。Codewhale 可以阅读工作区、查资料，但不能修改文件，也不能运行 shell 命令。适合先弄懂代码库，或先商定做法。",
            ],
            [
              "Work",
              "日常编码。Codewhale 会阅读、修改文件并运行命令，范围受你选择的审批设置约束。这是默认模式。",
            ],
            [
              "Operate",
              "面向更大的目标。权限与 Work 相同，但 Codewhale 会先拆出有名字的步骤，把彼此独立的步骤并行交给子智能体，并在汇报完成之前逐一核对结果。",
            ],
          ],
        },
        {
          p: "输入框为空时按 Tab，可以按 Plan → Work → Operate 的顺序切换；也可以输入 `/mode` 打开选择器，或者直接切换：",
        },
        { code: "/mode plan\n/mode work\n/mode operate", lang: "Codewhale" },
        {
          p: "你选中的模式也会成为下次启动会话时的模式。回合进行中 Codewhale 不接受切换模式，请先按 Esc 停止当前回合。",
        },
      ],
    },
    {
      id: "approvals",
      title: "决定它什么时候问你",
      blocks: [
        {
          p: "按 Shift+Tab 可以按 Ask → Auto-Review → Full Access 的顺序切换。无论这里选什么，Plan 模式始终是只读的。",
        },
        {
          rows: [
            [
              "Ask",
              "默认设置。工作区内的文件修改会直接写入，并以 diff 的形式展示给你；shell 命令和其他有实际影响的工具会停下来等你批准。",
            ],
            [
              "Auto-Review",
              "自动审查工具调用，但仍可能请你作出选择。能确证安全的调用直接执行；发布类操作和破坏性的后台操作一律拦截；其余调用交给一次独立的模型审查。高风险调用和审查失败的调用会被拒绝，而不是执行。",
            ],
            [
              "Full Access",
              "不弹出审批提示。仓库规则和托管策略该拦的照样会拦。只在你信任的工作区里使用。",
            ],
          ],
        },
        {
          note: "Ask 模式下，工作区内的文件修改不会事先询问。开始之前请先提交或暂存重要的改动，并通过[查看改动](/docs/review)检查或回滚修改。",
        },
      ],
    },
    {
      id: "prompt",
      title: "回应审批提示",
      blocks: [
        { p: "Codewhale 停在审批提示时，会显示将要执行的完整命令。按一个键作答：" },
        {
          rows: [
            ["y", "只允许这一次调用。"],
            ["a", "在本次会话剩余时间内都允许。"],
            ["n", "拒绝。Codewhale 会得知这次调用被拒绝了。"],
            ["Esc", "停止整个回合。"],
          ],
          codeTerms: true,
        },
        {
          p: "新提示默认选中的是“拒绝”，所以没看清就按 Enter 会拒绝这次调用。如果想改变这一点，或者让等待过久的提示自动拒绝，可以在 `~/.codewhale/config.toml` 中设置：",
        },
        {
          code: `[approval]
default_selection = "allow_once"   # default: "deny"
timeout_seconds = 300              # default: wait indefinitely`,
          lang: "config.toml",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/review",
      label: "查看改动",
      note: "查看本次会话的 diff，把文件回滚到之前某个回合。",
    },
    {
      href: "/docs/sandbox",
      label: "限制命令的访问范围",
      note: "批准不等于沙箱。了解各平台上操作系统实际限制了什么。",
    },
    {
      href: "/docs/subagents",
      label: "并行运行智能体",
      note: "Operate 如何处理彼此独立的步骤，以及怎样查看进度。",
    },
  ],
  sourceNote: "来源文档：docs/MODES.md、docs/INSTALL.md §10、docs/CONFIGURATION.md · 修改时同步更新 docs-map.ts。",
};
