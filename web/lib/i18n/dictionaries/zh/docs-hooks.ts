import type { DocsHooksDict } from "../types";

/** 「在事件发生时运行命令」页的简体中文词典；与 `en/docs-hooks.ts` 逐段对应。 */
export const docsHooks: DocsHooksDict = {
  metaTitle: "在事件发生时运行命令 · Codewhale 文档",
  metaDescription:
    "在会话开始、工具调用之前、回合结束或 Codewhale 等你回应时运行你自己的脚本——用来补充上下文、执行规则或接收通知。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "在事件发生时运行命令",
  lede:
    "钩子会在 Codewhale 会话的特定时刻运行你指定的命令。可以用它拦下危险的命令、给消息补充上下文、记录发生了什么，或者在 Codewhale 等你回应时提醒你。",
  sections: [
    {
      id: "first-hook",
      title: "添加第一个钩子",
      blocks: [
        { p: "钩子写在 `~/.codewhale/config.toml` 中。下面这个钩子会在每次会话开始时打印一行文字：" },
        {
          code: `[hooks]
enabled = true

[[hooks.hooks]]
name = "announce"
event = "session_start"
command = "echo 'Codewhale session started'"`,
          lang: "config.toml",
        },
        {
          p: "启动一个会话，运行 `/hooks`，就能看到所有已配置的钩子、钩子总开关是否打开，以及被拒绝加载的条目。`/hooks events` 会列出所有事件名称。",
        },
      ],
    },
    {
      id: "gate",
      title: "在命令执行前拦下它",
      blocks: [
        {
          p: "`tool_call_before` 钩子会在每次工具调用执行之前看到它，并可以放行、拒绝，或强制弹出审批提示。这个 POSIX shell 示例拒绝所有名为 `bash` 的工具调用，而不是猜测命令字符串会执行什么。",
        },
        {
          code: `#!/bin/sh
printf '%s\\n' '{"decision":"deny","reason":"Shell calls are blocked by this hook."}'`,
          lang: "deny-shell.sh",
        },
        { p: "创建 `~/.codewhale/hooks/`，把脚本保存为其中的 `deny-shell.sh`，然后在 macOS 或 Linux 上赋予可执行权限：" },
        { code: "chmod +x ~/.codewhale/hooks/deny-shell.sh", lang: "终端" },
        {
          code: `[[hooks.hooks]]
name = "deny-shell"
event = "tool_call_before"
command = "~/.codewhale/hooks/deny-shell.sh"
condition = { type = "tool_name", name = "bash" }
continue_on_error = false`,
          lang: "config.toml",
        },
        {
          p: "钩子从环境变量中读取这次调用，并在标准输出上用 JSON 作答：`allow`、`deny` 或 `ask`，还可以附带 `reason`、改写后的输入（`updatedInput`）或给模型的补充上下文（`additionalContext`）。退出码 2 一律表示拒绝。多个钩子同时作答时，拒绝优先于询问，询问优先于放行。",
        },
        { note: "这个示例只覆盖名称恰好为 `bash` 的工具，不会阻止其他工具、外部客户端或你自行运行的命令。通过子字符串匹配强制推送标志不是 shell 安全边界；更广泛的限制应使用执行策略与操作系统沙箱。" },
        {
          note: "只要当前权限设置会询问或审查，`ask` 就会强制弹出提示。Full Access 不增加审批提示，因此 `ask` 在该设置下不会新增提示。",
        },
      ],
    },
    {
      id: "events",
      title: "选择触发时机",
      blocks: [
        {
          p: "有三个事件能改变接下来发生的事，其余事件只做观察：它们的输出会被忽略，失败也只会产生警告。",
        },
        {
          rows: [
            ["message_submit", "在你的消息发给模型之前。可以替换文本，或阻止发送。"],
            ["tool_call_before", "在每次工具调用之前。可以放行、拒绝、询问、改写输入或补充上下文。"],
            ["shell_env", "在每条 shell 命令运行之前。可以添加环境变量。"],
            ["session_start / session_end", "会话打开时，或正常关闭时。"],
            ["turn_end", "回合结束后，附带状态、耗时和 token 用量。"],
            ["tool_call_after", "每个工具结果返回之后，有退出码时会附带退出码。"],
            ["waiting_for_user", "Codewhale 开始等待你的审批、回答或暂停中的目标时。"],
            ["session_idle / session_busy", "会话闲下来，或重新开始工作时。"],
            ["session_error / on_error", "回合最终失败时，或发生任何错误、工具失败时。"],
            ["mode_change", "切换模式时。"],
            ["subagent_spawn / subagent_complete", "子智能体启动或结束时。"],
          ],
          codeTerms: true,
        },
        {
          p: "`condition` 可以缩小钩子的触发范围：按工具名（支持 `*` 通配）、工具类别、模式或退出码，也可以用 `all` 和 `any` 组合。永远不可能与所属事件匹配的条件，会在加载配置时直接被拒绝——这样你以为已经生效的拦截规则，不会悄无声息地失效。",
        },
      ],
    },
    {
      id: "options",
      title: "设置超时与失败行为",
      blocks: [
        {
          rows: [
            ["timeout_secs", "钩子最长可运行多久。默认 30 秒。"],
            ["continue_on_error", "`true`（默认）：钩子失败只发出警告。`false`：失败即阻止。"],
            ["background", "`true` 表示只作为观察者运行，不能阻止或改写。"],
            ["working_dir", "写在 `[hooks]` 下：钩子的运行目录。默认是会话的工作区。"],
          ],
          codeTerms: true,
        },
        {
          note: "`[hooks] default_timeout_secs` 会替换每个钩子自己的 `timeout_secs`，而不只是补上未设置的那些。如果想让各钩子使用各自的超时，请不要设置它。",
        },
      ],
    },
    {
      id: "project",
      title: "使用仓库自带的钩子",
      blocks: [
        {
          p: "仓库可以在 `.codewhale/hooks.toml` 中附带钩子。由于它们会在你的机器上运行命令，只有在你信任该工作区，并且批准了这份文件的确切内容之后，才会加载：",
        },
        { code: "/hooks review\n/hooks approve <digest>\n/hooks revoke", lang: "Codewhale" },
        {
          p: "`/hooks review` 会显示其中的命令以及文件摘要；批准该摘要后，从下一次会话起启用这份确切的内容。文件有任何改动都需要重新批准。命令调用的脚本也请一并审阅。",
        },
      ],
    },
    {
      id: "headless",
      title: "在脚本和 CI 中使用钩子",
      blocks: [
        {
          p: "钩子在交互式会话中运行。`codewhale exec` 默认不触发任何钩子；加上 `--hooks` 后会触发 `tool_call_before` 和 `shell_env`。由于没有人可以回应，`ask` 会被当作拒绝。",
        },
        { code: 'codewhale exec --auto --sandbox workspace-write --hooks "run the test suite and fix the first failure"', lang: "终端" },
      ],
    },
  ],
  next: [
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "了解钩子的决定如何与审批设置共同起作用。",
    },
    {
      href: "/docs/mcp",
      label: "用 MCP 连接工具",
      note: "用同样的 `tool_call_before` 钩子管控 MCP 工具。",
    },
    {
      href: "/docs/configuration",
      label: "修改设置",
      note: "`config.toml` 在哪里，以及项目可以覆盖哪些设置。",
    },
  ],
  sourceNote: "来源文档：docs/HOOKS.md（权威）、docs/CONFIGURATION.md · 修改时同步更新 docs-map.ts。",
};
